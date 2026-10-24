//! `ground trace-tests`: records one manifest per test file of a package.
//!
//! A runner adapter ([`Runner`]) lists a package's test files with the
//! runner's own enumeration and builds, per file, the command that runs that
//! one file with **no discovery**. A runner that walks the package's
//! directories to find tests would leave every directory it listed in every
//! manifest, and any file added anywhere would then invalidate all of them
//! (34 of the 43 select-everything commits in the namzu measurement). Each file
//! is traced in its own `ground trace` process, so tracers never share a
//! process and a test's environment and working directory are its own.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::manifest::{Manifest, normalize_path};
use super::store::ManifestStore;

/// One test file to trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Repo-relative path the manifest is keyed by.
    pub test: String,
    /// The command (program first) that runs exactly this file.
    pub command: Vec<String>,
    /// Environment set for the command, on top of the inherited one.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

/// A test runner the engine knows how to drive.
pub trait Runner: Sync {
    /// The test files, as the runner itself enumerates them, repo-relative.
    fn list(&self) -> Result<Vec<String>, String>;
    /// The command that runs exactly `test` without discovering any other.
    fn job(&self, test: &str) -> Result<Job, String>;
}

/// How a batch of traces runs.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Repository root: manifest paths are relative to it.
    pub root: PathBuf,
    /// The manifest directory (a store `ground select` can read directly).
    pub out: PathBuf,
    pub jobs: usize,
    /// Environment variables recorded into every manifest (`ground trace --env`).
    pub env_names: Vec<String>,
    /// The `ground` binary that runs each trace.
    pub ground: PathBuf,
}

/// What a batch did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub traced: usize,
    /// Tests whose run exited non-zero (their manifest says so; select
    /// always picks them).
    pub failing: Vec<String>,
    /// Tests for which no manifest could be made, with why.
    pub errors: Vec<(String, String)>,
    /// Size on disk of each manifest written.
    pub sizes: Vec<u64>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.failing.is_empty() && self.errors.is_empty()
    }
}

fn tail(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    let start = s.len().saturating_sub(600);
    let start = (start..s.len())
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s[start..].to_owned()
}

/// Traces one job in a `ground trace` child and stores its manifest.
fn trace_one(job: &Job, plan: &Plan, work: &Path, n: usize) -> Result<(Manifest, u64), String> {
    let tmp = work.join(format!("{n}.json"));
    let _ = std::fs::remove_file(&tmp);
    let mut cmd = Command::new(&plan.ground);
    cmd.arg("trace")
        .arg("--root")
        .arg(&plan.root)
        .args(["--test", &job.test])
        .arg("--out")
        .arg(&tmp);
    for name in &plan.env_names {
        cmd.args(["--env", name]);
    }
    cmd.arg("--").args(&job.command);
    cmd.current_dir(&job.cwd).envs(job.env.iter().cloned());
    let out = cmd
        .output()
        .map_err(|e| format!("cannot start ground: {e}"))?;
    let bytes = std::fs::read(&tmp).map_err(|_| {
        format!(
            "no manifest was written ({}): {}",
            out.status,
            tail(&out.stderr)
        )
    })?;
    let _ = std::fs::remove_file(&tmp);
    let mut m = Manifest::parse(&bytes)?;
    // The adapter's own scaffolding (a generated config) lives under `work`;
    // it is not an input of the test and will not exist at selection time.
    let scaffold = |p: &str| {
        let abs = Path::new(&m.root).join(normalize_path(p));
        abs.starts_with(work)
    };
    let (files, absent) = (
        m.files
            .iter()
            .filter(|f| !scaffold(&f.path))
            .cloned()
            .collect(),
        m.absent.iter().filter(|a| !scaffold(a)).cloned().collect(),
    );
    m.files = files;
    m.absent = absent;
    let store = ManifestStore::new(&plan.out);
    store.put(&m).map_err(|e| e.to_string())?;
    let size = std::fs::metadata(store.path_of(&m.test)).map_or(0, |md| md.len());
    Ok((m, size))
}

/// Traces every test in `tests` with at most `plan.jobs` running at once.
/// A test whose trace fails is reported, never skipped silently, and never
/// leaves a manifest behind that was not written whole.
pub fn trace_tests(runner: &dyn Runner, tests: &[String], plan: &Plan) -> Result<Report, String> {
    let work = plan.out.join(".ground-work");
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let work = std::fs::canonicalize(&work).map_err(|e| e.to_string())?;
    let next = AtomicUsize::new(0);
    let report = Mutex::new(Report::default());
    let workers = plan.jobs.clamp(1, tests.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let n = next.fetch_add(1, Ordering::SeqCst);
                    let Some(test) = tests.get(n) else { break };
                    let result = runner
                        .job(test)
                        .and_then(|job| trace_one(&job, plan, &work, n));
                    let mut r = report.lock().unwrap_or_else(|e| e.into_inner());
                    match result {
                        Ok((m, size)) => {
                            r.traced += 1;
                            r.sizes.push(size);
                            if m.exit_code != 0 {
                                r.failing.push(test.clone());
                            }
                        }
                        Err(e) => r.errors.push((test.clone(), e)),
                    }
                }
            });
        }
    });
    let _ = std::fs::remove_dir(&work);
    let mut r = report.into_inner().unwrap_or_else(|e| e.into_inner());
    r.failing.sort();
    r.errors.sort();
    r.sizes.sort_unstable();
    Ok(r)
}

/// Keeps the tests whose repo-relative path matches `glob`.
pub fn filter_tests(tests: Vec<String>, glob: Option<&str>) -> Result<Vec<String>, String> {
    let Some(glob) = glob else { return Ok(tests) };
    let g = globset::GlobBuilder::new(glob)
        .literal_separator(true)
        .build()
        .map_err(|e| format!("bad --filter {glob:?}: {e}"))?
        .compile_matcher();
    let set: BTreeSet<String> = tests.into_iter().filter(|t| g.is_match(t)).collect();
    Ok(set.into_iter().collect())
}

// ---------------------------------------------------------------- vitest --

/// vitest, driven through a generated config.
///
/// The config imports the package's own (so plugins, aliases, setup files and
/// relative paths behave exactly as in a normal run) and overrides
/// `test.include` with the one literal file named by `GROUND_TRACE_TEST`, so
/// vitest does not walk the package to find tests. It lives outside the
/// package, in the output directory's work area, and `--root` pins the
/// package as the project root.
pub struct Vitest {
    package: PathBuf,
    root: PathBuf,
    node: String,
    entry: PathBuf,
    base_config: Option<PathBuf>,
    generated: PathBuf,
    wrapper: Vec<String>,
}

const CONFIG_EXTS: &[&str] = &["ts", "mts", "cts", "js", "mjs", "cjs"];

fn find_config(package: &Path) -> Option<PathBuf> {
    ["vitest.config", "vite.config"]
        .iter()
        .flat_map(|stem| CONFIG_EXTS.iter().map(move |e| format!("{stem}.{e}")))
        .map(|n| package.join(n))
        .find(|p| p.is_file())
}

/// `node_modules/vitest`'s CLI entry, from its own `bin` field, searching the
/// package and every ancestor.
fn find_vitest_entry(package: &Path) -> Result<PathBuf, String> {
    for dir in package.ancestors() {
        let pj = dir.join("node_modules/vitest/package.json");
        let Ok(text) = std::fs::read_to_string(&pj) else {
            continue;
        };
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", pj.display()))?;
        let bin = match &v["bin"] {
            serde_json::Value::String(s) => Some(s.as_str()),
            b => b["vitest"].as_str(),
        };
        let bin = bin.ok_or_else(|| format!("{} declares no bin.vitest", pj.display()))?;
        let real = std::fs::canonicalize(&pj).map_err(|e| format!("{}: {e}", pj.display()))?;
        return Ok(real.parent().unwrap_or(Path::new("/")).join(bin));
    }
    Err(format!(
        "vitest is not installed for {} (no node_modules/vitest above it); install dependencies first",
        package.display()
    ))
}

/// The generated config's text. Public so its contract is testable.
pub fn vitest_config_source(base: Option<&Path>) -> String {
    let import = match base {
        Some(p) => format!(
            "import base from {}\n",
            serde_json::to_string(&p.to_string_lossy()).unwrap_or_default()
        ),
        None => "const base = {}\n".to_owned(),
    };
    format!(
        "// Generated by `ground trace-tests`: the package's config with test.include pinned\n\
         // to the single file in GROUND_TRACE_TEST, so vitest walks no directories to find tests.\n\
         {import}\
         const file = process.env.GROUND_TRACE_TEST\n\
         if (!file) throw new Error('GROUND_TRACE_TEST is not set')\n\
         const env = {{ command: 'serve', mode: 'test', isSsrBuild: false, isPreview: false }}\n\
         const resolved = (typeof base === 'function' ? await base(env) : await base) ?? {{}}\n\
         if (resolved.test?.projects || resolved.test?.workspace) {{\n\
         \tthrow new Error('ground trace-tests cannot pin the include of a multi-project vitest config')\n\
         }}\n\
         const literal = file.replace(/[\\\\*?[\\]{{}}()!]/g, '\\\\$&')\n\
         export default {{ ...resolved, test: {{ ...resolved.test, include: [literal] }} }}\n"
    )
}

impl Vitest {
    /// `package` holds the tests; `work` is where the generated config goes;
    /// `wrapper` (possibly empty) is a program run in front of vitest, for a
    /// repository whose tests need an environment of their own.
    pub fn new(
        package: &Path,
        root: &Path,
        config: Option<&Path>,
        work: &Path,
        wrapper: Vec<String>,
    ) -> Result<Vitest, String> {
        let package =
            std::fs::canonicalize(package).map_err(|e| format!("{}: {e}", package.display()))?;
        let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
        if !package.starts_with(&root) {
            return Err(format!(
                "{} is not inside {}",
                package.display(),
                root.display()
            ));
        }
        let entry = find_vitest_entry(&package)?;
        let base_config = match config {
            Some(c) => Some(std::fs::canonicalize(c).map_err(|e| format!("{}: {e}", c.display()))?),
            None => find_config(&package),
        };
        std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
        let work = std::fs::canonicalize(work).map_err(|e| e.to_string())?;
        let generated = work.join("vitest.config.mjs");
        std::fs::write(&generated, vitest_config_source(base_config.as_deref()))
            .map_err(|e| format!("{}: {e}", generated.display()))?;
        Ok(Vitest {
            package,
            root,
            node: "node".into(),
            entry,
            base_config,
            generated,
            wrapper,
        })
    }

    fn vitest(&self, args: &[&str]) -> Vec<String> {
        let mut v = vec![self.node.clone(), self.entry.to_string_lossy().into_owned()];
        v.extend(args.iter().map(|a| (*a).to_owned()));
        v
    }

    fn rel_to_root(&self, abs: &Path) -> Result<String, String> {
        let abs = std::fs::canonicalize(abs).unwrap_or_else(|_| abs.to_owned());
        abs.strip_prefix(&self.root)
            .map(|r| normalize_path(&r.to_string_lossy()))
            .map_err(|_| format!("{} lies outside the repository root", abs.display()))
    }
}

/// The files in `vitest list --filesOnly --json` output (a JSON array of
/// `{file}`), tolerating text printed before it.
pub fn parse_vitest_list(stdout: &str) -> Result<Vec<String>, String> {
    let start = stdout
        .char_indices()
        .filter(|(_, c)| *c == '[')
        .map(|(i, _)| i)
        .find(|&i| serde_json::from_str::<Vec<serde_json::Value>>(&stdout[i..]).is_ok())
        .ok_or_else(|| {
            format!(
                "vitest list printed no JSON array: {}",
                tail(stdout.as_bytes())
            )
        })?;
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&stdout[start..]).map_err(|e| e.to_string())?;
    rows.iter()
        .map(|r| {
            r["file"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("vitest list row has no file: {r}"))
        })
        .collect()
}

impl Runner for Vitest {
    fn list(&self) -> Result<Vec<String>, String> {
        let root = self.package.to_string_lossy().into_owned();
        let mut args = vec!["list", "--filesOnly", "--json", "--root", root.as_str()];
        let cfg = self
            .base_config
            .as_ref()
            .map(|c| c.to_string_lossy().into_owned());
        if let Some(c) = &cfg {
            args.extend(["--config", c.as_str()]);
        }
        let argv = self.vitest(&args);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(&self.package)
            .output()
            .map_err(|e| format!("cannot run vitest list: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "vitest list failed ({}): {}",
                out.status,
                tail(&out.stderr)
            ));
        }
        let files = parse_vitest_list(&String::from_utf8_lossy(&out.stdout))?;
        let set: BTreeSet<String> = files
            .iter()
            .map(|f| self.rel_to_root(Path::new(f)))
            .collect::<Result<_, _>>()?;
        Ok(set.into_iter().collect())
    }

    fn job(&self, test: &str) -> Result<Job, String> {
        let abs = self.root.join(test);
        let in_package = abs
            .strip_prefix(&self.package)
            .map_err(|_| format!("{test} is outside the package {}", self.package.display()))?;
        let pkg_rel = in_package.to_string_lossy().replace('\\', "/");
        let package = self.package.to_string_lossy().into_owned();
        let config = self.generated.to_string_lossy().into_owned();
        let mut command = self.wrapper.clone();
        command.extend(self.vitest(&["run", "--root", &package, "--config", &config]));
        Ok(Job {
            test: test.to_owned(),
            command,
            env: vec![("GROUND_TRACE_TEST".into(), pkg_rel)],
            cwd: self.package.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_is_read_past_any_text_before_the_json() {
        let out = "warning: [x] something\n[\n  {\"file\": \"/r/a.test.ts\"},\n  {\"file\": \"/r/b.test.ts\"}\n]\n";
        assert_eq!(
            parse_vitest_list(out).unwrap(),
            ["/r/a.test.ts", "/r/b.test.ts"]
        );
        assert_eq!(parse_vitest_list("[]").unwrap(), Vec::<String>::new());
        assert!(parse_vitest_list("no json").is_err());
        assert!(parse_vitest_list("[{\"name\":1}]").is_err());
    }

    #[test]
    fn the_generated_config_pins_one_literal_include_and_refuses_projects() {
        let src = vitest_config_source(Some(Path::new("/p/vitest.config.ts")));
        assert!(src.contains("import base from \"/p/vitest.config.ts\""));
        assert!(src.contains("include: [literal]"));
        assert!(src.contains("GROUND_TRACE_TEST"));
        assert!(src.contains("projects"));
        assert!(vitest_config_source(None).contains("const base = {}"));
    }

    #[test]
    fn filter_keeps_matching_tests_sorted() {
        let t = vec![
            "b/x.test.ts".to_owned(),
            "a/y.test.ts".into(),
            "a/z.spec.ts".into(),
        ];
        assert_eq!(
            filter_tests(t.clone(), Some("a/**/*.test.ts")).unwrap(),
            ["a/y.test.ts"]
        );
        assert_eq!(filter_tests(t, None).unwrap().len(), 3);
        assert!(filter_tests(vec![], Some("[")).is_err());
    }
}
