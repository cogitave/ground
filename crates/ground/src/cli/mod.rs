//! The `ground` command line.

mod doctor;
mod engine;
mod install_service;
mod run;
mod status;
mod trace;

pub use doctor::{
    Check, CheckResult, DoctorReport, DoctorRow, DoctorStatus, default_checks, format_report,
    run_checks,
};
pub use engine::{Format, ManifestsCommand, NOTHING_SELECTED, SelectArgs};
pub use install_service::{
    InstallServiceOptions, built_daemon_entry, install_service, render_unit,
};
pub use run::{Cli, Command, USAGE, run_cli};
pub use status::{format_status, summaries};
pub use trace::{TraceArgs, cmd_trace};
