import { mkdtempSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { StateStore } from '../state/index.js'
import { FakeClock } from '../testing/index.js'
import { socketRequest } from './client.js'
import { type RunningServer, startServer } from './server.js'

const repo = 'cogitave/namzu' as const
let dir: string
let path: string
let store: StateStore
let server: RunningServer | undefined

const deps = () => ({
  store,
  repos: () => [repo],
  status: () => [{ repo, mainTip: 'abc', versionPr: null, lastEvent: null, activeShip: null }],
})

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'ground-sock-'))
  path = join(dir, 'groundd.sock')
  store = new StateStore(new FakeClock(), ':memory:')
})

afterEach(async () => {
  await server?.close()
  server = undefined
  store.close()
  rmSync(dir, { recursive: true, force: true })
})

describe('groundd socket', () => {
  it('is private to the user', async () => {
    server = await startServer(path, deps())
    expect(statSync(path).mode & 0o777).toBe(0o600)
  })

  it('answers status', async () => {
    server = await startServer(path, deps())
    expect(await socketRequest(path, { cmd: 'status' })).toEqual({
      ok: true,
      data: [{ repo, mainTip: 'abc', versionPr: null, lastEvent: null, activeShip: null }],
    })
  })

  it('answers events, newest first and limited', async () => {
    store.recordEvent(repo, 'a')
    store.recordEvent(repo, 'b')
    server = await startServer(path, deps())
    const reply = (await socketRequest(path, { cmd: 'events', repo, limit: 1 })) as {
      ok: boolean
      data: { kind: string }[]
    }
    expect(reply.data.map((e) => e.kind)).toEqual(['b'])
  })

  it('refuses unknown commands, unknown repos and non-objects', async () => {
    server = await startServer(path, deps())
    expect(await socketRequest(path, { cmd: 'nope' })).toEqual({
      ok: false,
      error: 'unknown cmd: nope',
    })
    expect(await socketRequest(path, { cmd: 'events', repo: 'x/y' })).toMatchObject({ ok: false })
    expect(await socketRequest(path, 5)).toMatchObject({ ok: false })
  })

  it('replaces a stale socket file but not a live daemon', async () => {
    writeFileSync(path, '')
    server = await startServer(path, deps())
    await expect(startServer(path, deps())).rejects.toThrow(/already running/)
  })
})
