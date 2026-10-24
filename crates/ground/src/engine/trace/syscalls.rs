//! The syscalls the tracer stops on and how to read their arguments.
//!
//! The numbers come from `libc` per architecture; aarch64 has no `open`,
//! `creat`, `stat`, `lstat`, `access` or `readlink` (only the `*at` forms), so
//! those entries exist on x86_64 only.

/// `AT_FDCWD`: resolve against the tracee's working directory.
pub const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;

pub const O_CREAT: u64 = 0o100;
pub const O_TRUNC: u64 = 0o1000;
pub const O_PATH: u64 = 0o10_000_000;

/// Where an open's flags live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flags {
    /// The flags are the argument itself.
    Value(u64),
    /// `openat2`: the address of a `struct open_how` whose first field is the flags.
    OpenHow(u64),
}

/// A filtered syscall, decoded far enough to know which path it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Open { dirfd: i32, path: u64, flags: Flags },
    Creat { path: u64 },
    Exec { dirfd: i32, path: u64 },
    Probe { dirfd: i32, path: u64, follow: bool },
}

/// Every syscall number the seccomp filter traps.
pub fn filtered() -> Vec<u32> {
    #[allow(unused_mut)]
    let mut v: Vec<i64> = vec![
        libc::SYS_openat,
        libc::SYS_openat2,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_readlinkat,
    ];
    #[cfg(target_arch = "x86_64")]
    v.extend([
        libc::SYS_open,
        libc::SYS_creat,
        libc::SYS_stat,
        libc::SYS_lstat,
        libc::SYS_access,
        libc::SYS_readlink,
    ]);
    v.into_iter().map(|n| n as u32).collect()
}

fn fd(arg: u64) -> i32 {
    arg as u32 as i32
}

/// Decodes syscall `nr` with its six raw arguments; `None` for a number the
/// filter does not trap.
pub fn decode(nr: u64, a: [u64; 6]) -> Option<Op> {
    let nr = nr as i64;
    #[cfg(target_arch = "x86_64")]
    {
        match nr {
            n if n == libc::SYS_open => {
                return Some(Op::Open {
                    dirfd: AT_FDCWD,
                    path: a[0],
                    flags: Flags::Value(a[1]),
                });
            }
            n if n == libc::SYS_creat => return Some(Op::Creat { path: a[0] }),
            n if n == libc::SYS_stat || n == libc::SYS_access => {
                return Some(Op::Probe {
                    dirfd: AT_FDCWD,
                    path: a[0],
                    follow: true,
                });
            }
            n if n == libc::SYS_lstat || n == libc::SYS_readlink => {
                return Some(Op::Probe {
                    dirfd: AT_FDCWD,
                    path: a[0],
                    follow: false,
                });
            }
            _ => {}
        }
    }
    Some(match nr {
        n if n == libc::SYS_openat => Op::Open {
            dirfd: fd(a[0]),
            path: a[1],
            flags: Flags::Value(a[2]),
        },
        n if n == libc::SYS_openat2 => Op::Open {
            dirfd: fd(a[0]),
            path: a[1],
            flags: Flags::OpenHow(a[2]),
        },
        n if n == libc::SYS_execve => Op::Exec {
            dirfd: AT_FDCWD,
            path: a[0],
        },
        n if n == libc::SYS_execveat => Op::Exec {
            dirfd: fd(a[0]),
            path: a[1],
        },
        n if n == libc::SYS_newfstatat => Op::Probe {
            dirfd: fd(a[0]),
            path: a[1],
            follow: a[3] & AT_SYMLINK_NOFOLLOW == 0,
        },
        n if n == libc::SYS_statx => Op::Probe {
            dirfd: fd(a[0]),
            path: a[1],
            follow: a[2] & AT_SYMLINK_NOFOLLOW == 0,
        },
        n if n == libc::SYS_faccessat => Op::Probe {
            dirfd: fd(a[0]),
            path: a[1],
            follow: true,
        },
        n if n == libc::SYS_faccessat2 => Op::Probe {
            dirfd: fd(a[0]),
            path: a[1],
            follow: a[3] & AT_SYMLINK_NOFOLLOW == 0,
        },
        n if n == libc::SYS_readlinkat => Op::Probe {
            dirfd: fd(a[0]),
            path: a[1],
            follow: false,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_filtered_number_decodes() {
        for nr in filtered() {
            assert!(decode(u64::from(nr), [1; 6]).is_some(), "syscall {nr}");
        }
    }

    #[test]
    fn an_unfiltered_number_does_not_decode() {
        assert_eq!(decode(libc::SYS_read as u64, [0; 6]), None);
    }

    #[test]
    fn openat_takes_the_dirfd_path_and_flags_from_the_right_arguments() {
        let op = decode(
            libc::SYS_openat as u64,
            [(-100i32) as u32 as u64, 0x10, 0o101, 0, 0, 0],
        );
        assert_eq!(
            op,
            Some(Op::Open {
                dirfd: AT_FDCWD,
                path: 0x10,
                flags: Flags::Value(0o101)
            })
        );
    }

    #[test]
    fn statx_nofollow_is_read_from_its_flags_argument() {
        let op = decode(
            libc::SYS_statx as u64,
            [3, 0x20, AT_SYMLINK_NOFOLLOW, 0, 0, 0],
        );
        assert_eq!(
            op,
            Some(Op::Probe {
                dirfd: 3,
                path: 0x20,
                follow: false
            })
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn plain_open_and_creat_use_the_working_directory() {
        assert_eq!(
            decode(libc::SYS_creat as u64, [0x30, 0o644, 0, 0, 0, 0]),
            Some(Op::Creat { path: 0x30 })
        );
        assert_eq!(
            decode(libc::SYS_open as u64, [0x40, 0, 0, 0, 0, 0]),
            Some(Op::Open {
                dirfd: AT_FDCWD,
                path: 0x40,
                flags: Flags::Value(0)
            })
        );
    }
}
