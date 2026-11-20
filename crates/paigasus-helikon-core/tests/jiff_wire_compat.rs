//! Pins the JSON form of `SessionEvent.ts`, as jiff 0.2 writes it.
//!
//! A jiff major upgrade must not change the stored session rows (SMA-831).
//! Each test reads a stored row and writes it again. The bytes must stay the same.

use jiff::Timestamp;
use paigasus_helikon_core::SessionEvent;

/// Reads `json`, checks the timestamp, writes the event again, and compares the bytes.
fn assert_bytes_kept(json: &str, expected_ts: Timestamp) {
    let event: SessionEvent = serde_json::from_str(json).expect("the row must parse");
    assert_eq!(event.ts(), expected_ts);
    let written = serde_json::to_string(&event).expect("the event must serialize");
    assert_eq!(written, json);
}

/// Builds the JSON of a `user_message` event with the given timestamp text.
fn row(ts: &str) -> String {
    format!(r#"{{"type":"user_message","content":[{{"type":"text","text":"hi"}}],"ts":"{ts}"}}"#)
}

#[test]
fn nanosecond_ts_keeps_its_bytes() {
    assert_bytes_kept(
        &row("2026-11-20T15:08:42.123456789Z"),
        Timestamp::new(1_795_187_322, 123_456_789).unwrap(),
    );
}

#[test]
fn trimmed_fraction_ts_keeps_its_bytes() {
    assert_bytes_kept(
        &row("2026-11-20T15:08:42.5Z"),
        Timestamp::new(1_795_187_322, 500_000_000).unwrap(),
    );
}

#[test]
fn whole_second_ts_keeps_its_bytes() {
    assert_bytes_kept(
        &row("2026-11-20T15:08:42Z"),
        Timestamp::new(1_795_187_322, 0).unwrap(),
    );
}

#[test]
fn utc_offset_ts_parses() {
    let event: SessionEvent =
        serde_json::from_str(&row("2026-11-20T15:08:42.123456789+00:00")).expect("must parse");
    assert_eq!(
        event.ts(),
        Timestamp::new(1_795_187_322, 123_456_789).unwrap()
    );
}
