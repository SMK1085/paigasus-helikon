#![cfg(feature = "microvm")]
#![allow(missing_docs)]

use std::time::Duration;

use paigasus_helikon_tools::{EgressPolicy, EgressProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Start the proxy on an ephemeral loopback port; return its `host:port`.
async fn start_proxy(policy: EgressPolicy) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { EgressProxy::new(policy).serve(listener).await });
    format!("127.0.0.1:{}", addr.port())
}

/// Send a raw CONNECT and return the proxy's status line.
async fn connect_status(proxy: &str, target: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(proxy).await.unwrap();
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = vec![0u8; 128];
    let n = s.read(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

#[tokio::test]
async fn connect_to_nonallowlisted_domain_is_denied_fast() {
    let proxy = start_proxy(EgressPolicy::deny_all().allow_domains(["example.com"])).await;
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        connect_status(&proxy, "evil.test:443"),
    )
    .await
    .expect("deny must be fast, not a hang");
    assert!(status.contains("403"), "expected 403, got: {status}");
}

#[tokio::test]
async fn connect_to_domain_resolving_to_private_ip_is_denied() {
    // localhost resolves to loopback (blocked); allow it by domain but not by IP.
    let proxy = start_proxy(EgressPolicy::deny_all().allow_domains(["localhost"])).await;
    let status = connect_status(&proxy, "localhost:9").await;
    assert!(
        status.contains("403"),
        "SSRF: private IP must be denied, got: {status}"
    );
}

#[tokio::test]
async fn connect_to_allowlisted_loopback_tunnels_bytes() {
    // A loopback echo upstream; allow private IPs + the host so the tunnel forms.
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = upstream.accept().await.unwrap();
        let mut b = [0u8; 5];
        sock.read_exact(&mut b).await.unwrap();
        sock.write_all(&b).await.unwrap();
    });
    let proxy = start_proxy(
        EgressPolicy::deny_all()
            .allow_domains(["127.0.0.1"])
            .allow_private_ips(true),
    )
    .await;
    let mut s = tokio::net::TcpStream::connect(&proxy).await.unwrap();
    let target = format!("127.0.0.1:{}", up_addr.port());
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut line = vec![0u8; 64];
    let n = s.read(&mut line).await.unwrap();
    assert!(String::from_utf8_lossy(&line[..n]).contains("200"));
    // Tunnel established; echo round-trips.
    s.write_all(b"hello").await.unwrap();
    let mut echo = [0u8; 5];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"hello");
}
