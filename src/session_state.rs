use std::fmt::Display;

use axum::extract::FromRequestParts;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FlashLevel {
    Info,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlashMessage {
    pub level: FlashLevel,
    pub content: String,
}

impl FlashMessage {
    pub fn is_error(&self) -> bool {
        matches!(self.level, FlashLevel::Error)
    }
}

impl Display for FlashMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.content)
    }
}

#[derive(Clone)]
pub struct TypedSession(Session);

impl TypedSession {
    const USER_ID_KEY: &'static str = "user_id";
    const FLASH_KEY: &'static str = "flash_messages";

    pub async fn insert_user_id(&self, user_id: Uuid) -> Result<(), anyhow::Error> {
        self.0.insert(Self::USER_ID_KEY, user_id).await?;
        Ok(())
    }

    pub async fn get_user_id(&self) -> Result<Option<Uuid>, anyhow::Error> {
        Ok(self.0.get(Self::USER_ID_KEY).await?)
    }

    pub async fn log_out(&self) -> Result<(), anyhow::Error> {
        let _ = self.0.remove::<Uuid>(Self::USER_ID_KEY).await?;
        Ok(())
    }

    pub async fn flash_error(&self, message: impl Into<String>) {
        self.push_flash(FlashLevel::Error, message.into()).await;
    }

    pub async fn flash_info(&self, message: impl Into<String>) {
        self.push_flash(FlashLevel::Info, message.into()).await;
    }

    pub async fn get_flash_messages(&self) -> Vec<FlashMessage> {
        self.0
            .remove(Self::FLASH_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    async fn push_flash(&self, level: FlashLevel, content: String) {
        let mut messages: Vec<FlashMessage> = self
            .0
            .get(Self::FLASH_KEY)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        messages.push(FlashMessage { level, content });
        let _ = self.0.insert(Self::FLASH_KEY, messages).await;
    }
}

impl<S> FromRequestParts<S> for TypedSession
where
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        Ok(Self(session))
    }
}
