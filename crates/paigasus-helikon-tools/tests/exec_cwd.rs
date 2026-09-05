//! `HostBackend`'s **working-directory pinning**, asserted end-to-end on every
//! platform (SMA-615).
//!
//! Ungated on purpose. Every pre-existing real-process test that depends on cwd
//! is `#![cfg(unix)]` (`tests/host_backend.rs`), and the two ungated
//! process-spawning files that predate this one were both deliberately made
//! cwd-independent, so nothing in the suite has ever checked this on Windows.
//!
//! Every assertion here embeds BOTH captured streams. On Windows the question
//! this file exists to answer is settled by text `cmd.exe` may print to either
//! one, and `cargo test` runs without `--nocapture`, so a passing test's output
//! never reaches the CI log — the assertion message is the only readout.
#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use paigasus_helikon_tools::{ExecOutput, ExecRequest, HostBackend, Sandbox};

/// Prints the child's working directory: a `sh` builtin on unix, `cd` with no
/// arguments on Windows.
#[cfg(unix)]
const PRINT_CWD: &str = "pwd";
#[cfg(windows)]
const PRINT_CWD: &str = "cd";

/// Prints a file named by a RELATIVE path — the user-visible consequence of a
/// pinned (or unpinned) working directory.
#[cfg(unix)]
const PRINT_MARKER: &str = "cat marker.txt";
#[cfg(windows)]
const PRINT_MARKER: &str = "type marker.txt";

/// Short, newline-free, `%`-free ASCII. `cmd.exe`'s `type` emits file bytes
/// verbatim and a `%` would risk environment expansion, so this content cannot
/// make the test fail for a quoting or expansion reason.
const MARKER_CONTENT: &str = "sma615-marker";

/// What `cmd.exe` prints when it rejects its startup working directory as a UNC
/// path and resets to `%SystemRoot%`. The banner and the reset are the same code
/// path, so seeing it is confirmation on its own — whatever `cd` then printed,
/// and whichever stream it landed on.
#[cfg(windows)]
const UNC_BANNER: &str = "UNC paths are not supported";

/// Run `command` through a real `HostBackend` over a fresh sandbox at `dir`,
/// returning the captured output alongside the root the backend was told to pin
/// to.
///
/// The outer `tokio::time::timeout` mirrors `exec_timeout_portable.rs` and
/// `exec_env_defaults.rs`: a regression must fail fast rather than stall the
/// required Windows gate for the whole backend timeout.
async fn run_in_sandbox(dir: &Path, command: &str) -> (ExecOutput, PathBuf) {
    let sandbox = Sandbox::open(dir).expect("open sandbox");
    let root = sandbox.root().to_path_buf();
    let backend = HostBackend::builder(sandbox)
        .timeout(Duration::from_secs(10))
        .build();

    let out = tokio::time::timeout(
        Duration::from_secs(20),
        backend.run(ExecRequest::new(command)),
    )
    .await
    .expect("run must return promptly, not hang")
    .expect("the backend must not error; a non-zero exit is a normal result");

    (out, root)
}

/// The child's own report of its working directory, exactly as printed — NOT
/// normalized. The LAST non-empty line, because on Windows a UNC banner would
/// precede `cd`'s output.
fn reported_cwd(out: &ExecOutput) -> &str {
    out.stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "the shell printed no working directory at all; stdout={:?} stderr={:?}",
                out.stdout, out.stderr
            )
        })
}

/// `HostBackend` runs its command with the working directory pinned to the
/// sandbox root — the contract stated in `src/exec/host.rs`'s module docs, on
/// `HostBackend::builder`, and in `docs/book/src/concepts/tools.md`.
#[tokio::test]
async fn host_backend_pins_cwd_to_the_sandbox_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;

    assert_eq!(
        out.exit_code,
        Some(0),
        "the shell must run at all before its working directory means anything; \
         stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );

    // Decisive on its own: `cmd.exe` prints this banner on the same code path
    // that resets the working directory to `%SystemRoot%`.
    #[cfg(windows)]
    assert!(
        !out.stdout.contains(UNC_BANNER) && !out.stderr.contains(UNC_BANNER),
        "cmd.exe rejected the sandbox root as a UNC path and reset its working \
         directory; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );

    let reported = reported_cwd(&out);

    // Canonicalize BOTH sides. That normalizes 8.3 short names (the Windows
    // runner's TEMP is `C:\Users\RUNNER~1\...`), case, and verbatim-ness. It
    // normalizes *spelling*, never *identity*, so a working directory of
    // `C:\Windows` still compares unequal here.
    let observed = Path::new(reported).canonicalize().unwrap_or_else(|e| {
        panic!(
            "the shell reported a working directory that does not resolve: \
             {reported:?}: {e}; stdout={:?} stderr={:?}",
            out.stdout, out.stderr
        )
    });
    let expected = root
        .canonicalize()
        .unwrap_or_else(|e| panic!("the sandbox root {} does not resolve: {e}", root.display()));

    assert_eq!(
        observed, expected,
        "the child ran in the wrong directory: it reported {reported:?}; \
         stdout={:?} stderr={:?}",
        out.stdout, out.stderr
    );
}

/// A relative path in a command resolves inside the sandbox. This is what a user
/// actually hits, and it fails with a different message than the contract test
/// above — so a red gate distinguishes "the working directory is elsewhere" from
/// "the two spellings differ".
///
/// The ungated counterpart of `tests/host_backend.rs`'s
/// `host_backend_runs_command_in_cwd`, which stays where it is: that file's other
/// tests are unix-only for `rlimit` reasons.
#[tokio::test]
async fn host_backend_resolves_a_relative_path_in_the_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), MARKER_CONTENT).unwrap();

    let (out, _root) = run_in_sandbox(tmp.path(), PRINT_MARKER).await;

    assert_eq!(
        out.exit_code,
        Some(0),
        "a relative path must resolve inside the sandbox; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains(MARKER_CONTENT),
        "the file read back did not contain the marker; stdout={:?} stderr={:?}",
        out.stdout,
        out.stderr
    );
}

/// Characterization guard for **why** the cwd contract holds on Windows
/// (SMA-615). Measured on `test (windows-latest, stable)` and
/// `test (windows-latest, 1.94)` on 2026-09-05: given the verbatim working
/// directory `\\?\C:\Users\runneradmin\AppData\Local\Temp\.tmpTg6QTA`, the child
/// reported `C:\Users\runneradmin\AppData\Local\Temp\.tmpTg6QTA` and stderr was
/// empty — no UNC banner.
///
/// So `CreateProcessW` normalizes the verbatim prefix away before the child
/// observes it, and `cmd.exe` never sees a path beginning `\\`. The ticket's
/// suspicion fails for *that* reason — **not** because `cmd.exe` tolerates
/// verbatim working directories, which was never tested and is not claimed here.
///
/// Pinned so the assumption cannot rot silently. If a future Rust changes how
/// `Command::current_dir` passes `lpCurrentDirectory`, or a future Windows stops
/// normalizing, the verbatim path would reach `cmd.exe` and the working directory
/// would stop being pinned — `host_backend_pins_cwd_to_the_sandbox_root` would go
/// red alongside this, and the fix would be to strip the prefix in
/// `Sandbox::open` (`dunce::canonicalize`).
#[tokio::test]
#[cfg(windows)]
async fn windows_child_reports_a_normalized_cwd() {
    let tmp = tempfile::tempdir().unwrap();
    let (out, root) = run_in_sandbox(tmp.path(), PRINT_CWD).await;
    let reported = reported_cwd(&out);

    assert!(
        !reported.starts_with(r"\\?\"),
        "the child reported the verbatim path {reported:?} for a working \
         directory passed as {}; CreateProcessW no longer normalizes it, so \
         cmd.exe may stop honouring the sandbox root. stdout={:?} stderr={:?}",
        root.display(),
        out.stdout,
        out.stderr
    );
}
