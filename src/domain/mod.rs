mod email_address;

pub use email_address::EmailAddress;

/// Which way a queued message is travelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Contact -> alias -> mailbox.
    Forward,
    /// Mailbox -> reverse alias -> contact.
    Reply,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Forward => "forward",
            Direction::Reply => "reply",
        }
    }
}

impl std::str::FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "forward" => Ok(Direction::Forward),
            "reply" => Ok(Direction::Reply),
            other => Err(format!("unknown direction: {other}")),
        }
    }
}
