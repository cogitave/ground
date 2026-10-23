//! GitHub over its REST API (about a dozen endpoints), with the token from
//! `gh auth token`. Admin merges shell out to `gh pr merge --admin
//! --match-head-commit`, because ruleset bypass is proven to work through `gh`
//! and not through the REST merge call.

use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use super::exec::{Exec, SystemExec, token_from_gh};
use crate::ports::{
    GitHubPort, MergeMethod, MergeOptions, MergeResult, PortError, PortResult, PrFile,
    PrFileStatus, PrState, PullRequest, RunFilter, RunJob, WorkflowRun,
};

const API: &str = "https://api.github.com";
const PER_PAGE: usize = 100;

pub fn split_repo(repo: &str) -> PortResult<(&str, &str)> {
    repo.split_once('/')
        .filter(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'))
        .ok_or_else(|| PortError::msg(format!("\"{repo}\" is not an owner/name slug")))
}

// ---- pure mapping from the REST shapes (only the fields read) ----

#[derive(Debug, Clone, Deserialize)]
pub struct RawRepo {
    pub full_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawHead {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
    pub repo: Option<RawRepo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawBase {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawUser {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawPr {
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub merged: Option<bool>,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub merge_commit_sha: Option<String>,
    pub head: RawHead,
    pub base: RawBase,
    pub user: Option<RawUser>,
    #[serde(default)]
    pub mergeable: Option<bool>,
}

pub fn map_pr(raw: RawPr) -> PullRequest {
    let merged = raw.merged.unwrap_or(raw.merged_at.is_some());
    PullRequest {
        number: raw.number,
        title: raw.title,
        state: if raw.state == "open" {
            PrState::Open
        } else {
            PrState::Closed
        },
        merged,
        merge_commit_sha: if merged {
            raw.merge_commit_sha.filter(|s| !s.is_empty())
        } else {
            None
        },
        head_ref: raw.head.ref_,
        head_sha: raw.head.sha,
        // A deleted fork has no repo; an empty-owner slug can never match the base repo.
        head_repo: raw
            .head
            .repo
            .map_or_else(|| "/".to_owned(), |r| r.full_name),
        base_ref: raw.base.ref_,
        base_sha: raw.base.sha,
        author: raw.user.map(|u| u.login).unwrap_or_default(),
        mergeable: raw.mergeable,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawFile {
    pub filename: String,
    pub status: String,
    #[serde(default)]
    pub patch: Option<String>,
}

pub fn map_file(raw: RawFile) -> PrFile {
    // GitHub also reports copied/changed/unchanged; all of them are edits to a path.
    let status = match raw.status.as_str() {
        "added" => PrFileStatus::Added,
        "removed" => PrFileStatus::Removed,
        "renamed" => PrFileStatus::Renamed,
        _ => PrFileStatus::Modified,
    };
    PrFile {
        path: raw.filename,
        status,
        patch: raw.patch,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawRun {
    pub id: u64,
    pub path: String,
    pub event: String,
    pub head_sha: String,
    pub head_branch: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    #[serde(default)]
    pub run_attempt: Option<u32>,
    pub created_at: String,
    #[serde(default)]
    pub actor: Option<RawUser>,
    #[serde(default)]
    pub triggering_actor: Option<RawUser>,
    pub html_url: String,
}

pub fn map_run(raw: RawRun) -> WorkflowRun {
    let actor = raw.actor.map(|a| a.login).unwrap_or_default();
    WorkflowRun {
        id: raw.id,
        // `path` is ".github/workflows/ci.yml"; callers address workflows by file name.
        workflow_file: raw.path.rsplit('/').next().unwrap_or(&raw.path).to_owned(),
        event: raw.event,
        head_sha: raw.head_sha,
        head_branch: raw.head_branch.unwrap_or_default(),
        status: raw.status.unwrap_or_default(),
        conclusion: raw.conclusion,
        run_attempt: raw.run_attempt.unwrap_or(1),
        created_at: raw.created_at,
        triggering_actor: raw
            .triggering_actor
            .map_or_else(|| actor.clone(), |a| a.login),
        actor,
        html_url: raw.html_url,
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawJob {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
}

pub fn map_job(raw: RawJob) -> RunJob {
    RunJob {
        name: raw.name,
        status: raw.status,
        conclusion: raw.conclusion,
    }
}

pub fn gh_merge_args(
    repo: &str,
    number: u64,
    method: MergeMethod,
    expected_head_sha: &str,
) -> Vec<String> {
    [
        "pr",
        "merge",
        &number.to_string(),
        "--repo",
        repo,
        &format!("--{}", method.as_str()),
        "--admin",
        "--match-head-commit",
        expected_head_sha,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The JSON body (`Null` when empty). A non-JSON body on a failure keeps its
/// status (`check` reports it); on a success it is a protocol error naming both.
fn decode_body(status: u16, bytes: &[u8]) -> PortResult<Value> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    match serde_json::from_slice(bytes) {
        Ok(v) => Ok(v),
        Err(_) if !(200..300).contains(&status) => Ok(Value::Null),
        Err(_) => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
            Err(PortError::msg(format!(
                "GitHub answered {status} with a body that is not JSON: {head}"
            )))
        }
    }
}

// ---- the adapter ----

pub struct RestGitHub<E: Exec = SystemExec> {
    client: reqwest::Client,
    base: String,
    exec: E,
    token: OnceCell<String>,
}

impl RestGitHub<SystemExec> {
    /// Authenticates with `gh auth token`, fetched on first use.
    pub fn new() -> Self {
        Self::with_exec(SystemExec)
    }
}

impl Default for RestGitHub<SystemExec> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Exec> RestGitHub<E> {
    /// `exec` runs `gh` for the token and for admin merges.
    pub fn with_exec(exec: E) -> Self {
        Self {
            client: crate::adapters::http_client(),
            base: API.to_owned(),
            exec,
            token: OnceCell::new(),
        }
    }

    /// Skips `gh auth token`.
    pub fn with_token(mut self, token: &str) -> Self {
        self.token = OnceCell::new_with(Some(token.to_owned()));
        self
    }

    /// Points the client at another API root (tests use a loopback server).
    pub fn with_base_url(mut self, base: &str) -> Self {
        self.base = base.trim_end_matches('/').to_owned();
        self
    }

    async fn token(&self) -> PortResult<&str> {
        self.token
            .get_or_try_init(|| token_from_gh(&self.exec))
            .await
            .map(String::as_str)
    }

    fn url(&self, segments: &[&str]) -> PortResult<Url> {
        let mut url = Url::parse(&self.base).map_err(|e| PortError::msg(e.to_string()))?;
        url.path_segments_mut()
            .map_err(|()| PortError::msg("GitHub API root cannot be a base"))?
            .pop_if_empty()
            .extend(
                segments
                    .iter()
                    .flat_map(|s| s.split('/'))
                    .filter(|s| !s.is_empty()),
            );
        Ok(url)
    }

    fn repo_url(&self, repo: &str, rest: &[&str]) -> PortResult<Url> {
        let (owner, name) = split_repo(repo)?;
        let mut segments = vec!["repos", owner, name];
        segments.extend_from_slice(rest);
        self.url(&segments)
    }

    /// Sends the request; returns the status and the JSON body (`Null` when empty).
    async fn send(
        &self,
        method: Method,
        url: Url,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> PortResult<(u16, Value)> {
        let token = self.token().await?;
        let mut req = self
            .client
            .request(method, url)
            .bearer_auth(token)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", concat!("ground/", env!("CARGO_PKG_VERSION")))
            .query(query);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await?;
        let status = res.status().as_u16();
        let bytes = res.bytes().await?;
        let json = decode_body(status, &bytes)?;
        Ok((status, json))
    }

    fn check(status: u16, body: &Value) -> PortResult<()> {
        if (200..300).contains(&status) {
            return Ok(());
        }
        Err(PortError::Http {
            service: "GitHub".into(),
            status,
            detail: body
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    }

    async fn request(
        &self,
        method: Method,
        url: Url,
        query: &[(&str, String)],
        body: Option<Value>,
    ) -> PortResult<Value> {
        let (status, json) = self.send(method, url, query, body).await?;
        Self::check(status, &json)?;
        Ok(json)
    }

    /// Follows `page=` until a short page. `key` names the array inside an object response.
    async fn paginate<T: for<'de> Deserialize<'de>>(
        &self,
        url: Url,
        key: Option<&str>,
        query: &[(&str, String)],
    ) -> PortResult<Vec<T>> {
        let mut out = Vec::new();
        for page in 1.. {
            let mut q = query.to_vec();
            q.push(("per_page", PER_PAGE.to_string()));
            q.push(("page", page.to_string()));
            let body = self.request(Method::GET, url.clone(), &q, None).await?;
            let items = match key {
                Some(k) => body.get(k).cloned().unwrap_or(Value::Null),
                None => body,
            };
            let items: Vec<T> = serde_json::from_value(items)
                .map_err(|e| PortError::msg(format!("unexpected GitHub response: {e}")))?;
            let full = items.len() >= PER_PAGE;
            out.extend(items);
            if !full {
                break;
            }
        }
        Ok(out)
    }
}

fn decode<T: for<'de> Deserialize<'de>>(value: Value) -> PortResult<T> {
    serde_json::from_value(value)
        .map_err(|e| PortError::msg(format!("unexpected GitHub response: {e}")))
}

impl<E: Exec> GitHubPort for RestGitHub<E> {
    async fn find_open_pr(
        &self,
        repo: &str,
        head: &str,
        base: &str,
    ) -> PortResult<Option<PullRequest>> {
        let (owner, _) = split_repo(repo)?;
        let query = [
            ("state", "open".to_owned()),
            ("head", format!("{owner}:{head}")),
            ("base", base.to_owned()),
            ("per_page", "1".to_owned()),
        ];
        let list = self
            .request(Method::GET, self.repo_url(repo, &["pulls"])?, &query, None)
            .await?;
        let first = list
            .get(0)
            .and_then(|p| p.get("number"))
            .and_then(Value::as_u64);
        // The list endpoint omits `mergeable`, so fetch the PR itself.
        match first {
            Some(number) => self.get_pr(repo, number).await.map(Some),
            None => Ok(None),
        }
    }

    async fn get_pr(&self, repo: &str, number: u64) -> PortResult<PullRequest> {
        let url = self.repo_url(repo, &["pulls", &number.to_string()])?;
        let raw: RawPr = decode(self.request(Method::GET, url, &[], None).await?)?;
        Ok(map_pr(raw))
    }

    async fn list_pr_files(&self, repo: &str, number: u64) -> PortResult<Vec<PrFile>> {
        let url = self.repo_url(repo, &["pulls", &number.to_string(), "files"])?;
        let files: Vec<RawFile> = self.paginate(url, None, &[]).await?;
        Ok(files.into_iter().map(map_file).collect())
    }

    async fn list_dir(&self, repo: &str, ref_: &str, path: &str) -> PortResult<Vec<String>> {
        let url = self.repo_url(repo, &["contents", path])?;
        let (status, body) = self
            .send(Method::GET, url, &[("ref", ref_.to_owned())], None)
            .await?;
        if status == 404 {
            return Ok(Vec::new());
        }
        Self::check(status, &body)?;
        Ok(match body {
            Value::Array(entries) => entries
                .iter()
                .filter_map(|e| e.get("name").and_then(Value::as_str).map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        })
    }

    async fn get_branch_sha(&self, repo: &str, branch: &str) -> PortResult<String> {
        let url = self.repo_url(repo, &["branches", branch])?;
        let body = self.request(Method::GET, url, &[], None).await?;
        body.pointer("/commit/sha")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| PortError::msg(format!("GitHub sent no commit for branch {branch}")))
    }

    async fn list_workflow_runs(
        &self,
        repo: &str,
        workflow_file: &str,
        filter: &RunFilter,
    ) -> PortResult<Vec<WorkflowRun>> {
        let url = self.repo_url(repo, &["actions", "workflows", workflow_file, "runs"])?;
        let mut query = vec![("per_page", "30".to_owned())];
        for (key, value) in [
            ("head_sha", &filter.head_sha),
            ("branch", &filter.branch),
            ("event", &filter.event),
        ] {
            if let Some(v) = value.as_ref().filter(|v| !v.is_empty()) {
                query.push((key, v.clone()));
            }
        }
        let body = self.request(Method::GET, url, &query, None).await?;
        let runs: Vec<RawRun> = decode(body.get("workflow_runs").cloned().unwrap_or(json!([])))?;
        Ok(runs.into_iter().map(map_run).collect())
    }

    async fn list_run_jobs(&self, repo: &str, run_id: u64) -> PortResult<Vec<RunJob>> {
        let url = self.repo_url(repo, &["actions", "runs", &run_id.to_string(), "jobs"])?;
        let jobs: Vec<RawJob> = self.paginate(url, Some("jobs"), &[]).await?;
        Ok(jobs.into_iter().map(map_job).collect())
    }

    async fn approve_run(&self, repo: &str, run_id: u64) -> PortResult<()> {
        let url = self.repo_url(repo, &["actions", "runs", &run_id.to_string(), "approve"])?;
        self.request(Method::POST, url, &[], None).await.map(drop)
    }

    async fn rerun_failed_jobs(&self, repo: &str, run_id: u64) -> PortResult<()> {
        let url = self.repo_url(
            repo,
            &["actions", "runs", &run_id.to_string(), "rerun-failed-jobs"],
        )?;
        self.request(Method::POST, url, &[], None).await.map(drop)
    }

    async fn merge_pr(
        &self,
        repo: &str,
        number: u64,
        opts: &MergeOptions,
    ) -> PortResult<MergeResult> {
        if opts.admin {
            self.exec
                .run(
                    "gh",
                    &gh_merge_args(repo, number, opts.method, &opts.expected_head_sha),
                )
                .await?;
            let pr = self.get_pr(repo, number).await?;
            let merge_commit_sha = pr.merge_commit_sha.ok_or_else(|| {
                PortError::msg(format!("PR #{number} merged but has no merge commit sha"))
            })?;
            return Ok(MergeResult { merge_commit_sha });
        }
        let url = self.repo_url(repo, &["pulls", &number.to_string(), "merge"])?;
        let body = json!({ "merge_method": opts.method.as_str(), "sha": opts.expected_head_sha });
        let res = self.request(Method::PUT, url, &[], Some(body)).await?;
        let merge_commit_sha = res
            .get("sha")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                PortError::msg(format!("GitHub sent no merge commit for PR #{number}"))
            })?;
        Ok(MergeResult { merge_commit_sha })
    }
}

#[cfg(test)]
mod tests;
