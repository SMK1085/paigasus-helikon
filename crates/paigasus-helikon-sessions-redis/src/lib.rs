//! Redis Streams-backed [`Session`] implementation for the Paigasus Helikon SDK.
//!
//! Events are stored in a Redis Stream at key `helikon:session:{id}:events`.
//! Each stream entry carries four fields: `seq` (monotonic integer), `kind`
//! (variant tag), `payload` (JSON), and `ts` (nanoseconds since Unix epoch as
//! a decimal string).
//!
//! Appends use an atomic Lua script (`APPEND_SCRIPT`) to compute contiguous
//! sequence numbers and `XADD` all events in one round-trip. Concurrent writers
//! on the same session key race through Redis's single-threaded command loop —
//! no two batches can interleave, so sequences are gapless and duplicate-free.
//!
//! ## Quick start
//!
//! ```no_run
//! # use paigasus_helikon_core::Session;
//! # use paigasus_helikon_sessions_redis::RedisSession;
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let session = RedisSession::connect("redis://127.0.0.1/", "session-abc").await?;
//! session.append(&[]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## BYO `ConnectionManager` (TLS, pooled)
//!
//! ```no_run
//! # use paigasus_helikon_sessions_redis::RedisSession;
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = redis::Client::open("rediss://user:pass@localhost:6380/")?;
//! let conn   = redis::aio::ConnectionManager::new(client).await?;
//! let session = RedisSession::new(conn, "session-xyz");
//! # Ok(())
//! # }
//! ```
//!
//! ## Operational notes
//!
//! - **No automatic trimming.** The stream grows unboundedly. Configure
//!   `maxmemory-policy noeviction` (or a TTL via a sidecar) so Redis does not
//!   silently evict stream entries.
//! - **Persistence.** Use `appendonly yes` (AOF) or snapshotting to survive
//!   restarts. An eviction or flush loses all events for that session.
//!
//! [`Session`]: paigasus_helikon_core::Session

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

/// Lua script for atomic, contiguous-sequence append.
///
/// Arguments come in groups of three: `kind`, `payload`, `ts`.
/// Returns the starting sequence number (`n = XLEN` before the append).
const APPEND_SCRIPT: &str = r#"
local n = redis.call('XLEN', KEYS[1])
for i = 0, (#ARGV / 3) - 1 do
  redis.call('XADD', KEYS[1], '*',
    'seq', n + i, 'kind', ARGV[i*3 + 1], 'payload', ARGV[i*3 + 2], 'ts', ARGV[i*3 + 3])
end
return n
"#;

/// Error returned when a required field is absent in a Redis Stream entry.
#[derive(Debug, thiserror::Error)]
#[error("missing field '{field}' in Redis Stream entry")]
struct MissingField {
    field: &'static str,
}

/// Redis Streams-backed [`Session`]. One instance represents one session
/// identified by `session_id`.
///
/// Instances are cheap to clone — [`ConnectionManager`] is internally
/// reference-counted and multiplexes commands over a single async connection.
///
/// [`Session`]: paigasus_helikon_core::Session
#[derive(Clone)]
pub struct RedisSession {
    conn: ConnectionManager,
    session_id: String,
    /// Redis stream key: `helikon:session:{session_id}:events`
    key: String,
}

impl std::fmt::Debug for RedisSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSession")
            .field("session_id", &self.session_id)
            .field("key", &self.key)
            .field("conn", &"ConnectionManager { .. }")
            .finish()
    }
}

impl RedisSession {
    /// Create a `RedisSession` from an existing [`ConnectionManager`].
    ///
    /// The stream key is derived as `helikon:session:{session_id}:events`.
    /// Use this constructor when you manage your own connection (TLS, pooling,
    /// custom retry policy).
    pub fn new(conn: ConnectionManager, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        let key = format!("helikon:session:{}:events", session_id);
        Self {
            conn,
            session_id,
            key,
        }
    }

    /// Connect to Redis at `url` and return a session for `session_id`.
    ///
    /// `url` follows the `redis://[user:pass@]host[:port][/db]` or
    /// `rediss://…` (TLS) scheme understood by the [`redis`] crate.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Backend`] if the URL is invalid or the initial
    /// connection to Redis fails.
    pub async fn connect(url: &str, session_id: impl Into<String>) -> Result<Self, SessionError> {
        let client = redis::Client::open(url).map_err(SessionError::backend)?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(SessionError::backend)?;
        Ok(Self::new(conn, session_id))
    }

    /// The `session_id` this instance reads and writes.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[async_trait]
impl Session for RedisSession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        // Clone is O(1): ConnectionManager is reference-counted.
        let mut conn = self.conn.clone();
        let script = redis::Script::new(APPEND_SCRIPT);
        let mut invocation = script.prepare_invoke();
        invocation.key(&self.key);
        for ev in events {
            let kind = ev.kind();
            let payload = serde_json::to_string(ev).map_err(SessionError::backend)?;
            let ts = ev.ts_nanos_saturating().to_string();
            invocation.arg(kind).arg(payload).arg(ts);
        }
        // Return type i64 matches the Lua `return n` (XLEN result).
        let _: i64 = invocation
            .invoke_async(&mut conn)
            .await
            .map_err(SessionError::backend)?;
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        // `since` is exclusive: return events with seq > watermark.
        // Default to -1 so that None returns all events (0 > -1).
        let watermark: i64 = match since {
            // seq IDs > i64::MAX are unreachable in practice; saturate so
            // the filter returns empty rather than wrapping.
            Some(s) => i64::try_from(s.0).unwrap_or(i64::MAX),
            None => -1,
        };
        let mut conn = self.conn.clone();
        let reply: redis::streams::StreamRangeReply = conn
            .xrange(&self.key, "-", "+")
            .await
            .map_err(SessionError::backend)?;

        let mut result = Vec::new();
        for entry in reply.ids {
            let seq: i64 = entry
                .get("seq")
                .ok_or_else(|| SessionError::backend(MissingField { field: "seq" }))?;
            if seq <= watermark {
                continue;
            }
            let payload: String = entry
                .get("payload")
                .ok_or_else(|| SessionError::backend(MissingField { field: "payload" }))?;
            let event: SessionEvent =
                serde_json::from_str(&payload).map_err(SessionError::backend)?;
            result.push(event);
        }
        Ok(result)
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(project(&self.events(None).await?))
    }
}
