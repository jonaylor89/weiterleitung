//! Header surgery on raw RFC 5322 messages.
//!
//! The MIME body is forwarded byte for byte; only the headers that would leak
//! the mailbox address — or that no longer hold after rewriting the sender —
//! are replaced.

/// Headers dropped from every message we relay: authentication results that
/// no longer apply once `From` changes, plus anything routing-related.
const STRIPPED_HEADERS: &[&str] = &[
    "from",
    "sender",
    "reply-to",
    "return-path",
    "dkim-signature",
    "domainkey-signature",
    "arc-authentication-results",
    "arc-message-signature",
    "arc-seal",
    "authentication-results",
    "received-spf",
    "x-weiterleitung-alias",
];

/// Splits a raw message into its header block and the body that follows the
/// first empty line. Handles both CRLF and bare LF line endings.
pub fn split_headers_body(raw: &[u8]) -> (&[u8], &[u8]) {
    if let Some(index) = find_subsequence(raw, b"\r\n\r\n") {
        return (&raw[..index + 2], &raw[index + 4..]);
    }
    if let Some(index) = find_subsequence(raw, b"\n\n") {
        return (&raw[..index + 1], &raw[index + 2..]);
    }
    (raw, &[])
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Returns the header block with every field in `names` removed, continuation
/// lines included.
pub fn strip_headers(headers: &[u8], names: &[&str]) -> Vec<u8> {
    let text = String::from_utf8_lossy(headers);
    let mut kept = Vec::with_capacity(headers.len());
    let mut dropping = false;

    for line in text.split_inclusive('\n') {
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');
        if !is_continuation {
            let name = line
                .split_once(':')
                .map(|(name, _)| name.trim().to_lowercase())
                .unwrap_or_default();
            dropping = names.iter().any(|candidate| *candidate == name);
        }
        if !dropping {
            kept.extend_from_slice(line.as_bytes());
        }
    }

    kept
}

/// Reads the (unfolded) value of a header, if present.
pub fn header_value(headers: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(headers);
    let mut value: Option<String> = None;

    for line in text.split_inclusive('\n') {
        let is_continuation = line.starts_with(' ') || line.starts_with('\t');
        if is_continuation {
            if let Some(value) = value.as_mut() {
                value.push(' ');
                value.push_str(line.trim());
                continue;
            }
        } else if value.is_some() {
            break;
        } else if let Some((candidate, rest)) = line.split_once(':')
            && candidate.trim().eq_ignore_ascii_case(name)
        {
            value = Some(rest.trim().to_string());
        }
    }

    value
}

pub struct ForwardRewrite<'a> {
    /// Address the message was sent to.
    pub alias: &'a str,
    /// Reverse alias the mailbox replies to in order to reach the sender.
    pub reverse_alias: &'a str,
    /// Display name of the original sender, if any.
    pub sender_name: Option<&'a str>,
    /// Original envelope/header sender.
    pub sender_email: &'a str,
}

/// Rewrites a message from a contact so it can be delivered to the mailbox:
/// the `From` becomes the contact's reverse alias, so a plain reply travels
/// back out through the alias.
pub fn rewrite_for_forward(raw: &[u8], params: &ForwardRewrite<'_>) -> Vec<u8> {
    let (headers, body) = split_headers_body(raw);
    let kept = strip_headers(headers, STRIPPED_HEADERS);

    let display = match params.sender_name {
        Some(name) if !name.trim().is_empty() => {
            format!("{} - {}", name.trim(), params.sender_email)
        }
        _ => params.sender_email.to_string(),
    };

    let mut out = Vec::with_capacity(raw.len() + 256);
    out.extend_from_slice(
        format!(
            "From: {}\r\nReply-To: {}\r\nX-Weiterleitung-Alias: {}\r\nX-Weiterleitung-Sender: {}\r\n",
            quoted_address(&display, params.reverse_alias),
            params.reverse_alias,
            params.alias,
            params.sender_email,
        )
        .as_bytes(),
    );
    out.extend_from_slice(&kept);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

pub struct ReplyRewrite<'a> {
    /// Alias the reply goes out as.
    pub alias: &'a str,
    /// Real address of the contact.
    pub contact_email: &'a str,
}

/// Rewrites a reply sent by the mailbox to a reverse alias so the contact only
/// ever sees the alias.
pub fn rewrite_for_reply(raw: &[u8], params: &ReplyRewrite<'_>) -> Vec<u8> {
    let (headers, body) = split_headers_body(raw);
    let mut stripped: Vec<&str> = STRIPPED_HEADERS.to_vec();
    stripped.extend_from_slice(&["to", "cc", "bcc", "x-weiterleitung-sender"]);
    let kept = strip_headers(headers, &stripped);

    let mut out = Vec::with_capacity(raw.len() + 128);
    out.extend_from_slice(
        format!(
            "From: {}\r\nTo: {}\r\nX-Weiterleitung-Alias: {}\r\n",
            params.alias, params.contact_email, params.alias,
        )
        .as_bytes(),
    );
    out.extend_from_slice(&kept);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(body);
    out
}

/// Formats `Display Name <address>`, quoting the display name when needed.
fn quoted_address(display: &str, address: &str) -> String {
    let needs_quoting = display
        .chars()
        .any(|c| matches!(c, '<' | '>' | '(' | ')' | ',' | ':' | ';' | '@' | '"' | '.'));
    let escaped = display.replace('\\', r"\\").replace('"', r#"\""#);
    if needs_quoting {
        format!("\"{escaped}\" <{address}>")
    } else {
        format!("{escaped} <{address}>")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ForwardRewrite, ReplyRewrite, header_value, rewrite_for_forward, rewrite_for_reply,
        split_headers_body, strip_headers,
    };

    const MESSAGE: &[u8] = b"From: Shop Support <support@shop.example>\r\n\
Subject: Your order\r\n\
DKIM-Signature: v=1; a=rsa-sha256;\r\n\tbh=abc; b=def\r\n\
To: cedar-ripple.4f8a@example.com\r\n\
\r\n\
Hello there\r\n";

    #[test]
    fn headers_and_body_are_split_on_the_blank_line() {
        let (headers, body) = split_headers_body(MESSAGE);
        assert!(headers.starts_with(b"From: Shop Support"));
        assert_eq!(body, b"Hello there\r\n");
    }

    #[test]
    fn stripping_removes_folded_continuation_lines() {
        let (headers, _) = split_headers_body(MESSAGE);
        let stripped = strip_headers(headers, &["dkim-signature"]);
        let stripped = String::from_utf8(stripped).unwrap();
        assert!(!stripped.contains("DKIM-Signature"));
        assert!(!stripped.contains("bh=abc"));
        assert!(stripped.contains("Subject: Your order"));
    }

    #[test]
    fn folded_header_values_are_unfolded() {
        let (headers, _) = split_headers_body(MESSAGE);
        assert_eq!(
            header_value(headers, "from").as_deref(),
            Some("Shop Support <support@shop.example>")
        );
        assert_eq!(
            header_value(headers, "dkim-signature").as_deref(),
            Some("v=1; a=rsa-sha256; bh=abc; b=def")
        );
        assert!(header_value(headers, "x-missing").is_none());
    }

    #[test]
    fn forwarding_replaces_the_sender_with_the_reverse_alias() {
        let rewritten = rewrite_for_forward(
            MESSAGE,
            &ForwardRewrite {
                alias: "cedar-ripple.4f8a@example.com",
                reverse_alias: "ct.abc123@example.com",
                sender_name: Some("Shop Support"),
                sender_email: "support@shop.example",
            },
        );
        let rewritten = String::from_utf8(rewritten).unwrap();

        assert!(rewritten.starts_with(
            "From: \"Shop Support - support@shop.example\" <ct.abc123@example.com>\r\n"
        ));
        assert!(rewritten.contains("Reply-To: ct.abc123@example.com\r\n"));
        assert!(rewritten.contains("X-Weiterleitung-Alias: cedar-ripple.4f8a@example.com\r\n"));
        assert!(!rewritten.contains("From: Shop Support <support@shop.example>"));
        assert!(!rewritten.contains("DKIM-Signature"));
        assert!(rewritten.ends_with("\r\nHello there\r\n"));
    }

    #[test]
    fn replies_go_out_as_the_alias_and_hide_the_mailbox() {
        let reply = b"From: Johannes <me@personal.example>\r\n\
To: ct.abc123@example.com\r\n\
Cc: someone@personal.example\r\n\
Subject: Re: Your order\r\n\
\r\n\
Thanks!\r\n";

        let rewritten = rewrite_for_reply(
            reply,
            &ReplyRewrite {
                alias: "cedar-ripple.4f8a@example.com",
                contact_email: "support@shop.example",
            },
        );
        let rewritten = String::from_utf8(rewritten).unwrap();

        assert!(rewritten.starts_with("From: cedar-ripple.4f8a@example.com\r\n"));
        assert!(rewritten.contains("To: support@shop.example\r\n"));
        assert!(!rewritten.contains("me@personal.example"));
        assert!(!rewritten.contains("Cc:"));
        assert!(rewritten.contains("Subject: Re: Your order"));
        assert!(rewritten.ends_with("\r\nThanks!\r\n"));
    }
}
