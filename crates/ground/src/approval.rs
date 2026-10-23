//! Hard rule 4: an `action_required` run may be approved only when the PR's
//! head lives in the base repository (never a fork), its branch is on the
//! repo's allow-list, and the run is for the PR's current head SHA. Opt-in per
//! repository. Pure on purpose: the one place the rule is written down.

use crate::config::RepoConfig;
use crate::ports::{PrState, PullRequest, WorkflowRun};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allowed,
    Refused { reason: String },
}

impl ApprovalDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allowed)
    }
}

/// The slice of [`RepoConfig`] the rule reads.
#[derive(Debug, Clone, Copy)]
pub struct ApprovalPolicy<'a> {
    pub repo: &'a str,
    pub approve_bot_runs: bool,
    pub approve_branches: &'a [String],
}

impl<'a> From<&'a RepoConfig> for ApprovalPolicy<'a> {
    fn from(c: &'a RepoConfig) -> Self {
        Self {
            repo: &c.repo,
            approve_bot_runs: c.approve_bot_runs,
            approve_branches: &c.approve_branches,
        }
    }
}

fn refuse(reason: impl Into<String>) -> ApprovalDecision {
    ApprovalDecision::Refused {
        reason: reason.into(),
    }
}

pub fn can_approve_run<'a>(
    policy: impl Into<ApprovalPolicy<'a>>,
    pr: &PullRequest,
    run: &WorkflowRun,
) -> ApprovalDecision {
    let policy = policy.into();
    if !policy.approve_bot_runs {
        return refuse("approveBotRuns is off");
    }
    if pr.state != PrState::Open {
        return refuse("the PR is not open");
    }
    // GitHub slugs are case-insensitive.
    if pr.head_repo.to_lowercase() != policy.repo.to_lowercase() {
        return refuse(format!(
            "head repository {} is not {}",
            pr.head_repo, policy.repo
        ));
    }
    if !policy.approve_branches.contains(&pr.head_ref) {
        return refuse(format!("branch {} is not on the allow-list", pr.head_ref));
    }
    if run.head_branch != pr.head_ref {
        return refuse(format!(
            "run is for branch {}, not {}",
            run.head_branch, pr.head_ref
        ));
    }
    if run.head_sha != pr.head_sha {
        return refuse("run is not for the PR head SHA");
    }
    ApprovalDecision::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{make_pr, make_run};

    fn policy(branches: &[String], on: bool) -> ApprovalPolicy<'_> {
        ApprovalPolicy {
            repo: "cogitave/namzu",
            approve_bot_runs: on,
            approve_branches: branches,
        }
    }

    #[test]
    fn truth_table() {
        let default_branches = vec!["changeset-release/main".to_string()];
        let empty: Vec<String> = vec![];
        type Tweak = fn(&mut PullRequest, &mut WorkflowRun);
        // (name, opt-in, allow-list is empty, tweak, allowed)
        let cases: Vec<(&str, bool, bool, Tweak, bool)> = vec![
            ("all conditions hold", true, false, |_, _| {}, true),
            ("opt-in is off", false, false, |_, _| {}, false),
            (
                "head is a fork",
                true,
                false,
                |p, _| p.head_repo = "mallory/namzu".into(),
                false,
            ),
            (
                "branch not allow-listed",
                true,
                false,
                |p, r| {
                    p.head_ref = "feature/x".into();
                    r.head_branch = "feature/x".into();
                },
                false,
            ),
            ("empty allow-list", true, true, |_, _| {}, false),
            (
                "run is for an older SHA",
                true,
                false,
                |_, r| r.head_sha = "old-sha".into(),
                false,
            ),
            (
                "run is for another branch",
                true,
                false,
                |_, r| r.head_branch = "other".into(),
                false,
            ),
            (
                "PR is closed",
                true,
                false,
                |p, _| p.state = PrState::Closed,
                false,
            ),
            (
                "repo slug differs only by case",
                true,
                false,
                |p, _| p.head_repo = "Cogitave/Namzu".into(),
                true,
            ),
        ];
        for (name, on, empty_list, tweak, allowed) in cases {
            let mut pr = make_pr(|_| {});
            let mut run = make_run(|_| {});
            tweak(&mut pr, &mut run);
            let branches = if empty_list {
                &empty
            } else {
                &default_branches
            };
            let d = can_approve_run(policy(branches, on), &pr, &run);
            assert_eq!(d.is_allowed(), allowed, "{name}");
            if let ApprovalDecision::Refused { reason } = d {
                assert!(!reason.is_empty(), "{name}: reason is empty");
            }
        }
    }
}
