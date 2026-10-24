//! `ground trace-tests`.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};

use crate::engine::runner::{Plan, Report, Runner, Vitest, filter_tests, trace_tests};
use crate::engine::store::default_dir;
use crate::engine::trace::git_toplevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RunnerKind {
    Vitest,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct TraceTestsArgs {
    /// The test runner to drive
    #[arg(long, value_enum)]
    pub runner: RunnerKind,
    /// The package directory whose test files are traced
    #[arg(long)]
    pub package: PathBuf,
    /// Traces in parallel (default: cores minus four, at least one)
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Manifest directory (default $GROUND_HOME/manifests/<repo-key>, the store `ground select` reads)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Only trace tests whose repo-relative path matches this glob
    #[arg(long)]
    pub filter: Option<String>,
    /// Repository root (default: the git toplevel of the package)
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Runner config to wrap (default: the package's vitest.config.* or vite.config.*)
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Environment variable whose value is recorded in every manifest (repeatable)
    #[arg(long = "env", value_name = "NAME")]
    pub env: Vec<String>,
    /// A program (and arguments) run in front of the runner for each test, for
    /// a repository whose tests need an environment of their own
    #[arg(last = true)]
    pub wrapper: Vec<String>,
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .saturating_sub(4)
        .max(1)
}

fn human(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / f64::from(1 << 20))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

pub fn summary(r: &Report) -> String {
    let total: u64 = r.sizes.iter().sum();
    let p50 = r.sizes.get(r.sizes.len() / 2).copied().unwrap_or(0);
    let max = r.sizes.last().copied().unwrap_or(0);
    format!(
        "traced {} tests ({} failing, {} without a manifest); manifests {} total, p50 {}, max {}",
        r.traced,
        r.failing.len(),
        r.errors.len(),
        human(total),
        human(p50),
        human(max)
    )
}

pub fn cmd_trace_tests(home: &Path, args: TraceTestsArgs) -> Result<i32, String> {
    if !crate::engine::trace::supported() {
        return Err("tracing is unsupported on this platform".into());
    }
    let root = match &args.root {
        Some(r) => r.clone(),
        None => git_toplevel(&args.package).map_err(|e| e.to_string())?,
    };
    let root = std::fs::canonicalize(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    let out = args.out.clone().unwrap_or_else(|| default_dir(home, &root));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let work = out.join(".ground-work");
    let runner: Box<dyn Runner> = match args.runner {
        RunnerKind::Vitest => Box::new(Vitest::new(
            &args.package,
            &root,
            args.config.as_deref(),
            &work,
            args.wrapper.clone(),
        )?),
    };
    let tests = filter_tests(runner.list()?, args.filter.as_deref())?;
    if tests.is_empty() {
        return Err("no test files to trace".into());
    }
    let plan = Plan {
        root,
        out: out.clone(),
        jobs: args.jobs.unwrap_or_else(default_jobs),
        env_names: args.env,
        ground: std::env::current_exe().map_err(|e| e.to_string())?,
    };
    let report = trace_tests(runner.as_ref(), &tests, &plan)?;
    let _ = std::fs::remove_dir_all(&work);
    for t in &report.failing {
        eprintln!("ground: {t} failed while it was traced");
    }
    for (t, why) in &report.errors {
        eprintln!("ground: {t}: {why}");
    }
    println!("{}", summary(&report));
    println!("manifests in {}", out.display());
    Ok(i32::from(!report.ok()))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    #[test]
    fn flags_and_a_wrapper_after_the_double_dash() {
        let c = Cli::try_parse_from([
            "ground",
            "trace-tests",
            "--runner",
            "vitest",
            "--package",
            "packages/sdk",
            "--jobs",
            "8",
            "--out",
            "m",
            "--filter",
            "**/a*.test.ts",
            "--env",
            "TZ",
            "--",
            "sh",
            "wrap.sh",
        ])
        .unwrap();
        let Some(Command::TraceTests(a)) = c.command else {
            panic!("not trace-tests");
        };
        assert_eq!(a.jobs, Some(8));
        assert_eq!(a.filter.as_deref(), Some("**/a*.test.ts"));
        assert_eq!(a.env, ["TZ"]);
        assert_eq!(a.wrapper, ["sh", "wrap.sh"]);
    }

    #[test]
    fn runner_and_package_are_required_and_the_runner_is_checked() {
        assert!(Cli::try_parse_from(["ground", "trace-tests", "--package", "p"]).is_err());
        assert!(Cli::try_parse_from(["ground", "trace-tests", "--runner", "vitest"]).is_err());
        assert!(
            Cli::try_parse_from([
                "ground",
                "trace-tests",
                "--runner",
                "nope",
                "--package",
                "p"
            ])
            .is_err()
        );
    }

    #[test]
    fn the_summary_names_counts_and_sizes() {
        let r = super::Report {
            traced: 3,
            failing: vec!["a".into()],
            errors: vec![],
            sizes: vec![1024, 2048, 3 << 20],
        };
        let s = super::summary(&r);
        assert!(s.contains("traced 3 tests (1 failing, 0 without"), "{s}");
        assert!(s.contains("3.0 MB total") || s.contains("MB total"), "{s}");
    }
}
