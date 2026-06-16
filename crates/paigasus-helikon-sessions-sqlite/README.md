# paigasus-helikon-sessions-sqlite

A SQLite-backed `Session` implementation for the [Paigasus Helikon](https://github.com/SMK1085/paigasus-helikon) AI SDK — a Rust SDK for building AI agents. `SqliteSession` persists conversation event logs in a single SQLite database; multiple sessions share one `SqlitePool`, isolated by `session_id`, and concurrent writers are safe.

## Install

```bash
cargo add paigasus-helikon-sessions-sqlite
```

Most users enable the `sessions-sqlite` feature on the [`paigasus-helikon`](https://crates.io/crates/paigasus-helikon) facade instead, which re-exports this crate as `paigasus_helikon::sessions_sqlite`.

## Example

```rust
use paigasus_helikon_sessions_sqlite::SqliteSession;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::time::Duration;

let opts = SqliteConnectOptions::new()
    .filename("sessions.db")
    .create_if_missing(true)
    .journal_mode(SqliteJournalMode::Wal)
    .busy_timeout(Duration::from_secs(30));
let pool = SqlitePoolOptions::new().connect_with(opts).await?;

// Opens (or implicitly creates) the session and runs migrations.
let session = SqliteSession::open(pool, "user-123").await?;
```

Wrap the session in `Arc` and pass it into `RunContext::new(...)` (whose session parameter is `Arc<dyn Session>`) in place of `Arc::new(MemorySession::new())` to persist transcripts across runs. WAL journal mode plus a `busy_timeout` are recommended for concurrent writers — see the crate docs for the rationale and the `open_without_migrate` fast path.

## Links

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-sessions-sqlite)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [sessions](https://smk1085.github.io/paigasus-helikon/concepts/sessions.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
