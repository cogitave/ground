import type { GitHubPort, PrFile, PullRequest, RepoSlug, RunJob, WorkflowRun } from '../ports.js'

export function makePr(overrides: Partial<PullRequest> = {}): PullRequest {
  return {
    number: 1,
    title: 'chore(release): version packages',
    state: 'open',
    merged: false,
    headRef: 'changeset-release/main',
    headSha: 'head-sha-1',
    headRepo: 'cogitave/namzu',
    baseRef: 'main',
    baseSha: 'base-sha-1',
    author: 'github-actions[bot]',
    mergeable: true,
    ...overrides,
  }
}

export function makeRun(overrides: Partial<WorkflowRun> = {}): WorkflowRun {
  const id = overrides.id ?? 100
  return {
    id,
    workflowFile: 'ci.yml',
    event: 'pull_request',
    headSha: 'head-sha-1',
    headBranch: 'changeset-release/main',
    status: 'queued',
    conclusion: null,
    runAttempt: 1,
    createdAt: '2026-01-01T00:00:00Z',
    actor: 'github-actions[bot]',
    triggeringActor: 'github-actions[bot]',
    htmlUrl: `https://github.com/cogitave/namzu/actions/runs/${id}`,
    ...overrides,
  }
}

export type RunStage = Partial<WorkflowRun>

export interface RunScript {
  /** Applied one per poll (each listWorkflowRuns call that returns the run), after it is returned. */
  stages?: RunStage[]
  /** Replaces the remaining stages when the run is approved. */
  onApprove?: RunStage[]
  /** Replaces the remaining stages when failed jobs are re-run; runAttempt is bumped. */
  onRerun?: RunStage[]
}

export interface WriteCall {
  method: 'approveRun' | 'rerunFailedJobs' | 'mergePr'
  repo: RepoSlug
  args: Record<string, unknown>
}

interface ScriptedRun {
  run: WorkflowRun
  repo: RepoSlug
  script: RunScript
  pending: RunStage[]
}

/** In-memory GitHubPort. Reads are scripted through the set and add helpers; every write is recorded. */
export class FakeGitHub implements GitHubPort {
  readonly writes: WriteCall[] = []
  /** Called after a successful merge, so a test can make the release run appear. */
  onMerge?: (pr: PullRequest, mergeCommitSha: string) => void

  private readonly prs = new Map<string, PullRequest>()
  private readonly files = new Map<string, PrFile[]>()
  private readonly dirs = new Map<string, string[]>()
  private readonly branches = new Map<string, string>()
  private readonly jobs = new Map<string, RunJob[]>()
  private readonly runs: ScriptedRun[] = []
  private readonly mergeShas = new Map<string, string>()

  // ---- scripting ----

  setPr(repo: RepoSlug, pr: PullRequest): this {
    this.prs.set(`${repo}#${pr.number}`, pr)
    return this
  }

  /** Mutates a stored PR, e.g. to simulate the head moving between two reads. */
  updatePr(repo: RepoSlug, number: number, patch: Partial<PullRequest>): this {
    this.prs.set(`${repo}#${number}`, { ...this.requirePr(repo, number), ...patch })
    return this
  }

  setPrFiles(repo: RepoSlug, number: number, files: PrFile[]): this {
    this.files.set(`${repo}#${number}`, files)
    return this
  }

  setDir(repo: RepoSlug, ref: string, path: string, entries: string[]): this {
    this.dirs.set(`${repo}@${ref}:${path}`, entries)
    return this
  }

  setBranchSha(repo: RepoSlug, branch: string, sha: string): this {
    this.branches.set(`${repo}@${branch}`, sha)
    return this
  }

  setJobs(repo: RepoSlug, runId: number, jobs: RunJob[]): this {
    this.jobs.set(`${repo}#${runId}`, jobs)
    return this
  }

  /** The merge commit sha the next merge of this PR returns. */
  setMergeCommit(repo: RepoSlug, number: number, sha: string): this {
    this.mergeShas.set(`${repo}#${number}`, sha)
    return this
  }

  addRun(repo: RepoSlug, run: WorkflowRun, script: RunScript = {}): this {
    this.runs.push({ run: { ...run }, repo, script, pending: [...(script.stages ?? [])] })
    return this
  }

  /** The current state of a scripted run, without advancing it. */
  runState(repo: RepoSlug, runId: number): WorkflowRun {
    return this.requireRun(repo, runId).run
  }

  writesOf(method: WriteCall['method']): WriteCall[] {
    return this.writes.filter((w) => w.method === method)
  }

  // ---- GitHubPort ----

  async findOpenPr(repo: RepoSlug, head: string, base: string): Promise<PullRequest | null> {
    for (const [key, pr] of this.prs) {
      if (
        key.startsWith(`${repo}#`) &&
        pr.state === 'open' &&
        pr.headRef === head &&
        pr.baseRef === base
      ) {
        return pr
      }
    }
    return null
  }

  async getPr(repo: RepoSlug, number: number): Promise<PullRequest> {
    return this.requirePr(repo, number)
  }

  async listPrFiles(repo: RepoSlug, number: number): Promise<PrFile[]> {
    this.requirePr(repo, number)
    return this.files.get(`${repo}#${number}`) ?? []
  }

  async listDir(repo: RepoSlug, ref: string, path: string): Promise<string[]> {
    return this.dirs.get(`${repo}@${ref}:${path}`) ?? []
  }

  async getBranchSha(repo: RepoSlug, branch: string): Promise<string> {
    const sha = this.branches.get(`${repo}@${branch}`)
    if (!sha) throw new Error(`FakeGitHub: no branch ${repo}@${branch}`)
    return sha
  }

  async listWorkflowRuns(
    repo: RepoSlug,
    workflowFile: string,
    filter: { headSha?: string; branch?: string; event?: string },
  ): Promise<WorkflowRun[]> {
    const matched = this.runs.filter(
      (s) =>
        s.repo === repo &&
        s.run.workflowFile === workflowFile &&
        (filter.headSha === undefined || s.run.headSha === filter.headSha) &&
        (filter.branch === undefined || s.run.headBranch === filter.branch) &&
        (filter.event === undefined || s.run.event === filter.event),
    )
    const snapshot = matched.map((s) => ({ ...s.run }))
    for (const s of matched) {
      const next = s.pending.shift()
      if (next) s.run = { ...s.run, ...next }
    }
    return snapshot
  }

  async listRunJobs(repo: RepoSlug, runId: number): Promise<RunJob[]> {
    return this.jobs.get(`${repo}#${runId}`) ?? []
  }

  async approveRun(repo: RepoSlug, runId: number): Promise<void> {
    const s = this.requireRun(repo, runId)
    this.writes.push({ method: 'approveRun', repo, args: { runId } })
    s.run = { ...s.run, status: 'queued', conclusion: null }
    if (s.script.onApprove) s.pending = [...s.script.onApprove]
  }

  async rerunFailedJobs(repo: RepoSlug, runId: number): Promise<void> {
    const s = this.requireRun(repo, runId)
    this.writes.push({ method: 'rerunFailedJobs', repo, args: { runId } })
    s.run = { ...s.run, status: 'queued', conclusion: null, runAttempt: s.run.runAttempt + 1 }
    if (s.script.onRerun) s.pending = [...s.script.onRerun]
  }

  async mergePr(
    repo: RepoSlug,
    number: number,
    opts: { method: 'squash' | 'merge' | 'rebase'; admin: boolean; expectedHeadSha: string },
  ): Promise<{ mergeCommitSha: string }> {
    const pr = this.requirePr(repo, number)
    this.writes.push({ method: 'mergePr', repo, args: { number, ...opts } })
    if (pr.state !== 'open') throw new Error(`FakeGitHub: PR #${number} is not open`)
    if (pr.headSha !== opts.expectedHeadSha) {
      throw new Error(
        `FakeGitHub: head sha mismatch on PR #${number} (expected ${opts.expectedHeadSha}, is ${pr.headSha})`,
      )
    }
    const mergeCommitSha = this.mergeShas.get(`${repo}#${number}`) ?? `merge-commit-${number}`
    const merged = { ...pr, state: 'closed' as const, merged: true, mergeCommitSha }
    this.prs.set(`${repo}#${number}`, merged)
    this.branches.set(`${repo}@${pr.baseRef}`, mergeCommitSha)
    this.onMerge?.(merged, mergeCommitSha)
    return { mergeCommitSha }
  }

  private requirePr(repo: RepoSlug, number: number): PullRequest {
    const pr = this.prs.get(`${repo}#${number}`)
    if (!pr) throw new Error(`FakeGitHub: no PR ${repo}#${number}`)
    return pr
  }

  private requireRun(repo: RepoSlug, runId: number): ScriptedRun {
    const s = this.runs.find((r) => r.repo === repo && r.run.id === runId)
    if (!s) throw new Error(`FakeGitHub: no run ${repo}#${runId}`)
    return s
  }
}
