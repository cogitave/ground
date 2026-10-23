use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinSet;

use super::client::socket_request;
use super::summary::RepoSummary;
use crate::state::StateStore;

const MAX_LIMIT: i64 = 500;
const DEFAULT_LIMIT: i64 = 50;

#[derive(Clone)]
pub struct ServerDeps {
    pub store: Arc<StateStore>,
    pub repos: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub status: Arc<dyn Fn() -> Vec<RepoSummary> + Send + Sync>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Ok(Value),
    Err(String),
}

impl Reply {
    pub fn to_json(&self) -> Value {
        match self {
            Reply::Ok(data) => json!({"ok": true, "data": data}),
            Reply::Err(error) => json!({"ok": false, "error": error}),
        }
    }
}

/// JavaScript-style `String(v)`, so error texts match the reference.
fn show(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

pub fn handle_request(deps: &ServerDeps, raw: &Value) -> Reply {
    let Some(req) = raw.as_object() else {
        return Reply::Err("request must be a JSON object".into());
    };
    match req.get("cmd").and_then(Value::as_str) {
        Some("status") => Reply::Ok(json!((deps.status)())),
        Some("events") => {
            let repo = req.get("repo").and_then(Value::as_str);
            let Some(repo) = repo.filter(|r| (deps.repos)().iter().any(|x| x == r)) else {
                return Reply::Err(format!("unknown repo: {}", show(req.get("repo"))));
            };
            let asked = req
                .get("limit")
                .and_then(|l| {
                    l.as_i64()
                        .or_else(|| l.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
                })
                .unwrap_or(DEFAULT_LIMIT);
            let limit = asked.clamp(1, MAX_LIMIT) as u32;
            match deps.store.list_events(repo, limit) {
                Ok(events) => Reply::Ok(json!(events)),
                Err(e) => Reply::Err(e.to_string()),
            }
        }
        _ => Reply::Err(format!("unknown cmd: {}", show(req.get("cmd")))),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("groundd is already running ({})", .0.display())]
    AlreadyRunning(PathBuf),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub struct RunningServer {
    path: PathBuf,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl RunningServer {
    pub async fn close(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Creates the socket already private, without touching the process-wide
/// umask (which other threads creating files would inherit): bind inside a
/// fresh 0700 directory next to the target, chmod, then rename into place.
/// The socket is never reachable by another user at any point.
fn bind_private(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::DirBuilderExt;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let staging = parent.join(format!(".groundd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let staged = staging.join("s");
    let result = (|| {
        let listener = UnixListener::bind(&staged)?;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

pub async fn start_server(path: &Path, deps: ServerDeps) -> Result<RunningServer, ServerError> {
    if path.exists() {
        // A live daemon owns the socket; only a dead one's leftover may be removed.
        if socket_request(path, &json!({"cmd": "status"}))
            .await
            .is_ok()
        {
            return Err(ServerError::AlreadyRunning(path.to_owned()));
        }
        // Only a leftover socket may go; anything else at the path is not ours.
        use std::os::unix::fs::FileTypeExt;
        if !std::fs::symlink_metadata(path)?.file_type().is_socket() {
            return Err(ServerError::Io(std::io::Error::other(format!(
                "{} exists and is not a socket",
                path.display()
            ))));
        }
        std::fs::remove_file(path)?;
    }
    let listener = bind_private(path)?;
    let (stop, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut conns = JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.changed() => break,
                accepted = listener.accept() => {
                    if let Ok((sock, _)) = accepted {
                        conns.spawn(serve_connection(sock, deps.clone()));
                    }
                }
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
            }
        }
        conns.abort_all();
        while conns.join_next().await.is_some() {}
    });
    Ok(RunningServer {
        path: path.to_owned(),
        stop,
        task,
    })
}

/// A request line longer than this is refused and the connection closed.
const MAX_LINE: u64 = 64 * 1024;
/// A connection that sends nothing for this long is dropped.
const IDLE: std::time::Duration = std::time::Duration::from_secs(60);

/// Reads one `\n`-terminated line of at most `MAX_LINE` bytes.
async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(rd: &mut R) -> Option<Result<String, ()>> {
    let mut buf = Vec::new();
    let n = tokio::time::timeout(
        IDLE,
        (&mut *rd).take(MAX_LINE + 1).read_until(b'\n', &mut buf),
    )
    .await
    .ok()?
    .ok()?;
    if n == 0 {
        return None;
    }
    if buf.len() as u64 > MAX_LINE {
        return Some(Err(()));
    }
    Some(Ok(String::from_utf8_lossy(&buf).into_owned()))
}

async fn serve_connection(sock: UnixStream, deps: ServerDeps) {
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);
    while let Some(line) = read_line(&mut rd).await {
        let reply = match line {
            Err(()) => Reply::Err("request too long".into()),
            Ok(line) if line.trim().is_empty() => continue,
            Ok(line) => match serde_json::from_str::<Value>(&line) {
                // The store takes a blocking lock and does SQLite I/O.
                Ok(req) => {
                    let deps = deps.clone();
                    tokio::task::spawn_blocking(move || handle_request(&deps, &req))
                        .await
                        .unwrap_or_else(|e| Reply::Err(format!("internal error: {e}")))
                }
                Err(err) => Reply::Err(format!("bad request: {err}")),
            },
        };
        let too_long = matches!(&reply, Reply::Err(m) if m == "request too long");
        let mut out = reply.to_json().to_string();
        out.push('\n');
        if wr.write_all(out.as_bytes()).await.is_err() || too_long {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testing::FakeClock;

    const REPO: &str = "cogitave/namzu";

    fn summary() -> RepoSummary {
        RepoSummary {
            repo: REPO.into(),
            main_tip: Some("abc".into()),
            version_pr: None,
            last_event: None,
            active_ship: None,
            last_poll_at: None,
            last_error: None,
        }
    }

    fn deps(store: &Arc<StateStore>) -> ServerDeps {
        ServerDeps {
            store: store.clone(),
            repos: Arc::new(|| vec![REPO.to_owned()]),
            status: Arc::new(|| vec![summary()]),
        }
    }

    fn store() -> Arc<StateStore> {
        Arc::new(StateStore::new(FakeClock::default(), ":memory:").unwrap())
    }

    #[tokio::test]
    async fn the_socket_is_private_to_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let server = start_server(&path, deps(&store())).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        server.close().await;
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn answers_status() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let server = start_server(&path, deps(&store())).await.unwrap();
        let reply = socket_request(&path, &json!({"cmd": "status"}))
            .await
            .unwrap();
        assert_eq!(
            reply,
            json!({"ok": true, "data": [{
                "repo": REPO, "mainTip": "abc", "versionPr": null, "lastEvent": null, "activeShip": null
            }]})
        );
        server.close().await;
    }

    #[tokio::test]
    async fn answers_events_newest_first_and_limited() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let store = store();
        store.record_event(REPO, "a", None).unwrap();
        store.record_event(REPO, "b", None).unwrap();
        let server = start_server(&path, deps(&store)).await.unwrap();
        let reply = socket_request(&path, &json!({"cmd": "events", "repo": REPO, "limit": 1}))
            .await
            .unwrap();
        let kinds: Vec<_> = reply["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].clone())
            .collect();
        assert_eq!(kinds, [json!("b")]);
        server.close().await;
    }

    #[tokio::test]
    async fn refuses_unknown_commands_unknown_repos_and_non_objects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let server = start_server(&path, deps(&store())).await.unwrap();
        assert_eq!(
            socket_request(&path, &json!({"cmd": "nope"}))
                .await
                .unwrap(),
            json!({"ok": false, "error": "unknown cmd: nope"})
        );
        let unknown = socket_request(&path, &json!({"cmd": "events", "repo": "x/y"}))
            .await
            .unwrap();
        assert_eq!(unknown["ok"], false);
        assert_eq!(unknown["error"], "unknown repo: x/y");
        assert_eq!(socket_request(&path, &json!(5)).await.unwrap()["ok"], false);
        server.close().await;
    }

    #[tokio::test]
    async fn answers_garbage_with_an_error_and_keeps_the_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let server = start_server(&path, deps(&store())).await.unwrap();
        let stream = UnixStream::connect(&path).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        wr.write_all(b"not json\n\n{\"cmd\":\"status\"}\n")
            .await
            .unwrap();
        let mut lines = BufReader::new(rd).lines();
        let first: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["ok"], false);
        assert!(
            first["error"]
                .as_str()
                .unwrap()
                .starts_with("bad request: ")
        );
        let second: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(second["ok"], true);
        server.close().await;
    }

    #[tokio::test]
    async fn refuses_an_overlong_request_line_and_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        let server = start_server(&path, deps(&store())).await.unwrap();
        let stream = UnixStream::connect(&path).await.unwrap();
        let (rd, mut wr) = stream.into_split();
        let blob = vec![b'a'; (MAX_LINE as usize) + 10];
        wr.write_all(&blob).await.unwrap();
        let mut lines = BufReader::new(rd).lines();
        let reply: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply, json!({"ok": false, "error": "request too long"}));
        assert!(lines.next_line().await.unwrap().is_none());
        server.close().await;
    }

    #[tokio::test]
    async fn will_not_delete_a_non_socket_at_the_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        std::fs::write(&path, "precious").unwrap();
        let err = start_server(&path, deps(&store())).await.err().unwrap();
        assert!(err.to_string().contains("not a socket"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "precious");
    }

    #[tokio::test]
    async fn replaces_a_stale_socket_file_but_not_a_live_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groundd.sock");
        // A dead daemon's leftover: a socket file nobody listens on.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        let server = start_server(&path, deps(&store())).await.unwrap();
        let second = start_server(&path, deps(&store())).await;
        assert!(matches!(second, Err(ServerError::AlreadyRunning(_))));
        assert!(
            second
                .err()
                .unwrap()
                .to_string()
                .contains("already running")
        );
        server.close().await;
    }
}
