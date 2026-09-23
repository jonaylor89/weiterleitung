use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::domain::Direction;
use crate::mailbox::parse_timestamp;

/// A message accepted by the SMTP server and waiting to go out.
#[derive(Debug, Clone)]
pub struct QueuedMessage {
    pub message_id: String,
    pub direction: String,
    pub envelope_from: String,
    pub envelope_to: String,
    pub raw_path: String,
    pub alias_id: Option<String>,
    pub contact_id: Option<String>,
    pub subject: Option<String>,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub struct NewOutboundMessage<'a> {
    pub direction: Direction,
    pub envelope_from: String,
    pub envelope_to: String,
    pub raw: &'a [u8],
    pub alias_id: Option<String>,
    pub contact_id: Option<String>,
    pub subject: Option<String>,
}

/// Persists the raw message under `mail_dir` and queues it for delivery.
#[tracing::instrument(name = "Enqueue outbound message", skip(pool, mail_dir, message))]
pub async fn enqueue(
    pool: &SqlitePool,
    mail_dir: &Path,
    message: NewOutboundMessage<'_>,
) -> Result<String, anyhow::Error> {
    tokio::fs::create_dir_all(mail_dir)
        .await
        .with_context(|| format!("Failed to create the mail directory {}", mail_dir.display()))?;

    let message_id = Uuid::new_v4().to_string();
    let raw_path: PathBuf = mail_dir.join(format!("{message_id}.eml"));
    tokio::fs::write(&raw_path, message.raw)
        .await
        .with_context(|| format!("Failed to write the raw message to {}", raw_path.display()))?;

    let now = Utc::now().to_rfc3339();
    let direction = message.direction.as_str();
    let raw_path_str = raw_path.to_string_lossy().to_string();

    sqlx::query!(
        r#"
        INSERT INTO outbox (
            message_id, direction, envelope_from, envelope_to, raw_path,
            alias_id, contact_id, subject, status, attempts, next_attempt_at, created_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)
        "#,
        message_id,
        direction,
        message.envelope_from,
        message.envelope_to,
        raw_path_str,
        message.alias_id,
        message.contact_id,
        message.subject,
        now,
        now,
    )
    .execute(pool)
    .await
    .context("Failed to enqueue the outbound message")?;

    Ok(message_id)
}

/// Atomically claims messages whose retry time has come.
#[tracing::instrument(name = "Claim due messages", skip(pool))]
pub async fn claim_due(pool: &SqlitePool, limit: i64) -> Result<Vec<QueuedMessage>, anyhow::Error> {
    let now = Utc::now().to_rfc3339();
    let mut tx = pool.begin().await?;

    let rows = sqlx::query!(
        r#"
        SELECT
            message_id, direction, envelope_from, envelope_to, raw_path,
            alias_id, contact_id, subject, status, attempts, last_error, created_at
        FROM outbox
        WHERE status = 'pending' AND next_attempt_at <= ?
        ORDER BY next_attempt_at ASC
        LIMIT ?
        "#,
        now,
        limit
    )
    .fetch_all(&mut *tx)
    .await?;

    let messages: Vec<QueuedMessage> = rows
        .into_iter()
        .map(|row| QueuedMessage {
            message_id: row.message_id,
            direction: row.direction,
            envelope_from: row.envelope_from,
            envelope_to: row.envelope_to,
            raw_path: row.raw_path,
            alias_id: row.alias_id,
            contact_id: row.contact_id,
            subject: row.subject,
            status: row.status,
            attempts: row.attempts,
            last_error: row.last_error,
            created_at: parse_timestamp(&row.created_at),
        })
        .collect();

    for message in &messages {
        sqlx::query!(
            "UPDATE outbox SET status = 'sending' WHERE message_id = ?",
            message.message_id
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(messages)
}

/// Messages are marked `sending` while the worker holds them, so a crash or a
/// restart mid-delivery would strand them there forever. Returning them to the
/// queue on startup costs at most a duplicate send, which the retry logic can
/// produce anyway.
#[tracing::instrument(name = "Requeue interrupted messages", skip(pool))]
pub async fn requeue_interrupted(pool: &SqlitePool) -> Result<u64, anyhow::Error> {
    let now = Utc::now().to_rfc3339();
    let requeued = sqlx::query!(
        r#"
        UPDATE outbox
        SET status = 'pending', next_attempt_at = ?
        WHERE status = 'sending'
        "#,
        now,
    )
    .execute(pool)
    .await?
    .rows_affected();

    Ok(requeued)
}

#[tracing::instrument(name = "Mark message delivered", skip(pool))]
pub async fn mark_delivered(pool: &SqlitePool, message_id: &str) -> Result<(), anyhow::Error> {
    let now = Utc::now().to_rfc3339();
    sqlx::query!(
        r#"
        UPDATE outbox
        SET status = 'delivered', delivered_at = ?, last_error = NULL
        WHERE message_id = ?
        "#,
        now,
        message_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Records a failed attempt, scheduling an exponential backoff retry until
/// `max_attempts` is reached, at which point the message is marked `failed`.
#[tracing::instrument(name = "Mark message attempt failed", skip(pool))]
pub async fn mark_attempt_failed(
    pool: &SqlitePool,
    message_id: &str,
    attempts: i64,
    max_attempts: i64,
    error: &str,
) -> Result<(), anyhow::Error> {
    let attempts = attempts + 1;
    if attempts >= max_attempts {
        sqlx::query!(
            r#"
            UPDATE outbox
            SET status = 'failed', attempts = ?, last_error = ?
            WHERE message_id = ?
            "#,
            attempts,
            error,
            message_id
        )
        .execute(pool)
        .await?;
        return Ok(());
    }

    let next_attempt_at = (Utc::now() + backoff(attempts)).to_rfc3339();
    sqlx::query!(
        r#"
        UPDATE outbox
        SET status = 'pending', attempts = ?, last_error = ?, next_attempt_at = ?
        WHERE message_id = ?
        "#,
        attempts,
        error,
        next_attempt_at,
        message_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Doubling backoff starting at one minute, capped at six hours.
fn backoff(attempts: i64) -> Duration {
    let minutes = 1i64
        .checked_shl(attempts.clamp(0, 16) as u32)
        .unwrap_or(360)
        .min(360);
    Duration::minutes(minutes)
}

#[tracing::instrument(name = "List recent messages", skip(pool))]
pub async fn list_recent(
    pool: &SqlitePool,
    limit: i64,
) -> Result<Vec<QueuedMessage>, anyhow::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            message_id, direction, envelope_from, envelope_to, raw_path,
            alias_id, contact_id, subject, status, attempts, last_error, created_at
        FROM outbox
        ORDER BY created_at DESC
        LIMIT ?
        "#,
        limit
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| QueuedMessage {
            message_id: row.message_id,
            direction: row.direction,
            envelope_from: row.envelope_from,
            envelope_to: row.envelope_to,
            raw_path: row.raw_path,
            alias_id: row.alias_id,
            contact_id: row.contact_id,
            subject: row.subject,
            status: row.status,
            attempts: row.attempts,
            last_error: row.last_error,
            created_at: parse_timestamp(&row.created_at),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::backoff;

    #[test]
    fn backoff_doubles_and_is_capped() {
        assert_eq!(backoff(1).num_minutes(), 2);
        assert_eq!(backoff(3).num_minutes(), 8);
        assert_eq!(backoff(20).num_minutes(), 360);
    }
}
