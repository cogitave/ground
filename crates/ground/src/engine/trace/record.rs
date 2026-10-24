//! Turns the stream of observed file accesses into a manifest. Pure
//! bookkeeping (plus hashing at the end): it knows nothing about ptrace, so it
//! is testable on every platform.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::engine::manifest::{FileEntry, MANIFEST_VERSION, Manifest};

/// How the traced tree touched a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// Opened for reading (or read/write of a file that already existed).
    Read,
    /// Created or truncated: the tree is making this file, not consuming it.
    Write,
    /// Passed to `execve`/`execveat`.
    Exec,
    /// `stat`, `access`, `readlink`, `O_PATH`: only its existence or metadata.
    Probe,
}

/// One filtered syscall of the traced tree, already resolved to an absolute
/// path. `existed` and `is_dir` describe the path just before the syscall ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access {
    pub path: PathBuf,
    pub kind: AccessKind,
    pub existed: bool,
    pub is_dir: bool,
    /// The call examined the symlink itself (`lstat`, `readlink`, a dangling
    /// target), so what it saw is the link's text, not a file's content.
    pub link: bool,
}

/// What `ground trace` writes. `files` and `absent` are relative to `root`
/// (forward slashes); `exec` is relative when inside `root`, else absolute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceManifest {
    pub version: u32,
    /// Absolute, canonical root the paths are relative to.
    pub root: String,
    pub command: Vec<String>,
    pub exit_code: i32,
    /// Files under `root` the tree read, exec'd or probed (and that existed),
    /// minus those whose first access was a write. A directory that was
    /// opened is hashed over its sorted entry names.
    pub files: Vec<FileEntry>,
    /// Paths under `root` probed and not found.
    pub absent: Vec<String>,
    /// Resolved executables of every exec.
    pub exec: Vec<String>,
    /// Content hashes of `exec`: the toolchain fingerprint.
    #[serde(default)]
    pub toolchain: Vec<FileEntry>,
    /// The environment allow-list as it was (`null` = unset); the CLI fills it.
    #[serde(default)]
    pub env: BTreeMap<String, Option<String>>,
    /// `git rev-parse HEAD` of the checkout; the CLI fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Engine version that wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Repo-relative test file this manifest belongs to, when `--test` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

impl TraceManifest {
    /// The store's manifest type, for `test` (repo-relative).
    pub fn into_manifest(self, test: String) -> Manifest {
        Manifest {
            version: self.version,
            root: self.root,
            command: self.command,
            exit_code: self.exit_code,
            test,
            files: self.files,
            absent: self.absent,
            exec: self.exec,
            toolchain: self.toolchain,
            env: self.env,
            commit: self.commit,
            engine: self.engine,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum First {
    Input,
    Absent,
    Output,
}

/// Marker hashes that never equal a real one: the entry always selects.
const VOLATILE: &str = "volatile";
const MISSING: &str = "missing";

/// Identity of a regular file's bytes cheap enough to take at the moment of
/// the read: inode, size and both timestamps.
type StatKey = (u64, u64, i64, i64, i64, i64);

/// What a path looked like when the tree first read it. A manifest hashes the
/// path at the end of the run, so a file the tree (or a sibling) rewrote in the
/// meantime would be recorded as its end state while the test ran against the
/// start state; comparing with this catches that.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Snap {
    File(StatKey),
    /// A directory's listing hash.
    Dir(String),
}

#[derive(Debug, Clone)]
struct Seen {
    class: First,
    /// The content (or listing) the tree read.
    content: Option<Snap>,
    /// The link text it read.
    link: Option<String>,
}

#[cfg(unix)]
fn stat_key(m: &fs::Metadata) -> StatKey {
    use std::os::unix::fs::MetadataExt;
    (
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}

#[cfg(not(unix))]
fn stat_key(m: &fs::Metadata) -> StatKey {
    let t = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or((0, 0), |d| {
            (d.as_secs() as i64, i64::from(d.subsec_nanos()))
        });
    (0, m.len(), t.0, t.1, 0, 0)
}

fn snapshot(path: &Path) -> Option<Snap> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_file() {
        Some(Snap::File(stat_key(&meta)))
    } else if meta.is_dir() {
        hash_path(path).map(Snap::Dir)
    } else {
        None
    }
}

/// blake3 (hex) of a symlink's target text, tagged so it never collides with
/// a file hash.
pub fn hash_link(path: &Path) -> Option<String> {
    let target = fs::read_link(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"link\0");
    hasher.update(target.to_string_lossy().as_bytes());
    Some(hasher.finalize().to_hex().to_string())
}

/// The hash to record for what was read as `snap`: the end-of-run hash when
/// nothing moved underneath the reader, else a marker that always selects.
fn settle(snap: &Snap, path: &Path) -> String {
    match snap {
        Snap::File(key) => match fs::metadata(path) {
            Ok(m) if m.is_file() && stat_key(&m) == *key => {
                hash_path(path).unwrap_or_else(|| MISSING.into())
            }
            Ok(_) => VOLATILE.into(),
            Err(_) => MISSING.into(),
        },
        Snap::Dir(before) => match hash_path(path) {
            Some(now) if now == *before => now,
            Some(_) => VOLATILE.into(),
            None => MISSING.into(),
        },
    }
}

/// Accumulates accesses; `finish` hashes and emits the manifest.
pub struct Recorder {
    root: PathBuf,
    seen: HashMap<PathBuf, Seen>,
    exec: BTreeSet<String>,
    canon_dirs: HashMap<PathBuf, Option<PathBuf>>,
}

/// `node_modules/.vite`, `.cache` and `.turbo` (at any depth) hold caches the
/// toolchain rewrites on each run; they are not inputs.
fn is_tool_cache(rel: &Path) -> bool {
    let parts: Vec<_> = rel.components().map(|c| c.as_os_str()).collect();
    parts.iter().enumerate().any(|(i, c)| {
        *c == "node_modules"
            && parts
                .get(i + 1)
                .is_some_and(|n| *n == ".vite" || *n == ".cache" || *n == ".turbo")
    })
}

/// Lexical normalisation of an absolute path: drops `.`, resolves `..`.
pub fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl Recorder {
    /// `root` must be absolute; it is canonicalised when it exists.
    pub fn new(root: &Path) -> Recorder {
        let root = fs::canonicalize(root).unwrap_or_else(|_| normalise(root));
        Recorder {
            root,
            seen: HashMap::new(),
            exec: BTreeSet::new(),
            canon_dirs: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path relative to the root, if it lies under it. Tries the lexical
    /// path first, then the path with its parent directory canonicalised
    /// (an access through a symlinked prefix of the root).
    fn relative(&mut self, path: &Path) -> Option<PathBuf> {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return Some(rel.to_owned());
        }
        let parent = path.parent()?.to_owned();
        let name = path.file_name()?.to_owned();
        let canon = self
            .canon_dirs
            .entry(parent.clone())
            .or_insert_with(|| fs::canonicalize(&parent).ok())
            .clone()?;
        canon
            .join(name)
            .strip_prefix(&self.root)
            .ok()
            .map(Path::to_owned)
    }

    pub fn record(&mut self, access: &Access) {
        let Some(rel) = self.relative(&normalise(&access.path)) else {
            return;
        };
        if rel.as_os_str().is_empty() {
            return; // the root directory itself
        }
        // A tool's own cache is rewritten by every run (vitest's results file
        // holds durations), so its end-of-run hash can never match a later one.
        if is_tool_cache(&rel) {
            return;
        }
        // A probe of an existing directory (every ancestor gets stat'ed during
        // module resolution) says nothing about its contents.
        if access.kind == AccessKind::Probe && access.existed && access.is_dir {
            return;
        }
        let class = match access.kind {
            AccessKind::Write => First::Output,
            _ if !access.existed => First::Absent,
            _ => First::Input,
        };
        let abs = self.root.join(&rel);
        let seen = self.seen.entry(rel).or_insert(Seen {
            class,
            content: None,
            link: None,
        });
        // Only reads of a path whose first access found it there count; a
        // later write does not turn what was read into something else.
        if seen.class != First::Input || access.kind == AccessKind::Write || !access.existed {
            return;
        }
        if access.link {
            if seen.link.is_none() {
                seen.link = hash_link(&abs);
            }
        } else if seen.content.is_none() {
            seen.content = snapshot(&abs);
        }
    }

    /// An executable the tree ran, as the kernel resolved it (`/proc/<tid>/exe`).
    pub fn record_exec_binary(&mut self, path: &Path) {
        let path = normalise(path);
        let shown = match self.relative(&path) {
            Some(rel) if !rel.as_os_str().is_empty() => to_slash(&rel),
            _ => path.to_string_lossy().into_owned(),
        };
        self.exec.insert(shown);
    }

    /// Hashes every recorded input as it is now (the end of the run).
    pub fn finish(self, command: Vec<String>, exit_code: i32) -> TraceManifest {
        let mut files: BTreeMap<(String, bool), String> = BTreeMap::new();
        let mut absent = BTreeSet::new();
        for (rel, seen) in &self.seen {
            let abs = self.root.join(rel);
            match seen.class {
                First::Output => {}
                First::Absent => {
                    absent.insert(to_slash(rel));
                }
                First::Input => {
                    if let Some(snap) = &seen.content {
                        files.insert((to_slash(rel), false), settle(snap, &abs));
                    }
                    if let Some(before) = &seen.link {
                        let hash = match hash_link(&abs) {
                            Some(now) if now == *before => now,
                            Some(_) => VOLATILE.into(),
                            None => MISSING.into(),
                        };
                        files.insert((to_slash(rel), true), hash);
                    }
                }
            }
        }
        let toolchain = self
            .exec
            .iter()
            .map(|e| {
                let path = Path::new(e);
                let abs = if path.is_absolute() {
                    path.to_owned()
                } else {
                    self.root.join(path)
                };
                FileEntry::file(e.clone(), hash_path(&abs).unwrap_or_else(|| MISSING.into()))
            })
            .collect();
        TraceManifest {
            version: MANIFEST_VERSION,
            root: self.root.to_string_lossy().into_owned(),
            command,
            exit_code,
            files: files
                .into_iter()
                .map(|((path, link), blake3)| FileEntry { path, blake3, link })
                .collect(),
            absent: absent.into_iter().collect(),
            exec: self.exec.into_iter().collect(),
            toolchain,
            env: BTreeMap::new(),
            commit: None,
            engine: Some(crate::engine::ENGINE_VERSION.to_owned()),
            test: None,
        }
    }
}

fn to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// blake3 (hex) of a regular file's bytes, or of a directory's sorted entry
/// names tagged by type. None when it is gone or is neither (device, socket).
pub fn hash_path(path: &Path) -> Option<String> {
    let meta = fs::metadata(path).ok()?;
    if meta.is_file() {
        let mut hasher = blake3::Hasher::new();
        let mut file = fs::File::open(path).ok()?;
        io::copy(&mut file, &mut hasher).ok()?;
        Some(hasher.finalize().to_hex().to_string())
    } else if meta.is_dir() {
        let mut entries: Vec<(Vec<u8>, bool)> = fs::read_dir(path)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                (e.file_name().to_string_lossy().as_bytes().to_vec(), is_dir)
            })
            .collect();
        entries.sort();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"dir\0");
        for (name, is_dir) in entries {
            hasher.update(&[u8::from(is_dir)]);
            hasher.update(&name);
            hasher.update(&[0]);
        }
        Some(hasher.finalize().to_hex().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(path: &Path, kind: AccessKind, existed: bool, is_dir: bool) -> Access {
        Access {
            path: path.to_owned(),
            kind,
            existed,
            is_dir,
            link: false,
        }
    }

    fn lnk(path: &Path, existed: bool) -> Access {
        Access {
            link: true,
            ..acc(path, AccessKind::Probe, existed, false)
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(d.path()).unwrap();
        (d, root)
    }

    #[test]
    fn normalise_resolves_dots() {
        assert_eq!(normalise(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalise(Path::new("/a/b/")), PathBuf::from("/a/b"));
        assert_eq!(normalise(Path::new("/../a")), PathBuf::from("/a"));
    }

    #[test]
    fn a_read_file_is_hashed_at_the_end_and_a_probe_of_a_missing_path_is_absent() {
        let (_d, root) = setup();
        fs::write(root.join("f1"), b"one").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("f1"), AccessKind::Read, true, false));
        r.record(&acc(&root.join("nope"), AccessKind::Probe, false, false));
        let m = r.finish(vec!["x".into()], 0);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path, "f1");
        assert_eq!(m.files[0].blake3, blake3::hash(b"one").to_hex().to_string());
        assert_eq!(m.absent, vec!["nope".to_string()]);
    }

    #[test]
    fn a_file_rewritten_after_it_was_read_is_volatile_not_its_end_state() {
        let (_d, root) = setup();
        fs::write(root.join("f1"), b"one").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("f1"), AccessKind::Read, true, false));
        // A sibling rewrites it (different size, so the stat tuple moves
        // whatever the timestamp granularity) before the run ends.
        fs::write(root.join("f1"), b"three").unwrap();
        let m = r.finish(vec![], 0);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].blake3, VOLATILE);
        assert_ne!(
            m.files[0].blake3,
            blake3::hash(b"three").to_hex().to_string()
        );
    }

    #[test]
    fn a_directory_whose_listing_changed_during_the_run_is_volatile() {
        let (_d, root) = setup();
        fs::create_dir(root.join("d")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("d"), AccessKind::Read, true, true));
        fs::write(root.join("d/new"), b"x").unwrap();
        assert_eq!(r.finish(vec![], 0).files[0].blake3, VOLATILE);
    }

    #[test]
    fn a_probed_symlink_is_fingerprinted_by_its_target_text() {
        let (_d, root) = setup();
        fs::write(root.join("t1"), b"same").unwrap();
        fs::write(root.join("t2"), b"same").unwrap();
        std::os::unix::fs::symlink("t1", root.join("l")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&lnk(&root.join("l"), true));
        let a = r.finish(vec![], 0);
        assert_eq!(a.files.len(), 1);
        assert!(a.files[0].link && a.files[0].path == "l");
        // Retargeting to a file with identical bytes still changes it.
        fs::remove_file(root.join("l")).unwrap();
        std::os::unix::fs::symlink("t2", root.join("l")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&lnk(&root.join("l"), true));
        assert_ne!(r.finish(vec![], 0).files[0].blake3, a.files[0].blake3);
    }

    #[test]
    fn a_dangling_symlink_probe_is_kept_not_dropped() {
        let (_d, root) = setup();
        std::os::unix::fs::symlink("nowhere", root.join("l")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&lnk(&root.join("l"), true));
        let a = r.finish(vec![], 0);
        assert_eq!(a.files.len(), 1);
        assert!(a.files[0].link);
        fs::remove_file(root.join("l")).unwrap();
        std::os::unix::fs::symlink("elsewhere", root.join("l")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&lnk(&root.join("l"), true));
        assert_ne!(r.finish(vec![], 0).files[0].blake3, a.files[0].blake3);
    }

    #[test]
    fn a_followed_and_lstat_ed_symlink_records_both_content_and_link() {
        let (_d, root) = setup();
        fs::write(root.join("t"), b"x").unwrap();
        std::os::unix::fs::symlink("t", root.join("l")).unwrap();
        let mut r = Recorder::new(&root);
        r.record(&lnk(&root.join("l"), true));
        r.record(&acc(&root.join("l"), AccessKind::Read, true, false));
        let m = r.finish(vec![], 0);
        assert_eq!(m.files.len(), 2);
        assert!(m.files.iter().any(|f| f.link) && m.files.iter().any(|f| !f.link));
    }

    #[test]
    fn exec_binaries_are_hashed_into_the_toolchain_fingerprint() {
        let (_d, root) = setup();
        fs::write(root.join("tool"), b"v1").unwrap();
        let mut r = Recorder::new(&root);
        r.record_exec_binary(&root.join("tool"));
        r.record_exec_binary(&root.join("vanished"));
        let m = r.finish(vec![], 0);
        let tool = m.toolchain.iter().find(|e| e.path == "tool").unwrap();
        assert_eq!(tool.blake3, blake3::hash(b"v1").to_hex().to_string());
        let gone = m.toolchain.iter().find(|e| e.path == "vanished").unwrap();
        assert_eq!(gone.blake3, MISSING);
    }

    #[test]
    fn a_path_first_written_is_an_output_and_never_an_input() {
        let (_d, root) = setup();
        fs::write(root.join("out"), b"made").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("out"), AccessKind::Write, false, false));
        r.record(&acc(&root.join("out"), AccessKind::Read, true, false));
        let m = r.finish(vec![], 0);
        assert!(m.files.is_empty() && m.absent.is_empty());
    }

    #[test]
    fn a_read_before_a_write_stays_an_input() {
        let (_d, root) = setup();
        fs::write(root.join("f"), b"x").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("f"), AccessKind::Read, true, false));
        r.record(&acc(&root.join("f"), AccessKind::Write, true, false));
        assert_eq!(r.finish(vec![], 0).files.len(), 1);
    }

    #[test]
    fn a_path_probed_absent_is_not_hashed_even_if_created_and_read_later() {
        let (_d, root) = setup();
        fs::write(root.join("late"), b"x").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("late"), AccessKind::Probe, false, false));
        r.record(&acc(&root.join("late"), AccessKind::Read, true, false));
        let m = r.finish(vec![], 0);
        assert!(m.files.is_empty());
        assert_eq!(m.absent, vec!["late".to_string()]);
    }

    #[test]
    fn paths_outside_the_root_are_dropped_and_dotdot_is_resolved() {
        let (_d, root) = setup();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("f"), b"x").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(
            Path::new("/etc/hostname"),
            AccessKind::Read,
            true,
            false,
        ));
        r.record(&acc(&root.join("d/../f"), AccessKind::Read, true, false));
        let m = r.finish(vec![], 0);
        assert_eq!(m.files.len(), 1);
        assert_eq!(m.files[0].path, "f");
    }

    #[test]
    fn a_probed_directory_is_skipped_but_an_opened_one_is_hashed_by_listing() {
        let (_d, root) = setup();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("d/a"), b"x").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("d"), AccessKind::Probe, true, true));
        assert!(r.finish(vec![], 0).files.is_empty());

        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("d"), AccessKind::Read, true, true));
        let before = r.finish(vec![], 0);
        assert_eq!(before.files.len(), 1);
        fs::write(root.join("d/b"), b"y").unwrap();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("d"), AccessKind::Read, true, true));
        assert_ne!(r.finish(vec![], 0).files[0].blake3, before.files[0].blake3);
    }

    #[test]
    fn tool_caches_under_node_modules_are_never_recorded() {
        let (_d, root) = setup();
        let mut r = Recorder::new(&root);
        for p in [
            "packages/a/node_modules/.vite/vitest/h/results.json",
            "node_modules/.cache/x/y",
            "node_modules/.turbo/z",
        ] {
            r.record(&acc(&root.join(p), AccessKind::Read, true, false));
            r.record(&acc(&root.join(p), AccessKind::Probe, false, false));
        }
        let m = r.finish(vec![], 0);
        assert!(m.files.is_empty() && m.absent.is_empty());
    }

    #[test]
    fn an_input_deleted_before_the_end_is_dropped() {
        let (_d, root) = setup();
        let mut r = Recorder::new(&root);
        r.record(&acc(&root.join("gone"), AccessKind::Read, true, false));
        assert!(r.finish(vec![], 0).files.is_empty());
    }

    #[test]
    fn exec_binaries_are_relative_inside_the_root_and_absolute_outside() {
        let (_d, root) = setup();
        let mut r = Recorder::new(&root);
        r.record_exec_binary(&root.join("bin/tool"));
        r.record_exec_binary(Path::new("/usr/bin/dash"));
        let m = r.finish(vec![], 0);
        assert_eq!(m.exec, vec!["/usr/bin/dash".to_string(), "bin/tool".into()]);
    }

    #[test]
    fn it_converts_to_the_store_manifest() {
        let (_d, root) = setup();
        let m = Recorder::new(&root).finish(vec!["c".into()], 3);
        let s = m.into_manifest("a.test.ts".into());
        assert_eq!((s.exit_code, s.test.as_str()), (3, "a.test.ts"));
        assert_eq!(s.engine.as_deref(), Some(crate::engine::ENGINE_VERSION));
    }
}
