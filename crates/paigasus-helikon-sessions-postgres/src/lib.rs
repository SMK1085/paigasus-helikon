//! PostgreSQL-backed [`Session`] implementation. Mirrors the sqlite backend's
//! event-log shape; safe for concurrent writers via a per-session advisory lock.
//!
//! Events are stored in a `session_events` table with a `JSONB` payload column.
//! Appends take a per-session `pg_advisory_xact_lock` before computing the next
//! sequence number, ensuring no two concurrent writers can produce the same
//! `(session_id, sequence)` pair.
//!
//! ## Pool setup
//!
//! ```no_run
//! # async fn build() -> Result<sqlx::PgPool, sqlx::Error> {
//! let pool = sqlx::PgPool::connect("postgres://user:pass@localhost/mydb").await?;
//! # Ok(pool)
//! # }
//! ```
//!
//! ## Recommended usage
//!
//! ```no_run
//! # use paigasus_helikon_core::Session;
//! # use paigasus_helikon_sessions_postgres::PostgresSession;
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let pool = sqlx::PgPool::connect("postgres://user:pass@localhost/mydb").await?;
//! // Migrate once at process start, then use open_without_migrate on the hot path.
//! PostgresSession::migrate(&pool).await?;
//! let session = PostgresSession::open_without_migrate(pool, "session-abc");
//! session.append(&[]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! [`Session`]: paigasus_helikon_core::Session

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use sqlx::PgPool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// PostgreSQL-backed [`Session`]. One instance is one session (`session_id`);
/// pools are shared across instances.
///
/// [`Session`]: paigasus_helikon_core::Session
#[derive(Debug, Clone)]
pub struct PostgresSession {
    pool: PgPool,
    session_id: String,
}

impl PostgresSession {
    /// Run embedded migrations on `pool`. Idempotent — safe to call on every startup.
    ///
    /// Optional: [`PostgresSession::open`] runs migrations internally. Call
    /// this directly if you manage many sessions and want to migrate once at
    /// process start, then use [`PostgresSession::open_without_migrate`] on the
    /// hot path to skip the per-`open` round-trip to `_sqlx_migrations`.
    ///
    /// **Editing a migration file after first deploy will fail** with a
    /// checksum mismatch against the `_sqlx_migrations` table. Add a new
    /// numbered file (e.g., `0002_…`) instead of mutating an existing one.
    pub async fn migrate(pool: &PgPool) -> Result<(), SessionError> {
        MIGRATOR.run(pool).await.map_err(SessionError::backend)?;
        Ok(())
    }

    /// Open (and migrate) a session within `pool`. Runs migrations as a side
    /// effect (one round-trip to `_sqlx_migrations`). For repeated session-opens
    /// against an already-migrated pool, prefer
    /// [`PostgresSession::open_without_migrate`].
    pub async fn open(pool: PgPool, session_id: impl Into<String>) -> Result<Self, SessionError> {
        Self::migrate(&pool).await?;
        Ok(Self::open_without_migrate(pool, session_id))
    }

    /// Open a session without running migrations. The caller must have already
    /// invoked [`PostgresSession::migrate`] on this pool; otherwise the first
    /// [`Session::append`] fails with `SessionError::Backend` wrapping a
    /// `relation "session_events" does not exist` error.
    pub fn open_without_migrate(pool: PgPool, session_id: impl Into<String>) -> Self {
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
impl Session for PostgresSession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        // Single transaction on ONE pooled connection: the advisory lock must
        // cover the INSERTs. Per-session lock auto-releases at COMMIT. The key is
        // `hashtextextended($1, 0)` — a 64-bit hash, so the full advisory-lock key
        // space is used and distinct sessions effectively never collide (unlike the
        // 32-bit `hashtext`, which could occasionally serialize unrelated sessions).
        let mut tx = self.pool.begin().await.map_err(SessionError::backend)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(&self.session_id)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = $1",
        )
        .bind(&self.session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(SessionError::backend)?;

        for (offset, ev) in events.iter().enumerate() {
            let seq = next + offset as i64;
            let payload = serde_json::to_value(ev).map_err(SessionError::backend)?;
            sqlx::query(
                "INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(&self.session_id)
            .bind(seq)
            .bind(ev.ts_nanos_saturating())
            .bind(ev.kind())
            .bind(payload)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;
        }
        tx.commit().await.map_err(SessionError::backend)?;
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        // `since` is exclusive ("those after"). Default to -1 so the
        // `sequence > $2` filter is a no-op when None.
        let watermark: i64 = match since {
            // `s.0` is u64; `> i64::MAX` is unreachable in practice but means "after
            // every possible sequence." Saturating to i64::MAX makes the filter
            // return empty rather than erroring — semantically correct.
            Some(s) => i64::try_from(s.0).unwrap_or(i64::MAX),
            None => -1,
        };
        let rows: Vec<(sqlx::types::Json<SessionEvent>,)> = sqlx::query_as(
            "SELECT payload FROM session_events \
             WHERE session_id = $1 AND sequence > $2 ORDER BY sequence",
        )
        .bind(&self.session_id)
        .bind(watermark)
        .fetch_all(&self.pool)
        .await
        .map_err(SessionError::backend)?;
        Ok(rows.into_iter().map(|(j,)| j.0).collect())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(project(&self.events(None).await?))
    }
}
