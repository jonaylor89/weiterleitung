//! The inbound SMTP server as a remote MTA sees it.

use crate::harness::spawn_app_with_smtp;
use weiterleitung::alias::{insert_alias, set_alias_enabled};
use weiterleitung::delivery::list_recent;
use weiterleitung::domain::EmailAddress;
use weiterleitung::mailbox::insert_mailbox;

#[tokio::test]
async fn the_server_announces_its_limits_and_refuses_plaintext_upgrades() {
    let app = spawn_app_with_smtp(|settings| settings.inbound.max_message_size = 1024).await;
    let mut session = app.smtp().await;

    let ehlo = session.command("EHLO tester.example").await;
    assert!(ehlo.contains("250-SIZE 1024"), "{ehlo}");
    assert!(ehlo.contains("8BITMIME"), "{ehlo}");

    // No inbound STARTTLS yet, and a wrong answer here would have the peer
    // start a TLS handshake into a clear-text socket.
    assert!(session.command("STARTTLS").await.starts_with("454"));
    assert!(
        session
            .command("RCPT TO:<x@example.com>")
            .await
            .starts_with("503")
    );
    session.quit().await;
}

#[tokio::test]
async fn unknown_and_disabled_recipients_are_rejected_at_rcpt() {
    let app = spawn_app_with_smtp(|_| {}).await;
    let mailbox = insert_mailbox(
        &app.pool,
        &EmailAddress::parse("me@personal.example").unwrap(),
        true,
    )
    .await
    .unwrap();
    let alias = insert_alias(
        &app.pool,
        &EmailAddress::parse("burner@example.com").unwrap(),
        &mailbox.mailbox_id,
        None,
    )
    .await
    .unwrap();
    set_alias_enabled(&app.pool, &alias.alias_id, false)
        .await
        .unwrap();

    let mut session = app.smtp().await;
    session.command("EHLO tester.example").await;
    session.command("MAIL FROM:<stranger@shop.example>").await;
    for recipient in ["burner@example.com", "nobody@example.com"] {
        let reply = session.command(&format!("RCPT TO:<{recipient}>")).await;
        assert!(
            reply.starts_with("550"),
            "{recipient} was accepted: {reply}"
        );
    }
    // Nothing was accepted, so DATA has nothing to work with.
    assert!(session.command("DATA").await.starts_with("503"));
    session.quit().await;

    assert!(list_recent(&app.pool, 10).await.unwrap().is_empty());
}

/// Bigger than `SIZE`: the message is refused outright rather than silently
/// truncated, and nothing is queued.
#[tokio::test]
async fn oversized_messages_are_refused() {
    let app = spawn_app_with_smtp(|settings| settings.inbound.max_message_size = 2048).await;
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

    let mut session = app.smtp().await;
    let reply = session
        .send_mail(
            "support@shop.example",
            &alias_address.to_string(),
            &format!("Subject: Big\n\n{}\n", "x".repeat(4096)),
        )
        .await;
    assert!(reply.starts_with("552"), "{reply}");

    // The session stays usable for the next transaction.
    assert!(session.command("RSET").await.starts_with("250"));
    session.quit().await;

    assert!(list_recent(&app.pool, 10).await.unwrap().is_empty());
}
