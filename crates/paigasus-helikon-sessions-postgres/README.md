# paigasus-helikon-sessions-postgres

PostgreSQL-backed [`Session`] backend for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK.

Stores conversation event logs in a `session_events` table with a `JSONB` payload column.
Concurrent writers are serialized per-session via a PostgreSQL advisory lock
(`pg_advisory_xact_lock`) taken inside a single transaction, ensuring no sequence
collisions without database-level contention between unrelated sessions.

## Installation

```sh
cargo add paigasus-helikon-sessions-postgres
```

## Quickstart

```rust,no_run
use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_postgres::PostgresSession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::PgPool::connect("postgres://user:pass@localhost/mydb").await?;

    // Migrate once at process start (idempotent).
    PostgresSession::migrate(&pool).await?;

    // Open a session. Use open_without_migrate on the hot path to skip the
    // round-trip to _sqlx_migrations.
    let session = PostgresSession::open_without_migrate(pool, "my-session-id");

    // Append and read events through the Session trait.
    use paigasus_helikon_core::{ContentPart, SessionEvent};
    let event = SessionEvent::user_message(vec![ContentPart::Text { text: "Hello".into() }]);
    session.append(&[event]).await?;
    let events = session.events(None).await?;
    println!("stored {} event(s)", events.len());

    Ok(())
}
```

## Table schema

```sql
CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT   NOT NULL,
    sequence   BIGINT NOT NULL,
    ts_nanos   BIGINT NOT NULL,
    kind       TEXT   NOT NULL,
    payload    JSONB  NOT NULL,
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
```

Migrations run automatically when you call `PostgresSession::open` or
`PostgresSession::migrate`. The migration file is embedded in the binary via
`sqlx::migrate!`.

## Concurrency

Each `append` call acquires `pg_advisory_xact_lock(hashtext(session_id))` inside a
single transaction before computing the next sequence number and inserting rows.
The lock is session-scoped so concurrent writers to **different** sessions do not
block each other.

## TLS

TLS uses `rustls` with the `aws-lc-rs` crypto backend (feature `tls-rustls-aws-lc-rs`),
matching the workspace-wide `CryptoProvider` already installed by the AWS SDK and
`reqwest`. A `ring`-based TLS variant would cause a dual-`CryptoProvider` panic under
`cargo test --workspace --all-features` and is intentionally omitted.

## License

Licensed under either of [Apache License, Version 2.0](../../LICENSE-APACHE) or
[MIT License](../../LICENSE-MIT), at your option.
