pub mod rewrite;
pub mod router;
mod server;
pub mod session;

pub use router::AliasRouter;
pub use server::{SmtpServer, run_smtp_server_until_stopped};
pub use session::{Envelope, MailHandler, RecipientDecision, SessionConfig, handle_session};
