use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domain::EmailAddress;

/// A real inbox that aliases forward to.
#[derive(Debug, Clone)]
pub struct Mailbox {
    pub mailbox_id: String,
    pub email: String,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

#[tracing::instrument(name = "Insert mailbox", skip(pool))]
pub async fn insert_mailbox(
    pool: &SqlitePool,
    email: &EmailAddress,
    is_default: bool,
) -> Result<Mailbox, anyhow::Error> {
    let mailbox_id = Uuid::new_v4().to_string();
    let email = email.normalised();
    let created_at = Utc::now();
    let created_at_str = created_at.to_rfc3339();
    let is_default_int = i64::from(is_default);

    let mut tx = pool.begin().await?;
    if is_default {
        sqlx::query!("UPDATE mailboxes SET is_default = 0")
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query!(
        r#"
        INSERT INTO mailboxes (mailbox_id, email, is_default, created_at)
        VALUES (?, ?, ?, ?)
        "#,
        mailbox_id,
        email,
        is_default_int,
        created_at_str,
    )
    .execute(&mut *tx)
    .await
    .context("Failed to insert the mailbox")?;
    tx.commit().await?;

    Ok(Mailbox {
        mailbox_id,
        email,
        is_default,
        created_at,
    })
}

#[tracing::instrument(name = "List mailboxes", skip(pool))]
pub async fn list_mailboxes(pool: &SqlitePool) -> Result<Vec<Mailbox>, anyhow::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT mailbox_id, email, is_default, created_at
        FROM mailboxes
        ORDER BY is_default DESC, email ASC
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Mailbox {
            mailbox_id: row.mailbox_id,
            email: row.email,
            is_default: row.is_default != 0,
            created_at: parse_timestamp(&row.created_at),
        })
        .collect())
}

#[tracing::instrument(name = "Get mailbox", skip(pool))]
pub async fn get_mailbox(
    pool: &SqlitePool,
    mailbox_id: &str,
) -> Result<Option<Mailbox>, anyhow::Error> {
    let row = sqlx::query!(
        r#"
        SELECT mailbox_id, email, is_default, created_at
        FROM mailboxes
        WHERE mailbox_id = ?
        "#,
        mailbox_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| Mailbox {
        mailbox_id: row.mailbox_id,
        email: row.email,
        is_default: row.is_default != 0,
        created_at: parse_timestamp(&row.created_at),
    }))
}

#[tracing::instrument(name = "Find mailbox by email", skip(pool))]
pub async fn find_mailbox_by_email(
    pool: &SqlitePool,
    email: &EmailAddress,
) -> Result<Option<Mailbox>, anyhow::Error> {
    let email = email.normalised();
    let row = sqlx::query!(
        r#"
        SELECT mailbox_id, email, is_default, created_at
        FROM mailboxes
        WHERE email = ?
        "#,
        email
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| Mailbox {
        mailbox_id: row.mailbox_id,
        email: row.email,
        is_default: row.is_default != 0,
        created_at: parse_timestamp(&row.created_at),
    }))
}

/// Mailboxes together with the number of aliases pointing at them.
#[tracing::instrument(name = "List mailboxes with alias counts", skip(pool))]
pub async fn list_mailboxes_with_alias_counts(
    pool: &SqlitePool,
) -> Result<Vec<(Mailbox, i64)>, anyhow::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            m.mailbox_id, m.email, m.is_default, m.created_at,
            COUNT(a.alias_id) AS alias_count
        FROM mailboxes m
        LEFT JOIN aliases a ON a.mailbox_id = m.mailbox_id
        GROUP BY m.mailbox_id
        ORDER BY m.is_default DESC, m.email ASC
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| {
            (
                Mailbox {
                    mailbox_id: row.mailbox_id,
                    email: row.email,
                    is_default: row.is_default != 0,
                    created_at: parse_timestamp(&row.created_at),
                },
                row.alias_count,
            )
        })
        .collect())
}

#[tracing::instrument(name = "Get default mailbox", skip(pool))]
pub async fn default_mailbox(pool: &SqlitePool) -> Result<Option<Mailbox>, anyhow::Error> {
    Ok(list_mailboxes(pool).await?.into_iter().next())
}

#[tracing::instrument(name = "Delete mailbox", skip(pool))]
pub async fn delete_mailbox(pool: &SqlitePool, mailbox_id: &str) -> Result<(), anyhow::Error> {
    sqlx::query!("DELETE FROM mailboxes WHERE mailbox_id = ?", mailbox_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub(crate) fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
