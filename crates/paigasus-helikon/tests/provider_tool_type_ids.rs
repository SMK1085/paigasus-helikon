//! The Bedrock provider matches the provider id "anthropic" (SMA-844).
#![cfg(feature = "anthropic")]

#[test]
fn anthropic_provider_id_is_the_value_that_bedrock_matches() {
    assert_eq!(paigasus_helikon::anthropic::PROVIDER_ID, "anthropic");
}
