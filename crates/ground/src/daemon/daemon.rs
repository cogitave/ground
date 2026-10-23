use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::server::{RunningServer, ServerDeps, ServerError, start_server};
use super::summary::summarize_repo;
use super::watcher::{WatchStatus, Watcher};
use crate::config::RepoConfig;
use crate::ports::{Clock, GitHubPort, Logger};
use crate::state::{StateError, StateStore};

pub struct DaemonDeps<G, C> {
    pub configs: Vec<RepoConfig>,
    pub github: Arc<G>,
    pub store: Arc<StateStore>,
    pub clock: C,
    pub log: Arc<dyn Logger>,
    pub socket_path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error(transparent)]
    State(#[from] StateError),
}

pub struct RunningDaemon {
    stop: watch::Sender<bool>,
    loops: Vec<JoinHandle<()>>,
    server: RunningServer,
    log: Arc<dyn Logger>,
}

impl RunningDaemon {
    /// Stops the loops and the socket; the caller owns the store and closes it afterwards.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        for l in self.loops {
            let _ = l.await;
        }
        self.server.close().await;
        self.log.info("groundd stopped", None);
    }
}

pub async fn start_daemon<G, C>(deps: DaemonDeps<G, C>) -> Result<RunningDaemon, DaemonError>
where
    G: GitHubPort + 'static,
    C: Clock + Clone,
{
    let DaemonDeps {
        configs,
        github,
        store,
        clock,
        log,
        socket_path,
    } = deps;

    let mut watchers = Vec::new();
    let mut statuses: HashMap<String, Arc<Mutex<WatchStatus>>> = HashMap::new();
    for c in &configs {
        let w = Watcher::new(
            c.clone(),
            github.clone(),
            store.clone(),
            clock.clone(),
            log.clone(),
        )?;
        statuses.insert(c.repo.clone(), w.status_handle());
        watchers.push(w);
    }

    let names: Vec<String> = configs.iter().map(|c| c.repo.clone()).collect();
    let server = start_server(
        &socket_path,
        ServerDeps {
            store: store.clone(),
            repos: {
                let names = names.clone();
                Arc::new(move || names.clone())
            },
            status: {
                let names = names.clone();
                let store = store.clone();
                Arc::new(move || {
                    names
                        .iter()
                        .filter_map(|repo| {
                            let mut s = summarize_repo(&store, repo).ok()?;
                            if let Some(w) = statuses.get(repo) {
                                let w = w.lock().unwrap_or_else(|e| e.into_inner());
                                s.last_poll_at = w.last_poll_at.clone();
                                s.last_error = w.last_error.clone();
                            }
                            Some(s)
                        })
                        .collect()
                })
            },
        },
    )
    .await?;

    let (stop, stopped) = watch::channel(false);
    let loops = watchers
        .into_iter()
        .map(|mut w| {
            let rx = stopped.clone();
            tokio::spawn(async move { w.run(rx).await })
        })
        .collect();
    log.info("groundd started", Some(json!({"repos": names})));

    Ok(RunningDaemon {
        stop,
        loops,
        server,
        log,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::daemon::socket_request;
    use crate::testing::{FakeClock, FakeGitHub, MemoryLogger};

    #[tokio::test]
    async fn serves_live_status_and_stops_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("groundd.sock");
        let gh = FakeGitHub::new();
        gh.set_branch_sha("a/b", "main", "tip-1");
        let clock = FakeClock::default();
        let store = Arc::new(StateStore::new(clock.clone(), ":memory:").unwrap());
        let config = crate::daemon::test_support::config("a/b");
        let daemon = start_daemon(DaemonDeps {
            configs: vec![config],
            github: Arc::new(gh),
            store,
            clock,
            log: Arc::new(MemoryLogger::new()),
            socket_path: sock.clone(),
        })
        .await
        .unwrap();
        // The fake clock's sleep returns at once, so the loop polls continuously;
        // wait on the socket until the first poll has recorded the tip.
        let mut tip = Value::Null;
        for _ in 0..1000 {
            let reply = socket_request(&sock, &json!({"cmd": "status"}))
                .await
                .unwrap();
            tip = reply["data"][0]["mainTip"].clone();
            if !tip.is_null() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(tip, "tip-1");
        daemon.stop().await;
        assert!(!sock.exists());
    }
}
