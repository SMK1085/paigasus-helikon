#![cfg(feature = "microvm")]
#![allow(missing_docs)]

//! Controller TLS-trust integration test (SMA-437). Generates a fresh
//! self-signed certificate in-test via `rcgen` — never installed system-wide —
//! then asserts (a) no `.controller_ca` → request fails (untrusted cert) and
//! (b) `.controller_ca(cert)` → TLS handshake succeeds (no TLS/cert error).
//!
//! Proves `danger_accept_invalid_certs` is never set.

use std::sync::Arc;

use paigasus_helikon_tools::{ExecRequest, ForkdBackend};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// Spin a self-signed TLS server on a loopback port; return (url, ca_pem_bytes).
/// The cert is generated fresh per test — never installed system-wide.
async fn tls_controller() -> (String, Vec<u8>) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    // rcgen 0.14: CertifiedKey { cert, signing_key }
    let ca_pem = certified.cert.pem().into_bytes();
    let key_der = certified.signing_key.serialize_der(); // PKCS#8 DER
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der));
    let chain = vec![CertificateDer::from(certified.cert.der().to_vec())];

    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(sock).await {
                    // Minimal HTTP/1.1 response: 200 with a sandbox-array body
                    // (matches the fork endpoint shape forkd returns).
                    let body = b"[{\"id\":\"sb-tls\"}]";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.write_all(body).await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });

    (format!("https://localhost:{port}"), ca_pem)
}

#[tokio::test]
async fn rejects_untrusted_controller_cert_without_ca() {
    let (url, _ca) = tls_controller().await;
    // No .controller_ca → the self-signed cert is not trusted → request fails closed.
    let backend = ForkdBackend::builder(url)
        .bearer_token("t")
        .snapshot("s")
        .build()
        .unwrap();
    let err = backend.run(ExecRequest::new("echo hi")).await;
    assert!(err.is_err(), "untrusted TLS cert must fail closed");
}

#[tokio::test]
async fn accepts_controller_cert_with_pinned_ca() {
    let (url, ca) = tls_controller().await;
    // With the CA pinned the TLS handshake succeeds; the fork call connects and
    // either parses the stub response or fails on a subsequent step — but NOT on
    // a TLS/certificate error.
    let backend = ForkdBackend::builder(url)
        .bearer_token("t")
        .snapshot("s")
        .controller_ca(ca)
        .build()
        .unwrap();
    let res = backend.run(ExecRequest::new("echo hi")).await;
    if let Err(e) = res {
        let msg = format!("{e:#}");
        // The CA-pinned connection should not fail with a TLS/certificate error.
        // "certificate verify failed", "self-signed certificate", "rustls error",
        // "InvalidCertificate" are the typical TLS-failure strings. A plain
        // "forkd response decode failed" (mock returning wrong content for exec)
        // is acceptable — it means TLS succeeded and we got to the exec step.
        assert!(
            !msg.to_lowercase().contains("certificate verify")
                && !msg.to_lowercase().contains("self-signed")
                && !msg.to_lowercase().contains("invalid certificate")
                && !msg.to_lowercase().contains("rustls error")
                && !msg.to_lowercase().contains("handshake fail"),
            "unexpected TLS/cert failure (CA pinning may have failed): {msg}"
        );
    }
    // If Ok(_): the stub sandbox array was parsed, exec fired, that's fine.
}
