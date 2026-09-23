use anyhow::Context;
use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::SaltString,
};
use chrono::Utc;
use secrecy::{ExposeSecret, Secret};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::telemetry::spawn_blocking_with_tracing;

#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("Invalid credentials")]
    InvalidCredentials(#[source] anyhow::Error),

    #[error(transparent)]
    UnexpectedError(#[from] anyhow::Error),
}

pub struct Credentials {
    pub username: String,
    pub password: Secret<String>,
}

#[tracing::instrument(name = "Validate credentials", skip(credentials, pool))]
pub async fn validate_credentials(
    credentials: Credentials,
    pool: &SqlitePool,
) -> Result<Uuid, AuthError> {
    let mut user_id = None;
    // A dummy hash keeps the timing of unknown usernames indistinguishable
    // from wrong passwords.
    let mut expected_password_hash = Secret::new(
        "$argon2id$v=19$m=15000,t=2,p=1$\
        gZiV/M1gPc22ElAH/Jh1Hw$\
        CWOrkoo7oJBQ/iyh7uJ0LO2aLEfrHwTWllSAxT0zRno"
            .to_string(),
    );

    if let Some((stored_user_id, stored_password_hash)) =
        get_stored_credentials(&credentials.username, pool)
            .await
            .map_err(AuthError::UnexpectedError)?
    {
        user_id = Some(stored_user_id);
        expected_password_hash = stored_password_hash;
    }

    spawn_blocking_with_tracing(move || {
        verify_password_hash(expected_password_hash, credentials.password)
    })
    .await
    .context("Failed to spawn a blocking task")
    .map_err(AuthError::UnexpectedError)?
    .context("Invalid password")
    .map_err(AuthError::InvalidCredentials)?;

    user_id.ok_or_else(|| AuthError::InvalidCredentials(anyhow::anyhow!("Unknown username")))
}

#[tracing::instrument(name = "Get stored credentials", skip(username, pool))]
async fn get_stored_credentials(
    username: &str,
    pool: &SqlitePool,
) -> Result<Option<(Uuid, Secret<String>)>, anyhow::Error> {
    let row = sqlx::query!(
        r#"
        SELECT user_id, password_hash
        FROM users
        WHERE username = ?
        "#,
        username,
    )
    .fetch_optional(pool)
    .await
    .context("Failed to query the stored credentials")?
    .map(|row| {
        let user_id = Uuid::parse_str(&row.user_id).expect("Invalid UUID in users.user_id");
        (user_id, Secret::new(row.password_hash))
    });
    Ok(row)
}

#[tracing::instrument(
    name = "Verify password hash",
    skip(expected_password_hash, password_candidate)
)]
fn verify_password_hash(
    expected_password_hash: Secret<String>,
    password_candidate: Secret<String>,
) -> Result<(), anyhow::Error> {
    let expected_password_hash = PasswordHash::new(expected_password_hash.expose_secret())
        .context("Failed to parse the stored password hash")?;

    Argon2::default()
        .verify_password(
            password_candidate.expose_secret().as_bytes(),
            &expected_password_hash,
        )
        .context("Invalid password")
}

pub fn compute_password_hash(password: Secret<String>) -> Result<Secret<String>, anyhow::Error> {
    let salt = SaltString::generate(&mut rand::thread_rng());
    let password_hash = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(15000, 2, 1, None).context("Failed to build the Argon2 parameters")?,
    )
    .hash_password(password.expose_secret().as_bytes(), &salt)?
    .to_string();

    Ok(Secret::new(password_hash))
}

/// Creates the single operator account, or resets its password, so the
/// configuration stays the source of truth across restarts.
#[tracing::instrument(name = "Seed the admin user", skip(pool, password))]
pub async fn seed_admin_user(
    pool: &SqlitePool,
    username: &str,
    password: Secret<String>,
) -> Result<(), anyhow::Error> {
    let password_hash = spawn_blocking_with_tracing(move || compute_password_hash(password))
        .await
        .context("Failed to spawn a blocking task")??;
    let password_hash = password_hash.expose_secret();
    let user_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();

    sqlx::query!(
        r#"
        INSERT INTO users (user_id, username, password_hash, created_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT (username) DO UPDATE SET password_hash = excluded.password_hash
        "#,
        user_id,
        username,
        password_hash,
        created_at,
    )
    .execute(pool)
    .await
    .context("Failed to seed the admin user")?;

    Ok(())
}
