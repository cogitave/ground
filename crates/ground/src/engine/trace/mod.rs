//! `ground trace`: records which files a command's process tree touched.
//!
//! Linux only (x86_64 tested; aarch64 compiled from the same tables). The command runs under ptrace with a seccomp filter that stops
//! the tracer on file-system lookups only (open, exec, stat, access, readlink
//! families); every descendant, thread or process, is followed. The result is a
//! [`TraceManifest`]: the inputs under a root (hashed at the end of the run),
//! the paths probed and not found, and the executables that ran.
//!
//! Limits, by design: it is an input recorder, not a sandbox. A path in
//! tracee memory can change between the stop and the syscall (another thread
//! rewriting it), a task using the 32-bit syscall ABI fails the trace, and
//! setuid binaries do not elevate (the filter needs `no_new_privs`). On any
//! other platform, or when the trace fails, the caller must select everything.

mod deps;
mod record;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod sys;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod syscalls;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod tracer;

use std::path::{Path, PathBuf};
use std::process::Command;

pub use record::{Access, AccessKind, Recorder, TraceManifest, hash_link, hash_path, normalise};

/// Why a trace produced no manifest.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("tracing unsupported on this platform")]
    Unsupported,
    #[error("{0}")]
    Setup(String),
    #[error("trace failed: {0}")]
    Tracer(String),
    #[error("trace failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Whether this build can trace at all.
pub const fn supported() -> bool {
    cfg!(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
}

/// What to trace: `command` (program then arguments) is run in the current
/// directory and inherits stdio and the environment.
#[derive(Debug, Clone)]
pub struct TraceOptions {
    /// Files outside it are not recorded (exec'd binaries still are, as the
    /// toolchain fingerprint).
    pub root: PathBuf,
    pub command: Vec<String>,
}

/// Runs the command to completion under the tracer. Blocks the calling thread
/// (all ptrace calls must come from the thread that forked).
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub fn trace_command(opts: &TraceOptions) -> Result<TraceManifest, TraceError> {
    tracer::trace(&opts.root, &opts.command)
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub fn trace_command(_opts: &TraceOptions) -> Result<TraceManifest, TraceError> {
    Err(TraceError::Unsupported)
}

/// The git toplevel containing `dir` (the default trace root).
pub fn git_toplevel(dir: &Path) -> Result<PathBuf, TraceError> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .map_err(|e| TraceError::Setup(format!("cannot run git to find the root: {e}")))?;
    if !out.status.success() {
        return Err(TraceError::Setup(
            "not inside a git repository; pass --root".into(),
        ));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}
