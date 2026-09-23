# weiterleitung

A single-binary, self-hosted email alias service — a personal SimpleLogin.

Give every site its own alias (`shop-name.4f8a@yourdomain.tld`). Mail sent to an
alias is forwarded to your real mailbox with the sender rewritten to a *reverse
alias*; hitting reply in your mail client sends the answer back out **as the
alias**, so the other side never learns your real address.

```
contact ──▶ alias@yourdomain ──▶ [weiterleitung] ──▶ you@personal.tld
                                        │              (From: reverse alias)
contact ◀── alias@yourdomain ◀──────────┘◀── reply to reverse alias
```

## What it does

- **Aliases** — random (`cedar-ripple.4f8a@…`) or custom local parts, each
  pointing at one mailbox, individually enable/disable-able, with forward /
  reply / blocked counters.
- **Mailboxes** — one or more real destination inboxes.
- **Reverse aliases** — a stable `ct.<token>@yourdomain` per (alias, contact)
  pair; only the owning mailbox may send through one.
- **Inbound SMTP** — its own RFC 5321 server; unknown, disabled, or foreign
  recipients are rejected at `RCPT TO` instead of being accepted and bounced.
- **Outbound queue** — raw messages spooled to disk, envelope and retry state in
  SQLite, exponential backoff, optional DKIM signing, relay through a smarthost
  or direct MX delivery.
- **Web admin** — Askama-rendered pages for aliases, mailboxes, contacts and
  recent mail, behind a single operator login.

## Running it

```bash
cargo run                      # APP_ENVIRONMENT=local by default
```

That starts three workers in one process: the HTTP admin on `:8000`, the inbound
SMTP listener on `:2525`, and the delivery worker. SQLite and the mail spool are
created under `./data/`. Locally, delivery relays into a sink on `127.0.0.1:1025`
(e.g. [mailpit](https://mailpit.axllent.org/)) so no mail escapes the machine.

Log in with `admin.username` / `admin.password`; the account is (re)created from
the configuration on every boot.

## Configuration

Layered YAML under `configuration/`: `base.yaml`, then `local.yaml` or
`production.yaml` (chosen by `APP_ENVIRONMENT`), then environment variables
prefixed with `APP_` using `__` for nesting:

```bash
APP_ADMIN__PASSWORD=… APP_ALIASES__DOMAINS=alias.tld,alias2.tld cargo run
```

| Key | Meaning |
|---|---|
| `aliases.domains` | Domains this instance accepts mail for; the first is used for new aliases |
| `aliases.reverse_alias_prefix` | Local-part prefix of reverse aliases (default `ct`) |
| `inbound.host` / `inbound.port` | Where the SMTP listener binds (`0.0.0.0:25` in production) |
| `delivery.mode` | `relay` (smarthost) or `direct` (MX lookup) |
| `delivery.mail_dir` | Directory holding the raw `.eml` spool |
| `dkim.*` | Selector, domain and PEM private key used to sign outgoing mail |

## Deploying

1. Point an MX record for your alias domain at the host, and open port 25.
2. Publish SPF, DKIM (`dkim.selector`), DMARC and reverse DNS — without them,
   forwarded mail lands in spam.
3. Run with `APP_ENVIRONMENT=production`, `APP_APPLICATION__HMAC_SECRET` and
   `APP_ADMIN__PASSWORD` set, and put the admin UI behind TLS.

[`docs/production.md`](docs/production.md) has the full runbook — DNS records,
DKIM key generation, a systemd unit, backups, and the known gaps (no inbound
STARTTLS, no graceful shutdown, no spam filtering) worth knowing before you
point real mail at it.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

The delivery tests forward mail into a real [Mailpit](https://mailpit.axllent.org/)
server, so `mailpit` has to be on `PATH`; [`docs/testing.md`](docs/testing.md)
covers the setup and what the suite proves.

Database queries are checked at compile time against the `.sqlx/` offline data;
regenerate it after changing a query:

```bash
DATABASE_URL="sqlite://data/weiterleitung.db?mode=rwc" cargo sqlx prepare -- --all-targets
```
