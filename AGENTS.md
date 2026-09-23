# Agent guidance

`weiterleitung` is a single-binary Rust service: an Axum admin UI, an inbound
SMTP server, and an outbound delivery worker, all spawned from `main.rs` and
sharing one SQLite database.

## Layout

```
src/
├── main.rs            # spawns the three workers, reports exits
├── startup.rs         # Application::build, router, migrations, admin seeding
├── configuration.rs   # layered YAML + APP_ env overrides
├── domain/            # EmailAddress, Direction
├── mailbox/           # destination inboxes
├── alias/             # aliases + local-part generator
├── contact/           # contacts and their reverse aliases
├── smtp/              # session.rs (protocol), router.rs (routing), rewrite.rs (headers), server.rs (listener)
├── delivery/          # queue.rs (outbox), relay.rs (smarthost/MX + DKIM), worker.rs
├── routes/            # HTTP handlers
└── web_templates.rs   # Askama view models
```

Mail rules worth keeping in mind when editing `smtp/`:

- Recipients are resolved at `RCPT TO`, never accepted-then-bounced.
- Only the alias's own mailbox may send through a reverse alias.
- Forwarded mail must not leak the mailbox address; replies must not leak it to
  the contact. `rewrite.rs` strips the original authentication and routing
  headers for that reason.

## Checks

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

Integration tests in `tests/api.rs` use a temporary SQLite database and spool
per test; no external services are required.

## Database

Migrations live in `migrations/` (SQLite DDL: `TEXT` for UUIDs/timestamps,
`INTEGER` for booleans, `?` placeholders, and `NOT NULL` on primary keys so the
`sqlx` macros infer non-nullable columns). After changing a query:

```bash
DATABASE_URL="sqlite://data/weiterleitung.db?mode=rwc" cargo sqlx prepare -- --all-targets
```

Commit the regenerated `.sqlx/` data.

## Commits

Conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `test:`), one
logical change per commit.
