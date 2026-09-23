use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::alias::random_token;
use crate::domain::EmailAddress;
use crate::mailbox::parse_timestamp;

/// A remote party that corresponds with one alias, plus the reverse alias the
/// mailbox replies to in order to reach them.
#[derive(Debug, Clone)]
pub struct Contact {
    pub contact_id: String,
    pub alias_id: String,
    pub email: String,
    pub name: Option<String>,
    pub reverse_alias: String,
    pub created_at: DateTime<Utc>,
}

/// A contact joined with the alias it belongs to.
#[derive(Debug, Clone)]
pub struct ContactWithAlias {
    pub contact: Contact,
    pub alias_address: String,
    pub alias_enabled: bool,
    pub mailbox_email: String,
}

/// Looks up the contact for `(alias, sender)`, creating it — together with a
/// fresh reverse alias — the first time that pair is seen.
#[tracing::instrument(name = "Get or create contact", skip(pool))]
pub async fn get_or_create_contact(
    pool: &SqlitePool,
    alias_id: &str,
    email: &EmailAddress,
    name: Option<&str>,
    reverse_alias_prefix: &str,
    reverse_alias_domain: &str,
) -> Result<Contact, anyhow::Error> {
    let email = email.normalised();

    if let Some(existing) = find_contact(pool, alias_id, &email).await? {
        return Ok(existing);
    }

    let contact_id = Uuid::new_v4().to_string();
    let reverse_alias = format!(
        "{reverse_alias_prefix}.{}@{reverse_alias_domain}",
        random_token(12)
    );
    let created_at = Utc::now();
    let created_at_str = created_at.to_rfc3339();

    sqlx::query!(
        r#"
        INSERT INTO contacts (contact_id, alias_id, email, name, reverse_alias, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT (alias_id, email) DO NOTHING
        "#,
        contact_id,
        alias_id,
        email,
        name,
        reverse_alias,
        created_at_str,
    )
    .execute(pool)
    .await
    .context("Failed to insert the contact")?;

    // Another inbound connection may have won the race; re-read either way.
    find_contact(pool, alias_id, &email)
        .await?
        .context("Contact disappeared right after being inserted")
}

#[tracing::instrument(name = "Find contact", skip(pool))]
async fn find_contact(
    pool: &SqlitePool,
    alias_id: &str,
    email: &str,
) -> Result<Option<Contact>, anyhow::Error> {
    let row = sqlx::query!(
        r#"
        SELECT contact_id, alias_id, email, name, reverse_alias, created_at
        FROM contacts
        WHERE alias_id = ? AND email = ?
        "#,
        alias_id,
        email
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| Contact {
        contact_id: row.contact_id,
        alias_id: row.alias_id,
        email: row.email,
        name: row.name,
        reverse_alias: row.reverse_alias,
        created_at: parse_timestamp(&row.created_at),
    }))
}

#[tracing::instrument(name = "Find contact by reverse alias", skip(pool))]
pub async fn find_contact_by_reverse_alias(
    pool: &SqlitePool,
    reverse_alias: &EmailAddress,
) -> Result<Option<ContactWithAlias>, anyhow::Error> {
    let reverse_alias = reverse_alias.normalised();
    let row = sqlx::query!(
        r#"
        SELECT
            c.contact_id, c.alias_id, c.email, c.name, c.reverse_alias, c.created_at,
            a.address AS alias_address, a.enabled AS alias_enabled,
            m.email AS mailbox_email
        FROM contacts c
        JOIN aliases a ON a.alias_id = c.alias_id
        JOIN mailboxes m ON m.mailbox_id = a.mailbox_id
        WHERE c.reverse_alias = ?
        "#,
        reverse_alias
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| ContactWithAlias {
        contact: Contact {
            contact_id: row.contact_id,
            alias_id: row.alias_id,
            email: row.email,
            name: row.name,
            reverse_alias: row.reverse_alias,
            created_at: parse_timestamp(&row.created_at),
        },
        alias_address: row.alias_address,
        alias_enabled: row.alias_enabled != 0,
        mailbox_email: row.mailbox_email,
    }))
}

#[tracing::instrument(name = "List contacts for alias", skip(pool))]
pub async fn list_contacts_for_alias(
    pool: &SqlitePool,
    alias_id: &str,
) -> Result<Vec<Contact>, anyhow::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT contact_id, alias_id, email, name, reverse_alias, created_at
        FROM contacts
        WHERE alias_id = ?
        ORDER BY created_at DESC
        "#,
        alias_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| Contact {
            contact_id: row.contact_id,
            alias_id: row.alias_id,
            email: row.email,
            name: row.name,
            reverse_alias: row.reverse_alias,
            created_at: parse_timestamp(&row.created_at),
        })
        .collect())
}
