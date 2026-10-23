import type { RepoSlug } from '../ports.js'
import type { StateStore } from '../state/index.js'

export interface RepoSummary {
  repo: RepoSlug
  mainTip: string | null
  versionPr: { number: number; headSha: string } | null
  lastEvent: { kind: string; at: string } | null
  activeShip: { id: string; step: string; prNumber: number | null } | null
  /** Live-only: filled in by the daemon, absent when read from the store. */
  lastPollAt?: string | null
  lastError?: string | null
}

export interface RecordedState {
  mainTip: string | undefined
  /** undefined: never seen; null: seen and gone. */
  versionPr: { number: number; headSha: string } | null | undefined
  /** runId -> signature of the last recorded state. */
  runs: Map<number, string>
}

const SCAN = 500

export function runSignature(r: {
  status: string
  conclusion: string | null
  runAttempt: number
}): string {
  return `${r.status}/${r.conclusion ?? ''}/${r.runAttempt}`
}

/** Rebuilds what the watcher last recorded, so a restart does not repeat events. */
export function recordedState(store: StateStore, repo: RepoSlug): RecordedState {
  const state: RecordedState = { mainTip: undefined, versionPr: undefined, runs: new Map() }
  for (const e of store.listEvents(repo, SCAN)) {
    const p = e.payload
    if (e.kind === 'main.tip' && state.mainTip === undefined) {
      state.mainTip = p.sha as string
    } else if (e.kind.startsWith('version-pr.') && state.versionPr === undefined) {
      state.versionPr =
        e.kind === 'version-pr.gone'
          ? null
          : { number: p.number as number, headSha: p.headSha as string }
    } else if (e.kind === 'run.state' && !state.runs.has(p.runId as number)) {
      state.runs.set(
        p.runId as number,
        runSignature({
          status: p.status as string,
          conclusion: (p.conclusion as string | null) ?? null,
          runAttempt: p.attempt as number,
        }),
      )
    }
  }
  return state
}

export function summarizeRepo(store: StateStore, repo: RepoSlug): RepoSummary {
  const s = recordedState(store, repo)
  const last = store.listEvents(repo, 1)[0]
  const ship = store.getActiveShipSession(repo)
  return {
    repo,
    mainTip: s.mainTip ?? null,
    versionPr: s.versionPr ?? null,
    lastEvent: last ? { kind: last.kind, at: last.at } : null,
    activeShip: ship ? { id: ship.id, step: ship.step, prNumber: ship.prNumber } : null,
  }
}
