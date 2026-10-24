//! The only module in the crate that uses `unsafe`: fork, ptrace, waitpid,
//! seccomp and cross-process reads. Everything here is a thin wrapper that
//! takes and returns safe types; the tracer itself (`tracer.rs`) is safe code.
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int, c_long};
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};

pub type Pid = i32;

// Requests, options and events from <linux/ptrace.h>. Spelled out because the
// libc crate types them differently on glibc and musl.
const PTRACE_TRACEME: c_long = 0;
const PTRACE_PEEKDATA: c_long = 2;
const PTRACE_CONT: c_long = 7;
const PTRACE_SETOPTIONS: c_long = 0x4200;
const PTRACE_GETEVENTMSG: c_long = 0x4201;
const PTRACE_GET_SYSCALL_INFO: c_long = 0x420e;

const O_TRACEFORK: usize = 0x2;
const O_TRACEVFORK: usize = 0x4;
const O_TRACECLONE: usize = 0x8;
const O_TRACEEXEC: usize = 0x10;
const O_TRACESECCOMP: usize = 0x80;
const O_EXITKILL: usize = 0x10_0000;

pub const EVENT_FORK: u32 = 1;
pub const EVENT_VFORK: u32 = 2;
pub const EVENT_CLONE: u32 = 3;
pub const EVENT_EXEC: u32 = 4;
pub const EVENT_SECCOMP: u32 = 7;

const SECCOMP_MODE_FILTER: c_int = 2;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JEQ_K: u16 = 0x15;
#[cfg(target_arch = "x86_64")]
const BPF_JGE_K: u16 = 0x35;

/// On x86_64 the x32 ABI reports the same audit arch as the native one and
/// marks its syscall numbers with this bit.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// Whether `nr` belongs to the x32 ABI (never on other architectures).
pub fn is_x32(nr: u64) -> bool {
    cfg!(target_arch = "x86_64") && nr & 0x4000_0000 != 0
}
const BPF_RET_K: u16 = 0x06;

#[cfg(target_arch = "x86_64")]
pub const AUDIT_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
pub const AUDIT_ARCH: u32 = 0xC000_00B7;

const SYSCALL_INFO_SECCOMP: u8 = 3;
const PATH_MAX: usize = 4096;
const PAGE: usize = 4096;

/// A traced child that has been forked and is about to stop itself.
pub struct Spawned {
    pub pid: Pid,
    /// Read end of the pipe the child writes a setup failure to.
    pub err_pipe: OwnedFd,
}

fn cvt(r: c_long) -> io::Result<c_long> {
    if r == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// True for the error a ptrace call returns once the tracee is gone.
pub fn is_gone(e: &io::Error) -> bool {
    e.raw_os_error() == Some(libc::ESRCH)
}

fn ptrace(req: c_long, pid: Pid, addr: usize, data: usize) -> io::Result<c_long> {
    // SAFETY: a raw ptrace(2) call. Every argument is an integer; where `data`
    // is a pointer (GET_SYSCALL_INFO, PEEKDATA, GETEVENTMSG) the callers in
    // this module pass the address of a live local that outlives the call and
    // is at least as large as the kernel is told it is.
    cvt(unsafe { libc::syscall(libc::SYS_ptrace, req, c_long::from(pid), addr, data) })
}

/// Builds the seccomp program: RET_TRACE for each syscall in `numbers` (and for
/// any foreign ABI, which the tracer cannot decode and reports), else RET_ALLOW.
pub fn build_filter(numbers: &[u32]) -> Vec<libc::sock_filter> {
    let ins = |code: u16, jt: u8, jf: u8, k: u32| libc::sock_filter { code, jt, jf, k };
    let mut prog = vec![
        ins(BPF_LD_W_ABS, 0, 0, 4), // seccomp_data.arch
        ins(BPF_JEQ_K, 1, 0, AUDIT_ARCH),
        ins(BPF_RET_K, 0, 0, SECCOMP_RET_TRACE),
        ins(BPF_LD_W_ABS, 0, 0, 0), // seccomp_data.nr
    ];
    // An x32 number matches no entry below and would be allowed untraced; trap
    // it so the tracer refuses the trace.
    #[cfg(target_arch = "x86_64")]
    let x32_at = {
        prog.push(ins(BPF_JGE_K, 0, 0, X32_SYSCALL_BIT));
        prog.len() - 1
    };
    let first = prog.len();
    let allow_at = first + numbers.len();
    #[cfg(target_arch = "x86_64")]
    {
        prog[x32_at].jt = u8::try_from(allow_at - x32_at).expect("filter too long for a jump");
    }
    for (i, nr) in numbers.iter().enumerate() {
        // The TRACE return sits right after ALLOW: skip the rest of the
        // comparisons and the ALLOW.
        let to_trace = u8::try_from(allow_at - (first + i)).expect("filter too long for a jump");
        prog.push(ins(BPF_JEQ_K, to_trace, 0, *nr));
    }
    prog.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW));
    prog.push(ins(BPF_RET_K, 0, 0, SECCOMP_RET_TRACE));
    prog
}

/// Forks the command: the child asks to be traced, stops itself (so the parent
/// can set ptrace options before any filtered syscall), installs the filter and
/// execs through `PATH`. A failure in the child is written to the pipe as
/// `[stage, errno]` and the child exits 127.
pub fn spawn_traced(argv: &[CString], filter: &[libc::sock_filter]) -> io::Result<Spawned> {
    assert!(!argv.is_empty(), "spawn_traced needs a command");
    let mut ptrs: Vec<*const c_char> = argv.iter().map(|c| c.as_ptr()).collect();
    ptrs.push(std::ptr::null());
    let prog = libc::sock_fprog {
        len: u16::try_from(filter.len()).expect("filter too long"),
        filter: filter.as_ptr().cast_mut(),
    };
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` is a valid two-element array for pipe2 to fill.
    cvt(unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } as c_long)?;
    // SAFETY: getpid has no preconditions.
    let parent = unsafe { libc::getpid() };
    // SAFETY: fork has no preconditions; the child below only calls
    // async-signal-safe functions (ptrace, kill, prctl, execvp, write, _exit)
    // on memory prepared before the fork, and never returns.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = io::Error::last_os_error();
        // SAFETY: both fds were just created and are owned here.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        return Err(e);
    }
    if pid == 0 {
        // SAFETY: runs only in the forked child; see `child`.
        unsafe { child(fds[1], &ptrs, &prog, parent) }
    }
    // SAFETY: fds[1] is the write end this process no longer needs; fds[0] is
    // owned exclusively from here (OwnedFd closes it).
    let err_pipe = unsafe {
        libc::close(fds[1]);
        OwnedFd::from_raw_fd(fds[0])
    };
    Ok(Spawned { pid, err_pipe })
}

/// Child side of `spawn_traced`.
///
/// # Safety
/// Must only be called in the child right after `fork`, with `argv` a
/// NUL-terminated array of valid C strings and `prog` a valid filter program.
unsafe fn child(
    err_fd: c_int,
    argv: &[*const c_char],
    prog: &libc::sock_fprog,
    parent: c_int,
) -> ! {
    fn fail(err_fd: c_int, stage: u8) -> ! {
        let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let mut buf = [0u8; 5];
        buf[0] = stage;
        buf[1..].copy_from_slice(&errno.to_ne_bytes());
        // SAFETY: write(2) of a live stack buffer; _exit never returns.
        unsafe {
            libc::write(err_fd, buf.as_ptr().cast(), buf.len());
            libc::_exit(127)
        }
    }
    // SAFETY (whole body): the caller guarantees we are the fresh child, and
    // every call below is async-signal-safe with valid arguments.
    unsafe {
        // If the tracer dies before it has set PTRACE_O_EXITKILL, the child
        // would stay stopped forever; have the kernel kill it instead, and
        // check the tracer did not die before the request took effect.
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
            fail(err_fd, 5);
        }
        if libc::getppid() != parent {
            libc::_exit(127);
        }
        if libc::syscall(
            libc::SYS_ptrace,
            PTRACE_TRACEME,
            0 as c_long,
            0 as c_long,
            0 as c_long,
        ) == -1
        {
            fail(err_fd, 1);
        }
        // Wait for the parent to set its options before the first exec.
        libc::kill(libc::getpid(), libc::SIGSTOP);
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 {
            fail(err_fd, 2);
        }
        if libc::prctl(
            libc::PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER,
            std::ptr::from_ref(prog),
        ) == -1
        {
            fail(err_fd, 3);
        }
        libc::execvp(argv[0], argv.as_ptr());
        fail(err_fd, 4)
    }
}

/// Reads what the child wrote to the setup-failure pipe, once it has exited
/// (every write end is then closed, so this cannot block).
pub fn read_setup_error(pipe: &OwnedFd) -> Option<String> {
    use std::os::fd::AsRawFd;
    let mut buf = [0u8; 5];
    // SAFETY: read(2) into a live 5-byte stack buffer from an fd we own.
    let n = unsafe { libc::read(pipe.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    if n != 5 {
        return None;
    }
    let errno = i32::from_ne_bytes([buf[1], buf[2], buf[3], buf[4]]);
    let stage = match buf[0] {
        1 => "ptrace(TRACEME)",
        2 => "prctl(NO_NEW_PRIVS)",
        3 => "seccomp filter",
        _ => "exec",
    };
    Some(format!("{stage}: {}", io::Error::from_raw_os_error(errno)))
}

/// One `waitpid` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    Exited {
        pid: Pid,
        code: i32,
    },
    Signaled {
        pid: Pid,
        sig: i32,
    },
    /// `event` is the `PTRACE_EVENT_*` (0 for a plain signal stop).
    Stopped {
        pid: Pid,
        sig: i32,
        event: u32,
    },
    NoChildren,
}

fn wait(pid: Pid, flags: c_int) -> io::Result<Wait> {
    loop {
        let mut status: c_int = 0;
        // SAFETY: waitpid with a valid pointer to a local status word.
        let r = unsafe { libc::waitpid(pid, &mut status, flags) };
        if r < 0 {
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::ECHILD) => return Ok(Wait::NoChildren),
                _ => return Err(e),
            }
        }
        return Ok(if libc::WIFEXITED(status) {
            Wait::Exited {
                pid: r,
                code: libc::WEXITSTATUS(status),
            }
        } else if libc::WIFSIGNALED(status) {
            Wait::Signaled {
                pid: r,
                sig: libc::WTERMSIG(status),
            }
        } else {
            Wait::Stopped {
                pid: r,
                sig: libc::WSTOPSIG(status),
                event: ((status >> 16) & 0xff) as u32,
            }
        });
    }
}

/// Waits for any traced task of the calling thread (`__WNOTHREAD` keeps one
/// trace from reaping another thread's children).
pub fn wait_any() -> io::Result<Wait> {
    wait(-1, libc::__WALL | libc::__WNOTHREAD)
}

pub fn wait_for(pid: Pid) -> io::Result<Wait> {
    wait(pid, libc::__WALL | libc::__WNOTHREAD)
}

/// Sets the options on the first tracee; descendants inherit them.
pub fn set_options(pid: Pid) -> io::Result<()> {
    let opts =
        O_TRACEFORK | O_TRACEVFORK | O_TRACECLONE | O_TRACEEXEC | O_TRACESECCOMP | O_EXITKILL;
    ptrace(PTRACE_SETOPTIONS, pid, 0, opts).map(|_| ())
}

/// Resumes a stopped tracee, delivering `sig` (0 for none).
pub fn cont(pid: Pid, sig: i32) -> io::Result<()> {
    ptrace(PTRACE_CONT, pid, 0, sig as usize).map(|_| ())
}

/// The new task's id after a fork/vfork/clone event stop.
pub fn event_msg(pid: Pid) -> io::Result<u64> {
    let mut msg: u64 = 0;
    ptrace(
        PTRACE_GETEVENTMSG,
        pid,
        0,
        std::ptr::from_mut(&mut msg) as usize,
    )?;
    Ok(msg)
}

/// The calling thread's kernel id: the `TracerPid` its tracees report (ptrace
/// attaches to a thread, not a process).
pub fn gettid() -> Pid {
    // SAFETY: gettid(2) takes no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) as Pid }
}

pub fn kill(pid: Pid) {
    // SAFETY: kill(2) with integer arguments; a stale pid only yields ESRCH.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// A seccomp-stop's syscall: its number, ABI and raw arguments.
#[derive(Debug, Clone, Copy)]
pub struct SeccompStop {
    pub arch: u32,
    pub nr: u64,
    pub args: [u64; 6],
}

#[repr(C)]
struct RawSyscallInfo {
    op: u8,
    _pad: [u8; 3],
    arch: u32,
    _instruction_pointer: u64,
    _stack_pointer: u64,
    // The union: entry/seccomp are `nr` + six args (+ `ret_data`), exit is
    // `rval` + `is_error`; eight words cover the largest.
    data: [u64; 8],
}

/// Reads the syscall of a seccomp stop via `PTRACE_GET_SYSCALL_INFO`, which
/// gives the number and arguments on every architecture without a register
/// table. `Ok(None)` if the stop is not a seccomp stop.
pub fn seccomp_stop(pid: Pid) -> io::Result<Option<SeccompStop>> {
    let mut raw = RawSyscallInfo {
        op: 0,
        _pad: [0; 3],
        arch: 0,
        _instruction_pointer: 0,
        _stack_pointer: 0,
        data: [0; 8],
    };
    ptrace(
        PTRACE_GET_SYSCALL_INFO,
        pid,
        std::mem::size_of::<RawSyscallInfo>(),
        std::ptr::from_mut(&mut raw) as usize,
    )?;
    if raw.op != SYSCALL_INFO_SECCOMP {
        return Ok(None);
    }
    let mut args = [0u64; 6];
    args.copy_from_slice(&raw.data[1..7]);
    Ok(Some(SeccompStop {
        arch: raw.arch,
        nr: raw.data[0],
        args,
    }))
}

/// Reads up to `buf.len()` bytes of the tracee's memory at `addr`; short reads
/// happen at an unmapped page.
fn read_mem(pid: Pid, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let remote = libc::iovec {
        iov_base: addr as usize as *mut _,
        iov_len: buf.len(),
    };
    // SAFETY: `local` points at `buf`, which is valid for `buf.len()` writes;
    // `remote` is only an address in the other process, which the kernel
    // validates.
    let n = unsafe { libc::process_vm_readv(pid, &local, 1, &remote, 1, 0) };
    if n >= 0 {
        return Ok(n as usize);
    }
    let e = io::Error::last_os_error();
    match e.raw_os_error() {
        // process_vm_readv can be denied (container seccomp) or missing.
        Some(libc::ENOSYS | libc::EPERM) => peek_mem(pid, addr, buf),
        _ => Err(e),
    }
}

/// Fallback: whole aligned words through PTRACE_PEEKDATA, which never straddle
/// a page.
fn peek_mem(pid: Pid, addr: u64, buf: &mut [u8]) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        let at = addr + done as u64;
        let aligned = at & !7;
        let mut word: u64 = 0;
        if let Err(e) = ptrace(
            PTRACE_PEEKDATA,
            pid,
            aligned as usize,
            std::ptr::from_mut(&mut word) as usize,
        ) {
            return if done > 0 { Ok(done) } else { Err(e) };
        }
        let bytes = word.to_ne_bytes();
        let skip = (at - aligned) as usize;
        let take = (8 - skip).min(buf.len() - done);
        buf[done..done + take].copy_from_slice(&bytes[skip..skip + take]);
        done += take;
    }
    Ok(done)
}

/// Reads the NUL-terminated string at `addr` (at most `PATH_MAX` bytes),
/// without the terminator. An unreadable address is `Ok(None)`: the syscall
/// will fail with EFAULT and touched nothing.
pub fn read_cstring(pid: Pid, addr: u64) -> io::Result<Option<Vec<u8>>> {
    let mut out = Vec::new();
    let mut at = addr;
    while out.len() < PATH_MAX {
        let to_page_end = PAGE - (at as usize % PAGE);
        let want = to_page_end.min(PATH_MAX - out.len());
        let mut chunk = vec![0u8; want];
        let n = match read_mem(pid, at, &mut chunk) {
            Ok(n) => n,
            Err(e) if matches!(e.raw_os_error(), Some(libc::EFAULT | libc::EIO)) => 0,
            Err(e) => return Err(e),
        };
        if let Some(nul) = chunk[..n].iter().position(|b| *b == 0) {
            out.extend_from_slice(&chunk[..nul]);
            return Ok(Some(out));
        }
        out.extend_from_slice(&chunk[..n]);
        if n < want {
            return Ok(if out.is_empty() { None } else { Some(out) });
        }
        at += n as u64;
    }
    Ok(Some(out))
}

/// Reads the first eight bytes at `addr` as a native-endian u64 (the flags of
/// an `open_how`).
pub fn read_u64(pid: Pid, addr: u64) -> io::Result<Option<u64>> {
    let mut b = [0u8; 8];
    match read_mem(pid, addr, &mut b) {
        Ok(8) => Ok(Some(u64::from_ne_bytes(b))),
        Ok(_) => Ok(None),
        Err(e) if matches!(e.raw_os_error(), Some(libc::EFAULT | libc::EIO)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A just-enough classic-BPF interpreter for the opcodes the filter uses.
    fn run(prog: &[libc::sock_filter], nr: u32, arch: u32) -> u32 {
        let (mut acc, mut pc) = (0u32, 0usize);
        loop {
            let i = prog[pc];
            pc += 1;
            match i.code {
                BPF_LD_W_ABS => acc = if i.k == 0 { nr } else { arch },
                BPF_JEQ_K => pc += usize::from(if acc == i.k { i.jt } else { i.jf }),
                #[cfg(target_arch = "x86_64")]
                BPF_JGE_K => pc += usize::from(if acc >= i.k { i.jt } else { i.jf }),
                BPF_RET_K => return i.k,
                c => panic!("unexpected opcode {c:#x}"),
            }
        }
    }

    #[test]
    fn the_filter_traces_listed_numbers_and_allows_the_rest() {
        let prog = build_filter(&[257, 59, 262]);
        for nr in [257, 59, 262] {
            assert_eq!(run(&prog, nr, AUDIT_ARCH), SECCOMP_RET_TRACE, "{nr}");
        }
        for nr in [0, 1, 3, 256, 263] {
            assert_eq!(run(&prog, nr, AUDIT_ARCH), SECCOMP_RET_ALLOW, "{nr}");
        }
    }

    #[test]
    fn a_foreign_abi_is_traced_so_the_tracer_can_refuse_it() {
        let prog = build_filter(&[257]);
        assert_eq!(run(&prog, 0, AUDIT_ARCH ^ 0x4000_0000), SECCOMP_RET_TRACE);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn an_x32_syscall_number_is_traced_so_the_tracer_can_refuse_it() {
        let prog = build_filter(&[257, 59]);
        // 0x4000_0000 | 0 is read(2) in the x32 ABI: it matches no entry, and
        // without the range check the filter would allow it untraced.
        for nr in [0x4000_0000, 0x4000_0001, 0x4000_0000 | 257, u32::MAX] {
            assert_eq!(run(&prog, nr, AUDIT_ARCH), SECCOMP_RET_TRACE, "{nr:#x}");
        }
        assert!(is_x32(0x4000_0003) && !is_x32(3));
    }

    #[test]
    fn the_real_filter_covers_every_filtered_syscall() {
        let nrs = super::super::syscalls::filtered();
        let prog = build_filter(&nrs);
        for nr in nrs {
            assert_eq!(run(&prog, nr, AUDIT_ARCH), SECCOMP_RET_TRACE);
        }
    }
}
