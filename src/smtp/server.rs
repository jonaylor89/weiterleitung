use std::sync::Arc;
use tokio::net::TcpListener;

use crate::configuration::Settings;
use crate::smtp::router::AliasRouter;
use crate::smtp::session::{SessionConfig, handle_session};
use crate::startup::get_connection_pool;

/// The inbound SMTP listener, built separately from the accept loop so callers
/// can learn the bound port before serving (port 0 in tests).
pub struct SmtpServer {
    listener: TcpListener,
    router: Arc<AliasRouter>,
    config: Arc<SessionConfig>,
}

impl SmtpServer {
    pub async fn build(configuration: &Settings) -> Result<Self, anyhow::Error> {
        let pool = get_connection_pool(&configuration.database).await;
        let address = format!(
            "{}:{}",
            configuration.inbound.host, configuration.inbound.port
        );
        let listener = TcpListener::bind(&address).await?;

        Ok(Self {
            listener,
            router: Arc::new(AliasRouter::new(pool, configuration)),
            config: Arc::new(SessionConfig {
                hostname: configuration.inbound.hostname.clone(),
                max_message_size: configuration.inbound.max_message_size,
            }),
        })
    }

    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .expect("The inbound listener is bound")
            .port()
    }

    pub async fn run_until_stopped(self) -> Result<(), anyhow::Error> {
        tracing::info!("Inbound SMTP server listening on port {}", self.port());

        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::error!(error = %e, "Failed to accept an SMTP connection");
                    continue;
                }
            };

            let router = Arc::clone(&self.router);
            let config = Arc::clone(&self.config);
            tokio::spawn(async move {
                tracing::info!(peer = %peer, "SMTP connection opened");
                if let Err(e) = handle_session(stream, router.as_ref(), config.as_ref()).await {
                    tracing::warn!(error = %e, peer = %peer, "SMTP session ended with an error");
                }
            });
        }
    }
}

/// Accepts inbound SMTP connections until the process is shut down.
pub async fn run_smtp_server_until_stopped(configuration: Settings) -> Result<(), anyhow::Error> {
    SmtpServer::build(&configuration)
        .await?
        .run_until_stopped()
        .await
}
