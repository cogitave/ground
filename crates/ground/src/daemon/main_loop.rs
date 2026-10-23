use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use tokio::signal::unix::{SignalKind, signal};

use super::console_logger::ConsoleLogger;
use super::daemon::{DaemonDeps, start_daemon};
use super::paths::{db_path, ground_home, socket_path};
use super::repos::load_repos;
use crate::adapters::{RestGitHub, SystemClock};
use crate::ports::Logger;
use crate::state::StateStore;

/// Runs groundd in the foreground until SIGTERM or SIGINT; returns the exit code.
pub async fn run_daemon(env: &HashMap<String, String>) -> i32 {
    let clock = SystemClock;
    let log: Arc<dyn Logger> = Arc::new(
        ConsoleLogger::new(clock).verbose(env.get("GROUND_DEBUG").is_some_and(|v| v == "1")),
    );
    let home = ground_home(&|k| env.get(k).cloned());
    let loaded = load_repos(&home);
    for e in &loaded.errors {
        log.error("skipping repo config", Some(json!({"error": e})));
    }
    if loaded.configs.is_empty() {
        log.error(
            "no valid repo configs, nothing to watch",
            Some(json!({"home": home})),
        );
        return 1;
    }

    let store = match StateStore::new(clock, db_path(&home)) {
        Ok(s) => Arc::new(s),
        Err(err) => {
            log.error("cannot start", Some(json!({"error": err.to_string()})));
            return 1;
        }
    };
    let (mut term, mut int) = match (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) {
        (Ok(t), Ok(i)) => (t, i),
        (Err(e), _) | (_, Err(e)) => {
            log.error("cannot start", Some(json!({"error": e.to_string()})));
            return 1;
        }
    };
    let daemon = match start_daemon(DaemonDeps {
        configs: loaded.configs,
        github: Arc::new(RestGitHub::new()),
        store,
        clock,
        log: log.clone(),
        socket_path: socket_path(&home),
    })
    .await
    {
        Ok(d) => d,
        Err(err) => {
            log.error("cannot start", Some(json!({"error": err.to_string()})));
            return 1;
        }
    };

    let sig = tokio::select! {
        _ = term.recv() => "SIGTERM",
        _ = int.recv() => "SIGINT",
    };
    log.info("shutting down", Some(json!({"signal": sig})));
    daemon.stop().await;
    0
}
