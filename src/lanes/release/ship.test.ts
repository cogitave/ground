import { describe, expect, it } from 'vitest'
import type { RepoConfig } from '../../config.js'
import type { RepoSlug, WorkflowRun } from '../../ports.js'
import { StateStore } from '../../state/index.js'
import {
  FakeClock,
  FakeGitHub,
  FakeRegistry,
  MemoryLogger,
  makePr,
  makeRun,
} from '../../testing/index.js'
import { ship } from './ship.js'
import type { ShipOptions } from './types.js'

const REPO: RepoSlug = 'cogitave/namzu'
const SDK_PATCH = [
  '@@ -1,5 +1,5 @@',
  ' {',
  '   "name": "@namzu/sdk",',
  '-  "version": "1.0.0",',
  '+  "version": "1.1.0",',
  '   "type": "module",',
].join('\n')

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

/** Virtual clock that lets a test act between two polls or break the run off. */
class HookClock extends FakeClock {
  count = 0
  hook?: (n: number) => void
  override async sleep(ms: number): Promise<void> {
    await super.sleep(ms)
    this.count += 1
    this.hook?.(this.count)
  }
}

/** Moves the PR head right after CI reports green, i.e. between the gate and the merge. */
class MovingGitHub extends FakeGitHub {
  moveTo?: string
  override async listWorkflowRuns(...args: Parameters<FakeGitHub['listWorkflowRuns']>) {
    const runs = await super.listWorkflowRuns(...args)
    if (this.moveTo && runs.some((r) => r.conclusion === 'success')) {
      this.updatePr(REPO, 1, { headSha: this.moveTo })
    }
    return runs
  }
}

/** After a re-run request, lists the old attempt once more, as GitHub briefly does. */
class LaggingGitHub extends FakeGitHub {
  private stale: WorkflowRun[] | null = null
  private pending = false
  override async rerunFailedJobs(...args: Parameters<FakeGitHub['rerunFailedJobs']>) {
    this.stale = await super.listWorkflowRuns(args[0], 'ci.yml', {})
    await super.rerunFailedJobs(...args)
    this.pending = true
  }
  override async listWorkflowRuns(...args: Parameters<FakeGitHub['listWorkflowRuns']>) {
    if (this.pending && this.stale && args[1] === 'ci.yml') {
      this.pending = false
      return this.stale
    }
    return super.listWorkflowRuns(...args)
  }
}

const green = (n = 1): Partial<WorkflowRun>[] => [
  ...Array(n).fill({ status: 'in_progress' }),
  { status: 'completed', conclusion: 'success' },
]

function world(
  opts: {
    config?: Partial<RepoConfig>
    github?: FakeGitHub
    prHead?: string
    pr?: Parameters<typeof makePr>[0]
  } = {},
) {
  const config = { ...CONFIG, ...opts.config }
  const github = opts.github ?? new FakeGitHub()
  const clock = new HookClock()
  const registry = new FakeRegistry().setVersion('@namzu/sdk', '1.0.0')
  const logger = new MemoryLogger()
  const store = new StateStore(clock, ':memory:')
  github
    .setPr(REPO, makePr(opts.pr))
    .setPrFiles(REPO, 1, [
      { path: '.changeset/a.md', status: 'removed' },
      { path: '.changeset/b.md', status: 'removed' },
      { path: 'packages/sdk/package.json', status: 'modified', patch: SDK_PATCH },
    ])
    .setDir(REPO, 'main', '.changeset', ['a.md', 'b.md', 'README.md', 'config.json'])
    .setBranchSha(REPO, 'main', 'base-sha-1')
    .setJobs(REPO, 100, [
      { name: 'lint', status: 'completed', conclusion: 'success' },
      { name: 'test', status: 'completed', conclusion: 'failure' },
    ])
  // The release run appears when the merge lands, like GitHub's push trigger.
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
      { stages: green() },
    )
    registry.setVersion('@namzu/sdk', '1.1.0')
  }
  const go = (o: Partial<ShipOptions> = {}, overrides: { clock?: HookClock } = {}) =>
    ship(
      { config, github, registry, clock: overrides.clock ?? clock, logger, store },
      { confirm: true, ...o },
    )
  return { config, github, clock, registry, logger, store, go }
}

describe('ship: happy path', () => {
  it('dry-run stops before the first write and leaves the session resumable', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    const out = await w.go({ confirm: false })
    expect(out.status).toBe('planned')
    expect(out.plan?.action).toBe('merge')
    expect(out.plan?.description).toContain('@namzu/sdk@1.1.0')
    expect(w.github.writes).toEqual([])
    expect(w.store.getActiveShipSession(REPO)).not.toBeNull()
  })

  it('with confirm merges once at the checked head and confirms the registry', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(out.packages).toEqual([{ name: '@namzu/sdk', from: '1.0.0', to: '1.1.0' }])
    expect(w.github.writesOf('mergePr')).toHaveLength(1)
    expect(w.github.writesOf('mergePr')[0]?.args).toMatchObject({
      method: 'squash',
      admin: true,
      expectedHeadSha: 'head-sha-1',
    })
    expect(out.baseWasTip).toBe(true)
    expect(out.summary).toContain('@namzu/sdk@1.1.0')
    expect(w.store.getActiveShipSession(REPO)).toBeNull()
    expect(w.store.getShipSession(out.sessionId)?.outcome).toBe('shipped')
  })

  it('reports that the base moved when it is no longer the tip', async () => {
    const w = world()
    w.github.setBranchSha(REPO, 'main', 'someone-else')
    w.github.addRun(REPO, makeRun(), { stages: green() })
    const out = await w.go()
    expect(out.baseWasTip).toBe(false)
    expect(out.summary).toContain('re-validated inline')
  })
})

describe('ship: finding the version PR', () => {
  it('reports nothing to ship without pending changesets', async () => {
    const w = world()
    w.github.updatePr(REPO, 1, { state: 'closed' })
    w.github.setDir(REPO, 'main', '.changeset', ['README.md', 'config.json'])
    const out = await w.go()
    expect(out.status).toBe('nothing_to_ship')
    expect(w.clock.sleeps).toEqual([])
  })

  it('reports nothing to ship when changesets are pending but no release run is going', async () => {
    const w = world()
    w.github.updatePr(REPO, 1, { state: 'closed' })
    const out = await w.go()
    expect(out.status).toBe('nothing_to_ship')
    expect(out.summary).toContain('no release run')
  })

  it('waits for the running release to open the PR, then ships', async () => {
    const w = world()
    w.github.updatePr(REPO, 1, { state: 'closed' })
    w.github.addRun(
      REPO,
      makeRun({
        id: 50,
        workflowFile: 'release.yml',
        event: 'push',
        headBranch: 'main',
        headSha: 'x',
      }),
      {
        stages: [
          { status: 'in_progress' },
          { status: 'in_progress' },
          { status: 'completed', conclusion: 'success' },
        ],
      },
    )
    w.github.addRun(REPO, makeRun(), { stages: green() })
    w.clock.hook = (n) => {
      if (n === 3) w.github.updatePr(REPO, 1, { state: 'open' })
    }
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.clock.count).toBeGreaterThanOrEqual(3)
  })
})

describe('ship: snapshot check', () => {
  it('waits for a stale PR to be refreshed, then gates the new head', async () => {
    const w = world()
    w.github.setDir(REPO, 'main', '.changeset', ['a.md', 'b.md', 'c.md', 'README.md'])
    w.github.addRun(REPO, makeRun({ id: 101, headSha: 'head-sha-2' }), { stages: green() })
    w.clock.hook = (n) => {
      if (n === 2) {
        w.github.updatePr(REPO, 1, { headSha: 'head-sha-2' })
        w.github.setPrFiles(REPO, 1, [
          { path: '.changeset/a.md', status: 'removed' },
          { path: '.changeset/b.md', status: 'removed' },
          { path: '.changeset/c.md', status: 'removed' },
          { path: 'packages/sdk/package.json', status: 'modified', patch: SDK_PATCH },
        ])
      }
    }
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.github.writesOf('mergePr')[0]?.args.expectedHeadSha).toBe('head-sha-2')
  })

  it('times out, resumably, when the PR is never refreshed', async () => {
    const w = world({ config: { versionPrWaitMinutes: 2 } })
    w.github.setDir(REPO, 'main', '.changeset', ['a.md', 'b.md', 'c.md'])
    const out = await w.go()
    expect(out.status).toBe('timed_out')
    expect(out.summary).toContain('c.md')
    expect(w.github.writes).toEqual([])
    expect(w.store.getActiveShipSession(REPO)).not.toBeNull()
  })
})

describe('ship: approving CI runs', () => {
  const actionRequired = {
    stages: [{ status: 'completed', conclusion: 'action_required' }],
    onApprove: [{ status: 'in_progress' }, { status: 'completed', conclusion: 'success' }],
  } as const

  it('approves the current head when enabled', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }), {
      onApprove: [...actionRequired.onApprove],
    })
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.github.writesOf('approveRun')).toHaveLength(1)
  })

  it('plans the approval in a dry run without writing', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }))
    const out = await w.go({ confirm: false })
    expect(out.status).toBe('planned')
    expect(out.plan).toMatchObject({ action: 'approve', runId: 100 })
    expect(w.github.writes).toEqual([])
  })

  it('does not approve when disabled and reports it as blocked', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }))
    const out = await w.go({ ciWaitMinutes: 2 })
    expect(out.status).toBe('blocked')
    expect(out.summary).toContain('approveBotRuns')
    expect(w.github.writes).toEqual([])
  })

  it('ignores an action_required run for an older head', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(
      REPO,
      makeRun({ id: 90, headSha: 'old-sha', status: 'completed', conclusion: 'action_required' }),
    )
    w.github.addRun(REPO, makeRun({ id: 100 }), { stages: green() })
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.github.writesOf('approveRun')).toEqual([])
  })

  it('does nothing while only an older head has a run', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(
      REPO,
      makeRun({ id: 90, headSha: 'old-sha', status: 'completed', conclusion: 'action_required' }),
    )
    const out = await w.go({ ciWaitMinutes: 2 })
    expect(out.status).toBe('timed_out')
    expect(w.github.writes).toEqual([])
  })

  it('never approves a fork head, even with approval enabled', async () => {
    const w = world({ config: { approveBotRuns: true }, pr: { headRepo: 'someone/namzu' } })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }))
    const out = await w.go({ ciWaitMinutes: 2 })
    expect(out.status).toBe('blocked')
    expect(w.github.writes).toEqual([])
  })

  it('never approves a run whose branch differs from the PR head branch', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(
      REPO,
      makeRun({ status: 'completed', conclusion: 'action_required', headBranch: 'other' }),
    )
    const out = await w.go({ ciWaitMinutes: 2 })
    expect(out.status).toBe('blocked')
    expect(w.github.writes).toEqual([])
  })

  it('never approves a branch outside the allow-list', async () => {
    const w = world({ config: { approveBotRuns: true, approveBranches: ['other/branch'] } })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }))
    const out = await w.go({ ciWaitMinutes: 2 })
    expect(out.status).toBe('blocked')
    expect(w.github.writes).toEqual([])
  })
})

describe('ship: failures and re-runs', () => {
  it('does not re-run twice while GitHub still lists the old attempt', async () => {
    const w = world({ github: new LaggingGitHub() })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'failure' }), {
      onRerun: green(),
    })
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.github.writesOf('rerunFailedJobs')).toHaveLength(1)
  })

  it('re-runs once, records the flake, then stops with the failing jobs', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'failure' }), {
      onRerun: [{ status: 'completed', conclusion: 'failure' }],
    })
    const out = await w.go()
    expect(out.status).toBe('failed')
    expect(out.failedJobs).toEqual(['test'])
    expect(w.github.writesOf('rerunFailedJobs')).toHaveLength(1)
    expect(w.github.writesOf('mergePr')).toEqual([])
    expect(w.store.listFlakes(REPO)).toMatchObject([{ runId: 100, job: 'test', attempt: 1 }])
  })

  it('ships when the re-run passes, and says so', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'failure' }), {
      onRerun: green(),
    })
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(out.reruns).toBe(1)
    expect(w.store.listFlakes(REPO)).toHaveLength(1)
  })

  it('plans the re-run in a dry run without writing', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'failure' }))
    const out = await w.go({ confirm: false })
    expect(out.plan).toMatchObject({ action: 'rerun', runId: 100 })
    expect(w.github.writes).toEqual([])
    expect(w.store.listFlakes(REPO)).toEqual([])
  })

  it('re-runs a failed release once, then reports the failure', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    w.github.onMerge = (_pr, sha) => {
      w.github.addRun(
        REPO,
        makeRun({
          id: 200,
          workflowFile: 'release.yml',
          event: 'push',
          headSha: sha,
          headBranch: 'main',
          status: 'completed',
          conclusion: 'failure',
        }),
        {
          onRerun: [{ status: 'completed', conclusion: 'failure' }],
        },
      )
    }
    const out = await w.go()
    expect(out.status).toBe('failed')
    expect(out.mergeCommitSha).toBe('merge-commit-1')
    expect(w.github.writesOf('rerunFailedJobs')).toHaveLength(1)
  })
})

describe('ship: head check before merge', () => {
  it('aborts without merging when the head moved after CI passed', async () => {
    const gh = new MovingGitHub()
    gh.moveTo = 'head-sha-2'
    const w = world({ github: gh })
    gh.addRun(REPO, makeRun(), { stages: [{ status: 'completed', conclusion: 'success' }] })
    const out = await w.go()
    expect(out.status).toBe('aborted')
    expect(out.summary).toContain('moved')
    expect(gh.writes).toEqual([])
  })
})

describe('ship: registry', () => {
  it('tolerates propagation lag', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    w.github.onMerge = (_pr, sha) => {
      w.github.addRun(
        REPO,
        makeRun({
          id: 200,
          workflowFile: 'release.yml',
          event: 'push',
          headSha: sha,
          headBranch: 'main',
          status: 'completed',
          conclusion: 'success',
        }),
      )
      w.registry.publishAfterQueries('@namzu/sdk', '1.1.0', 4)
    }
    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(w.registry.queries.length).toBe(5)
  })

  it('reports, without throwing, a version the registry never shows', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    w.github.onMerge = (_pr, sha) => {
      w.github.addRun(
        REPO,
        makeRun({
          id: 200,
          workflowFile: 'release.yml',
          event: 'push',
          headSha: sha,
          headBranch: 'main',
          status: 'completed',
          conclusion: 'success',
        }),
      )
    }
    const out = await w.go()
    expect(out.status).toBe('registry_pending')
    expect(out.missingPackages).toEqual([{ name: '@namzu/sdk', from: '1.0.0', to: '1.1.0' }])
    expect(w.clock.totalSlept).toBeGreaterThanOrEqual(10 * 60_000)
    expect(w.store.getActiveShipSession(REPO)).not.toBeNull()
  })
})

describe('ship: resume', () => {
  it('continues a CI wait without approving twice', async () => {
    const w = world({ config: { approveBotRuns: true } })
    w.github.addRun(REPO, makeRun({ status: 'completed', conclusion: 'action_required' }), {
      onApprove: green(3),
    })
    const first = new HookClock()
    first.hook = (n) => {
      if (n === 2) throw new Error('interrupted')
    }
    await expect(w.go({}, { clock: first })).rejects.toThrow('interrupted')
    const session = w.store.getActiveShipSession(REPO)
    expect(session?.step).toBe('gate')
    expect(w.github.writesOf('approveRun')).toHaveLength(1)

    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(out.sessionId).toBe(session?.id)
    expect(w.github.writesOf('approveRun')).toHaveLength(1)
    expect(w.github.writesOf('mergePr')).toHaveLength(1)
  })

  it('continues the release wait after the merge without merging again', async () => {
    const w = world()
    w.github.addRun(REPO, makeRun(), { stages: green() })
    const first = new HookClock()
    first.hook = () => {
      if (w.github.writesOf('mergePr').length > 0) throw new Error('interrupted')
    }
    await expect(w.go({}, { clock: first })).rejects.toThrow('interrupted')
    expect(w.store.getActiveShipSession(REPO)?.step).toBe('release')

    const out = await w.go()
    expect(out.status).toBe('shipped')
    expect(out.mergeCommitSha).toBe('merge-commit-1')
    expect(w.github.writesOf('mergePr')).toHaveLength(1)
  })
})
