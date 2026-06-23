#![cfg(feature = "microvm")]
#![allow(missing_docs)]

//! Live forkd integration tests. NOT `#[ignore]`'d: they compile on every PR (so
//! they cannot bit-rot) and skip LOUDLY when no controller is configured. Run on
//! an x86_64 KVM host with `FORKD_URL`/`FORKD_TOKEN`/`FORKD_SNAPSHOT` set (see
//! `docs/runbooks/forkd-live-validation.md`).

use std::time::{Duration, Instant};

use paigasus_helikon_tools::{EgressPolicy, ExecRequest, ForkdBackend};

/// Returns `(url, token, snapshot)` if all three env vars are set, otherwise
/// prints a loud skip message to stderr and returns `None`.
fn live_env() -> Option<(String, String, String)> {
    match (
        std::env::var("FORKD_URL"),
        std::env::var("FORKD_TOKEN"),
        std::env::var("FORKD_SNAPSHOT"),
    ) {
        (Ok(u), Ok(t), Ok(s)) => Some((u, t, s)),
        _ => {
            eprintln!(
                "SKIP live forkd test: set FORKD_URL, FORKD_TOKEN, FORKD_SNAPSHOT \
                 (+ optional FORKD_CA path, FORKD_PROXY) to run against a live KVM controller"
            );
            None
        }
    }
}

/// Build a backend from the live environment. `enforce` wires in `.enforce_egress`
/// using `FORKD_PROXY` (required when `enforce == true`).
fn backend(enforce: bool) -> Option<std::sync::Arc<dyn paigasus_helikon_tools::ExecutionBackend>> {
    let (url, token, snapshot) = live_env()?;
    let mut b = ForkdBackend::builder(url)
        .bearer_token(token)
        .snapshot(snapshot);
    if let Ok(ca_path) = std::env::var("FORKD_CA") {
        b = b.controller_ca(std::fs::read(ca_path).expect("FORKD_CA file readable"));
    }
    if enforce {
        let Ok(proxy) = std::env::var("FORKD_PROXY") else {
            eprintln!(
                "SKIP enforced-egress test: set FORKD_PROXY to run against a live egress proxy"
            );
            return None;
        };
        b = b
            .egress_policy(EgressPolicy::deny_all().allow_domains(["example.com"]))
            .enforce_egress(proxy);
    }
    Some(b.build().expect("backend builds"))
}

#[tokio::test]
async fn live_forkd_runs_bash_in_a_microvm() {
    let Some(backend) = backend(false) else {
        return;
    };
    let out = backend
        .run(ExecRequest::new("echo from-a-microvm"))
        .await
        .unwrap();
    assert_eq!(out.stdout.trim(), "from-a-microvm");
    assert_eq!(out.exit_code, Some(0));
}

#[tokio::test]
async fn live_forkd_denies_nonallowlisted_egress() {
    let Some(backend) = backend(true) else {
        return;
    };
    // A proxy-aware client hitting a NON-allowlisted domain must fail FAST (proxy
    // 403), distinguishing "denied" from "hung/timeout".
    let start = Instant::now();
    let out = backend
        .run(ExecRequest::new(
            "curl -s -o /dev/null -w '%{http_code}' --max-time 5 https://evil.test || echo DENIED",
        ))
        .await
        .unwrap();
    assert!(
        out.stdout.contains("DENIED") || out.stdout.trim() == "403",
        "non-allowlisted egress should be denied, got: {:?}",
        out.stdout
    );
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "deny must be fast (< 8s), not a hang"
    );

    // The allowlisted domain must succeed.
    let ok = backend
        .run(ExecRequest::new(
            "curl -s -o /dev/null -w '%{http_code}' --max-time 5 https://example.com",
        ))
        .await
        .unwrap();
    assert_eq!(
        ok.stdout.trim(),
        "200",
        "allow-listed egress should succeed"
    );
}
