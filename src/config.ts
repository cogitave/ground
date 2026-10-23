import { readFileSync } from 'node:fs'
import type { RepoSlug } from './ports.js'

export interface RepoConfig {
  repo: RepoSlug
  defaultBranch: string
  adapter: string
  ciWorkflow: string
  releaseWorkflow: string
  releaseBranch: string
  approveBotRuns: boolean
  approveBranches: string[]
  adminMerge: boolean
  mergeMethod: 'squash' | 'merge' | 'rebase'
  maxRerunsPerRun: number
  versionPrWaitMinutes: number
  registryLagMinutes: number
  pollSeconds: number
}

export class ConfigError extends Error {
  override name = 'ConfigError'
}

const MERGE_METHODS = ['squash', 'merge', 'rebase'] as const

function fail(source: string, msg: string): never {
  throw new ConfigError(`${source}: ${msg}`)
}

function str(o: Record<string, unknown>, key: string, source: string): string {
  const v = o[key]
  if (typeof v !== 'string' || v.length === 0) fail(source, `"${key}" must be a non-empty string`)
  return v
}

function bool(o: Record<string, unknown>, key: string, source: string): boolean {
  const v = o[key]
  if (typeof v !== 'boolean') fail(source, `"${key}" must be a boolean`)
  return v
}

function posInt(o: Record<string, unknown>, key: string, source: string, min: number): number {
  const v = o[key]
  if (typeof v !== 'number' || !Number.isInteger(v) || v < min) {
    fail(source, `"${key}" must be an integer >= ${min}`)
  }
  return v
}

export function parseRepoConfig(raw: unknown, source = 'config'): RepoConfig {
  if (typeof raw !== 'object' || raw === null || Array.isArray(raw)) {
    fail(source, 'must be a JSON object')
  }
  const o = raw as Record<string, unknown>
  const repo = str(o, 'repo', source)
  if (!/^[^/\s]+\/[^/\s]+$/.test(repo))
    fail(source, `"repo" must look like owner/name, got "${repo}"`)

  const branches = o.approveBranches
  if (!Array.isArray(branches) || !branches.every((b) => typeof b === 'string' && b.length > 0)) {
    fail(source, '"approveBranches" must be an array of non-empty strings')
  }
  const method = o.mergeMethod
  if (!MERGE_METHODS.includes(method as (typeof MERGE_METHODS)[number])) {
    fail(source, `"mergeMethod" must be one of ${MERGE_METHODS.join(', ')}`)
  }
  const approveBotRuns = bool(o, 'approveBotRuns', source)
  if (approveBotRuns && branches.length === 0) {
    fail(source, '"approveBotRuns" needs at least one entry in "approveBranches"')
  }

  return {
    repo: repo as RepoSlug,
    defaultBranch: str(o, 'defaultBranch', source),
    adapter: str(o, 'adapter', source),
    ciWorkflow: str(o, 'ciWorkflow', source),
    releaseWorkflow: str(o, 'releaseWorkflow', source),
    releaseBranch: str(o, 'releaseBranch', source),
    approveBotRuns,
    approveBranches: branches as string[],
    adminMerge: bool(o, 'adminMerge', source),
    mergeMethod: method as RepoConfig['mergeMethod'],
    maxRerunsPerRun: posInt(o, 'maxRerunsPerRun', source, 0),
    versionPrWaitMinutes: posInt(o, 'versionPrWaitMinutes', source, 0),
    registryLagMinutes: posInt(o, 'registryLagMinutes', source, 0),
    pollSeconds: posInt(o, 'pollSeconds', source, 1),
  }
}

export function loadRepoConfig(path: string): RepoConfig {
  let text: string
  try {
    text = readFileSync(path, 'utf8')
  } catch (err) {
    fail(path, `cannot read file (${(err as Error).message})`)
  }
  let json: unknown
  try {
    json = JSON.parse(text)
  } catch (err) {
    fail(path, `invalid JSON (${(err as Error).message})`)
  }
  return parseRepoConfig(json, path)
}
