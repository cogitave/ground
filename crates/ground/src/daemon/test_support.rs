//! Shared fixtures for the daemon tests.

use serde_json::{Value, json};

use crate::config::{RepoConfig, parse_repo_config};

pub fn config_json(repo: &str) -> Value {
    json!({
        "repo": repo,
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

pub fn config(repo: &str) -> RepoConfig {
    parse_repo_config(&config_json(repo), "test").unwrap()
}
