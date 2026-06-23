//! Run the SMA-437 egress proxy as a standalone process (used by the Docker
//! forkd+KVM harness). Reads bind addr from `EGRESS_BIND` (default 127.0.0.1:8443)
//! and a comma-separated allow-list from `EGRESS_ALLOW`.
//!
//! Run: `EGRESS_ALLOW=example.com cargo run -p paigasus-helikon-tools \
//!       --features microvm --example egress_proxy`

use paigasus_helikon_tools::{EgressPolicy, EgressProxy};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let bind = std::env::var("EGRESS_BIND").unwrap_or_else(|_| "127.0.0.1:8443".into());
    let allow: Vec<String> = std::env::var("EGRESS_ALLOW")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let policy = if allow.is_empty() {
        EgressPolicy::deny_all()
    } else {
        EgressPolicy::deny_all().allow_domains(allow)
    };
    let listener = TcpListener::bind(&bind).await?;
    eprintln!("egress proxy listening on {bind}");
    EgressProxy::new(policy).serve(listener).await
}
