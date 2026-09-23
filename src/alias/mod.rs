mod generator;

pub use generator::{random_local_part, random_token};

use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::domain::EmailAddress;
use crate::mailbox::parse_timestamp;

#[derive(Debug, Clone)]
pub struct Alias {
    pub alias_id: String,
    pub address: String,
    pub mailbox_id: String,
    pub note: Option<String>,
    pub enabled: bool,
    pub forward_count: i64,
    pub reply_count: i64,
    pub blocked_count: i64,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// An alias joined with the address of the mailbox it forwards to.
#[derive(Debug, Clone)]
pub struct AliasWithMailbox {
    pub alias: Alias,
    pub mailbox_email: String,
}

#[tracing::instrument(name = "Insert alias", skip(pool))]
pub async fn insert_alias(
    pool: &SqlitePool,
    address: &EmailAddress,
    mailbox_id: &str,
    note: Option<String>,
) -> Result<Alias, anyhow::Error> {
    let alias_id = Uuid::new_v4().to_string();
    let address = address.normalised();
    let created_at = Utc::now();
    let created_at_str = created_at.to_rfc3339();

    sqlx::query!(
        r#"
        INSERT INTO aliases (alias_id, address, mailbox_id, note, enabled, created_at)
        VALUES (?, ?, ?, ?, 1, ?)
        "#,
        alias_id,
        address,
        mailbox_id,
        note,
        created_at_str,
    )
    .execute(pool)
    .await
    .context("Failed to insert the alias")?;

    Ok(Alias {
        alias_id,
        address,
        mailbox_id: mailbox_id.to_string(),
        note,
        enabled: true,
        forward_count: 0,
        reply_count: 0,
        blocked_count: 0,
        created_at,
        last_used_at: None,
    })
}

#[tracing::instrument(name = "Find alias by address", skip(pool))]
pub async fn find_alias_by_address(
    pool: &SqlitePool,
    address: &EmailAddress,
) -> Result<Option<AliasWithMailbox>, anyhow::Error> {
    let address = address.normalised();
    let row = sqlx::query!(
        r#"
        SELECT
            a.alias_id, a.address, a.mailbox_id, a.note, a.enabled,
            a.forward_count, a.reply_count, a.blocked_count,
            a.created_at, a.last_used_at,
            m.email AS mailbox_email
        FROM aliases a
        JOIN mailboxes m ON m.mailbox_id = a.mailbox_id
        WHERE a.address = ?
        "#,
        address
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| AliasWithMailbox {
        alias: Alias {
            alias_id: row.alias_id,
            address: row.address,
            mailbox_id: row.mailbox_id,
            note: row.note,
            enabled: row.enabled != 0,
            forward_count: row.forward_count,
            reply_count: row.reply_count,
            blocked_count: row.blocked_count,
            created_at: parse_timestamp(&row.created_at),
            last_used_at: row.last_used_at.as_deref().map(parse_timestamp),
        },
        mailbox_email: row.mailbox_email,
    }))
}

#[tracing::instrument(name = "List aliases", skip(pool))]
pub async fn list_aliases(pool: &SqlitePool) -> Result<Vec<AliasWithMailbox>, anyhow::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT
            a.alias_id, a.address, a.mailbox_id, a.note, a.enabled,
            a.forward_count, a.reply_count, a.blocked_count,
            a.created_at, a.last_used_at,
            m.email AS mailbox_email
        FROM aliases a
        JOIN mailboxes m ON m.mailbox_id = a.mailbox_id
        ORDER BY a.created_at DESC
        "#
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| AliasWithMailbox {
            alias: Alias {
                alias_id: row.alias_id,
                address: row.address,
                mailbox_id: row.mailbox_id,
                note: row.note,
                enabled: row.enabled != 0,
                forward_count: row.forward_count,
                reply_count: row.reply_count,
                blocked_count: row.blocked_count,
                created_at: parse_timestamp(&row.created_at),
                last_used_at: row.last_used_at.as_deref().map(parse_timestamp),
            },
            mailbox_email: row.mailbox_email,
        })
        .collect())
}

#[tracing::instrument(name = "Set alias enabled", skip(pool))]
pub async fn set_alias_enabled(
    pool: &SqlitePool,
    alias_id: &str,
    enabled: bool,
) -> Result<(), anyhow::Error> {
    let enabled = i64::from(enabled);
    sqlx::query!(
        "UPDATE aliases SET enabled = ? WHERE alias_id = ?",
        enabled,
        alias_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[tracing::instrument(name = "Delete alias", skip(pool))]
pub async fn delete_alias(pool: &SqlitePool, alias_id: &str) -> Result<(), anyhow::Error> {
    sqlx::query!("DELETE FROM aliases WHERE alias_id = ?", alias_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Counter bumped after a message is accepted for an alias.
#[derive(Debug, Clone, Copy)]
pub enum AliasCounter {
    Forwarded,
    Replied,
    Blocked,
}

#[tracing::instrument(name = "Record alias activity", skip(pool))]
pub async fn record_alias_activity(
    pool: &SqlitePool,
    alias_id: &str,
    counter: AliasCounter,
) -> Result<(), anyhow::Error> {
    let now = Utc::now().to_rfc3339();
    match counter {
        AliasCounter::Forwarded => {
            sqlx::query!(
                "UPDATE aliases SET forward_count = forward_count + 1, last_used_at = ? WHERE alias_id = ?",
                now,
                alias_id
            )
            .execute(pool)
            .await?;
        }
        AliasCounter::Replied => {
            sqlx::query!(
                "UPDATE aliases SET reply_count = reply_count + 1, last_used_at = ? WHERE alias_id = ?",
                now,
                alias_id
            )
            .execute(pool)
            .await?;
        }
        AliasCounter::Blocked => {
            sqlx::query!(
                "UPDATE aliases SET blocked_count = blocked_count + 1, last_used_at = ? WHERE alias_id = ?",
                now,
                alias_id
            )
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
