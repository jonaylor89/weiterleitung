use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// One message as handed over by a client.
#[derive(Debug, Clone, Default)]
pub struct Envelope {
    pub mail_from: String,
    pub rcpt_to: Vec<String>,
    pub data: Vec<u8>,
}

/// Verdict for a `RCPT TO` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientDecision {
    Accept,
    Reject(String),
}

/// What the SMTP server does with the mail it accepts. Kept behind a trait so
/// the protocol loop can be exercised without a database.
pub trait MailHandler: Send + Sync {
    fn verify_recipient(
        &self,
        mail_from: &str,
        recipient: &str,
    ) -> impl Future<Output = RecipientDecision> + Send;

    fn handle(&self, envelope: Envelope) -> impl Future<Output = Result<(), String>> + Send;
}

pub struct SessionConfig {
    pub hostname: String,
    pub max_message_size: usize,
}

const MAX_RECIPIENTS: usize = 50;
const MAX_LINE_LENGTH: usize = 4096;

/// Runs the RFC 5321 conversation for a single connection.
pub async fn handle_session<S, H>(
    stream: S,
    handler: &H,
    config: &SessionConfig,
) -> Result<(), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
    H: MailHandler,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut envelope = Envelope::default();
    let mut greeted = false;
    // Tracked separately from the address: the null sender `<>` is legal and
    // arrives as an empty string.
    let mut have_sender = false;

    write_line(
        &mut writer,
        &format!("220 {} ESMTP weiterleitung", config.hostname),
    )
    .await?;

    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let command = line.trim_end_matches(['\r', '\n']);
        let (verb, rest) = match command.split_once(' ') {
            Some((verb, rest)) => (verb.to_ascii_uppercase(), rest.trim()),
            None => (command.to_ascii_uppercase(), ""),
        };

        match verb.as_str() {
            "EHLO" => {
                greeted = true;
                have_sender = false;
                envelope = Envelope::default();
                write_line(&mut writer, &format!("250-{} greets you", config.hostname)).await?;
                write_line(
                    &mut writer,
                    &format!("250-SIZE {}", config.max_message_size),
                )
                .await?;
                write_line(&mut writer, "250-8BITMIME").await?;
                write_line(&mut writer, "250 SMTPUTF8").await?;
            }
            "HELO" => {
                greeted = true;
                have_sender = false;
                envelope = Envelope::default();
                write_line(&mut writer, &format!("250 {}", config.hostname)).await?;
            }
            "MAIL" => {
                if !greeted {
                    write_line(&mut writer, "503 Bad sequence of commands").await?;
                    continue;
                }
                match parse_address_argument(rest, "FROM") {
                    Some(address) => {
                        have_sender = true;
                        envelope = Envelope {
                            mail_from: address,
                            ..Envelope::default()
                        };
                        write_line(&mut writer, "250 2.1.0 Sender ok").await?;
                    }
                    None => {
                        write_line(&mut writer, "501 5.5.4 Syntax: MAIL FROM:<address>").await?;
                    }
                }
            }
            "RCPT" => {
                if !have_sender {
                    write_line(&mut writer, "503 5.5.1 Need MAIL before RCPT").await?;
                    continue;
                }
                if envelope.rcpt_to.len() >= MAX_RECIPIENTS {
                    write_line(&mut writer, "452 4.5.3 Too many recipients").await?;
                    continue;
                }
                match parse_address_argument(rest, "TO") {
                    Some(address) => match handler
                        .verify_recipient(&envelope.mail_from, &address)
                        .await
                    {
                        RecipientDecision::Accept => {
                            envelope.rcpt_to.push(address);
                            write_line(&mut writer, "250 2.1.5 Recipient ok").await?;
                        }
                        RecipientDecision::Reject(reason) => {
                            write_line(&mut writer, &format!("550 5.1.1 {reason}")).await?;
                        }
                    },
                    None => {
                        write_line(&mut writer, "501 5.5.4 Syntax: RCPT TO:<address>").await?;
                    }
                }
            }
            "DATA" => {
                if envelope.rcpt_to.is_empty() {
                    write_line(&mut writer, "503 5.5.1 Need RCPT before DATA").await?;
                    continue;
                }
                write_line(&mut writer, "354 End data with <CR><LF>.<CR><LF>").await?;

                match read_data(&mut reader, config.max_message_size).await? {
                    Ok(data) => {
                        envelope.data = data;
                        let reply = match handler.handle(std::mem::take(&mut envelope)).await {
                            Ok(()) => "250 2.0.0 Message accepted".to_string(),
                            Err(reason) => format!("451 4.3.0 {reason}"),
                        };
                        have_sender = false;
                        write_line(&mut writer, &reply).await?;
                    }
                    Err(reply) => {
                        have_sender = false;
                        envelope = Envelope::default();
                        write_line(&mut writer, &reply).await?;
                    }
                }
            }
            "RSET" => {
                have_sender = false;
                envelope = Envelope::default();
                write_line(&mut writer, "250 2.0.0 Ok").await?;
            }
            "NOOP" => write_line(&mut writer, "250 2.0.0 Ok").await?,
            "VRFY" | "EXPN" => {
                write_line(&mut writer, "252 2.5.2 Cannot verify recipients").await?
            }
            "STARTTLS" => write_line(&mut writer, "454 4.7.0 TLS not available").await?,
            "QUIT" => {
                write_line(&mut writer, "221 2.0.0 Bye").await?;
                return Ok(());
            }
            "" => write_line(&mut writer, "500 5.5.2 Error: bad syntax").await?,
            _ => write_line(&mut writer, "502 5.5.1 Command not implemented").await?,
        }
    }
}

/// Reads the DATA payload, undoing dot stuffing. The outer error is I/O; the
/// inner one carries the SMTP reply to send back.
async fn read_data<R>(
    reader: &mut BufReader<R>,
    max_message_size: usize,
) -> Result<Result<Vec<u8>, String>, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut data: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut line: Vec<u8> = Vec::with_capacity(1024);
    let mut too_large = false;

    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(Err("421 4.4.2 Connection closed during DATA".to_string()));
        }

        let content = strip_eol(&line);
        if content == b"." {
            break;
        }

        if too_large {
            continue;
        }

        // RFC 5321 §4.5.2: a leading dot is stuffed by the client.
        let content = content.strip_prefix(b".").unwrap_or(content);
        if data.len() + content.len() + 2 > max_message_size {
            too_large = true;
            continue;
        }
        data.extend_from_slice(content);
        data.extend_from_slice(b"\r\n");
    }

    if too_large {
        return Ok(Err("552 5.3.4 Message too large".to_string()));
    }
    Ok(Ok(data))
}

fn strip_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Parses `FROM:<addr>` / `TO:<addr>`, dropping any ESMTP parameters that
/// follow. An empty path (the null sender) comes back as an empty string.
fn parse_address_argument(rest: &str, keyword: &str) -> Option<String> {
    if rest.len() > MAX_LINE_LENGTH {
        return None;
    }
    let rest = rest.trim();
    let (candidate, value) = rest.split_once(':')?;
    if !candidate.trim().eq_ignore_ascii_case(keyword) {
        return None;
    }

    let value = value.trim();
    let address = if let Some(end) = value.find('>') {
        value.strip_prefix('<')?.get(..end.saturating_sub(1))?
    } else {
        value.split_whitespace().next().unwrap_or_default()
    };

    Some(address.trim().to_string())
}

async fn write_line<W>(writer: &mut W, line: &str) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\r\n").await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::{
        Envelope, MailHandler, RecipientDecision, SessionConfig, handle_session,
        parse_address_argument,
    };
    use std::sync::Mutex;
    use tokio::io::duplex;

    #[derive(Default)]
    struct RecordingHandler {
        known_recipient: String,
        received: Mutex<Vec<Envelope>>,
    }

    impl MailHandler for RecordingHandler {
        async fn verify_recipient(&self, _mail_from: &str, recipient: &str) -> RecipientDecision {
            if recipient.eq_ignore_ascii_case(&self.known_recipient) {
                RecipientDecision::Accept
            } else {
                RecipientDecision::Reject(format!("Unknown recipient {recipient}"))
            }
        }

        async fn handle(&self, envelope: Envelope) -> Result<(), String> {
            self.received.lock().unwrap().push(envelope);
            Ok(())
        }
    }

    #[test]
    fn address_arguments_are_parsed() {
        assert_eq!(
            parse_address_argument("FROM:<a@b.com> SIZE=100", "FROM").as_deref(),
            Some("a@b.com")
        );
        assert_eq!(
            parse_address_argument("TO: <a@b.com>", "TO").as_deref(),
            Some("a@b.com")
        );
        assert_eq!(
            parse_address_argument("FROM:<>", "FROM").as_deref(),
            Some("")
        );
        assert!(parse_address_argument("TO:<a@b.com>", "FROM").is_none());
    }

    #[tokio::test]
    async fn a_full_transaction_is_accepted_and_dot_unstuffed() {
        let handler = RecordingHandler {
            known_recipient: "alias@example.com".to_string(),
            ..RecordingHandler::default()
        };
        let config = SessionConfig {
            hostname: "mail.example.com".to_string(),
            max_message_size: 1024 * 1024,
        };

        let (client, server) = duplex(64 * 1024);
        let session = tokio::spawn(async move {
            let handler = handler;
            handle_session(server, &handler, &config).await.unwrap();
            handler
        });

        let transcript = concat!(
            "EHLO client.example\r\n",
            "MAIL FROM:<sender@shop.example>\r\n",
            "RCPT TO:<nobody@example.com>\r\n",
            "RCPT TO:<alias@example.com>\r\n",
            "DATA\r\n",
            "Subject: Hi\r\n",
            "\r\n",
            "..leading dot\r\n",
            ".\r\n",
            "QUIT\r\n",
        );
        let replies = super::tests::exchange(client, transcript).await;

        assert!(replies.contains("220 mail.example.com ESMTP weiterleitung"));
        assert!(replies.contains("550 5.1.1 Unknown recipient nobody@example.com"));
        assert!(replies.contains("250 2.0.0 Message accepted"));
        assert!(replies.contains("221 2.0.0 Bye"));

        let handler = session.await.unwrap();
        let received = handler.received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].mail_from, "sender@shop.example");
        assert_eq!(received[0].rcpt_to, vec!["alias@example.com".to_string()]);
        assert_eq!(
            String::from_utf8(received[0].data.clone()).unwrap(),
            "Subject: Hi\r\n\r\n.leading dot\r\n"
        );
    }

    #[tokio::test]
    async fn data_before_rcpt_is_refused() {
        let handler = RecordingHandler {
            known_recipient: "alias@example.com".to_string(),
            ..RecordingHandler::default()
        };
        let config = SessionConfig {
            hostname: "mail.example.com".to_string(),
            max_message_size: 1024,
        };

        let (client, server) = duplex(8 * 1024);
        tokio::spawn(async move { handle_session(server, &handler, &config).await });

        let replies = super::tests::exchange(
            client,
            "EHLO client.example\r\nMAIL FROM:<a@b.com>\r\nDATA\r\nQUIT\r\n",
        )
        .await;
        assert!(replies.contains("503 5.5.1 Need RCPT before DATA"));
    }

    async fn exchange(mut client: tokio::io::DuplexStream, transcript: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        client.write_all(transcript.as_bytes()).await.unwrap();
        client.flush().await.unwrap();
        let mut replies = String::new();
        client.read_to_string(&mut replies).await.unwrap();
        replies
    }
}
