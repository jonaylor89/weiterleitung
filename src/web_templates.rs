use askama::Template;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

use crate::session_state::FlashMessage;

#[derive(Debug, Clone)]
pub struct AliasView {
    pub alias_id: String,
    pub address: String,
    pub mailbox_email: String,
    pub note: String,
    pub enabled: bool,
    pub forward_count: i64,
    pub reply_count: i64,
    pub blocked_count: i64,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct MailboxView {
    pub mailbox_id: String,
    pub email: String,
    pub is_default: bool,
    pub alias_count: i64,
}

#[derive(Debug, Clone)]
pub struct MessageView {
    pub direction: String,
    pub envelope_from: String,
    pub envelope_to: String,
    pub subject: String,
    pub status: String,
    pub attempts: i64,
    pub last_error: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct ContactView {
    pub email: String,
    pub name: String,
    pub reverse_alias: String,
    pub created_at: String,
}

#[derive(Template)]
#[template(path = "web/login.html")]
pub struct LoginTemplate {
    pub flash_messages: Vec<FlashMessage>,
}

#[derive(Template)]
#[template(path = "web/dashboard.html")]
pub struct DashboardTemplate {
    pub flash_messages: Vec<FlashMessage>,
    pub aliases: Vec<AliasView>,
    pub mailboxes: Vec<MailboxView>,
    pub messages: Vec<MessageView>,
    pub default_domain: String,
}

#[derive(Template)]
#[template(path = "web/mailboxes.html")]
pub struct MailboxesTemplate {
    pub flash_messages: Vec<FlashMessage>,
    pub mailboxes: Vec<MailboxView>,
}

#[derive(Template)]
#[template(path = "web/alias.html")]
pub struct AliasTemplate {
    pub flash_messages: Vec<FlashMessage>,
    pub alias: AliasView,
    pub contacts: Vec<ContactView>,
}

/// Renders a template, turning a template error into a 500 instead of panicking.
pub fn render(template: impl Template) -> Response {
    match template.render() {
        Ok(body) => Html(body).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to render a template");
            (StatusCode::INTERNAL_SERVER_ERROR, "Template error").into_response()
        }
    }
}
