//! The tracer loop: safe code over the wrappers in `sys.rs`.
//!
//! One thread forks the command and then waits for every task in its tree
//! (processes and threads alike; ptrace reports them all to the tracer). Only
//! the filtered syscalls stop a task, so the tree runs at full speed between
//! file-system lookups.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::TraceError;
use super::record::{Access, AccessKind, Recorder, TraceManifest, normalise};
use super::sys::{self, Pid, Wait};
use super::syscalls::{self, AT_FDCWD, Flags, O_CREAT, O_PATH, O_TRUNC, Op};

struct Task {
    /// Its first stop (the SIGSTOP of a fresh tracee) has not been seen.
    fresh: bool,
}

/// The live tasks. Dropping it (a panic, an early return) kills them: a
/// tracee must never be left stopped.
struct Tree {
    alive: HashMap<Pid, Task>,
}

/// Whether `pid` is a task this process is tracing right now. `alive` can hold
/// stale ids (a child that exited before its fork event arrived, a thread that
/// gave up its id by exec'ing), and a stale id may belong to an unrelated
/// process by the time it is killed; only a live tracee of ours has our thread
/// id (the tracer is the thread that forked) as its `TracerPid`.
fn is_our_tracee(pid: Pid) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return false;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("TracerPid:"))
        .and_then(|v| v.trim().parse::<u32>().ok())
        == Some(sys::gettid() as u32)
}

fn kill_ours(pid: Pid) {
    if is_our_tracee(pid) {
        sys::kill(pid);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        self.alive.keys().for_each(|pid| kill_ours(*pid));
    }
}

impl Tree {
    /// Kills everything and reaps it, best effort.
    fn abort(&mut self) {
        self.alive.keys().for_each(|pid| kill_ours(*pid));
        while !self.alive.is_empty() {
            match sys::wait_any() {
                Ok(Wait::Exited { pid, .. } | Wait::Signaled { pid, .. }) => {
                    self.alive.remove(&pid);
                }
                Ok(Wait::Stopped { pid, .. }) => {
                    // A task forked after the kill: take it down as well.
                    self.alive.entry(pid).or_insert(Task { fresh: false });
                    kill_ours(pid);
                    let _ = sys::cont(pid, 0);
                }
                Ok(Wait::NoChildren) | Err(_) => break,
            }
        }
        self.alive.clear();
    }
}

/// Runs `command` under the tracer and returns its manifest. The command's
/// exit status is in the manifest; an `Err` means the trace itself failed
/// (and the tree is dead).
pub fn trace(root: &Path, command: &[String]) -> Result<TraceManifest, TraceError> {
    if command.is_empty() {
        return Err(TraceError::Setup("no command to trace".into()));
    }
    let argv = command
        .iter()
        .map(|a| CString::new(a.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TraceError::Setup("a command argument contains a NUL byte".into()))?;
    let filter = sys::build_filter(&syscalls::filtered());
    let mut recorder = Recorder::new(root);

    let spawned = sys::spawn_traced(&argv, &filter)?;
    let root_pid = spawned.pid;
    let mut tree = Tree {
        alive: HashMap::from([(root_pid, Task { fresh: false })]),
    };

    // The child stops itself before installing the filter; set the options now
    // so its exec (and everything after) is reported.
    match sys::wait_for(root_pid)? {
        Wait::Stopped { sig, .. } if sig == libc::SIGSTOP => {}
        Wait::Exited { .. } | Wait::Signaled { .. } => {
            tree.alive.clear();
            let why = sys::read_setup_error(&spawned.err_pipe)
                .unwrap_or_else(|| "the child exited before it could be traced".into());
            return Err(TraceError::Setup(why));
        }
        other => {
            tree.abort();
            return Err(TraceError::Tracer(format!(
                "unexpected first stop {other:?}"
            )));
        }
    }
    let started = sys::set_options(root_pid).and_then(|()| sys::cont(root_pid, 0));
    if let Err(e) = started {
        tree.abort();
        return Err(e.into());
    }

    match run(&mut tree, &mut recorder, root_pid) {
        Ok(code) => {
            if code == 127
                && let Some(why) = sys::read_setup_error(&spawned.err_pipe)
            {
                return Err(TraceError::Setup(why));
            }
            // Every task was reaped; nothing left for `Tree::drop` to kill.
            tree.alive.clear();
            Ok(recorder.finish(command.to_vec(), code))
        }
        Err(e) => {
            tree.abort();
            Err(e)
        }
    }
}

/// Follows the tree until the root exits; returns its exit code. Whatever it
/// left running (a dev server, a worker daemon) is killed: an untraced run
/// returns as soon as the root exits, and the filter cannot be left on a task
/// nobody traces (a stopped-for-tracer syscall without a tracer fails with
/// ENOSYS). Their later accesses are not recorded.
fn run(tree: &mut Tree, recorder: &mut Recorder, root_pid: Pid) -> Result<i32, TraceError> {
    let mut root_code = None;
    while !tree.alive.is_empty() {
        match sys::wait_any()? {
            Wait::NoChildren => break,
            Wait::Exited { pid, code } => {
                tree.alive.remove(&pid);
                if pid == root_pid {
                    root_code = Some(code);
                }
            }
            Wait::Signaled { pid, sig } => {
                tree.alive.remove(&pid);
                if pid == root_pid {
                    root_code = Some(128 + sig);
                }
            }
            Wait::Stopped { pid, sig, event } => stopped(tree, recorder, pid, sig, event)?,
        }
        if root_code.is_some() {
            tree.abort();
            break;
        }
    }
    root_code.ok_or_else(|| TraceError::Tracer("the traced command's exit was never seen".into()))
}

/// `Ok` also when the task vanished under us (ESRCH): its exit is reported by
/// the next wait.
fn resume(pid: Pid, sig: i32) -> Result<(), TraceError> {
    match sys::cont(pid, sig) {
        Err(e) if !sys::is_gone(&e) => Err(e.into()),
        _ => Ok(()),
    }
}

fn stopped(
    tree: &mut Tree,
    recorder: &mut Recorder,
    pid: Pid,
    sig: i32,
    event: u32,
) -> Result<(), TraceError> {
    let initial = match tree.alive.get_mut(&pid) {
        Some(task) => std::mem::replace(&mut task.fresh, false),
        None => {
            tree.alive.insert(pid, Task { fresh: false });
            true
        }
    };
    if initial && sig == libc::SIGSTOP && event == 0 {
        return resume(pid, 0);
    }
    if sig != libc::SIGTRAP || event == 0 {
        // A signal on its way to the task: pass it on. Exec is reported as
        // PTRACE_EVENT_EXEC and the filter as PTRACE_EVENT_SECCOMP, so a plain
        // SIGTRAP is never the tracer's own: it is the task's (int3, raise).
        return resume(pid, sig);
    }
    match event {
        sys::EVENT_SECCOMP => {
            match on_syscall(recorder, pid) {
                Err(TraceError::Io(e)) if sys::is_gone(&e) => {}
                other => other?,
            }
            resume(pid, 0)
        }
        sys::EVENT_FORK | sys::EVENT_VFORK | sys::EVENT_CLONE => {
            if let Ok(new) = sys::event_msg(pid) {
                tree.alive.entry(new as Pid).or_insert(Task { fresh: true });
            }
            resume(pid, 0)
        }
        sys::EVENT_EXEC => {
            if let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) {
                recorder.record_exec_binary(&strip_deleted(&exe));
            }
            resume(pid, 0)
        }
        _ => resume(pid, 0),
    }
}

fn strip_deleted(p: &Path) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(s) => PathBuf::from(s),
        None => p.to_owned(),
    }
}

/// Decodes the stopped syscall, resolves its path in the tracee's view and
/// records it.
fn on_syscall(recorder: &mut Recorder, pid: Pid) -> Result<(), TraceError> {
    let Some(stop) = sys::seccomp_stop(pid)? else {
        return Ok(());
    };
    if stop.arch != sys::AUDIT_ARCH || sys::is_x32(stop.nr) {
        return Err(TraceError::Tracer(
            "a traced process used a syscall ABI this tracer cannot decode".into(),
        ));
    }
    let Some(op) = syscalls::decode(stop.nr, stop.args) else {
        return Ok(());
    };
    let (dirfd, ptr, follow) = match op {
        Op::Open { dirfd, path, .. } | Op::Exec { dirfd, path } => (dirfd, path, true),
        Op::Creat { path } => (AT_FDCWD, path, true),
        Op::Probe {
            dirfd,
            path,
            follow,
        } => (dirfd, path, follow),
    };
    let Some(raw) = sys::read_cstring(pid, ptr)? else {
        return Ok(());
    };
    let Some(path) = resolve(pid, dirfd, &raw) else {
        return Ok(());
    };
    let (mut existed, mut is_dir) = lookup(&path, follow);
    // A call that examined the link itself, or followed it to nothing, saw the
    // link's text: record that, so retargeting it is noticed.
    let is_link = fs::symlink_metadata(&path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    let link = is_link && (!follow || !existed);
    if link {
        existed = true;
        is_dir = false;
    }
    let kind = match op {
        Op::Creat { .. } => AccessKind::Write,
        Op::Exec { .. } => AccessKind::Exec,
        Op::Probe { .. } => AccessKind::Probe,
        Op::Open { flags, .. } => {
            let flags = match flags {
                Flags::Value(v) => v,
                Flags::OpenHow(addr) => sys::read_u64(pid, addr)?.unwrap_or(0),
            };
            if flags & O_PATH != 0 {
                AccessKind::Probe
            } else if flags & O_TRUNC != 0 || (flags & O_CREAT != 0 && !existed) {
                AccessKind::Write
            } else {
                AccessKind::Read
            }
        }
    };
    recorder.record(&Access {
        path,
        kind,
        existed,
        is_dir,
        link,
    });
    Ok(())
}

/// Whether `path` exists right now and whether it is a directory. A lookup
/// that fails for any reason other than absence counts as existing (it was
/// there; we just may not read it).
fn lookup(path: &Path, follow: bool) -> (bool, bool) {
    let meta = if follow {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };
    match meta {
        Ok(m) => (true, m.is_dir()),
        Err(e) => match e.raw_os_error() {
            Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP) => (false, false),
            _ => (true, false),
        },
    }
}

/// The absolute path the tracee meant: against its cwd or the directory behind
/// `dirfd`, `..` resolved the way the kernel would when the path exists.
fn resolve(pid: Pid, dirfd: i32, raw: &[u8]) -> Option<PathBuf> {
    if raw.is_empty() {
        return None; // AT_EMPTY_PATH: the dirfd itself, not a name
    }
    let given = Path::new(OsStr::from_bytes(raw));
    let joined = if given.is_absolute() {
        given.to_owned()
    } else {
        let base = if dirfd == AT_FDCWD {
            fs::read_link(format!("/proc/{pid}/cwd")).ok()?
        } else {
            fs::read_link(format!("/proc/{pid}/fd/{dirfd}")).ok()?
        };
        if !base.is_absolute() {
            return None; // a pipe or socket
        }
        base.join(given)
    };
    if joined.components().any(|c| c.as_os_str() == "..") {
        // `a/link/../b` is not `a/b`; ask the file system when it can answer.
        if let Ok(canon) = fs::canonicalize(&joined) {
            return Some(canon);
        }
    }
    Some(normalise(&joined))
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn only_a_task_this_process_is_tracing_is_ours_to_kill() {
        assert!(!is_our_tracee(sys::gettid()), "ourselves");
        assert!(!is_our_tracee(i32::MAX), "no such process");
    }

    #[test]
    fn a_stale_id_that_now_names_an_unrelated_process_is_left_alone() {
        let mut other = Command::new("sleep")
            .arg("3600")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        kill_ours(other.id() as Pid);
        assert!(other.try_wait().unwrap().is_none(), "it was killed");
        other.kill().unwrap();
        other.wait().unwrap();
    }
}
