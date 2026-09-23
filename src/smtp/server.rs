use std::sync::Arc;
use tokio::net::TcpListener;

use crate::configuration::Settings;
use crate::smtp::router::AliasRouter;
use crate::smtp::session::{SessionConfig, handle_session};
use crate::startup::get_connection_pool;

/// Accepts inbound SMTP connections until the process is shut down.
pub async fn run_smtp_server_until_stopped(configuration: Settings) -> Result<(), anyhow::Error> {
    let pool = get_connection_pool(&configuration.database).await;
    let address = format!(
        "{}:{}",
        configuration.inbound.host, configuration.inbound.port
    );
    let listener = TcpListener::bind(&address).await?;
    tracing::info!("Inbound SMTP server listening on {address}");

    let router = Arc::new(AliasRouter::new(pool, &configuration));
    let config = Arc::new(SessionConfig {
        hostname: configuration.inbound.hostname.clone(),
        max_message_size: configuration.inbound.max_message_size,
    });

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(e) => {
                tracing::error!(error = %e, "Failed to accept an SMTP connection");
                continue;
            }
        };

        let router = Arc::clone(&router);
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            tracing::info!(peer = %peer, "SMTP connection opened");
            if let Err(e) = handle_session(stream, router.as_ref(), config.as_ref()).await {
                tracing::warn!(error = %e, peer = %peer, "SMTP session ended with an error");
            }
        });
    }
}
