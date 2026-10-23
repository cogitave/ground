import { mkdtempSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { FakeClock } from '../testing/fake-clock.js'
import { StateStore } from './store.js'

const repo = 'cogitave/namzu'

function setup() {
  const clock = new FakeClock('2026-01-01T00:00:00Z')
  return { clock, store: new StateStore(clock, ':memory:') }
}

describe('ship sessions', () => {
  it('creates, updates and finds the active session', async () => {
    const { clock, store } = setup()
    expect(store.getActiveShipSession(repo)).toBeNull()
    const s = store.createShipSession({ repo, step: 'find-pr' })
    expect(s).toMatchObject({ repo, step: 'find-pr', prNumber: null, outcome: null, data: {} })
    expect(s.startedAt).toBe('2026-01-01T00:00:00.000Z')

    await clock.sleep(1000)
    const u = store.updateShipSession(s.id, { step: 'merge', prNumber: 5, data: { sha: 'x' } })
    expect(u).toMatchObject({ step: 'merge', prNumber: 5, data: { sha: 'x' } })
    expect(u.updatedAt).toBe('2026-01-01T00:00:01.000Z')
    expect(u.startedAt).toBe(s.startedAt)
    expect(store.getActiveShipSession(repo)?.id).toBe(s.id)
  })

  it('stops being active once it has an outcome, and is per repo', () => {
    const { store } = setup()
    const s = store.createShipSession({ repo, step: 'summary' })
    store.createShipSession({ repo: 'cogitave/other', step: 'find-pr' })
    store.updateShipSession(s.id, { outcome: 'shipped' })
    expect(store.getActiveShipSession(repo)).toBeNull()
    expect(store.getShipSession(s.id)?.outcome).toBe('shipped')
    expect(store.getActiveShipSession('cogitave/other')).not.toBeNull()
  })

  it('returns the newest active session', () => {
    const { store } = setup()
    store.createShipSession({ repo, step: 'a' })
    const b = store.createShipSession({ repo, step: 'b' })
    expect(store.getActiveShipSession(repo)?.id).toBe(b.id)
  })

  it('rejects updates to unknown sessions', () => {
    expect(() => setup().store.updateShipSession('nope', { step: 'x' })).toThrow(/no ship session/)
  })
})

describe('events and flakes', () => {
  it('lists events newest first, honouring the limit', () => {
    const { store } = setup()
    store.recordEvent(repo, 'a', { n: 1 })
    store.recordEvent(repo, 'b')
    store.recordEvent('cogitave/other', 'c')
    expect(store.listEvents(repo).map((e) => e.kind)).toEqual(['b', 'a'])
    expect(store.listEvents(repo, 1).map((e) => e.kind)).toEqual(['b'])
    expect(store.listEvents(repo)[1]?.payload).toEqual({ n: 1 })
  })

  it('records flakes per repo', () => {
    const { store } = setup()
    store.recordFlake({ repo, runId: 7, job: 'test', attempt: 1 })
    store.recordFlake({ repo: 'cogitave/other', runId: 8, job: 'lint', attempt: 1 })
    expect(store.listFlakes(repo)).toMatchObject([{ runId: 7, job: 'test', attempt: 1 }])
  })
})

describe('on-disk store', () => {
  it('creates a 0700 directory, migrates once and persists across opens', () => {
    const clock = new FakeClock()
    const path = join(mkdtempSync(join(tmpdir(), 'ground-state-')), 'nested', 'state.db')
    const first = new StateStore(clock, path)
    expect(statSync(join(path, '..')).mode & 0o777).toBe(0o700)
    first.recordEvent(repo, 'kept')
    first.close()
    const second = new StateStore(clock, path)
    expect(second.listEvents(repo).map((e) => e.kind)).toEqual(['kept'])
    second.close()
  })
})
