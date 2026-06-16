//! OS-sandbox demo (Linux). Run with:
//!   cargo run -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo
#![allow(missing_docs)]

#[cfg(all(feature = "os-sandbox", target_os = "linux"))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use paigasus_helikon_tools::{ExecRequest, ExecutionBackend, OsSandboxBackend, Sandbox};

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

#[cfg(not(all(feature = "os-sandbox", target_os = "linux")))]
fn main() {
    eprintln!("This example requires --features os-sandbox on Linux.");
}
