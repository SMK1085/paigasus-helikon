# paigasus-helikon-sessions-redis

Redis Streams-backed [`Session`][session] backend for the [Paigasus Helikon AI SDK][helikon].

Events are stored in a Redis Stream at key `helikon:session:{id}:events`.
Each entry carries `seq` (monotonic integer), `kind` (variant tag),
`payload` (JSON), and `ts` (nanoseconds since Unix epoch).

Concurrent appends are serialized through Redis's single-threaded command
loop via an atomic Lua script that assigns contiguous sequence numbers —
no gaps, no duplicates.

## Install

```sh
cargo add paigasus-helikon-sessions-redis
```

## Quick start

```rust,no_run
use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_redis::RedisSession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = RedisSession::connect("redis://127.0.0.1/", "my-session").await?;
    session.append(&[]).await?;
    let events = session.events(None).await?;
    println!("{} events", events.len());
    Ok(())
}
```

## BYO `ConnectionManager` (TLS, custom retry)

```rust,no_run
use paigasus_helikon_sessions_redis::RedisSession;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = redis::Client::open("rediss://user:pass@redis.example.com:6380/")?;
    let conn   = redis::aio::ConnectionManager::new(client).await?;
    let session = RedisSession::new(conn, "my-session");
    Ok(())
}
```

## Storage model

Each `append` call runs a Lua script that:

1. Reads the stream length (`XLEN`) to get the next sequence number.
2. Issues one `XADD` per event, tagging each with a contiguous `seq` field.
3. Returns the starting sequence number.

Because Redis executes Lua scripts atomically, two concurrent callers can
never produce the same sequence number or leave gaps.

## Operational notes

- **No automatic trimming.** The stream grows unboundedly. Configure
  `maxmemory-policy noeviction` so Redis does not silently evict entries,
  or manage stream length with a sidecar / TTL strategy.
- **Persistence.** Enable `appendonly yes` (AOF) or RDB snapshotting. A
  flush or restart without persistence loses all session events.

[session]: https://docs.rs/paigasus-helikon-core/latest/paigasus_helikon_core/trait.Session.html
[helikon]: https://github.com/SMK1085/paigasus-helikon
