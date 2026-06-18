//! Lexical path matching for permission path-rules (SMA-415).
//!
//! Core has no filesystem root (the cap-std root lives in
//! `paigasus-helikon-tools`), so all matching here is **lexical** on the tool's
//! `path` argument and therefore *advisory* — a convenience filter, not a
//! containment boundary. Patterns follow a small gitignore-style subset:
//! a pattern without a `/` matches at any depth; a pattern with a `/` is
//! anchored to the path root. Matching is case-insensitive.

use std::sync::Arc;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Lexically clean a candidate path: normalize `\` to `/` (so Windows-style
/// separators can't smuggle a `.git`/`.ssh`/`.env` component past matching),
/// strip a leading `./`, drop `.` components, and collapse `..` without touching
/// the filesystem. A leading `..` that escapes the root survives (so it will not
/// match an anchored pattern).
pub(crate) fn clean_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let trimmed = normalized.strip_prefix("./").unwrap_or(&normalized);
    let mut out: Vec<&str> = Vec::new();
    for comp in trimmed.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|c| *c != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// `true` if the cleaned path writes into a protected VCS/secret location:
/// any `.git` or `.ssh` path component, or a final component equal to `.env`
/// or beginning `.env.` (e.g. `.env.local`). Component equality — never a
/// substring — so `name.git/`, `.gitignore`, and `environment.env` do not trip.
/// Comparison is case-insensitive (so `.SSH/` / `.ENV` cannot bypass the
/// breaker on case-insensitive filesystems), matching [`PathGlob`].
pub(crate) fn is_protected_dotpath(path: &str) -> bool {
    let cleaned = clean_path(path);
    let comps: Vec<&str> = cleaned.split('/').filter(|c| !c.is_empty()).collect();
    if comps
        .iter()
        .any(|c| c.eq_ignore_ascii_case(".git") || c.eq_ignore_ascii_case(".ssh"))
    {
        return true;
    }
    match comps.last() {
        Some(last) => {
            let lower = last.to_ascii_lowercase();
            lower == ".env" || lower.starts_with(".env.")
        }
        None => false,
    }
}

/// A compiled, case-insensitive path-glob. Cheap to clone (the matcher is
/// behind `Arc`). Equality/Debug use the normalized source pattern only — the
/// compiled `GlobSet` is opaque — so `DenyRule`/`AllowRule` keep derive-style
/// `PartialEq`/`Eq`/`Debug`.
#[derive(Clone)]
pub(crate) struct PathGlob {
    pattern: String,
    set: Arc<GlobSet>,
}

impl PathGlob {
    /// Compile `pattern` (normalized by trimming a leading `./` or `/`). A
    /// degenerate pattern that normalizes to empty (e.g. `"/"`) compiles to a
    /// matcher that never matches.
    pub(crate) fn new(pattern: impl Into<String>) -> Self {
        let pattern = normalize_pattern(pattern.into());
        let set = Arc::new(build_globset(&pattern));
        Self { pattern, set }
    }

    /// `true` if `path` (lexically cleaned) matches this glob.
    pub(crate) fn matches_path(&self, path: &str) -> bool {
        self.set.is_match(clean_path(path))
    }
}

impl PartialEq for PathGlob {
    fn eq(&self, other: &Self) -> bool {
        self.pattern == other.pattern
    }
}
impl Eq for PathGlob {}
impl std::fmt::Debug for PathGlob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathGlob")
            .field("pattern", &self.pattern)
            .finish()
    }
}

/// Trim a leading `./` then a single leading `/` (gitignore anchor — we anchor
/// instead by the presence of an interior `/`).
fn normalize_pattern(pat: String) -> String {
    let pat = pat.replace('\\', "/");
    let p = pat.strip_prefix("./").unwrap_or(&pat);
    let p = p.strip_prefix('/').unwrap_or(p);
    p.to_owned()
}

/// Build a case-insensitive `GlobSet`. A pattern with no `/` is unanchored
/// (matches at any depth) → `{pat, **/pat}`; a pattern with a `/` is anchored.
///
/// An invalid glob is a programmer error (call-site literals), so it fails fast
/// via `debug_assert!` in debug/test builds; in release it is logged and the
/// rule degrades to a non-matching matcher rather than panicking a live agent.
fn build_globset(pattern: &str) -> GlobSet {
    // A bare `**` already matches at any depth, so don't form a redundant
    // (and potentially rejected) `**/**`.
    let globs: Vec<String> = if pattern.contains('/') || pattern == "**" {
        vec![pattern.to_owned()]
    } else {
        vec![pattern.to_owned(), format!("**/{pattern}")]
    };
    let mut builder = GlobSetBuilder::new();
    for g in &globs {
        match GlobBuilder::new(g)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
        {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(e) => {
                debug_assert!(false, "invalid path-rule glob `{g}`: {e}");
                tracing::warn!(glob = %g, error = %e, "invalid path-rule glob; this rule will not match");
            }
        }
    }
    builder.build().unwrap_or_else(|e| {
        debug_assert!(false, "path-rule globset build failed: {e}");
        tracing::warn!(error = %e, "path-rule globset build failed; this rule will not match");
        GlobSet::empty()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_path_strips_dot_and_collapses_dotdot() {
        assert_eq!(clean_path("./a/b"), "a/b");
        assert_eq!(clean_path("a/../b"), "b");
        assert_eq!(clean_path("src/../.git/config"), ".git/config");
        assert_eq!(clean_path("../escape"), "../escape"); // leading .. survives
        assert_eq!(clean_path("a/../../b"), "../b"); // escape via subdir survives
        assert_eq!(clean_path("a\\b\\c"), "a/b/c"); // backslashes normalized to /
        assert_eq!(clean_path(".\\src\\..\\.git"), ".git"); // mixed Windows-style
    }

    #[test]
    fn windows_backslash_paths_cannot_bypass_matching() {
        // A `.git\config` write must still trip the breaker and a glob.
        assert!(is_protected_dotpath(".git\\config"));
        assert!(is_protected_dotpath("a\\.ssh\\id_rsa"));
        assert!(is_protected_dotpath("cfg\\.env"));
        assert!(PathGlob::new(".env").matches_path("cfg\\.env"));
        assert!(PathGlob::new("src/**").matches_path("src\\main.rs"));
    }

    #[test]
    fn unanchored_pattern_matches_any_depth() {
        let g = PathGlob::new(".env");
        assert!(g.matches_path(".env"));
        assert!(g.matches_path("a/b/.env"));
        assert!(g.matches_path("./.env"));
        assert!(!g.matches_path(".envrc"));
    }

    #[test]
    fn extension_pattern_matches_any_depth() {
        let g = PathGlob::new("*.pem");
        assert!(g.matches_path("key.pem"));
        assert!(g.matches_path("secrets/key.pem"));
        assert!(!g.matches_path("key.pub"));
    }

    #[test]
    fn anchored_pattern_scopes_to_root() {
        let g = PathGlob::new("src/**");
        assert!(g.matches_path("src/main.rs"));
        assert!(g.matches_path("src/a/b.rs"));
        assert!(!g.matches_path("tests/main.rs"));
        // `..` cannot escape the anchored prefix once collapsed.
        assert!(!g.matches_path("src/../.git/config"));
    }

    #[test]
    fn leading_slash_anchor_is_stripped() {
        let g = PathGlob::new("/src/**");
        assert!(g.matches_path("src/main.rs"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let g = PathGlob::new(".env");
        assert!(g.matches_path(".ENV"));
        assert!(g.matches_path(".Env"));
    }

    #[test]
    fn path_glob_eq_is_normalized() {
        assert_eq!(PathGlob::new(".env"), PathGlob::new("./.env"));
    }

    #[test]
    fn protected_dotpath_trips_on_component_only() {
        // trips
        assert!(is_protected_dotpath(".git/config"));
        assert!(is_protected_dotpath("a/.ssh/id_rsa"));
        assert!(is_protected_dotpath(".env"));
        assert!(is_protected_dotpath("src/.env.local"));
        // case-insensitive (cannot bypass on case-insensitive filesystems)
        assert!(is_protected_dotpath(".SSH/id_rsa"));
        assert!(is_protected_dotpath(".Git/config"));
        assert!(is_protected_dotpath("config/.ENV"));
        // `..` that collapses INTO a protected component still trips
        assert!(is_protected_dotpath("src/../.git/config"));
        // does NOT trip
        assert!(!is_protected_dotpath("name.git/config")); // bare repo
        assert!(!is_protected_dotpath(".gitignore"));
        assert!(!is_protected_dotpath("environment.env"));
        assert!(!is_protected_dotpath("src/main.rs"));
    }
}
