use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::Notify;

use super::{ShipDeps, ShipError, ShipOptions, ShipOutcome, ShipPlanAction, ShipStatus, ship};
use crate::config::RepoConfig;
use crate::ports::{
    Clock, GitHubPort, MergeMethod, MergeOptions, MergeResult, PortError, PortResult, PrFile,
    PrFileStatus, PrState, PullRequest, RunFilter, RunJob, WorkflowRun,
};
use crate::state::{ShipSessionPatch, StateStore};
use crate::testing::{
    FakeClock, FakeGitHub, FakeRegistry, MemoryLogger, RunScript, RunStage, WriteMethod, make_pr,
    make_run,
};

const REPO: &str = "cogitave/namzu";
const SDK_PATCH: &str = "@@ -1,5 +1,5 @@\n {\n   \"name\": \"@namzu/sdk\",\n-  \"version\": \"1.0.0\",\n+  \"version\": \"1.1.0\",\n   \"type\": \"module\",";

fn base_config() -> RepoConfig {
    RepoConfig {
        repo: REPO.into(),
        default_branch: "main".into(),
        adapter: "pnpm-changesets".into(),
        ci_workflow: "ci.yml".into(),
        release_workflow: "release.yml".into(),
        release_branch: "changeset-release/main".into(),
        approve_bot_runs: false,
        approve_branches: vec!["changeset-release/main".into()],
        admin_merge: true,
        merge_method: MergeMethod::Squash,
        max_reruns_per_run: 1,
        version_pr_wait_minutes: 45,
        registry_lag_minutes: 10,
        poll_seconds: 30,
    }
}

type Hook = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// Virtual clock that lets a test act between two polls (hook returns false) or break the
/// run off (hook returns true: the sleep never returns and `interrupted` is notified).
#[derive(Clone)]
struct HookClock {
    base: FakeClock,
    count: Arc<AtomicU32>,
    hook: Arc<Mutex<Option<Hook>>>,
    interrupted: Arc<Notify>,
}

impl HookClock {
    fn new() -> Self {
        Self {
            base: FakeClock::default(),
            count: Arc::new(AtomicU32::new(0)),
            hook: Arc::new(Mutex::new(None)),
            interrupted: Arc::new(Notify::new()),
        }
    }
    fn set_hook(&self, f: impl Fn(u32) -> bool + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(f));
    }
    fn count(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }
}

impl Clock for HookClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.base.now()
    }
    async fn sleep(&self, ms: u64) {
        self.base.sleep(ms).await;
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        let hook = self.hook.lock().unwrap().clone();
        if hook.is_some_and(|h| h(n)) {
            self.interrupted.notify_one();
            std::future::pending::<()>().await;
        }
    }
}

#[derive(Default)]
struct Lag {
    stale: Option<Vec<WorkflowRun>>,
    pending: bool,
}

/// Wraps the fake. `move_to` moves the PR head right after CI reports green (between the gate
/// and the merge); `lagging` makes GitHub list the old CI attempt once more after a re-run.
struct Port {
    inner: FakeGitHub,
    move_to: Option<String>,
    lagging: bool,
    lag: Mutex<Lag>,
    faults: Mutex<Faults>,
}

#[derive(Clone, Copy)]
enum Fault {
    /// `PortError::Transient`.
    Transient,
    /// `PortError::Http` 404.
    NotFound,
    /// `PortError::Message`.
    Message,
}

#[derive(Default)]
struct Faults {
    /// Operation name to (failures left, kind). `u32::MAX` is an outage that never ends.
    pending: std::collections::HashMap<&'static str, (u32, Fault)>,
    calls: std::collections::HashMap<&'static str, u32>,
    /// Once a re-run succeeds, this operation starts failing: a read right after the write.
    after_rerun: Option<(&'static str, u32, Fault)>,
    /// The same, for an approval.
    after_approve: Option<(&'static str, u32, Fault)>,
}

impl Port {
    /// Counts the call and fails it while this operation has failures scripted.
    fn trip(&self, op: &'static str) -> PortResult<()> {
        let mut faults = self.faults.lock().unwrap();
        *faults.calls.entry(op).or_default() += 1;
        let Some((left, kind)) = faults.pending.get_mut(op) else {
            return Ok(());
        };
        if *left == 0 {
            return Ok(());
        }
        *left -= 1;
        Err(match kind {
            Fault::Transient => PortError::Transient(format!("{op} fell over")),
            Fault::NotFound => PortError::Http {
                service: "GitHub".into(),
                status: 404,
                detail: Some(format!("{op} missing")),
            },
            Fault::Message => PortError::msg(format!("{op} broke")),
        })
    }
}

impl GitHubPort for Port {
    async fn find_open_pr(&self, r: &str, h: &str, b: &str) -> PortResult<Option<PullRequest>> {
        self.trip("find_open_pr")?;
        self.inner.find_open_pr(r, h, b).await
    }
    async fn get_pr(&self, r: &str, n: u64) -> PortResult<PullRequest> {
        self.trip("get_pr")?;
        self.inner.get_pr(r, n).await
    }
    async fn list_pr_files(&self, r: &str, n: u64) -> PortResult<Vec<PrFile>> {
        self.trip("list_pr_files")?;
        self.inner.list_pr_files(r, n).await
    }
    async fn list_dir(&self, r: &str, g: &str, p: &str) -> PortResult<Vec<String>> {
        self.trip("list_dir")?;
        self.inner.list_dir(r, g, p).await
    }
    async fn get_branch_sha(&self, r: &str, b: &str) -> PortResult<String> {
        self.inner.get_branch_sha(r, b).await
    }
    async fn list_workflow_runs(
        &self,
        repo: &str,
        wf: &str,
        filter: &RunFilter,
    ) -> PortResult<Vec<WorkflowRun>> {
        self.trip(if wf == "ci.yml" {
            "runs:ci"
        } else {
            "runs:release"
        })?;
        if self.lagging && wf == "ci.yml" {
            let stale = {
                let mut lag = self.lag.lock().unwrap();
                if lag.pending && lag.stale.is_some() {
                    lag.pending = false;
                    lag.stale.clone()
                } else {
                    None
                }
            };
            if let Some(stale) = stale {
                return Ok(stale);
            }
        }
        let runs = self.inner.list_workflow_runs(repo, wf, filter).await?;
        if let Some(to) = &self.move_to
            && runs
                .iter()
                .any(|r| r.conclusion.as_deref() == Some("success"))
        {
            self.inner.update_pr(REPO, 1, |p| p.head_sha.clone_from(to));
        }
        Ok(runs)
    }
    async fn list_run_jobs(&self, r: &str, id: u64) -> PortResult<Vec<RunJob>> {
        self.trip("list_run_jobs")?;
        self.inner.list_run_jobs(r, id).await
    }
    async fn approve_run(&self, r: &str, id: u64) -> PortResult<()> {
        self.trip("approve_run")?;
        self.inner.approve_run(r, id).await?;
        let mut faults = self.faults.lock().unwrap();
        if let Some((op, n, kind)) = faults.after_approve.take() {
            faults.pending.insert(op, (n, kind));
        }
        Ok(())
    }
    async fn rerun_failed_jobs(&self, r: &str, id: u64) -> PortResult<()> {
        self.trip("rerun_failed_jobs")?;
        if self.lagging {
            let stale = self
                .inner
                .list_workflow_runs(r, "ci.yml", &RunFilter::default())
                .await?;
            self.lag.lock().unwrap().stale = Some(stale);
        }
        self.inner.rerun_failed_jobs(r, id).await?;
        {
            let mut faults = self.faults.lock().unwrap();
            if let Some((op, n, kind)) = faults.after_rerun.take() {
                faults.pending.insert(op, (n, kind));
            }
        }
        self.lag.lock().unwrap().pending = true;
        Ok(())
    }
    async fn merge_pr(&self, r: &str, n: u64, o: &MergeOptions) -> PortResult<MergeResult> {
        self.inner.merge_pr(r, n, o).await
    }
}

fn green(n: usize) -> Vec<RunStage> {
    let mut v = vec![RunStage::status("in_progress"); n];
    v.push(RunStage::completed("success"));
    v
}

fn stages(stages: Vec<RunStage>) -> RunScript {
    RunScript {
        stages,
        ..RunScript::default()
    }
}

fn release_run(sha: &str, tweak: impl FnOnce(&mut WorkflowRun)) -> WorkflowRun {
    make_run(|r| {
        r.id = 200;
        r.workflow_file = "release.yml".into();
        r.event = "push".into();
        r.head_sha = sha.into();
        r.head_branch = "main".into();
        tweak(r);
    })
}

fn removed(path: &str) -> PrFile {
    PrFile {
        path: path.into(),
        status: PrFileStatus::Removed,
        patch: None,
    }
}

fn sdk_file() -> PrFile {
    PrFile {
        path: "packages/sdk/package.json".into(),
        status: PrFileStatus::Modified,
        patch: Some(SDK_PATCH.into()),
    }
}

fn run_action_required() -> WorkflowRun {
    make_run(|r| {
        r.status = "completed".into();
        r.conclusion = Some("action_required".into());
    })
}

fn run_with(status: &str, conclusion: &str) -> WorkflowRun {
    make_run(|r| {
        r.status = status.into();
        r.conclusion = Some(conclusion.into());
    })
}

struct World {
    config: RepoConfig,
    gh: FakeGitHub,
    port: Port,
    clock: HookClock,
    registry: FakeRegistry,
    store: StateStore,
    logger: MemoryLogger,
}

struct Build {
    config: Box<dyn FnOnce(&mut RepoConfig)>,
    pr: Box<dyn FnOnce(&mut PullRequest)>,
    move_to: Option<String>,
    lagging: bool,
}

impl Default for Build {
    fn default() -> Self {
        Self {
            config: Box::new(|_| {}),
            pr: Box::new(|_| {}),
            move_to: None,
            lagging: false,
        }
    }
}

impl World {
    fn new() -> Self {
        Self::build(Build::default())
    }

    fn with_config(f: impl FnOnce(&mut RepoConfig) + 'static) -> Self {
        Self::build(Build {
            config: Box::new(f),
            ..Build::default()
        })
    }

    fn build(b: Build) -> Self {
        let mut config = base_config();
        (b.config)(&mut config);
        let gh = FakeGitHub::new();
        let clock = HookClock::new();
        let registry = FakeRegistry::new();
        registry.set_version("@namzu/sdk", "1.0.0");
        let store = StateStore::new(clock.clone(), ":memory:").unwrap();
        gh.set_pr(REPO, make_pr(b.pr))
            .set_pr_files(
                REPO,
                1,
                vec![
                    removed(".changeset/a.md"),
                    removed(".changeset/b.md"),
                    sdk_file(),
                ],
            )
            .set_dir(
                REPO,
                "main",
                ".changeset",
                &["a.md", "b.md", "README.md", "config.json"],
            )
            .set_branch_sha(REPO, "main", "base-sha-1")
            .set_jobs(
                REPO,
                100,
                vec![
                    RunJob {
                        name: "lint".into(),
                        status: "completed".into(),
                        conclusion: Some("success".into()),
                    },
                    RunJob {
                        name: "test".into(),
                        status: "completed".into(),
                        conclusion: Some("failure".into()),
                    },
                ],
            );
        // The release run appears when the merge lands, like GitHub's push trigger.
        let (g2, r2) = (gh.clone(), registry.clone());
        gh.on_merge(move |_pr, sha| {
            g2.add_run(REPO, release_run(sha, |_| {}), stages(green(0)));
            r2.set_version("@namzu/sdk", "1.1.0");
        });
        let port = Port {
            inner: gh.clone(),
            move_to: b.move_to,
            lagging: b.lagging,
            lag: Mutex::new(Lag::default()),
            faults: Mutex::new(Faults::default()),
        };
        Self {
            config,
            gh,
            port,
            clock,
            registry,
            store,
            logger: MemoryLogger::new(),
        }
    }

    async fn run_with<C: Clock>(
        &self,
        clock: &C,
        opts: ShipOptions,
    ) -> Result<ShipOutcome, ShipError> {
        let deps = ShipDeps {
            config: &self.config,
            github: &self.port,
            registry: &self.registry,
            clock,
            logger: &self.logger,
            store: &self.store,
        };
        ship(&deps, &opts).await
    }

    async fn go_opts(&self, opts: ShipOptions) -> ShipOutcome {
        self.run_with(&self.clock, opts).await.unwrap()
    }

    async fn go(&self) -> ShipOutcome {
        self.go_opts(ShipOptions {
            confirm: true,
            ..ShipOptions::default()
        })
        .await
    }

    async fn dry(&self) -> ShipOutcome {
        self.go_opts(ShipOptions::default()).await
    }

    async fn go_ci_wait(&self, minutes: u64) -> ShipOutcome {
        self.go_opts(ShipOptions {
            confirm: true,
            ci_wait_minutes: Some(minutes),
            ..ShipOptions::default()
        })
        .await
    }

    /// Runs with `clock` until its hook interrupts, then drops the run (a crash).
    async fn go_until_interrupted(&self, clock: &HookClock) {
        let opts = ShipOptions {
            confirm: true,
            ..ShipOptions::default()
        };
        tokio::select! {
            r = self.run_with(clock, opts) => panic!("the run was expected to be interrupted, got {r:?}"),
            () = clock.interrupted.notified() => {}
        }
    }

    fn active(&self) -> Option<crate::state::ShipSession> {
        self.store.get_active_ship_session(REPO).unwrap()
    }

    /// The next `n` calls of `op` fail with `kind`.
    fn fail(&self, op: &'static str, n: u32, kind: Fault) {
        self.port
            .faults
            .lock()
            .unwrap()
            .pending
            .insert(op, (n, kind));
    }

    fn calls(&self, op: &'static str) -> u32 {
        self.port
            .faults
            .lock()
            .unwrap()
            .calls
            .get(op)
            .copied()
            .unwrap_or(0)
    }

    fn warned(&self) -> usize {
        self.logger
            .messages(Some(crate::ports::LogLevel::Warn))
            .iter()
            .filter(|m| m.contains("unanswered"))
            .count()
    }

    fn writes(&self, m: WriteMethod) -> usize {
        self.gh.writes_of(m).len()
    }
}

// ship.test.ts: ship: happy path

#[tokio::test]
async fn dry_run_stops_before_the_first_write_and_leaves_the_session_resumable() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let out = w.dry().await;
    assert_eq!(out.status, ShipStatus::Planned);
    let plan = out.plan.unwrap();
    assert_eq!(plan.action, ShipPlanAction::Merge);
    assert!(plan.description.contains("@namzu/sdk@1.1.0"));
    assert!(w.gh.writes().is_empty());
    assert!(w.active().is_some());
}

#[tokio::test]
async fn with_confirm_merges_once_at_the_checked_head_and_confirms_the_registry() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(out.packages.len(), 1);
    assert_eq!(out.packages[0].name, "@namzu/sdk");
    assert_eq!(out.packages[0].from.as_deref(), Some("1.0.0"));
    assert_eq!(out.packages[0].to, "1.1.0");
    let merges = w.gh.writes_of(WriteMethod::MergePr);
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].args["method"], "squash");
    assert_eq!(merges[0].args["admin"], true);
    assert_eq!(merges[0].args["expectedHeadSha"], "head-sha-1");
    assert_eq!(out.base_was_tip, Some(true));
    assert!(out.summary.contains("@namzu/sdk@1.1.0"));
    assert!(w.active().is_none());
    assert_eq!(
        w.store
            .get_ship_session(&out.session_id)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("shipped")
    );
}

#[tokio::test]
async fn reports_that_the_base_moved_when_it_is_no_longer_the_tip() {
    let w = World::new();
    w.gh.set_branch_sha(REPO, "main", "someone-else");
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let out = w.go().await;
    assert_eq!(out.base_was_tip, Some(false));
    assert!(out.summary.contains("re-validated inline"));
}

// ship.test.ts: ship: finding the version PR

#[tokio::test]
async fn reports_nothing_to_ship_without_pending_changesets() {
    let w = World::new();
    w.gh.update_pr(REPO, 1, |p| p.state = PrState::Closed);
    w.gh.set_dir(REPO, "main", ".changeset", &["README.md", "config.json"]);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::NothingToShip);
    assert!(w.clock.base.sleeps().is_empty());
}

#[tokio::test]
async fn reports_nothing_to_ship_when_changesets_are_pending_but_no_release_run_is_going() {
    let w = World::new();
    w.gh.update_pr(REPO, 1, |p| p.state = PrState::Closed);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::NothingToShip);
    assert!(out.summary.contains("no release run"));
}

#[tokio::test]
async fn waits_for_the_running_release_to_open_the_pr_then_ships() {
    let w = World::new();
    w.gh.update_pr(REPO, 1, |p| p.state = PrState::Closed);
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 50;
            r.workflow_file = "release.yml".into();
            r.event = "push".into();
            r.head_branch = "main".into();
            r.head_sha = "x".into();
        }),
        stages(vec![
            RunStage::status("in_progress"),
            RunStage::status("in_progress"),
            RunStage::completed("success"),
        ]),
    );
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let gh = w.gh.clone();
    w.clock.set_hook(move |n| {
        if n == 3 {
            gh.update_pr(REPO, 1, |p| p.state = PrState::Open);
        }
        false
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert!(w.clock.count() >= 3);
}

// ship.test.ts: ship: snapshot check

#[tokio::test]
async fn waits_for_a_stale_pr_to_be_refreshed_then_gates_the_new_head() {
    let w = World::new();
    w.gh.set_dir(
        REPO,
        "main",
        ".changeset",
        &["a.md", "b.md", "c.md", "README.md"],
    );
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 101;
            r.head_sha = "head-sha-2".into();
        }),
        stages(green(1)),
    );
    let gh = w.gh.clone();
    w.clock.set_hook(move |n| {
        if n == 2 {
            gh.update_pr(REPO, 1, |p| p.head_sha = "head-sha-2".into());
            gh.set_pr_files(
                REPO,
                1,
                vec![
                    removed(".changeset/a.md"),
                    removed(".changeset/b.md"),
                    removed(".changeset/c.md"),
                    sdk_file(),
                ],
            );
        }
        false
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(
        w.gh.writes_of(WriteMethod::MergePr)[0].args["expectedHeadSha"],
        "head-sha-2"
    );
}

#[tokio::test]
async fn snapshot_times_out_resumably_when_the_pr_is_never_refreshed() {
    let w = World::with_config(|c| c.version_pr_wait_minutes = 2);
    w.gh.set_dir(REPO, "main", ".changeset", &["a.md", "b.md", "c.md"]);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert!(out.summary.contains("c.md"));
    assert!(w.gh.writes().is_empty());
    assert!(w.active().is_some());
}

// ship.test.ts: ship: approving CI runs

fn approving() -> World {
    World::with_config(|c| c.approve_bot_runs = true)
}

fn on_approve() -> RunScript {
    RunScript {
        on_approve: Some(vec![
            RunStage::status("in_progress"),
            RunStage::completed("success"),
        ]),
        ..RunScript::default()
    }
}

#[tokio::test]
async fn approves_the_current_head_when_enabled() {
    let w = approving();
    w.gh.add_run(REPO, run_action_required(), on_approve());
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::ApproveRun), 1);
}

#[tokio::test]
async fn plans_the_approval_in_a_dry_run_without_writing() {
    let w = approving();
    w.gh.add_run(REPO, run_action_required(), RunScript::default());
    let out = w.dry().await;
    assert_eq!(out.status, ShipStatus::Planned);
    let plan = out.plan.unwrap();
    assert_eq!(plan.action, ShipPlanAction::Approve);
    assert_eq!(plan.run_id, Some(100));
    assert!(w.gh.writes().is_empty());
}

#[tokio::test]
async fn does_not_approve_when_disabled_and_reports_it_as_blocked() {
    let w = World::new();
    w.gh.add_run(REPO, run_action_required(), RunScript::default());
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::Blocked);
    assert!(out.summary.contains("approveBotRuns"));
    assert!(w.gh.writes().is_empty());
}

#[tokio::test]
async fn ignores_an_action_required_run_for_an_older_head() {
    let w = approving();
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 90;
            r.head_sha = "old-sha".into();
            r.status = "completed".into();
            r.conclusion = Some("action_required".into());
        }),
        RunScript::default(),
    );
    w.gh.add_run(REPO, make_run(|r| r.id = 100), stages(green(1)));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::ApproveRun), 0);
}

#[tokio::test]
async fn does_nothing_while_only_an_older_head_has_a_run() {
    let w = approving();
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 90;
            r.head_sha = "old-sha".into();
            r.status = "completed".into();
            r.conclusion = Some("action_required".into());
        }),
        RunScript::default(),
    );
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert!(w.gh.writes().is_empty());
}

#[tokio::test]
async fn never_approves_a_fork_head_even_with_approval_enabled() {
    let w = World::build(Build {
        config: Box::new(|c| c.approve_bot_runs = true),
        pr: Box::new(|p| p.head_repo = "someone/namzu".into()),
        ..Build::default()
    });
    w.gh.add_run(REPO, run_action_required(), RunScript::default());
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::Blocked);
    assert!(w.gh.writes().is_empty());
}

#[tokio::test]
async fn never_approves_a_run_whose_branch_differs_from_the_pr_head_branch() {
    let w = approving();
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.status = "completed".into();
            r.conclusion = Some("action_required".into());
            r.head_branch = "other".into();
        }),
        RunScript::default(),
    );
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::Blocked);
    assert!(w.gh.writes().is_empty());
}

#[tokio::test]
async fn never_approves_a_branch_outside_the_allow_list() {
    let w = World::with_config(|c| {
        c.approve_bot_runs = true;
        c.approve_branches = vec!["other/branch".into()];
    });
    w.gh.add_run(REPO, run_action_required(), RunScript::default());
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::Blocked);
    assert!(w.gh.writes().is_empty());
}

// ship.test.ts: ship: failures and re-runs

fn on_rerun(stages: Vec<RunStage>) -> RunScript {
    RunScript {
        on_rerun: Some(stages),
        ..RunScript::default()
    }
}

#[tokio::test]
async fn does_not_re_run_twice_while_github_still_lists_the_old_attempt() {
    let w = World::build(Build {
        lagging: true,
        ..Build::default()
    });
    w.gh.add_run(REPO, run_with("completed", "failure"), on_rerun(green(0)));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::RerunFailedJobs), 1);
}

#[tokio::test]
async fn re_runs_once_records_the_flake_then_stops_with_the_failing_jobs() {
    let w = World::new();
    w.gh.add_run(
        REPO,
        run_with("completed", "failure"),
        on_rerun(vec![RunStage::completed("failure")]),
    );
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Failed);
    assert_eq!(out.failed_jobs, Some(vec!["test".to_owned()]));
    assert_eq!(w.writes(WriteMethod::RerunFailedJobs), 1);
    assert!(w.gh.writes_of(WriteMethod::MergePr).is_empty());
    let flakes = w.store.list_flakes(REPO).unwrap();
    assert_eq!(flakes.len(), 1);
    assert_eq!(
        (flakes[0].run_id, flakes[0].job.as_str(), flakes[0].attempt),
        (100, "test", 1)
    );
}

#[tokio::test]
async fn ships_when_the_re_run_passes_and_says_so() {
    let w = World::new();
    w.gh.add_run(REPO, run_with("completed", "failure"), on_rerun(green(0)));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(out.reruns, 1);
    assert_eq!(w.store.list_flakes(REPO).unwrap().len(), 1);
}

#[tokio::test]
async fn plans_the_re_run_in_a_dry_run_without_writing() {
    let w = World::new();
    w.gh.add_run(REPO, run_with("completed", "failure"), RunScript::default());
    let out = w.dry().await;
    let plan = out.plan.unwrap();
    assert_eq!(plan.action, ShipPlanAction::Rerun);
    assert_eq!(plan.run_id, Some(100));
    assert!(w.gh.writes().is_empty());
    assert!(w.store.list_flakes(REPO).unwrap().is_empty());
}

#[tokio::test]
async fn re_runs_a_failed_release_once_then_reports_the_failure() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let g2 = w.gh.clone();
    w.gh.on_merge(move |_pr, sha| {
        g2.add_run(
            REPO,
            release_run(sha, |r| {
                r.status = "completed".into();
                r.conclusion = Some("failure".into());
            }),
            on_rerun(vec![RunStage::completed("failure")]),
        );
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Failed);
    assert_eq!(out.merge_commit_sha.as_deref(), Some("merge-commit-1"));
    assert_eq!(w.writes(WriteMethod::RerunFailedJobs), 1);
}

// ship.test.ts: ship: head check before merge

#[tokio::test]
async fn aborts_without_merging_when_the_head_moved_after_ci_passed() {
    let w = World::build(Build {
        move_to: Some("head-sha-2".into()),
        ..Build::default()
    });
    w.gh.add_run(
        REPO,
        make_run(|_| {}),
        stages(vec![RunStage::completed("success")]),
    );
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Aborted);
    assert!(out.summary.contains("moved"));
    assert!(w.gh.writes().is_empty());
}

// ship.test.ts: ship: registry

#[tokio::test]
async fn tolerates_propagation_lag() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let (g2, r2) = (w.gh.clone(), w.registry.clone());
    w.gh.on_merge(move |_pr, sha| {
        g2.add_run(
            REPO,
            release_run(sha, |r| {
                r.status = "completed".into();
                r.conclusion = Some("success".into());
            }),
            RunScript::default(),
        );
        r2.publish_after_queries("@namzu/sdk", "1.1.0", 4);
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.registry.queries().len(), 5);
}

#[tokio::test]
async fn reports_without_failing_a_version_the_registry_never_shows() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let g2 = w.gh.clone();
    w.gh.on_merge(move |_pr, sha| {
        g2.add_run(
            REPO,
            release_run(sha, |r| {
                r.status = "completed".into();
                r.conclusion = Some("success".into());
            }),
            RunScript::default(),
        );
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::RegistryPending);
    let missing = out.missing_packages.unwrap();
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].name, "@namzu/sdk");
    assert_eq!(missing[0].from.as_deref(), Some("1.0.0"));
    assert_eq!(missing[0].to, "1.1.0");
    assert!(w.clock.base.total_slept() >= 10 * 60_000);
    assert!(w.active().is_some());
}

// ship.test.ts: ship: resume

#[tokio::test]
async fn continues_a_ci_wait_without_approving_twice() {
    let w = approving();
    w.gh.add_run(
        REPO,
        run_action_required(),
        RunScript {
            on_approve: Some(green(3)),
            ..RunScript::default()
        },
    );
    let first = HookClock::new();
    first.set_hook(|n| n == 2);
    w.go_until_interrupted(&first).await;
    let session = w.active().unwrap();
    assert_eq!(session.step, "gate");
    assert_eq!(w.writes(WriteMethod::ApproveRun), 1);

    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(out.session_id, session.id);
    assert_eq!(w.writes(WriteMethod::ApproveRun), 1);
    assert_eq!(w.writes(WriteMethod::MergePr), 1);
}

#[tokio::test]
async fn continues_the_release_wait_after_the_merge_without_merging_again() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let first = HookClock::new();
    let gh = w.gh.clone();
    first.set_hook(move |_| !gh.writes_of(WriteMethod::MergePr).is_empty());
    w.go_until_interrupted(&first).await;
    assert_eq!(w.active().unwrap().step, "release");

    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(out.merge_commit_sha.as_deref(), Some("merge-commit-1"));
    assert_eq!(w.writes(WriteMethod::MergePr), 1);
}

// Beyond the TS suite: the outcome serialises as the TS JSON shape.
#[tokio::test]
async fn outcome_serialises_with_camel_case_keys() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let out = w.dry().await;
    let v = serde_json::to_value(&out).unwrap();
    assert_eq!(v["status"], "planned");
    assert_eq!(v["prNumber"], json!(1));
    assert!(v.get("sessionId").is_some());
}

// ---- a transient read failure is an unanswered poll, not the end of the ship ----

/// A release run that is already green when the merge lands.
fn release_lands_green(w: &World) {
    let (g2, r2) = (w.gh.clone(), w.registry.clone());
    w.gh.on_merge(move |_pr, sha| {
        r2.set_version("@namzu/sdk", "1.1.0");
        g2.add_run(
            REPO,
            release_run(sha, |r| {
                r.status = "completed".into();
                r.conclusion = Some("success".into());
            }),
            RunScript::default(),
        );
    });
}

#[tokio::test]
async fn survives_transient_failures_reading_the_release_run_and_still_ships() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    release_lands_green(&w);
    w.fail("runs:release", 3, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.warned(), 3);
    assert_eq!(w.writes(WriteMethod::MergePr), 1);
}

#[tokio::test]
async fn survives_a_transient_registry_failure_and_still_confirms_the_publish() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.registry.fail_queries(2, true);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.warned(), 2);
    assert_eq!(w.registry.queries().len(), 3);
}

#[tokio::test]
async fn survives_a_transient_failure_in_the_ci_gate() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.fail("runs:ci", 2, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.warned(), 2);
}

#[tokio::test]
async fn survives_a_transient_failure_in_the_snapshot_check() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.fail("list_pr_files", 1, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.warned(), 1);
}

#[tokio::test]
async fn survives_a_transient_failure_while_finding_the_version_pr() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.fail("find_open_pr", 2, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.warned(), 2);
}

#[tokio::test]
async fn a_transient_outage_longer_than_the_step_budget_times_out_instead_of_erroring() {
    // find_pr / snapshot share the version PR budget.
    let w = World::with_config(|c| c.version_pr_wait_minutes = 2);
    w.fail("find_open_pr", u32::MAX, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert!(w.active().is_some());

    // The CI gate.
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.fail("runs:ci", u32::MAX, Fault::Transient);
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert_eq!(w.active().unwrap().step, "gate");
    assert!(w.gh.writes().is_empty());

    // The release run: merged, then an outage; resumable.
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    release_lands_green(&w);
    w.fail("runs:release", u32::MAX, Fault::Transient);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert_eq!(w.writes(WriteMethod::MergePr), 1);
    assert_eq!(w.active().unwrap().step, "release");

    // The registry.
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.registry.fail_queries(u32::MAX, true);
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::RegistryPending);
    assert!(w.active().is_some());
}

#[tokio::test]
async fn a_permanent_read_error_still_propagates() {
    for (op, kind) in [
        ("runs:release", Fault::NotFound),
        ("runs:ci", Fault::Message),
        ("find_open_pr", Fault::NotFound),
        ("list_dir", Fault::Message),
    ] {
        let w = World::new();
        w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
        release_lands_green(&w);
        w.fail(op, u32::MAX, kind);
        let err = w.run_with(&w.clock, confirm()).await.unwrap_err();
        let ShipError::Port(e) = err else {
            panic!("{op}: not a port error")
        };
        assert!(!e.is_transient(), "{op}");
        assert_eq!(w.warned(), 0, "{op}");
    }
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.registry.fail_queries(1, false);
    assert!(w.run_with(&w.clock, confirm()).await.is_err());
}

fn confirm() -> ShipOptions {
    ShipOptions {
        confirm: true,
        ..ShipOptions::default()
    }
}

#[tokio::test]
async fn a_failed_write_is_never_retried_whatever_its_kind() {
    for kind in [Fault::Message, Fault::Transient, Fault::NotFound] {
        // CI re-run.
        let w = World::new();
        w.gh.add_run(REPO, run_with("completed", "failure"), on_rerun(green(0)));
        w.fail("rerun_failed_jobs", u32::MAX, kind);
        let err = w.run_with(&w.clock, confirm()).await.unwrap_err();
        assert!(
            matches!(&err, ShipError::Port(e) if !e.is_transient()),
            "{err}"
        );
        assert_eq!(w.calls("rerun_failed_jobs"), 1);
        assert!(w.gh.writes().is_empty());

        // Release re-run.
        let w = World::new();
        w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
        let g2 = w.gh.clone();
        w.gh.on_merge(move |_pr, sha| {
            g2.add_run(
                REPO,
                release_run(sha, |r| {
                    r.status = "completed".into();
                    r.conclusion = Some("failure".into());
                }),
                on_rerun(vec![RunStage::completed("failure")]),
            );
        });
        w.fail("rerun_failed_jobs", u32::MAX, kind);
        let err = w.run_with(&w.clock, confirm()).await.unwrap_err();
        assert!(
            matches!(&err, ShipError::Port(e) if !e.is_transient()),
            "{err}"
        );
        assert_eq!(w.calls("rerun_failed_jobs"), 1);
        assert_eq!(w.writes(WriteMethod::MergePr), 1);
    }

    // Approval.
    let w = approving();
    w.gh.add_run(REPO, run_action_required(), RunScript::default());
    w.fail("approve_run", u32::MAX, Fault::Transient);
    let err = w.run_with(&w.clock, confirm()).await.unwrap_err();
    assert!(
        matches!(&err, ShipError::Port(e) if !e.is_transient()),
        "{err}"
    );
    assert_eq!(w.calls("approve_run"), 1);
}

#[tokio::test]
async fn a_transient_read_right_after_a_rerun_does_not_repeat_the_rerun() {
    // The re-run is recorded the moment it is made, so a read that fails on the next poll
    // (listing runs, then jobs) cannot send ground back to re-run the same attempt.
    let w = World::new();
    w.gh.add_run(REPO, run_with("completed", "failure"), on_rerun(green(1)));
    w.port.faults.lock().unwrap().after_rerun = Some(("runs:ci", 2, Fault::Transient));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::RerunFailedJobs), 1);
}

#[tokio::test]
async fn a_transient_read_right_after_an_approval_does_not_repeat_the_approval() {
    let w = approving();
    w.gh.add_run(REPO, run_action_required(), on_approve());
    w.port.faults.lock().unwrap().after_approve = Some(("runs:ci", 2, Fault::Transient));
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::ApproveRun), 1);
}

#[tokio::test]
async fn an_unreadable_stored_deadline_gets_a_fresh_budget_instead_of_waiting_forever() {
    let w = approving();
    w.gh.add_run(REPO, run_action_required(), on_approve());
    let first = HookClock::new();
    first.set_hook(|n| n == 1);
    w.go_until_interrupted(&first).await;
    let session = w.active().unwrap();
    let mut data = session.data.clone();
    data["deadlines"] = json!({ "ci": "not a date" });
    w.store
        .update_ship_session(
            &session.id,
            ShipSessionPatch {
                data: Some(data),
                ..ShipSessionPatch::default()
            },
        )
        .unwrap();
    w.fail("runs:ci", u32::MAX, Fault::Transient);
    let out = w.go_ci_wait(2).await;
    assert_eq!(out.status, ShipStatus::TimedOut);
}

// ---- resuming at the merge step re-checks the changeset snapshot ----

#[tokio::test]
async fn a_changeset_that_lands_after_a_dry_run_sends_the_confirm_back_to_the_snapshot_wait() {
    let w = World::with_config(|c| c.version_pr_wait_minutes = 2);
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    let planned = w.dry().await;
    assert_eq!(planned.status, ShipStatus::Planned);
    assert_eq!(w.active().unwrap().step, "merge");

    w.gh.set_dir(
        REPO,
        "main",
        ".changeset",
        &["a.md", "b.md", "c.md", "README.md", "config.json"],
    );
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::TimedOut);
    assert!(out.summary.contains("c.md"), "{}", out.summary);
    assert!(w.gh.writes_of(WriteMethod::MergePr).is_empty());
    assert_eq!(w.active().unwrap().step, "snapshot");
    assert!(
        w.logger
            .messages(None)
            .iter()
            .any(|m| m.contains("went stale before the merge"))
    );
}

#[tokio::test]
async fn a_stale_pr_found_at_the_merge_is_refreshed_then_gated_again_and_merged() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    w.gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 101;
            r.head_sha = "head-sha-2".into();
        }),
        stages(green(1)),
    );
    assert_eq!(w.dry().await.status, ShipStatus::Planned);
    w.gh.set_dir(REPO, "main", ".changeset", &["a.md", "b.md", "c.md"]);
    let gh = w.gh.clone();
    let first_sleep = w.clock.count() + 1;
    w.clock.set_hook(move |n| {
        if n == first_sleep {
            gh.update_pr(REPO, 1, |p| p.head_sha = "head-sha-2".into());
            gh.set_pr_files(
                REPO,
                1,
                vec![
                    removed(".changeset/a.md"),
                    removed(".changeset/b.md"),
                    removed(".changeset/c.md"),
                    sdk_file(),
                ],
            );
        }
        false
    });
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    let merges = w.gh.writes_of(WriteMethod::MergePr);
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].args["expectedHeadSha"], "head-sha-2");
}

#[tokio::test]
async fn with_no_new_changeset_the_resumed_confirm_merges_as_before() {
    let w = World::new();
    w.gh.add_run(REPO, make_run(|_| {}), stages(green(1)));
    assert_eq!(w.dry().await.status, ShipStatus::Planned);
    assert_eq!(w.active().unwrap().step, "merge");
    let out = w.go().await;
    assert_eq!(out.status, ShipStatus::Shipped);
    assert_eq!(w.writes(WriteMethod::MergePr), 1);
}
