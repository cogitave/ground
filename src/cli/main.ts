import { parseArgs } from 'node:util'
import { SystemClock } from '../adapters/clock-system.js'
import { OctokitGitHub } from '../adapters/github-octokit.js'
import { NpmRegistry } from '../adapters/registry-npm.js'
import type { RepoConfig } from '../config.js'
import { socketRequest } from '../daemon/client.js'
import { ConsoleLogger } from '../daemon/console-logger.js'
import { isEntry } from '../daemon/entry.js'
import { dbPath, groundHome, socketPath } from '../daemon/paths.js'
import { loadRepos } from '../daemon/repos.js'
import { type RepoSummary, summarizeRepo } from '../daemon/summary.js'
import type { ShipDeps, ShipStatus } from '../lanes/release/index.js'
import { ship } from '../lanes/release/index.js'
import type { RepoSlug } from '../ports.js'
import { StateStore } from '../state/index.js'
import { defaultChecks, formatReport, runChecks } from './doctor.js'
import { builtDaemonEntry, installService } from './install-service.js'

const USAGE = `usage: ground <command>

  status [repo]                 what ground has seen (asks groundd, else reads the state db)
  ship <repo> [--confirm] [--json]
                                walk a version PR to npm; dry run unless --confirm
  daemon                        run groundd in the foreground
  doctor                        check the setup
  install-service               write the systemd user unit (does not enable it)
`

const out = (s: string): void => {
  process.stdout.write(`${s}\n`)
}

function findConfig(configs: RepoConfig[], name: string | undefined): RepoConfig {
  const c = configs.find((x) => x.repo === name)
  if (!c) {
    const known = configs.map((x) => x.repo).join(', ') || 'none'
    throw new Error(`unknown repo "${name ?? ''}" (configured: ${known})`)
  }
  return c
}

async function summaries(home: string): Promise<RepoSummary[]> {
  try {
    const reply = (await socketRequest(socketPath(home), { cmd: 'status' })) as {
      ok: boolean
      data?: RepoSummary[]
    }
    if (reply.ok && reply.data) return reply.data
  } catch {
    // daemon down: fall through to the db
  }
  const { configs } = loadRepos(home)
  const store = new StateStore(new SystemClock(), dbPath(home))
  try {
    return configs.map((c) => summarizeRepo(store, c.repo))
  } finally {
    store.close()
  }
}

async function cmdStatus(args: string[], home: string): Promise<number> {
  const { values, positionals } = parseArgs({
    args,
    options: { json: { type: 'boolean' } },
    allowPositionals: true,
  })
  let rows = await summaries(home)
  if (positionals[0]) rows = rows.filter((r) => r.repo === (positionals[0] as RepoSlug))
  if (rows.length === 0) {
    process.stderr.write('no matching repositories\n')
    return 1
  }
  if (values.json) {
    out(JSON.stringify(rows, null, 2))
    return 0
  }
  for (const r of rows) {
    out(r.repo)
    out(`  main        ${r.mainTip ?? 'not seen yet'}`)
    out(`  version PR  ${r.versionPr ? `#${r.versionPr.number} @ ${r.versionPr.headSha}` : 'none'}`)
    out(
      `  ship        ${r.activeShip ? `${r.activeShip.step} (PR ${r.activeShip.prNumber ?? '-'})` : 'idle'}`,
    )
    out(`  last event  ${r.lastEvent ? `${r.lastEvent.kind} at ${r.lastEvent.at}` : 'none'}`)
    if (r.lastError) out(`  last error  ${r.lastError}`)
  }
  return 0
}

/** Exit 0 only when there is nothing left for a human to look at. */
const OK_STATUSES: ReadonlySet<ShipStatus> = new Set(['shipped', 'nothing_to_ship', 'planned'])

export async function runShipCommand(
  deps: ShipDeps,
  opts: { confirm: boolean; json: boolean },
  write: (s: string) => void = out,
): Promise<number> {
  const outcome = await ship(deps, { confirm: opts.confirm })
  write(opts.json ? JSON.stringify(outcome, null, 2) : outcome.summary)
  return OK_STATUSES.has(outcome.status) ? 0 : 1
}

async function cmdShip(args: string[], home: string): Promise<number> {
  const { values, positionals } = parseArgs({
    args,
    options: { confirm: { type: 'boolean' }, json: { type: 'boolean' } },
    allowPositionals: true,
  })
  const { configs } = loadRepos(home)
  const config = findConfig(configs, positionals[0])
  const clock = new SystemClock()
  const store = new StateStore(clock, dbPath(home))
  try {
    return await runShipCommand(
      {
        config,
        github: new OctokitGitHub(),
        registry: new NpmRegistry(),
        clock,
        logger: new ConsoleLogger(clock),
        store,
      },
      { confirm: values.confirm === true, json: values.json === true },
    )
  } finally {
    store.close()
  }
}

async function cmdDoctor(home: string): Promise<number> {
  const report = await runChecks(defaultChecks({ ...process.env, GROUND_HOME: home }))
  out(formatReport(report))
  return report.exitCode
}

function cmdInstallService(home: string): number {
  const { unitPath, command } = installService({
    daemonEntry: builtDaemonEntry(),
    nodePath: process.execPath,
    groundHome: process.env.GROUND_HOME ? home : undefined,
  })
  out(`wrote ${unitPath}`)
  out('to start it, run:')
  out(`  ${command}`)
  return 0
}

export async function runCli(argv: string[]): Promise<number> {
  const [cmd, ...rest] = argv
  const home = groundHome()
  switch (cmd) {
    case 'status':
      return cmdStatus(rest, home)
    case 'ship':
      return cmdShip(rest, home)
    case 'daemon': {
      const { runDaemon } = await import('../daemon/main.js')
      return runDaemon()
    }
    case 'doctor':
      return cmdDoctor(home)
    case 'install-service':
      return cmdInstallService(home)
    default:
      process.stderr.write(USAGE)
      return cmd === undefined || cmd === 'help' || cmd === '--help' ? 0 : 2
  }
}

if (isEntry(import.meta.url)) {
  runCli(process.argv.slice(2)).then(
    (code) => {
      process.exitCode = code
    },
    (err: Error) => {
      process.stderr.write(`ground: ${err.message}\n`)
      process.exitCode = 1
    },
  )
}
