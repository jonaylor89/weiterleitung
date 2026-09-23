# Testing

```bash
cargo test
```

Everything runs against real components: a temporary SQLite database and mail
spool per test, the real inbound SMTP listener on a random port, and — for the
delivery tests — a real [Mailpit](https://mailpit.axllent.org/) SMTP server that
the outbound relay talks to over TCP.

## Mailpit

The `tests/api/mailpit.rs` tests need the `mailpit` binary on `PATH`, or
`MAILPIT_BIN` pointing at it:

```bash
curl -fsSL -o mailpit.tar.gz \
  https://github.com/axllent/mailpit/releases/download/v1.31.2/mailpit-linux-amd64.tar.gz
tar -xzf mailpit.tar.gz mailpit
sudo install -m 0755 mailpit /usr/local/bin/mailpit
```

CI installs the same pinned version. The tests fail loudly rather than skipping
if it is missing.

Each test starts its own Mailpit process with a temporary database and random
SMTP/HTTP ports, waits for `/readyz`, and kills the process on drop. Assertions
read back what Mailpit actually received over the wire through its HTTP API:
the parsed message (`/api/v1/messages`) and the raw bytes including
`Return-Path` and the `Received` line, which is where the envelope sender and
recipient come from.

Failure paths use Mailpit's chaos endpoint (`--enable-chaos`) to make it reject
recipients with a `451` or `550`, so retry and give-up behaviour is exercised
against real SMTP responses rather than a mock.

## Layout

| File | Covers |
| --- | --- |
| `tests/api/harness.rs` | `TestApp`, raw SMTP client, Mailpit process control |
| `tests/api/admin.rs` | health check, login, session redirects |
| `tests/api/inbound.rs` | SMTP protocol: `SIZE`, `STARTTLS`, rejections, oversized mail |
| `tests/api/routing.rs` | alias resolution, reverse-alias creation, reply authorisation |
| `tests/api/delivery.rs` | relay against an in-process sink, crash recovery |
| `tests/api/mailpit.rs` | end-to-end forward, reply, retry, give-up, auth, DKIM |

`tests/api/dkim_test_key.pem` is a throwaway key generated for the DKIM test.
It signs nothing outside the test suite.

## Timing

No fixed sleeps: readiness and delivery are polled with bounded deadlines, and
the delivery worker is normally driven one pass at a time through
`drain_outbox`, so the tests are deterministic. Retry tests move
`next_attempt_at` into the past with SQL instead of waiting out the backoff.
