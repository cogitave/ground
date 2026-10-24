//! Selection: which tests must run for a change (`docs/incremental.md`).
//! Fails closed: anything unreadable, missing or unexpected selects.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::Serialize;

use super::ENGINE_VERSION;
use super::manifest::{DEPS_FORMAT, DepEntry, FileEntry, Manifest, normalize_path};
use super::store::ManifestStore;

/// Changes to any of these select every test.
pub const DEFAULT_GLOBAL: &[&str] = &[
    "**/pnpm-lock.yaml",
    "**/package-lock.json",
    "**/yarn.lock",
    "**/bun.lock",
    "**/bun.lockb",
    "**/Cargo.lock",
    "**/pnpm-workspace.yaml",
    "**/.npmrc",
    "**/.yarnrc.yml",
    "**/bunfig.toml",
    "**/tsconfig*.json",
    "**/vitest.config.*",
    "**/vitest.workspace.*",
    "**/vite.config.*",
    ".github/workflows/**",
];

/// The lockfile glob that stops being global once every manifest records the
/// installed packages it read: install state is then checked package by
/// package, and a test that reads the lockfile itself has it as an input.
pub const RELAXED_LOCKFILE: &str = "**/pnpm-lock.yaml";

/// What counts as a test file when enumerating the repository.
pub const DEFAULT_TESTS: &[&str] = &[
    "**/*.test.{ts,tsx,js,jsx,mjs,cjs,mts,cts}",
    "**/*.spec.{ts,tsx,js,jsx,mjs,cjs,mts,cts}",
    "**/*.test-d.{ts,tsx,mts,cts}",
];

/// Code files in a directory like these are probably tests a runner could be
/// configured to run (`__tests__/foo.ts`, `test/foo.ts`).
const TEST_DIRS: &[&str] = &["__tests__", "__test__", "test", "tests"];
const CODE_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts"];

/// Reasons listed per test for changed inputs are capped at this many.
const MAX_INPUT_REASONS: usize = 5;

#[derive(Debug, Clone)]
pub struct SelectOptions {
    pub root: PathBuf,
    pub base: String,
    pub manifests: PathBuf,
    pub global: Vec<String>,
    pub always: Vec<String>,
    pub tests: Vec<String>,
    /// The caller named the test files itself (`--tests`), so the engine
    /// trusts the list. Otherwise a new file that looks like a test but
    /// matches no glob selects everything: it may be a test the runner runs
    /// that the default globs cannot list.
    pub tests_declared: bool,
    /// The caller listed the lockfile glob itself (`--global '**/pnpm-lock.yaml'`):
    /// the lockfile stays a global input even though manifests record installed
    /// packages.
    pub lockfile_global: bool,
}

impl SelectOptions {
    pub fn new(root: PathBuf, base: String, manifests: PathBuf) -> Self {
        Self {
            root,
            base,
            manifests,
            global: DEFAULT_GLOBAL.iter().map(|s| (*s).to_owned()).collect(),
            always: vec![],
            tests: DEFAULT_TESTS.iter().map(|s| (*s).to_owned()).collect(),
            tests_declared: false,
            lockfile_global: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selected {
    pub test: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub total: usize,
    pub selected: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub base: String,
    /// Set when everything was selected for a global reason.
    pub all: bool,
    pub all_reason: Option<String>,
    pub selected: Vec<Selected>,
    pub counts: Counts,
}

fn globset(patterns: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).map_err(|e| format!("bad glob {p:?}: {e}"))?);
    }
    b.build().map_err(|e| e.to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| normalize_path(&String::from_utf8_lossy(s)))
        .collect())
}

/// Paths changed since the merge base with `base`, plus uncommitted and
/// untracked ones. Deletions and both sides of a rename are included.
fn changed_paths(root: &Path, base: &str) -> Result<BTreeSet<String>, String> {
    let range = format!("{base}...HEAD");
    let mut set = BTreeSet::new();
    let diffs: [&[&str]; 2] = [
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--relative",
            &range,
        ],
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--relative",
            "HEAD",
        ],
    ];
    for args in diffs {
        set.extend(git(root, args)?);
    }
    set.extend(git(
        root,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )?);
    Ok(set)
}

/// Every path that differs from where the manifests were traced, from the
/// merge base, in the working tree or untracked, relative to the git toplevel
/// (not to `root`: a lockfile above a package root is still a global input).
fn changed_from_toplevel(
    root: &Path,
    base: &str,
    commits: &BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let mut set = BTreeSet::new();
    let range = format!("{base}...HEAD");
    let diff = |rest: &[&str]| -> Vec<String> {
        let mut v = vec!["diff", "--name-only", "-z", "--no-renames"];
        v.extend_from_slice(rest);
        v.into_iter().map(str::to_owned).collect()
    };
    let mut jobs = vec![diff(&[&range]), diff(&["HEAD"])];
    for c in commits {
        // `c` comes from a file on disk: keep it from being read as an option.
        if c.starts_with('-') {
            return Err(format!("manifest records a malformed commit {c:?}"));
        }
        jobs.push(diff(&[c, "HEAD", "--"]));
    }
    for args in jobs {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        set.extend(git(root, &args)?);
    }
    set.extend(git(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--full-name",
            "--",
            ":/",
        ],
    )?);
    Ok(set)
}

fn list_tests(root: &Path, patterns: &[String]) -> Result<Vec<String>, String> {
    let gs = globset(patterns)?;
    let files = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let set: BTreeSet<String> = files
        .into_iter()
        .filter(|f| gs.is_match(f) && root.join(f).is_file())
        .collect();
    Ok(set.into_iter().collect())
}

/// Maps a manifest path to where it lives now: absolute paths under the
/// manifest's root are re-based onto `root`; other absolute paths stay.
fn resolve(root: &Path, manifest_root: &str, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        match path.strip_prefix(manifest_root) {
            Ok(rel) if !manifest_root.is_empty() => root.join(rel),
            _ => path.to_owned(),
        }
    } else {
        root.join(p)
    }
}

/// The same hash the tracer records: file bytes, a directory's sorted entry
/// names (a directory is an input when the test walked it), or, for a `link`
/// entry, the symlink's target text.
fn hash_file(p: &Path, link: bool) -> Option<String> {
    if link {
        super::trace::hash_link(p)
    } else {
        super::trace::hash_path(p)
    }
}

/// Whether `rel` is a code file under a test-like directory.
fn looks_like_test(rel: &str) -> bool {
    let path = Path::new(rel);
    let code = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| CODE_EXTS.contains(&e));
    let dir = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .any(|c| TEST_DIRS.iter().any(|d| c.as_os_str() == *d));
    code && dir
}

/// `Path::exists` follows symlinks and reads every error as absence; this
/// counts a dangling symlink, or a path it may not stat, as existing.
fn exists_now(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(_) => true,
        Err(e) => !matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        ),
    }
}

fn everything(base: &str, tests: Vec<String>, reason: String, always_all: bool) -> Selection {
    let n = tests.len();
    Selection {
        base: base.to_owned(),
        all: always_all,
        all_reason: Some(reason.clone()),
        selected: tests
            .into_iter()
            .map(|test| Selected {
                test,
                reasons: vec![reason.clone()],
            })
            .collect(),
        counts: Counts {
            total: n,
            selected: n,
            skipped: 0,
        },
    }
}

pub fn select(opts: &SelectOptions) -> Selection {
    let root = &opts.root;
    let tests = match list_tests(root, &opts.tests) {
        Ok(t) => t,
        Err(e) => {
            return everything(
                &opts.base,
                vec![],
                format!("enumerating tests failed: {e}"),
                true,
            );
        }
    };
    let always = match globset(&opts.always) {
        Ok(g) => g,
        Err(e) => return everything(&opts.base, tests, e, true),
    };
    let changed = match changed_paths(root, &opts.base) {
        Ok(c) => c,
        Err(e) => {
            return everything(
                &opts.base,
                tests,
                format!("cannot compute changes: {e}"),
                true,
            );
        }
    };
    let manifests = match ManifestStore::new(&opts.manifests).load_all() {
        Ok(m) => m,
        Err(e) => return everything(&opts.base, tests, format!("manifests unusable: {e}"), true),
    };
    let by_test: HashMap<&str, &Manifest> =
        manifests.iter().map(|m| (m.test.as_str(), m)).collect();
    // A manifest of a test that no longer exists says nothing about this run.
    let relevant: Vec<&Manifest> = tests
        .iter()
        .filter_map(|t| by_test.get(t.as_str()).copied())
        .collect();

    if let Some(reason) = manifest_level_reason(&relevant) {
        return everything(&opts.base, tests, reason, true);
    }

    // The lockfile is global unless every manifest records installed packages
    // (a manifest written before that existed lists node_modules files only,
    // and a lockfile change then has no other way in). With no manifest at all
    // every test is selected anyway.
    let packages_recorded = !opts.lockfile_global
        && !relevant.is_empty()
        && relevant.iter().all(|m| m.deps_format >= DEPS_FORMAT);
    let global_patterns: Vec<String> = opts
        .global
        .iter()
        .filter(|g| !(packages_recorded && g.as_str() == RELAXED_LOCKFILE))
        .cloned()
        .collect();
    let global = match globset(&global_patterns) {
        Ok(g) => g,
        Err(e) => return everything(&opts.base, tests, e, true),
    };

    // Global inputs are diffed from the commits the manifests were traced at
    // as well as from the base: a lockfile that landed on the base after they
    // were recorded makes every one of them stale.
    let commits: BTreeSet<String> = relevant.iter().filter_map(|m| m.commit.clone()).collect();
    let global_changed = match changed_from_toplevel(root, &opts.base, &commits) {
        Ok(c) => c,
        Err(e) => {
            return everything(
                &opts.base,
                tests,
                format!("cannot compute changes: {e}"),
                true,
            );
        }
    };
    if let Some(p) = global_changed
        .iter()
        .chain(changed.iter())
        .find(|p| global.is_match(p.as_str()))
    {
        return everything(
            &opts.base,
            tests,
            format!("global input changed: {p}"),
            true,
        );
    }

    // Hash every distinct input and toolchain binary of every manifested test
    // once, in parallel.
    let mut to_hash: BTreeSet<(PathBuf, bool)> = BTreeSet::new();
    for m in &relevant {
        to_hash.extend(
            m.files
                .iter()
                .map(|f| (resolve(root, &m.root, &f.path), f.link)),
        );
        to_hash.extend(
            m.toolchain
                .iter()
                .map(|f| (resolve(root, &m.root, &f.path), false)),
        );
    }
    let hashes: HashMap<(PathBuf, bool), Option<String>> = to_hash
        .into_par_iter()
        .map(|k| {
            let h = hash_file(&k.0, k.1);
            (k, h)
        })
        .collect();
    let now = |m: &Manifest, e: &FileEntry| -> Option<&String> {
        hashes
            .get(&(resolve(root, &m.root, &e.path), e.link))
            .and_then(Option::as_ref)
    };

    for m in &relevant {
        let moved = m
            .toolchain
            .iter()
            .find(|e| !now(m, e).is_some_and(|h| h.eq_ignore_ascii_case(&e.blake3)));
        if let Some(e) = moved {
            let reason = format!("toolchain changed: {} (recorded for {})", e.path, m.test);
            return everything(&opts.base, tests, reason, true);
        }
    }

    // Installed packages: each recorded one must still be there. If the
    // install they were read from is gone or unreadable altogether nothing can
    // be trusted (the lockfile may not be global any more), so everything runs.
    let installed = match installed_packages(root, &relevant) {
        Ok(i) => i,
        Err(reason) => return everything(&opts.base, tests, reason, true),
    };

    // A test file the runner may run that the globs do not list can never be
    // selected by its own manifest; a new one means we cannot know.
    if !opts.tests_declared {
        let listed: BTreeSet<&str> = tests.iter().map(String::as_str).collect();
        let read_by_a_test: BTreeSet<&PathBuf> = hashes.keys().map(|(p, _)| p).collect();
        let unknown = changed.iter().find(|p| {
            looks_like_test(p)
                && !listed.contains(p.as_str())
                && root.join(p).is_file()
                && !read_by_a_test.contains(&root.join(p))
        });
        if let Some(p) = unknown {
            let reason = format!(
                "{p} looks like a test but matches none of the test globs and no test read it; \
                 the runner may run it (pass --tests to declare the list)"
            );
            return everything(&opts.base, tests, reason, true);
        }
    }

    let mut selected = Vec::new();
    for t in &tests {
        let mut reasons = Vec::new();
        match by_test.get(t.as_str()) {
            None => reasons.push("no manifest".to_owned()),
            Some(m) => {
                if m.exit_code != 0 {
                    reasons.push(format!("last traced run failed (exit {})", m.exit_code));
                }
                if changed.contains(t) {
                    reasons.push("test file changed".to_owned());
                }
                let mut input_reasons = BTreeMap::new();
                for f in &m.files {
                    let key = (f.path.clone(), f.link);
                    match now(m, f) {
                        Some(h) if h.eq_ignore_ascii_case(&f.blake3) => {}
                        Some(_) => {
                            input_reasons.insert(key, format!("input changed: {}", f.path));
                        }
                        None => {
                            input_reasons.insert(key, format!("input missing: {}", f.path));
                        }
                    }
                }
                reasons.extend(input_reasons.into_values().take(MAX_INPUT_REASONS));
                reasons.extend(
                    m.deps
                        .iter()
                        .filter_map(|d| match installed.get(&dep_path(root, m, d)) {
                            Some(Some(h))
                                if !d.blake3.is_empty() && h.eq_ignore_ascii_case(&d.blake3) =>
                            {
                                None
                            }
                            Some(Some(_)) => Some(format!("dependency changed: {}", d.dep)),
                            _ => Some(format!("dependency not installed: {}", d.dep)),
                        })
                        .take(MAX_INPUT_REASONS),
                );
                reasons.extend(
                    m.absent
                        .iter()
                        .filter(|a| exists_now(&resolve(root, &m.root, a)))
                        .take(MAX_INPUT_REASONS)
                        .map(|a| format!("absent path now exists: {a}")),
                );
            }
        }
        if always.is_match(t.as_str()) {
            reasons.push("matches --always".to_owned());
        }
        if !reasons.is_empty() {
            selected.push(Selected {
                test: t.clone(),
                reasons,
            });
        }
    }
    let counts = Counts {
        total: tests.len(),
        selected: selected.len(),
        skipped: tests.len() - selected.len(),
    };
    Selection {
        base: opts.base.clone(),
        all: false,
        all_reason: None,
        selected,
        counts,
    }
}

fn dep_path(root: &Path, m: &Manifest, d: &DepEntry) -> PathBuf {
    resolve(root, &m.root, &d.path())
}

/// The content hash of each distinct package the manifests recorded, or
/// `None` when it is not installed now: not a real directory (a link or a file
/// does not count), no `package.json`, or unreadable. `Err`
/// when a virtual store a manifest recorded cannot be listed (no install, or
/// no right to look), which fails closed to selecting everything.
fn installed_packages(
    root: &Path,
    manifests: &[&Manifest],
) -> Result<HashMap<PathBuf, Option<String>>, String> {
    let mut stores = BTreeSet::new();
    let mut deps = BTreeSet::new();
    for m in manifests {
        for d in &m.deps {
            stores.insert(resolve(root, &m.root, &d.store));
            deps.insert(dep_path(root, m, d));
        }
    }
    for s in &stores {
        if let Err(e) = std::fs::read_dir(s) {
            return Err(format!(
                "installed packages unreadable ({}: {e}); install dependencies first",
                s.display()
            ));
        }
    }
    Ok(deps
        .into_par_iter()
        .map(|p| {
            let dir = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
            let ok = dir && std::fs::metadata(p.join("package.json")).is_ok_and(|m| m.is_file());
            let hash = if ok {
                super::trace::hash_package(&p)
            } else {
                None
            };
            (p, hash)
        })
        .collect())
}

/// What the manifests alone say makes every one of them untrustworthy: an
/// engine that is not this one (or unrecorded), no toolchain or commit
/// recorded, or a declared environment variable that now differs.
fn manifest_level_reason(relevant: &[&Manifest]) -> Option<String> {
    for m in relevant {
        match m.engine.as_deref() {
            Some(v) if v == ENGINE_VERSION => {}
            Some(v) => {
                return Some(format!(
                    "engine version changed: manifest for {} was written by {v}, running {ENGINE_VERSION}",
                    m.test
                ));
            }
            None => {
                return Some(format!("manifest for {} records no engine version", m.test));
            }
        }
        if m.toolchain.is_empty() {
            return Some(format!(
                "manifest for {} records no toolchain fingerprint",
                m.test
            ));
        }
        if m.commit.is_none() {
            return Some(format!("manifest for {} records no commit", m.test));
        }
        for (name, recorded) in &m.env {
            if std::env::var(name).ok() != *recorded {
                return Some(format!(
                    "environment variable {name} differs from the traced run of {}",
                    m.test
                ));
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
