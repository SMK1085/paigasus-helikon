//! Check the provenance header of a wire-format fixture.
//!
//! The rule is in `CONTRIBUTING.md`, section "Wire-format fixtures". The
//! first line of a fixture is `: Provenance: CAPTURED ...` or
//! `: Provenance: HAND-AUTHORED ...`. Each other header line is a comment
//! or is empty. The header ends at the first `event:` or `data:` line.
//! See SMA-845.

/// The reason a fixture header fails the check.
#[derive(Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// The first line does not start with `: Provenance: `.
    MissingProvenanceLine,
    /// The text after `: Provenance: ` does not start with `CAPTURED` or `HAND-AUTHORED`.
    UnknownLabel,
    /// A header line does not start with `:` and is not empty.
    NonCommentHeaderLine {
        /// 1-based number of the offending line.
        line: usize,
    },
}

/// True when `text` starts with `word` and the next character ends the word.
///
/// The next character is a space, `.`, `,` or `:`, or the text ends there.
fn has_label(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '.', ',', ':']))
}

/// Check the header of the fixture `text`.
///
/// Only the lines before the first `event:` or `data:` line are checked.
pub fn check(text: &str) -> Result<(), HeaderError> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let Some(label) = first.strip_prefix(": Provenance: ") else {
        return Err(HeaderError::MissingProvenanceLine);
    };
    if !(has_label(label, "CAPTURED") || has_label(label, "HAND-AUTHORED")) {
        return Err(HeaderError::UnknownLabel);
    }
    for (index, line) in lines.enumerate() {
        if line.starts_with("event:") || line.starts_with("data:") {
            break;
        }
        if !(line.is_empty() || line.starts_with(':')) {
            return Err(HeaderError::NonCommentHeaderLine { line: index + 2 });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_header_passes() {
        assert_eq!(
            check(": Provenance: CAPTURED x\n: more\n\ndata: {}\n"),
            Ok(())
        );
    }

    #[test]
    fn hand_authored_header_passes() {
        assert_eq!(
            check(": Provenance: HAND-AUTHORED x\nevent: a\ndata: {}\n"),
            Ok(())
        );
    }

    #[test]
    fn missing_header_fails() {
        assert_eq!(check("data: {}\n"), Err(HeaderError::MissingProvenanceLine));
    }

    #[test]
    fn lowercase_label_fails() {
        assert_eq!(
            check(": Provenance: captured 2026\ndata: {}\n"),
            Err(HeaderError::UnknownLabel)
        );
    }

    #[test]
    fn label_needs_a_word_boundary() {
        assert_eq!(
            check(": Provenance: CAPTUREDX\ndata: {}\n"),
            Err(HeaderError::UnknownLabel)
        );
    }

    #[test]
    fn line_without_colon_fails() {
        assert_eq!(
            check(": Provenance: CAPTURED x\nProvenance continues\ndata: {}\n"),
            Err(HeaderError::NonCommentHeaderLine { line: 2 })
        );
    }

    #[test]
    fn only_lines_before_first_frame_are_checked() {
        assert_eq!(
            check(": Provenance: CAPTURED x\nevent: a\n# --- turn 2 ---\nevent: b\n"),
            Ok(())
        );
    }
}
