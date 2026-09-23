//! Integration tests. Each one gets its own temporary SQLite database and mail
//! spool, so they can run in parallel without stepping on each other.

use secrecy::Secret;
use sqlx::SqlitePool;
use std::sync::LazyLock;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use weiterleitung::alias::{find_alias_by_address, insert_alias};
use weiterleitung::configuration::{
    DeliveryMode, DkimSettings, RelaySettings, Settings, get_configuration,
};
use weiterleitung::contact::find_contact_by_reverse_alias;
use weiterleitung::delivery::{Relay, list_recent};
use weiterleitung::domain::EmailAddress;
use weiterleitung::mailbox::insert_mailbox;
use weiterleitung::smtp::{AliasRouter, Envelope, MailHandler, RecipientDecision};
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

struct TestApp {
    address: String,
    pool: SqlitePool,
    settings: Settings,
    // Dropping this removes the database and the mail spool.
    _workspace: TempDir,
}

impl TestApp {
    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .cookie_store(true)
            .build()
            .expect("Failed to build the HTTP client")
    }
}

async fn spawn_app() -> TestApp {
    LazyLock::force(&TRACING);

    let workspace = tempfile::tempdir().expect("Failed to create a temporary directory");
    let settings = {
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
        settings
    };

    let application = Application::build(settings.clone())
        .await
        .expect("Failed to build the application");
    let port = application.port();
    tokio::spawn(application.run_until_stopped());

    TestApp {
        address: format!("http://127.0.0.1:{port}"),
        pool: get_connection_pool(&settings.database).await,
        settings,
        _workspace: workspace,
    }
}

/// Guards against the process-wide rustls crypto provider being ambiguous,
/// which makes the relay panic the moment it opens a connection.
#[tokio::test]
async fn the_relay_delivers_a_message_to_a_smarthost() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_port = listener.local_addr().unwrap().port();
    // The relay probes for STARTTLS first and reconnects in clear text when the
    // peer does not offer it, so the sink has to survive more than one
    // connection.
    let sink = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = socket.split();
            let mut lines = tokio::io::BufReader::new(reader).lines();
            let mut received = Vec::new();
            let mut in_data = false;
            writer.write_all(b"220 sink ESMTP\r\n").await.unwrap();
            while let Ok(Some(line)) = lines.next_line().await {
                if in_data {
                    if line == "." {
                        in_data = false;
                        writer.write_all(b"250 2.0.0 Queued\r\n").await.unwrap();
                    } else {
                        received.push(line);
                    }
                    continue;
                }
                let response: &[u8] = match line.split_whitespace().next().unwrap_or_default() {
                    "EHLO" | "HELO" => b"250 sink\r\n",
                    "DATA" => {
                        in_data = true;
                        b"354 Go ahead\r\n"
                    }
                    "QUIT" => b"221 2.0.0 Bye\r\n",
                    _ => b"250 2.1.0 Ok\r\n",
                };
                writer.write_all(response).await.unwrap();
            }
            if !received.is_empty() {
                return received;
            }
        }
    });

    let mut settings = get_configuration().unwrap().delivery;
    settings.mode = DeliveryMode::Relay;
    settings.relay = Some(RelaySettings {
        host: "127.0.0.1".to_string(),
        port: relay_port,
        username: None,
        password: None,
        implicit_tls: false,
    });
    let relay = Relay::build(&settings, &DkimSettings::default()).unwrap();

    relay
        .send(
            "ct.abc@example.com",
            "support@shop.example",
            b"Subject: Hi\r\n\r\nHello\r\n",
        )
        .await
        .expect("The message should have been handed to the smarthost");

    let received = sink.await.unwrap();
    assert!(received.iter().any(|line| line == "Subject: Hi"));
}

#[tokio::test]
async fn health_check_works() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .get(format!("{}/health_check", app.address))
        .send()
        .await
        .expect("Failed to execute the request");

    assert!(response.status().is_success());
}

#[tokio::test]
async fn the_admin_area_redirects_anonymous_visitors_to_the_login_page() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .get(format!("{}/admin/dashboard", app.address))
        .send()
        .await
        .expect("Failed to execute the request");

    assert_eq!(response.status().as_u16(), 303);
    assert_eq!(response.headers()["Location"], "/login");
}

#[tokio::test]
async fn the_configured_admin_can_log_in_and_reach_the_dashboard() {
    let app = spawn_app().await;
    let client = TestApp::client();

    let response = client
        .post(format!("{}/login", app.address))
        .form(&serde_json::json!({"username": "admin", "password": "password"}))
        .send()
        .await
        .expect("Failed to execute the request");
    assert_eq!(response.headers()["Location"], "/admin/dashboard");

    let body = client
        .get(format!("{}/admin/dashboard", app.address))
        .send()
        .await
        .expect("Failed to execute the request")
        .text()
        .await
        .expect("Failed to read the response body");
    assert!(body.contains("new alias"));
}

#[tokio::test]
async fn wrong_credentials_are_rejected() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .post(format!("{}/login", app.address))
        .form(&serde_json::json!({"username": "admin", "password": "wrong"}))
        .send()
        .await
        .expect("Failed to execute the request");

    assert_eq!(response.headers()["Location"], "/login");
}

/// The core of the service: a stranger writes to an alias, the mail is queued
/// for the mailbox, and the reply path back to the stranger exists.
#[tokio::test]
async fn mail_to_an_alias_is_queued_for_the_mailbox_with_a_reverse_alias() {
    let app = spawn_app().await;
    let mailbox = insert_mailbox(
        &app.pool,
        &EmailAddress::parse("me@personal.example").unwrap(),
        true,
    )
    .await
    .unwrap();
    let alias_address = EmailAddress::parse("shop.1a2b@example.com").unwrap();
    insert_alias(&app.pool, &alias_address, &mailbox.mailbox_id, None)
        .await
        .unwrap();

    let router = AliasRouter::new(app.pool.clone(), &app.settings);
    let envelope = Envelope {
        mail_from: "support@shop.example".to_string(),
        rcpt_to: vec![alias_address.to_string()],
        data: b"From: Shop Support <support@shop.example>\r\n\
               To: shop.1a2b@example.com\r\n\
               Subject: Your order\r\n\
               \r\n\
               It shipped.\r\n"
            .to_vec(),
    };
    router.handle(envelope).await.unwrap();

    let queued = list_recent(&app.pool, 10).await.unwrap();
    assert_eq!(queued.len(), 1);
    let queued = &queued[0];
    assert_eq!(queued.direction, "forward");
    assert_eq!(queued.envelope_to, "me@personal.example");
    assert_eq!(queued.subject.as_deref(), Some("Your order"));

    // The envelope sender doubles as the reverse alias, so replying to it
    // reaches the contact.
    let reverse_alias = EmailAddress::parse(&queued.envelope_from).unwrap();
    let contact = find_contact_by_reverse_alias(&app.pool, &reverse_alias)
        .await
        .unwrap()
        .expect("The forward should have created a contact");
    assert_eq!(contact.contact.email, "support@shop.example");

    let raw = tokio::fs::read_to_string(&queued.raw_path).await.unwrap();
    assert!(raw.contains(&format!(
        "From: \"Shop Support - support@shop.example\" <{reverse_alias}>"
    )));
    assert!(raw.contains(&format!("Reply-To: {reverse_alias}")));

    let alias = find_alias_by_address(&app.pool, &alias_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alias.alias.forward_count, 1);
}

#[tokio::test]
async fn replies_through_a_reverse_alias_only_come_from_the_mailbox() {
    let app = spawn_app().await;
    let mailbox = insert_mailbox(
        &app.pool,
        &EmailAddress::parse("me@personal.example").unwrap(),
        true,
    )
    .await
    .unwrap();
    let alias_address = EmailAddress::parse("shop.1a2b@example.com").unwrap();
    insert_alias(&app.pool, &alias_address, &mailbox.mailbox_id, None)
        .await
        .unwrap();

    let router = AliasRouter::new(app.pool.clone(), &app.settings);
    router
        .handle(Envelope {
            mail_from: "support@shop.example".to_string(),
            rcpt_to: vec![alias_address.to_string()],
            data: b"From: support@shop.example\r\nSubject: Hi\r\n\r\nHello\r\n".to_vec(),
        })
        .await
        .unwrap();
    let reverse_alias = list_recent(&app.pool, 1).await.unwrap()[0]
        .envelope_from
        .clone();

    let stranger = router
        .verify_recipient("someone@else.example", &reverse_alias)
        .await;
    assert!(matches!(stranger, RecipientDecision::Reject(_)));

    let mailbox_reply = router
        .verify_recipient("me@personal.example", &reverse_alias)
        .await;
    assert!(matches!(mailbox_reply, RecipientDecision::Accept));

    router
        .handle(Envelope {
            mail_from: "me@personal.example".to_string(),
            rcpt_to: vec![reverse_alias],
            data: b"From: me@personal.example\r\nSubject: Re: Hi\r\n\r\nThanks\r\n".to_vec(),
        })
        .await
        .unwrap();

    let reply = list_recent(&app.pool, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|message| message.direction == "reply")
        .expect("The reply should have been queued");
    assert_eq!(reply.envelope_from, alias_address.to_string());
    assert_eq!(reply.envelope_to, "support@shop.example");

    let raw = tokio::fs::read_to_string(&reply.raw_path).await.unwrap();
    assert!(raw.contains(&format!("From: {alias_address}")));
    assert!(!raw.contains("me@personal.example"));
}

#[tokio::test]
async fn disabled_and_unknown_aliases_are_refused_at_rcpt() {
    let app = spawn_app().await;
    let mailbox = insert_mailbox(
        &app.pool,
        &EmailAddress::parse("me@personal.example").unwrap(),
        true,
    )
    .await
    .unwrap();
    let alias_address = EmailAddress::parse("burner@example.com").unwrap();
    let alias = insert_alias(&app.pool, &alias_address, &mailbox.mailbox_id, None)
        .await
        .unwrap();
    weiterleitung::alias::set_alias_enabled(&app.pool, &alias.alias_id, false)
        .await
        .unwrap();

    let router = AliasRouter::new(app.pool.clone(), &app.settings);
    for recipient in [
        "burner@example.com",
        "nobody@example.com",
        "someone@not-our-domain.example",
    ] {
        assert!(matches!(
            router
                .verify_recipient("stranger@shop.example", recipient)
                .await,
            RecipientDecision::Reject(_)
        ));
    }

    let alias = find_alias_by_address(&app.pool, &alias_address)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(alias.alias.blocked_count, 1);
}
