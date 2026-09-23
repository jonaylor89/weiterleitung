//! Alias routing: what the SMTP server decides and what it queues.

use crate::harness::spawn_app;
use weiterleitung::alias::{find_alias_by_address, insert_alias};
use weiterleitung::contact::find_contact_by_reverse_alias;
use weiterleitung::delivery::list_recent;
use weiterleitung::domain::EmailAddress;
use weiterleitung::mailbox::insert_mailbox;
use weiterleitung::smtp::{AliasRouter, Envelope, MailHandler, RecipientDecision};

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
