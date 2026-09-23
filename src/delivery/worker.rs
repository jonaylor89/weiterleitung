use sqlx::SqlitePool;
use std::time::Duration;

use crate::configuration::Settings;
use crate::delivery::queue::{self, QueuedMessage};
use crate::delivery::relay::Relay;
use crate::startup::get_connection_pool;

const BATCH_SIZE: i64 = 10;

/// Drains the outbox until the process is shut down.
pub async fn run_delivery_worker_until_stopped(
    configuration: Settings,
) -> Result<(), anyhow::Error> {
    let pool = get_connection_pool(&configuration.database).await;
    let relay = Relay::build(&configuration.delivery, &configuration.dkim)?;
    let poll_interval = Duration::from_secs(configuration.delivery.poll_interval_secs);
    let max_attempts = configuration.delivery.max_attempts;

    let requeued = queue::requeue_interrupted(&pool).await?;
    if requeued > 0 {
        tracing::warn!(
            requeued,
            "Requeued messages left mid-delivery by a previous run"
        );
    }

    tracing::info!("Delivery worker started");
    loop {
        match drain_outbox(&pool, &relay, max_attempts).await {
            Ok(0) | Err(_) => tokio::time::sleep(poll_interval).await,
            Ok(_) => {}
        }
    }
}

/// Delivers every message that is due right now and reports how many were
/// claimed. Callers that need a single pass (tests, one-shot runs) use this
/// instead of the polling loop.
#[tracing::instrument(name = "Drain outbox", skip(pool, relay))]
pub async fn drain_outbox(
    pool: &SqlitePool,
    relay: &Relay,
    max_attempts: i64,
) -> Result<usize, anyhow::Error> {
    let messages = queue::claim_due(pool, BATCH_SIZE).await?;
    let claimed = messages.len();

    for message in messages {
        if let Err(e) = deliver(pool, relay, &message, max_attempts).await {
            tracing::error!(
                error.cause_chain = ?e,
                error.message = %e,
                message_id = %message.message_id,
                "Failed to process a queued message",
            );
        }
    }

    Ok(claimed)
}

#[tracing::instrument(
    name = "Deliver message",
    skip(pool, relay, message),
    fields(message_id = %message.message_id, envelope_to = %message.envelope_to)
)]
async fn deliver(
    pool: &SqlitePool,
    relay: &Relay,
    message: &QueuedMessage,
    max_attempts: i64,
) -> Result<(), anyhow::Error> {
    let raw = match tokio::fs::read(&message.raw_path).await {
        Ok(raw) => raw,
        Err(e) => {
            // The spool file is gone; retrying cannot help.
            queue::mark_attempt_failed(
                pool,
                &message.message_id,
                max_attempts,
                max_attempts,
                &format!("Failed to read {}: {e}", message.raw_path),
            )
            .await?;
            return Ok(());
        }
    };

    match relay
        .send(&message.envelope_from, &message.envelope_to, &raw)
        .await
    {
        Ok(()) => {
            queue::mark_delivered(pool, &message.message_id).await?;
            tracing::info!("Message delivered");
        }
        Err(e) => {
            queue::mark_attempt_failed(
                pool,
                &message.message_id,
                message.attempts,
                max_attempts,
                &e.to_string(),
            )
            .await?;
            tracing::warn!(error = %e, "Delivery attempt failed");
        }
    }

    Ok(())
}
