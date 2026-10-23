//! The seams every lane is written against: GitHub, the npm registry, time and
//! logging. Real implementations live in `adapters`, fakes in `testing`.
//!
//! The async traits return `impl Future + Send` so they can be used from
//! spawned tasks; implementations may simply write `async fn`.

use std::future::Future;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `owner/name`. GitHub slugs are case-insensitive; compare accordingly.
pub type RepoSlug = String;

/// Error returned by every port call.
#[derive(Debug, thiserror::Error)]
pub enum PortError {
    /// Anything that failed, with a message fit to show the operator.
    #[error("{0}")]
    Message(String),
    /// An HTTP answer that was not a success.
    #[error("{service} answered {status}{}", .detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
    Http {
        service: String,
        status: u16,
        detail: Option<String>,
    },
}

impl PortError {
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

impl From<reqwest::Error> for PortError {
    fn from(err: reqwest::Error) -> Self {
        Self::Message(err.to_string())
    }
}

pub type PortResult<T> = Result<T, PortError>;

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
    /// Waits `ms` milliseconds. Fakes advance virtual time and return at once.
    fn sleep(&self, ms: u64) -> impl Future<Output = ()> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// Structured logging port. `data` is a JSON object (or `None`).
pub trait Logger: Send + Sync {
    fn log(&self, level: LogLevel, msg: &str, data: Option<Value>);

    fn debug(&self, msg: &str, data: Option<Value>) {
        self.log(LogLevel::Debug, msg, data);
    }
    fn info(&self, msg: &str, data: Option<Value>) {
        self.log(LogLevel::Info, msg, data);
    }
    fn warn(&self, msg: &str, data: Option<Value>) {
        self.log(LogLevel::Warn, msg, data);
    }
    fn error(&self, msg: &str, data: Option<Value>) {
        self.log(LogLevel::Error, msg, data);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: PrState,
    pub merged: bool,
    /// Set once the PR is merged; GitHub's merge_commit_sha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit_sha: Option<String>,
    pub head_ref: String,
    pub head_sha: String,
    pub head_repo: RepoSlug,
    pub base_ref: String,
    pub base_sha: String,
    pub author: String,
    pub mergeable: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrFileStatus {
    Added,
    Removed,
    Modified,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrFile {
    pub path: String,
    pub status: PrFileStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRun {
    pub id: u64,
    pub workflow_file: String,
    pub event: String,
    pub head_sha: String,
    pub head_branch: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub run_attempt: u32,
    pub created_at: String,
    pub actor: String,
    pub triggering_actor: String,
    pub html_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunJob {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
}

/// Which workflow runs `list_workflow_runs` returns; `None` means "any".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub event: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Merge => "merge",
            Self::Rebase => "rebase",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeOptions {
    pub method: MergeMethod,
    pub admin: bool,
    /// The merge is refused if the PR head is no longer this sha.
    pub expected_head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeResult {
    pub merge_commit_sha: String,
}

pub trait GitHubPort: Send + Sync {
    fn find_open_pr(
        &self,
        repo: &str,
        head: &str,
        base: &str,
    ) -> impl Future<Output = PortResult<Option<PullRequest>>> + Send;
    fn get_pr(
        &self,
        repo: &str,
        number: u64,
    ) -> impl Future<Output = PortResult<PullRequest>> + Send;
    fn list_pr_files(
        &self,
        repo: &str,
        number: u64,
    ) -> impl Future<Output = PortResult<Vec<PrFile>>> + Send;
    /// Entry names of a directory at `ref_`; empty when the directory does not exist.
    fn list_dir(
        &self,
        repo: &str,
        ref_: &str,
        path: &str,
    ) -> impl Future<Output = PortResult<Vec<String>>> + Send;
    fn get_branch_sha(
        &self,
        repo: &str,
        branch: &str,
    ) -> impl Future<Output = PortResult<String>> + Send;
    fn list_workflow_runs(
        &self,
        repo: &str,
        workflow_file: &str,
        filter: &RunFilter,
    ) -> impl Future<Output = PortResult<Vec<WorkflowRun>>> + Send;
    fn list_run_jobs(
        &self,
        repo: &str,
        run_id: u64,
    ) -> impl Future<Output = PortResult<Vec<RunJob>>> + Send;
    fn approve_run(&self, repo: &str, run_id: u64) -> impl Future<Output = PortResult<()>> + Send;
    fn rerun_failed_jobs(
        &self,
        repo: &str,
        run_id: u64,
    ) -> impl Future<Output = PortResult<()>> + Send;
    fn merge_pr(
        &self,
        repo: &str,
        number: u64,
        opts: &MergeOptions,
    ) -> impl Future<Output = PortResult<MergeResult>> + Send;
}

pub trait RegistryPort: Send + Sync {
    /// `dist-tags.latest` of the package, or `None` when the registry has no such package.
    fn latest_version(&self, pkg: &str) -> impl Future<Output = PortResult<Option<String>>> + Send;
}
