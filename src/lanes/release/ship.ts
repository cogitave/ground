import { canApproveRun } from '../../approval.js'
import type { PullRequest, WorkflowRun } from '../../ports.js'
import type { ShipSession } from '../../state/index.js'
import { scanBumps } from './bumps.js'
import type {
  BumpedPackage,
  ShipDeps,
  ShipOptions,
  ShipOutcome,
  ShipPlan,
  ShipStatus,
} from './types.js'

type Step = 'find_pr' | 'snapshot' | 'gate' | 'merge' | 'release' | 'registry'

/** Persisted in the session row; everything else is re-derived from GitHub on resume. */
interface ShipData {
  prNumber?: number
  /** The head the CI gate is judging; a different head restarts from the snapshot check. */
  headSha?: string
  packages?: BumpedPackage[]
  unresolved?: string[]
  mergeCommitSha?: string
  baseWasTip?: boolean
  /** Wait deadlines (ISO) by name, so a resumed session keeps its remaining budget. */
  deadlines?: Record<string, string>
  summary?: string
}

type StepResult = { next: Step } | { done: ShipOutcome }

const WAITING = Symbol('waiting')
const TIMEOUT = Symbol('timeout')
type Probe<T> = T | typeof WAITING

/** Outcomes that leave the session open so the next `ship` carries on from the same step. */
const RESUMABLE: ReadonlySet<ShipStatus> = new Set([
  'planned',
  'timed_out',
  'blocked',
  'registry_pending',
])

const OK_JOB = new Set(['success', 'skipped', 'neutral'])
const RELEASE_RERUNS = 1

const CHANGESET_IGNORED = new Set(['README.md', 'config.json'])

function changesetNames(entries: string[]): Set<string> {
  return new Set(
    entries
      .map((e) => e.split('/').pop() as string)
      .filter((n) => n.endsWith('.md') && !CHANGESET_IGNORED.has(n)),
  )
}

function newest(runs: WorkflowRun[]): WorkflowRun | undefined {
  return runs.reduce<WorkflowRun | undefined>((a, r) => (!a || r.id > a.id ? r : a), undefined)
}

/**
 * `ground ship`: version PR to published packages, as a resumable state machine.
 * Every step re-reads GitHub, so the persisted state is only what GitHub cannot
 * tell us (the head being judged, the merge commit, the wait budgets).
 */
export async function ship(deps: ShipDeps, options: ShipOptions): Promise<ShipOutcome> {
  return new ShipRun(deps, options).run()
}

class ShipRun {
  private session!: ShipSession
  private data: ShipData = {}
  private ciReruns = 0
  private releaseReruns = 0
  // Attempt a rerun was requested for, per run: GitHub may still list the old attempt briefly.
  private readonly rerunRequested = new Map<number, number>()

  constructor(
    private readonly deps: ShipDeps,
    private readonly opts: ShipOptions,
  ) {}

  private get cfg() {
    return this.deps.config
  }

  async run(): Promise<ShipOutcome> {
    const { store, logger } = this.deps
    const active = store.getActiveShipSession(this.cfg.repo)
    if (active) {
      logger.info('resuming ship session', { id: active.id, step: active.step })
      this.session = active
    } else {
      this.session = store.createShipSession({ repo: this.cfg.repo, step: 'find_pr' })
    }
    this.data = { ...(this.session.data as ShipData) }

    let step = this.session.step as Step
    for (;;) {
      this.save({ step })
      const result = await this.runStep(step)
      if ('done' in result) return this.finish(result.done)
      step = result.next
    }
  }

  private runStep(step: Step): Promise<StepResult> {
    switch (step) {
      case 'find_pr':
        return this.findPr()
      case 'snapshot':
        return this.snapshot()
      case 'gate':
        return this.gate()
      case 'merge':
        return this.merge()
      case 'release':
        return this.release()
      case 'registry':
        return this.registry()
    }
  }

  // ---- step 1 ----

  private async findPr(): Promise<StepResult> {
    const { github, logger } = this.deps
    const { repo, releaseBranch, defaultBranch } = this.cfg
    let sawRunning = false
    let grace = false
    const probe = async (): Promise<Probe<PullRequest | string>> => {
      const pr = await github.findOpenPr(repo, releaseBranch, defaultBranch)
      if (pr) return pr
      const pending = await this.pendingChangesets()
      if (pending.size === 0)
        return 'The default branch has no pending changesets and no open version PR.'
      if (await this.releaseRunInProgress()) {
        sawRunning = true
        return WAITING
      }
      // The version PR is opened when the run ends; allow one more poll for it.
      if (sawRunning && !grace) {
        grace = true
        return WAITING
      }
      return `${pending.size} changeset(s) are pending but no release run is in progress to open a version PR.`
    }
    const found = await this.waitFor('versionPr', this.cfg.versionPrWaitMinutes, probe)
    if (found === TIMEOUT) {
      return this.done(
        'timed_out',
        `No version PR from ${releaseBranch} appeared within ${this.cfg.versionPrWaitMinutes} minutes.`,
      )
    }
    if (typeof found === 'string') return this.done('nothing_to_ship', `Nothing to ship. ${found}`)
    logger.info('found version PR', { number: found.number })
    this.data.prNumber = found.number
    this.save({ prNumber: found.number })
    return { next: 'snapshot' }
  }

  // ---- step 2 ----

  private async snapshot(): Promise<StepResult> {
    const { github, logger } = this.deps
    const { repo, defaultBranch } = this.cfg
    let diff = ''
    const probe = async (): Promise<Probe<PullRequest | string>> => {
      const pr = await this.currentPr()
      if (!pr) return WAITING
      const onBranch = changesetNames(await github.listDir(repo, defaultBranch, '.changeset'))
      const deleted = changesetNames(
        (await github.listPrFiles(repo, pr.number))
          .filter((f) => f.status === 'removed' && f.path.startsWith('.changeset/'))
          .map((f) => f.path),
      )
      const missing = [...onBranch].filter((n) => !deleted.has(n)).sort()
      const extra = [...deleted].filter((n) => !onBranch.has(n)).sort()
      if (missing.length === 0 && extra.length === 0) return pr
      diff = `not deleted by the PR: [${missing.join(', ')}]; deleted but not on ${defaultBranch}: [${extra.join(', ')}]`
      logger.info('version PR is stale, waiting for the release run to refresh it', {
        missing,
        extra,
      })
      return WAITING
    }
    const res = await this.waitFor('snapshot', this.cfg.versionPrWaitMinutes, probe)
    if (res === TIMEOUT || typeof res === 'string') {
      return this.done(
        'timed_out',
        `Version PR is stale and was not refreshed within ${this.cfg.versionPrWaitMinutes} minutes (${diff}).`,
      )
    }
    this.data.prNumber = res.number
    this.data.headSha = res.headSha
    this.save({ prNumber: res.number })
    return { next: 'gate' }
  }

  /** The version PR as it is now, following a re-created PR; null while there is none open. */
  private async currentPr(): Promise<PullRequest | null> {
    const { github } = this.deps
    const { repo, releaseBranch, defaultBranch } = this.cfg
    return github.findOpenPr(repo, releaseBranch, defaultBranch)
  }

  // ---- steps 3 and 4 ----

  private async gate(): Promise<StepResult> {
    const { github, logger } = this.deps
    const { repo, ciWorkflow } = this.cfg
    const prNumber = this.data.prNumber
    if (prNumber === undefined) return { next: 'find_pr' }
    let blockedBy: string | null = null

    type Verdict =
      | { kind: 'green' }
      | { kind: 'restart' }
      | { kind: 'plan'; plan: ShipPlan }
      | { kind: 'failed'; jobs: string[]; run: WorkflowRun }
      | { kind: 'aborted'; reason: string }

    const probe = async (): Promise<Probe<Verdict>> => {
      const pr = await github.getPr(repo, prNumber)
      if (pr.merged)
        return { kind: 'aborted', reason: `PR #${pr.number} was merged outside ground.` }
      if (pr.state !== 'open') return { kind: 'aborted', reason: `PR #${pr.number} was closed.` }
      if (pr.headSha !== this.data.headSha) {
        logger.info('version PR head moved, re-checking its changesets', {
          from: this.data.headSha,
          to: pr.headSha,
        })
        return { kind: 'restart' }
      }
      // Re-filtered here because the gate must hold even if a port returns more than asked.
      const runs = (
        await github.listWorkflowRuns(repo, ciWorkflow, {
          headSha: pr.headSha,
          event: 'pull_request',
        })
      ).filter((r) => r.headSha === pr.headSha && r.event === 'pull_request')
      const run = newest(runs)
      if (!run) return WAITING
      // GitHub's own attempt counter is the record of how often this run was re-run.
      this.ciReruns = run.runAttempt - 1

      if ((this.rerunRequested.get(run.id) ?? 0) >= run.runAttempt) return WAITING

      if (run.status === 'action_required' || run.conclusion === 'action_required') {
        const decision = canApproveRun(this.cfg, pr, run)
        if (!decision.allowed) {
          const refusal = decision.reason
          if (blockedBy !== refusal)
            logger.warn('CI run needs approval and ground may not give it', {
              runId: run.id,
              reason: refusal,
            })
          blockedBy = refusal
          return WAITING
        }
        const plan: ShipPlan = {
          action: 'approve',
          runId: run.id,
          description: `Approve CI run ${run.id} (${run.htmlUrl}) for ${pr.headRef} at ${pr.headSha.slice(0, 7)}.`,
        }
        if (!this.opts.confirm) return { kind: 'plan', plan }
        await github.approveRun(repo, run.id)
        this.deps.store.recordEvent(repo, 'ship.approve', { runId: run.id, headSha: pr.headSha })
        logger.info('approved CI run', { runId: run.id })
        return WAITING
      }
      blockedBy = null

      if (run.status !== 'completed') return WAITING
      if (run.conclusion === 'success') return { kind: 'green' }

      const failedJobs = (await github.listRunJobs(repo, run.id)).filter(
        (j) => !OK_JOB.has(j.conclusion ?? '') && j.status === 'completed',
      )
      const names = failedJobs.map((j) => j.name)
      if (this.ciReruns >= this.cfg.maxRerunsPerRun) return { kind: 'failed', jobs: names, run }
      const plan: ShipPlan = {
        action: 'rerun',
        runId: run.id,
        description: `Re-run failed jobs of CI run ${run.id}: ${names.join(', ') || '(none listed)'}.`,
      }
      if (!this.opts.confirm) return { kind: 'plan', plan }
      await github.rerunFailedJobs(repo, run.id)
      this.rerunRequested.set(run.id, run.runAttempt)
      for (const job of names)
        this.deps.store.recordFlake({ repo, runId: run.id, job, attempt: run.runAttempt })
      this.deps.store.recordEvent(repo, 'ship.rerun', { runId: run.id, jobs: names })
      logger.warn('re-ran failed CI jobs', { runId: run.id, jobs: names })
      return WAITING
    }

    const verdict = await this.waitFor('ci', this.opts.ciWaitMinutes ?? 120, probe)
    if (verdict === TIMEOUT) {
      return blockedBy
        ? this.done(
            'blocked',
            `CI on the version PR is waiting for an approval ground may not give: ${blockedBy}. Approve it by hand; \`ground ship\` resumes.`,
          )
        : this.done('timed_out', 'CI on the version PR did not finish in time.')
    }
    switch (verdict.kind) {
      case 'green':
        return { next: 'merge' }
      case 'restart':
        return { next: 'snapshot' }
      case 'plan':
        return this.done('planned', verdict.plan.description, { plan: verdict.plan })
      case 'aborted':
        return this.done('aborted', verdict.reason)
      case 'failed':
        return this.done(
          'failed',
          `CI failed on the version PR after ${this.ciReruns} re-run(s): ${verdict.jobs.join(', ') || 'no failing job listed'}. ${verdict.run.htmlUrl}`,
          { failedJobs: verdict.jobs },
        )
    }
  }

  // ---- step 5 ----

  private async merge(): Promise<StepResult> {
    const { github, logger } = this.deps
    const { repo, defaultBranch } = this.cfg
    const prNumber = this.data.prNumber
    if (prNumber === undefined || this.data.headSha === undefined) return { next: 'find_pr' }

    const pr = await github.getPr(repo, prNumber)
    if (pr.merged) {
      // Merged but we never recorded the commit: a crash between the merge and the save.
      const mergeCommitSha = pr.mergeCommitSha ?? (await github.getBranchSha(repo, defaultBranch))
      logger.warn('PR was already merged, taking its merge commit from GitHub', { mergeCommitSha })
      this.data.mergeCommitSha = mergeCommitSha
      if (!this.data.packages) {
        const scan = scanBumps(await github.listPrFiles(repo, prNumber))
        this.data.packages = scan.packages
        this.data.unresolved = scan.unresolved
      }
      this.save({})
      return { next: 'release' }
    }
    if (pr.state !== 'open')
      return this.done('aborted', `PR #${pr.number} was closed before it could be merged.`)
    if (pr.headSha !== this.data.headSha) {
      return this.done(
        'aborted',
        `The version PR head moved from ${this.data.headSha.slice(0, 7)} to ${pr.headSha.slice(0, 7)} after CI passed; not merging. Run \`ground ship\` again.`,
      )
    }

    const scan = scanBumps(await github.listPrFiles(repo, prNumber))
    const baseWasTip = (await github.getBranchSha(repo, defaultBranch)) === pr.baseSha
    const how = `${this.cfg.mergeMethod}${this.cfg.adminMerge ? ' (admin)' : ''}`
    const tipNote = baseWasTip
      ? ''
      : ` ${defaultBranch} has moved since the PR was built, so the release will re-validate inline.`
    if (!this.opts.confirm) {
      const plan: ShipPlan = {
        action: 'merge',
        description: `Merge PR #${pr.number} by ${how} at ${pr.headSha.slice(0, 7)}, publishing ${describe(scan.packages)}.${tipNote}`,
      }
      return this.done('planned', plan.description, { plan, packages: scan.packages })
    }

    // Persist what we are about to do before the write that cannot be undone.
    this.data.packages = scan.packages
    this.data.unresolved = scan.unresolved
    this.data.baseWasTip = baseWasTip
    this.save({})
    const { mergeCommitSha } = await github.mergePr(repo, prNumber, {
      method: this.cfg.mergeMethod,
      admin: this.cfg.adminMerge,
      expectedHeadSha: this.data.headSha,
    })
    this.data.mergeCommitSha = mergeCommitSha
    this.save({ step: 'release' })
    this.deps.store.recordEvent(repo, 'ship.merge', { prNumber, mergeCommitSha, baseWasTip })
    logger.info('merged version PR', { prNumber, mergeCommitSha, baseWasTip })
    return { next: 'release' }
  }

  // ---- step 6 ----

  private async release(): Promise<StepResult> {
    const { github, logger } = this.deps
    const { repo, releaseWorkflow } = this.cfg
    const sha = this.data.mergeCommitSha
    if (!sha) return { next: 'find_pr' }
    let attemptSeen = 1

    type Verdict = { kind: 'green' } | { kind: 'failed'; run: WorkflowRun; jobs: string[] }
    const probe = async (): Promise<Probe<Verdict>> => {
      const run = newest(
        (await github.listWorkflowRuns(repo, releaseWorkflow, { headSha: sha })).filter(
          (r) => r.headSha === sha,
        ),
      )
      if (!run) return WAITING
      attemptSeen = run.runAttempt
      if ((this.rerunRequested.get(run.id) ?? 0) >= run.runAttempt) return WAITING
      if (run.status !== 'completed') return WAITING
      if (run.conclusion === 'success') return { kind: 'green' }
      const jobs = (await github.listRunJobs(repo, run.id))
        .filter((j) => j.status === 'completed' && !OK_JOB.has(j.conclusion ?? ''))
        .map((j) => j.name)
      if (run.runAttempt - 1 >= RELEASE_RERUNS) return { kind: 'failed', run, jobs }
      await github.rerunFailedJobs(repo, run.id)
      this.rerunRequested.set(run.id, run.runAttempt)
      for (const job of jobs)
        this.deps.store.recordFlake({ repo, runId: run.id, job, attempt: run.runAttempt })
      this.deps.store.recordEvent(repo, 'ship.release_rerun', { runId: run.id, jobs })
      logger.warn('re-ran failed release jobs', { runId: run.id, jobs })
      return WAITING
    }
    const verdict = await this.waitFor('release', this.opts.releaseWaitMinutes ?? 90, probe)
    this.releaseReruns = attemptSeen - 1
    if (verdict === TIMEOUT)
      return this.done(
        'timed_out',
        `The release run for ${sha.slice(0, 7)} did not finish in time. The PR is merged; \`ground ship\` resumes the wait.`,
        { mergeCommitSha: sha },
      )
    if (verdict.kind === 'failed') {
      return this.done(
        'failed',
        `The release run failed after a re-run (${verdict.jobs.join(', ') || 'no failing job listed'}). The PR is merged but nothing is confirmed published. ${verdict.run.htmlUrl}`,
        { failedJobs: verdict.jobs, mergeCommitSha: sha },
      )
    }
    return { next: 'registry' }
  }

  // ---- step 7 ----

  private async registry(): Promise<StepResult> {
    const { registry, logger } = this.deps
    const packages = this.data.packages ?? []
    const missing = new Map(packages.map((p) => [p.name, p]))
    const probe = async (): Promise<Probe<true>> => {
      for (const [name, pkg] of [...missing]) {
        if ((await registry.latestVersion(name)) === pkg.to) missing.delete(name)
      }
      return missing.size === 0 ? true : WAITING
    }
    if (packages.length > 0) {
      const res = await this.waitFor('registry', this.cfg.registryLagMinutes, probe)
      if (res === TIMEOUT) {
        const left = [...missing.values()]
        logger.warn('registry never showed the new versions', { missing: left })
        return this.done(
          'registry_pending',
          `Release run is green but the registry does not show ${describe(left)} after ${this.cfg.registryLagMinutes} minutes. A green run is not a publish; check the registry again later with \`ground ship\`.`,
          { missingPackages: left, mergeCommitSha: this.data.mergeCommitSha },
        )
      }
    }
    return this.done('shipped', this.shippedSummary(packages), {
      mergeCommitSha: this.data.mergeCommitSha,
      baseWasTip: this.data.baseWasTip,
    })
  }

  private shippedSummary(packages: BumpedPackage[]): string {
    const lines = [
      `Shipped PR #${this.data.prNumber}: ${packages.length > 0 ? `${describe(packages)} confirmed on the registry` : 'no package versions to confirm'}.`,
    ]
    if (this.data.baseWasTip === false)
      lines.push(
        `${this.cfg.defaultBranch} had moved before the merge, so the release re-validated inline.`,
      )
    if (this.reruns > 0) lines.push(`Re-runs used: ${this.reruns}.`)
    const unresolved = this.data.unresolved ?? []
    if (unresolved.length > 0)
      lines.push(
        `Not verified on the registry (package name not visible in the diff): ${unresolved.join(', ')}.`,
      )
    return lines.join(' ')
  }

  // ---- plumbing ----

  /**
   * Polls `probe` until it answers, sleeping pollSeconds between tries. The probe
   * always runs once, so an expired budget (e.g. a resumed session) still looks.
   * The deadline is persisted, and cleared on exit so the next wait starts fresh.
   */
  private async waitFor<T>(
    key: string,
    minutes: number,
    probe: () => Promise<Probe<T>>,
  ): Promise<T | typeof TIMEOUT> {
    const { clock } = this.deps
    this.data.deadlines ??= {}
    const deadlines = this.data.deadlines
    if (!deadlines[key]) {
      deadlines[key] = new Date(clock.now().getTime() + minutes * 60_000).toISOString()
      this.save({})
    }
    const deadline = new Date(deadlines[key] as string).getTime()
    let result: T | typeof TIMEOUT
    for (;;) {
      const v = await probe()
      if (v !== WAITING) {
        result = v
        break
      }
      if (clock.now().getTime() >= deadline) {
        result = TIMEOUT
        break
      }
      await clock.sleep(this.cfg.pollSeconds * 1000)
    }
    delete deadlines[key]
    this.save({})
    return result
  }

  private get reruns(): number {
    return this.ciReruns + this.releaseReruns
  }

  private async pendingChangesets(): Promise<Set<string>> {
    return changesetNames(
      await this.deps.github.listDir(this.cfg.repo, this.cfg.defaultBranch, '.changeset'),
    )
  }

  private async releaseRunInProgress(): Promise<boolean> {
    const runs = await this.deps.github.listWorkflowRuns(this.cfg.repo, this.cfg.releaseWorkflow, {
      branch: this.cfg.defaultBranch,
    })
    return runs.some((r) => r.status !== 'completed')
  }

  private save(patch: { step?: Step; prNumber?: number }): void {
    this.session = this.deps.store.updateShipSession(this.session.id, {
      ...patch,
      data: this.data as Record<string, unknown>,
    })
  }

  private done(status: ShipStatus, summary: string, extra: Partial<ShipOutcome> = {}): StepResult {
    return {
      done: {
        status,
        summary,
        sessionId: this.session.id,
        prNumber: this.data.prNumber ?? null,
        packages: this.data.packages ?? [],
        reruns: this.reruns,
        ...extra,
      },
    }
  }

  private finish(outcome: ShipOutcome): ShipOutcome {
    const { store, logger } = this.deps
    this.data.summary = outcome.summary
    store.updateShipSession(this.session.id, {
      data: this.data as Record<string, unknown>,
      outcome: RESUMABLE.has(outcome.status) ? null : outcome.status,
    })
    store.recordEvent(this.cfg.repo, `ship.${outcome.status}`, {
      sessionId: outcome.sessionId,
      prNumber: outcome.prNumber,
      summary: outcome.summary,
    })
    logger.info(outcome.summary, { status: outcome.status })
    return outcome
  }
}

function describe(packages: BumpedPackage[]): string {
  return packages.length === 0 ? 'no packages' : packages.map((p) => `${p.name}@${p.to}`).join(', ')
}
