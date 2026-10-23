import { describe, expect, it } from 'vitest'
import type { RepoConfig } from '../config.js'
import type { RepoSlug } from '../ports.js'
import { StateStore } from '../state/index.js'
import {
  FakeClock,
  FakeGitHub,
  FakeRegistry,
  MemoryLogger,
  makePr,
  makeRun,
} from '../testing/index.js'
import { runShipCommand } from './main.js'

const REPO: RepoSlug = 'cogitave/namzu'
const CONFIG: RepoConfig = {
  repo: REPO,
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
const PATCH = [
  '@@ -1,5 +1,5 @@',
  ' {',
  '   "name": "@namzu/sdk",',
  '-  "version": "1.0.0",',
  '+  "version": "1.1.0",',
  '   "type": "module",',
].join('\n')

function deps() {
  const clock = new FakeClock()
  const github = new FakeGitHub()
  github
    .setPr(REPO, makePr())
    .setPrFiles(REPO, 1, [
      { path: '.changeset/a.md', status: 'removed' },
      { path: 'packages/sdk/package.json', status: 'modified', patch: PATCH },
    ])
    .setDir(REPO, 'main', '.changeset', ['a.md', 'README.md', 'config.json'])
    .setBranchSha(REPO, 'main', 'base-sha-1')
  github.addRun(REPO, makeRun(), { stages: [{ status: 'completed', conclusion: 'success' }] })
  const registry = new FakeRegistry().setVersion('@namzu/sdk', '1.0.0')
  github.onMerge = (_pr, sha) => {
    github.addRun(
      REPO,
      makeRun({
        id: 200,
        workflowFile: 'release.yml',
        event: 'push',
        headSha: sha,
        headBranch: 'main',
      }),
      { stages: [{ status: 'completed', conclusion: 'success' }] },
    )
    registry.setVersion('@namzu/sdk', '1.1.0')
  }
  return {
    github,
    d: {
      config: CONFIG,
      github,
      registry,
      clock,
      logger: new MemoryLogger(),
      store: new StateStore(clock, ':memory:'),
    },
  }
}

describe('ground ship command', () => {
  it('prints the plan and exits 0 on a dry run, writing nothing', async () => {
    const { d, github } = deps()
    const lines: string[] = []
    const code = await runShipCommand(d, { confirm: false, json: false }, (s) => lines.push(s))
    expect(code).toBe(0)
    expect(lines.join('\n')).toContain('@namzu/sdk@1.1.0')
    expect(github.writes).toEqual([])
  })

  it('ships with --confirm and exits 0', async () => {
    const { d, github } = deps()
    const lines: string[] = []
    const code = await runShipCommand(d, { confirm: true, json: false }, (s) => lines.push(s))
    expect(code).toBe(0)
    expect(github.writesOf('mergePr')).toHaveLength(1)
  })

  it('prints the outcome as JSON with --json', async () => {
    const { d } = deps()
    const lines: string[] = []
    await runShipCommand(d, { confirm: false, json: true }, (s) => lines.push(s))
    expect(JSON.parse(lines[0] ?? '')).toMatchObject({ status: 'planned' })
  })

  it('exits non-zero when CI fails for good', async () => {
    const { d } = deps()
    const github = new FakeGitHub()
    github
      .setPr(REPO, makePr())
      .setPrFiles(REPO, 1, [{ path: '.changeset/a.md', status: 'removed' }])
      .setDir(REPO, 'main', '.changeset', ['a.md'])
      .setBranchSha(REPO, 'main', 'base-sha-1')
    github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'failure' }))
    d.github = github
    d.config = { ...CONFIG, maxRerunsPerRun: 0 }
    const code = await runShipCommand(d, { confirm: true, json: false }, () => {})
    expect(code).toBe(1)
  })
})
