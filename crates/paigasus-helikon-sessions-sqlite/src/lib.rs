//! SQLite-backed [`Session`] implementation for the Paigasus Helikon SDK.
//!
//! Stores conversation event logs in a single SQLite database. Multiple
//! sessions share one `SqlitePool` and are isolated by `session_id`. Safe
//! for concurrent writers — appends serialize through SQLite's database-level
//! write lock; the `(session_id, sequence)` primary key is the uniqueness
//! backstop.
//!
//! ## Recommended pool configuration
//!
//! ```no_run
//! use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteJournalMode};
//! use std::time::Duration;
//!
//! # async fn build() -> Result<sqlx::SqlitePool, sqlx::Error> {
//! let opts = SqliteConnectOptions::new()
//!     .filename("sessions.db")
//!     .create_if_missing(true)
//!     .journal_mode(SqliteJournalMode::Wal)
//!     .busy_timeout(Duration::from_secs(30));
//! SqlitePoolOptions::new().connect_with(opts).await
//! # }
//! ```
//!
//! `busy_timeout` is a write-contention parameter; 30 seconds is the value
//! exercised by this crate's `concurrent_writers` test against a real WAL
//! pool on CI. Tune downward if you know your workload is single-writer or
//! upward if you expect heavy multi-writer contention.
//!
//! ## Provider-translator caveat
//!
//! The [`project`] function in `paigasus-helikon-core` renders [`Compacted`]
//! events as `Item::System`. Both shipped provider translators reshape
//! system messages — Anthropic hoists them to the top-level `system` field,
//! OpenAI concatenates them. Compaction summaries reach the model but as
//! top-level instructions, not positional cutovers.
//!
//! [`Session`]: paigasus_helikon_core::Session
//! [`project`]: paigasus_helikon_core::project
//! [`Compacted`]: paigasus_helikon_core::SessionEvent::Compacted

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use sqlx::SqlitePool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// SQLite-backed [`Session`] implementation. One instance is one session
/// (identified by `session_id`); pools are shared across instances.
#[derive(Debug, Clone)]
pub struct SqliteSession {
    pool: SqlitePool,
    session_id: String,
}

impl SqliteSession {
    /// Run embedded migrations on `pool`. Idempotent — safe on every startup.
    ///
    /// Optional: [`SqliteSession::open`] runs migrations internally. Call
    /// this directly if you manage many sessions and want to migrate once at
    /// process start, then use [`SqliteSession::open_without_migrate`] on the
    /// hot path to skip the per-`open` round-trip to `_sqlx_migrations`.
    ///
    /// **Editing a migration file after first deploy will fail** with a
    /// checksum mismatch against the `_sqlx_migrations` table. Add a new
    /// numbered file (e.g., `0002_…`) instead of mutating an existing one.
    pub async fn migrate(pool: &SqlitePool) -> Result<(), SessionError> {
        MIGRATOR.run(pool).await.map_err(SessionError::backend)?;
        Ok(())
    }

    /// Open (or implicitly create) a session within `pool`. Runs migrations
    /// as a side effect (one round-trip to `_sqlx_migrations`). For repeated
    /// session-opens against an already-migrated pool, prefer
    /// [`SqliteSession::open_without_migrate`].
    pub async fn open(
        pool: SqlitePool,
        session_id: impl Into<String>,
    ) -> Result<Self, SessionError> {
        Self::migrate(&pool).await?;
        Ok(Self::open_without_migrate(pool, session_id))
    }

    /// Open a session without running migrations. The caller must have
    /// already invoked [`SqliteSession::migrate`] on this pool; otherwise
    /// the first [`Session::append`] fails with `SessionError::Backend`
    /// wrapping a `no such table` error.
    pub fn open_without_migrate(pool: SqlitePool, session_id: impl Into<String>) -> Self {
        Self {
            pool,
            session_id: session_id.into(),
        }
    }

    /// The `session_id` this instance reads and writes.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[async_trait]
impl Session for SqliteSession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }

        // `BEGIN IMMEDIATE` acquires SQLite's RESERVED lock up-front, so the
        // SELECT-MAX-then-INSERT sequence can't race two concurrent writers
        // into a UNIQUE-constraint collision on `(session_id, sequence)`.
        // sqlx's plain `pool.begin()` issues `BEGIN` (DEFERRED), which would
        // let two readers compute the same `next` value before either locks
        // for write. See `begin_ansi_transaction_sql` in sqlx-core.
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(SessionError::backend)?;

        // Find next sequence number for this session. COALESCE handles the
        // first-append case (MAX returns NULL on an empty result set).
        let row: (i64,) = sqlx::query_as(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = ?",
        )
        .bind(&self.session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(SessionError::backend)?;
        let start: i64 = row.0;

        for (offset, ev) in events.iter().enumerate() {
            let next = start + offset as i64;
            let (kind, ts_nanos) = event_metadata(ev);
            let payload = serde_json::to_string(ev).map_err(SessionError::backend)?;

            sqlx::query(
                "INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload) \
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&self.session_id)
            .bind(next)
            .bind(ts_nanos)
            .bind(kind)
            .bind(&payload)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;
        }

        tx.commit().await.map_err(SessionError::backend)?;
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        // `since` is exclusive ("those after"). Default to -1 so the
        // `sequence > ?` filter is a no-op when None.
        let watermark: i64 = match since {
            // `s.0` is u64; `> i64::MAX` is unreachable in practice but means "after
            // every possible sequence." Saturating to i64::MAX makes the filter
            // return empty rather than erroring — semantically correct.
            Some(s) => i64::try_from(s.0).unwrap_or(i64::MAX),
            None => -1,
        };

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT payload FROM session_events \
             WHERE session_id = ? AND sequence > ? \
             ORDER BY sequence",
        )
        .bind(&self.session_id)
        .bind(watermark)
        .fetch_all(&self.pool)
        .await
        .map_err(SessionError::backend)?;

        rows.into_iter()
            .map(|(payload,)| {
                serde_json::from_str::<SessionEvent>(&payload).map_err(SessionError::backend)
            })
            .collect()
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        let events = self.events(None).await?;
        Ok(project(&events))
    }
}

/// Extract the `(kind, ts_nanos)` denormalized columns for an event. `kind`
/// matches the serde tag of the variant; `ts_nanos` is the timestamp in
/// i64 nanoseconds since the Unix epoch (covers ±292 years from 1970).
///
/// `jiff::Timestamp::as_nanosecond` returns `i128` to fit jiff's wider
/// supported range (±9999 years). `i64::try_from` clamps via `unwrap_or`
/// to the saturating bounds — any timestamp outside ±292 years from 1970
/// is well outside the SDK's lifetime and only the audit-index column
/// suffers; the canonical timestamp lives in the JSON `payload`.
fn event_metadata(ev: &SessionEvent) -> (&'static str, i64) {
    // `SessionEvent` is `#[non_exhaustive]`, so a cross-crate match must
    // handle the open-world case. A new variant added in core without a
    // corresponding update here is a programming error — panic so it is
    // caught by tests rather than silently writing a corrupt `kind` row.
    let (kind, ts) = match ev {
        SessionEvent::UserMessage { ts, .. } => ("user_message", *ts),
        SessionEvent::AssistantMessage { ts, .. } => ("assistant_message", *ts),
        SessionEvent::ToolCalled { ts, .. } => ("tool_called", *ts),
        SessionEvent::ToolReturned { ts, .. } => ("tool_returned", *ts),
        SessionEvent::HandoffOccurred { ts, .. } => ("handoff_occurred", *ts),
        SessionEvent::Compacted { ts, .. } => ("compacted", *ts),
        _ => panic!(
            "SqliteSession: unhandled SessionEvent variant {ev:?} — \
             extend `event_metadata` when adding a new variant"
        ),
    };
    let nanos_i128 = ts.as_nanosecond();
    let saturated = if nanos_i128 < 0 { i64::MIN } else { i64::MAX };
    let ts_nanos = i64::try_from(nanos_i128).unwrap_or(saturated);
    (kind, ts_nanos)
}
