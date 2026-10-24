//! `ground trace-tests` end to end with a fake runner: a shell command per
//! test stands in for the real runner, and real `ground trace` children trace
//! it. Nothing depends on timing.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};

use ground::engine::manifest::Manifest;
use ground::engine::runner::{Job, Plan, Runner, filter_tests, trace_tests};
use ground::engine::store::ManifestStore;

/// Runs `sh -c <script>` per test, where the script is looked up by name.
struct Fake {
    root: PathBuf,
    tests: Vec<String>,
    /// test -> script
    scripts: Vec<(String, String)>,
}

impl Runner for Fake {
    fn list(&self) -> Result<Vec<String>, String> {
        Ok(self.tests.clone())
    }
    fn job(&self, test: &str) -> Result<Job, String> {
        let script = self
            .scripts
            .iter()
            .find(|(t, _)| t == test)
            .map(|(_, s)| s.clone())
            .ok_or_else(|| format!("no script for {test}"))?;
        Ok(Job {
            test: test.to_owned(),
            command: vec!["sh".into(), "-c".into(), script],
            env: vec![("GROUND_TRACE_TEST".into(), test.to_owned())],
            cwd: self.root.clone(),
        })
    }
}

struct Fx {
    _d: tempfile::TempDir,
    root: PathBuf,
    out: PathBuf,
}

fn fx() -> Fx {
    let d = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(d.path()).unwrap();
    fs::create_dir_all(root.join("pkg/src")).unwrap();
    for (f, body) in [
        ("pkg/src/a.test.ts", "a"),
        ("pkg/src/b.test.ts", "b"),
        ("pkg/src/lib.ts", "lib"),
        ("pkg/src/other.ts", "other"),
    ] {
        fs::write(root.join(f), body).unwrap();
    }
    let out = root.join("out");
    Fx { _d: d, root, out }
}

fn plan(fx: &Fx, jobs: usize) -> Plan {
    Plan {
        root: fx.root.clone(),
        out: fx.out.clone(),
        jobs,
        env_names: vec!["GROUND_TRACE_TEST".into()],
        ground: PathBuf::from(env!("CARGO_BIN_EXE_ground")),
    }
}

fn q(p: &Path) -> String {
    format!("'{}'", p.display())
}

fn load(fx: &Fx) -> Vec<Manifest> {
    ManifestStore::new(&fx.out).load_all().unwrap()
}

#[test]
fn one_manifest_per_test_keyed_by_path_and_readable_by_the_store() {
    let fx = fx();
    let r = &fx.root;
    let runner = Fake {
        root: r.clone(),
        tests: vec!["pkg/src/a.test.ts".into(), "pkg/src/b.test.ts".into()],
        scripts: vec![
            (
                "pkg/src/a.test.ts".into(),
                format!(
                    "cat {} {} >/dev/null",
                    q(&r.join("pkg/src/a.test.ts")),
                    q(&r.join("pkg/src/lib.ts"))
                ),
            ),
            (
                "pkg/src/b.test.ts".into(),
                format!("cat {} >/dev/null", q(&r.join("pkg/src/b.test.ts"))),
            ),
        ],
    };
    let tests = runner.list().unwrap();
    let report = trace_tests(&runner, &tests, &plan(&fx, 2)).unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.traced, 2);
    assert_eq!(report.sizes.len(), 2);
    let ms = load(&fx);
    let keys: Vec<_> = ms.iter().map(|m| m.test.as_str()).collect();
    assert_eq!(keys, ["pkg/src/a.test.ts", "pkg/src/b.test.ts"]);
    let a = ms.iter().find(|m| m.test.ends_with("a.test.ts")).unwrap();
    let paths: Vec<_> = a.files.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"pkg/src/lib.ts"), "{paths:?}");
    assert!(
        !paths.contains(&"pkg/src/other.ts"),
        "no discovery: {paths:?}"
    );
    assert!(a.env.contains_key("GROUND_TRACE_TEST"));
    assert!(
        !fx.out.join(".ground-work/0.json").exists(),
        "scratch is cleaned up"
    );
}

#[test]
fn a_failing_test_keeps_its_manifest_and_is_reported() {
    let fx = fx();
    let runner = Fake {
        root: fx.root.clone(),
        tests: vec!["pkg/src/a.test.ts".into()],
        scripts: vec![("pkg/src/a.test.ts".into(), "exit 3".into())],
    };
    let report = trace_tests(&runner, &runner.list().unwrap(), &plan(&fx, 1)).unwrap();
    assert!(!report.ok());
    assert_eq!(report.failing, ["pkg/src/a.test.ts"]);
    assert_eq!(load(&fx)[0].exit_code, 3);
}

#[test]
fn a_command_that_cannot_start_is_an_error_not_a_manifest() {
    let fx = fx();
    let runner = Fake {
        root: fx.root.clone(),
        tests: vec!["pkg/src/a.test.ts".into(), "pkg/src/b.test.ts".into()],
        scripts: vec![(
            "pkg/src/a.test.ts".into(),
            format!("cat {} >/dev/null", q(&fx.root.join("pkg/src/a.test.ts"))),
        )],
    };
    let report = trace_tests(&runner, &runner.list().unwrap(), &plan(&fx, 2)).unwrap();
    assert_eq!(report.traced, 1);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(report.errors[0].0, "pkg/src/b.test.ts");
    assert!(report.errors[0].1.contains("no script"));
    assert_eq!(load(&fx).len(), 1);
}

#[test]
fn the_adapters_scaffolding_under_the_work_area_is_not_an_input() {
    let fx = fx();
    let work = fx.out.join(".ground-work");
    fs::create_dir_all(&work).unwrap();
    fs::write(work.join("generated.config.mjs"), "export default {}").unwrap();
    let runner = Fake {
        root: fx.root.clone(),
        tests: vec!["pkg/src/a.test.ts".into()],
        scripts: vec![(
            "pkg/src/a.test.ts".into(),
            format!(
                "cat {} {} >/dev/null; test -e {}",
                q(&work.join("generated.config.mjs")),
                q(&fx.root.join("pkg/src/a.test.ts")),
                q(&work.join("not-there"))
            ),
        )],
    };
    let report = trace_tests(&runner, &runner.list().unwrap(), &plan(&fx, 1)).unwrap();
    assert_eq!(
        report.errors.len() + report.failing.len(),
        1,
        "test -e fails: {report:?}"
    );
    let m = &load(&fx)[0];
    assert!(
        m.files.iter().all(|f| !f.path.starts_with("out/")),
        "{:?}",
        m.files
    );
    assert!(
        m.absent.iter().all(|a| !a.starts_with("out/")),
        "{:?}",
        m.absent
    );
    assert!(m.files.iter().any(|f| f.path == "pkg/src/a.test.ts"));
}

#[test]
fn collapses_pnpm_packages_in_a_traced_run() {
    let fx = fx();
    let pkg = fx
        .root
        .join("node_modules/.pnpm/zod@3.25.76/node_modules/zod");
    fs::create_dir_all(pkg.join("dist")).unwrap();
    fs::write(pkg.join("package.json"), "{}").unwrap();
    fs::write(pkg.join("dist/a.js"), "a").unwrap();
    let runner = Fake {
        root: fx.root.clone(),
        tests: vec!["pkg/src/a.test.ts".into()],
        scripts: vec![(
            "pkg/src/a.test.ts".into(),
            format!(
                "cat {} {} {} >/dev/null",
                q(&fx.root.join("pkg/src/a.test.ts")),
                q(&pkg.join("package.json")),
                q(&pkg.join("dist/a.js"))
            ),
        )],
    };
    let report = trace_tests(&runner, &runner.list().unwrap(), &plan(&fx, 1)).unwrap();
    assert!(report.ok(), "{report:?}");
    let m = &load(&fx)[0];
    assert_eq!(m.deps.len(), 1);
    assert_eq!(m.deps[0].dep, "zod@3.25.76/zod");
    assert_eq!(
        m.deps[0].blake3,
        ground::engine::trace::hash_package(&pkg).unwrap()
    );
    assert!(
        m.files.iter().all(|f| !f.path.contains("node_modules")),
        "{:?}",
        m.files
    );
}

#[test]
fn filter_picks_by_repo_relative_glob() {
    let t = vec![
        "pkg/src/a.test.ts".to_owned(),
        "pkg/src/deep/b.test.ts".into(),
    ];
    assert_eq!(
        filter_tests(t, Some("pkg/src/*.test.ts")).unwrap(),
        ["pkg/src/a.test.ts"]
    );
}

/// Real vitest, only when `GROUND_VITEST_PACKAGE` names an installed package
/// (a directory with vitest in its node_modules); `GROUND_VITEST_WRAPPER`
/// (whitespace-separated) is a program run in front of vitest for a package
/// whose tests need an environment of their own. Skipped otherwise: it needs
/// node and a dependency install this repository does not have.
#[test]
fn real_vitest_manifests_hold_no_discovery_directories() {
    let Ok(pkg) = std::env::var("GROUND_VITEST_PACKAGE") else {
        eprintln!("skipped: GROUND_VITEST_PACKAGE is not set");
        return;
    };
    let pkg = fs::canonicalize(pkg).unwrap();
    let root = ground::engine::trace::git_toplevel(&pkg).unwrap();
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("m");
    let work = out.join(".ground-work");
    let wrapper: Vec<String> = std::env::var("GROUND_VITEST_WRAPPER")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let runner = ground::engine::runner::Vitest::new(&pkg, &root, None, &work, wrapper).unwrap();
    let tests = runner.list().unwrap();
    assert!(!tests.is_empty());
    let one = vec![tests[0].clone()];
    let plan = Plan {
        root: root.clone(),
        out: out.clone(),
        jobs: 1,
        env_names: vec![],
        ground: PathBuf::from(env!("CARGO_BIN_EXE_ground")),
    };
    let report = trace_tests(&runner, &one, &plan).unwrap();
    assert!(report.ok(), "{report:?}");
    let m = ManifestStore::new(&out).load_all().unwrap().remove(0);
    let dirs: Vec<_> = m
        .files
        .iter()
        .filter(|f| root.join(&f.path).is_dir())
        .map(|f| f.path.as_str())
        .collect();
    let pkg_rel = pkg
        .strip_prefix(&root)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(
        dirs.iter()
            .all(|d| !d.starts_with(&format!("{pkg_rel}/src"))
                || *d == Path::new(&m.test).parent().unwrap().to_str().unwrap()),
        "discovery directories leaked into the manifest: {dirs:?}"
    );
}

#[test]
fn a_run_that_exits_zero_without_reading_its_test_leaves_no_manifest() {
    let fx = fx();
    let r = &fx.root;
    let a = "pkg/src/a.test.ts";
    // A first, complete trace exists; the second run finds no test.
    let good = Fake {
        root: r.clone(),
        tests: vec![a.into()],
        scripts: vec![(a.into(), format!("cat {} >/dev/null", q(&r.join(a))))],
    };
    let report = trace_tests(&good, &good.list().unwrap(), &plan(&fx, 1)).unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(load(&fx).len(), 1);
    let empty = Fake {
        root: r.clone(),
        tests: vec![a.into()],
        scripts: vec![(
            a.into(),
            format!("cat {} >/dev/null", q(&r.join("pkg/src/lib.ts"))),
        )],
    };
    let report = trace_tests(&empty, &empty.list().unwrap(), &plan(&fx, 1)).unwrap();
    assert_eq!(report.traced, 0, "{report:?}");
    assert_eq!(report.errors.len(), 1);
    assert!(
        report.errors[0].1.contains("never read"),
        "{:?}",
        report.errors
    );
    assert!(
        ManifestStore::new(&fx.out).load_all().unwrap().is_empty(),
        "the stale manifest of the earlier run is gone too"
    );
}
