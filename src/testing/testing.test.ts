import { describe, expect, it } from 'vitest'
import { FakeClock } from './fake-clock.js'
import { FakeGitHub, makePr, makeRun } from './fake-github.js'
import { FakeRegistry } from './fake-registry.js'
import { MemoryLogger } from './memory-logger.js'

const repo = 'cogitave/namzu'

describe('FakeClock', () => {
  it('advances virtual time on sleep without waiting and records sleeps', async () => {
    const clock = new FakeClock('2026-01-01T00:00:00Z')
    await clock.sleep(30_000)
    await clock.sleep(500)
    expect(clock.sleeps).toEqual([30_000, 500])
    expect(clock.totalSlept).toBe(30_500)
    expect(clock.now().toISOString()).toBe('2026-01-01T00:00:30.500Z')
    clock.advance(1000)
    expect(clock.now().toISOString()).toBe('2026-01-01T00:00:31.500Z')
  })
})

describe('FakeRegistry', () => {
  it('returns null for unknown packages and records queries', async () => {
    const r = new FakeRegistry()
    expect(await r.latestVersion('a')).toBeNull()
    expect(r.queries).toEqual(['a'])
  })

  it('publishes after the scheduled number of queries', async () => {
    const r = new FakeRegistry().setVersion('a', '1.0.0').publishAfterQueries('a', '1.1.0', 2)
    expect(await r.latestVersion('a')).toBe('1.0.0')
    expect(await r.latestVersion('a')).toBe('1.0.0')
    expect(await r.latestVersion('a')).toBe('1.1.0')
    expect(await r.latestVersion('a')).toBe('1.1.0')
  })
})

describe('MemoryLogger', () => {
  it('keeps entries by level', () => {
    const l = new MemoryLogger()
    l.info('a', { x: 1 })
    l.warn('b')
    expect(l.messages()).toEqual(['a', 'b'])
    expect(l.messages('warn')).toEqual(['b'])
    expect(l.entries[0]?.data).toEqual({ x: 1 })
  })
})

describe('FakeGitHub', () => {
  it('finds open PRs by head and base and ignores closed ones', async () => {
    const gh = new FakeGitHub()
      .setPr(repo, makePr({ number: 1, state: 'closed' }))
      .setPr(repo, makePr({ number: 2 }))
    expect((await gh.findOpenPr(repo, 'changeset-release/main', 'main'))?.number).toBe(2)
    expect(await gh.findOpenPr(repo, 'other', 'main')).toBeNull()
    await expect(gh.getPr(repo, 9)).rejects.toThrow(/no PR/)
  })

  it('serves files, dirs and branch shas', async () => {
    const gh = new FakeGitHub()
      .setPr(repo, makePr())
      .setPrFiles(repo, 1, [{ path: 'a.md', status: 'removed' }])
      .setDir(repo, 'main', '.changeset', ['x.md'])
      .setBranchSha(repo, 'main', 'abc')
    expect(await gh.listPrFiles(repo, 1)).toEqual([{ path: 'a.md', status: 'removed' }])
    expect(await gh.listDir(repo, 'main', '.changeset')).toEqual(['x.md'])
    expect(await gh.listDir(repo, 'main', 'nope')).toEqual([])
    expect(await gh.getBranchSha(repo, 'main')).toBe('abc')
    await expect(gh.getBranchSha(repo, 'dev')).rejects.toThrow()
  })

  it('advances a run one stage per poll and filters', async () => {
    const gh = new FakeGitHub().addRun(repo, makeRun({ id: 7 }), {
      stages: [{ status: 'in_progress' }, { status: 'completed', conclusion: 'success' }],
    })
    const poll = () =>
      gh.listWorkflowRuns(repo, 'ci.yml', { headSha: 'head-sha-1', event: 'pull_request' })
    expect((await poll())[0]?.status).toBe('queued')
    expect((await poll())[0]?.status).toBe('in_progress')
    const done = (await poll())[0]
    expect([done?.status, done?.conclusion]).toEqual(['completed', 'success'])
    expect(await gh.listWorkflowRuns(repo, 'ci.yml', { headSha: 'old' })).toEqual([])
    expect(await gh.listWorkflowRuns(repo, 'release.yml', {})).toEqual([])
  })

  it('records approvals and re-runs and swaps in their scripts', async () => {
    const gh = new FakeGitHub().addRun(repo, makeRun({ id: 7, status: 'action_required' }), {
      onApprove: [{ status: 'completed', conclusion: 'failure' }],
      onRerun: [{ status: 'completed', conclusion: 'success' }],
    })
    await gh.approveRun(repo, 7)
    expect(gh.runState(repo, 7).status).toBe('queued')
    expect((await gh.listWorkflowRuns(repo, 'ci.yml', {}))[0]?.status).toBe('queued')
    expect(gh.runState(repo, 7).conclusion).toBe('failure')
    await gh.rerunFailedJobs(repo, 7)
    expect(gh.runState(repo, 7).runAttempt).toBe(2)
    await gh.listWorkflowRuns(repo, 'ci.yml', {})
    expect(gh.runState(repo, 7).conclusion).toBe('success')
    expect(gh.writes.map((w) => w.method)).toEqual(['approveRun', 'rerunFailedJobs'])
    expect(gh.writesOf('approveRun')[0]?.args).toEqual({ runId: 7 })
    await expect(gh.approveRun(repo, 99)).rejects.toThrow(/no run/)
  })

  it('merges only with the expected head sha and fires onMerge', async () => {
    const gh = new FakeGitHub().setPr(repo, makePr()).setMergeCommit(repo, 1, 'm1')
    let seen: string | undefined
    gh.onMerge = (_pr, sha) => {
      seen = sha
    }
    await expect(
      gh.mergePr(repo, 1, { method: 'squash', admin: true, expectedHeadSha: 'stale' }),
    ).rejects.toThrow(/mismatch/)
    const res = await gh.mergePr(repo, 1, {
      method: 'squash',
      admin: true,
      expectedHeadSha: 'head-sha-1',
    })
    expect(res.mergeCommitSha).toBe('m1')
    expect(seen).toBe('m1')
    expect((await gh.getPr(repo, 1)).merged).toBe(true)
    expect(await gh.getBranchSha(repo, 'main')).toBe('m1')
    expect(gh.writesOf('mergePr')).toHaveLength(2)
    await expect(
      gh.mergePr(repo, 1, { method: 'squash', admin: true, expectedHeadSha: 'head-sha-1' }),
    ).rejects.toThrow(/not open/)
  })

  it('lets a test move the head between reads', async () => {
    const gh = new FakeGitHub().setPr(repo, makePr()).updatePr(repo, 1, { headSha: 'new' })
    expect((await gh.getPr(repo, 1)).headSha).toBe('new')
  })

  it('serves scripted jobs', async () => {
    const gh = new FakeGitHub().setJobs(repo, 7, [
      { name: 'test', status: 'completed', conclusion: 'failure' },
    ])
    expect(await gh.listRunJobs(repo, 7)).toHaveLength(1)
    expect(await gh.listRunJobs(repo, 8)).toEqual([])
  })
})
