//! End-to-end tests against a real SMTP server.
//!
//! Mail is posted over the wire to the inbound SMTP server, drained by the real
//! delivery worker, and read back out of Mailpit, so these cover the whole path
//! including the parts a hand-written sink cannot: ESMTP negotiation, dot
//! stuffing, 8-bit content, and the server's own view of the envelope.

use crate::harness::{Mailpit, MailpitOptions, spawn_app_with_smtp};
use std::time::Duration;
use weiterleitung::alias::insert_alias;
use weiterleitung::configuration::{DeliveryMode, RelaySettings, Settings};
use weiterleitung::delivery::{
    Relay, drain_outbox, list_recent, run_delivery_worker_until_stopped,
};
use weiterleitung::domain::EmailAddress;
use weiterleitung::mailbox::insert_mailbox;

const MAILBOX: &str = "me@personal.example";
const CONTACT: &str = "support@shop.example";
const ALIAS: &str = "shop.1a2b@example.com";

/// Points the delivery settings at Mailpit, which speaks clear-text SMTP.
fn relay_through(mailpit: &Mailpit) -> impl FnOnce(&mut Settings) + '_ {
    move |settings: &mut Settings| {
        settings.delivery.mode = DeliveryMode::Relay;
        settings.delivery.relay = Some(RelaySettings {
            host: "127.0.0.1".to_string(),
            port: mailpit.smtp_port,
            username: None,
            password: None,
            implicit_tls: false,
        });
    }
}

/// Seeds the mailbox and the alias every test in this module uses.
async fn seed(pool: &sqlx::SqlitePool) {
    let mailbox = insert_mailbox(pool, &EmailAddress::parse(MAILBOX).unwrap(), true)
        .await
        .unwrap();
    insert_alias(
        pool,
        &EmailAddress::parse(ALIAS).unwrap(),
        &mailbox.mailbox_id,
        None,
    )
    .await
    .unwrap();
}

/// Runs the real worker pass and asserts everything left the queue.
async fn deliver_everything(app: &crate::harness::TestApp) {
    let relay = Relay::build(&app.settings.delivery, &app.settings.dkim)
        .expect("Failed to build the relay");
    let claimed = drain_outbox(&app.pool, &relay, app.settings.delivery.max_attempts)
        .await
        .expect("The delivery pass failed");
    assert!(claimed > 0, "The worker found nothing to deliver");

    for message in list_recent(&app.pool, 50).await.unwrap() {
        assert_eq!(
            message.status, "delivered",
            "{} was not delivered: {:?}",
            message.message_id, message.last_error
        );
    }
}

/// A stranger mails the alias; the mailbox receives it from a reverse alias
/// that hides the contact's address from nobody but exposes ours to no one.
#[tokio::test]
async fn a_forwarded_message_arrives_at_the_mailbox_through_a_reverse_alias() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(relay_through(&mailpit)).await;
    seed(&app.pool).await;

    let mut session = app.smtp().await;
    let reply = session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\n\
             To: shop.1a2b@example.com\n\
             Subject: Your order\n\
             \n\
             It shipped. Tracking: 12345.\n\
             .. a line that must survive dot stuffing\n",
        )
        .await;
    assert!(reply.starts_with("250"), "The message was refused: {reply}");
    session.quit().await;

    deliver_everything(&app).await;

    let messages = mailpit.wait_for_messages(1).await;
    let message = &messages[0];
    assert_eq!(message.subject, "Your order");

    // The contact never appears as the sender: the mailbox sees the reverse
    // alias, with the real address only in the display name.
    let reverse_alias = &message.from.address;
    assert!(
        reverse_alias.starts_with("ct.") && reverse_alias.ends_with("@example.com"),
        "Unexpected sender: {reverse_alias}"
    );
    assert!(message.from.name.contains(CONTACT));
    assert_eq!(message.reply_to[0].address, *reverse_alias);

    let (envelope_from, envelope_to) = mailpit.envelope(&message.id).await;
    assert_eq!(envelope_from, *reverse_alias);
    assert_eq!(envelope_to, MAILBOX);

    let raw = mailpit.raw(&message.id).await;
    assert!(raw.contains("It shipped. Tracking: 12345."));
    // The client stuffed the leading dot, so both servers must have unstuffed
    // it exactly once on the way through.
    assert!(
        raw.contains("\n. a line that must survive dot stuffing"),
        "Dot stuffing mangled the body:\n{raw}"
    );
}

/// The mailbox answers the reverse alias; the contact sees the public alias and
/// never the mailbox address.
#[tokio::test]
async fn a_reply_reaches_the_contact_without_exposing_the_mailbox() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(relay_through(&mailpit)).await;
    seed(&app.pool).await;

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;
    deliver_everything(&app).await;

    let reverse_alias = mailpit.wait_for_messages(1).await[0].from.address.clone();

    let mut session = app.smtp().await;
    let reply = session
        .send_mail(
            MAILBOX,
            &reverse_alias,
            &format!(
                "From: {MAILBOX}\n\
                 To: {reverse_alias}\n\
                 Subject: Re: Your order\n\
                 \n\
                 Thanks, got it.\n"
            ),
        )
        .await;
    assert!(reply.starts_with("250"), "The reply was refused: {reply}");
    session.quit().await;
    deliver_everything(&app).await;

    let messages = mailpit.wait_for_messages(2).await;
    let reply = messages
        .iter()
        .find(|message| message.subject == "Re: Your order")
        .expect("The reply never reached the contact");
    assert_eq!(reply.from.address, ALIAS);

    let (envelope_from, envelope_to) = mailpit.envelope(&reply.id).await;
    assert_eq!(envelope_from, ALIAS);
    assert_eq!(envelope_to, CONTACT);

    let raw = mailpit.raw(&reply.id).await;
    assert!(
        !raw.contains(MAILBOX),
        "The reply leaked the mailbox address:\n{raw}"
    );
}

/// A rejection from the receiving server must leave the message queued for a
/// later attempt rather than dropping it, and the retry must succeed.
#[tokio::test]
async fn a_temporary_rejection_is_retried_until_it_is_accepted() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(relay_through(&mailpit)).await;
    seed(&app.pool).await;
    mailpit.set_recipient_rejection(451, 100).await;

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;

    let relay = Relay::build(&app.settings.delivery, &app.settings.dkim).unwrap();
    drain_outbox(&app.pool, &relay, app.settings.delivery.max_attempts)
        .await
        .unwrap();

    let queued = &list_recent(&app.pool, 1).await.unwrap()[0];
    assert_eq!(queued.status, "pending");
    assert_eq!(queued.attempts, 1);
    assert!(queued.last_error.is_some());
    mailpit
        .assert_no_message_within(Duration::from_millis(250))
        .await;

    // The backoff would keep the message waiting, so bring its retry forward
    // the way a later worker pass would find it.
    sqlx::query("UPDATE outbox SET next_attempt_at = datetime('now', '-1 hour')")
        .execute(&app.pool)
        .await
        .unwrap();
    mailpit.set_recipient_rejection(451, 0).await;

    deliver_everything(&app).await;
    let delivered = mailpit.wait_for_messages(1).await;
    assert_eq!(mailpit.envelope(&delivered[0].id).await.1, MAILBOX);
}

/// A permanent rejection is not worth retrying forever: once the attempts run
/// out the message is parked as `failed`.
#[tokio::test]
async fn a_message_that_keeps_being_rejected_ends_up_failed() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(|settings| {
        relay_through(&mailpit)(settings);
        settings.delivery.max_attempts = 2;
    })
    .await;
    seed(&app.pool).await;
    mailpit.set_recipient_rejection(550, 100).await;

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;

    let relay = Relay::build(&app.settings.delivery, &app.settings.dkim).unwrap();
    for _ in 0..2 {
        sqlx::query("UPDATE outbox SET next_attempt_at = datetime('now', '-1 hour')")
            .execute(&app.pool)
            .await
            .unwrap();
        drain_outbox(&app.pool, &relay, app.settings.delivery.max_attempts)
            .await
            .unwrap();
    }

    let queued = &list_recent(&app.pool, 1).await.unwrap()[0];
    assert_eq!(queued.status, "failed");
    assert_eq!(queued.attempts, 2);
    mailpit
        .assert_no_message_within(Duration::from_millis(250))
        .await;
}

/// Delivery through an authenticated smarthost, the shape most self-hosted
/// setups use.
#[tokio::test]
async fn mail_can_be_relayed_through_an_authenticated_smarthost() {
    let mailpit = Mailpit::start_with(
        MailpitOptions::default()
            .arg("--smtp-auth-accept-any")
            .arg("--smtp-auth-allow-insecure"),
    )
    .await;
    let app = spawn_app_with_smtp(|settings| {
        settings.delivery.mode = DeliveryMode::Relay;
        settings.delivery.relay = Some(RelaySettings {
            host: "127.0.0.1".to_string(),
            port: mailpit.smtp_port,
            username: Some("weiterleitung".to_string()),
            password: Some(secrecy::Secret::new("hunter2".to_string())),
            implicit_tls: false,
        });
    })
    .await;
    seed(&app.pool).await;

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;

    deliver_everything(&app).await;
    let delivered = mailpit.wait_for_messages(1).await;
    assert_eq!(mailpit.envelope(&delivered[0].id).await.1, MAILBOX);
}

/// The tests above drive the worker by hand; this one runs the same loop the
/// binary spawns, so the polling wiring is covered too.
#[tokio::test]
async fn the_delivery_worker_sends_queued_mail_on_its_own() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(|settings| {
        relay_through(&mailpit)(settings);
        settings.delivery.poll_interval_secs = 1;
    })
    .await;
    seed(&app.pool).await;
    tokio::spawn(run_delivery_worker_until_stopped(app.settings.clone()));

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;

    let delivered = mailpit.wait_for_messages(1).await;
    assert_eq!(mailpit.envelope(&delivered[0].id).await.1, MAILBOX);
}

/// DKIM has to be signed over the message we actually put on the wire, so the
/// signature is checked on what the receiving server stored.
#[tokio::test]
async fn forwarded_mail_carries_a_dkim_signature_when_a_key_is_configured() {
    let mailpit = Mailpit::start().await;
    let app = spawn_app_with_smtp(|settings| {
        relay_through(&mailpit)(settings);
        settings.dkim.enabled = true;
        settings.dkim.private_key_path = Some(dkim_key_path());
        settings.dkim.selector = "test".to_string();
        settings.dkim.domain = "example.com".to_string();
    })
    .await;
    seed(&app.pool).await;

    let mut session = app.smtp().await;
    session
        .send_mail(
            CONTACT,
            ALIAS,
            "From: Shop Support <support@shop.example>\nSubject: Your order\n\nIt shipped.\n",
        )
        .await;
    session.quit().await;
    deliver_everything(&app).await;

    let messages = mailpit.wait_for_messages(1).await;
    let raw = mailpit.raw(&messages[0].id).await;
    let signature = raw
        .lines()
        .find(|line| line.starts_with("DKIM-Signature:"))
        .expect("The forwarded message was not signed");
    assert!(signature.contains("d=example.com"), "{signature}");
    assert!(signature.contains("s=test"), "{signature}");
}

/// Writes the test signing key to a temporary file. It is a throwaway key that
/// only ever signs mail delivered to a local Mailpit.
fn dkim_key_path() -> std::path::PathBuf {
    let path = std::env::temp_dir().join("weiterleitung-test-dkim.pem");
    std::fs::write(&path, include_str!("dkim_test_key.pem")).expect("Failed to write the test key");
    path
}
