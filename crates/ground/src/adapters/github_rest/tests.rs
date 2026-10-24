use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::testing::RecordingExec;

fn raw_pr() -> Value {
    json!({
        "number": 5, "title": "t", "state": "open", "merged": false,
        "head": {"ref": "b", "sha": "h", "repo": {"full_name": "o/r"}},
        "base": {"ref": "main", "sha": "bs"},
        "user": {"login": "bot"}, "mergeable": null
    })
}

fn pr_from(value: Value) -> PullRequest {
    map_pr(serde_json::from_value(value).unwrap())
}

#[test]
fn map_pr_maps_the_fields() {
    let pr = pr_from(raw_pr());
    assert_eq!(
        pr,
        PullRequest {
            number: 5,
            title: "t".into(),
            state: PrState::Open,
            merged: false,
            merge_commit_sha: None,
            head_ref: "b".into(),
            head_sha: "h".into(),
            head_repo: "o/r".into(),
            base_ref: "main".into(),
            base_sha: "bs".into(),
            author: "bot".into(),
            mergeable: None,
        }
    );
}

#[test]
fn map_pr_survives_a_deleted_fork_and_derives_merged_from_merged_at() {
    let mut raw = raw_pr();
    raw["state"] = json!("closed");
    raw.as_object_mut().unwrap().remove("merged");
    raw["merged_at"] = json!("2026-01-01");
    raw["merge_commit_sha"] = json!("mc");
    raw["head"]["repo"] = Value::Null;
    raw["user"] = Value::Null;
    let pr = pr_from(raw);
    assert_eq!(pr.state, PrState::Closed);
    assert!(pr.merged);
    assert_eq!(pr.head_repo, "/");
    assert_eq!(pr.author, "");
    assert_eq!(pr.merge_commit_sha.as_deref(), Some("mc"));
}

#[test]
fn map_pr_drops_the_merge_commit_of_an_unmerged_pr() {
    let mut raw = raw_pr();
    raw["merge_commit_sha"] = json!("test-merge-ref");
    assert_eq!(pr_from(raw).merge_commit_sha, None);
}

#[test]
fn map_file_normalises_status_and_keeps_the_patch_only_when_present() {
    let file = |v: Value| map_file(serde_json::from_value(v).unwrap());
    assert_eq!(
        file(json!({"filename": "a", "status": "changed"})),
        PrFile {
            path: "a".into(),
            status: PrFileStatus::Modified,
            patch: None
        }
    );
    assert_eq!(
        file(json!({"filename": "a", "status": "renamed", "patch": "p"})),
        PrFile {
            path: "a".into(),
            status: PrFileStatus::Renamed,
            patch: Some("p".into())
        }
    );
}

#[test]
fn map_job_maps_jobs() {
    let job = map_job(
        serde_json::from_value(
            json!({"name": "x", "status": "completed", "conclusion": "failure"}),
        )
        .unwrap(),
    );
    assert_eq!(
        job,
        RunJob {
            name: "x".into(),
            status: "completed".into(),
            conclusion: Some("failure".into())
        }
    );
}

#[test]
fn map_run_reduces_the_workflow_path_to_a_file_name() {
    let run = map_run(
        serde_json::from_value(json!({
            "id": 1, "path": ".github/workflows/ci.yml", "event": "pull_request",
            "head_sha": "s", "head_branch": null, "status": "completed",
            "conclusion": "success", "created_at": "c", "actor": {"login": "a"}, "html_url": "u"
        }))
        .unwrap(),
    );
    assert_eq!(run.workflow_file, "ci.yml");
    assert_eq!(run.head_branch, "");
    assert_eq!(run.run_attempt, 1);
    assert_eq!(
        (run.actor.as_str(), run.triggering_actor.as_str()),
        ("a", "a")
    );
}

#[test]
fn builds_the_admin_merge_command() {
    assert_eq!(
        gh_merge_args("o/r", 5, MergeMethod::Squash, "abc"),
        [
            "pr",
            "merge",
            "5",
            "--repo",
            "o/r",
            "--squash",
            "--admin",
            "--match-head-commit",
            "abc"
        ]
    );
}

#[test]
fn split_repo_wants_owner_and_name() {
    assert_eq!(split_repo("o/r").unwrap(), ("o", "r"));
    for bad in ["o", "/r", "o/", "o/r/x"] {
        assert!(split_repo(bad).is_err(), "{bad}");
    }
}

// ---- a loopback HTTP server standing in for api.github.com ----

#[derive(Debug, Clone)]
struct Seen {
    request_line: String,
    headers: String,
    body: String,
}

/// Answers each request with the first route whose prefix matches its request line.
async fn serve(routes: Vec<(&'static str, u16, Value)>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, length) = loop {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    break (end + 4, length);
                }
                assert!(n > 0, "connection closed mid-request");
            };
            while buf.len() < head_end + length {
                let n = sock.read(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk[..n]);
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let mut lines = text[..head_end].lines();
            let request_line = lines.next().unwrap().to_owned();
            let headers = lines.collect::<Vec<_>>().join("\n").to_lowercase();
            log.lock().unwrap().push(Seen {
                request_line: request_line.clone(),
                headers,
                body: text[head_end..].to_owned(),
            });
            let (status, body) = routes
                .iter()
                .find(|(prefix, _, _)| request_line.starts_with(prefix))
                .map_or((500, json!({"message": "no route"})), |(_, s, b)| {
                    (*s, b.clone())
                });
            let body = if body.is_null() {
                String::new()
            } else {
                body.to_string()
            };
            let reply = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (base, seen)
}

fn gh(base: &str, exec: RecordingExec) -> RestGitHub<RecordingExec> {
    RestGitHub::with_exec(exec)
        .with_token("tok")
        .with_base_url(base)
}

#[tokio::test]
async fn shells_out_to_gh_for_an_admin_merge_and_reads_the_merge_commit() {
    let mut merged = raw_pr();
    merged["merged"] = json!(true);
    merged["merge_commit_sha"] = json!("mc");
    let (base, seen) = serve(vec![("GET /repos/o/r/pulls/5 ", 200, merged)]).await;
    let exec = RecordingExec::new("");
    let opts = MergeOptions {
        method: MergeMethod::Squash,
        admin: true,
        expected_head_sha: "h".into(),
    };
    let res = gh(&base, exec.clone())
        .merge_pr("o/r", 5, &opts)
        .await
        .unwrap();
    assert_eq!(res.merge_commit_sha, "mc");
    assert_eq!(&exec.calls()[0][..3], ["gh", "pr", "merge"]);
    let seen = seen.lock().unwrap();
    assert!(
        seen[0].headers.contains("authorization: bearer tok"),
        "{}",
        seen[0].headers
    );
    assert!(
        seen[0].headers.contains("user-agent: ground/"),
        "{}",
        seen[0].headers
    );
}

#[tokio::test]
async fn merges_through_the_rest_api_when_not_admin() {
    let (base, seen) = serve(vec![(
        "PUT /repos/o/r/pulls/5/merge",
        200,
        json!({"sha": "m9"}),
    )])
    .await;
    let opts = MergeOptions {
        method: MergeMethod::Rebase,
        admin: false,
        expected_head_sha: "h".into(),
    };
    let exec = RecordingExec::new("");
    let res = gh(&base, exec.clone())
        .merge_pr("o/r", 5, &opts)
        .await
        .unwrap();
    assert_eq!(res.merge_commit_sha, "m9");
    assert!(exec.calls().is_empty());
    let body: Value = serde_json::from_str(&seen.lock().unwrap()[0].body).unwrap();
    assert_eq!(body, json!({"merge_method": "rebase", "sha": "h"}));
}

#[tokio::test]
async fn list_dir_maps_404_to_empty_and_lists_names() {
    let (base, seen) = serve(vec![
        (
            "GET /repos/o/r/contents/.changeset?ref=main",
            200,
            json!([{"name": "a.md"}, {"name": "b.md"}]),
        ),
        (
            "GET /repos/o/r/contents/nope",
            404,
            json!({"message": "Not Found"}),
        ),
    ])
    .await;
    let g = gh(&base, RecordingExec::new(""));
    assert_eq!(
        g.list_dir("o/r", "main", ".changeset").await.unwrap(),
        ["a.md", "b.md"]
    );
    assert!(g.list_dir("o/r", "main", "nope").await.unwrap().is_empty());
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn approves_and_reruns_with_posts_and_reports_http_errors() {
    let (base, seen) = serve(vec![
        ("POST /repos/o/r/actions/runs/7/approve", 201, json!({})),
        (
            "POST /repos/o/r/actions/runs/7/rerun-failed-jobs",
            403,
            json!({"message": "nope"}),
        ),
    ])
    .await;
    let g = gh(&base, RecordingExec::new(""));
    g.approve_run("o/r", 7).await.unwrap();
    let err = g.rerun_failed_jobs("o/r", 7).await.unwrap_err();
    assert_eq!(err.to_string(), "GitHub answered 403: nope");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn lists_runs_with_filters_and_finds_the_open_pr_through_the_pr_endpoint() {
    let (base, seen) = serve(vec![
        (
            "GET /repos/o/r/actions/workflows/ci.yml/runs",
            200,
            json!({"workflow_runs": [{
                "id": 1, "path": ".github/workflows/ci.yml", "event": "pull_request",
                "head_sha": "s", "head_branch": "b", "status": "queued", "conclusion": null,
                "created_at": "c", "html_url": "u"
            }]}),
        ),
        ("GET /repos/o/r/pulls?", 200, json!([{"number": 5}])),
        ("GET /repos/o/r/pulls/5 ", 200, raw_pr()),
    ])
    .await;
    let g = gh(&base, RecordingExec::new(""));
    let filter = RunFilter {
        head_sha: Some("s".into()),
        event: Some("pull_request".into()),
        branch: None,
    };
    let runs = g
        .list_workflow_runs("o/r", "ci.yml", &filter)
        .await
        .unwrap();
    assert_eq!(runs[0].workflow_file, "ci.yml");
    let pr = g.find_open_pr("o/r", "b", "main").await.unwrap().unwrap();
    assert_eq!(pr.number, 5);
    let lines: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.request_line.clone())
        .collect();
    assert!(
        lines[0].contains("head_sha=s") && lines[0].contains("event=pull_request"),
        "{}",
        lines[0]
    );
    assert!(!lines[0].contains("branch="), "{}", lines[0]);
    assert!(
        lines[1].contains("head=o%3Ab") && lines[1].contains("base=main"),
        "{}",
        lines[1]
    );
}

#[tokio::test]
async fn reads_the_token_from_gh_once_and_only_when_needed() {
    let (base, seen) = serve(vec![("GET /repos/o/r/pulls/5 ", 200, raw_pr())]).await;
    let exec = RecordingExec::new("fromgh\n");
    let g = RestGitHub::with_exec(exec.clone()).with_base_url(&base);
    assert!(exec.calls().is_empty());
    g.get_pr("o/r", 5).await.unwrap();
    g.get_pr("o/r", 5).await.unwrap();
    assert_eq!(exec.calls(), [["gh", "auth", "token"]]);
    assert!(
        seen.lock().unwrap()[1]
            .headers
            .contains("authorization: bearer fromgh")
    );
}

#[test]
fn a_non_json_success_body_names_the_status_and_the_body() {
    let err = decode_body(200, b"<html>gateway</html>")
        .unwrap_err()
        .to_string();
    assert!(err.contains("200") && err.contains("<html>"), "{err}");
    assert_eq!(decode_body(502, b"<html>").unwrap(), Value::Null);
    assert_eq!(decode_body(204, b"").unwrap(), Value::Null);
}

// ---- transient or permanent: only a failed READ is worth asking again ----

#[test]
fn only_a_read_answered_5xx_or_429_is_transient() {
    for status in [500, 502, 503, 504, 429] {
        assert!(PortError::from_http(true, "GitHub", status, None).is_transient());
        assert!(!PortError::from_http(false, "GitHub", status, None).is_transient());
    }
    for status in [400, 401, 403, 404, 410, 422] {
        assert!(!PortError::from_http(true, "GitHub", status, None).is_transient());
    }
    assert_eq!(
        PortError::from_http(true, "GitHub", 503, Some("busy".into())).to_string(),
        "GitHub answered 503: busy"
    );
}

#[test]
fn into_permanent_drops_the_retry_hint_and_keeps_the_text() {
    let err = PortError::Transient("x".into()).into_permanent();
    assert!(!err.is_transient());
    assert_eq!(err.to_string(), "x");
}

#[tokio::test]
async fn a_get_answered_503_or_429_is_transient_and_a_404_or_401_is_not() {
    let (base, _) = serve(vec![
        ("GET /repos/o/r/pulls/1 ", 503, json!({"message": "down"})),
        ("GET /repos/o/r/pulls/2 ", 429, json!({"message": "slow"})),
        (
            "GET /repos/o/r/pulls/3 ",
            404,
            json!({"message": "Not Found"}),
        ),
        ("GET /repos/o/r/pulls/4 ", 401, json!({"message": "Bad"})),
        ("GET /repos/o/r/pulls/5 ", 422, json!({"message": "No"})),
    ])
    .await;
    let g = gh(&base, RecordingExec::new(""));
    assert!(g.get_pr("o/r", 1).await.unwrap_err().is_transient());
    assert!(g.get_pr("o/r", 2).await.unwrap_err().is_transient());
    for n in [3, 4, 5] {
        assert!(!g.get_pr("o/r", n).await.unwrap_err().is_transient());
    }
    // list_dir goes through `send` directly; a 5xx there is transient too.
    let (base, _) = serve(vec![("GET /repos/o/r/contents/", 502, json!({}))]).await;
    let g = gh(&base, RecordingExec::new(""));
    assert!(
        g.list_dir("o/r", "main", "d")
            .await
            .unwrap_err()
            .is_transient()
    );
}

#[tokio::test]
async fn a_post_answered_5xx_is_never_transient() {
    let (base, seen) = serve(vec![
        ("POST /repos/o/r/actions/runs/7/approve", 503, json!({})),
        (
            "POST /repos/o/r/actions/runs/7/rerun-failed-jobs",
            429,
            json!({}),
        ),
        ("PUT /repos/o/r/pulls/5/merge", 502, json!({})),
    ])
    .await;
    let g = gh(&base, RecordingExec::new(""));
    assert!(!g.approve_run("o/r", 7).await.unwrap_err().is_transient());
    assert!(
        !g.rerun_failed_jobs("o/r", 7)
            .await
            .unwrap_err()
            .is_transient()
    );
    let opts = MergeOptions {
        method: MergeMethod::Squash,
        admin: false,
        expected_head_sha: "h".into(),
    };
    assert!(
        !g.merge_pr("o/r", 5, &opts)
            .await
            .unwrap_err()
            .is_transient()
    );
    assert_eq!(seen.lock().unwrap().len(), 3);
}

/// A base URL nobody listens on: connecting is refused at once.
async fn dead_base() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    base
}

#[tokio::test]
async fn a_refused_connection_is_transient_for_a_get_only_and_names_its_cause() {
    let g = gh(&dead_base().await, RecordingExec::new(""));
    let err = g.get_pr("o/r", 1).await.unwrap_err();
    assert!(err.is_transient(), "{err}");
    let text = err.to_string();
    assert!(text.starts_with("error sending request"), "{text}");
    assert!(
        text.len() > "error sending request for url".len() + 20,
        "{text}"
    );
    assert!(
        text.to_lowercase().contains("connect") || text.to_lowercase().contains("refused"),
        "{text}"
    );
    let err = g.approve_run("o/r", 7).await.unwrap_err();
    assert!(!err.is_transient(), "{err}");
}
