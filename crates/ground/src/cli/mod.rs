//! The `ground` command line.

mod doctor;
mod install_service;
mod run;
mod status;

pub use doctor::{
    Check, CheckResult, DoctorReport, DoctorRow, DoctorStatus, default_checks, format_report,
    run_checks,
};
pub use install_service::{
    InstallServiceOptions, built_daemon_entry, install_service, render_unit,
};
pub use run::{Cli, Command, USAGE, run_cli};
pub use status::{format_status, summaries};
