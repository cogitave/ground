use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::bumps::scan_bumps;
use super::types::{
    BumpedPackage, ShipDeps, ShipError, ShipOptions, ShipOutcome, ShipPlan, ShipPlanAction,
    ShipStatus,
};
use crate::approval::{ApprovalDecision, can_approve_run};
use crate::ports::{
    Clock, GitHubPort, Logger, MergeOptions, PrFileStatus, PrState, PullRequest, RegistryPort,
    RunFilter, WorkflowRun,
};
use crate::state::{NewShipSession, ShipSessionPatch};

type Result<T> = std::result::Result<T, ShipError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    FindPr,
    Snapshot,
    Gate,
    Merge,
    Release,
    Registry,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Self::FindPr => "find_pr",
            Self::Snapshot => "snapshot",
            Self::Gate => "gate",
            Self::Merge => "merge",
            Self::Release => "release",
            Self::Registry => "registry",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "find_pr" => Self::FindPr,
            "snapshot" => Self::Snapshot,
            "gate" => Self::Gate,
            "merge" => Self::Merge,
            "release" => Self::Release,
            "registry" => Self::Registry,
            _ => return None,
        })
    }
}

/// Persisted in the session row; everything else is re-derived from GitHub on resume.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ShipData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pr_number: Option<u64>,
    /// The head the CI gate is judging; a different head restarts from the snapshot check.
    #[serde(skip_serializing_if = "Option::is_none")]
    head_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    packages: Option<Vec<BumpedPackage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unresolved: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    merge_commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_was_tip: Option<bool>,
    /// Wait deadlines (ISO) by name, so a resumed session keeps its remaining budget.
    deadlines: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

enum StepResult {
    Next(Step),
    Done(Box<ShipOutcome>),
}

/// Optional overrides of the defaults `done` derives from the session data.
#[derive(Default)]
struct Extra {
    plan: Option<ShipPlan>,
    packages: Option<Vec<BumpedPackage>>,
    merge_commit_sha: Option<String>,
    base_was_tip: Option<bool>,
    failed_jobs: Option<Vec<String>>,
    missing_packages: Option<Vec<BumpedPackage>>,
}

const OK_JOB: [&str; 3] = ["success", "skipped", "neutral"];
const RELEASE_RERUNS: u32 = 1;
const CHANGESET_IGNORED: [&str; 2] = ["README.md", "config.json"];

fn changeset_names<I, S>(entries: I) -> BTreeSet<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    entries
        .into_iter()
        .map(|e| e.as_ref().rsplit('/').next().unwrap_or_default().to_owned())
        .filter(|n| n.ends_with(".md") && !CHANGESET_IGNORED.contains(&n.as_str()))
        .collect()
}

fn newest(runs: Vec<WorkflowRun>) -> Option<WorkflowRun> {
    runs.into_iter()
        .reduce(|a, r| if r.id > a.id { r } else { a })
}

fn describe(packages: &[BumpedPackage]) -> String {
    if packages.is_empty() {
        "no packages".into()
    } else {
        packages
            .iter()
            .map(|p| format!("{}@{}", p.name, p.to))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

fn is_ok_job(conclusion: &Option<String>) -> bool {
    OK_JOB.contains(&conclusion.as_deref().unwrap_or(""))
}

/// `ground ship`: version PR to published packages, as a resumable state machine.
/// Every step re-reads GitHub, so the persisted state is only what GitHub cannot
/// tell us (the head being judged, the merge commit, the wait budgets).
pub async fn ship<G, R, C, L>(
    deps: &ShipDeps<'_, G, R, C, L>,
    options: &ShipOptions,
) -> Result<ShipOutcome>
where
    G: GitHubPort,
    R: RegistryPort,
    C: Clock,
    L: Logger,
{
    ShipRun::start(*deps, options.clone())?.run().await
}

enum CiVerdict {
    Green,
    Restart,
    Plan(ShipPlan),
    Failed { jobs: Vec<String>, url: String },
    Aborted(String),
}

enum Found {
    Pr(PullRequest),
    Reason(String),
}

struct ShipRun<'a, G, R, C, L> {
    deps: ShipDeps<'a, G, R, C, L>,
    opts: ShipOptions,
    session_id: String,
    start_step: Step,
    data: ShipData,
    ci_reruns: u32,
    release_reruns: u32,
    /// Attempt a rerun was requested for, per run: GitHub may still list the old attempt briefly.
    rerun_requested: HashMap<u64, u32>,
}

impl<'a, G, R, C, L> ShipRun<'a, G, R, C, L>
where
    G: GitHubPort,
    R: RegistryPort,
    C: Clock,
    L: Logger,
{
    fn start(deps: ShipDeps<'a, G, R, C, L>, opts: ShipOptions) -> Result<Self> {
        let repo = &deps.config.repo;
        let session = match deps.store.get_active_ship_session(repo)? {
            Some(active) => {
                deps.logger.info(
                    "resuming ship session",
                    Some(json!({ "id": active.id, "step": active.step })),
                );
                active
            }
            None => deps
                .store
                .create_ship_session(NewShipSession::new(repo.as_str(), Step::FindPr.as_str()))?,
        };
        let data = serde_json::from_value(session.data.clone()).unwrap_or_default();
        Ok(Self {
            deps,
            opts,
            session_id: session.id,
            start_step: Step::parse(&session.step).unwrap_or(Step::FindPr),
            data,
            ci_reruns: 0,
            release_reruns: 0,
            rerun_requested: HashMap::new(),
        })
    }

    fn cfg(&self) -> &'a crate::config::RepoConfig {
        self.deps.config
    }

    async fn run(mut self) -> Result<ShipOutcome> {
        let mut step = self.start_step;
        loop {
            self.save(Some(step), None)?;
            let result = match step {
                Step::FindPr => self.find_pr().await?,
                Step::Snapshot => self.snapshot().await?,
                Step::Gate => self.gate().await?,
                Step::Merge => self.merge().await?,
                Step::Release => self.release().await?,
                Step::Registry => self.registry().await?,
            };
            match result {
                StepResult::Done(outcome) => return self.finish(*outcome),
                StepResult::Next(next) => step = next,
            }
        }
    }

    // ---- step 1 ----

    async fn find_pr(&mut self) -> Result<StepResult> {
        let cfg = self.cfg();
        let wait = cfg.version_pr_wait_minutes;
        let deadline = self.begin_wait("versionPr", wait)?;
        let mut saw_running = false;
        let mut grace = false;
        let found = loop {
            if let Some(found) = self.find_pr_probe(&mut saw_running, &mut grace).await? {
                break Some(found);
            }
            if self.pause(deadline).await {
                break None;
            }
        };
        self.end_wait("versionPr")?;
        let found = match found {
            None => {
                return Ok(self.done(
                    ShipStatus::TimedOut,
                    format!(
                        "No version PR from {} appeared within {} minutes.",
                        cfg.release_branch, wait
                    ),
                    Extra::default(),
                ));
            }
            Some(Found::Reason(reason)) => {
                return Ok(self.done(
                    ShipStatus::NothingToShip,
                    format!("Nothing to ship. {reason}"),
                    Extra::default(),
                ));
            }
            Some(Found::Pr(pr)) => pr,
        };
        self.deps
            .logger
            .info("found version PR", Some(json!({ "number": found.number })));
        self.data.pr_number = Some(found.number);
        self.save(None, Some(found.number))?;
        Ok(StepResult::Next(Step::Snapshot))
    }

    async fn find_pr_probe(
        &mut self,
        saw_running: &mut bool,
        grace: &mut bool,
    ) -> Result<Option<Found>> {
        let cfg = self.cfg();
        if let Some(pr) = self
            .deps
            .github
            .find_open_pr(&cfg.repo, &cfg.release_branch, &cfg.default_branch)
            .await?
        {
            return Ok(Some(Found::Pr(pr)));
        }
        let pending = self.pending_changesets().await?;
        if pending.is_empty() {
            return Ok(Some(Found::Reason(
                "The default branch has no pending changesets and no open version PR.".into(),
            )));
        }
        if self.release_run_in_progress().await? {
            *saw_running = true;
            return Ok(None);
        }
        // The version PR is opened when the run ends; allow one more poll for it.
        if *saw_running && !*grace {
            *grace = true;
            return Ok(None);
        }
        Ok(Some(Found::Reason(format!(
            "{} changeset(s) are pending but no release run is in progress to open a version PR.",
            pending.len()
        ))))
    }

    // ---- step 2 ----

    async fn snapshot(&mut self) -> Result<StepResult> {
        let cfg = self.cfg();
        let wait = cfg.version_pr_wait_minutes;
        let deadline = self.begin_wait("snapshot", wait)?;
        let mut diff = String::new();
        let res = loop {
            if let Some(pr) = self.snapshot_probe(&mut diff).await? {
                break Some(pr);
            }
            if self.pause(deadline).await {
                break None;
            }
        };
        self.end_wait("snapshot")?;
        let Some(pr) = res else {
            return Ok(self.done(
                ShipStatus::TimedOut,
                format!(
                    "Version PR is stale and was not refreshed within {wait} minutes ({diff})."
                ),
                Extra::default(),
            ));
        };
        self.data.pr_number = Some(pr.number);
        self.data.head_sha = Some(pr.head_sha.clone());
        self.save(None, Some(pr.number))?;
        Ok(StepResult::Next(Step::Gate))
    }

    async fn snapshot_probe(&mut self, diff: &mut String) -> Result<Option<PullRequest>> {
        let cfg = self.cfg();
        let github = self.deps.github;
        // The version PR as it is now, following a re-created PR; none while there is none open.
        let Some(pr) = github
            .find_open_pr(&cfg.repo, &cfg.release_branch, &cfg.default_branch)
            .await?
        else {
            return Ok(None);
        };
        let on_branch = changeset_names(
            github
                .list_dir(&cfg.repo, &cfg.default_branch, ".changeset")
                .await?,
        );
        let deleted = changeset_names(
            github
                .list_pr_files(&cfg.repo, pr.number)
                .await?
                .into_iter()
                .filter(|f| f.status == PrFileStatus::Removed && f.path.starts_with(".changeset/"))
                .map(|f| f.path),
        );
        let missing: Vec<String> = on_branch.difference(&deleted).cloned().collect();
        let extra: Vec<String> = deleted.difference(&on_branch).cloned().collect();
        if missing.is_empty() && extra.is_empty() {
            return Ok(Some(pr));
        }
        *diff = format!(
            "not deleted by the PR: [{}]; deleted but not on {}: [{}]",
            missing.join(", "),
            cfg.default_branch,
            extra.join(", ")
        );
        self.deps.logger.info(
            "version PR is stale, waiting for the release run to refresh it",
            Some(json!({ "missing": missing, "extra": extra })),
        );
        Ok(None)
    }

    // ---- steps 3 and 4 ----

    async fn gate(&mut self) -> Result<StepResult> {
        let Some(pr_number) = self.data.pr_number else {
            return Ok(StepResult::Next(Step::FindPr));
        };
        let mut blocked_by: Option<String> = None;
        let wait = self.opts.ci_wait_minutes.unwrap_or(120);
        let deadline = self.begin_wait("ci", wait)?;
        let verdict = loop {
            if let Some(v) = self.ci_probe(pr_number, &mut blocked_by).await? {
                break Some(v);
            }
            if self.pause(deadline).await {
                break None;
            }
        };
        self.end_wait("ci")?;
        let Some(verdict) = verdict else {
            return Ok(match blocked_by {
                Some(reason) => self.done(
                    ShipStatus::Blocked,
                    format!(
                        "CI on the version PR is waiting for an approval ground may not give: {reason}. Approve it by hand; `ground ship` resumes."
                    ),
                    Extra::default(),
                ),
                None => self.done(
                    ShipStatus::TimedOut,
                    "CI on the version PR did not finish in time.".into(),
                    Extra::default(),
                ),
            });
        };
        Ok(match verdict {
            CiVerdict::Green => StepResult::Next(Step::Merge),
            CiVerdict::Restart => StepResult::Next(Step::Snapshot),
            CiVerdict::Plan(plan) => self.done(
                ShipStatus::Planned,
                plan.description.clone(),
                Extra {
                    plan: Some(plan),
                    ..Extra::default()
                },
            ),
            CiVerdict::Aborted(reason) => self.done(ShipStatus::Aborted, reason, Extra::default()),
            CiVerdict::Failed { jobs, url } => {
                let listed = if jobs.is_empty() {
                    "no failing job listed".to_owned()
                } else {
                    jobs.join(", ")
                };
                self.done(
                    ShipStatus::Failed,
                    format!(
                        "CI failed on the version PR after {} re-run(s): {listed}. {}",
                        self.ci_reruns, url
                    ),
                    Extra {
                        failed_jobs: Some(jobs),
                        ..Extra::default()
                    },
                )
            }
        })
    }

    async fn ci_probe(
        &mut self,
        pr_number: u64,
        blocked_by: &mut Option<String>,
    ) -> Result<Option<CiVerdict>> {
        let cfg = self.cfg();
        let github = self.deps.github;
        let repo = cfg.repo.as_str();
        let pr = github.get_pr(repo, pr_number).await?;
        if pr.merged {
            return Ok(Some(CiVerdict::Aborted(format!(
                "PR #{} was merged outside ground.",
                pr.number
            ))));
        }
        if pr.state != PrState::Open {
            return Ok(Some(CiVerdict::Aborted(format!(
                "PR #{} was closed.",
                pr.number
            ))));
        }
        if self.data.head_sha.as_deref() != Some(pr.head_sha.as_str()) {
            self.deps.logger.info(
                "version PR head moved, re-checking its changesets",
                Some(json!({ "from": self.data.head_sha, "to": pr.head_sha })),
            );
            return Ok(Some(CiVerdict::Restart));
        }
        // Re-filtered here because the gate must hold even if a port returns more than asked.
        let filter = RunFilter {
            head_sha: Some(pr.head_sha.clone()),
            event: Some("pull_request".into()),
            ..RunFilter::default()
        };
        let runs: Vec<WorkflowRun> = github
            .list_workflow_runs(repo, &cfg.ci_workflow, &filter)
            .await?
            .into_iter()
            .filter(|r| r.head_sha == pr.head_sha && r.event == "pull_request")
            .collect();
        let Some(run) = newest(runs) else {
            return Ok(None);
        };
        // GitHub's own attempt counter is the record of how often this run was re-run.
        self.ci_reruns = run.run_attempt.saturating_sub(1);

        if self.rerun_requested.get(&run.id).copied().unwrap_or(0) >= run.run_attempt {
            return Ok(None);
        }

        if run.status == "action_required" || run.conclusion.as_deref() == Some("action_required") {
            if let ApprovalDecision::Refused { reason } = can_approve_run(cfg, &pr, &run) {
                if blocked_by.as_deref() != Some(reason.as_str()) {
                    self.deps.logger.warn(
                        "CI run needs approval and ground may not give it",
                        Some(json!({ "runId": run.id, "reason": reason })),
                    );
                }
                *blocked_by = Some(reason);
                return Ok(None);
            }
            let plan = ShipPlan {
                action: ShipPlanAction::Approve,
                run_id: Some(run.id),
                description: format!(
                    "Approve CI run {} ({}) for {} at {}.",
                    run.id,
                    run.html_url,
                    pr.head_ref,
                    short(&pr.head_sha)
                ),
            };
            if !self.opts.confirm {
                return Ok(Some(CiVerdict::Plan(plan)));
            }
            github.approve_run(repo, run.id).await?;
            self.deps.store.record_event(
                repo,
                "ship.approve",
                Some(json!({ "runId": run.id, "headSha": pr.head_sha })),
            )?;
            self.deps
                .logger
                .info("approved CI run", Some(json!({ "runId": run.id })));
            return Ok(None);
        }
        *blocked_by = None;

        if run.status != "completed" {
            return Ok(None);
        }
        if run.conclusion.as_deref() == Some("success") {
            return Ok(Some(CiVerdict::Green));
        }

        let names: Vec<String> = github
            .list_run_jobs(repo, run.id)
            .await?
            .into_iter()
            .filter(|j| !is_ok_job(&j.conclusion) && j.status == "completed")
            .map(|j| j.name)
            .collect();
        if u64::from(self.ci_reruns) >= cfg.max_reruns_per_run {
            return Ok(Some(CiVerdict::Failed {
                jobs: names,
                url: run.html_url,
            }));
        }
        let listed = if names.is_empty() {
            "(none listed)".to_owned()
        } else {
            names.join(", ")
        };
        let plan = ShipPlan {
            action: ShipPlanAction::Rerun,
            run_id: Some(run.id),
            description: format!("Re-run failed jobs of CI run {}: {listed}.", run.id),
        };
        if !self.opts.confirm {
            return Ok(Some(CiVerdict::Plan(plan)));
        }
        github.rerun_failed_jobs(repo, run.id).await?;
        self.rerun_requested.insert(run.id, run.run_attempt);
        for job in &names {
            self.deps
                .store
                .record_flake(repo, run.id, job, run.run_attempt)?;
        }
        self.deps.store.record_event(
            repo,
            "ship.rerun",
            Some(json!({ "runId": run.id, "jobs": names })),
        )?;
        self.deps.logger.warn(
            "re-ran failed CI jobs",
            Some(json!({ "runId": run.id, "jobs": names })),
        );
        Ok(None)
    }

    // ---- step 5 ----

    async fn merge(&mut self) -> Result<StepResult> {
        let cfg = self.cfg();
        let github = self.deps.github;
        let repo = cfg.repo.as_str();
        let (Some(pr_number), Some(head_sha)) = (self.data.pr_number, self.data.head_sha.clone())
        else {
            return Ok(StepResult::Next(Step::FindPr));
        };

        let pr = github.get_pr(repo, pr_number).await?;
        if pr.merged {
            // Merged but we never recorded the commit: a crash between the merge and the save.
            let merge_commit_sha = match pr.merge_commit_sha.clone() {
                Some(sha) => sha,
                None => github.get_branch_sha(repo, &cfg.default_branch).await?,
            };
            self.deps.logger.warn(
                "PR was already merged, taking its merge commit from GitHub",
                Some(json!({ "mergeCommitSha": merge_commit_sha })),
            );
            self.data.merge_commit_sha = Some(merge_commit_sha);
            if self.data.packages.is_none() {
                let scan = scan_bumps(&github.list_pr_files(repo, pr_number).await?);
                self.data.packages = Some(scan.packages);
                self.data.unresolved = Some(scan.unresolved);
            }
            self.save(None, None)?;
            return Ok(StepResult::Next(Step::Release));
        }
        if pr.state != PrState::Open {
            return Ok(self.done(
                ShipStatus::Aborted,
                format!("PR #{} was closed before it could be merged.", pr.number),
                Extra::default(),
            ));
        }
        if pr.head_sha != head_sha {
            return Ok(self.done(
                ShipStatus::Aborted,
                format!(
                    "The version PR head moved from {} to {} after CI passed; not merging. Run `ground ship` again.",
                    short(&head_sha),
                    short(&pr.head_sha)
                ),
                Extra::default(),
            ));
        }

        let scan = scan_bumps(&github.list_pr_files(repo, pr_number).await?);
        let base_was_tip = github.get_branch_sha(repo, &cfg.default_branch).await? == pr.base_sha;
        let how = format!(
            "{}{}",
            cfg.merge_method.as_str(),
            if cfg.admin_merge { " (admin)" } else { "" }
        );
        let tip_note = if base_was_tip {
            String::new()
        } else {
            format!(
                " {} has moved since the PR was built, so the release will re-validate inline.",
                cfg.default_branch
            )
        };
        if !self.opts.confirm {
            let plan = ShipPlan {
                action: ShipPlanAction::Merge,
                run_id: None,
                description: format!(
                    "Merge PR #{} by {how} at {}, publishing {}.{tip_note}",
                    pr.number,
                    short(&pr.head_sha),
                    describe(&scan.packages)
                ),
            };
            return Ok(self.done(
                ShipStatus::Planned,
                plan.description.clone(),
                Extra {
                    plan: Some(plan),
                    packages: Some(scan.packages),
                    ..Extra::default()
                },
            ));
        }

        // Persist what we are about to do before the write that cannot be undone.
        self.data.packages = Some(scan.packages);
        self.data.unresolved = Some(scan.unresolved);
        self.data.base_was_tip = Some(base_was_tip);
        self.save(None, None)?;
        let merged = github
            .merge_pr(
                repo,
                pr_number,
                &MergeOptions {
                    method: cfg.merge_method,
                    admin: cfg.admin_merge,
                    expected_head_sha: head_sha,
                },
            )
            .await?;
        self.data.merge_commit_sha = Some(merged.merge_commit_sha.clone());
        self.save(Some(Step::Release), None)?;
        self.deps.store.record_event(
            repo,
            "ship.merge",
            Some(json!({
                "prNumber": pr_number,
                "mergeCommitSha": merged.merge_commit_sha,
                "baseWasTip": base_was_tip,
            })),
        )?;
        self.deps.logger.info(
            "merged version PR",
            Some(json!({
                "prNumber": pr_number,
                "mergeCommitSha": merged.merge_commit_sha,
                "baseWasTip": base_was_tip,
            })),
        );
        Ok(StepResult::Next(Step::Release))
    }

    // ---- step 6 ----

    async fn release(&mut self) -> Result<StepResult> {
        let Some(sha) = self.data.merge_commit_sha.clone().filter(|s| !s.is_empty()) else {
            return Ok(StepResult::Next(Step::FindPr));
        };
        let wait = self.opts.release_wait_minutes.unwrap_or(90);
        let deadline = self.begin_wait("release", wait)?;
        let mut attempt_seen = 1;
        let verdict = loop {
            if let Some(v) = self.release_probe(&sha, &mut attempt_seen).await? {
                break Some(v);
            }
            if self.pause(deadline).await {
                break None;
            }
        };
        self.end_wait("release")?;
        self.release_reruns = attempt_seen.saturating_sub(1);
        let Some(verdict) = verdict else {
            return Ok(self.done(
                ShipStatus::TimedOut,
                format!(
                    "The release run for {} did not finish in time. The PR is merged; `ground ship` resumes the wait.",
                    short(&sha)
                ),
                Extra {
                    merge_commit_sha: Some(sha),
                    ..Extra::default()
                },
            ));
        };
        match verdict {
            Some((run, jobs)) => {
                let listed = if jobs.is_empty() {
                    "no failing job listed".to_owned()
                } else {
                    jobs.join(", ")
                };
                Ok(self.done(
                    ShipStatus::Failed,
                    format!(
                        "The release run failed after a re-run ({listed}). The PR is merged but nothing is confirmed published. {}",
                        run.html_url
                    ),
                    Extra {
                        failed_jobs: Some(jobs),
                        merge_commit_sha: Some(sha),
                        ..Extra::default()
                    },
                ))
            }
            None => Ok(StepResult::Next(Step::Registry)),
        }
    }

    /// `Some(None)` is green, `Some(Some((run, jobs)))` is failed for good, `None` keeps waiting.
    async fn release_probe(
        &mut self,
        sha: &str,
        attempt_seen: &mut u32,
    ) -> Result<Option<Option<(WorkflowRun, Vec<String>)>>> {
        let cfg = self.cfg();
        let github = self.deps.github;
        let repo = cfg.repo.as_str();
        let filter = RunFilter {
            head_sha: Some(sha.to_owned()),
            ..RunFilter::default()
        };
        let runs: Vec<WorkflowRun> = github
            .list_workflow_runs(repo, &cfg.release_workflow, &filter)
            .await?
            .into_iter()
            .filter(|r| r.head_sha == sha)
            .collect();
        let Some(run) = newest(runs) else {
            return Ok(None);
        };
        *attempt_seen = run.run_attempt;
        if self.rerun_requested.get(&run.id).copied().unwrap_or(0) >= run.run_attempt {
            return Ok(None);
        }
        if run.status != "completed" {
            return Ok(None);
        }
        if run.conclusion.as_deref() == Some("success") {
            return Ok(Some(None));
        }
        let jobs: Vec<String> = github
            .list_run_jobs(repo, run.id)
            .await?
            .into_iter()
            .filter(|j| j.status == "completed" && !is_ok_job(&j.conclusion))
            .map(|j| j.name)
            .collect();
        if run.run_attempt.saturating_sub(1) >= RELEASE_RERUNS {
            return Ok(Some(Some((run, jobs))));
        }
        github.rerun_failed_jobs(repo, run.id).await?;
        self.rerun_requested.insert(run.id, run.run_attempt);
        for job in &jobs {
            self.deps
                .store
                .record_flake(repo, run.id, job, run.run_attempt)?;
        }
        self.deps.store.record_event(
            repo,
            "ship.release_rerun",
            Some(json!({ "runId": run.id, "jobs": jobs })),
        )?;
        self.deps.logger.warn(
            "re-ran failed release jobs",
            Some(json!({ "runId": run.id, "jobs": jobs })),
        );
        Ok(None)
    }

    // ---- step 7 ----

    async fn registry(&mut self) -> Result<StepResult> {
        let packages = self.data.packages.clone().unwrap_or_default();
        // Keyed by name like a JS Map: a repeated name keeps its first position, last value.
        let mut missing: Vec<BumpedPackage> = Vec::new();
        for p in &packages {
            match missing.iter_mut().find(|m| m.name == p.name) {
                Some(slot) => *slot = p.clone(),
                None => missing.push(p.clone()),
            }
        }
        if !packages.is_empty() {
            let wait = self.cfg().registry_lag_minutes;
            let deadline = self.begin_wait("registry", wait)?;
            let confirmed = loop {
                let mut still = Vec::new();
                for pkg in std::mem::take(&mut missing) {
                    let latest = self.deps.registry.latest_version(&pkg.name).await?;
                    if latest.as_deref() != Some(pkg.to.as_str()) {
                        still.push(pkg);
                    }
                }
                missing = still;
                if missing.is_empty() {
                    break true;
                }
                if self.pause(deadline).await {
                    break false;
                }
            };
            self.end_wait("registry")?;
            if !confirmed {
                self.deps.logger.warn(
                    "registry never showed the new versions",
                    Some(json!({ "missing": missing })),
                );
                return Ok(self.done(
                    ShipStatus::RegistryPending,
                    format!(
                        "Release run is green but the registry does not show {} after {} minutes. A green run is not a publish; check the registry again later with `ground ship`.",
                        describe(&missing),
                        wait
                    ),
                    Extra {
                        missing_packages: Some(missing),
                        merge_commit_sha: self.data.merge_commit_sha.clone(),
                        ..Extra::default()
                    },
                ));
            }
        }
        Ok(self.done(
            ShipStatus::Shipped,
            self.shipped_summary(&packages),
            Extra {
                merge_commit_sha: self.data.merge_commit_sha.clone(),
                base_was_tip: self.data.base_was_tip,
                ..Extra::default()
            },
        ))
    }

    fn shipped_summary(&self, packages: &[BumpedPackage]) -> String {
        let confirmed = if packages.is_empty() {
            "no package versions to confirm".to_owned()
        } else {
            format!("{} confirmed on the registry", describe(packages))
        };
        let pr = self
            .data
            .pr_number
            .map_or_else(|| "undefined".to_owned(), |n| n.to_string());
        let mut lines = vec![format!("Shipped PR #{pr}: {confirmed}.")];
        if self.data.base_was_tip == Some(false) {
            lines.push(format!(
                "{} had moved before the merge, so the release re-validated inline.",
                self.cfg().default_branch
            ));
        }
        if self.reruns() > 0 {
            lines.push(format!("Re-runs used: {}.", self.reruns()));
        }
        let unresolved = self.data.unresolved.clone().unwrap_or_default();
        if !unresolved.is_empty() {
            lines.push(format!(
                "Not verified on the registry (package name not visible in the diff): {}.",
                unresolved.join(", ")
            ));
        }
        lines.join(" ")
    }

    // ---- plumbing ----

    /// Starts (or, on a resumed session, continues) the named wait budget and returns its
    /// deadline. The deadline is persisted, and cleared by `end_wait` so the next wait starts
    /// fresh. A malformed stored deadline never expires.
    fn begin_wait(&mut self, key: &str, minutes: u64) -> Result<Option<DateTime<Utc>>> {
        if !self.data.deadlines.contains_key(key) {
            let at = self.deps.clock.now() + Duration::minutes(minutes as i64);
            self.data.deadlines.insert(
                key.to_owned(),
                at.to_rfc3339_opts(SecondsFormat::Millis, true),
            );
            self.save(None, None)?;
        }
        Ok(DateTime::parse_from_rfc3339(&self.data.deadlines[key])
            .ok()
            .map(|d| d.with_timezone(&Utc)))
    }

    fn end_wait(&mut self, key: &str) -> Result<()> {
        self.data.deadlines.remove(key);
        self.save(None, None)
    }

    /// After an unanswered probe: true when the budget is spent, otherwise sleeps one poll.
    /// The probe always runs once before this, so an expired budget still looks.
    async fn pause(&self, deadline: Option<DateTime<Utc>>) -> bool {
        if deadline.is_some_and(|d| self.deps.clock.now() >= d) {
            return true;
        }
        self.deps.clock.sleep(self.cfg().poll_seconds * 1000).await;
        false
    }

    fn reruns(&self) -> u32 {
        self.ci_reruns + self.release_reruns
    }

    async fn pending_changesets(&self) -> Result<BTreeSet<String>> {
        let cfg = self.cfg();
        Ok(changeset_names(
            self.deps
                .github
                .list_dir(&cfg.repo, &cfg.default_branch, ".changeset")
                .await?,
        ))
    }

    async fn release_run_in_progress(&self) -> Result<bool> {
        let cfg = self.cfg();
        let filter = RunFilter {
            branch: Some(cfg.default_branch.clone()),
            ..RunFilter::default()
        };
        let runs = self
            .deps
            .github
            .list_workflow_runs(&cfg.repo, &cfg.release_workflow, &filter)
            .await?;
        Ok(runs.iter().any(|r| r.status != "completed"))
    }

    fn save(&self, step: Option<Step>, pr_number: Option<u64>) -> Result<()> {
        self.deps.store.update_ship_session(
            &self.session_id,
            ShipSessionPatch {
                step: step.map(|s| s.as_str().to_owned()),
                pr_number: pr_number.map(Some),
                data: Some(serde_json::to_value(&self.data)?),
                ..ShipSessionPatch::default()
            },
        )?;
        Ok(())
    }

    fn done(&self, status: ShipStatus, summary: String, extra: Extra) -> StepResult {
        StepResult::Done(Box::new(ShipOutcome {
            status,
            summary,
            session_id: self.session_id.clone(),
            pr_number: self.data.pr_number,
            plan: extra.plan,
            packages: extra
                .packages
                .or_else(|| self.data.packages.clone())
                .unwrap_or_default(),
            merge_commit_sha: extra.merge_commit_sha,
            base_was_tip: extra.base_was_tip,
            failed_jobs: extra.failed_jobs,
            missing_packages: extra.missing_packages,
            reruns: self.reruns(),
        }))
    }

    fn finish(mut self, outcome: ShipOutcome) -> Result<ShipOutcome> {
        let store = self.deps.store;
        self.data.summary = Some(outcome.summary.clone());
        store.update_ship_session(
            &self.session_id,
            ShipSessionPatch {
                data: Some(serde_json::to_value(&self.data)?),
                outcome: Some(
                    (!outcome.status.is_resumable()).then(|| outcome.status.as_str().to_owned()),
                ),
                ..ShipSessionPatch::default()
            },
        )?;
        store.record_event(
            &self.cfg().repo,
            &format!("ship.{}", outcome.status.as_str()),
            Some(json!({
                "sessionId": outcome.session_id,
                "prNumber": outcome.pr_number,
                "summary": outcome.summary,
            })),
        )?;
        self.deps.logger.info(
            &outcome.summary,
            Some(json!({ "status": outcome.status.as_str() })),
        );
        Ok(outcome)
    }
}
