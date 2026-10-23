import { beforeEach, describe, expect, it } from 'vitest'
import type { RepoConfig } from '../config.js'
import { StateStore } from '../state/index.js'
import { FakeClock, FakeGitHub, MemoryLogger, makePr, makeRun } from '../testing/index.js'
import { Watcher } from './watcher.js'

const repo = 'cogitave/namzu' as const
const config: RepoConfig = {
  repo,
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

let gh: FakeGitHub
let clock: FakeClock
let store: StateStore
let log: MemoryLogger

const kinds = (): string[] =>
  store
    .listEvents(repo)
    .map((e) => e.kind)
    .reverse()

const watcher = (cfg: RepoConfig = config): Watcher => new Watcher(cfg, gh, store, clock, log)

beforeEach(() => {
  gh = new FakeGitHub().setBranchSha(repo, 'main', 'tip-1')
  clock = new FakeClock()
  store = new StateStore(clock, ':memory:')
  log = new MemoryLogger()
})

describe('Watcher dedupe', () => {
  it('records the tip once and again only when it moves', async () => {
    const w = watcher()
    await w.tick()
    await w.tick()
    gh.setBranchSha(repo, 'main', 'tip-2')
    await w.tick()
    expect(kinds()).toEqual(['main.tip', 'main.tip'])
    expect(store.listEvents(repo)[0]?.payload).toMatchObject({ sha: 'tip-2', previous: 'tip-1' })
  })

  it('records version PR appearance, head change and disappearance', async () => {
    const w = watcher()
    await w.tick()
    gh.setPr(repo, makePr())
    await w.tick()
    await w.tick()
    gh.updatePr(repo, 1, { headSha: 'head-sha-2' })
    await w.tick()
    gh.updatePr(repo, 1, { state: 'closed' })
    await w.tick()
    expect(kinds()).toEqual([
      'main.tip',
      'version-pr.appeared',
      'version-pr.head-changed',
      'version-pr.gone',
    ])
  })

  it('records a run only when its state changes', async () => {
    gh.setPr(repo, makePr())
    gh.addRun(repo, makeRun(), {
      stages: [
        { status: 'in_progress' },
        { status: 'in_progress' },
        { status: 'completed', conclusion: 'success' },
      ],
    })
    const w = watcher()
    for (let i = 0; i < 4; i++) await w.tick()
    const states = store.listEvents(repo).filter((e) => e.kind === 'run.state')
    expect(states.map((e) => e.payload.status).reverse()).toEqual([
      'queued',
      'in_progress',
      'completed',
    ])
  })

  it('does not repeat events after a restart', async () => {
    gh.setPr(repo, makePr())
    await watcher().tick()
    const before = kinds()
    await watcher().tick()
    expect(kinds()).toEqual(before)
  })

  it('logs a failing poll once and keeps going', async () => {
    const bad = new FakeGitHub()
    const w = new Watcher(config, bad, store, clock, log)
    await w.tick()
    await w.tick()
    expect(log.messages('error')).toEqual(['poll failed'])
    expect(w.lastError).toContain('no branch')
    expect(w.lastPollAt).not.toBeNull()
  })
})

describe('Watcher approval', () => {
  const actionRequired = makeRun({ status: 'completed', conclusion: 'action_required' })
  const on = { ...config, approveBotRuns: true }

  it('approves an action_required run for the PR head, once', async () => {
    gh.setPr(repo, makePr()).addRun(repo, actionRequired, {
      onApprove: [{ status: 'in_progress' }],
    })
    const w = watcher(on)
    await w.tick()
    await w.tick()
    expect(gh.writesOf('approveRun')).toHaveLength(1)
    expect(kinds()).toContain('run.approved')
  })

  it('does nothing when the repo has not opted in', async () => {
    gh.setPr(repo, makePr()).addRun(repo, actionRequired)
    await watcher().tick()
    expect(gh.writesOf('approveRun')).toHaveLength(0)
  })

  it('never approves a fork head', async () => {
    gh.setPr(repo, makePr({ headRepo: 'mallory/namzu' })).addRun(repo, actionRequired)
    await watcher(on).tick()
    expect(gh.writesOf('approveRun')).toHaveLength(0)
  })

  it('never approves a run for an older SHA', async () => {
    gh.setPr(repo, makePr()).addRun(repo, { ...actionRequired, headSha: 'old' })
    await watcher(on).tick()
    expect(gh.writesOf('approveRun')).toHaveLength(0)
  })

  it('run() stops when the stop promise resolves', async () => {
    gh.setPr(repo, makePr())
    const never = { now: () => clock.now(), sleep: () => new Promise<void>(() => {}) }
    const w = new Watcher(config, gh, store, never, log)
    let stop: () => void = () => {}
    const stopped = new Promise<void>((r) => {
      stop = r
    })
    const running = w.run(stopped)
    stop()
    await running
    expect(w.lastPollAt).not.toBeNull()
  })
})
