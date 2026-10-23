import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { ConfigError, loadRepoConfig, parseRepoConfig } from './config.js'

const valid = {
  repo: 'cogitave/namzu',
  defaultBranch: 'main',
  adapter: 'pnpm-changesets',
  ciWorkflow: 'ci.yml',
  releaseWorkflow: 'release.yml',
  releaseBranch: 'changeset-release/main',
  approveBotRuns: false,
  approveBranches: ['changeset-release/main'],
  adminMerge: true,
  mergeMethod: 'squash',
  maxRerunsPerRun: 1,
  versionPrWaitMinutes: 45,
  registryLagMinutes: 10,
  pollSeconds: 30,
}

describe('parseRepoConfig', () => {
  it('accepts the design example', () => {
    expect(parseRepoConfig(valid)).toEqual(valid)
  })

  it.each([
    ['repo', 'nope', /owner\/name/],
    ['mergeMethod', 'fast-forward', /mergeMethod/],
    ['pollSeconds', 0, /pollSeconds/],
    ['maxRerunsPerRun', 1.5, /maxRerunsPerRun/],
    ['adminMerge', 'yes', /adminMerge/],
    ['approveBranches', [1], /approveBranches/],
    ['ciWorkflow', '', /ciWorkflow/],
  ])('rejects bad %s', (key, value, message) => {
    expect(() => parseRepoConfig({ ...valid, [key]: value })).toThrow(message)
  })

  it('rejects a missing key and names it', () => {
    const { releaseBranch: _omit, ...rest } = valid
    expect(() => parseRepoConfig(rest)).toThrow(/releaseBranch/)
  })

  it('refuses approving bot runs with an empty allow-list', () => {
    expect(() => parseRepoConfig({ ...valid, approveBotRuns: true, approveBranches: [] })).toThrow(
      /approveBranches/,
    )
  })

  it('rejects non-objects', () => {
    expect(() => parseRepoConfig([])).toThrow(ConfigError)
    expect(() => parseRepoConfig(null)).toThrow(ConfigError)
  })
})

describe('loadRepoConfig', () => {
  const dir = mkdtempSync(join(tmpdir(), 'ground-config-'))

  it('loads a file', () => {
    const p = join(dir, 'ok.json')
    writeFileSync(p, JSON.stringify(valid))
    expect(loadRepoConfig(p).repo).toBe('cogitave/namzu')
  })

  it('names the file on invalid JSON and on a missing file', () => {
    const p = join(dir, 'bad.json')
    writeFileSync(p, '{')
    expect(() => loadRepoConfig(p)).toThrow(/bad\.json: invalid JSON/)
    expect(() => loadRepoConfig(join(dir, 'missing.json'))).toThrow(/cannot read file/)
  })
})
