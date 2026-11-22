# SMA-861: script the captured split `input_json_delta` — design

Linear: [SMA-861](https://linear.app/smaschek/issue/SMA-861). Size: a small change.

## Intent

The `anthropic` module of `tests/provider-stream-conformance/tests/conformance.rs`
sends each tool call's argument JSON in one `input_json_delta`. That shape comes
from the HAND-AUTHORED SMA-317 fixtures `parallel_tool_use.txt` and
`tool_use_then_continuation.txt`. The real API sends a different shape. The three
SMA-840 captures each send an empty first `input_json_delta`
(`"partial_json":""`), then the arguments in three fragments:

- `thinking_text_tool_use.txt` lines 143-153
- `empty_thinking_tool_use.txt` lines 94-104
- `redacted_thinking_tool_use.txt` lines 95-105

All three captures send the same four `partial_json` values:
`""`, `{"city"`, `: "Pari`, `s"}`.

The suite's provenance rule says that scripts copy captured or committed traffic.
This change makes the two Anthropic tool scripts send the captured shape and the
captured bytes.

## Goals

- `ToolCallCleanStop` sends the four captured `input_json_delta` values, in order,
  at index 0.
- `ThinkingToolCallCleanStop` sends the same four values at index 1.
- Assertion 7 ("exactly one delta for a `call_id` carries the name") still passes.
- The module doc section "`ToolCallCleanStop` never splits `input_json_delta`" is
  rewritten to describe the split, and it cites the three capture line ranges.
- `cargo test -p paigasus-helikon-provider-stream-conformance` passes.
- `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-features --all-targets -- -D warnings` pass.
- Mutation check (recorded in the PR body, not committed). Make each change to
  `crates/paigasus-helikon-providers-anthropic/src/stream.rs` lines 175-185, run
  `anthropic::conforms`, record the result, then revert:
  - (a) the translator puts the name on every delta. Expected: assertion 7 fails
    with `ToolNameNotExactlyOnce` (count 4). The old one-delta script cannot catch
    this. This is the coverage that the change adds.
  - (b) the translator drops the empty delta but still sets `name_emitted`.
    Expected: the floor at `src/lib.rs` lines 560-567 ("no ToolCallDelta carried
    the tool name") fails.
  - (c) the translator drops empty deltas before the name logic, so the name
    moves to `{"city"`. Expected: the suite passes. This is correct, because the
    core contract does not say which delta carries the name.

## Book and README

No `docs/book/` or `README.md` edit is necessary. The change is test-only, and
the crate has `publish = false` (`tests/provider-stream-conformance/Cargo.toml`
line 5).

## Non-goals

- No change to the harness (`src/lib.rs`) or to the translator (`stream.rs`).
- No change to the `bedrock`, `openai_chat`, `litellm` or other modules.
- The tool name stays `get_weather` and the tool id stays `tu_conformance_0`. The
  captures use `lookup_city_code` and real ids. This change copies only the
  argument fragments.
- The scripts still do not send the capture's `caller` key, `ping` events or the
  capture's text block. Those differences stay as they are documented now.

## Anti-goals

- The empty first delta must not be removed or merged into the first fragment.
  The property under test is: for a call with four deltas whose first delta is
  empty, exactly one delta carries the name. Today the translator gives the name
  to the first delta, also when `partial_json` is empty (`stream.rs` lines
  165-185), and the three fragments after it carry `name: None`. The suite does
  not check WHICH delta carries the name. The doc text must not claim that it
  does.
- Fragment text must not be invented. A fragment that no capture contains breaks
  the provenance rule.

## Approaches

- **A — copy the capture fragments verbatim (chosen by Sven).** The fragments are
  `""`, `{"city"`, `: "Pari`, `s"}`. The joined arguments are
  `{"city": "Paris"}`. The request's user message changes from Berlin to Paris so
  that the exchange stays coherent. Every argument byte comes from a capture.
- **B — keep Berlin and split at the same points.** The fragments would be `""`,
  `{"city"`, `:"Berl`, `in"}`. The diff is smaller, but no capture contains these
  bytes. Rejected: it copies the shape but not the data.

## Design

1. Add one constant in the `anthropic` module:

   ```rust
   /// The four `input_json_delta` values that each of the three CAPTURED
   /// SMA-840 fixtures sends for its tool call, in order. The first one is
   /// empty. ...cites the three line ranges...
   const CAPTURED_ARGS: &[&str] = &["", "{\"city\"", ": \"Pari", "s\"}"];
   ```

   Grounding: the constant cites all three captures, because all three send the
   same bytes. The two HAND-AUTHORED SMA-840 fixtures
   (`interleaved_thinking.txt`, `unsigned_thinking_tool_use.txt`) split the
   arguments differently and are not a source.

2. `ToolCallCleanStop`: replace the one `input_json_delta(...)` with one
   `input_json_delta(a)` for each `a` in `CAPTURED_ARGS`.
3. `ThinkingToolCallCleanStop`: replace the one `input_json_delta_at(1, ...)` with
   one `input_json_delta_at(1, a)` for each `a` in `CAPTURED_ARGS`.
4. `request()`: change the user message to `"What is the weather in Paris?"`.
   This is for coherence only. No check reads the request body
   (`PacedServer` reads only the first request's headers), and the provenance
   rule applies to served bytes, not to the request. The other modules keep
   Berlin.
5. `scan_finds_only_a_populated_stop_reason`: replace the one Berlin
   `input_json_delta` entry with one entry that uses `CAPTURED_ARGS[1]`. More
   entries add no check: `serde_json` escapes each `"` in `partial_json`, so no
   value can make the marker bytes.
6. Rewrite the documentation to match. Line numbers are on `main` at `94bcf64a`:
   - lines 1667-1671 ("Only one fixture that a helper copies is CAPTURED ...
     the `tool_use` helpers copy HAND-AUTHORED shapes"): the three CAPTURED
     SMA-840 fixtures now ground the `input_json_delta` values. The
     `tool_use_start` shape stays HAND-AUTHORED.
   - lines 1712-1721, the bullet for the SMA-317 files: they ground only
     `tool_use_start`'s shape now, not the one-delta argument shape. Remove the
     reference to the old section title at line 1721.
   - lines 1722-1734, the `thinking_text_tool_use.txt` bullet: it also grounds
     `CAPTURED_ARGS`. Remove "one whole `input_json_delta`" from the list of
     differences.
   - lines 1744-1748: `empty_thinking_tool_use.txt` and
     `redacted_thinking_tool_use.txt` now ground `CAPTURED_ARGS`. Move them out
     of the "no helper copies a shape from" list.
   - lines 1805-1826, the section "`ToolCallCleanStop` never splits
     `input_json_delta`": rewrite it under a new title (for example "Tool scripts
     send the captured split `input_json_delta`"). Remove lines 1819-1822 (the
     SMA-845 audit history). Say that the floor at `src/lib.rs` lines 560-567
     catches "zero deltas carry the name", and that assertion 7 catches "more
     than one". Do not claim that the suite checks which delta carries the name.
   - lines 2065-2070, the `input_json_delta` helper doc: remove "carrying a
     whole argument JSON", the reference to the old title, and "never called
     twice".
   - lines 2195-2201, the `ToolCallCleanStop` comment: remove "the whole argument
     JSON arrives on one `input_json_delta`", the reference to the old title,
     and the claim that the script matches `parallel_tool_use.txt`'s
     single-call shape for the delta part.
   - lines 2223-2230, the `ThinkingToolCallCleanStop` comment: remove "one whole
     `input_json_delta`, not an empty one and three fragments" from the list of
     differences.
   - After the edits, `grep -n "never splits\|one whole\|whole argument"` on the
     file finds no stale text in the `anthropic` module.

## Left to the builder

- The exact wording of the rewritten doc text, in STE.
- How to push the fragments (`extend` with `iter().map(...)`, or a small helper).

## Risk

SMA-567 is open on another branch and edits the same file. Its branch does not
change these two scripts, but it edits nearby doc text. A merge conflict is
possible. SMA-567 adds a check that joins `args_delta` values per `call_id` in
stream order (`push_str`), so an empty fragment does no harm. At its head
`526afd29`, it declares no expected calls (`&[]`) for the `anthropic`
`ToolCallCleanStop` and `ThinkingToolCallCleanStop`, so its check does not read
these scripts. The branch that merges second must rebase and run the full suite
again, not only fix the text conflict.
