//! Lexical path matching for permission path-rules (SMA-415).
//!
//! Core has no filesystem root (the cap-std root lives in
//! `paigasus-helikon-tools`), so all matching here is **lexical** on the tool's
//! `path` argument and therefore *advisory* — a convenience filter, not a
//! containment boundary. Patterns follow a small gitignore-style subset:
//! a pattern without a `/` matches at any depth; a pattern with a `/` is
//! anchored to the path root. Matching is case-insensitive.

// Items are pub(crate) and will be consumed by permission.rs in a later task.
#![allow(dead_code)]

use std::sync::Arc;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Lexically clean a candidate path: strip a leading `./`, drop `.` components,
/// and collapse `..` without touching the filesystem. A leading `..` that
/// escapes the root survives (so it will not match an anchored pattern).
pub(crate) fn clean_path(path: &str) -> String {
    let trimmed = path.strip_prefix("./").unwrap_or(path);
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
pub(crate) fn is_protected_dotpath(path: &str) -> bool {
    let cleaned = clean_path(path);
    let comps: Vec<&str> = cleaned.split('/').filter(|c| !c.is_empty()).collect();
    if comps.iter().any(|c| *c == ".git" || *c == ".ssh") {
        return true;
    }
    matches!(comps.last(), Some(last) if *last == ".env" || last.starts_with(".env."))
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
    /// Compile `pattern` (normalized by trimming a leading `./` or `/`).
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
    let p = pat.strip_prefix("./").unwrap_or(&pat);
    let p = p.strip_prefix('/').unwrap_or(p);
    p.to_owned()
}

/// Build a case-insensitive `GlobSet`. A pattern with no `/` is unanchored
/// (matches at any depth) → `{pat, **/pat}`; a pattern with a `/` is anchored.
fn build_globset(pattern: &str) -> GlobSet {
    let globs: Vec<String> = if pattern.contains('/') {
        vec![pattern.to_owned()]
    } else {
        vec![pattern.to_owned(), format!("**/{pattern}")]
    };
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = GlobBuilder::new(&g)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
        {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
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
        // does NOT trip
        assert!(!is_protected_dotpath("name.git/config")); // bare repo
        assert!(!is_protected_dotpath(".gitignore"));
        assert!(!is_protected_dotpath("environment.env"));
        assert!(!is_protected_dotpath("src/main.rs"));
    }
}
