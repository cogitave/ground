export interface Clock {
  now(): Date
  sleep(ms: number): Promise<void>
}

export interface Logger {
  debug(msg: string, data?: Record<string, unknown>): void
  info(msg: string, data?: Record<string, unknown>): void
  warn(msg: string, data?: Record<string, unknown>): void
  error(msg: string, data?: Record<string, unknown>): void
}

export type RepoSlug = `${string}/${string}`

export interface PullRequest {
  number: number
  title: string
  state: 'open' | 'closed'
  merged: boolean
  headRef: string
  headSha: string
  headRepo: RepoSlug
  baseRef: string
  baseSha: string
  author: string
  mergeable: boolean | null
}

export interface PrFile {
  path: string
  status: 'added' | 'removed' | 'modified' | 'renamed'
  patch?: string
}

export interface WorkflowRun {
  id: number
  workflowFile: string
  event: string
  headSha: string
  headBranch: string
  status: string
  conclusion: string | null
  runAttempt: number
  createdAt: string
  actor: string
  triggeringActor: string
  htmlUrl: string
}

export interface RunJob {
  name: string
  status: string
  conclusion: string | null
}

export interface GitHubPort {
  findOpenPr(repo: RepoSlug, head: string, base: string): Promise<PullRequest | null>
  getPr(repo: RepoSlug, number: number): Promise<PullRequest>
  listPrFiles(repo: RepoSlug, number: number): Promise<PrFile[]>
  listDir(repo: RepoSlug, ref: string, path: string): Promise<string[]>
  getBranchSha(repo: RepoSlug, branch: string): Promise<string>
  listWorkflowRuns(
    repo: RepoSlug,
    workflowFile: string,
    filter: { headSha?: string; branch?: string; event?: string },
  ): Promise<WorkflowRun[]>
  listRunJobs(repo: RepoSlug, runId: number): Promise<RunJob[]>
  approveRun(repo: RepoSlug, runId: number): Promise<void>
  rerunFailedJobs(repo: RepoSlug, runId: number): Promise<void>
  mergePr(
    repo: RepoSlug,
    number: number,
    opts: { method: 'squash' | 'merge' | 'rebase'; admin: boolean; expectedHeadSha: string },
  ): Promise<{ mergeCommitSha: string }>
}

export interface RegistryPort {
  latestVersion(pkg: string): Promise<string | null>
}
