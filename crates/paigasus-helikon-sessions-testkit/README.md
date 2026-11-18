# paigasus-helikon-sessions-testkit

Session conformance suite for Paigasus Helikon backends, including your own.
The suite runs four tests against a `Session` implementation: append and read,
exclusive watermark, projection, and concurrent writers. The session backend
crates of Paigasus Helikon use it. You can use it to test a backend that you
write.

- [API reference (docs.rs)](https://docs.rs/paigasus-helikon-sessions-testkit)
- [Guide & concepts](https://smk1085.github.io/paigasus-helikon/) — see [sessions](https://smk1085.github.io/paigasus-helikon/concepts/sessions.html)
- [Source & issues](https://github.com/SMK1085/paigasus-helikon)

## Install

```sh
cargo add --dev paigasus-helikon-sessions-testkit
cargo add --dev paigasus-helikon-core
cargo add --dev tokio --features macros,rt-multi-thread
```

Use the same `paigasus-helikon-core` version as your backend. If your backend
already depends on core, you do not need the dev-dependency.

## Run the suite against your backend

Add this test to your crate. The factory must return a fresh, empty session
on each call. The suite calls the factory once for each sub-test.

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use paigasus_helikon_core::{MemorySession, Session};
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn my_session_passes_conformance() {
    let pid = std::process::id();
    let counter = AtomicU64::new(0);
    run_all(|| {
        let id = counter.fetch_add(1, Ordering::SeqCst);
        let _session_id = format!("conf-{pid}-{id}"); // use this id for a shared database
        async move { Arc::new(MemorySession::new()) as Arc<dyn Session> }
    })
    .await;
}
```

Replace `MemorySession::new()` with your backend, opened under `_session_id`,
so that each call gets a fresh, empty session.

`run_all` needs a Tokio runtime, because `run_concurrent_writers` calls
`tokio::spawn`. Use the `multi_thread` flavor with `worker_threads = 4`.

## Run a subset

Each test is also a public function. Each one takes the same factory as
`run_all`:

- `run_append_read`
- `run_watermark_exclusive`
- `run_projection`
- `run_concurrent_writers`

## Same core version

Your session type and this crate must use the same `paigasus-helikon-core`
version. Run this command in your crate:

```sh
cargo tree -i paigasus-helikon-core
```

If it fails with "specification ... is ambiguous", your crate and the testkit
resolve two core versions. Align your `paigasus-helikon-core` requirement with
the requirement of the testkit.

## Stability

On 0.x versions, new sub-tests arrive in patch releases. A new sub-test can
fail a backend that passed before. To control the upgrade, pin the version
with `=0.1.x`, for example `=0.1.0`.

## License

Licensed under either of [Apache-2.0](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-APACHE) or [MIT](https://github.com/SMK1085/paigasus-helikon/blob/main/LICENSE-MIT), at your option.
