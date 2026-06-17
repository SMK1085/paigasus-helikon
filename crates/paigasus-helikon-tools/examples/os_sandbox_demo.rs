//! OS-sandbox demo (Linux/macOS). Run with:
//!   cargo run -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo
#![allow(missing_docs)]

#[cfg(all(
    feature = "os-sandbox",
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        target_os = "macos"
    )
))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `ExecutionBackend` is intentionally not imported: `build()` returns
    // `Arc<dyn ExecutionBackend>`, and trait methods on a trait object resolve
    // without the trait in scope.
    use paigasus_helikon_tools::{ExecRequest, OsSandboxBackend, Sandbox};

    let dir = tempfile::tempdir()?;
    let backend = match OsSandboxBackend::builder(Sandbox::open(dir.path())?).build() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("OS sandbox unavailable ({e}); this host lacks Landlock.");
            return Ok(());
        }
    };
    println!("guarantees: {:?}", backend.guarantees());

    let blocked = backend
        .run(ExecRequest::new(
            "echo pwned > /tmp/escape_demo.txt; echo rc=$?",
        ))
        .await?;
    println!("write-outside-root attempt → {}", blocked.stdout.trim());
    Ok(())
}

#[cfg(not(all(
    feature = "os-sandbox",
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        target_os = "macos"
    )
)))]
fn main() {
    eprintln!("This example requires --features os-sandbox on Linux (x86_64/aarch64) or macOS.");
}
