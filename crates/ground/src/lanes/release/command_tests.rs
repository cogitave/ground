use super::{ShipDeps, run_ship_command};
use crate::config::RepoConfig;
use crate::ports::{MergeMethod, PrFile, PrFileStatus};
use crate::state::StateStore;
use crate::testing::{
    FakeClock, FakeGitHub, FakeRegistry, MemoryLogger, RunScript, RunStage, WriteMethod, make_pr,
    make_run,
};

const REPO: &str = "cogitave/namzu";
const PATCH: &str = "@@ -1,5 +1,5 @@\n {\n   \"name\": \"@namzu/sdk\",\n-  \"version\": \"1.0.0\",\n+  \"version\": \"1.1.0\",\n   \"type\": \"module\",";

fn config() -> RepoConfig {
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

fn success_run() -> RunScript {
    RunScript {
        stages: vec![RunStage::completed("success")],
        ..RunScript::default()
    }
}

fn world() -> (FakeGitHub, FakeRegistry) {
    let github = FakeGitHub::new();
    github
        .set_pr(REPO, make_pr(|_| {}))
        .set_pr_files(
            REPO,
            1,
            vec![
                PrFile {
                    path: ".changeset/a.md".into(),
                    status: PrFileStatus::Removed,
                    patch: None,
                },
                PrFile {
                    path: "packages/sdk/package.json".into(),
                    status: PrFileStatus::Modified,
                    patch: Some(PATCH.into()),
                },
            ],
        )
        .set_dir(
            REPO,
            "main",
            ".changeset",
            &["a.md", "README.md", "config.json"],
        )
        .set_branch_sha(REPO, "main", "base-sha-1");
    github.add_run(REPO, make_run(|_| {}), success_run());
    let registry = FakeRegistry::new();
    registry.set_version("@namzu/sdk", "1.0.0");
    let (g2, r2) = (github.clone(), registry.clone());
    github.on_merge(move |_pr, sha| {
        g2.add_run(
            REPO,
            make_run(|r| {
                r.id = 200;
                r.workflow_file = "release.yml".into();
                r.event = "push".into();
                r.head_sha = sha.into();
                r.head_branch = "main".into();
            }),
            success_run(),
        );
        r2.set_version("@namzu/sdk", "1.1.0");
    });
    (github, registry)
}

async fn run(
    github: &FakeGitHub,
    registry: &FakeRegistry,
    cfg: &RepoConfig,
    confirm: bool,
    json: bool,
) -> (i32, Vec<String>) {
    let clock = FakeClock::default();
    let logger = MemoryLogger::new();
    let store = StateStore::new(clock.clone(), ":memory:").unwrap();
    let deps = ShipDeps {
        config: cfg,
        github,
        registry,
        clock: &clock,
        logger: &logger,
        store: &store,
    };
    let mut lines = Vec::new();
    let code = run_ship_command(&deps, confirm, json, |s| lines.push(s.to_owned()))
        .await
        .unwrap();
    (code, lines)
}

// ship-command.test.ts: "prints the plan and exits 0 on a dry run, writing nothing"
#[tokio::test]
async fn prints_the_plan_and_exits_0_on_a_dry_run_writing_nothing() {
    let (github, registry) = world();
    let (code, lines) = run(&github, &registry, &config(), false, false).await;
    assert_eq!(code, 0);
    assert!(lines.join("\n").contains("@namzu/sdk@1.1.0"));
    assert!(github.writes().is_empty());
}

// ship-command.test.ts: "ships with --confirm and exits 0"
#[tokio::test]
async fn ships_with_confirm_and_exits_0() {
    let (github, registry) = world();
    let (code, _) = run(&github, &registry, &config(), true, false).await;
    assert_eq!(code, 0);
    assert_eq!(github.writes_of(WriteMethod::MergePr).len(), 1);
}

// ship-command.test.ts: "prints the outcome as JSON with --json"
#[tokio::test]
async fn prints_the_outcome_as_json_with_json() {
    let (github, registry) = world();
    let (_, lines) = run(&github, &registry, &config(), false, true).await;
    let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["status"], "planned");
}

// ship-command.test.ts: "exits non-zero when CI fails for good"
#[tokio::test]
async fn exits_non_zero_when_ci_fails_for_good() {
    let github = FakeGitHub::new();
    github
        .set_pr(REPO, make_pr(|_| {}))
        .set_pr_files(
            REPO,
            1,
            vec![PrFile {
                path: ".changeset/a.md".into(),
                status: PrFileStatus::Removed,
                patch: None,
            }],
        )
        .set_dir(REPO, "main", ".changeset", &["a.md"])
        .set_branch_sha(REPO, "main", "base-sha-1");
    github.add_run(
        REPO,
        make_run(|r| {
            r.status = "completed".into();
            r.conclusion = Some("failure".into());
        }),
        RunScript::default(),
    );
    let mut cfg = config();
    cfg.max_reruns_per_run = 0;
    let (code, _) = run(&github, &FakeRegistry::new(), &cfg, true, false).await;
    assert_eq!(code, 1);
}
