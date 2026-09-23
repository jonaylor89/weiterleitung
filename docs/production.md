# Running weiterleitung in production

What the service already does, what you have to set up around it, and what is
still missing from the code. Read the last section before you point real mail at
it.

## 1. Host and network

- A host with a **static IP and a clean reputation**, and an ISP/cloud provider
  that does not block outbound port 25. Most consumer ISPs and several clouds
  (e.g. AWS, GCP by default) block it; either request an exception or run in
  `delivery.mode: relay` through a smarthost that does not
  (Postmark/SES/Mailgun/Fastmail…).
- Inbound **TCP 25 open** to the world. The admin UI (`application.port`, 8000)
  must *not* be — put it behind a reverse proxy or a VPN/tailnet.
- **Reverse DNS (PTR)** for the IP pointing at `inbound.hostname`, and that name
  resolving back to the IP. Without matching forward/reverse DNS, a large share
  of receivers reject or spam-folder your mail.

## 2. DNS for the alias domain

| Record | Value |
|---|---|
| `MX` | `10 mail.example.com.` — the host running weiterleitung |
| `A`/`AAAA` | `mail.example.com` → the host IP |
| `SPF` (`TXT @`) | `v=spf1 ip4:<your ip> -all` (or `include:` your smarthost) |
| `DKIM` (`TXT <selector>._domainkey`) | `v=DKIM1; k=rsa; p=<public key>` |
| `DMARC` (`TXT _dmarc`) | `v=DMARC1; p=quarantine; rua=mailto:you@…` |

Generate the DKIM key pair, keep the private half at
`dkim.private_key_path` (mode `0600`, owned by the service user), publish the
public half under `dkim.selector`:

```bash
openssl genrsa -out /etc/weiterleitung/dkim.pem 2048
openssl rsa -in /etc/weiterleitung/dkim.pem -pubout -outform der | base64 -w0
```

Forwarding breaks the original sender's SPF/DKIM, which is exactly why the
service rewrites `From:` to the reverse alias and signs with **your** domain —
so keep DKIM enabled; without it forwarded mail will be treated as spoofing.

## 3. Configuration

Run with `APP_ENVIRONMENT=production`, and set at least these in the
environment (not in the YAML, which is in git):

```
APP_APPLICATION__HMAC_SECRET=<64+ random bytes>
APP_ADMIN__PASSWORD=<strong password>
APP_ALIASES__DOMAINS=alias.tld
APP_INBOUND__HOSTNAME=mail.alias.tld
APP_DELIVERY__EHLO_HOSTNAME=mail.alias.tld
APP_APPLICATION__BASE_URL=https://mail.alias.tld
```

The admin password is re-applied to the account on every boot, so rotate it by
changing the variable and restarting.

Persistent state lives in two places, both of which need backups:
`database.path` (SQLite: aliases, contacts, outbox) and `delivery.mail_dir`
(raw `.eml` spool). Back up the database with `sqlite3 … ".backup"` rather than
copying the file while the service runs.

## 4. Service unit

```ini
[Unit]
Description=weiterleitung
After=network-online.target

[Service]
User=weiterleitung
WorkingDirectory=/opt/weiterleitung
Environment=APP_ENVIRONMENT=production
EnvironmentFile=/etc/weiterleitung/env
ExecStart=/opt/weiterleitung/weiterleitung
# Port 25 without running as root:
AmbientCapabilities=CAP_NET_BIND_SERVICE
Restart=always
RestartSec=5
ProtectSystem=strict
ReadWritePaths=/var/lib/weiterleitung
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

`Restart=always` matters: any of the three workers exiting terminates the
process by design, and systemd is what brings it back.

Put the admin UI behind nginx/Caddy with a TLS certificate for
`application.base_url`; the session cookie is only marked secure over HTTPS.

## 5. Smoke test before pointing DNS at it

```bash
swaks --to alias@yourdomain.tld --from you@gmail.com --server mail.yourdomain.tld
```

Then check <https://www.mail-tester.com> and a message to
`check-auth@verifier.port25.com` to confirm SPF, DKIM, DMARC and rDNS all pass.

## 6. Known gaps — decide before going live

These are real limitations of the current code, not deployment steps:

- **No inbound TLS.** The SMTP listener answers `STARTTLS` with `454`, so
  inbound mail arrives in clear text; senders that require TLS will defer.
  Terminating TLS (or fronting with a proxy that does) is the biggest missing
  piece for a public MX.
- **Direct delivery accepts invalid certificates** (`allow_invalid_certs()` in
  `delivery/relay.rs`) and falls back to clear text when a peer offers no
  STARTTLS. That is normal opportunistic-TLS behaviour for MTAs, but it means
  delivery is not protected against an active attacker. Relay mode to a
  smarthost with a valid certificate is stricter.
- **No graceful shutdown.** A restart drops in-flight SMTP sessions; queued
  mail survives (it is on disk), a message being transmitted at that moment is
  retried, so duplicates are possible.
- **No spam filtering, rate limiting, or connection limits.** A public MX with
  an open port 25 will be probed constantly. Unknown recipients are rejected at
  `RCPT TO`, which is the main protection; consider fail2ban on the log and a
  firewall rate limit, or put rspamd in front.
- **No bounce handling.** Delivery failures are retried `delivery.max_attempts`
  times and then marked failed in the outbox — nobody is notified. Watch the
  dashboard's recent messages, or the `outbox` table, for `failed` rows.
- **No loop protection beyond alias ownership.** Do not make a mailbox address
  resolve back to one of your own aliases.
- **Single admin account, no 2FA.** Keep the UI off the public internet.
- **No PGP encryption or catch-all aliases** (SimpleLogin has both).
