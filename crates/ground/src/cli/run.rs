use std::collections::HashMap;
use std::path::Path;

use clap::{Args, Parser, Subcommand};

use super::doctor::{default_checks, format_report, run_checks};
use super::engine::{ManifestsCommand, SelectArgs, cmd_manifests, cmd_select};
use super::install_service::{InstallServiceOptions, built_daemon_entry, install_service};
use super::status::{format_status, summaries};
use super::trace::{TraceArgs, cmd_trace};
use super::trace_tests::{TraceTestsArgs, cmd_trace_tests};
use crate::adapters::{NpmRegistry, RestGitHub, SystemClock};
use crate::config::RepoConfig;
use crate::daemon::{ConsoleLogger, db_path, ground_home_from_env, load_repos, run_daemon};
use crate::lanes::release::{ShipDeps, run_ship_command};
use crate::state::StateStore;

pub const USAGE: &str = "usage: ground <command>

  status [repo]                 what ground has seen (asks groundd, else reads the state db)
  ship <repo> [--confirm] [--json]
                                walk a version PR to npm; dry run unless --confirm
  daemon                        run groundd in the foreground
  select --base <ref> [--manifests <dir>] [--root <dir>] [--format json|lines|vitest]
         [--global <glob>...] [--scoped <glob>...] [--always <glob>...]
                                print the tests a change requires
  manifests import <dir>        copy trace manifests into the store
  trace [--root <dir>] [--out <file>] [--test <path>] -- <command...>
                                run a command and record the files it reads
  trace-tests --runner vitest --package <dir> [--jobs N] [--out <dir>] [--filter <glob>]
              [-- <wrapper...>]
                                trace each test file of a package, no runner discovery
  doctor                        check the setup
  install-service               write the systemd user unit (does not enable it)
";

#[derive(Debug, Parser)]
#[command(
    name = "ground",
    version,
    about = "Delivery service for any GitHub repository",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum Command {
    /// What ground has seen (asks groundd, else reads the state db)
    Status {
        repo: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Walk a version PR to npm; dry run unless --confirm
    Ship(ShipArgs),
    /// Print the tests a change requires
    Select(SelectArgs),
    /// Manage trace manifests
    #[command(subcommand)]
    Manifests(ManifestsCommand),
    /// Run a command and record the files its process tree reads
    Trace(TraceArgs),
    /// Trace every test file of a package, one manifest each, without runner discovery
    TraceTests(TraceTestsArgs),
    /// Run groundd in the foreground
    Daemon,
    /// Check the setup
    Doctor,
    /// Write the systemd user unit (does not enable it)
    InstallService,
}

#[derive(Debug, Args, PartialEq, Eq)]
pub struct ShipArgs {
    pub repo: String,
    #[arg(long)]
    pub confirm: bool,
    #[arg(long)]
    pub json: bool,
}

fn out(s: &str) {
    println!("{s}");
}

fn find_config<'a>(configs: &'a [RepoConfig], name: &str) -> Result<&'a RepoConfig, String> {
    configs.iter().find(|c| c.repo == name).ok_or_else(|| {
        let known = configs
            .iter()
            .map(|c| c.repo.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "unknown repo \"{name}\" (configured: {})",
            if known.is_empty() { "none" } else { &known }
        )
    })
}

async fn cmd_status(home: &Path, repo: Option<String>, json: bool) -> Result<i32, String> {
    let mut rows = summaries(home).await?;
    if let Some(repo) = repo {
        rows.retain(|r| r.repo == repo);
    }
    if rows.is_empty() {
        eprintln!("no matching repositories");
        return Ok(1);
    }
    if json {
        out(&serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?);
    } else {
        format_status(&rows).iter().for_each(|l| out(l));
    }
    Ok(0)
}

async fn cmd_ship(home: &Path, args: ShipArgs) -> Result<i32, String> {
    let loaded = load_repos(home);
    let config = find_config(&loaded.configs, &args.repo)?;
    let clock = SystemClock;
    let store = StateStore::new(clock, db_path(home)).map_err(|e| e.to_string())?;
    let github = RestGitHub::new();
    let registry = NpmRegistry::new();
    let logger = ConsoleLogger::new(clock);
    let deps = ShipDeps {
        config,
        github: &github,
        registry: &registry,
        clock: &clock,
        logger: &logger,
        store: &store,
    };
    run_ship_command(&deps, args.confirm, args.json, out)
        .await
        .map_err(|e| e.to_string())
}

async fn cmd_doctor(home: &Path) -> i32 {
    let report = run_checks(&default_checks(home)).await;
    out(&format_report(&report));
    report.exit_code
}

fn cmd_install_service(home: &Path) -> Result<i32, String> {
    let (unit_path, command) = install_service(&InstallServiceOptions {
        daemon_entry: built_daemon_entry()?,
        config_home: None,
        ground_home: std::env::var_os("GROUND_HOME")
            .filter(|v| !v.is_empty())
            .map(|_| home.to_owned()),
    })
    .map_err(|e| e.to_string())?;
    out(&format!("wrote {}", unit_path.display()));
    out("to start it, run:");
    out(&format!("  {command}"));
    Ok(0)
}

/// Runs a parsed command line and returns the exit code. Errors print as
/// `ground: <message>` and exit 1; a usage error exits 2 (clap).
pub async fn run_cli(cli: Cli) -> i32 {
    let home = ground_home_from_env();
    let result = match cli.command {
        None => {
            eprint!("{USAGE}");
            Ok(0)
        }
        Some(Command::Status { repo, json }) => cmd_status(&home, repo, json).await,
        Some(Command::Ship(args)) => cmd_ship(&home, args).await,
        Some(Command::Select(args)) => cmd_select(&home, args),
        Some(Command::Manifests(cmd)) => cmd_manifests(&home, cmd),
        Some(Command::Trace(args)) => cmd_trace(args),
        Some(Command::TraceTests(args)) => cmd_trace_tests(&home, args),
        Some(Command::Daemon) => {
            let env: HashMap<String, String> = std::env::vars().collect();
            Ok(run_daemon(&env).await)
        }
        Some(Command::Doctor) => Ok(cmd_doctor(&home).await),
        Some(Command::InstallService) => cmd_install_service(&home),
    };
    result.unwrap_or_else(|msg| {
        eprintln!("ground: {msg}");
        1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ship_flags_and_status_arguments() {
        let c = Cli::try_parse_from(["ground", "ship", "a/b", "--confirm", "--json"]).unwrap();
        assert_eq!(
            c.command,
            Some(Command::Ship(ShipArgs {
                repo: "a/b".into(),
                confirm: true,
                json: true
            }))
        );
        let c = Cli::try_parse_from(["ground", "status", "a/b"]).unwrap();
        assert_eq!(
            c.command,
            Some(Command::Status {
                repo: Some("a/b".into()),
                json: false
            })
        );
        assert_eq!(Cli::try_parse_from(["ground"]).unwrap().command, None);
    }

    #[test]
    fn an_unknown_command_is_a_usage_error_with_exit_code_2() {
        let err = Cli::try_parse_from(["ground", "nope"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn find_config_names_the_configured_repos() {
        let err = find_config(&[], "x/y").unwrap_err();
        assert_eq!(err, "unknown repo \"x/y\" (configured: none)");
    }
}
