import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { Octokit } from '@octokit/rest'
import type { GitHubPort, PrFile, PullRequest, RepoSlug, RunJob, WorkflowRun } from '../ports.js'

const execFileAsync = promisify(execFile)

export type ExecFn = (cmd: string, args: string[]) => Promise<{ stdout: string }>

const defaultExec: ExecFn = (cmd, args) => execFileAsync(cmd, args)

export async function tokenFromGh(exec: ExecFn = defaultExec): Promise<string> {
  const { stdout } = await exec('gh', ['auth', 'token'])
  const token = stdout.trim()
  if (!token) throw new Error('`gh auth token` returned nothing; run `gh auth login`')
  return token
}

export function splitRepo(repo: RepoSlug): { owner: string; repo: string } {
  const [owner, name] = repo.split('/') as [string, string]
  return { owner, repo: name }
}

// ---- pure mapping from the REST shapes (only the fields read) ----

export interface RawPr {
  number: number
  title: string
  state: string
  merged?: boolean
  merged_at?: string | null
  head: { ref: string; sha: string; repo: { full_name: string } | null }
  base: { ref: string; sha: string }
  user: { login: string } | null
  mergeable?: boolean | null
}

export function mapPr(raw: RawPr): PullRequest {
  const merged = raw.merged ?? raw.merged_at != null
  return {
    number: raw.number,
    title: raw.title,
    state: raw.state === 'open' ? 'open' : 'closed',
    merged,
    headRef: raw.head.ref,
    headSha: raw.head.sha,
    // A deleted fork has no repo; an empty-owner slug can never match the base repo.
    headRepo: (raw.head.repo?.full_name ?? '/') as RepoSlug,
    baseRef: raw.base.ref,
    baseSha: raw.base.sha,
    author: raw.user?.login ?? '',
    mergeable: raw.mergeable ?? null,
  }
}

export interface RawFile {
  filename: string
  status: string
  patch?: string
}

export function mapFile(raw: RawFile): PrFile {
  // GitHub also reports copied/changed/unchanged; all of them are edits to a path.
  const status: PrFile['status'] =
    raw.status === 'added' || raw.status === 'removed' || raw.status === 'renamed'
      ? raw.status
      : 'modified'
  return raw.patch === undefined
    ? { path: raw.filename, status }
    : { path: raw.filename, status, patch: raw.patch }
}

export interface RawRun {
  id: number
  path: string
  event: string
  head_sha: string
  head_branch: string | null
  status: string | null
  conclusion: string | null
  run_attempt?: number
  created_at: string
  actor?: { login: string } | null
  triggering_actor?: { login: string } | null
  html_url: string
}

export function mapRun(raw: RawRun): WorkflowRun {
  const actor = raw.actor?.login ?? ''
  return {
    id: raw.id,
    // `path` is ".github/workflows/ci.yml"; callers address workflows by file name.
    workflowFile: raw.path.split('/').pop() ?? raw.path,
    event: raw.event,
    headSha: raw.head_sha,
    headBranch: raw.head_branch ?? '',
    status: raw.status ?? '',
    conclusion: raw.conclusion,
    runAttempt: raw.run_attempt ?? 1,
    createdAt: raw.created_at,
    actor,
    triggeringActor: raw.triggering_actor?.login ?? actor,
    htmlUrl: raw.html_url,
  }
}

export function mapJob(raw: { name: string; status: string; conclusion: string | null }): RunJob {
  return { name: raw.name, status: raw.status, conclusion: raw.conclusion }
}

export function ghMergeArgs(
  repo: RepoSlug,
  number: number,
  opts: { method: 'squash' | 'merge' | 'rebase'; expectedHeadSha: string },
): string[] {
  return [
    'pr',
    'merge',
    String(number),
    '--repo',
    repo,
    `--${opts.method}`,
    '--admin',
    '--match-head-commit',
    opts.expectedHeadSha,
  ]
}

export interface OctokitGitHubOptions {
  token?: () => Promise<string>
  exec?: ExecFn
  octokit?: Octokit
}

export class OctokitGitHub implements GitHubPort {
  private octokitPromise: Promise<Octokit> | undefined
  private readonly exec: ExecFn
  private readonly getToken: () => Promise<string>
  private readonly injected: Octokit | undefined

  constructor(opts: OctokitGitHubOptions = {}) {
    this.exec = opts.exec ?? defaultExec
    this.getToken = opts.token ?? (() => tokenFromGh(this.exec))
    this.injected = opts.octokit
  }

  private client(): Promise<Octokit> {
    this.octokitPromise ??= this.injected
      ? Promise.resolve(this.injected)
      : this.getToken().then((auth) => new Octokit({ auth }))
    return this.octokitPromise
  }

  async findOpenPr(repo: RepoSlug, head: string, base: string): Promise<PullRequest | null> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const { data } = await ok.rest.pulls.list({
      owner,
      repo: name,
      state: 'open',
      head: `${owner}:${head}`,
      base,
      per_page: 1,
    })
    const first = data[0]
    // The list endpoint omits `mergeable`, so fetch the PR itself.
    return first ? this.getPr(repo, first.number) : null
  }

  async getPr(repo: RepoSlug, number: number): Promise<PullRequest> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const { data } = await ok.rest.pulls.get({ owner, repo: name, pull_number: number })
    return mapPr(data)
  }

  async listPrFiles(repo: RepoSlug, number: number): Promise<PrFile[]> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const files = await ok.paginate(ok.rest.pulls.listFiles, {
      owner,
      repo: name,
      pull_number: number,
      per_page: 100,
    })
    return files.map(mapFile)
  }

  async listDir(repo: RepoSlug, ref: string, path: string): Promise<string[]> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    try {
      const { data } = await ok.rest.repos.getContent({ owner, repo: name, path, ref })
      return Array.isArray(data) ? data.map((e) => e.name) : []
    } catch (err) {
      if ((err as { status?: number }).status === 404) return []
      throw err
    }
  }

  async getBranchSha(repo: RepoSlug, branch: string): Promise<string> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const { data } = await ok.rest.repos.getBranch({ owner, repo: name, branch })
    return data.commit.sha
  }

  async listWorkflowRuns(
    repo: RepoSlug,
    workflowFile: string,
    filter: { headSha?: string; branch?: string; event?: string },
  ): Promise<WorkflowRun[]> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const { data } = await ok.rest.actions.listWorkflowRuns({
      owner,
      repo: name,
      workflow_id: workflowFile,
      per_page: 30,
      ...(filter.headSha && { head_sha: filter.headSha }),
      ...(filter.branch && { branch: filter.branch }),
      ...(filter.event && { event: filter.event }),
    })
    return data.workflow_runs.map((r) => mapRun(r as RawRun))
  }

  async listRunJobs(repo: RepoSlug, runId: number): Promise<RunJob[]> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    const jobs = await ok.paginate(ok.rest.actions.listJobsForWorkflowRun, {
      owner,
      repo: name,
      run_id: runId,
      per_page: 100,
    })
    return jobs.map(mapJob)
  }

  async approveRun(repo: RepoSlug, runId: number): Promise<void> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    await ok.rest.actions.approveWorkflowRun({ owner, repo: name, run_id: runId })
  }

  async rerunFailedJobs(repo: RepoSlug, runId: number): Promise<void> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    await ok.rest.actions.reRunWorkflowFailedJobs({ owner, repo: name, run_id: runId })
  }

  async mergePr(
    repo: RepoSlug,
    number: number,
    opts: { method: 'squash' | 'merge' | 'rebase'; admin: boolean; expectedHeadSha: string },
  ): Promise<{ mergeCommitSha: string }> {
    const { owner, repo: name } = splitRepo(repo)
    const ok = await this.client()
    if (opts.admin) {
      // Ruleset bypass is proven to work through gh and not through the REST merge call.
      await this.exec('gh', ghMergeArgs(repo, number, opts))
      const { data } = await ok.rest.pulls.get({ owner, repo: name, pull_number: number })
      if (!data.merge_commit_sha)
        throw new Error(`PR #${number} merged but has no merge commit sha`)
      return { mergeCommitSha: data.merge_commit_sha }
    }
    const { data } = await ok.rest.pulls.merge({
      owner,
      repo: name,
      pull_number: number,
      merge_method: opts.method,
      sha: opts.expectedHeadSha,
    })
    return { mergeCommitSha: data.sha }
  }
}
