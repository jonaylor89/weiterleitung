use axum::Form;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use secrecy::Secret;

use crate::authentication::{AuthError, Credentials, validate_credentials};
use crate::session_state::TypedSession;
use crate::startup::AppState;
use crate::web_templates::{LoginTemplate, render};

pub async fn home() -> Redirect {
    Redirect::to("/admin/dashboard")
}

pub async fn login_form(session: TypedSession) -> Response {
    render(LoginTemplate {
        flash_messages: session.get_flash_messages().await,
    })
}

#[derive(serde::Deserialize)]
pub struct LoginFormData {
    username: String,
    password: Secret<String>,
}

#[tracing::instrument(
    name = "Log in",
    skip(state, session, form),
    fields(username = %form.username)
)]
pub async fn login(
    State(state): State<AppState>,
    session: TypedSession,
    Form(form): Form<LoginFormData>,
) -> Response {
    let credentials = Credentials {
        username: form.username,
        password: form.password,
    };

    match validate_credentials(credentials, &state.db_pool).await {
        Ok(user_id) => {
            if let Err(e) = session.insert_user_id(user_id).await {
                tracing::error!(error = %e, "Failed to store the session");
                session.flash_error("Could not start a session").await;
                return Redirect::to("/login").into_response();
            }
            Redirect::to("/admin/dashboard").into_response()
        }
        Err(AuthError::InvalidCredentials(_)) => {
            session.flash_error("Invalid credentials").await;
            Redirect::to("/login").into_response()
        }
        Err(AuthError::UnexpectedError(e)) => {
            tracing::error!(error = %e, "Failed to validate the credentials");
            session.flash_error("Something went wrong").await;
            Redirect::to("/login").into_response()
        }
    }
}
