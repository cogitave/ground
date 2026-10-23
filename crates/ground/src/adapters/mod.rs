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
