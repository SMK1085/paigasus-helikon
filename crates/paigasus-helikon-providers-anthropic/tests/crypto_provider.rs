//! `build()` without a caller client under `rustls-no-provider` (SMA-868).
#![cfg(all(feature = "rustls-no-provider", not(feature = "rustls-aws-lc")))]
use paigasus_helikon_providers_anthropic::{AnthropicModel, BuildError};

#[test]
fn no_provider_build_needs_an_installed_default() {
    assert!(rustls::crypto::CryptoProvider::get_default().is_none());
    let err = AnthropicModel::messages("claude-sonnet-5-5")
        .api_key("sk-test")
        .build()
        .unwrap_err();
    assert!(matches!(err, BuildError::NoCryptoProvider), "got {err:?}");

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .unwrap();
    AnthropicModel::messages("claude-sonnet-5-5")
        .api_key("sk-test")
        .build()
        .expect("build succeeds once a default is installed");
}
