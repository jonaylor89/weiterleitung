//! The outbox and the relay, without a real mail server in the way.

use crate::harness::spawn_app;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use weiterleitung::alias::insert_alias;
use weiterleitung::configuration::{DeliveryMode, DkimSettings, RelaySettings, get_configuration};
use weiterleitung::delivery::{Relay, claim_due, requeue_interrupted};
use weiterleitung::domain::EmailAddress;
use weiterleitung::mailbox::insert_mailbox;
use weiterleitung::smtp::{AliasRouter, Envelope, MailHandler};

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
async fn messages_left_mid_delivery_by_a_crash_are_requeued() {
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
    AliasRouter::new(app.pool.clone(), &app.settings)
        .handle(Envelope {
            mail_from: "support@shop.example".to_string(),
            rcpt_to: vec![alias_address.to_string()],
            data: b"From: support@shop.example\r\nSubject: Hi\r\n\r\nHello\r\n".to_vec(),
        })
        .await
        .unwrap();

    // The worker claims the message and then dies before reporting an outcome.
    let claimed = claim_due(&app.pool, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(claim_due(&app.pool, 10).await.unwrap().is_empty());

    assert_eq!(requeue_interrupted(&app.pool).await.unwrap(), 1);
    assert_eq!(claim_due(&app.pool, 10).await.unwrap().len(), 1);
}
