use std::fmt::{Debug, Display};
use tokio::task::JoinError;
use weiterleitung::configuration::get_configuration;
use weiterleitung::delivery::run_delivery_worker_until_stopped;
use weiterleitung::smtp::run_smtp_server_until_stopped;
use weiterleitung::startup::Application;
use weiterleitung::telemetry::{get_subscriber, init_subscriber};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let subscriber = get_subscriber("weiterleitung".into(), "info".into(), std::io::stdout);
    init_subscriber(subscriber);

    let configuration = get_configuration().expect("Failed to read configuration");

    let application = Application::build(configuration.clone()).await?;
    let application_task = tokio::spawn(application.run_until_stopped());

    let smtp_task = tokio::spawn(run_smtp_server_until_stopped(configuration.clone()));

    let delivery_task = tokio::spawn(run_delivery_worker_until_stopped(configuration));

    tokio::select! {
        o = application_task => report_exit("API", o),
        o = smtp_task => report_exit("Inbound SMTP server", o),
        o = delivery_task => report_exit("Delivery worker", o),
    };

    Ok(())
}

fn report_exit(task_name: &str, outcome: Result<Result<(), impl Debug + Display>, JoinError>) {
    match outcome {
        Ok(Ok(())) => {
            tracing::info!("{} has exited", task_name)
        }
        Ok(Err(e)) => {
            tracing::error!(
                error.cause_chain = ?e,
                error.message = %e,
                "{} failed",
                task_name,
            )
        }
        Err(e) => {
            tracing::error!(
                error.cause_chain = ?e,
                error.message = %e,
                "'{}' task failed to complete",
                task_name,
            )
        }
    }
}
