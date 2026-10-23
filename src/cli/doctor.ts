import { SystemClock } from '../adapters/clock-system.js'
import { tokenFromGh } from '../adapters/github-octokit.js'
import { socketRequest } from '../daemon/client.js'
import { dbPath, groundHome, socketPath } from '../daemon/paths.js'
import { loadRepos } from '../daemon/repos.js'
import { StateStore } from '../state/index.js'

export interface CheckResult {
  ok: boolean
  detail: string
}

export interface Check {
  name: string
  /** A failing 'warn' check is reported but does not fail doctor: ground fails open. */
  level?: 'fail' | 'warn'
  run(): Promise<CheckResult>
}

export interface DoctorReport {
  rows: { name: string; status: 'ok' | 'WARN' | 'FAIL'; detail: string }[]
  exitCode: number
}

export async function runChecks(checks: Check[]): Promise<DoctorReport> {
  const rows: DoctorReport['rows'] = []
  for (const c of checks) {
    let r: CheckResult
    try {
      r = await c.run()
    } catch (err) {
      r = { ok: false, detail: (err as Error).message }
    }
    rows.push({
      name: c.name,
      status: r.ok ? 'ok' : c.level === 'warn' ? 'WARN' : 'FAIL',
      detail: r.detail,
    })
  }
  return { rows, exitCode: rows.some((r) => r.status === 'FAIL') ? 1 : 0 }
}

export function formatReport(report: DoctorReport): string {
  const w = Math.max(...report.rows.map((r) => r.name.length), 5)
  return report.rows
    .map((r) => `${r.status.padEnd(4)}  ${r.name.padEnd(w)}  ${r.detail}`)
    .join('\n')
}

export function defaultChecks(env: NodeJS.ProcessEnv = process.env): Check[] {
  const home = groundHome(env)
  return [
    {
      name: 'node >= 24',
      run: async () => {
        const major = Number(process.versions.node.split('.')[0])
        return { ok: major >= 24, detail: `node ${process.versions.node}` }
      },
    },
    {
      name: 'gh token',
      run: async () => {
        await tokenFromGh()
        return { ok: true, detail: 'gh auth token works' }
      },
    },
    {
      name: 'repo configs',
      run: async () => {
        const { configs, errors } = loadRepos(home)
        if (errors.length > 0) return { ok: false, detail: errors.join('; ') }
        if (configs.length === 0) return { ok: false, detail: `no configs in ${home}/repos` }
        return { ok: true, detail: configs.map((c) => c.repo).join(', ') }
      },
    },
    {
      name: 'state db',
      run: async () => {
        const store = new StateStore(new SystemClock(), dbPath(home))
        store.close()
        return { ok: true, detail: dbPath(home) }
      },
    },
    {
      name: 'daemon socket',
      level: 'warn',
      run: async () => {
        try {
          await socketRequest(socketPath(home), { cmd: 'status' })
          return { ok: true, detail: socketPath(home) }
        } catch {
          return { ok: false, detail: 'groundd is not running (ground still works without it)' }
        }
      },
    },
  ]
}
