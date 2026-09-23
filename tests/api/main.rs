//! Integration tests. Each one gets its own temporary SQLite database and mail
//! spool, so they can run in parallel without stepping on each other. The
//! `mailpit` module additionally runs a real SMTP server per test.

mod admin;
mod delivery;
mod harness;
mod inbound;
mod mailpit;
mod routing;
