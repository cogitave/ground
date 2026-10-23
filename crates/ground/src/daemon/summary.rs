use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::{StateError, StateStore};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionPrRef {
    pub number: u64,
    pub head_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastEvent {
    pub kind: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveShip {
    pub id: String,
    pub step: String,
    pub pr_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    pub repo: String,
    pub main_tip: Option<String>,
    pub version_pr: Option<VersionPrRef>,
    pub last_event: Option<LastEvent>,
    pub active_ship: Option<ActiveShip>,
    /// Live-only: filled in by the daemon, absent when read from the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_poll_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RecordedState {
    pub main_tip: Option<String>,
    /// `None`: never seen; `Some(None)`: seen and gone.
    pub version_pr: Option<Option<VersionPrRef>>,
    /// run id -> signature of the last recorded state.
    pub runs: HashMap<u64, String>,
}

const SCAN: u32 = 500;

pub fn run_signature(status: &str, conclusion: Option<&str>, run_attempt: u32) -> String {
    format!("{status}/{}/{run_attempt}", conclusion.unwrap_or(""))
}

/// Rebuilds what the watcher last recorded, so a restart does not repeat events.
pub fn recorded_state(store: &StateStore, repo: &str) -> Result<RecordedState, StateError> {
    let mut state = RecordedState::default();
    for e in store.list_events(repo, SCAN)? {
        let p = &e.payload;
        if e.kind == "main.tip" && state.main_tip.is_none() {
            state.main_tip = p["sha"].as_str().map(str::to_owned);
        } else if e.kind.starts_with("version-pr.") && state.version_pr.is_none() {
            state.version_pr = Some(if e.kind == "version-pr.gone" {
                None
            } else {
                Some(VersionPrRef {
                    number: p["number"].as_u64().unwrap_or(0),
                    head_sha: p["headSha"].as_str().unwrap_or("").to_owned(),
                })
            });
        } else if e.kind == "run.state"
            && let Some(id) = p["runId"].as_u64()
        {
            {
                state.runs.entry(id).or_insert_with(|| {
                    let conclusion = match &p["conclusion"] {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    };
                    run_signature(
                        p["status"].as_str().unwrap_or(""),
                        conclusion,
                        p["attempt"].as_u64().unwrap_or(0) as u32,
                    )
                });
            }
        }
    }
    Ok(state)
}

pub fn summarize_repo(store: &StateStore, repo: &str) -> Result<RepoSummary, StateError> {
    let s = recorded_state(store, repo)?;
    let last = store.list_events(repo, 1)?.into_iter().next();
    let ship = store.get_active_ship_session(repo)?;
    Ok(RepoSummary {
        repo: repo.to_owned(),
        main_tip: s.main_tip,
        version_pr: s.version_pr.flatten(),
        last_event: last.map(|e| LastEvent {
            kind: e.kind,
            at: e.at,
        }),
        active_ship: ship.map(|s| ActiveShip {
            id: s.id,
            step: s.step,
            pr_number: s.pr_number,
        }),
        last_poll_at: None,
        last_error: None,
    })
}
