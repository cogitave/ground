//! Real implementations of the ports.

mod clock_system;
mod exec;
mod github_rest;
mod registry_npm;

pub use clock_system::SystemClock;
pub use exec::{Exec, SystemExec, token_from_gh};
pub use github_rest::{
    RawFile, RawJob, RawPr, RawRun, RestGitHub, gh_merge_args, map_file, map_job, map_pr, map_run,
    split_repo,
};
pub use registry_npm::{NpmRegistry, interpret_latest, parse_latest, registry_url};

/// One HTTP client for both adapters: a stalled connection must fail a poll,
/// not hang it (and the graceful stop waiting on it) forever. A pooled
/// connection is kept only within one poll's burst of requests: left idle
/// across a poll interval it was closed by the far end (or a NAT) often enough
/// that every second or third poll failed with "connection closed before
/// message completed".
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .pool_idle_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}
