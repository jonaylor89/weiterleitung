use anyhow::{Context, anyhow};
use hickory_resolver::TokioAsyncResolver;
use mail_auth::common::crypto::{RsaKey, Sha256};
use mail_auth::dkim::{DkimSigner, Done};
use mail_send::SmtpClientBuilder;
use mail_send::smtp::message::Message;
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use secrecy::ExposeSecret;
use std::sync::Once;
use std::time::Duration;

use crate::configuration::{DeliveryMode, DeliverySettings, DkimSettings};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Hands finished messages to the outside world, either through a smarthost or
/// straight to the recipient domain's MX.
pub struct Relay {
    settings: DeliverySettings,
    signer: Option<DkimSigner<RsaKey<Sha256>, Done>>,
    resolver: TokioAsyncResolver,
}

/// Several dependencies pull in rustls with different crypto backends, which
/// leaves the process-wide provider ambiguous; TLS then panics on first use.
fn install_crypto_provider() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Relay {
    pub fn build(settings: &DeliverySettings, dkim: &DkimSettings) -> Result<Self, anyhow::Error> {
        install_crypto_provider();

        if settings.mode == DeliveryMode::Relay && settings.relay.is_none() {
            return Err(anyhow!(
                "`delivery.mode` is `relay` but no `delivery.relay` block was configured"
            ));
        }

        Ok(Self {
            settings: settings.clone(),
            signer: build_signer(dkim)?,
            resolver: TokioAsyncResolver::tokio_from_system_conf()
                .context("Failed to build the DNS resolver")?,
        })
    }

    #[tracing::instrument(name = "Relay message", skip(self, raw), fields(bytes = raw.len()))]
    pub async fn send(
        &self,
        envelope_from: &str,
        envelope_to: &str,
        raw: &[u8],
    ) -> Result<(), anyhow::Error> {
        let message = Message::new(
            envelope_from.to_string(),
            vec![envelope_to.to_string()],
            raw.to_vec(),
        );

        match self.settings.mode {
            DeliveryMode::Relay => {
                let relay = self
                    .settings
                    .relay
                    .as_ref()
                    .expect("relay settings are validated in `Relay::build`");
                let mut builder = SmtpClientBuilder::new(relay.host.clone(), relay.port)
                    .map_err(|e| anyhow!("Invalid relay host: {e}"))?
                    .implicit_tls(relay.implicit_tls)
                    .helo_host(self.settings.ehlo_hostname.clone())
                    .timeout(CONNECT_TIMEOUT);
                if let (Some(username), Some(password)) = (&relay.username, &relay.password) {
                    builder =
                        builder.credentials((username.clone(), password.expose_secret().clone()));
                }
                self.deliver(&builder, message).await
            }
            DeliveryMode::Direct => {
                let domain = envelope_to
                    .rsplit_once('@')
                    .map(|(_, domain)| domain.to_string())
                    .ok_or_else(|| anyhow!("`{envelope_to}` has no domain part"))?;
                let hosts = self.lookup_mx(&domain).await?;
                let mut last_error = None;

                for host in hosts {
                    let builder = SmtpClientBuilder::new(host.clone(), 25)
                        .map_err(|e| anyhow!("Invalid MX host `{host}`: {e}"))?
                        .implicit_tls(false)
                        .allow_invalid_certs()
                        .helo_host(self.settings.ehlo_hostname.clone())
                        .timeout(CONNECT_TIMEOUT);
                    match self.deliver(&builder, message.clone()).await {
                        Ok(()) => return Ok(()),
                        Err(e) => {
                            tracing::warn!(error = %e, mx = %host, "MX delivery attempt failed");
                            last_error = Some(e);
                        }
                    }
                }

                Err(last_error
                    .unwrap_or_else(|| anyhow!("No MX host accepted mail for `{domain}`")))
            }
        }
    }

    /// Opportunistic TLS: fall back to clear text when the peer offers no
    /// STARTTLS, which is common for internal smarthosts and MX hosts.
    async fn deliver(
        &self,
        builder: &SmtpClientBuilder<String>,
        message: Message<'_>,
    ) -> Result<(), anyhow::Error> {
        match builder.connect().await {
            Ok(mut client) => self.transmit(&mut client, message).await,
            Err(mail_send::Error::MissingStartTls) => {
                let mut client = builder
                    .connect_plain()
                    .await
                    .context("Failed to connect over clear text")?;
                self.transmit(&mut client, message).await
            }
            Err(e) => Err(anyhow!("Failed to connect to the SMTP server: {e}")),
        }
    }

    async fn transmit<T>(
        &self,
        client: &mut mail_send::SmtpClient<T>,
        message: Message<'_>,
    ) -> Result<(), anyhow::Error>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        match &self.signer {
            Some(signer) => client
                .send_signed(message, signer)
                .await
                .map_err(|e| anyhow!("Failed to send the DKIM-signed message: {e}")),
            None => client
                .send(message)
                .await
                .map_err(|e| anyhow!("Failed to send the message: {e}")),
        }
    }

    #[tracing::instrument(name = "Look up MX records", skip(self))]
    async fn lookup_mx(&self, domain: &str) -> Result<Vec<String>, anyhow::Error> {
        match self.resolver.mx_lookup(domain).await {
            Ok(response) => {
                let mut records: Vec<_> = response.iter().collect();
                records.sort_by_key(|record| record.preference());
                let hosts: Vec<String> = records
                    .into_iter()
                    .map(|record| {
                        record
                            .exchange()
                            .to_utf8()
                            .trim_end_matches('.')
                            .to_string()
                    })
                    .filter(|host| !host.is_empty())
                    .collect();
                if hosts.is_empty() {
                    // RFC 5321 §5.1: fall back to the A record of the domain.
                    Ok(vec![domain.to_string()])
                } else {
                    Ok(hosts)
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "MX lookup failed, falling back to the A record");
                Ok(vec![domain.to_string()])
            }
        }
    }
}

fn build_signer(
    dkim: &DkimSettings,
) -> Result<Option<DkimSigner<RsaKey<Sha256>, Done>>, anyhow::Error> {
    if !dkim.enabled {
        return Ok(None);
    }

    let path = dkim
        .private_key_path
        .as_ref()
        .ok_or_else(|| anyhow!("DKIM is enabled but `dkim.private_key_path` is not set"))?;
    let pem = std::fs::read(path)
        .with_context(|| format!("Failed to read the DKIM key at {}", path.display()))?;
    let key_der = PrivateKeyDer::from_pem_slice(&pem)
        .map_err(|e| anyhow!("Failed to read the DKIM private key as PEM: {e}"))?;
    let key = RsaKey::<Sha256>::from_key_der(key_der)
        .map_err(|e| anyhow!("Failed to parse the DKIM private key: {e}"))?;

    Ok(Some(
        DkimSigner::from_key(key)
            .domain(dkim.domain.clone())
            .selector(dkim.selector.clone())
            .headers(["From", "To", "Subject", "Date", "Message-ID"]),
    ))
}
