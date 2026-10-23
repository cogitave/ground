//! The `groundd` daemon: one watcher loop per configured repository and a
//! private unix socket that answers `status` and `events`.

mod client;
mod console_logger;
#[allow(clippy::module_inception)]
mod daemon;
mod main_loop;
mod paths;
mod repos;
mod server;
mod summary;
mod watcher;

#[cfg(test)]
mod test_support;

pub use client::{ClientError, socket_request};
pub use console_logger::ConsoleLogger;
pub use daemon::{DaemonDeps, DaemonError, RunningDaemon, start_daemon};
pub use main_loop::run_daemon;
pub use paths::{db_path, ground_home, ground_home_from_env, repos_dir, socket_path};
pub use repos::{LoadedRepos, load_repos};
pub use server::{Reply, RunningServer, ServerDeps, ServerError, handle_request, start_server};
pub use summary::{RecordedState, RepoSummary, recorded_state, run_signature, summarize_repo};
pub use watcher::{WatchStatus, Watcher};
