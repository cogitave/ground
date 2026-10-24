//! `ground trace`.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;

use crate::engine::trace::{TraceOptions, git_toplevel, trace_command};

#[derive(Debug, Args, PartialEq, Eq)]
pub struct TraceArgs {
    /// Directory whose files are recorded (default: the git toplevel of the cwd)
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Write the manifest here (default: print it on stdout, after the command's own output)
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Repo-relative test file the manifest belongs to; makes it loadable by `ground select`
    #[arg(long)]
    pub test: Option<String>,
    /// Environment variable whose value is part of the trace (repeatable). A
    /// manifest records only the variables named here, and `ground select`
    /// selects everything when one now differs.
    #[arg(long = "env", value_name = "NAME")]
    pub env: Vec<String>,
    /// The command to run under the tracer
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}

/// `git rev-parse HEAD` in `root`, if it is a checkout with a commit.
fn head_commit(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!id.is_empty()).then_some(id)
}

/// Runs the command and returns its exit code. The manifest is written even
/// when the command fails (its `exit_code` says so); a trace that cannot run is
/// an error, never a partial manifest.
pub fn cmd_trace(args: TraceArgs) -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = match args.root {
        Some(r) => r,
        None => git_toplevel(&cwd).map_err(|e| e.to_string())?,
    };
    let mut manifest = trace_command(&TraceOptions {
        root,
        command: args.command,
    })
    .map_err(|e| e.to_string())?;
    manifest.test = args.test;
    manifest.env = args
        .env
        .iter()
        .map(|name| (name.clone(), std::env::var(name).ok()))
        .collect();
    manifest.commit = head_commit(Path::new(&manifest.root));
    let code = manifest.exit_code;
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    match args.out {
        Some(path) => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        None => println!("{json}"),
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    #[test]
    fn the_command_follows_a_double_dash_and_keeps_its_own_flags() {
        let c = Cli::try_parse_from([
            "ground", "trace", "--root", "/r", "--out", "m.json", "--", "sh", "-c", "cat f",
        ])
        .unwrap();
        let Some(Command::Trace(a)) = c.command else {
            panic!("not a trace command");
        };
        assert_eq!(a.root.as_deref(), Some(std::path::Path::new("/r")));
        assert_eq!(a.command, ["sh", "-c", "cat f"]);
    }

    #[test]
    fn env_names_are_repeatable_and_stay_before_the_command() {
        let c = Cli::try_parse_from([
            "ground", "trace", "--env", "TZ", "--env", "NODE_ENV", "--", "sh", "--env",
        ])
        .unwrap();
        let Some(Command::Trace(a)) = c.command else {
            panic!("not a trace command");
        };
        assert_eq!(a.env, ["TZ", "NODE_ENV"]);
        assert_eq!(a.command, ["sh", "--env"]);
    }

    #[test]
    fn the_head_commit_is_read_from_a_checkout_and_absent_elsewhere() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(super::head_commit(d.path()), None);
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(d.path())
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        assert_eq!(super::head_commit(d.path()), None, "no commit yet");
        run(&["commit", "-q", "--allow-empty", "-m", "x"]);
        let id = super::head_commit(d.path()).unwrap();
        assert_eq!(id.len(), 40);
    }

    #[test]
    fn a_trace_without_a_command_is_a_usage_error() {
        assert!(Cli::try_parse_from(["ground", "trace"]).is_err());
    }
}
