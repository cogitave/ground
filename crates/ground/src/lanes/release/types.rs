use serde::{Deserialize, Serialize};

use crate::config::RepoConfig;
use crate::ports::PortError;
use crate::state::{StateError, StateStore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BumpedPackage {
    pub name: String,
    /// `None` for a package the PR introduces.
    pub from: Option<String>,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BumpScan {
    pub packages: Vec<BumpedPackage>,
    /// package.json files whose version moved but whose name the diff did not show.
    pub unresolved: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipStatus {
    NothingToShip,
    /// Dry run: stopped before the first GitHub write; `plan` says what it would do.
    Planned,
    Shipped,
    Failed,
    Aborted,
    /// A wait ran out. The session stays resumable.
    TimedOut,
    /// A run needs a human approval ground may not give. The session stays resumable.
    Blocked,
    /// Release run is green but the registry never showed the version. Resumable.
    RegistryPending,
}

impl ShipStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NothingToShip => "nothing_to_ship",
            Self::Planned => "planned",
            Self::Shipped => "shipped",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::TimedOut => "timed_out",
            Self::Blocked => "blocked",
            Self::RegistryPending => "registry_pending",
        }
    }

    /// Outcomes that leave the session open so the next `ship` carries on from the same step.
    pub fn is_resumable(self) -> bool {
        matches!(
            self,
            Self::Planned | Self::TimedOut | Self::Blocked | Self::RegistryPending
        )
    }

    /// Exit 0 only when there is nothing left for a human to look at.
    pub fn is_ok(self) -> bool {
        matches!(self, Self::Shipped | Self::NothingToShip | Self::Planned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipPlanAction {
    Approve,
    Rerun,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipPlan {
    pub action: ShipPlanAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShipOutcome {
    pub status: ShipStatus,
    /// Human-readable, printable as is.
    pub summary: String,
    pub session_id: String,
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ShipPlan>,
    pub packages: Vec<BumpedPackage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_commit_sha: Option<String>,
    /// Whether the base branch was still the tip the PR was built on when merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_was_tip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_jobs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_packages: Option<Vec<BumpedPackage>>,
    pub reruns: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ShipOptions {
    pub confirm: bool,
    /// How long to wait for the version PR's CI to finish, default 120.
    pub ci_wait_minutes: Option<u64>,
    /// How long to wait for the release run to appear and finish, default 90.
    pub release_wait_minutes: Option<u64>,
}

/// Everything `ship` talks to. Borrowed, so a test keeps its own handles.
pub struct ShipDeps<'a, G, R, C, L> {
    pub config: &'a RepoConfig,
    pub github: &'a G,
    pub registry: &'a R,
    pub clock: &'a C,
    pub logger: &'a L,
    pub store: &'a StateStore,
}

impl<G, R, C, L> Clone for ShipDeps<'_, G, R, C, L> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<G, R, C, L> Copy for ShipDeps<'_, G, R, C, L> {}

/// A failure of a port or of the state store; the session stays resumable.
#[derive(Debug, thiserror::Error)]
pub enum ShipError {
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    State(#[from] StateError),
}

impl From<serde_json::Error> for ShipError {
    fn from(e: serde_json::Error) -> Self {
        Self::State(StateError::from(e))
    }
}
