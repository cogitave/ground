//! `ground ship`: version PR to published packages, as a resumable state machine.

mod bumps;
mod command;
mod ship;
mod types;

pub use bumps::scan_bumps;
pub use command::run_ship_command;
pub use ship::ship;
pub use types::{
    BumpScan, BumpedPackage, ShipDeps, ShipError, ShipOptions, ShipOutcome, ShipPlan,
    ShipPlanAction, ShipStatus,
};

#[cfg(test)]
mod bumps_tests;
#[cfg(test)]
mod command_tests;
#[cfg(test)]
mod ship_tests;
