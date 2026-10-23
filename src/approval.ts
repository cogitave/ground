import type { RepoConfig } from './config.js'
import type { PullRequest, WorkflowRun } from './ports.js'

export type ApprovalDecision = { allowed: true } | { allowed: false; reason: string }

/**
 * Hard rule 4: an action_required run may be approved only when the PR's head
 * lives in the base repository (never a fork), its branch is on the repo's
 * allow-list, and the run is for the PR's current head SHA. Opt-in per repo.
 * Pure on purpose: the one place the rule is written down.
 */
export function canApproveRun(
  config: Pick<RepoConfig, 'repo' | 'approveBotRuns' | 'approveBranches'>,
  pr: PullRequest,
  run: WorkflowRun,
): ApprovalDecision {
  if (!config.approveBotRuns) return { allowed: false, reason: 'approveBotRuns is off' }
  if (pr.state !== 'open') return { allowed: false, reason: 'the PR is not open' }
  // GitHub slugs are case-insensitive.
  if (pr.headRepo.toLowerCase() !== config.repo.toLowerCase()) {
    return { allowed: false, reason: `head repository ${pr.headRepo} is not ${config.repo}` }
  }
  if (!config.approveBranches.includes(pr.headRef)) {
    return { allowed: false, reason: `branch ${pr.headRef} is not on the allow-list` }
  }
  if (run.headBranch !== pr.headRef) {
    return { allowed: false, reason: `run is for branch ${run.headBranch}, not ${pr.headRef}` }
  }
  if (run.headSha !== pr.headSha) {
    return { allowed: false, reason: 'run is not for the PR head SHA' }
  }
  return { allowed: true }
}
