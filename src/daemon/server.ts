import { chmodSync, existsSync, unlinkSync } from 'node:fs'
import { createServer, type Server, type Socket } from 'node:net'
import type { RepoSlug } from '../ports.js'
import type { StateStore } from '../state/index.js'
import { socketRequest } from './client.js'
import type { RepoSummary } from './summary.js'

export interface ServerDeps {
  store: StateStore
  repos: () => RepoSlug[]
  status: () => RepoSummary[]
}

const MAX_LIMIT = 500
const DEFAULT_LIMIT = 50

export type Reply = { ok: true; data: unknown } | { ok: false; error: string }

export function handleRequest(deps: ServerDeps, raw: unknown): Reply {
  if (typeof raw !== 'object' || raw === null)
    return { ok: false, error: 'request must be a JSON object' }
  const req = raw as Record<string, unknown>
  switch (req.cmd) {
    case 'status':
      return { ok: true, data: deps.status() }
    case 'events': {
      const repo = req.repo
      if (typeof repo !== 'string' || !deps.repos().includes(repo as RepoSlug)) {
        return { ok: false, error: `unknown repo: ${String(repo)}` }
      }
      const asked =
        typeof req.limit === 'number' && Number.isInteger(req.limit) ? req.limit : DEFAULT_LIMIT
      const limit = Math.min(Math.max(asked, 1), MAX_LIMIT)
      return { ok: true, data: deps.store.listEvents(repo as RepoSlug, limit) }
    }
    default:
      return { ok: false, error: `unknown cmd: ${String(req.cmd)}` }
  }
}

export interface RunningServer {
  close(): Promise<void>
}

export async function startServer(path: string, deps: ServerDeps): Promise<RunningServer> {
  if (existsSync(path)) {
    // A live daemon owns the socket; only a dead one's leftover may be removed.
    const alive = await socketRequest(path, { cmd: 'status' }).then(
      () => true,
      () => false,
    )
    if (alive) throw new Error(`groundd is already running (${path})`)
    unlinkSync(path)
  }

  const open = new Set<Socket>()
  const server: Server = createServer((sock) => {
    open.add(sock)
    sock.once('close', () => open.delete(sock))
    let buf = ''
    sock.setEncoding('utf8')
    sock.on('error', () => sock.destroy())
    sock.on('data', (chunk: string) => {
      buf += chunk
      for (let nl = buf.indexOf('\n'); nl >= 0; nl = buf.indexOf('\n')) {
        const line = buf.slice(0, nl)
        buf = buf.slice(nl + 1)
        if (line.trim().length === 0) continue
        let reply: Reply
        try {
          reply = handleRequest(deps, JSON.parse(line))
        } catch (err) {
          reply = { ok: false, error: `bad request: ${(err as Error).message}` }
        }
        sock.write(`${JSON.stringify(reply)}\n`)
      }
    })
  })

  // Create the socket already private; chmod after listen would leave a window.
  const previousUmask = process.umask(0o177)
  try {
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject)
      server.listen(path, () => resolve())
    })
  } finally {
    process.umask(previousUmask)
  }
  chmodSync(path, 0o600)

  return {
    close: () =>
      new Promise<void>((resolve) => {
        server.close(() => resolve())
        for (const s of open) s.destroy()
      }),
  }
}
