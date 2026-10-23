//! Per-repository configuration (`~/.ground/repos/<owner>__<repo>.json`).
//!
//! Validation is written by hand rather than derived so that every failure
//! names the offending key, exactly as the operator reads it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ports::{MergeMethod, RepoSlug};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepoConfig {
    pub repo: RepoSlug,
    pub default_branch: String,
    pub adapter: String,
    pub ci_workflow: String,
    pub release_workflow: String,
    pub release_branch: String,
    pub approve_bot_runs: bool,
    pub approve_branches: Vec<String>,
    pub admin_merge: bool,
    pub merge_method: MergeMethod,
    pub max_reruns_per_run: u64,
    pub version_pr_wait_minutes: u64,
    pub registry_lag_minutes: u64,
    pub poll_seconds: u64,
}

/// A configuration problem; the message starts with the source (file or "config").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConfigError(pub String);

const MERGE_METHODS: [&str; 3] = ["squash", "merge", "rebase"];
const KEYS: [&str; 14] = [
    "repo",
    "defaultBranch",
    "adapter",
    "ciWorkflow",
    "releaseWorkflow",
    "releaseBranch",
    "approveBotRuns",
    "approveBranches",
    "adminMerge",
    "mergeMethod",
    "maxRerunsPerRun",
    "versionPrWaitMinutes",
    "registryLagMinutes",
    "pollSeconds",
];

fn fail<T>(source: &str, msg: impl AsRef<str>) -> Result<T, ConfigError> {
    Err(ConfigError(format!("{source}: {}", msg.as_ref())))
}

fn str_of(o: &Map<String, Value>, key: &str, source: &str) -> Result<String, ConfigError> {
    match o.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => fail(source, format!("\"{key}\" must be a non-empty string")),
    }
}

fn bool_of(o: &Map<String, Value>, key: &str, source: &str) -> Result<bool, ConfigError> {
    match o.get(key) {
        Some(Value::Bool(b)) => Ok(*b),
        _ => fail(source, format!("\"{key}\" must be a boolean")),
    }
}

fn int_of(o: &Map<String, Value>, key: &str, source: &str, min: u64) -> Result<u64, ConfigError> {
    let parsed = match o.get(key) {
        Some(Value::Number(n)) => n.as_u64().or_else(|| {
            // JSON has one number type: 30.0 is the integer 30, as in JavaScript.
            n.as_f64()
                .filter(|f| f.fract() == 0.0 && *f >= 0.0 && *f <= u64::MAX as f64)
                .map(|f| f as u64)
        }),
        _ => None,
    };
    match parsed {
        Some(v) if v >= min => Ok(v),
        _ => fail(source, format!("\"{key}\" must be an integer >= {min}")),
    }
}

fn valid_slug(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let ok = |p: &str| !p.is_empty() && !p.chars().any(char::is_whitespace);
    ok(owner) && ok(name)
}

pub fn parse_repo_config(raw: &Value, source: &str) -> Result<RepoConfig, ConfigError> {
    let Value::Object(o) = raw else {
        return fail(source, "must be a JSON object");
    };
    let repo = str_of(o, "repo", source)?;
    if !valid_slug(&repo) {
        return fail(
            source,
            format!("\"repo\" must look like owner/name, got \"{repo}\""),
        );
    }

    let branches: Vec<String> = match o.get("approveBranches") {
        Some(Value::Array(items))
            if items
                .iter()
                .all(|b| matches!(b, Value::String(s) if !s.is_empty())) =>
        {
            items
                .iter()
                .filter_map(|b| b.as_str().map(str::to_owned))
                .collect()
        }
        _ => {
            return fail(
                source,
                "\"approveBranches\" must be an array of non-empty strings",
            );
        }
    };
    let merge_method = match o.get("mergeMethod").and_then(Value::as_str) {
        Some("squash") => MergeMethod::Squash,
        Some("merge") => MergeMethod::Merge,
        Some("rebase") => MergeMethod::Rebase,
        _ => {
            return fail(
                source,
                format!(
                    "\"mergeMethod\" must be one of {}",
                    MERGE_METHODS.join(", ")
                ),
            );
        }
    };
    let approve_bot_runs = bool_of(o, "approveBotRuns", source)?;
    if approve_bot_runs && branches.is_empty() {
        return fail(
            source,
            "\"approveBotRuns\" needs at least one entry in \"approveBranches\"",
        );
    }

    let config = RepoConfig {
        repo,
        default_branch: str_of(o, "defaultBranch", source)?,
        adapter: str_of(o, "adapter", source)?,
        ci_workflow: str_of(o, "ciWorkflow", source)?,
        release_workflow: str_of(o, "releaseWorkflow", source)?,
        release_branch: str_of(o, "releaseBranch", source)?,
        approve_bot_runs,
        approve_branches: branches,
        admin_merge: bool_of(o, "adminMerge", source)?,
        merge_method,
        max_reruns_per_run: int_of(o, "maxRerunsPerRun", source, 0)?,
        version_pr_wait_minutes: int_of(o, "versionPrWaitMinutes", source, 0)?,
        registry_lag_minutes: int_of(o, "registryLagMinutes", source, 0)?,
        poll_seconds: int_of(o, "pollSeconds", source, 1)?,
    };

    if let Some(unknown) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return fail(source, format!("unknown key \"{unknown}\""));
    }
    Ok(config)
}

pub fn load_repo_config(path: &Path) -> Result<RepoConfig, ConfigError> {
    let source = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(err) => return fail(&source, format!("cannot read file ({err})")),
    };
    let json: Value = match serde_json::from_str(&text) {
        Ok(j) => j,
        Err(err) => return fail(&source, format!("invalid JSON ({err})")),
    };
    parse_repo_config(&json, &source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid() -> Value {
        json!({
            "repo": "cogitave/namzu",
            "defaultBranch": "main",
            "adapter": "pnpm-changesets",
            "ciWorkflow": "ci.yml",
            "releaseWorkflow": "release.yml",
            "releaseBranch": "changeset-release/main",
            "approveBotRuns": false,
            "approveBranches": ["changeset-release/main"],
            "adminMerge": true,
            "mergeMethod": "squash",
            "maxRerunsPerRun": 1,
            "versionPrWaitMinutes": 45,
            "registryLagMinutes": 10,
            "pollSeconds": 30
        })
    }

    fn with(key: &str, value: Value) -> Value {
        let mut v = valid();
        v[key] = value;
        v
    }

    fn err_of(raw: &Value) -> String {
        parse_repo_config(raw, "config").unwrap_err().to_string()
    }

    #[test]
    fn accepts_the_design_example() {
        let c = parse_repo_config(&valid(), "config").unwrap();
        assert_eq!(c.repo, "cogitave/namzu");
        assert_eq!(c.merge_method, MergeMethod::Squash);
        assert_eq!(c.poll_seconds, 30);
        // Round-trips through serde with the same camelCase keys.
        assert_eq!(serde_json::to_value(&c).unwrap(), valid());
    }

    #[test]
    fn rejects_bad_values_naming_the_key() {
        let cases = [
            ("repo", json!("nope"), "owner/name"),
            ("mergeMethod", json!("fast-forward"), "mergeMethod"),
            ("pollSeconds", json!(0), "pollSeconds"),
            ("maxRerunsPerRun", json!(1.5), "maxRerunsPerRun"),
            ("adminMerge", json!("yes"), "adminMerge"),
            ("approveBranches", json!([1]), "approveBranches"),
            ("ciWorkflow", json!(""), "ciWorkflow"),
        ];
        for (key, value, needle) in cases {
            let message = err_of(&with(key, value));
            assert!(message.contains(needle), "{key}: {message}");
            assert!(message.starts_with("config: "), "{message}");
        }
    }

    #[test]
    fn rejects_a_missing_key_and_names_it() {
        let mut raw = valid();
        raw.as_object_mut().unwrap().remove("releaseBranch");
        assert!(err_of(&raw).contains("releaseBranch"));
    }

    #[test]
    fn refuses_approving_bot_runs_with_an_empty_allow_list() {
        let mut raw = with("approveBotRuns", json!(true));
        raw["approveBranches"] = json!([]);
        assert!(err_of(&raw).contains("approveBranches"));
    }

    #[test]
    fn rejects_non_objects_and_unknown_keys() {
        assert!(err_of(&json!([])).contains("must be a JSON object"));
        assert!(err_of(&Value::Null).contains("must be a JSON object"));
        assert!(err_of(&with("approveBotRun", json!(true))).contains("approveBotRun"));
    }

    #[test]
    fn slugs_need_exactly_owner_and_name() {
        for bad in ["a", "a/b/c", "/b", "a/", "a b/c", "a/b c"] {
            assert!(!valid_slug(bad), "{bad}");
        }
        assert!(valid_slug("a/b"));
    }

    #[test]
    fn integers_accept_whole_floats_only() {
        assert_eq!(
            parse_repo_config(&with("pollSeconds", json!(30.0)), "c")
                .unwrap()
                .poll_seconds,
            30
        );
        assert!(err_of(&with("versionPrWaitMinutes", json!(-1))).contains("versionPrWaitMinutes"));
    }

    #[test]
    fn loads_a_file_and_names_it_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let ok = dir.path().join("ok.json");
        std::fs::write(&ok, valid().to_string()).unwrap();
        assert_eq!(load_repo_config(&ok).unwrap().repo, "cogitave/namzu");

        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{").unwrap();
        assert!(
            load_repo_config(&bad)
                .unwrap_err()
                .to_string()
                .contains("bad.json: invalid JSON")
        );
        let missing = dir.path().join("missing.json");
        assert!(
            load_repo_config(&missing)
                .unwrap_err()
                .to_string()
                .contains("cannot read file")
        );
    }
}
