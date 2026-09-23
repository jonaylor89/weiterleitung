use std::ops::Deref;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::response::Redirect;
use uuid::Uuid;

use crate::session_state::TypedSession;

#[derive(Copy, Clone, Debug)]
pub struct UserId(Uuid);

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Deref for UserId {
    type Target = Uuid;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct AuthenticatedUser(pub UserId);

impl<S> FromRequestParts<S> for AuthenticatedUser
where
    S: Send + Sync,
{
    type Rejection = Redirect;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let session = TypedSession::from_request_parts(parts, state)
            .await
            .map_err(|_| Redirect::to("/login"))?;

        match session.get_user_id().await {
            Ok(Some(user_id)) => Ok(Self(UserId(user_id))),
            _ => {
                session
                    .flash_error("You must be logged in to access that page")
                    .await;
                Err(Redirect::to("/login"))
            }
        }
    }
}
