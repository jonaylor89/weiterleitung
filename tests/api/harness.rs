//! Shared fixtures. Every test gets its own temporary SQLite database, mail
//! spool and (where needed) Mailpit instance, so the suite runs in parallel.

use secrecy::Secret;
use serde::Deserialize;
use sqlx::SqlitePool;
use std::process::{Child, Command, Stdio};
use std::sync::LazyLock;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use weiterleitung::configuration::{Settings, get_configuration};
use weiterleitung::smtp::SmtpServer;
use weiterleitung::startup::{Application, get_connection_pool};
use weiterleitung::telemetry::{get_subscriber, init_subscriber};

static TRACING: LazyLock<()> = LazyLock::new(|| {
    if std::env::var("TEST_LOG").is_ok() {
        init_subscriber(get_subscriber(
            "test".into(),
            "debug".into(),
            std::io::stdout,
        ));
    } else {
        init_subscriber(get_subscriber("test".into(), "debug".into(), std::io::sink));
    }
});

pub struct TestApp {
    pub address: String,
    pub pool: SqlitePool,
    pub settings: Settings,
    /// Port of the inbound SMTP server, when the test asked for one.
    pub smtp_port: Option<u16>,
    // Dropping this removes the database and the mail spool.
    _workspace: TempDir,
}

impl TestApp {
    pub fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .cookie_store(true)
            .build()
            .expect("Failed to build the HTTP client")
    }

    /// Opens a session against the inbound SMTP server.
    pub async fn smtp(&self) -> SmtpSession {
        let port = self
            .smtp_port
            .expect("This app was spawned without an inbound SMTP server");
        SmtpSession::connect(port).await
    }
}

pub async fn spawn_app() -> TestApp {
    spawn(false, |_| {}).await
}

/// Spawns the HTTP admin API plus an inbound SMTP server on an ephemeral port,
/// so the test can post mail over the wire instead of calling the router.
pub async fn spawn_app_with_smtp(customise: impl FnOnce(&mut Settings)) -> TestApp {
    spawn(true, customise).await
}

async fn spawn(inbound: bool, customise: impl FnOnce(&mut Settings)) -> TestApp {
    LazyLock::force(&TRACING);

    let workspace = tempfile::tempdir().expect("Failed to create a temporary directory");
    let mut settings = {
        let mut settings = get_configuration().expect("Failed to read the configuration");
        settings.application.port = 0;
        settings.application.host = "127.0.0.1".to_string();
        settings.database.path = workspace
            .path()
            .join("weiterleitung.db")
            .to_string_lossy()
            .to_string();
        settings.delivery.mail_dir = workspace.path().join("mail");
        settings.admin.username = "admin".to_string();
        settings.admin.password = Secret::new("password".to_string());
        settings.aliases.domains = vec!["example.com".to_string()];
        settings.inbound.host = "127.0.0.1".to_string();
        settings.inbound.port = 0;
        settings
    };
    customise(&mut settings);

    let application = Application::build(settings.clone())
        .await
        .expect("Failed to build the application");
    let port = application.port();
    tokio::spawn(application.run_until_stopped());

    let smtp_port = if inbound {
        let server = SmtpServer::build(&settings)
            .await
            .expect("Failed to bind the inbound SMTP server");
        let smtp_port = server.port();
        tokio::spawn(server.run_until_stopped());
        Some(smtp_port)
    } else {
        None
    };

    TestApp {
        address: format!("http://127.0.0.1:{port}"),
        pool: get_connection_pool(&settings.database).await,
        settings,
        smtp_port,
        _workspace: workspace,
    }
}

/// A deliberately literal SMTP client: it speaks the wire protocol so the tests
/// exercise the real session code, and returns the raw replies so they can
/// assert on status codes.
pub struct SmtpSession {
    writer: OwnedWriteHalf,
    reader: tokio::io::Lines<BufReader<OwnedReadHalf>>,
}

impl SmtpSession {
    pub async fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("Failed to connect to the inbound SMTP server");
        let (reader, writer) = stream.into_split();
        let mut session = Self {
            writer,
            reader: BufReader::new(reader).lines(),
        };
        let banner = session.read_reply().await;
        assert!(banner.starts_with("220"), "Unexpected banner: {banner}");
        session
    }

    /// Reads one reply, joining the lines of a multi-line one.
    pub async fn read_reply(&mut self) -> String {
        let mut reply = String::new();
        loop {
            let line = tokio::time::timeout(Duration::from_secs(10), self.reader.next_line())
                .await
                .expect("Timed out waiting for an SMTP reply")
                .expect("Failed to read from the SMTP server")
                .expect("The SMTP server closed the connection");
            let more = line.as_bytes().get(3) == Some(&b'-');
            if !reply.is_empty() {
                reply.push('\n');
            }
            reply.push_str(&line);
            if !more {
                return reply;
            }
        }
    }

    pub async fn command(&mut self, command: &str) -> String {
        self.writer
            .write_all(format!("{command}\r\n").as_bytes())
            .await
            .expect("Failed to write an SMTP command");
        self.read_reply().await
    }

    /// Runs a whole transaction and returns the reply to the final dot.
    pub async fn send_mail(&mut self, from: &str, to: &str, body: &str) -> String {
        let ehlo = self.command("EHLO tester.example").await;
        assert!(ehlo.starts_with("250"), "EHLO was refused: {ehlo}");
        let mail = self.command(&format!("MAIL FROM:<{from}>")).await;
        assert!(mail.starts_with("250"), "MAIL FROM was refused: {mail}");
        let rcpt = self.command(&format!("RCPT TO:<{to}>")).await;
        assert!(rcpt.starts_with("250"), "RCPT TO was refused: {rcpt}");
        let data = self.command("DATA").await;
        assert!(data.starts_with("354"), "DATA was refused: {data}");
        self.writer
            .write_all(body.replace('\n', "\r\n").as_bytes())
            .await
            .expect("Failed to write the message body");
        self.command(".").await
    }

    pub async fn quit(mut self) {
        let _ = self.command("QUIT").await;
    }
}

/// A real Mailpit process: an SMTP server that accepts our outbound mail plus
/// an HTTP API the tests read the delivered messages back from.
pub struct Mailpit {
    process: Child,
    pub smtp_port: u16,
    pub http_port: u16,
    client: reqwest::Client,
    _workspace: TempDir,
}

/// Extra Mailpit flags, e.g. STARTTLS or authentication.
#[derive(Default)]
pub struct MailpitOptions {
    pub args: Vec<String>,
}

impl MailpitOptions {
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }
}

impl Mailpit {
    pub async fn start() -> Self {
        Self::start_with(MailpitOptions::default()).await
    }

    pub async fn start_with(options: MailpitOptions) -> Self {
        let binary = std::env::var("MAILPIT_BIN").unwrap_or_else(|_| "mailpit".to_string());
        let mut last_error = String::new();

        // Mailpit cannot report back an ephemeral port, so the ports are picked
        // by binding and releasing. Another process can steal one in between;
        // that is rare and retrying is enough.
        for _ in 0..5 {
            let workspace = tempfile::tempdir().expect("Failed to create a temporary directory");
            let smtp_port = free_port();
            let http_port = free_port();
            let process = Command::new(&binary)
                .args([
                    "--smtp",
                    &format!("127.0.0.1:{smtp_port}"),
                    "--listen",
                    &format!("127.0.0.1:{http_port}"),
                    "--database",
                    &workspace.path().join("mailpit.db").to_string_lossy(),
                    "--quiet",
                    "--disable-version-check",
                    "--enable-chaos",
                ])
                .args(&options.args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|e| {
                    panic!(
                        "Failed to start `{binary}`: {e}. The Mailpit integration tests need the \
                         Mailpit binary on PATH (or MAILPIT_BIN pointing at it); see \
                         docs/testing.md"
                    )
                });

            let mailpit = Self {
                process,
                smtp_port,
                http_port,
                client: reqwest::Client::new(),
                _workspace: workspace,
            };

            match mailpit.wait_until_ready().await {
                Ok(()) => return mailpit,
                Err(e) => last_error = e,
            }
        }

        panic!("Mailpit never became ready: {last_error}");
    }

    async fn wait_until_ready(&self) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if let Ok(response) = self.client.get(self.url("/readyz")).send().await
                && response.status().is_success()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Err(format!(
            "/readyz never succeeded on port {}",
            self.http_port
        ))
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.http_port)
    }

    /// Waits for `count` messages to arrive and returns them, newest first.
    pub async fn wait_for_messages(&self, count: usize) -> Vec<MailpitMessage> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut seen = 0;
        while std::time::Instant::now() < deadline {
            let messages = self.messages().await;
            seen = messages.len();
            if seen >= count {
                return messages;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("Mailpit received {seen} messages, expected {count}");
    }

    /// Asserts that nothing arrives within `window`.
    pub async fn assert_no_message_within(&self, window: Duration) {
        tokio::time::sleep(window).await;
        let messages = self.messages().await;
        assert!(
            messages.is_empty(),
            "Mailpit unexpectedly received {} message(s)",
            messages.len()
        );
    }

    pub async fn messages(&self) -> Vec<MailpitMessage> {
        let body = self
            .client
            .get(self.url("/api/v1/messages?limit=100"))
            .send()
            .await
            .expect("Failed to query Mailpit")
            .text()
            .await
            .expect("Failed to read the Mailpit message list");
        let response: MailpitMessages = serde_json::from_str(&body)
            .unwrap_or_else(|e| panic!("Failed to decode the Mailpit message list: {e}\n{body}"));
        response.messages
    }

    /// The message exactly as it came off the wire, including the headers our
    /// rewriting added and Mailpit's own `Return-Path` (the envelope sender).
    pub async fn raw(&self, id: &str) -> String {
        self.client
            .get(self.url(&format!("/api/v1/message/{id}/raw")))
            .send()
            .await
            .expect("Failed to fetch the raw message")
            .text()
            .await
            .expect("Failed to read the raw message")
    }

    /// The envelope of a delivered message as the receiving server recorded
    /// it: the `Return-Path` is the `MAIL FROM` and the `Received` trace holds
    /// the `RCPT TO`.
    pub async fn envelope(&self, id: &str) -> (String, String) {
        let raw = self.raw(id).await;
        let from = between(&raw, "Return-Path: <", ">")
            .unwrap_or_else(|| panic!("No Return-Path in:\n{raw}"));
        let to =
            between(&raw, "for <", ">").unwrap_or_else(|| panic!("No `for` recipient in:\n{raw}"));
        (from, to)
    }

    /// Makes Mailpit reject every recipient with `code` until reset to 0.
    pub async fn set_recipient_rejection(&self, code: u16, probability: u8) {
        let response = self
            .client
            .put(self.url("/api/v1/chaos"))
            .json(&serde_json::json!({
                "Recipient": {"ErrorCode": code, "Probability": probability}
            }))
            .send()
            .await
            .expect("Failed to configure Mailpit chaos");
        assert!(
            response.status().is_success(),
            "Mailpit rejected the chaos configuration: {}",
            response.status()
        );
    }
}

impl Drop for Mailpit {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[derive(serde::Deserialize)]
struct MailpitMessages {
    #[serde(deserialize_with = "null_as_empty")]
    messages: Vec<MailpitMessage>,
}

/// Mailpit sends `null` rather than `[]` for empty lists.
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(serde::Deserialize)]
pub struct MailpitMessage {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "From")]
    pub from: MailpitAddress,
    #[serde(rename = "ReplyTo", deserialize_with = "null_as_empty")]
    pub reply_to: Vec<MailpitAddress>,
    #[serde(rename = "Subject")]
    pub subject: String,
}

#[derive(serde::Deserialize)]
pub struct MailpitAddress {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Address")]
    pub address: String,
}

fn between(haystack: &str, start: &str, end: &str) -> Option<String> {
    let rest = haystack.split_once(start)?.1;
    Some(rest.split_once(end)?.0.to_string())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("Failed to reserve a port")
        .local_addr()
        .expect("The reserved socket is bound")
        .port()
}
