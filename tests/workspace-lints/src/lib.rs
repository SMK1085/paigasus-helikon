//! Workspace-wide source lints for the Helikon repo.
//!
//! Internal, never published. See
//! `docs/superpowers/specs/2026-08-19-sma-543-tracing-target-design.md`.

use std::collections::BTreeSet;

/// One `target =` / `parent =` argument found inside a `tracing` macro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offense {
    /// 1-based line number of the offending keyword.
    pub line: usize,
    /// Macro it appeared in, unqualified (e.g. `warn` for `tracing::warn!`).
    pub macro_name: String,
    /// Which keyword was misused: `target` or `parent`.
    pub keyword: String,
}

/// Macros where `target:` / `parent:` are macro syntax, not field names.
const TRACING_MACROS: &[&str] = &[
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "event",
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
    // Predicate macros — not used anywhere in this workspace today, but they
    // also accept `target:` and are included here for completeness.
    "enabled",
    "event_enabled",
    "span_enabled",
];

/// Keywords that must be introduced with `:` and never with `=`.
const KEYWORDS: &[&str] = &["target", "parent"];

/// Opt-out marker for a call site where `target`/`parent` are legitimate
/// field names, not the macro's target/parent syntax — e.g.
/// `tracing::info!(target: "paigasus::http", target = %uri, "req")`, where
/// the second `target` is an ordinary field. Written as a `// allow(...)`
/// comment either on the line immediately before the macro invocation, or
/// trailing the invocation's own line.
const ALLOW_MARKER: &str = "// allow(tracing-target-syntax)";

/// Opt-out marker for a call site that is deliberately exempt from the
/// `target:` coverage requirement Task 9 enforces — e.g. a legitimate
/// untargeted or computed-target invocation that cannot be made a literal.
/// Same placement rules as the syntax lint's own marker: a `// allow(...)`
/// comment either immediately before the invocation or trailing its own line.
pub const ALLOW_MARKER_COVERAGE: &str = "// allow(tracing-target-coverage)";

/// An interior delimiter did not close the way the argument walker expected
/// while walking a macro invocation's argument list. Well-formed Rust nests
/// delimiters strictly, so this should be unreachable against real source —
/// it exists to catch a future desync between the trivia masker and the
/// walker's depth tracking, surfaced as a value a caller can
/// attribute to the file it was reading rather than a `debug_assert_eq!`
/// panic naming only a byte offset (second SMA-543 review wave).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MismatchedDelimiter {
    /// Byte offset into the masked source where the mismatch was found.
    pub byte: usize,
    /// The closing byte actually encountered.
    pub found: u8,
    /// The closing byte the invocation's own opening delimiter required.
    pub expected: u8,
}

impl std::fmt::Display for MismatchedDelimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mismatched delimiter ending macro invocation at byte {}: found {:?}, expected {:?}",
            self.byte, self.found as char, self.expected as char
        )
    }
}

impl std::error::Error for MismatchedDelimiter {}

/// Scan Rust source for `tracing` macro arguments written `target = …` or
/// `parent = …`, which the macros silently treat as ordinary fields.
///
/// Every top-level argument is inspected, not just the first: for the span and
/// event macros the correct syntax puts `target:` *before* the level or span
/// name, so the erroneous form is only reachable in a later position.
///
/// A macro invocation is recognised by its **final path segment** alone —
/// `tracing::warn!`, `crate::obs::warn!`, `self::warn!`, a bare `warn!`
/// reached via `use tracing::warn;`, and a bare alias reached via
/// `use tracing::warn as w;` are all matched — because a qualifier (or an
/// entire import path) can be renamed or re-exported and still reach the
/// exact same macro; requiring the qualifier to read literally `tracing`
/// produced false negatives (second review wave). The accepted cost (spec
/// §4.7) is that an unrelated macro whose final segment merely collides
/// with a tracing macro name, e.g. `mycrate::warn!(target = "x", "m")`, is
/// now flagged too — silence a genuine collision with the
/// `// allow(tracing-target-syntax)` escape hatch below.
///
/// `target`/`parent` are also legal field names in ordinary code. A call
/// site may opt out of detection with a `// allow(tracing-target-syntax)`
/// comment, either on the line immediately before the invocation or
/// trailing the invocation's own line — e.g.
/// `tracing::info!(target: "ns", target = %uri, "m"); // allow(tracing-target-syntax)`.
///
/// Comments and literals are blanked before scanning, so a macro written out
/// inside a comment, doc example or string is never flagged.
///
/// # Panics
///
/// Panics, without file context, if the source contains a delimiter
/// mismatch that should be unreachable against valid Rust — see [`try_scan`]
/// for a form that surfaces this diagnosably instead.
pub fn scan(src: &str) -> Vec<Offense> {
    try_scan(src).unwrap_or_else(|e| panic!("{e}"))
}

/// Fallible form of [`scan`]. Returns [`MismatchedDelimiter`] instead of
/// panicking, so a caller walking many files (see
/// `tests/workspace-lints/tests/tracing_target_syntax.rs`) can attribute the
/// failure to the file it was reading — something a panic raised from
/// inside this `&str`-only function cannot do on its own.
pub fn try_scan(src: &str) -> Result<Vec<Offense>, MismatchedDelimiter> {
    let masked = mask_trivia(src);
    let allow_lines = collect_allow_marker_lines(src, &masked.line_comments, ALLOW_MARKER);
    let b = &masked.buf[..];
    let aliases = collect_macro_aliases(b);
    let mut offenses = Vec::new();
    walk_invocations(b, &aliases, |inv| {
        let macro_line = line_of(b, inv.name_start);
        // The marker suppresses the whole invocation, so this checks the
        // invocation's own start line, not each offending keyword's line
        // (an invocation can span many lines).
        let suppressed = allow_lines.contains(&macro_line)
            || (macro_line > 1 && allow_lines.contains(&(macro_line - 1)));
        if !suppressed {
            collect_args(b, inv.open, inv.closer, inv.name, &mut offenses)?;
        }
        Ok(())
    })?;
    Ok(offenses)
}

/// One recognised `tracing`-macro invocation's location, as found by
/// [`walk_invocations`]: `name` is the macro's final path segment,
/// `name_start` the byte offset that segment begins at (used for line
/// reporting), and `open`/`closer` locate its argument list the way
/// [`for_each_top_level_arg`] expects.
struct MacroInvocation<'a> {
    name: &'a str,
    name_start: usize,
    open: usize,
    closer: u8,
}

/// Find every recognised `tracing`-macro invocation in the masked buffer `b`
/// and call `f` with its location. Shared by [`try_scan`] (which flags
/// `target =` / `parent =` syntax offenses inside each invocation) and
/// [`scan_invocations`] (which classifies each invocation's `target:`
/// argument), so the "find `ident !` followed by an opening delimiter, then
/// recognise the identifier as a tracing macro" walk lives in exactly one
/// place. `f` may fail with [`MismatchedDelimiter`] while walking the
/// invocation's own argument list; that error is propagated immediately.
fn walk_invocations<'a>(
    b: &'a [u8],
    aliases: &[String],
    mut f: impl FnMut(MacroInvocation<'a>) -> Result<(), MismatchedDelimiter>,
) -> Result<(), MismatchedDelimiter> {
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'!' {
            i += 1;
            continue;
        }
        let Some((name_start, name_end)) = ident_range_before(b, i) else {
            i += 1;
            continue;
        };
        let Ok(name) = std::str::from_utf8(&b[name_start..name_end]) else {
            i += 1;
            continue;
        };
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_whitespace() {
            j += 1;
        }
        // Macros accept all three delimiters with identical token trees —
        // `warn!(...)`, `warn![...]` and `warn!{...}` are equally valid Rust
        // and equally reproduce the SMA-543 defect, so all three must be
        // recognised as an invocation, not just `(`.
        let closer = match b.get(j) {
            Some(b'(') => b')',
            Some(b'[') => b']',
            Some(b'{') => b'}',
            _ => {
                i += 1;
                continue;
            }
        };
        let is_tracing_macro = TRACING_MACROS.contains(&name) || aliases.iter().any(|a| a == name);
        if is_tracing_macro {
            f(MacroInvocation {
                name,
                name_start,
                open: j,
                closer,
            })?;
        }
        i = j + 1;
    }
    Ok(())
}

/// Distinct `<component>` segments of every `target: "paigasus::…"` literal in
/// one file's source.
///
/// This is the source half of the doc-sync guard in
/// `tests/workspace-lints/tests/tracing_target_docs.rs`: the components found
/// here must match the ones the mdBook documents. It reports **components
/// only** — the `::<subsystem>` leaf is explicitly free to change (SMA-557 D1),
/// so guarding it would redden CI on legitimate refactors.
///
/// Comments, char literals and text nested inside a string literal are invisible
/// to it, because it looks for `target:` in `mask_trivia`'s masked buffer and
/// reads the literal's contents back out of the original source.
///
/// Not macro-aware: it keys on a `target:` token followed by a `paigasus::`
/// literal, so a non-`tracing` field named `target` holding such a string would
/// be a false positive. Such sites **do** exist in this workspace —
/// `crates/paigasus-helikon-core/src/command_match.rs` builds a `Redirect {
/// target: "...".into() }` struct literal with no `tracing` macro anywhere
/// near it — but none of them happen to hold a `"paigasus::…"`-shaped string,
/// so `scan_targets` tolerates them only because a non-`paigasus::` literal
/// yields `None` from the `component_of` helper; a future site whose non-tracing
/// `target:` field happened to start with `"paigasus::"` would be a silent
/// false positive here. [`scan_invocations`] is immune to this class of bug
/// structurally, because it only classifies a `target:` argument found
/// while walking a *recognised macro invocation's* argument list.
///
/// A comment may sit between `target:` and its literal — `tracing` accepts
/// `target: /* note */ "paigasus::x::y"` — and that form is recognised. What is
/// **not** recognised is a target that is not a literal at all:
/// `target: SOME_CONST`, a `const &'static str`, which `tracing` also accepts,
/// yields no component. No such site exists in this workspace today.
pub fn scan_targets(src: &str) -> BTreeSet<String> {
    const NEEDLE: &[u8] = b"target:";
    let masked = mask_trivia(src);
    let b = &masked.buf[..];
    let mut out = BTreeSet::new();
    let mut i = 0;
    while let Some(rel) = find_sub(&b[i..], NEEDLE) {
        let after = i + rel + NEEDLE.len();
        // Take the next literal whose span is separated from `target:` by
        // nothing but whitespace *in the masked buffer*. That test is what
        // makes a comment in the gap transparent: `mask_trivia` blanks
        // comments to spaces, so they read as whitespace here, while any real
        // token — an identifier, a `format!`, an opening paren — does not, and
        // correctly rejects the match. Testing the raw source instead would
        // stop at the comment's leading `/` and silently skip the site.
        if let Some(&(start, end)) = masked.string_literals.iter().find(|&&(start, _)| {
            start >= after && b[after..start].iter().all(u8::is_ascii_whitespace)
        }) {
            if let Some(component) = component_of(&src[start..end]) {
                out.insert(component);
            }
        }
        i = after;
    }
    out
}

/// The `<component>` of a `"paigasus::<component>::…"` string literal, given the
/// literal's raw text **including** its delimiters.
///
/// Returns `None` for a literal outside the namespace, or one whose component
/// segment is empty (`"paigasus::"`).
fn component_of(literal: &str) -> Option<String> {
    let content = literal_content(literal)?;
    let rest = content.strip_prefix("paigasus::")?;
    let component = match rest.find("::") {
        Some(k) => &rest[..k],
        None => rest,
    };
    if component.is_empty() {
        None
    } else {
        Some(component.to_owned())
    }
}

/// The content of a string literal, given its raw text **including** its
/// delimiters and any `r#…#`/`b`/`c` prefix — i.e. the text strictly between
/// the first and last `"` byte. Shared by [`component_of`] (which further
/// requires a `paigasus::` prefix) and [`classify_target_arg`] (which does
/// not).
///
/// Returns `None` if `literal` contains no `"` pair at all, which should not
/// happen for a span taken from [`Masked::string_literals`].
fn literal_content(literal: &str) -> Option<&str> {
    let open = literal.find('"')?;
    let close = literal.rfind('"')?;
    if close <= open {
        return None;
    }
    literal.get(open + 1..close)
}

/// Index of the first occurrence of `needle` in `haystack`, or `None`.
///
/// `std` has no substring search for `&[u8]`, and this crate takes no
/// dependencies.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Start/end (exclusive) of the identifier immediately preceding `at`, if
/// any, skipping any whitespace between the identifier and `at` (so
/// `warn ! (` still finds `warn`). Stops at `:`, so a qualified
/// `tracing::warn!` yields just the `warn` range.
fn ident_range_before(b: &[u8], at: usize) -> Option<(usize, usize)> {
    let mut e = at;
    while e > 0 && b[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    let mut s = e;
    while s > 0 && is_ident_byte(b[s - 1]) {
        s -= 1;
    }
    if s == e {
        return None;
    }
    Some((s, e))
}

/// Line numbers (1-based) in `src` that carry a `// allow(<marker>)`-shaped
/// opt-out comment — [`ALLOW_MARKER`] or [`ALLOW_MARKER_COVERAGE`] — either
/// as a standalone comment or trailing code.
///
/// Matched against the *raw* source, because the marker text lives inside a
/// comment and [`mask_trivia`] has already blanked comments to spaces by the
/// time this runs. But raw-text matching alone cannot tell a genuine `//`
/// comment from marker-shaped text that merely appears inside a string, a
/// raw string, or a block comment — so an occurrence only counts when its
/// byte range falls inside `line_comment_ranges`, the genuine line-comment
/// spans [`mask_trivia`] already identified while masking. Re-deriving those
/// boundaries independently here, instead of reusing them, would risk a
/// second lexer disagreeing with the first about what counts as a comment.
fn collect_allow_marker_lines(
    src: &str,
    line_comment_ranges: &[(usize, usize)],
    marker: &str,
) -> BTreeSet<usize> {
    let bytes = src.as_bytes();
    src.match_indices(marker)
        .filter(|&(pos, _)| {
            let end = pos + marker.len();
            line_comment_ranges
                .iter()
                .any(|&(start, stop)| pos >= start && end <= stop)
        })
        .map(|(pos, _)| line_of(bytes, pos))
        .collect()
}

/// Public form of the internal allow-marker search for a caller (Task 8's
/// integration test, Task 9's coverage lint) that only has `src` and does not
/// already hold the masker's line-comment ranges — it re-derives them itself
/// from `src` to get the line-comment ranges the search needs.
///
/// `marker` is any `// allow(...)`-shaped opt-out text, typically the syntax
/// lint's own marker or [`ALLOW_MARKER_COVERAGE`].
pub fn allow_marker_lines(src: &str, marker: &str) -> BTreeSet<usize> {
    let masked = mask_trivia(src);
    collect_allow_marker_lines(src, &masked.line_comments, marker)
}

/// Local names that resolve to a `tracing` macro via
/// `use <path>::<macro> as <alias>;` (or the grouped form
/// `use <path>::{<macro> as <alias>, ...};`), so a fully renamed bare call
/// like `w!(target = "x", "m")` is still matched by its resolved identity.
/// Operates on the masked buffer, so an alias mentioned only inside a
/// comment or string is ignored.
///
/// A candidate word is only treated as an import alias when it sits
/// immediately after `::`, `{` or `,` (modulo whitespace) and immediately
/// before `as <ident>` (also modulo whitespace) — so an unrelated cast
/// expression whose operand happens to be named e.g. `warn` is not mistaken
/// for one.
fn collect_macro_aliases(b: &[u8]) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !is_ident_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_ident_byte(b[i]) {
            i += 1;
        }
        let end = i;
        let Ok(word) = std::str::from_utf8(&b[start..end]) else {
            continue;
        };
        if !TRACING_MACROS.contains(&word) {
            continue;
        }
        let mut p = start;
        while p > 0 && b[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        let in_import_position =
            (p >= 2 && &b[p - 2..p] == b"::") || (p >= 1 && matches!(b[p - 1], b'{' | b','));
        if !in_import_position {
            continue;
        }
        let mut m = end;
        while m < b.len() && b[m].is_ascii_whitespace() {
            m += 1;
        }
        if !starts_with_ident(b, m, "as") {
            continue;
        }
        m += 2;
        while m < b.len() && b[m].is_ascii_whitespace() {
            m += 1;
        }
        let alias_start = m;
        while m < b.len() && is_ident_byte(b[m]) {
            m += 1;
        }
        if m > alias_start {
            if let Ok(alias) = std::str::from_utf8(&b[alias_start..m]) {
                aliases.push(alias.to_owned());
            }
        }
    }
    aliases
}

fn blank(out: &mut [u8], from: usize, to: usize) {
    for byte in &mut out[from..to] {
        if *byte != b'\n' {
            *byte = b' ';
        }
    }
}

/// What [`mask_trivia`] found while masking one file's source.
struct Masked {
    /// Source bytes with comments and literals blanked to spaces, so a scan
    /// over it sees only genuine code.
    buf: Vec<u8>,
    /// Byte ranges of genuine `//` line comments (including `///` and `//!`).
    /// [`collect_allow_marker_lines`] uses these to tell a real
    /// `// allow(tracing-target-syntax)` from that text inside a string.
    line_comments: Vec<(usize, usize)>,
    /// Byte ranges of string literals — plain, raw, byte and C strings —
    /// delimiters included. Char literals are excluded: they cannot hold a
    /// tracing target, and a lifetime (`'a`) is not a literal at all.
    ///
    /// `scan_targets` needs these because `buf` has blanked the very bytes a
    /// target string is made of. Reporting the span instead of un-blanking
    /// keeps one lexer authoritative over what counts as code — re-deriving
    /// literal boundaries in a second scanner is what the note at
    /// `collect_allow_marker_lines` warns against.
    string_literals: Vec<(usize, usize)>,
}

/// Replace every byte inside a comment or literal with a space, preserving
/// length, byte offsets and newlines so offsets still map onto the original.
///
/// Also returns the byte ranges of every genuine **line comment** (`// …`
/// through end-of-line, which includes doc comments `///`/`//!` — they open
/// with the same two bytes this scan matches on). [`collect_allow_marker_lines`]
/// reuses these ranges to tell a real `// allow(tracing-target-syntax)`
/// comment apart from that same text merely appearing inside a string, a raw
/// string, or a block comment — all of which this function also blanks, but
/// does *not* record a range for, since only a line comment is a legitimate
/// home for the marker.
fn mask_trivia(src: &str) -> Masked {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut line_comments = Vec::new();
    let mut string_literals = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start, i);
                line_comments.push((start, i));
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 1usize;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
            }
            b'r' | b'b' | b'c' => match raw_or_byte_string_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    string_literals.push((i, end));
                    i = end;
                }
                None => i += 1,
            },
            b'"' => {
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        // Clamp: a trailing backslash at EOF (unterminated
                        // string) must not push `i` past `out.len()`, or the
                        // `blank` call below panics on an out-of-range slice.
                        i = (i + 2).min(b.len());
                    } else if b[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start, i);
                string_literals.push((start, i));
            }
            b'\'' => match char_literal_end(b, i) {
                Some(end) => {
                    blank(&mut out, i, end);
                    i = end;
                }
                // A lifetime, not a literal — leave it alone.
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    Masked {
        buf: out,
        line_comments,
        string_literals,
    }
}

/// End (exclusive) of a raw or byte string starting at `i`, if one does.
fn raw_or_byte_string_end(b: &[u8], i: usize) -> Option<usize> {
    // `bar` must not read as a byte string starting at its `r`.
    if i > 0 && is_ident_byte(b[i - 1]) {
        return None;
    }
    let mut j = i;
    // `b` (byte string) and `c` (C string) may prefix either form: b"…" / br#"…"#
    // / c"…" / cr#"…"#. Without consuming the prefix, the `r` of `cr#` is read as
    // an identifier byte and the raw-string machinery never engages.
    if b[j] == b'b' || b[j] == b'c' {
        j += 1;
    }
    let raw = b.get(j) == Some(&b'r');
    if raw {
        j += 1;
        let hashes_start = j;
        while b.get(j) == Some(&b'#') {
            j += 1;
        }
        let hashes = j - hashes_start;
        if b.get(j) != Some(&b'"') {
            return None;
        }
        j += 1;
        while j < b.len() {
            if b[j] == b'"' {
                let mut k = j + 1;
                let mut seen = 0;
                while seen < hashes && b.get(k) == Some(&b'#') {
                    k += 1;
                    seen += 1;
                }
                if seen == hashes {
                    return Some(k);
                }
            }
            j += 1;
        }
        return Some(b.len());
    }
    if (b[i] != b'b' && b[i] != b'c') || b.get(j) != Some(&b'"') {
        return None;
    }
    j += 1;
    while j < b.len() {
        if b[j] == b'\\' {
            // Same clamp as the `b'"'` arm of `mask_trivia`: an escape at
            // EOF must not push `j` past `b.len()`.
            j = (j + 2).min(b.len());
        } else if b[j] == b'"' {
            return Some(j + 1);
        } else {
            j += 1;
        }
    }
    Some(b.len())
}

/// End (exclusive) of a char literal starting at `i`, or `None` for a lifetime.
fn char_literal_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1) == Some(&b'\\') {
        // Start the scan for the closing `'` *after* the escaped character
        // (`i + 3`), not at it (`i + 2`). For `'\''` the byte at `i + 2` is
        // the escaped quote itself, not the closing delimiter — starting
        // there would stop one byte early and leave the real closing `'`
        // unmasked. Clamp so a trailing escape at EOF cannot push `j` past
        // `b.len()`.
        let mut j = (i + 3).min(b.len());
        while j < b.len() && b[j] != b'\'' {
            j += 1;
        }
        return if j < b.len() { Some(j + 1) } else { None };
    }
    let mut j = i + 1;
    if j >= b.len() {
        return None;
    }
    j += 1;
    // Consume UTF-8 continuation bytes of a multi-byte char.
    while j < b.len() && b[j] & 0b1100_0000 == 0b1000_0000 {
        j += 1;
    }
    if b.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

fn starts_with_ident(b: &[u8], at: usize, word: &str) -> bool {
    let w = word.as_bytes();
    b.len() >= at + w.len()
        && &b[at..at + w.len()] == w
        && !b.get(at + w.len()).is_some_and(|&c| is_ident_byte(c))
}

fn line_of(b: &[u8], at: usize) -> usize {
    b[..at].iter().filter(|&&c| c == b'\n').count() + 1
}

/// Walk one macro invocation's top-level (depth 0) arguments and call `f`
/// with the byte offset each argument starts at (its first non-whitespace
/// byte). `open` is the index of the invocation's opening delimiter (`(`,
/// `[` or `{`) and `closer` its matching close byte, so a `{`- or
/// `[`-delimited invocation terminates on its own closer rather than on the
/// first `)` it happens to contain.
///
/// Shared by [`collect_args`] (which flags an argument opening with
/// `target =` / `parent =`) and [`classify_target_arg`] (which classifies an
/// argument opening with `target:`), so the depth-tracking, comma-splitting
/// walk that decides what counts as "top-level" lives in exactly one place —
/// the two callers would otherwise have to agree on it independently.
fn for_each_top_level_arg(
    b: &[u8],
    open: usize,
    closer: u8,
    mut f: impl FnMut(usize),
) -> Result<(), MismatchedDelimiter> {
    let mut k = open + 1;
    let mut depth = 0usize;
    let mut at_arg_start = true;
    while k < b.len() {
        let c = b[k];
        if c == b'(' || c == b'[' || c == b'{' {
            depth += 1;
            at_arg_start = false;
        } else if c == b')' || c == b']' || c == b'}' {
            if depth == 0 {
                // Well-formed Rust nests delimiters strictly (each close
                // matches the most recently opened group), so the closer
                // reached at depth 0 must be this invocation's own —
                // any interior groups, of any delimiter kind, were already
                // closed by the depth count above. A mismatch here means a
                // future `mask_trivia` desync, not valid Rust; return it
                // rather than panic so a multi-file caller can name the file.
                if c != closer {
                    return Err(MismatchedDelimiter {
                        byte: k,
                        found: c,
                        expected: closer,
                    });
                }
                return Ok(());
            }
            depth -= 1;
            at_arg_start = false;
        } else if c == b',' && depth == 0 {
            at_arg_start = true;
        } else if c.is_ascii_whitespace() {
            // Whitespace never ends an argument-start position.
        } else {
            if at_arg_start && depth == 0 {
                f(k);
            }
            at_arg_start = false;
        }
        k += 1;
    }
    Ok(())
}

/// Flag any top-level argument that opens with `target =` or `parent =`.
fn collect_args(
    b: &[u8],
    open: usize,
    closer: u8,
    macro_name: &str,
    out: &mut Vec<Offense>,
) -> Result<(), MismatchedDelimiter> {
    for_each_top_level_arg(b, open, closer, |k| {
        if let Some(kw) = KEYWORDS.iter().find(|kw| starts_with_ident(b, k, kw)) {
            let mut m = k + kw.len();
            while m < b.len() && b[m].is_ascii_whitespace() {
                m += 1;
            }
            if b.get(m) == Some(&b'=') && b.get(m + 1) != Some(&b'=') {
                out.push(Offense {
                    line: line_of(b, k),
                    macro_name: macro_name.to_owned(),
                    keyword: (*kw).to_owned(),
                });
            }
        }
    })
}

/// Classification of the `target:` argument found (or not) in one `tracing`
/// macro invocation. Deliberately three states, not two: Task 8/9 must tell
/// "no target" apart from "computed target" apart from "literal target" — a
/// computed target satisfies coverage but cannot be shape-checked, which is
/// exactly how an event could be routed out of the namespace invisibly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetArg {
    /// The invocation passes no `target:` argument at all. Note this
    /// includes a `target = "…"` argument — that is macro *field* syntax
    /// (the SMA-543 defect [`scan`] separately flags), not the `target:`
    /// keyword, so it never actually overrides the event's target.
    Absent,
    /// The invocation passes `target:` with a value that is not a string
    /// literal — e.g. a `const`, or any other expression.
    NonLiteral,
    /// The invocation passes `target: "…"` with a string-literal value; the
    /// literal's content, with its delimiters and any `r#`/`b`/`c` prefix
    /// stripped.
    Literal(String),
}

/// One recognised `tracing` macro invocation and how it targets its event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// 1-based line number where the macro's name begins.
    pub line: usize,
    /// Macro it appeared in, unqualified (e.g. `warn` for `tracing::warn!`).
    pub macro_name: String,
    /// How this invocation specifies its `target:`.
    pub target: TargetArg,
}

/// Find every `tracing` macro invocation in `src` and classify how each one
/// specifies (or fails to specify) its `target:` argument.
///
/// Recognises the same invocation shapes [`scan`]/[`try_scan`] do — final
/// path segment, any of the three delimiter kinds, `use`-aliased bare
/// forms — by sharing the same internal invocation walker, and is
/// comment/literal-blind for the same reason: it operates on the masked
/// buffer the trivia masker produces.
///
/// Unlike [`scan_targets`], this is macro-aware: a `target:` argument is
/// only classified when it is a top-level argument of a *recognised macro
/// invocation*, found by walking that invocation's own argument list — never
/// by a bare needle search over the whole file. An ordinary struct-literal
/// field named `target` (e.g. `Redirect { target: "/etc/passwd".into() }`)
/// is invisible to it, because no macro invocation surrounds that site at
/// all.
///
/// # Errors
///
/// Returns [`MismatchedDelimiter`] under the same unreachable-against-valid-Rust
/// condition [`try_scan`] documents.
pub fn scan_invocations(src: &str) -> Result<Vec<Invocation>, MismatchedDelimiter> {
    let masked = mask_trivia(src);
    let b = &masked.buf[..];
    let aliases = collect_macro_aliases(b);
    let mut invocations = Vec::new();
    walk_invocations(b, &aliases, |inv| {
        let target = classify_target_arg(b, inv.open, inv.closer, &masked.string_literals, src)?;
        invocations.push(Invocation {
            line: line_of(b, inv.name_start),
            macro_name: inv.name.to_owned(),
            target,
        });
        Ok(())
    })?;
    Ok(invocations)
}

/// Classify one invocation's `target:` argument (see [`TargetArg`]),
/// looking only at its top-level arguments via [`for_each_top_level_arg`].
/// `string_literals` and `src` are [`Masked::string_literals`] and the
/// original (unmasked) source, needed to tell a literal value from a
/// non-literal one and to read the literal's content back out.
fn classify_target_arg(
    b: &[u8],
    open: usize,
    closer: u8,
    string_literals: &[(usize, usize)],
    src: &str,
) -> Result<TargetArg, MismatchedDelimiter> {
    let mut target = TargetArg::Absent;
    for_each_top_level_arg(b, open, closer, |k| {
        if !starts_with_ident(b, k, "target") {
            return;
        }
        let mut m = k + "target".len();
        while m < b.len() && b[m].is_ascii_whitespace() {
            m += 1;
        }
        // Only the `target:` keyword form actually sets the event's target;
        // `target = ...` compiles to an ordinary field (the SMA-543 defect)
        // and never reaches here as anything but `Absent`.
        if b.get(m) != Some(&b':') {
            return;
        }
        // Advance past real whitespace *and* comments (both blank to spaces
        // in `b`) to find where the value starts, but stop the instant a
        // position matches a known string-literal start — a literal's own
        // interior is also blanked to spaces in `b`, so a plain "skip
        // whitespace in `b`" loop would run straight through it and land on
        // whatever follows instead.
        let mut v = m + 1;
        while v < b.len()
            && b[v].is_ascii_whitespace()
            && !string_literals.iter().any(|&(start, _)| start == v)
        {
            v += 1;
        }
        target = match string_literals.iter().find(|&&(start, _)| start == v) {
            Some(&(start, end)) => {
                TargetArg::Literal(literal_content(&src[start..end]).unwrap_or("").to_owned())
            }
            None => TargetArg::NonLiteral,
        };
    })?;
    Ok(target)
}

/// Lines (1-based) carrying a `#[tracing::instrument]` or bare
/// `#[instrument]` attribute.
///
/// Operates on the trivia masker's masked buffer, so an occurrence inside a
/// comment (e.g. a doc comment mentioning the attribute in prose) or a
/// string literal is invisible to it — matching
/// `crates/paigasus-helikon-macros/src/signature.rs:210`'s doc comment,
/// which names `#[tracing::instrument]` in a sentence and must not be
/// reported.
///
/// Recognised structurally, not by a bare text search: the `instrument`
/// identifier must be preceded (modulo whitespace, and modulo zero or more
/// `tracing::`/other path qualifiers immediately before it, including a
/// leading `::` root as in `#[::tracing::instrument]`) by an attribute's
/// opening `#[`. The attribute need not begin immediately there — being
/// anywhere inside the attribute's brackets counts, so
/// `#[cfg_attr(test, tracing::instrument)]` is also recognised. That keeps an
/// unrelated identifier or expression named `instrument` from being mistaken
/// for the attribute.
pub fn instrument_attribute_lines(src: &str) -> BTreeSet<usize> {
    let masked = mask_trivia(src);
    let b = &masked.buf[..];
    let mut lines = BTreeSet::new();
    let mut i = 0;
    while i < b.len() {
        if !is_ident_byte(b[i]) || (i > 0 && is_ident_byte(b[i - 1])) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && is_ident_byte(b[i]) {
            i += 1;
        }
        if &b[start..i] == b"instrument" {
            let mut p = start;
            while p > 0 && b[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            // Skip zero or more optional qualifiers immediately before
            // `instrument`, e.g. the `tracing::` in `#[tracing::instrument]`,
            // looping so a leading `::` root (`#[::tracing::instrument]`) and
            // multi-segment paths are both fully stripped rather than just
            // the innermost segment.
            while p >= 2 && &b[p - 2..p] == b"::" {
                p -= 2;
                while p > 0 && is_ident_byte(b[p - 1]) {
                    p -= 1;
                }
                while p > 0 && b[p - 1].is_ascii_whitespace() {
                    p -= 1;
                }
            }
            if inside_attribute_brackets(b, p) {
                lines.insert(line_of(b, start));
            }
        }
    }
    lines
}

/// Whether position `at` in the masked buffer `b` sits inside some
/// attribute's `#[ … ]` brackets, i.e. an unmatched `#[` is reachable by
/// walking backward from `at`.
///
/// Only `[`/`]` are tracked; `(`/`)` and `{`/`}` are transparent to the walk.
/// That is deliberate: `#[cfg_attr(test, tracing::instrument)]` nests
/// `instrument` inside `cfg_attr(...)`'s parens, one level below the
/// attribute's own brackets, and those parens must not stop the backward
/// search from reaching the enclosing `#[`.
fn inside_attribute_brackets(b: &[u8], at: usize) -> bool {
    let mut depth: i32 = 0;
    let mut k = at;
    while k > 0 {
        k -= 1;
        match b[k] {
            b']' => depth += 1,
            b'[' => {
                if depth == 0 {
                    return k > 0 && b[k - 1] == b'#';
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(usize, String, String)> {
        scan(src)
            .into_iter()
            .map(|o| (o.line, o.macro_name, o.keyword))
            .collect()
    }

    #[test]
    fn scan_invocations_classifies_a_literal_target() {
        let src = r#"fn f() { tracing::warn!(target: "paigasus::core::agent", "m"); }"#;
        let got = scan_invocations(src).expect("well-formed source");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].macro_name, "warn");
        assert_eq!(
            got[0].target,
            TargetArg::Literal("paigasus::core::agent".to_owned())
        );
    }

    #[test]
    fn scan_invocations_reports_an_untargeted_site() {
        let src = r#"fn f() { tracing::warn!("m"); }"#;
        let got = scan_invocations(src).expect("well-formed source");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, TargetArg::Absent);
    }

    // Regression: `crates/paigasus-helikon-core/src/command_match.rs:436` contains
    // `Redirect { target: "/etc/passwd".into() }` inside a `#[cfg(test)]` module.
    // A needle-based scanner reads that as a tracing target and fails the shape
    // assertion on a file that emits nothing at all.
    #[test]
    fn scan_invocations_ignores_a_struct_field_named_target() {
        let src = r#"fn f() { let r = Redirect { op: Op::Write, target: "/etc/passwd".into() }; }"#;
        assert_eq!(scan_invocations(src).expect("well-formed source"), vec![]);
    }

    #[test]
    fn scan_invocations_classifies_a_non_literal_target() {
        let src = r#"fn f() { tracing::warn!(target: T_CORE, "m"); }"#;
        let got = scan_invocations(src).expect("well-formed source");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, TargetArg::NonLiteral);
    }

    #[test]
    fn scan_invocations_sees_span_macros_and_bare_forms() {
        let src = r#"
            use tracing::warn;
            fn f() {
                let _ = tracing::info_span!(target: "paigasus::core::agent", parent: p, "agent.run");
                warn!("bare");
            }
        "#;
        let got = scan_invocations(src).expect("well-formed source");
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].target,
            TargetArg::Literal("paigasus::core::agent".to_owned())
        );
        assert_eq!(got[1].target, TargetArg::Absent);
    }

    #[test]
    fn scan_invocations_is_blind_to_comments_and_string_literals() {
        let src = r#"
            // tracing::warn!("commented out");
            fn f() { let s = "tracing::warn!(\"in a string\")"; }
        "#;
        assert_eq!(scan_invocations(src).expect("well-formed source"), vec![]);
    }

    #[test]
    fn allow_marker_lines_finds_both_positions_and_ignores_string_literals() {
        let src = r#"
            // allow(tracing-target-coverage)
            fn a() { tracing::warn!("m"); }
            fn b() { tracing::warn!("m"); } // allow(tracing-target-coverage)
            fn c() { let s = "// allow(tracing-target-coverage)"; }
        "#;
        let lines = allow_marker_lines(src, ALLOW_MARKER_COVERAGE);
        assert_eq!(
            lines.len(),
            2,
            "the marker inside a string literal must not count"
        );
    }

    /// A real `#[tracing::instrument]` attribute is reported on its own line.
    #[test]
    fn instrument_attribute_lines_finds_the_qualified_form() {
        let src = "#[tracing::instrument]\nfn a() {}\n";
        assert_eq!(instrument_attribute_lines(src), [1].into_iter().collect());
    }

    /// The bare `#[instrument]` form (reachable via `use tracing::instrument;`)
    /// is also reported.
    #[test]
    fn instrument_attribute_lines_finds_the_bare_form() {
        let src = "#[instrument]\nfn a() {}\n";
        assert_eq!(instrument_attribute_lines(src), [1].into_iter().collect());
    }

    /// An occurrence inside a `//` comment must not be reported — this is the
    /// exact shape of `paigasus-helikon-macros/src/signature.rs:210`'s doc
    /// comment, which names the attribute in prose.
    #[test]
    fn instrument_attribute_lines_ignores_a_line_comment() {
        let src = "// mentions `#[tracing::instrument]` in prose\nfn a() {}\n";
        assert!(instrument_attribute_lines(src).is_empty());
    }

    /// An occurrence inside a string literal must not be reported.
    #[test]
    fn instrument_attribute_lines_ignores_a_string_literal() {
        let src = "fn a() { let s = \"#[tracing::instrument]\"; }";
        assert!(instrument_attribute_lines(src).is_empty());
    }

    /// A leading `::` root (`#[::tracing::instrument]`, an idiomatic way to
    /// avoid path shadowing) must still be recognised: the qualifier-strip
    /// has to loop rather than stop after one segment.
    #[test]
    fn instrument_attribute_lines_finds_the_leading_root_qualified_form() {
        let src = "#[::tracing::instrument]\nfn a() {}\n";
        assert_eq!(instrument_attribute_lines(src), [1].into_iter().collect());
    }

    /// `#[cfg_attr(test, tracing::instrument)]` places `instrument` inside
    /// the attribute's brackets but not immediately after `#[` — the
    /// attribute-interior check must still recognise it.
    #[test]
    fn instrument_attribute_lines_finds_the_cfg_attr_form() {
        let src = "#[cfg_attr(test, tracing::instrument)]\nfn a() {}\n";
        assert_eq!(instrument_attribute_lines(src), [1].into_iter().collect());
    }

    /// Both bypasses combined: a `cfg_attr` wrapping a leading-`::`-qualified
    /// `instrument`.
    #[test]
    fn instrument_attribute_lines_finds_the_cfg_attr_leading_root_form() {
        let src = "#[cfg_attr(feature = \"x\", ::tracing::instrument)]\nfn a() {}\n";
        assert_eq!(instrument_attribute_lines(src), [1].into_iter().collect());
    }

    /// Every form that actually compiles against `tracing` 0.1 and silently
    /// records a field instead of setting the metadata target. Verified with
    /// rustc; see spec §4.4 for the compile matrix.
    #[test]
    fn flags_every_compiling_bad_form() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"tracing::warn!(target = "x", "m");"#, "warn", "target"),
            (r#"warn!(target="x", "m");"#, "warn", "target"),
            (
                r#"tracing::warn!(parent: None, target = "x", "m");"#,
                "warn",
                "target",
            ),
            (
                r#"tracing::info_span!("nm", target = "x");"#,
                "info_span",
                "target",
            ),
            (
                r#"tracing::event!(Level::WARN, target = "x", "m");"#,
                "event",
                "target",
            ),
            (r#"tracing::info!(parent = p, "m");"#, "info", "parent"),
            // Whitespace between the macro path and `!` (`ident_range_before`
            // must skip back over it, not just abut it — SMA-543 fix wave).
            (r#"tracing::warn ! (target = "x", "m");"#, "warn", "target"),
            // A correctly-masked `'\''` char literal ahead of the macro call
            // (regression case for the `char_literal_end` off-by-one that
            // used to leave the real closing quote unmasked).
            (
                r#"let d = '\''; tracing::warn!(target = "x", "m");"#,
                "warn",
                "target",
            ),
        ];
        for (src, mac, kw) in cases {
            let got = kinds(src);
            assert_eq!(
                got,
                vec![(1, (*mac).to_owned(), (*kw).to_owned())],
                "expected one offense for `{src}`"
            );
        }
    }

    /// Macros accept all three delimiters with identical token trees, and
    /// all three reproduce the exact SMA-543 defect: `tracing` 0.1 +
    /// `tracing-subscriber` 0.3 both emit these on the module path, not on
    /// `"x"`. Each must be recognised as an invocation, not just `(`.
    #[test]
    fn flags_brace_and_bracket_delimited_invocations() {
        let cases: &[(&str, &str, &str)] = &[
            (r#"tracing::warn!(target = "x", "m");"#, "warn", "target"),
            (r#"tracing::warn![target = "x", "m"];"#, "warn", "target"),
            (r#"tracing::warn!{ target = "x", "m" }"#, "warn", "target"),
        ];
        for (src, mac, kw) in cases {
            let got = kinds(src);
            assert_eq!(
                got,
                vec![(1, (*mac).to_owned(), (*kw).to_owned())],
                "expected exactly one offense for `{src}`"
            );
        }
    }

    /// A non-`(` delimiter with the correct `target:` syntax must stay clean.
    #[test]
    fn accepts_brace_delimited_correct_form() {
        assert_eq!(kinds(r#"tracing::warn!{ target: "x", "m" }"#), vec![]);
    }

    /// A `(` nested inside a `{`-delimited invocation must still increment
    /// depth as before, so the top-level `target = ` is found and the walk
    /// does not mistake the nested `)` for the invocation's own closer.
    #[test]
    fn brace_invocation_tracks_nested_paren_depth() {
        assert_eq!(
            kinds(r#"tracing::warn!{ target = "x", other = compute(a, b), "m" }"#),
            vec![(1, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// Matching is on the macro's final path segment alone, regardless of
    /// qualifier — a qualifier can be arbitrarily renamed
    /// (`use tracing as t; t::warn!`) or reached through an unrelated
    /// module path (`crate::obs::warn!`, `self::warn!`) and still land on
    /// the exact same macro, so requiring the qualifier to read literally
    /// `tracing` produced false negatives (second review wave; spec
    /// §4.3/§4.7).
    #[test]
    fn matches_by_final_segment_regardless_of_qualifier() {
        let cases: &[(&str, usize)] = &[
            (r#"tracing::warn!(target = "x", "m");"#, 1),
            (r#"warn!(target = "x", "m");"#, 1),
            ("use tracing as t;\nt::warn!(target = \"x\", \"m\");\n", 2),
            (r#"crate::obs::warn!(target = "x", "m");"#, 1),
            (r#"self::warn!(target = "x", "m");"#, 1),
        ];
        for (src, line) in cases {
            assert_eq!(
                kinds(src),
                vec![(*line, "warn".to_owned(), "target".to_owned())],
                "expected exactly one offense for `{src}`"
            );
        }
    }

    /// A bare alias introduced by `use tracing::warn as w;` still reaches
    /// `tracing::warn!` at the token level, so it must resolve the same way.
    #[test]
    fn matches_a_use_alias_of_a_tracing_macro() {
        let src = "use tracing::warn as w;\nw!(target = \"x\", \"m\");\n";
        assert_eq!(kinds(src), vec![(2, "w".to_owned(), "target".to_owned())]);
    }

    /// Accepted cost (spec §4.7) of matching by final segment alone: an
    /// unrelated macro whose final path segment merely collides with a
    /// tracing macro name is now flagged too — the previous
    /// tracing-qualifier restriction let this case pass silently, with no
    /// way to opt back in short of editing the lint crate itself.
    #[test]
    fn foreign_macro_colliding_with_a_tracing_name_is_now_flagged() {
        assert_eq!(
            kinds(r#"mycrate::warn!(target = "x", "m");"#),
            vec![(1, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// The `// allow(tracing-target-syntax)` marker on the line immediately
    /// before an invocation suppresses it — the escape hatch for a
    /// legitimate `target =` / `parent =` *field* (as opposed to macro
    /// syntax), which otherwise has no remedy short of editing this crate.
    #[test]
    fn allow_marker_on_the_preceding_line_suppresses_the_offense() {
        let src = "// allow(tracing-target-syntax)\ntracing::warn!(target = \"x\", \"m\");\n";
        assert_eq!(kinds(src), vec![]);
    }

    /// The marker also works trailing the invocation's own line.
    #[test]
    fn allow_marker_trailing_the_invocation_line_suppresses_the_offense() {
        let src = r#"tracing::warn!(target = "x", "m"); // allow(tracing-target-syntax)"#;
        assert_eq!(kinds(src), vec![]);
    }

    /// A marker suppressing one invocation must not suppress a different,
    /// unmarked invocation elsewhere in the same file.
    #[test]
    fn allow_marker_does_not_suppress_a_different_site() {
        let src = "// allow(tracing-target-syntax)\ntracing::warn!(target = \"x\", \"m\");\ntracing::error!(target = \"y\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(3, "error".to_owned(), "target".to_owned())]
        );
    }

    /// The marker is the remedy for the accepted cost demonstrated by
    /// `foreign_macro_colliding_with_a_tracing_name_is_now_flagged`: a
    /// colliding foreign macro can be silenced explicitly.
    #[test]
    fn allow_marker_silences_a_colliding_foreign_macro() {
        let src = r#"mycrate::warn!(target = "x", "m"); // allow(tracing-target-syntax)"#;
        assert_eq!(kinds(src), vec![]);
    }

    /// Regression test (third review wave): the marker's *text* appearing
    /// inside a string literal on the same line as a real offense must not
    /// suppress it. The previous implementation matched the marker as raw
    /// text anywhere on the line, with no regard for whether that text was
    /// actually inside a comment.
    #[test]
    fn allow_marker_text_inside_a_string_literal_does_not_suppress() {
        let src =
            "let s = \"// allow(tracing-target-syntax)\"; tracing::warn!(target = \"x\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(1, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// Same false negative as above, but the marker text sits inside a block
    /// comment rather than a genuine line comment. Block comments are
    /// blanked by `mask_trivia` but deliberately not recorded as a home for
    /// the marker — only `// …` line comments count.
    #[test]
    fn allow_marker_text_inside_a_block_comment_does_not_suppress() {
        let src = "/* // allow(tracing-target-syntax) */ tracing::warn!(target = \"x\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(1, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// Same false negative again, inside a raw string.
    #[test]
    fn allow_marker_text_inside_a_raw_string_does_not_suppress() {
        let src =
            "let s = r#\"// allow(tracing-target-syntax)\"#; tracing::warn!(target = \"x\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(1, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// Regression test for the FIX B panic-message fix (second review
    /// wave): a delimiter mismatch — unreachable against valid Rust, but
    /// exercised here to prove the code path — is a recoverable error
    /// rather than a `debug_assert_eq!` panic that names only a byte
    /// offset. This source could never compile, but the scanner tracks
    /// delimiter depth independently of syntax validity, so it still
    /// reaches the same branch a future `mask_trivia` desync would hit
    /// against real source.
    #[test]
    fn mismatched_delimiter_is_a_diagnosable_error_not_a_panic() {
        let src = "tracing::warn!(target = \"x\", \"m\"];\n";
        let err = try_scan(src).expect_err("expected a mismatched-delimiter error");
        assert_eq!(err.found, b']');
        assert_eq!(err.expected, b')');
    }

    /// The real shape of the SMA-543 bug: the keyword is on its own line.
    #[test]
    fn flags_the_multiline_shape_and_reports_the_keyword_line() {
        let src = "fn f() {\n    tracing::warn!(\n        target = \"paigasus::openai::translate\",\n        \"unknown Item variant; skipping\"\n    );\n}\n";
        assert_eq!(
            kinds(src),
            vec![(3, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// `cargo test --workspace --all-features` also runs on windows-latest.
    #[test]
    fn line_numbers_survive_crlf() {
        let src = "fn f() {\r\n    tracing::warn!(\r\n        target = \"x\",\r\n        \"m\"\r\n    );\r\n}\r\n";
        assert_eq!(
            kinds(src),
            vec![(3, "warn".to_owned(), "target".to_owned())]
        );
    }

    #[test]
    fn accepts_correct_and_unrelated_forms() {
        let cases = [
            r#"tracing::warn!(target: "x", "m");"#,
            r#"tracing::info_span!(parent: parent, "nm");"#,
            r#"tracing::info!(count = 1, "m");"#,
            r#"let target = "x";"#,
            r#"if a == b { let parent = 1; }"#,
            r#"foo!(target = "x");"#,
            r#"Thing { target: "x" }"#,
            r#"fn f<'a>(x: &'a str) { tracing::warn!(target: "t", "m"); }"#,
            r#"tracing::warn!(target: "t", other = compute(a, b), "m");"#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    /// C-string prefixes take a different path through the masker than plain
    /// raw strings: without consuming the `c`, the `r` of `cr#` reads as an
    /// identifier byte and the raw-string machinery never engages, so the body
    /// gets scanned as code. Even-numbered inner quotes hid this by accident.
    #[test]
    fn masks_c_string_prefixes() {
        let cases = [
            r#"let s = c"tracing::warn!(target = 1)";"#,
            "let s = cr#\"tracing::warn!(target = 1)\"#;",
            // Odd number of unescaped inner quotes — the case that regressed.
            "let s = cr#\"a \"b tracing::warn!(target = 1) \"#;",
            "let s = c\"a \\\" tracing::warn!(target = 1)\";",
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    /// A masking bug must not let a *real* site after the literal go missing.
    #[test]
    fn c_string_does_not_hide_a_later_real_site() {
        let src = "let s = cr#\"x \"y tracing::warn!(target = 1)\"#;\ntracing::warn!(target = \"real\", \"m\");";
        assert_eq!(
            kinds(src),
            vec![(2, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// The guard scans its own source. Because the lexer blanks comments and
    /// literals, the bad forms in this very file are invisible to it — which
    /// is why there is deliberately no path-based self-exclusion (spec §4.5).
    #[test]
    fn ignores_comments_and_literals() {
        let cases = [
            r#"// tracing::warn!(target = "x", "m");"#,
            r#"/// tracing::warn!(target = "x", "m");"#,
            r#"/* tracing::warn!(target = "x", "m"); */"#,
            r#"/* outer /* nested */ tracing::warn!(target = "x"); */"#,
            r#"let s = "tracing::warn!(target = \"x\")";"#,
            "let s = r#\"tracing::warn!(target = \"x\")\"#;",
            r#"let s = b"warn!(target = 1)";"#,
        ];
        for src in cases {
            assert_eq!(kinds(src), vec![], "false positive on `{src}`");
        }
    }

    #[test]
    fn reports_every_offense_in_a_file() {
        let src = "tracing::warn!(target = \"a\", \"m\");\ntracing::warn!(target: \"ok\", \"m\");\ntracing::error!(target = \"b\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![
                (1, "warn".to_owned(), "target".to_owned()),
                (3, "error".to_owned(), "target".to_owned()),
            ]
        );
    }

    /// A string containing an unbalanced paren must not desynchronise the
    /// argument walk.
    #[test]
    fn unbalanced_paren_inside_a_literal_does_not_desync() {
        let src =
            "tracing::warn!(target: \"a)b\", \"m\");\ntracing::warn!(target = \"c\", \"m\");\n";
        assert_eq!(
            kinds(src),
            vec![(2, "warn".to_owned(), "target".to_owned())]
        );
    }

    /// A file ending mid-escape (an unterminated string literal) must not
    /// panic `mask_trivia`'s `blank` call with an out-of-range slice.
    #[test]
    fn unterminated_string_literal_does_not_panic() {
        let src = "let s = \"abc\\";
        assert_eq!(kinds(src), vec![]);
    }

    /// `mask_trivia` must report the byte span of every string literal, so a
    /// later scan can read the literal's *contents* out of the original source
    /// (the masked buffer has blanked them). Char literals are deliberately
    /// excluded — they can never hold a tracing target.
    #[test]
    fn mask_trivia_reports_string_literal_spans() {
        let src = "let a = \"one\"; let b = 'x'; let c = r#\"two\"#;";
        let masked = mask_trivia(src);
        let texts: Vec<&str> = masked
            .string_literals
            .iter()
            .map(|&(s, e)| &src[s..e])
            .collect();
        assert_eq!(texts, vec!["\"one\"", "r#\"two\"#"]);
    }

    /// The existing line-comment reporting must survive the signature change.
    #[test]
    fn mask_trivia_still_reports_line_comments() {
        let src = "// note\nlet a = 1;\n";
        let masked = mask_trivia(src);
        assert_eq!(masked.line_comments.len(), 1);
        let (s, e) = masked.line_comments[0];
        assert_eq!(&src[s..e], "// note");
    }

    /// The ordinary case: a component is taken from between the `paigasus::`
    /// prefix and the next `::`.
    #[test]
    fn scan_targets_extracts_components() {
        let src = concat!(
            "tracing::debug!(target: \"paigasus::openai::chat\", \"m\");\n",
            "tracing::warn!(target: \"paigasus::litellm::stream\", \"m\");\n",
            "tracing::warn!(target: \"paigasus::openai::responses\", \"m\");\n",
        );
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["litellm".to_owned(), "openai".to_owned()]);
    }

    /// A macro spanning several lines is the dominant real-world shape.
    #[test]
    fn scan_targets_handles_multiline_macros() {
        let src = "tracing::warn!(\n    target: \"paigasus::bedrock::translate\",\n    \"m\"\n);\n";
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["bedrock".to_owned()]);
    }

    /// A literal with no second `::` still yields a component. This shape does
    /// not occur in the workspace today; it must not panic.
    #[test]
    fn scan_targets_accepts_a_bare_component() {
        let src = "tracing::warn!(target: \"paigasus::gemini\", \"m\");\n";
        let got: Vec<String> = scan_targets(src).into_iter().collect();
        assert_eq!(got, vec!["gemini".to_owned()]);
    }

    /// Targets outside the namespace are not components.
    #[test]
    fn scan_targets_ignores_foreign_targets() {
        let src = "tracing::warn!(target: \"hyper::client\", \"m\");\n";
        assert!(scan_targets(src).is_empty());
    }

    /// `target =` is the SMA-543 defect: it records an ordinary field and the
    /// event never lands on that target, so it is not a target site at all.
    #[test]
    fn scan_targets_ignores_the_equals_form() {
        let src = "tracing::warn!(target = \"paigasus::openai::chat\", \"m\");\n";
        assert!(scan_targets(src).is_empty());
    }

    /// Comments are not code. This is not hypothetical: a `///` doc comment at
    /// `crates/paigasus-helikon-providers-litellm/src/translate/request.rs:497`
    /// made the spec's first inventory count 57 sites where there are 56.
    #[test]
    fn scan_targets_ignores_comments() {
        for src in [
            "// tracing::warn!(target: \"paigasus::ghost::x\", \"m\");\n",
            "/// reinstates `target: \"paigasus::ghost::x\"` inside this\n",
            "/* tracing::warn!(target: \"paigasus::ghost::x\"); */\n",
        ] {
            assert!(scan_targets(src).is_empty(), "leaked from: {src}");
        }
    }

    /// `tracing` accepts a comment between `target:` and its literal. Such a
    /// site must still be found — otherwise a new component written this way
    /// would silently bypass the documentation drift guard.
    #[test]
    fn scan_targets_sees_through_a_comment_before_the_literal() {
        for src in [
            "tracing::warn!(target: /* note */ \"paigasus::openai::chat\", \"m\");\n",
            "tracing::warn!(\n    target: // note\n    \"paigasus::openai::chat\",\n    \"m\"\n);\n",
        ] {
            let got: Vec<String> = scan_targets(src).into_iter().collect();
            assert_eq!(got, vec!["openai".to_owned()], "missed site in: {src}");
        }
    }

    /// The gap test must not be so permissive that a non-literal target matches
    /// a later literal in the same invocation. `target: SOME_CONST` yields
    /// nothing rather than picking up the message string.
    #[test]
    fn scan_targets_ignores_a_non_literal_target() {
        let src = "tracing::warn!(target: SOME_CONST, \"paigasus::openai::chat\");\n";
        assert!(scan_targets(src).is_empty());
    }

    /// A target inside an outer string literal is a test fixture, not a call
    /// site. This property is what lets the guard scan its own source without
    /// path-based self-exclusion.
    #[test]
    fn scan_targets_ignores_nested_literals() {
        let src = "let fixture = \"tracing::warn!(target: \\\"paigasus::ghost::x\\\")\";\n";
        assert!(scan_targets(src).is_empty());
    }
}
