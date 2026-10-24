//! Traces real commands. Nothing here depends on timing: every assertion is
//! about which files the traced tree opened, and a hang is caught by the test
//! runner's own per-test limit.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use ground::engine::trace::{TraceError, TraceManifest, TraceOptions, trace_command};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    Fixture { _dir: dir, root }
}

fn q(p: &Path) -> String {
    format!("'{}'", p.display())
}

fn trace_sh(root: &Path, script: &str) -> TraceManifest {
    trace(root, &["sh", "-c", script])
}

fn trace(root: &Path, command: &[&str]) -> TraceManifest {
    trace_command(&TraceOptions {
        root: root.to_owned(),
        command: command.iter().map(|s| (*s).to_owned()).collect(),
    })
    .expect("trace")
}

fn has_file(m: &TraceManifest, path: &str) -> bool {
    m.files.iter().any(|f| f.path == path)
}

#[test]
fn it_records_reads_in_grandchildren_probes_and_exit_code_but_not_its_own_outputs() {
    let fx = fixture();
    fs::write(fx.root.join("f1"), b"one").unwrap();
    fs::write(fx.root.join("f2"), b"two").unwrap();
    fs::create_dir(fx.root.join("d")).unwrap();
    let script = format!(
        "cd {} && cat f1 > /dev/null; ls d >/dev/null; test -e missing; \
         sh -c 'cat f2 >/dev/null'; echo made > made; cat made >/dev/null; exit 7",
        q(&fx.root)
    );
    let m = trace_sh(&fx.root, &script);

    assert_eq!(m.exit_code, 7);
    assert_eq!(m.root, fx.root.to_string_lossy());
    assert!(has_file(&m, "f1"), "{:?}", m.files);
    assert!(has_file(&m, "f2"), "read by a grandchild: {:?}", m.files);
    assert!(has_file(&m, "d"), "a listed directory: {:?}", m.files);
    assert!(m.absent.contains(&"missing".to_string()), "{:?}", m.absent);
    assert!(!has_file(&m, "made") && !m.absent.contains(&"made".to_string()));
    let f1 = m.files.iter().find(|f| f.path == "f1").unwrap();
    assert_eq!(f1.blake3, blake3::hash(b"one").to_hex().to_string());
    assert!(m.files.iter().all(|f| !f.path.starts_with('/')));
}

#[test]
fn it_fingerprints_the_toolchain_with_the_resolved_binaries_that_ran() {
    let fx = fixture();
    let m = trace_sh(&fx.root, "cat /dev/null; true");
    assert!(!m.exec.is_empty());
    assert!(m.exec.iter().all(|e| e.starts_with('/')), "{:?}", m.exec);
    // A shell was run; /bin/sh may be a symlink, so the resolved name differs.
    let sh = fs::canonicalize("/bin/sh").unwrap();
    assert!(
        m.exec.contains(&sh.to_string_lossy().into_owned()),
        "{:?}",
        m.exec
    );
}

#[test]
fn paths_outside_the_root_are_not_recorded() {
    let fx = fixture();
    let m = trace_sh(
        &fx.root,
        "cat /etc/passwd > /dev/null; test -e /nonexistent-ground-path",
    );
    assert!(m.files.is_empty(), "{:?}", m.files);
    assert!(m.absent.is_empty(), "{:?}", m.absent);
}

#[test]
fn relative_paths_with_dotdot_resolve_against_the_tracees_cwd() {
    let fx = fixture();
    fs::create_dir(fx.root.join("sub")).unwrap();
    fs::write(fx.root.join("top"), b"t").unwrap();
    let script = format!(
        "cd {} && cat ../top >/dev/null; test -e ../gone",
        q(&fx.root.join("sub"))
    );
    let m = trace_sh(&fx.root, &script);
    assert!(has_file(&m, "top"), "{:?}", m.files);
    assert!(m.absent.contains(&"gone".to_string()), "{:?}", m.absent);
}

#[test]
fn a_file_probed_absent_stays_absent_even_when_the_tree_then_creates_and_reads_it() {
    let fx = fixture();
    let script = format!(
        "cd {} && test -e late; echo x > late; cat late >/dev/null",
        q(&fx.root)
    );
    let m = trace_sh(&fx.root, &script);
    assert!(m.absent.contains(&"late".to_string()), "{:?}", m.absent);
    assert!(!has_file(&m, "late"));
}

#[test]
fn an_existing_file_opened_for_writing_without_truncation_is_still_an_input() {
    let fx = fixture();
    fs::write(fx.root.join("log"), b"old\n").unwrap();
    let script = format!("cd {} && echo more >> log", q(&fx.root));
    let m = trace_sh(&fx.root, &script);
    assert!(has_file(&m, "log"), "{:?}", m.files);
}

#[test]
fn an_existing_file_that_the_tree_truncates_is_an_output() {
    let fx = fixture();
    fs::write(fx.root.join("out"), b"old\n").unwrap();
    let script = format!("cd {} && echo new > out; cat out > /dev/null", q(&fx.root));
    let m = trace_sh(&fx.root, &script);
    assert!(!has_file(&m, "out"), "{:?}", m.files);
}

#[test]
fn a_command_killed_by_a_signal_exits_128_plus_the_signal() {
    let fx = fixture();
    let m = trace_sh(&fx.root, "kill -9 $$");
    assert_eq!(m.exit_code, 128 + 9);
}

#[test]
fn parallel_readers_started_by_xargs_are_all_captured() {
    let fx = fixture();
    let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
    for n in names {
        fs::write(fx.root.join(n), n.as_bytes()).unwrap();
    }
    let script = format!(
        "cd {} && printf 'a\\0b\\0c\\0d\\0e\\0f\\0g\\0h\\0' | xargs -0 -n 1 -P 4 cat > /dev/null",
        q(&fx.root)
    );
    let m = trace_sh(&fx.root, &script);
    assert_eq!(m.exit_code, 0);
    for n in names {
        assert!(has_file(&m, n), "{n} missing from {:?}", m.files);
    }
}

#[test]
fn background_jobs_that_outlive_their_parent_shell_line_are_still_followed() {
    let fx = fixture();
    for n in ["p", "q", "r"] {
        fs::write(fx.root.join(n), n.as_bytes()).unwrap();
    }
    let script = format!(
        "cd {}; cat p >/dev/null & cat q >/dev/null & cat r >/dev/null & wait",
        q(&fx.root)
    );
    let m = trace_sh(&fx.root, &script);
    for n in ["p", "q", "r"] {
        assert!(has_file(&m, n), "{n} missing from {:?}", m.files);
    }
}

/// Not a test of its own: when `GROUND_TRACE_THREADS_DIR` is set (by
/// `threads_in_one_process_are_captured`), this process reads every file in the
/// directory from its own thread. Run any other way it does nothing.
#[test]
fn threaded_reader_child() {
    let Some(dir) = std::env::var_os("GROUND_TRACE_THREADS_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let p = dir.join(format!("t{i}"));
            std::thread::spawn(move || fs::read(p).unwrap().len())
        })
        .collect();
    let total: usize = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(total, 8);
}

#[test]
fn threads_in_one_process_are_captured() {
    let fx = fixture();
    for i in 0..8 {
        fs::write(fx.root.join(format!("t{i}")), b"x").unwrap();
    }
    let exe = std::env::current_exe().unwrap();
    let dir_env = format!("GROUND_TRACE_THREADS_DIR={}", fx.root.display());
    let m = trace(
        &fx.root,
        &[
            "env",
            &dir_env,
            exe.to_str().unwrap(),
            "--exact",
            "threaded_reader_child",
            "--test-threads=1",
        ],
    );
    assert_eq!(m.exit_code, 0);
    for i in 0..8 {
        assert!(
            has_file(&m, &format!("t{i}")),
            "t{i} missing from {:?}",
            m.files
        );
    }
}

#[test]
fn a_command_that_does_not_exist_is_a_setup_error_not_a_manifest() {
    let fx = fixture();
    let err = trace_command(&TraceOptions {
        root: fx.root.clone(),
        command: vec!["ground-no-such-command-xyz".into()],
    })
    .unwrap_err();
    assert!(matches!(err, TraceError::Setup(_)), "{err}");
}

#[test]
fn an_empty_command_is_refused() {
    let fx = fixture();
    let err = trace_command(&TraceOptions {
        root: fx.root.clone(),
        command: vec![],
    })
    .unwrap_err();
    assert!(matches!(err, TraceError::Setup(_)));
}

#[test]
fn the_trace_ends_when_the_root_exits_and_kills_what_it_left_running() {
    let fx = fixture();
    let pidfile = fx.root.join("daemon.pid");
    // A "dev server": detached, long-running, still alive when the root exits.
    // An untraced run returns at the root's exit; so must the traced one.
    let script = format!(
        "sleep 3600 </dev/null >/dev/null 2>&1 & echo $! > {}; exit 3",
        q(&pidfile)
    );
    let m = trace_sh(&fx.root, &script);
    assert_eq!(m.exit_code, 3);
    let pid: u32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the leftover process {pid} is still there"
    );
}

#[test]
fn a_sigtrap_the_tracee_raises_itself_is_delivered_not_swallowed() {
    let fx = fixture();
    // The shell's default action for SIGTRAP is to die by it.
    let m = trace_sh(&fx.root, "kill -5 $$; exit 0");
    assert_eq!(m.exit_code, 128 + 5);
}

#[test]
fn a_probed_dangling_symlink_is_recorded_by_its_target_text() {
    let fx = fixture();
    std::os::unix::fs::symlink("nowhere", fx.root.join("l")).unwrap();
    let script = format!("cd {} && readlink l >/dev/null; test -L l", q(&fx.root));
    let m = trace_sh(&fx.root, &script);
    let l: Vec<_> = m.files.iter().filter(|f| f.path == "l").collect();
    assert_eq!(l.len(), 1, "{:?}", m.files);
    assert!(l[0].link);
    assert_ne!(l[0].blake3, "missing");
}

#[test]
fn an_input_the_tree_rewrites_after_reading_it_is_volatile() {
    let fx = fixture();
    fs::write(fx.root.join("f"), b"before").unwrap();
    // Read, then rewrite to different content: the end state is not what ran.
    let script = format!(
        "cd {} && cat f >/dev/null; echo a-longer-replacement > f",
        q(&fx.root)
    );
    let m = trace_sh(&fx.root, &script);
    let f = m.files.iter().find(|f| f.path == "f").expect("f recorded");
    assert_eq!(f.blake3, "volatile");
}

#[test]
fn the_toolchain_fingerprint_hashes_the_binaries_that_ran() {
    let fx = fixture();
    let m = trace_sh(&fx.root, "true");
    assert!(!m.toolchain.is_empty());
    for e in &m.toolchain {
        assert!(
            e.blake3.len() == 64 && e.blake3.bytes().all(|b| b.is_ascii_hexdigit()),
            "{e:?}"
        );
    }
    assert!(
        m.exec
            .iter()
            .all(|x| m.toolchain.iter().any(|e| &e.path == x))
    );
}
