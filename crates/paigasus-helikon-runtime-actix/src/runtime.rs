//! Process-wide executor for detached run work. actix runs each worker on its
//! own single-threaded `actix-rt` runtime (recyclable if a worker dies), so run
//! writer tasks and the registry sweeper run on ONE process-wide multi-thread
//! tokio runtime instead. It is created lazily on a dedicated OS thread — a
//! fresh thread avoids the "cannot create a runtime from within a runtime" panic
//! that firing `Runtime::new()` inside `#[actix_web::main]`/`#[tokio::test]`
//! would cause — and held `'static`, so it is never dropped in an async context.
use std::sync::OnceLock;
use tokio::runtime::{Builder, Handle};

static SHARED: OnceLock<Handle> = OnceLock::new();

/// Handle to the process-wide runtime that executes run writer tasks and the
/// registry sweeper. Lazily initialised on first use.
pub(crate) fn shared_handle() -> Handle {
    SHARED
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("helikon-actix-rt".to_owned())
                .spawn(move || {
                    let rt = Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .expect("build shared runtime");
                    tx.send(rt.handle().clone()).expect("send runtime handle");
                    rt.block_on(std::future::pending::<()>()); // keep alive for process lifetime
                })
                .expect("spawn shared runtime thread");
            rx.recv().expect("receive runtime handle")
        })
        .clone()
}
