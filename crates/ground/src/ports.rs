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
    /// A READ that failed in a way that says nothing about the resource and is
    /// worth asking again: a transport failure (connect, timeout, request or
    /// body error) or an HTTP 5xx/429 answer to a GET. Adapters produce it
    /// only for reads; a write that failed in transit may have taken effect,
    /// so a write error is never `Transient`. Wait loops treat it as an
    /// unanswered poll; every other caller treats it like `Message`.
    #[error("{0}")]
    Transient(String),
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

impl PortError {
    /// True only for a failed read worth asking again (see [`PortError::Transient`]).
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    /// The same error with the retry hint dropped. Callers that performed a
    /// write use it so nothing downstream can mistake the failure for a poll
    /// that merely went unanswered.
    pub fn into_permanent(self) -> Self {
        match self {
            Self::Transient(m) => Self::Message(m),
            other => other,
        }
    }

    /// Classifies an HTTP failure. Only a read (`read == true`) answered with
    /// 5xx or 429 is transient; 4xx otherwise and every write are permanent.
    pub fn from_http(read: bool, service: &str, status: u16, detail: Option<String>) -> Self {
        let err = Self::Http {
            service: service.to_owned(),
            status,
            detail,
        };
        if read && (status == 429 || (500..600).contains(&status)) {
            Self::Transient(err.to_string())
        } else {
            err
        }
    }

    /// Classifies a transport failure of a request. Only a read that failed
    /// to connect, timed out, or hit a request/body error is transient. The
    /// message carries the whole `source()` chain, since reqwest's own text
    /// ("error sending request for url") names no cause.
    pub fn from_transport(read: bool, err: &reqwest::Error) -> Self {
        let message = error_chain(err);
        if transport_is_transient(
            read,
            err.is_connect(),
            err.is_timeout(),
            err.is_request(),
            err.is_body(),
        ) {
            Self::Transient(message)
        } else {
            Self::Message(message)
        }
    }
}

/// The classification behind [`PortError::from_transport`], over the facts reqwest reports.
/// A write is never transient; a read is when it failed to connect, timed out, or hit a
/// request or body error. Any other failure of a read (a redirect loop, a decode error, a
/// builder error) is permanent.
pub(crate) fn transport_is_transient(
    read: bool,
    is_connect: bool,
    is_timeout: bool,
    is_request: bool,
    is_body: bool,
) -> bool {
    read && (is_connect || is_timeout || is_request || is_body)
}

/// `err` and each of its sources, joined as `a: b: c`.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

impl From<reqwest::Error> for PortError {
    /// Permanent: callers that know the method use [`PortError::from_transport`].
    fn from(err: reqwest::Error) -> Self {
        Self::from_transport(false, &err)
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

#[cfg(test)]
mod transport_tests {
    use super::transport_is_transient;

    #[test]
    fn only_a_read_that_failed_in_transit_is_transient() {
        // (connect, timeout, request, body)
        let kinds = [
            (true, false, false, false),
            (false, true, false, false),
            (false, false, true, false),
            (false, false, false, true),
        ];
        for (c, t, r, b) in kinds {
            assert!(transport_is_transient(true, c, t, r, b), "{c} {t} {r} {b}");
            assert!(
                !transport_is_transient(false, c, t, r, b),
                "{c} {t} {r} {b}"
            );
        }
        // Some other reqwest failure (redirect loop, decode, builder).
        assert!(!transport_is_transient(true, false, false, false, false));
        assert!(!transport_is_transient(false, false, false, false, false));
    }
}
