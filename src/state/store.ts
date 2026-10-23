import { randomUUID } from 'node:crypto'
import { chmodSync, mkdirSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import type { Clock, RepoSlug } from '../ports.js'

export const DEFAULT_DB_PATH = join(homedir(), '.ground', 'state.db')

export interface ShipSession {
  id: string
  repo: RepoSlug
  prNumber: number | null
  step: string
  data: Record<string, unknown>
  /** null while the session is still running. */
  outcome: string | null
  startedAt: string
  updatedAt: string
}

export interface StoredEvent {
  id: number
  repo: RepoSlug
  kind: string
  payload: Record<string, unknown>
  at: string
}

export interface Flake {
  id: number
  repo: RepoSlug
  runId: number
  job: string
  attempt: number
  at: string
}

export interface ShipSessionPatch {
  prNumber?: number | null
  step?: string
  data?: Record<string, unknown>
  outcome?: string | null
}

const MIGRATIONS: readonly string[] = [
  `CREATE TABLE ship_sessions (
     id TEXT PRIMARY KEY,
     repo TEXT NOT NULL,
     pr_number INTEGER,
     step TEXT NOT NULL,
     data TEXT NOT NULL DEFAULT '{}',
     outcome TEXT,
     started_at TEXT NOT NULL,
     updated_at TEXT NOT NULL
   );
   CREATE INDEX ship_sessions_repo ON ship_sessions (repo, outcome);
   CREATE TABLE events (
     id INTEGER PRIMARY KEY AUTOINCREMENT,
     repo TEXT NOT NULL,
     kind TEXT NOT NULL,
     payload TEXT NOT NULL DEFAULT '{}',
     at TEXT NOT NULL
   );
   CREATE INDEX events_repo ON events (repo, id);
   CREATE TABLE flakes (
     id INTEGER PRIMARY KEY AUTOINCREMENT,
     repo TEXT NOT NULL,
     run_id INTEGER NOT NULL,
     job TEXT NOT NULL,
     attempt INTEGER NOT NULL,
     at TEXT NOT NULL
   );
   CREATE INDEX flakes_repo ON flakes (repo, id);`,
]

type Row = Record<string, unknown>

function toSession(r: Row): ShipSession {
  return {
    id: r.id as string,
    repo: r.repo as RepoSlug,
    prNumber: (r.pr_number as number | null) ?? null,
    step: r.step as string,
    data: JSON.parse(r.data as string) as Record<string, unknown>,
    outcome: (r.outcome as string | null) ?? null,
    startedAt: r.started_at as string,
    updatedAt: r.updated_at as string,
  }
}

export class StateStore {
  private readonly db: DatabaseSync

  /** `path` may be ':memory:'. Any other path gets its directory created with mode 0700. */
  constructor(
    private readonly clock: Clock,
    path: string = DEFAULT_DB_PATH,
  ) {
    if (path !== ':memory:') {
      mkdirSync(dirname(path), { recursive: true, mode: 0o700 })
      // A pre-existing directory keeps its old mode, so tighten it explicitly.
      chmodSync(dirname(path), 0o700)
    }
    // groundd and `ground ship` open the same file from separate processes.
    this.db = new DatabaseSync(path, { timeout: 5000 })
    if (path !== ':memory:') {
      this.db.exec('PRAGMA journal_mode=WAL')
      chmodSync(path, 0o600)
    }
    this.migrate()
  }

  close(): void {
    this.db.close()
  }

  private migrate(): void {
    this.db.exec(
      'CREATE TABLE IF NOT EXISTS migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)',
    )
    const row = this.db.prepare('SELECT MAX(version) AS v FROM migrations').get() as Row
    const current = (row.v as number | null) ?? 0
    for (let i = current; i < MIGRATIONS.length; i++) {
      this.db.exec('BEGIN')
      try {
        this.db.exec(MIGRATIONS[i] as string)
        this.db
          .prepare('INSERT INTO migrations (version, applied_at) VALUES (?, ?)')
          .run(i + 1, this.stamp())
        this.db.exec('COMMIT')
      } catch (err) {
        this.db.exec('ROLLBACK')
        throw err
      }
    }
  }

  private stamp(): string {
    return this.clock.now().toISOString()
  }

  createShipSession(input: {
    repo: RepoSlug
    prNumber?: number | null
    step: string
    data?: Record<string, unknown>
  }): ShipSession {
    const id = randomUUID()
    const at = this.stamp()
    this.db
      .prepare(
        'INSERT INTO ship_sessions (id, repo, pr_number, step, data, started_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)',
      )
      .run(
        id,
        input.repo,
        input.prNumber ?? null,
        input.step,
        JSON.stringify(input.data ?? {}),
        at,
        at,
      )
    return this.getShipSession(id) as ShipSession
  }

  getShipSession(id: string): ShipSession | null {
    const r = this.db.prepare('SELECT * FROM ship_sessions WHERE id = ?').get(id)
    return r ? toSession(r) : null
  }

  updateShipSession(id: string, patch: ShipSessionPatch): ShipSession {
    const current = this.getShipSession(id)
    if (!current) throw new Error(`no ship session ${id}`)
    const next = {
      prNumber: patch.prNumber === undefined ? current.prNumber : patch.prNumber,
      step: patch.step ?? current.step,
      data: patch.data ?? current.data,
      outcome: patch.outcome === undefined ? current.outcome : patch.outcome,
    }
    this.db
      .prepare(
        'UPDATE ship_sessions SET pr_number = ?, step = ?, data = ?, outcome = ?, updated_at = ? WHERE id = ?',
      )
      .run(next.prNumber, next.step, JSON.stringify(next.data), next.outcome, this.stamp(), id)
    return this.getShipSession(id) as ShipSession
  }

  /** The newest session of the repo that has no outcome yet, i.e. one `ship` can resume. */
  getActiveShipSession(repo: RepoSlug): ShipSession | null {
    const r = this.db
      .prepare(
        'SELECT * FROM ship_sessions WHERE repo = ? AND outcome IS NULL ORDER BY started_at DESC, rowid DESC LIMIT 1',
      )
      .get(repo)
    return r ? toSession(r) : null
  }

  recordEvent(repo: RepoSlug, kind: string, payload: Record<string, unknown> = {}): StoredEvent {
    const at = this.stamp()
    const res = this.db
      .prepare('INSERT INTO events (repo, kind, payload, at) VALUES (?, ?, ?, ?)')
      .run(repo, kind, JSON.stringify(payload), at)
    return { id: Number(res.lastInsertRowid), repo, kind, payload, at }
  }

  /** Newest first. */
  listEvents(repo: RepoSlug, limit = 50): StoredEvent[] {
    return this.db
      .prepare('SELECT * FROM events WHERE repo = ? ORDER BY id DESC LIMIT ?')
      .all(repo, limit)
      .map((r) => ({
        id: r.id as number,
        repo: r.repo as RepoSlug,
        kind: r.kind as string,
        payload: JSON.parse(r.payload as string) as Record<string, unknown>,
        at: r.at as string,
      }))
  }

  recordFlake(input: { repo: RepoSlug; runId: number; job: string; attempt: number }): Flake {
    const at = this.stamp()
    const res = this.db
      .prepare('INSERT INTO flakes (repo, run_id, job, attempt, at) VALUES (?, ?, ?, ?, ?)')
      .run(input.repo, input.runId, input.job, input.attempt, at)
    return { id: Number(res.lastInsertRowid), ...input, at }
  }

  /** Newest first. */
  listFlakes(repo: RepoSlug): Flake[] {
    return this.db
      .prepare('SELECT * FROM flakes WHERE repo = ? ORDER BY id DESC')
      .all(repo)
      .map((r) => ({
        id: r.id as number,
        repo: r.repo as RepoSlug,
        runId: r.run_id as number,
        job: r.job as string,
        attempt: r.attempt as number,
        at: r.at as string,
      }))
  }
}
