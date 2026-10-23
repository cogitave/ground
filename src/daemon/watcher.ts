import { canApproveRun } from '../approval.js'
import type { RepoConfig } from '../config.js'
import type { Clock, GitHubPort, Logger, PullRequest, WorkflowRun } from '../ports.js'
import type { StateStore } from '../state/index.js'
import { type RecordedState, recordedState, runSignature } from './summary.js'

/** The newest runs only: a first poll must not replay a repository's whole history. */
const RUNS_PER_POLL = 10

const isActionRequired = (r: WorkflowRun): boolean =>
  r.status === 'action_required' || r.conclusion === 'action_required'

export class Watcher {
  lastPollAt: string | null = null
  lastError: string | null = null
  private seen: RecordedState
  private readonly approveTried = new Set<number>()

  constructor(
    private readonly config: RepoConfig,
    private readonly github: GitHubPort,
    private readonly store: StateStore,
    private readonly clock: Clock,
    private readonly log: Logger,
  ) {
    this.seen = recordedState(store, config.repo)
  }

  /** One poll. Records only what changed since the last one; never throws. */
  async tick(): Promise<void> {
    try {
      await this.poll()
      this.lastError = null
    } catch (err) {
      const msg = (err as Error).message
      // Log each distinct failure once, not once per poll.
      if (msg !== this.lastError)
        this.log.error('poll failed', { repo: this.config.repo, error: msg })
      this.lastError = msg
    }
    this.lastPollAt = this.clock.now().toISOString()
  }

  /** Polls until `stopped` resolves. Sleeping is raced against it so shutdown is prompt. */
  async run(stopped: Promise<void>): Promise<void> {
    let done = false
    void stopped.then(() => {
      done = true
    })
    while (!done) {
      await this.tick()
      if (done) break
      await Promise.race([this.clock.sleep(this.config.pollSeconds * 1000), stopped])
    }
  }

  private async poll(): Promise<void> {
    const { repo, defaultBranch, releaseBranch } = this.config

    const tip = await this.github.getBranchSha(repo, defaultBranch)
    if (tip !== this.seen.mainTip) {
      this.store.recordEvent(repo, 'main.tip', { sha: tip, previous: this.seen.mainTip ?? null })
      this.seen.mainTip = tip
    }

    const pr = await this.github.findOpenPr(repo, releaseBranch, defaultBranch)
    this.recordVersionPr(pr)

    const ciRuns = await this.github.listWorkflowRuns(
      repo,
      this.config.ciWorkflow,
      pr
        ? { headSha: pr.headSha, event: 'pull_request' }
        : { branch: releaseBranch, event: 'pull_request' },
    )
    const releaseRuns = await this.github.listWorkflowRuns(repo, this.config.releaseWorkflow, {
      branch: defaultBranch,
    })
    for (const run of [...ciRuns, ...releaseRuns].slice(0, RUNS_PER_POLL * 2)) this.recordRun(run)

    if (pr) {
      for (const run of ciRuns.filter(isActionRequired)) await this.maybeApprove(pr, run)
    }
  }

  private recordVersionPr(pr: PullRequest | null): void {
    const { repo } = this.config
    const prev = this.seen.versionPr
    if (pr) {
      const next = { number: pr.number, headSha: pr.headSha }
      if (prev && prev.number === next.number && prev.headSha === next.headSha) return
      const sameNumber = prev?.number === next.number
      this.store.recordEvent(repo, sameNumber ? 'version-pr.head-changed' : 'version-pr.appeared', {
        ...next,
        ...(sameNumber ? { previousHeadSha: prev?.headSha } : {}),
      })
      this.seen.versionPr = next
    } else if (prev) {
      this.store.recordEvent(repo, 'version-pr.gone', { number: prev.number })
      this.seen.versionPr = null
    }
  }

  private recordRun(run: WorkflowRun): void {
    const sig = runSignature(run)
    if (this.seen.runs.get(run.id) === sig) return
    this.store.recordEvent(this.config.repo, 'run.state', {
      runId: run.id,
      workflow: run.workflowFile,
      status: run.status,
      conclusion: run.conclusion,
      attempt: run.runAttempt,
      headSha: run.headSha,
      branch: run.headBranch,
      url: run.htmlUrl,
    })
    this.seen.runs.set(run.id, sig)
  }

  private async maybeApprove(listed: PullRequest, run: WorkflowRun): Promise<void> {
    if (this.approveTried.has(run.id)) return
    if (!this.config.approveBotRuns) return
    // The head may have moved since the list; decide on the PR as it is now.
    const pr = await this.github.getPr(this.config.repo, listed.number)
    const decision = canApproveRun(this.config, pr, run)
    if (!decision.allowed) {
      this.log.debug('not approving run', { runId: run.id, reason: decision.reason })
      return
    }
    this.approveTried.add(run.id)
    await this.github.approveRun(this.config.repo, run.id)
    this.store.recordEvent(this.config.repo, 'run.approved', {
      runId: run.id,
      prNumber: pr.number,
      headSha: pr.headSha,
    })
    this.log.info('approved run', { repo: this.config.repo, runId: run.id })
  }
}
