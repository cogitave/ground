use super::*;
use crate::ports::{
    Clock, GitHubPort, LogLevel, Logger, MergeMethod, MergeOptions, PrFile, PrFileStatus,
    RegistryPort, RunFilter, RunJob,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

const REPO: &str = "cogitave/namzu";

fn squash(sha: &str) -> MergeOptions {
    MergeOptions {
        method: MergeMethod::Squash,
        admin: true,
        expected_head_sha: sha.into(),
    }
}

#[tokio::test]
async fn fake_clock_advances_virtual_time_on_sleep_and_records_sleeps() {
    let clock = FakeClock::new("2026-01-01T00:00:00Z");
    clock.sleep(30_000).await;
    clock.sleep(500).await;
    assert_eq!(clock.sleeps(), [30_000, 500]);
    assert_eq!(clock.total_slept(), 30_500);
    assert_eq!(clock.now().to_rfc3339(), "2026-01-01T00:00:30.500+00:00");
    clock.advance(1000);
    assert_eq!(clock.now().to_rfc3339(), "2026-01-01T00:00:31.500+00:00");
    // Clones share time.
    let other = clock.clone();
    other.advance(500);
    assert_eq!(clock.now().to_rfc3339(), "2026-01-01T00:00:32+00:00");
}

#[tokio::test]
async fn fake_registry_returns_none_for_unknown_packages_and_records_queries() {
    let r = FakeRegistry::new();
    assert_eq!(r.latest_version("a").await.unwrap(), None);
    assert_eq!(r.queries(), ["a"]);
}

#[tokio::test]
async fn fake_registry_publishes_after_the_scheduled_number_of_queries() {
    let r = FakeRegistry::new();
    r.set_version("a", "1.0.0")
        .publish_after_queries("a", "1.1.0", 2);
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(r.latest_version("a").await.unwrap().unwrap());
    }
    assert_eq!(seen, ["1.0.0", "1.0.0", "1.1.0", "1.1.0"]);
}

#[test]
fn memory_logger_keeps_entries_by_level() {
    let l = MemoryLogger::new();
    l.info("a", Some(json!({"x": 1})));
    l.warn("b", None);
    assert_eq!(l.messages(None), ["a", "b"]);
    assert_eq!(l.messages(Some(LogLevel::Warn)), ["b"]);
    assert_eq!(l.entries()[0].data, Some(json!({"x": 1})));
}

#[tokio::test]
async fn fake_github_finds_open_prs_by_head_and_base_and_ignores_closed_ones() {
    let gh = FakeGitHub::new();
    gh.set_pr(
        REPO,
        make_pr(|p| {
            p.number = 1;
            p.state = crate::ports::PrState::Closed;
        }),
    )
    .set_pr(REPO, make_pr(|p| p.number = 2));
    let found = gh
        .find_open_pr(REPO, "changeset-release/main", "main")
        .await
        .unwrap();
    assert_eq!(found.unwrap().number, 2);
    assert!(
        gh.find_open_pr(REPO, "other", "main")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        gh.get_pr(REPO, 9)
            .await
            .unwrap_err()
            .to_string()
            .contains("no PR")
    );
}

#[tokio::test]
async fn fake_github_serves_files_dirs_and_branch_shas() {
    let gh = FakeGitHub::new();
    gh.set_pr(REPO, make_pr(|_| {}))
        .set_pr_files(
            REPO,
            1,
            vec![PrFile {
                path: "a.md".into(),
                status: PrFileStatus::Removed,
                patch: None,
            }],
        )
        .set_dir(REPO, "main", ".changeset", &["x.md"])
        .set_branch_sha(REPO, "main", "abc");
    assert_eq!(gh.list_pr_files(REPO, 1).await.unwrap()[0].path, "a.md");
    assert_eq!(
        gh.list_dir(REPO, "main", ".changeset").await.unwrap(),
        ["x.md"]
    );
    assert!(gh.list_dir(REPO, "main", "nope").await.unwrap().is_empty());
    assert_eq!(gh.get_branch_sha(REPO, "main").await.unwrap(), "abc");
    assert!(gh.get_branch_sha(REPO, "dev").await.is_err());
}

#[tokio::test]
async fn fake_github_advances_a_run_one_stage_per_poll_and_filters() {
    let gh = FakeGitHub::new();
    gh.add_run(
        REPO,
        make_run(|r| r.id = 7),
        RunScript {
            stages: vec![
                RunStage::status("in_progress"),
                RunStage::completed("success"),
            ],
            ..Default::default()
        },
    );
    let filter = RunFilter {
        head_sha: Some("head-sha-1".into()),
        event: Some("pull_request".into()),
        ..Default::default()
    };
    let poll = || gh.list_workflow_runs(REPO, "ci.yml", &filter);
    assert_eq!(poll().await.unwrap()[0].status, "queued");
    assert_eq!(poll().await.unwrap()[0].status, "in_progress");
    let done = poll().await.unwrap().remove(0);
    assert_eq!(
        (done.status.as_str(), done.conclusion.as_deref()),
        ("completed", Some("success"))
    );
    let old = RunFilter {
        head_sha: Some("old".into()),
        ..Default::default()
    };
    assert!(
        gh.list_workflow_runs(REPO, "ci.yml", &old)
            .await
            .unwrap()
            .is_empty()
    );
    let any = RunFilter::default();
    assert!(
        gh.list_workflow_runs(REPO, "release.yml", &any)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn fake_github_records_approvals_and_reruns_and_swaps_in_their_scripts() {
    let gh = FakeGitHub::new();
    gh.add_run(
        REPO,
        make_run(|r| {
            r.id = 7;
            r.status = "action_required".into();
        }),
        RunScript {
            on_approve: Some(vec![RunStage::completed("failure")]),
            on_rerun: Some(vec![RunStage::completed("success")]),
            ..Default::default()
        },
    );
    let any = RunFilter::default();
    gh.approve_run(REPO, 7).await.unwrap();
    assert_eq!(gh.run_state(REPO, 7).status, "queued");
    assert_eq!(
        gh.list_workflow_runs(REPO, "ci.yml", &any).await.unwrap()[0].status,
        "queued"
    );
    assert_eq!(gh.run_state(REPO, 7).conclusion.as_deref(), Some("failure"));
    gh.rerun_failed_jobs(REPO, 7).await.unwrap();
    assert_eq!(gh.run_state(REPO, 7).run_attempt, 2);
    gh.list_workflow_runs(REPO, "ci.yml", &any).await.unwrap();
    assert_eq!(gh.run_state(REPO, 7).conclusion.as_deref(), Some("success"));
    let methods: Vec<_> = gh.writes().iter().map(|w| w.method).collect();
    assert_eq!(
        methods,
        [WriteMethod::ApproveRun, WriteMethod::RerunFailedJobs]
    );
    assert_eq!(
        gh.writes_of(WriteMethod::ApproveRun)[0].args,
        json!({"runId": 7})
    );
    assert!(
        gh.approve_run(REPO, 99)
            .await
            .unwrap_err()
            .to_string()
            .contains("no run")
    );
}

#[tokio::test]
async fn fake_github_merges_only_with_the_expected_head_sha_and_fires_on_merge() {
    let gh = FakeGitHub::new();
    gh.set_pr(REPO, make_pr(|_| {}))
        .set_merge_commit(REPO, 1, "m1");
    let seen = Arc::new(Mutex::new(None));
    let sink = seen.clone();
    gh.on_merge(move |_, sha| *sink.lock().unwrap() = Some(sha.to_owned()));

    let err = gh.merge_pr(REPO, 1, &squash("stale")).await.unwrap_err();
    assert!(err.to_string().contains("mismatch"));
    let res = gh.merge_pr(REPO, 1, &squash("head-sha-1")).await.unwrap();
    assert_eq!(res.merge_commit_sha, "m1");
    assert_eq!(seen.lock().unwrap().as_deref(), Some("m1"));
    let pr = gh.get_pr(REPO, 1).await.unwrap();
    assert!(pr.merged);
    assert_eq!(pr.merge_commit_sha.as_deref(), Some("m1"));
    assert_eq!(gh.get_branch_sha(REPO, "main").await.unwrap(), "m1");
    assert_eq!(gh.writes_of(WriteMethod::MergePr).len(), 2);
    assert_eq!(
        gh.writes_of(WriteMethod::MergePr)[1].args,
        json!({"number": 1, "method": "squash", "admin": true, "expectedHeadSha": "head-sha-1"})
    );
    let err = gh
        .merge_pr(REPO, 1, &squash("head-sha-1"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not open"));
}

#[tokio::test]
async fn fake_github_on_merge_may_script_the_fake_it_belongs_to() {
    let gh = FakeGitHub::new();
    gh.set_pr(REPO, make_pr(|_| {}));
    let handle = gh.clone();
    gh.on_merge(move |_, sha| {
        handle.add_run(
            REPO,
            make_run(|r| {
                r.id = 500;
                r.workflow_file = "release.yml".into();
                r.head_sha = sha.to_owned();
            }),
            RunScript::default(),
        );
    });
    gh.merge_pr(REPO, 1, &squash("head-sha-1")).await.unwrap();
    assert_eq!(gh.run_state(REPO, 500).head_sha, "merge-commit-1");
}

#[tokio::test]
async fn fake_github_lets_a_test_move_the_head_between_reads() {
    let gh = FakeGitHub::new();
    gh.set_pr(REPO, make_pr(|_| {}))
        .update_pr(REPO, 1, |p| p.head_sha = "new".into());
    assert_eq!(gh.get_pr(REPO, 1).await.unwrap().head_sha, "new");
}

#[tokio::test]
async fn fake_github_serves_scripted_jobs() {
    let gh = FakeGitHub::new();
    gh.set_jobs(
        REPO,
        7,
        vec![RunJob {
            name: "test".into(),
            status: "completed".into(),
            conclusion: Some("failure".into()),
        }],
    );
    assert_eq!(gh.list_run_jobs(REPO, 7).await.unwrap().len(), 1);
    assert!(gh.list_run_jobs(REPO, 8).await.unwrap().is_empty());
}

#[test]
fn make_run_derives_the_url_from_the_id() {
    assert_eq!(
        make_run(|r| r.id = 7).html_url,
        "https://github.com/cogitave/namzu/actions/runs/7"
    );
}
