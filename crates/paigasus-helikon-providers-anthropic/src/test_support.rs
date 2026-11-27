//! Shared helpers for unit tests.

/// Install an aws-lc rustls process default when the `rustls-aws-lc` feature is
/// off. Under that feature the body is empty, so the default path runs with no
/// installed provider.
pub(crate) fn install_crypto_provider() {
    #[cfg(not(feature = "rustls-aws-lc"))]
    {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
}
