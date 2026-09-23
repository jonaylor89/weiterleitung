use sqlx::SqlitePool;
use std::path::PathBuf;

use crate::alias::{AliasCounter, find_alias_by_address, record_alias_activity};
use crate::configuration::{AliasSettings, Settings};
use crate::contact::{find_contact_by_reverse_alias, get_or_create_contact};
use crate::delivery::queue::{NewOutboundMessage, enqueue};
use crate::domain::{Direction, EmailAddress};
use crate::smtp::rewrite::{
    ForwardRewrite, ReplyRewrite, header_value, rewrite_for_forward, rewrite_for_reply,
    split_headers_body,
};
use crate::smtp::session::{Envelope, MailHandler, RecipientDecision};

/// Decides what an accepted recipient means: a contact writing to an alias, or
/// the mailbox replying through a reverse alias.
enum Route {
    Forward {
        alias_id: String,
        alias: String,
    },
    Reply {
        contact_id: String,
        alias: String,
        contact_email: String,
    },
}

/// Resolves recipients against the alias tables and queues the rewritten mail.
pub struct AliasRouter {
    pool: SqlitePool,
    aliases: AliasSettings,
    mail_dir: PathBuf,
}

impl AliasRouter {
    pub fn new(pool: SqlitePool, configuration: &Settings) -> Self {
        Self {
            pool,
            aliases: configuration.aliases.clone(),
            mail_dir: configuration.delivery.mail_dir.clone(),
        }
    }

    async fn route(&self, mail_from: &str, recipient: &str) -> Result<Route, String> {
        let recipient = EmailAddress::parse(recipient)
            .map_err(|_| format!("`{recipient}` is not a valid address"))?;

        if !self.aliases.owns_domain(recipient.domain()) {
            return Err("Relay access denied".to_string());
        }

        if let Some(contact) = find_contact_by_reverse_alias(&self.pool, &recipient)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to look up the reverse alias");
                "Temporary lookup failure".to_string()
            })?
        {
            // Only the mailbox behind the alias may send through a reverse
            // alias; otherwise anybody could impersonate the alias.
            if !contact.mailbox_email.eq_ignore_ascii_case(mail_from.trim()) {
                return Err("Reverse aliases only accept mail from their mailbox".to_string());
            }
            if !contact.alias_enabled {
                return Err("Alias is disabled".to_string());
            }
            return Ok(Route::Reply {
                contact_id: contact.contact.contact_id,
                alias: contact.alias_address,
                contact_email: contact.contact.email,
            });
        }

        let alias = find_alias_by_address(&self.pool, &recipient)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "Failed to look up the alias");
                "Temporary lookup failure".to_string()
            })?
            .ok_or_else(|| format!("No such alias: {recipient}"))?;

        if !alias.alias.enabled {
            let _ = record_alias_activity(&self.pool, &alias.alias.alias_id, AliasCounter::Blocked)
                .await;
            return Err("Alias is disabled".to_string());
        }

        Ok(Route::Forward {
            alias_id: alias.alias.alias_id,
            alias: alias.alias.address,
        })
    }

    #[tracing::instrument(name = "Forward to mailbox", skip(self, envelope))]
    async fn forward(
        &self,
        alias_id: &str,
        alias: &str,
        envelope: &Envelope,
    ) -> Result<(), anyhow::Error> {
        let address = EmailAddress::parse(alias).map_err(anyhow::Error::msg)?;
        let alias_with_mailbox = find_alias_by_address(&self.pool, &address).await?;
        let Some(alias_with_mailbox) = alias_with_mailbox else {
            anyhow::bail!("Alias `{alias}` vanished between RCPT and DATA");
        };

        let (headers, _) = split_headers_body(&envelope.data);
        let from_header = header_value(headers, "from");
        let sender_email = envelope_sender(&envelope.mail_from, from_header.as_deref())?;
        let sender_name = from_header.as_deref().and_then(display_name);

        let contact = get_or_create_contact(
            &self.pool,
            alias_id,
            &sender_email,
            sender_name.as_deref(),
            &self.aliases.reverse_alias_prefix,
            self.aliases.default_domain(),
        )
        .await?;

        let rewritten = rewrite_for_forward(
            &envelope.data,
            &ForwardRewrite {
                alias,
                reverse_alias: &contact.reverse_alias,
                sender_name: sender_name.as_deref(),
                sender_email: &sender_email.to_string(),
            },
        );

        enqueue(
            &self.pool,
            &self.mail_dir,
            NewOutboundMessage {
                direction: Direction::Forward,
                envelope_from: contact.reverse_alias.clone(),
                envelope_to: alias_with_mailbox.mailbox_email.clone(),
                raw: &rewritten,
                alias_id: Some(alias_id.to_string()),
                contact_id: Some(contact.contact_id),
                subject: header_value(headers, "subject"),
            },
        )
        .await?;

        record_alias_activity(&self.pool, alias_id, AliasCounter::Forwarded).await?;
        Ok(())
    }

    #[tracing::instrument(name = "Send reply as alias", skip(self, envelope))]
    async fn reply(
        &self,
        contact_id: &str,
        alias: &str,
        contact_email: &str,
        envelope: &Envelope,
    ) -> Result<(), anyhow::Error> {
        let (headers, _) = split_headers_body(&envelope.data);
        let rewritten = rewrite_for_reply(
            &envelope.data,
            &ReplyRewrite {
                alias,
                contact_email,
            },
        );

        let address = EmailAddress::parse(alias).map_err(anyhow::Error::msg)?;
        let alias_id = find_alias_by_address(&self.pool, &address)
            .await?
            .map(|alias| alias.alias.alias_id);

        enqueue(
            &self.pool,
            &self.mail_dir,
            NewOutboundMessage {
                direction: Direction::Reply,
                envelope_from: alias.to_string(),
                envelope_to: contact_email.to_string(),
                raw: &rewritten,
                alias_id: alias_id.clone(),
                contact_id: Some(contact_id.to_string()),
                subject: header_value(headers, "subject"),
            },
        )
        .await?;

        if let Some(alias_id) = alias_id {
            record_alias_activity(&self.pool, &alias_id, AliasCounter::Replied).await?;
        }
        Ok(())
    }
}

impl MailHandler for AliasRouter {
    async fn verify_recipient(&self, mail_from: &str, recipient: &str) -> RecipientDecision {
        match self.route(mail_from, recipient).await {
            Ok(_) => RecipientDecision::Accept,
            Err(reason) => RecipientDecision::Reject(reason),
        }
    }

    async fn handle(&self, envelope: Envelope) -> Result<(), String> {
        for recipient in &envelope.rcpt_to {
            let route = self
                .route(&envelope.mail_from, recipient)
                .await
                .map_err(|reason| {
                    format!("Recipient `{recipient}` is no longer routable: {reason}")
                })?;

            let outcome = match route {
                Route::Forward { alias_id, alias } => {
                    self.forward(&alias_id, &alias, &envelope).await
                }
                Route::Reply {
                    contact_id,
                    alias,
                    contact_email,
                } => {
                    self.reply(&contact_id, &alias, &contact_email, &envelope)
                        .await
                }
            };

            outcome.map_err(|e| {
                tracing::error!(
                    error.cause_chain = ?e,
                    error.message = %e,
                    recipient = %recipient,
                    "Failed to queue an inbound message",
                );
                "Failed to queue the message".to_string()
            })?;
        }

        Ok(())
    }
}

/// Prefers the envelope sender and falls back to the `From` header, which
/// matters for mail sent with the null sender (bounces, auto-replies).
fn envelope_sender(
    mail_from: &str,
    from_header: Option<&str>,
) -> Result<EmailAddress, anyhow::Error> {
    if let Ok(address) = EmailAddress::parse(mail_from) {
        return Ok(address);
    }
    let header = from_header.unwrap_or_default();
    let candidate = match (header.find('<'), header.find('>')) {
        (Some(start), Some(end)) if end > start => &header[start + 1..end],
        _ => header,
    };
    EmailAddress::parse(candidate).map_err(anyhow::Error::msg)
}

/// Pulls the display name out of a `From` header value.
fn display_name(from_header: &str) -> Option<String> {
    let (name, _) = from_header.split_once('<')?;
    let name = name.trim().trim_matches('"').trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::{display_name, envelope_sender};

    #[test]
    fn display_names_are_extracted_and_unquoted() {
        assert_eq!(
            display_name("\"Shop Support\" <support@shop.example>").as_deref(),
            Some("Shop Support")
        );
        assert_eq!(display_name("<support@shop.example>"), None);
        assert_eq!(display_name("support@shop.example"), None);
    }

    #[test]
    fn the_from_header_covers_for_the_null_sender() {
        let sender = envelope_sender("", Some("Shop <support@shop.example>")).unwrap();
        assert_eq!(sender.to_string(), "support@shop.example");

        let sender = envelope_sender("bounce@shop.example", None).unwrap();
        assert_eq!(sender.to_string(), "bounce@shop.example");

        assert!(envelope_sender("", None).is_err());
    }
}
