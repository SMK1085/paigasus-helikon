//! [`EgressProxy`] — an explicit forward proxy that enforces an [`EgressPolicy`]
//! on outbound traffic from the microVM tier. HTTPS is tunneled via `CONNECT`;
//! the destination host is checked against the domain allow/deny policy and
//! the resolved IPs are checked against the SSRF (private-range) block before
//! any upstream connection is made. Plain-HTTP absolute-URI requests are also
//! host-checked: non-allowlisted hosts receive `403 Forbidden`; allowlisted
//! hosts receive `501 Not Implemented` (plain-HTTP forwarding is not supported —
//! `CONNECT`/HTTPS is the enforced path). Non-proxy-aware traffic is dropped by
//! the per-VM netns default-deny (L3/L4) before it reaches the proxy.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::policy::EgressPolicy;

/// Per-connection I/O timeout: header read and DNS lookup must complete within this.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// An egress-policy-enforcing forward proxy. Run it with [`Self::serve`] against a
/// bound [`TcpListener`]; each accepted connection is handled on its own task.
///
/// The proxy is the application-layer half of the layered egress model (SMA-437):
/// it filters HTTP/S by domain. The L3/L4 default-deny that forces guest traffic
/// through it is the deployment's per-VM netns config (see the runbook).
pub struct EgressProxy {
    policy: Arc<EgressPolicy>,
}

impl EgressProxy {
    /// Build a proxy enforcing `policy`.
    pub fn new(policy: EgressPolicy) -> Self {
        Self {
            policy: Arc::new(policy),
        }
    }

    /// Accept connections on `listener` until it errors, handling each on a task.
    pub async fn serve(self, listener: TcpListener) -> io::Result<()> {
        loop {
            let (sock, _peer) = listener.accept().await?;
            let policy = Arc::clone(&self.policy);
            tokio::spawn(async move {
                let _ = handle(sock, policy).await;
            });
        }
    }
}

const DENY: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
const OK: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
const BAD: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
const MAX_HEAD: usize = 16 * 1024;

async fn handle(mut client: TcpStream, policy: Arc<EgressPolicy>) -> io::Result<()> {
    // Read the request head (request line + headers) up to CRLFCRLF, bounded.
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            client.write_all(BAD).await?;
            return Ok(());
        }
        let n = match tokio::time::timeout(IO_TIMEOUT, client.read(&mut byte)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                // Header read timed out.
                let _ = client.write_all(BAD).await;
                return Ok(());
            }
        };
        if n == 0 {
            return Ok(()); // client closed
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let Some(request_line) = text.lines().next() else {
        client.write_all(BAD).await?;
        return Ok(());
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(client, target, &policy).await
    } else if let Some(host) = absolute_uri_host(target) {
        // Plain HTTP via absolute-URI: enforce host, then deny (forwarding plain
        // HTTP is out of scope — proxy-aware HTTPS via CONNECT is the path; netns
        // default-deny drops non-proxy egress). Allowed plain-HTTP is rare; reject
        // with a clear 403 unless allow-listed, in which case 501 (not forwarded).
        if !policy.is_host_allowed(&host) {
            client.write_all(DENY).await?;
        } else {
            client
                .write_all(b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n")
                .await?;
        }
        Ok(())
    } else {
        client.write_all(BAD).await?;
        Ok(())
    }
}

async fn handle_connect(
    mut client: TcpStream,
    target: &str,
    policy: &EgressPolicy,
) -> io::Result<()> {
    let Some((host, port)) = split_host_port(target) else {
        client.write_all(BAD).await?;
        return Ok(());
    };
    if !policy.is_host_allowed(&host) {
        client.write_all(DENY).await?;
        return Ok(());
    }
    // Resolve and vet EVERY address (closes DNS-rebinding window).
    // Wrap lookup_host in a timeout to avoid hanging on a slow resolver.
    let addrs: Vec<SocketAddr> = match tokio::time::timeout(
        IO_TIMEOUT,
        tokio::net::lookup_host((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(it)) => it.filter(|a| policy.is_ip_allowed(a.ip())).collect(),
        Ok(Err(_)) | Err(_) => {
            client.write_all(DENY).await?;
            return Ok(());
        }
    };
    if addrs.is_empty() {
        client.write_all(DENY).await?; // resolved only to blocked IPs
        return Ok(());
    }
    // Try every vetted address; use the first that connects.
    let mut upstream_opt: Option<TcpStream> = None;
    for addr in addrs {
        match TcpStream::connect(addr).await {
            Ok(s) => {
                upstream_opt = Some(s);
                break;
            }
            Err(_) => continue,
        }
    }
    let mut upstream = match upstream_opt {
        Some(s) => s,
        None => {
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Ok(());
        }
    };
    client.write_all(OK).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// Split `host:port`, stripping IPv6 brackets. Returns `None` if malformed.
fn split_host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    if host.is_empty() {
        None
    } else {
        Some((host, port))
    }
}

/// Extract the host from an absolute-form HTTP request target (`http://host/..`).
fn absolute_uri_host(target: &str) -> Option<String> {
    let rest = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(authority);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_handles_ipv6_and_bad_input() {
        assert_eq!(
            split_host_port("example.com:443"),
            Some(("example.com".into(), 443))
        );
        assert_eq!(split_host_port("[::1]:8080"), Some(("::1".into(), 8080)));
        assert_eq!(split_host_port("noport"), None);
        assert_eq!(split_host_port(":443"), None);
    }

    #[test]
    fn absolute_uri_host_parses() {
        assert_eq!(
            absolute_uri_host("http://a.test/x").as_deref(),
            Some("a.test")
        );
        assert_eq!(
            absolute_uri_host("http://a.test:8080/x").as_deref(),
            Some("a.test")
        );
        assert_eq!(absolute_uri_host("/relative"), None);
    }
}
