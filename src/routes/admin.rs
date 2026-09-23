use axum::Form;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};

use crate::alias::{
    delete_alias as delete_alias_record, find_alias_by_address, insert_alias, list_aliases,
    random_local_part, set_alias_enabled,
};
use crate::contact::list_contacts_for_alias;
use crate::delivery::list_recent;
use crate::domain::EmailAddress;
use crate::mailbox::{
    delete_mailbox as delete_mailbox_record, insert_mailbox, list_mailboxes_with_alias_counts,
};
use crate::session_state::TypedSession;
use crate::startup::AppState;
use crate::web_templates::{
    AliasTemplate, AliasView, ContactView, DashboardTemplate, MailboxView, MailboxesTemplate,
    MessageView, StyleguideTemplate, render,
};

const RECENT_MESSAGE_LIMIT: i64 = 50;

#[tracing::instrument(name = "Admin dashboard", skip(state, session))]
pub async fn dashboard(State(state): State<AppState>, session: TypedSession) -> Response {
    let aliases = match list_aliases(&state.db_pool).await {
        Ok(aliases) => aliases,
        Err(e) => return internal_error(e, "Failed to list the aliases"),
    };
    let mailboxes = match list_mailboxes_with_alias_counts(&state.db_pool).await {
        Ok(mailboxes) => mailboxes,
        Err(e) => return internal_error(e, "Failed to list the mailboxes"),
    };
    let messages = match list_recent(&state.db_pool, RECENT_MESSAGE_LIMIT).await {
        Ok(messages) => messages,
        Err(e) => return internal_error(e, "Failed to list the recent messages"),
    };

    render(DashboardTemplate {
        flash_messages: session.get_flash_messages().await,
        aliases: aliases
            .into_iter()
            .map(|alias| AliasView {
                alias_id: alias.alias.alias_id,
                address: alias.alias.address,
                mailbox_email: alias.mailbox_email,
                note: alias.alias.note.unwrap_or_default(),
                enabled: alias.alias.enabled,
                forward_count: alias.alias.forward_count,
                reply_count: alias.alias.reply_count,
                blocked_count: alias.alias.blocked_count,
                created_at: alias.alias.created_at.format("%Y-%m-%d").to_string(),
            })
            .collect(),
        mailboxes: mailboxes
            .into_iter()
            .map(|(mailbox, alias_count)| MailboxView {
                mailbox_id: mailbox.mailbox_id,
                email: mailbox.email,
                is_default: mailbox.is_default,
                alias_count,
            })
            .collect(),
        messages: messages
            .into_iter()
            .map(|message| MessageView {
                direction: message.direction,
                envelope_from: message.envelope_from,
                envelope_to: message.envelope_to,
                subject: message.subject.unwrap_or_default(),
                status: message.status,
                attempts: message.attempts,
                last_error: message.last_error.unwrap_or_default(),
                created_at: message.created_at.format("%Y-%m-%d %H:%M").to_string(),
            })
            .collect(),
        default_domain: state.aliases.default_domain().to_string(),
    })
}

#[derive(serde::Deserialize)]
pub struct CreateAliasForm {
    local_part: String,
    mailbox_id: String,
    note: String,
}

#[tracing::instrument(name = "Create alias", skip(state, session, form))]
pub async fn create_alias(
    State(state): State<AppState>,
    session: TypedSession,
    Form(form): Form<CreateAliasForm>,
) -> Response {
    let local_part = if form.local_part.trim().is_empty() {
        random_local_part()
    } else {
        form.local_part.trim().to_string()
    };
    let address = format!("{local_part}@{}", state.aliases.default_domain());

    let address = match EmailAddress::parse(&address) {
        Ok(address) => address,
        Err(e) => {
            session.flash_error(e).await;
            return Redirect::to("/admin/dashboard").into_response();
        }
    };

    match find_alias_by_address(&state.db_pool, &address).await {
        Ok(Some(_)) => {
            session
                .flash_error(format!("{address} already exists"))
                .await;
            return Redirect::to("/admin/dashboard").into_response();
        }
        Ok(None) => {}
        Err(e) => return internal_error(e, "Failed to check for an existing alias"),
    }

    let note = (!form.note.trim().is_empty()).then(|| form.note.trim().to_string());
    match insert_alias(&state.db_pool, &address, &form.mailbox_id, note).await {
        Ok(alias) => {
            session
                .flash_info(format!("Created {}", alias.address))
                .await
        }
        Err(e) => {
            tracing::error!(error.cause_chain = ?e, "Failed to create the alias");
            session.flash_error("Failed to create the alias").await;
        }
    }

    Redirect::to("/admin/dashboard").into_response()
}

#[tracing::instrument(name = "Toggle alias", skip(state, session))]
pub async fn toggle_alias(
    State(state): State<AppState>,
    session: TypedSession,
    Path(alias_id): Path<String>,
) -> Response {
    let aliases = match list_aliases(&state.db_pool).await {
        Ok(aliases) => aliases,
        Err(e) => return internal_error(e, "Failed to list the aliases"),
    };
    let Some(alias) = aliases
        .into_iter()
        .find(|alias| alias.alias.alias_id == alias_id)
    else {
        session.flash_error("No such alias").await;
        return Redirect::to("/admin/dashboard").into_response();
    };

    if let Err(e) = set_alias_enabled(&state.db_pool, &alias_id, !alias.alias.enabled).await {
        return internal_error(e, "Failed to toggle the alias");
    }
    session
        .flash_info(format!(
            "{} is now {}",
            alias.alias.address,
            if alias.alias.enabled {
                "disabled"
            } else {
                "enabled"
            }
        ))
        .await;
    Redirect::to("/admin/dashboard").into_response()
}

#[tracing::instrument(name = "Delete alias", skip(state, session))]
pub async fn delete_alias(
    State(state): State<AppState>,
    session: TypedSession,
    Path(alias_id): Path<String>,
) -> Response {
    if let Err(e) = delete_alias_record(&state.db_pool, &alias_id).await {
        return internal_error(e, "Failed to delete the alias");
    }
    session.flash_info("Alias deleted").await;
    Redirect::to("/admin/dashboard").into_response()
}

#[tracing::instrument(name = "Alias detail", skip(state, session))]
pub async fn alias_detail(
    State(state): State<AppState>,
    session: TypedSession,
    Path(alias_id): Path<String>,
) -> Response {
    let aliases = match list_aliases(&state.db_pool).await {
        Ok(aliases) => aliases,
        Err(e) => return internal_error(e, "Failed to list the aliases"),
    };
    let Some(alias) = aliases
        .into_iter()
        .find(|alias| alias.alias.alias_id == alias_id)
    else {
        session.flash_error("No such alias").await;
        return Redirect::to("/admin/dashboard").into_response();
    };

    let contacts = match list_contacts_for_alias(&state.db_pool, &alias_id).await {
        Ok(contacts) => contacts,
        Err(e) => return internal_error(e, "Failed to list the contacts"),
    };

    render(AliasTemplate {
        flash_messages: session.get_flash_messages().await,
        alias: AliasView {
            alias_id: alias.alias.alias_id,
            address: alias.alias.address,
            mailbox_email: alias.mailbox_email,
            note: alias.alias.note.unwrap_or_default(),
            enabled: alias.alias.enabled,
            forward_count: alias.alias.forward_count,
            reply_count: alias.alias.reply_count,
            blocked_count: alias.alias.blocked_count,
            created_at: alias.alias.created_at.format("%Y-%m-%d").to_string(),
        },
        contacts: contacts
            .into_iter()
            .map(|contact| ContactView {
                email: contact.email,
                name: contact.name.unwrap_or_default(),
                reverse_alias: contact.reverse_alias,
                created_at: contact.created_at.format("%Y-%m-%d").to_string(),
            })
            .collect(),
    })
}

#[tracing::instrument(name = "Mailboxes", skip(state, session))]
pub async fn mailboxes(State(state): State<AppState>, session: TypedSession) -> Response {
    let mailboxes = match list_mailboxes_with_alias_counts(&state.db_pool).await {
        Ok(mailboxes) => mailboxes,
        Err(e) => return internal_error(e, "Failed to list the mailboxes"),
    };

    render(MailboxesTemplate {
        flash_messages: session.get_flash_messages().await,
        mailboxes: mailboxes
            .into_iter()
            .map(|(mailbox, alias_count)| MailboxView {
                mailbox_id: mailbox.mailbox_id,
                email: mailbox.email,
                is_default: mailbox.is_default,
                alias_count,
            })
            .collect(),
    })
}

#[derive(serde::Deserialize)]
pub struct CreateMailboxForm {
    email: String,
    #[serde(default)]
    is_default: Option<String>,
}

#[tracing::instrument(name = "Create mailbox", skip(state, session, form))]
pub async fn create_mailbox(
    State(state): State<AppState>,
    session: TypedSession,
    Form(form): Form<CreateMailboxForm>,
) -> Response {
    let email = match EmailAddress::parse(&form.email) {
        Ok(email) => email,
        Err(e) => {
            session.flash_error(e).await;
            return Redirect::to("/admin/mailboxes").into_response();
        }
    };

    match insert_mailbox(&state.db_pool, &email, form.is_default.is_some()).await {
        Ok(mailbox) => session.flash_info(format!("Added {}", mailbox.email)).await,
        Err(e) => {
            tracing::error!(error.cause_chain = ?e, "Failed to add the mailbox");
            session.flash_error("Failed to add the mailbox").await;
        }
    }

    Redirect::to("/admin/mailboxes").into_response()
}

#[tracing::instrument(name = "Delete mailbox", skip(state, session))]
pub async fn delete_mailbox(
    State(state): State<AppState>,
    session: TypedSession,
    Path(mailbox_id): Path<String>,
) -> Response {
    if let Err(e) = delete_mailbox_record(&state.db_pool, &mailbox_id).await {
        return internal_error(e, "Failed to delete the mailbox");
    }
    session.flash_info("Mailbox deleted").await;
    Redirect::to("/admin/mailboxes").into_response()
}

/// Renders every component in the design system, for reviewing it in both
/// colour schemes without touching real data.
#[tracing::instrument(name = "Styleguide", skip(session))]
pub async fn styleguide(session: TypedSession) -> Response {
    render(StyleguideTemplate {
        flash_messages: session.get_flash_messages().await,
    })
}

#[tracing::instrument(name = "Log out", skip(session))]
pub async fn log_out(session: TypedSession) -> Response {
    if let Err(e) = session.log_out().await {
        tracing::error!(error = %e, "Failed to clear the session");
    }
    Redirect::to("/login").into_response()
}

fn internal_error(error: anyhow::Error, context: &str) -> Response {
    tracing::error!(error.cause_chain = ?error, error.message = %error, "{context}");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        context.to_string(),
    )
        .into_response()
}
