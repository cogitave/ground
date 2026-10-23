use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::ports::{
    GitHubPort, MergeOptions, MergeResult, PortError, PortResult, PrFile, PrState, PullRequest,
    RunFilter, RunJob, WorkflowRun,
};

/// A PR as a bot-authored version PR into `main` on `cogitave/namzu`; adjust with `tweak`.
pub fn make_pr(tweak: impl FnOnce(&mut PullRequest)) -> PullRequest {
    let mut pr = PullRequest {
        number: 1,
        title: "chore(release): version packages".into(),
        state: PrState::Open,
        merged: false,
        merge_commit_sha: None,
        head_ref: "changeset-release/main".into(),
        head_sha: "head-sha-1".into(),
        head_repo: "cogitave/namzu".into(),
        base_ref: "main".into(),
        base_sha: "base-sha-1".into(),
        author: "github-actions[bot]".into(),
        mergeable: Some(true),
    };
    tweak(&mut pr);
    pr
}

/// A queued `ci.yml` pull_request run for `head-sha-1`; adjust with `tweak`.
/// `html_url` follows `id` unless `tweak` sets it.
pub fn make_run(tweak: impl FnOnce(&mut WorkflowRun)) -> WorkflowRun {
    let mut run = WorkflowRun {
        id: 100,
        workflow_file: "ci.yml".into(),
        event: "pull_request".into(),
        head_sha: "head-sha-1".into(),
        head_branch: "changeset-release/main".into(),
        status: "queued".into(),
        conclusion: None,
        run_attempt: 1,
        created_at: "2026-01-01T00:00:00Z".into(),
        actor: "github-actions[bot]".into(),
        triggering_actor: "github-actions[bot]".into(),
        html_url: String::new(),
    };
    tweak(&mut run);
    if run.html_url.is_empty() {
        run.html_url = format!("https://github.com/cogitave/namzu/actions/runs/{}", run.id);
    }
    run
}

/// A partial update applied to a scripted run (the TypeScript `Partial<WorkflowRun>`).
#[derive(Debug, Clone, Default)]
pub struct RunStage {
    pub status: Option<String>,
    /// `Some(None)` clears the conclusion.
    pub conclusion: Option<Option<String>>,
    pub run_attempt: Option<u32>,
    pub head_sha: Option<String>,
    pub head_branch: Option<String>,
}

impl RunStage {
    pub fn status(status: &str) -> Self {
        Self {
            status: Some(status.into()),
            ..Self::default()
        }
    }

    /// `status: completed` with the given conclusion.
    pub fn completed(conclusion: &str) -> Self {
        Self {
            status: Some("completed".into()),
            conclusion: Some(Some(conclusion.into())),
            ..Self::default()
        }
    }

    fn apply(&self, run: &mut WorkflowRun) {
        if let Some(v) = &self.status {
            run.status.clone_from(v);
        }
        if let Some(v) = &self.conclusion {
            run.conclusion.clone_from(v);
        }
        if let Some(v) = self.run_attempt {
            run.run_attempt = v;
        }
        if let Some(v) = &self.head_sha {
            run.head_sha.clone_from(v);
        }
        if let Some(v) = &self.head_branch {
            run.head_branch.clone_from(v);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunScript {
    /// Applied one per poll (each `list_workflow_runs` call that returns the run), after it is returned.
    pub stages: Vec<RunStage>,
    /// Replaces the remaining stages when the run is approved.
    pub on_approve: Option<Vec<RunStage>>,
    /// Replaces the remaining stages when failed jobs are re-run; `run_attempt` is bumped.
    pub on_rerun: Option<Vec<RunStage>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMethod {
    ApproveRun,
    RerunFailedJobs,
    MergePr,
}

/// One recorded write. `args` is `{runId}` for approvals and re-runs and
/// `{number, method, admin, expectedHeadSha}` for merges.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteCall {
    pub method: WriteMethod,
    pub repo: String,
    pub args: Value,
}

struct ScriptedRun {
    run: WorkflowRun,
    repo: String,
    script: RunScript,
    pending: Vec<RunStage>,
}

type MergeHook = Arc<dyn Fn(&PullRequest, &str) + Send + Sync>;

#[derive(Default)]
struct Inner {
    writes: Vec<WriteCall>,
    on_merge: Option<MergeHook>,
    /// Insertion-ordered, like a JS Map: `find_open_pr` returns the first match.
    prs: Vec<(String, PullRequest)>,
    files: HashMap<String, Vec<PrFile>>,
    dirs: HashMap<String, Vec<String>>,
    branches: HashMap<String, String>,
    jobs: HashMap<String, Vec<RunJob>>,
    runs: Vec<ScriptedRun>,
    merge_shas: HashMap<String, String>,
}

fn fail<T>(msg: String) -> PortResult<T> {
    Err(PortError::Message(msg))
}

impl Inner {
    fn require_pr(&self, repo: &str, number: u64) -> PortResult<PullRequest> {
        let key = format!("{repo}#{number}");
        match self.prs.iter().find(|(k, _)| *k == key) {
            Some((_, pr)) => Ok(pr.clone()),
            None => fail(format!("FakeGitHub: no PR {key}")),
        }
    }

    fn put_pr(&mut self, repo: &str, pr: PullRequest) {
        let key = format!("{repo}#{}", pr.number);
        match self.prs.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = pr,
            None => self.prs.push((key, pr)),
        }
    }

    fn require_run(&mut self, repo: &str, run_id: u64) -> PortResult<&mut ScriptedRun> {
        match self
            .runs
            .iter_mut()
            .find(|r| r.repo == repo && r.run.id == run_id)
        {
            Some(r) => Ok(r),
            None => fail(format!("FakeGitHub: no run {repo}#{run_id}")),
        }
    }
}

/// In-memory [`GitHubPort`]. Reads are scripted through the setters; every write is recorded.
///
/// Setters take `&self` and return `&Self`, so a test can chain them on a binding:
/// `let gh = FakeGitHub::new(); gh.set_pr(repo, pr).set_dir(...);`
#[derive(Clone, Default)]
pub struct FakeGitHub {
    inner: Arc<Mutex<Inner>>,
}

impl FakeGitHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- scripting ----

    pub fn set_pr(&self, repo: &str, pr: PullRequest) -> &Self {
        self.lock().put_pr(repo, pr);
        self
    }

    /// Mutates a stored PR, e.g. to simulate the head moving between two reads. Panics if absent.
    pub fn update_pr(
        &self,
        repo: &str,
        number: u64,
        patch: impl FnOnce(&mut PullRequest),
    ) -> &Self {
        let mut inner = self.lock();
        let mut pr = inner
            .require_pr(repo, number)
            .unwrap_or_else(|e| panic!("{e}"));
        patch(&mut pr);
        inner.put_pr(repo, pr);
        drop(inner);
        self
    }

    pub fn set_pr_files(&self, repo: &str, number: u64, files: Vec<PrFile>) -> &Self {
        self.lock().files.insert(format!("{repo}#{number}"), files);
        self
    }

    pub fn set_dir(&self, repo: &str, ref_: &str, path: &str, entries: &[&str]) -> &Self {
        self.lock().dirs.insert(
            format!("{repo}@{ref_}:{path}"),
            entries.iter().map(|e| (*e).into()).collect(),
        );
        self
    }

    pub fn set_branch_sha(&self, repo: &str, branch: &str, sha: &str) -> &Self {
        self.lock()
            .branches
            .insert(format!("{repo}@{branch}"), sha.into());
        self
    }

    pub fn set_jobs(&self, repo: &str, run_id: u64, jobs: Vec<RunJob>) -> &Self {
        self.lock().jobs.insert(format!("{repo}#{run_id}"), jobs);
        self
    }

    /// The merge commit sha the next merge of this PR returns.
    pub fn set_merge_commit(&self, repo: &str, number: u64, sha: &str) -> &Self {
        self.lock()
            .merge_shas
            .insert(format!("{repo}#{number}"), sha.into());
        self
    }

    pub fn add_run(&self, repo: &str, run: WorkflowRun, script: RunScript) -> &Self {
        let pending = script.stages.clone();
        self.lock().runs.push(ScriptedRun {
            run,
            repo: repo.into(),
            script,
            pending,
        });
        self
    }

    /// Called after a successful merge (outside the fake's lock), so a test can make the
    /// release run appear: `gh.on_merge(move |pr, sha| { other.add_run(..) })`.
    pub fn on_merge(&self, hook: impl Fn(&PullRequest, &str) + Send + Sync + 'static) -> &Self {
        self.lock().on_merge = Some(Arc::new(hook));
        self
    }

    /// The current state of a scripted run, without advancing it. Panics if absent.
    pub fn run_state(&self, repo: &str, run_id: u64) -> WorkflowRun {
        self.lock()
            .require_run(repo, run_id)
            .unwrap_or_else(|e| panic!("{e}"))
            .run
            .clone()
    }

    /// Every write so far, in order.
    pub fn writes(&self) -> Vec<WriteCall> {
        self.lock().writes.clone()
    }

    pub fn writes_of(&self, method: WriteMethod) -> Vec<WriteCall> {
        self.writes()
            .into_iter()
            .filter(|w| w.method == method)
            .collect()
    }
}

impl GitHubPort for FakeGitHub {
    async fn find_open_pr(
        &self,
        repo: &str,
        head: &str,
        base: &str,
    ) -> PortResult<Option<PullRequest>> {
        let prefix = format!("{repo}#");
        Ok(self
            .lock()
            .prs
            .iter()
            .find(|(k, pr)| {
                k.starts_with(&prefix)
                    && pr.state == PrState::Open
                    && pr.head_ref == head
                    && pr.base_ref == base
            })
            .map(|(_, pr)| pr.clone()))
    }

    async fn get_pr(&self, repo: &str, number: u64) -> PortResult<PullRequest> {
        self.lock().require_pr(repo, number)
    }

    async fn list_pr_files(&self, repo: &str, number: u64) -> PortResult<Vec<PrFile>> {
        let inner = self.lock();
        inner.require_pr(repo, number)?;
        Ok(inner
            .files
            .get(&format!("{repo}#{number}"))
            .cloned()
            .unwrap_or_default())
    }

    async fn list_dir(&self, repo: &str, ref_: &str, path: &str) -> PortResult<Vec<String>> {
        Ok(self
            .lock()
            .dirs
            .get(&format!("{repo}@{ref_}:{path}"))
            .cloned()
            .unwrap_or_default())
    }

    async fn get_branch_sha(&self, repo: &str, branch: &str) -> PortResult<String> {
        let key = format!("{repo}@{branch}");
        match self.lock().branches.get(&key) {
            Some(sha) => Ok(sha.clone()),
            None => fail(format!("FakeGitHub: no branch {key}")),
        }
    }

    async fn list_workflow_runs(
        &self,
        repo: &str,
        workflow_file: &str,
        filter: &RunFilter,
    ) -> PortResult<Vec<WorkflowRun>> {
        let mut inner = self.lock();
        let mut snapshot = Vec::new();
        for s in inner.runs.iter_mut().filter(|s| {
            s.repo == repo
                && s.run.workflow_file == workflow_file
                && filter
                    .head_sha
                    .as_ref()
                    .is_none_or(|v| *v == s.run.head_sha)
                && filter
                    .branch
                    .as_ref()
                    .is_none_or(|v| *v == s.run.head_branch)
                && filter.event.as_ref().is_none_or(|v| *v == s.run.event)
        }) {
            snapshot.push(s.run.clone());
            if !s.pending.is_empty() {
                let next = s.pending.remove(0);
                next.apply(&mut s.run);
            }
        }
        Ok(snapshot)
    }

    async fn list_run_jobs(&self, repo: &str, run_id: u64) -> PortResult<Vec<RunJob>> {
        Ok(self
            .lock()
            .jobs
            .get(&format!("{repo}#{run_id}"))
            .cloned()
            .unwrap_or_default())
    }

    async fn approve_run(&self, repo: &str, run_id: u64) -> PortResult<()> {
        let mut inner = self.lock();
        inner.require_run(repo, run_id)?;
        inner.writes.push(WriteCall {
            method: WriteMethod::ApproveRun,
            repo: repo.into(),
            args: json!({ "runId": run_id }),
        });
        let s = inner.require_run(repo, run_id)?;
        s.run.status = "queued".into();
        s.run.conclusion = None;
        if let Some(stages) = &s.script.on_approve {
            s.pending = stages.clone();
        }
        Ok(())
    }

    async fn rerun_failed_jobs(&self, repo: &str, run_id: u64) -> PortResult<()> {
        let mut inner = self.lock();
        inner.require_run(repo, run_id)?;
        inner.writes.push(WriteCall {
            method: WriteMethod::RerunFailedJobs,
            repo: repo.into(),
            args: json!({ "runId": run_id }),
        });
        let s = inner.require_run(repo, run_id)?;
        s.run.status = "queued".into();
        s.run.conclusion = None;
        s.run.run_attempt += 1;
        if let Some(stages) = &s.script.on_rerun {
            s.pending = stages.clone();
        }
        Ok(())
    }

    async fn merge_pr(
        &self,
        repo: &str,
        number: u64,
        opts: &MergeOptions,
    ) -> PortResult<MergeResult> {
        let (merged, merge_commit_sha, hook) = {
            let mut inner = self.lock();
            let pr = inner.require_pr(repo, number)?;
            inner.writes.push(WriteCall {
                method: WriteMethod::MergePr,
                repo: repo.into(),
                args: json!({
                    "number": number,
                    "method": opts.method.as_str(),
                    "admin": opts.admin,
                    "expectedHeadSha": opts.expected_head_sha,
                }),
            });
            if pr.state != PrState::Open {
                return fail(format!("FakeGitHub: PR #{number} is not open"));
            }
            if pr.head_sha != opts.expected_head_sha {
                return fail(format!(
                    "FakeGitHub: head sha mismatch on PR #{number} (expected {}, is {})",
                    opts.expected_head_sha, pr.head_sha
                ));
            }
            let sha = inner
                .merge_shas
                .get(&format!("{repo}#{number}"))
                .cloned()
                .unwrap_or_else(|| format!("merge-commit-{number}"));
            let merged = PullRequest {
                state: PrState::Closed,
                merged: true,
                merge_commit_sha: Some(sha.clone()),
                ..pr.clone()
            };
            inner.put_pr(repo, merged.clone());
            inner
                .branches
                .insert(format!("{repo}@{}", pr.base_ref), sha.clone());
            (merged, sha, inner.on_merge.clone())
        };
        if let Some(hook) = hook {
            hook(&merged, &merge_commit_sha);
        }
        Ok(MergeResult { merge_commit_sha })
    }
}
