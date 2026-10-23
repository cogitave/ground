use std::path::Path;

use serde_json::json;

use crate::adapters::SystemClock;
use crate::daemon::{
    RepoSummary, db_path, load_repos, socket_path, socket_request, summarize_repo,
};
use crate::state::StateStore;

/// Asks groundd; with no daemon, reads the state db.
pub async fn summaries(home: &Path) -> Result<Vec<RepoSummary>, String> {
    if let Ok(reply) = socket_request(&socket_path(home), &json!({"cmd": "status"})).await
        && reply["ok"] == true
        && let Ok(rows) = serde_json::from_value::<Vec<RepoSummary>>(reply["data"].clone())
    {
        return Ok(rows);
    }
    let loaded = load_repos(home);
    let store = StateStore::new(SystemClock, db_path(home)).map_err(|e| e.to_string())?;
    loaded
        .configs
        .iter()
        .map(|c| summarize_repo(&store, &c.repo).map_err(|e| e.to_string()))
        .collect()
}

pub fn format_status(rows: &[RepoSummary]) -> Vec<String> {
    let mut out = Vec::new();
    for r in rows {
        out.push(r.repo.clone());
        out.push(format!(
            "  main        {}",
            r.main_tip.as_deref().unwrap_or("not seen yet")
        ));
        out.push(format!(
            "  version PR  {}",
            r.version_pr.as_ref().map_or("none".to_owned(), |p| format!(
                "#{} @ {}",
                p.number, p.head_sha
            ))
        ));
        out.push(format!(
            "  ship        {}",
            r.active_ship
                .as_ref()
                .map_or("idle".to_owned(), |s| format!(
                    "{} (PR {})",
                    s.step,
                    s.pr_number.map_or("-".to_owned(), |n| n.to_string())
                ))
        ));
        out.push(format!(
            "  last event  {}",
            r.last_event
                .as_ref()
                .map_or("none".to_owned(), |e| format!("{} at {}", e.kind, e.at))
        ));
        if let Some(err) = &r.last_error {
            out.push(format!("  last error  {err}"));
        }
    }
    out
}
