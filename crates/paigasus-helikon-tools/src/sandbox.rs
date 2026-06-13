//! The capability-confined [`Sandbox`] shared by the filesystem tools.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;

/// A directory opened as an OS-confined capability. Filesystem operations
/// performed through this handle are resolved relative to the root and cannot
/// escape it (`..`, absolute paths, and escaping symlinks are rejected).
///
/// Cheap to clone (it is `Arc`-backed); share one `Sandbox` across many tools.
#[derive(Clone, Debug)]
pub struct Sandbox {
    inner: Arc<SandboxInner>,
}

#[derive(Debug)]
struct SandboxInner {
    root: PathBuf,
    // Used via `Sandbox::dir()` starting in Task 4; suppressed until then.
    #[allow(dead_code)]
    dir: Dir,
}

impl Sandbox {
    /// Open `root` as a capability-confined sandbox.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SandboxError> {
        let root = root.as_ref();
        let dir = Dir::open_ambient_dir(root, ambient_authority()).map_err(|source| {
            SandboxError::Open {
                path: root.to_path_buf(),
                source,
            }
        })?;
        let canonical = root.canonicalize().map_err(|source| SandboxError::Open {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self {
            inner: Arc::new(SandboxInner {
                root: canonical,
                dir,
            }),
        })
    }

    /// The canonical sandbox root on the host filesystem (diagnostics / cwd).
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The underlying capability directory handle.
    #[allow(dead_code)]
    pub(crate) fn dir(&self) -> &Dir {
        &self.inner.dir
    }
}

/// Reject a tool-supplied path that is absolute or contains a `..`/root/prefix
/// component before it reaches the capability layer. The `cap-std` `Dir` is the
/// backstop for symlink escapes; this is the deterministic front gate.
#[allow(dead_code)]
pub(crate) fn guard_relative(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    if p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!("path escapes the sandbox root: {path}"));
    }
    Ok(p)
}

/// Errors from constructing a [`Sandbox`]. In-`invoke` boundary violations use
/// [`paigasus_helikon_core::ToolError::Denied`], not this type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// The sandbox root could not be opened (missing, not a directory, perms).
    #[error("cannot open sandbox root {path}: {source}")]
    Open {
        /// The path that failed to open.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::guard_relative;
    use std::path::Path;

    #[test]
    fn rejects_parent_dir() {
        assert!(guard_relative("../escape").is_err());
        assert!(guard_relative("a/../b").is_err());
    }

    #[test]
    fn rejects_absolute() {
        assert!(guard_relative("/etc/passwd").is_err());
    }

    #[test]
    fn accepts_nested_relative() {
        assert_eq!(guard_relative("a/b/c.txt").unwrap(), Path::new("a/b/c.txt"));
    }

    #[test]
    fn accepts_dotdot_prefix_filename() {
        // "..foo" is a single Normal component, not a ParentDir component.
        assert!(guard_relative("..foo").is_ok());
    }

    #[test]
    fn accepts_cur_dir_prefix() {
        // "./a" has a CurDir component, which the guard intentionally allows.
        assert!(guard_relative("./a").is_ok());
    }
}
