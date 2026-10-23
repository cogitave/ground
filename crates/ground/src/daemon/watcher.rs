use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::SecondsFormat;
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use super::summary::{RecordedState, VersionPrRef, recorded_state, run_signature};
use crate::approval::can_approve_run;
use crate::config::RepoConfig;
use crate::ports::{Clock, GitHubPort, Logger, PortError, PullRequest, RunFilter, WorkflowRun};
use crate::state::{StateError, StateStore};

/// The newest runs only: a first poll must not replay a repository's whole history.
const RUNS_PER_POLL: usize = 10;

#[derive(Debug, thiserror::Error)]
enum PollError {
    #[error(transparent)]
    Port(#[from] PortError),
    #[error(transparent)]
    State(#[from] StateError),
}

/// What the daemon reports live about one watcher.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchStatus {
    pub last_poll_at: Option<String>,
    pub last_error: Option<String>,
}

fn is_action_required(r: &WorkflowRun) -> bool {
    r.status == "action_required" || r.conclusion.as_deref() == Some("action_required")
}

pub struct Watcher<G: GitHubPort, C: Clock> {
    config: RepoConfig,
    github: Arc<G>,
    store: Arc<StateStore>,
    clock: C,
    log: Arc<dyn Logger>,
    status: Arc<Mutex<WatchStatus>>,
    seen: RecordedState,
    approve_tried: HashSet<u64>,
}

impl<G: GitHubPort, C: Clock> Watcher<G, C> {
    pub fn new(
        config: RepoConfig,
        github: Arc<G>,
        store: Arc<StateStore>,
        clock: C,
        log: Arc<dyn Logger>,
    ) -> Result<Self, StateError> {
        let seen = recorded_state(&store, &config.repo)?;
        Ok(Self {
            config,
            github,
            store,
            clock,
            log,
            status: Arc::default(),
            seen,
            approve_tried: HashSet::new(),
        })
    }

    /// A handle the daemon reads while the loop runs.
    pub fn status_handle(&self) -> Arc<Mutex<WatchStatus>> {
        self.status.clone()
    }

    pub fn last_poll_at(&self) -> Option<String> {
        self.lock_status().last_poll_at.clone()
    }

    pub fn last_error(&self) -> Option<String> {
        self.lock_status().last_error.clone()
    }

    fn lock_status(&self) -> std::sync::MutexGuard<'_, WatchStatus> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One poll. Records only what changed since the last one; never fails.
    pub async fn tick(&mut self) {
        match self.poll().await {
            Ok(()) => self.lock_status().last_error = None,
            Err(err) => {
                let msg = err.to_string();
                // Log each distinct failure once, not once per poll.
                if self.last_error().as_deref() != Some(msg.as_str()) {
                    self.log.error(
                        "poll failed",
                        Some(json!({"repo": self.config.repo, "error": msg})),
                    );
                }
                self.lock_status().last_error = Some(msg);
            }
        }
        let at = self
            .clock
            .now()
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        self.lock_status().last_poll_at = Some(at);
    }

    /// Polls until `stop` flips to true (or its sender is dropped). Sleeping is
    /// raced against it so shutdown is prompt.
    pub async fn run(&mut self, mut stop: watch::Receiver<bool>) {
        loop {
            self.tick().await;
            if *stop.borrow() {
                break;
            }
            tokio::select! {
                () = self.clock.sleep(self.config.poll_seconds * 1000) => {}
                changed = stop.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
            }
            if *stop.borrow() {
                break;
            }
            // A clock whose sleep returns at once must not starve other tasks.
            tokio::task::yield_now().await;
        }
    }

    async fn poll(&mut self) -> Result<(), PollError> {
        let repo = self.config.repo.clone();
        let default_branch = self.config.default_branch.clone();
        let release_branch = self.config.release_branch.clone();

        let tip = self.github.get_branch_sha(&repo, &default_branch).await?;
        if self.seen.main_tip.as_deref() != Some(tip.as_str()) {
            self.store.record_event(
                &repo,
                "main.tip",
                Some(json!({"sha": tip, "previous": self.seen.main_tip})),
            )?;
            self.seen.main_tip = Some(tip);
        }

        let pr = self
            .github
            .find_open_pr(&repo, &release_branch, &default_branch)
            .await?;
        self.record_version_pr(pr.as_ref())?;

        let filter = match &pr {
            Some(pr) => RunFilter {
                head_sha: Some(pr.head_sha.clone()),
                event: Some("pull_request".into()),
                ..Default::default()
            },
            None => RunFilter {
                branch: Some(release_branch.clone()),
                event: Some("pull_request".into()),
                ..Default::default()
            },
        };
        let ci_runs = self
            .github
            .list_workflow_runs(&repo, &self.config.ci_workflow, &filter)
            .await?;
        let release_runs = self
            .github
            .list_workflow_runs(
                &repo,
                &self.config.release_workflow,
                &RunFilter {
                    branch: Some(default_branch.clone()),
                    ..Default::default()
                },
            )
            .await?;
        for run in ci_runs
            .iter()
            .chain(release_runs.iter())
            .take(RUNS_PER_POLL * 2)
        {
            self.record_run(run)?;
        }

        if let Some(pr) = &pr {
            for run in ci_runs.iter().filter(|r| is_action_required(r)) {
                self.maybe_approve(pr, run).await?;
            }
        }
        Ok(())
    }

    fn record_version_pr(&mut self, pr: Option<&PullRequest>) -> Result<(), StateError> {
        let repo = &self.config.repo;
        let prev = self.seen.version_pr.clone().flatten();
        if let Some(pr) = pr {
            let next = VersionPrRef {
                number: pr.number,
                head_sha: pr.head_sha.clone(),
            };
            if prev.as_ref() == Some(&next) {
                return Ok(());
            }
            let same_number = prev.as_ref().is_some_and(|p| p.number == next.number);
            let mut payload = Map::new();
            payload.insert("number".into(), json!(next.number));
            payload.insert("headSha".into(), json!(next.head_sha));
            if same_number {
                payload.insert(
                    "previousHeadSha".into(),
                    json!(prev.as_ref().map(|p| p.head_sha.clone())),
                );
            }
            let kind = if same_number {
                "version-pr.head-changed"
            } else {
                "version-pr.appeared"
            };
            self.store
                .record_event(repo, kind, Some(Value::Object(payload)))?;
            self.seen.version_pr = Some(Some(next));
        } else if let Some(prev) = prev {
            self.store.record_event(
                repo,
                "version-pr.gone",
                Some(json!({"number": prev.number})),
            )?;
            self.seen.version_pr = Some(None);
        }
        Ok(())
    }

    fn record_run(&mut self, run: &WorkflowRun) -> Result<(), StateError> {
        let sig = run_signature(&run.status, run.conclusion.as_deref(), run.run_attempt);
        if self.seen.runs.get(&run.id) == Some(&sig) {
            return Ok(());
        }
        self.store.record_event(
            &self.config.repo,
            "run.state",
            Some(json!({
                "runId": run.id,
                "workflow": run.workflow_file,
                "status": run.status,
                "conclusion": run.conclusion,
                "attempt": run.run_attempt,
                "headSha": run.head_sha,
                "branch": run.head_branch,
                "url": run.html_url,
            })),
        )?;
        self.seen.runs.insert(run.id, sig);
        Ok(())
    }

    async fn maybe_approve(
        &mut self,
        listed: &PullRequest,
        run: &WorkflowRun,
    ) -> Result<(), PollError> {
        if self.approve_tried.contains(&run.id) || !self.config.approve_bot_runs {
            return Ok(());
        }
        // The head may have moved since the list; decide on the PR as it is now.
        let pr = self.github.get_pr(&self.config.repo, listed.number).await?;
        let decision = can_approve_run(&self.config, &pr, run);
        if let crate::approval::ApprovalDecision::Refused { reason } = decision {
            self.log.debug(
                "not approving run",
                Some(json!({"runId": run.id, "reason": reason})),
            );
            return Ok(());
        }
        self.approve_tried.insert(run.id);
        self.github.approve_run(&self.config.repo, run.id).await?;
        self.store.record_event(
            &self.config.repo,
            "run.approved",
            Some(json!({"runId": run.id, "prNumber": pr.number, "headSha": pr.head_sha})),
        )?;
        self.log.info(
            "approved run",
            Some(json!({"repo": self.config.repo, "runId": run.id})),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::LogLevel;
    use crate::testing::{
        FakeClock, FakeGitHub, MemoryLogger, RunScript, RunStage, WriteMethod, make_pr, make_run,
    };

    const REPO: &str = "cogitave/namzu";

    fn config() -> RepoConfig {
        crate::daemon::test_support::config(REPO)
    }

    struct Rig {
        gh: FakeGitHub,
        clock: FakeClock,
        store: Arc<StateStore>,
        log: MemoryLogger,
    }

    impl Rig {
        fn new() -> Self {
            let gh = FakeGitHub::new();
            gh.set_branch_sha(REPO, "main", "tip-1");
            let clock = FakeClock::default();
            let store = Arc::new(StateStore::new(clock.clone(), ":memory:").unwrap());
            Self {
                gh,
                clock,
                store,
                log: MemoryLogger::new(),
            }
        }

        fn watcher(&self, cfg: RepoConfig) -> Watcher<FakeGitHub, FakeClock> {
            Watcher::new(
                cfg,
                Arc::new(self.gh.clone()),
                self.store.clone(),
                self.clock.clone(),
                Arc::new(self.log.clone()),
            )
            .unwrap()
        }

        /// Event kinds, oldest first.
        fn kinds(&self) -> Vec<String> {
            let mut k: Vec<_> = self
                .store
                .list_events(REPO, 50)
                .unwrap()
                .into_iter()
                .map(|e| e.kind)
                .collect();
            k.reverse();
            k
        }
    }

    #[tokio::test]
    async fn records_the_tip_once_and_again_only_when_it_moves() {
        let rig = Rig::new();
        let mut w = rig.watcher(config());
        w.tick().await;
        w.tick().await;
        rig.gh.set_branch_sha(REPO, "main", "tip-2");
        w.tick().await;
        assert_eq!(rig.kinds(), ["main.tip", "main.tip"]);
        let newest = &rig.store.list_events(REPO, 50).unwrap()[0];
        assert_eq!(newest.payload["sha"], "tip-2");
        assert_eq!(newest.payload["previous"], "tip-1");
    }

    #[tokio::test]
    async fn records_version_pr_appearance_head_change_and_disappearance() {
        let rig = Rig::new();
        let mut w = rig.watcher(config());
        w.tick().await;
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        w.tick().await;
        w.tick().await;
        rig.gh
            .update_pr(REPO, 1, |p| p.head_sha = "head-sha-2".into());
        w.tick().await;
        rig.gh
            .update_pr(REPO, 1, |p| p.state = crate::ports::PrState::Closed);
        w.tick().await;
        assert_eq!(
            rig.kinds(),
            [
                "main.tip",
                "version-pr.appeared",
                "version-pr.head-changed",
                "version-pr.gone"
            ]
        );
    }

    #[tokio::test]
    async fn records_a_run_only_when_its_state_changes() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        rig.gh.add_run(
            REPO,
            make_run(|_| {}),
            RunScript {
                stages: vec![
                    RunStage::status("in_progress"),
                    RunStage::status("in_progress"),
                    RunStage::completed("success"),
                ],
                ..Default::default()
            },
        );
        let mut w = rig.watcher(config());
        for _ in 0..4 {
            w.tick().await;
        }
        let mut states: Vec<String> = rig
            .store
            .list_events(REPO, 50)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == "run.state")
            .map(|e| e.payload["status"].as_str().unwrap().to_owned())
            .collect();
        states.reverse();
        assert_eq!(states, ["queued", "in_progress", "completed"]);
    }

    #[tokio::test]
    async fn does_not_repeat_events_after_a_restart() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        rig.watcher(config()).tick().await;
        let before = rig.kinds();
        rig.watcher(config()).tick().await;
        assert_eq!(rig.kinds(), before);
    }

    #[tokio::test]
    async fn logs_a_failing_poll_once_and_keeps_going() {
        let rig = Rig::new();
        let mut w = Watcher::new(
            config(),
            Arc::new(FakeGitHub::new()),
            rig.store.clone(),
            rig.clock.clone(),
            Arc::new(rig.log.clone()),
        )
        .unwrap();
        w.tick().await;
        w.tick().await;
        assert_eq!(rig.log.messages(Some(LogLevel::Error)), ["poll failed"]);
        assert!(w.last_error().unwrap().contains("no branch"));
        assert!(w.last_poll_at().is_some());
    }

    fn action_required() -> crate::ports::WorkflowRun {
        make_run(|r| {
            r.status = "completed".into();
            r.conclusion = Some("action_required".into());
        })
    }

    fn opted_in() -> RepoConfig {
        RepoConfig {
            approve_bot_runs: true,
            ..config()
        }
    }

    #[tokio::test]
    async fn approves_an_action_required_run_for_the_pr_head_once() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        rig.gh.add_run(
            REPO,
            action_required(),
            RunScript {
                on_approve: Some(vec![RunStage::status("in_progress")]),
                ..Default::default()
            },
        );
        let mut w = rig.watcher(opted_in());
        w.tick().await;
        w.tick().await;
        assert_eq!(rig.gh.writes_of(WriteMethod::ApproveRun).len(), 1);
        assert!(rig.kinds().contains(&"run.approved".to_owned()));
    }

    /// Hard rule 3: the daemon approves runs but never merges, whatever the PR
    /// and CI look like.
    #[tokio::test]
    async fn never_merges_even_with_a_green_approved_pr() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        rig.gh.add_run(
            REPO,
            action_required(),
            RunScript {
                on_approve: Some(vec![RunStage::status("in_progress")]),
                ..Default::default()
            },
        );
        let mut w = rig.watcher(opted_in());
        for _ in 0..4 {
            w.tick().await;
            rig.gh.set_branch_sha(REPO, "main", "tip-next");
        }
        assert!(rig.gh.writes_of(WriteMethod::MergePr).is_empty());
        assert!(
            rig.gh
                .writes()
                .iter()
                .all(|w| w.method != WriteMethod::MergePr)
        );
    }

    #[tokio::test]
    async fn does_nothing_when_the_repo_has_not_opted_in() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        rig.gh
            .add_run(REPO, action_required(), RunScript::default());
        rig.watcher(config()).tick().await;
        assert!(rig.gh.writes_of(WriteMethod::ApproveRun).is_empty());
    }

    #[tokio::test]
    async fn never_approves_a_fork_head() {
        let rig = Rig::new();
        rig.gh
            .set_pr(REPO, make_pr(|p| p.head_repo = "mallory/namzu".into()));
        rig.gh
            .add_run(REPO, action_required(), RunScript::default());
        rig.watcher(opted_in()).tick().await;
        assert!(rig.gh.writes_of(WriteMethod::ApproveRun).is_empty());
    }

    #[tokio::test]
    async fn never_approves_a_run_for_an_older_sha() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        let mut run = action_required();
        run.head_sha = "old".into();
        rig.gh.add_run(REPO, run, RunScript::default());
        rig.watcher(opted_in()).tick().await;
        assert!(rig.gh.writes_of(WriteMethod::ApproveRun).is_empty());
    }

    /// A clock whose sleep never returns: only the stop signal can end `run`.
    #[derive(Clone)]
    struct NeverSleeps(FakeClock);

    impl Clock for NeverSleeps {
        fn now(&self) -> chrono::DateTime<chrono::Utc> {
            self.0.now()
        }
        async fn sleep(&self, _ms: u64) {
            std::future::pending::<()>().await;
        }
    }

    #[tokio::test]
    async fn run_stops_when_the_stop_signal_fires() {
        let rig = Rig::new();
        rig.gh.set_pr(REPO, make_pr(|_| {}));
        let mut w = Watcher::new(
            config(),
            Arc::new(rig.gh.clone()),
            rig.store.clone(),
            NeverSleeps(rig.clock.clone()),
            Arc::new(rig.log.clone()),
        )
        .unwrap();
        let (stop, rx) = watch::channel(false);
        stop.send(true).unwrap();
        w.run(rx).await;
        assert!(w.last_poll_at().is_some());
    }

    #[tokio::test]
    async fn run_wakes_from_its_sleep_when_stopped() {
        let rig = Rig::new();
        let mut w = Watcher::new(
            config(),
            Arc::new(rig.gh.clone()),
            rig.store.clone(),
            NeverSleeps(rig.clock.clone()),
            Arc::new(rig.log.clone()),
        )
        .unwrap();
        let (stop, rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            w.run(rx).await;
            w
        });
        // The first poll finishes, then the loop parks in the never-returning sleep.
        tokio::task::yield_now().await;
        stop.send(true).unwrap();
        let w = handle.await.unwrap();
        assert!(w.last_poll_at().is_some());
    }
}
