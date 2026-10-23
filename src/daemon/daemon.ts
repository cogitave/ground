import type { RepoConfig } from '../config.js'
import type { Clock, GitHubPort, Logger } from '../ports.js'
import type { StateStore } from '../state/index.js'
import { type RunningServer, startServer } from './server.js'
import { summarizeRepo } from './summary.js'
import { Watcher } from './watcher.js'

export interface DaemonDeps {
  configs: RepoConfig[]
  github: GitHubPort
  store: StateStore
  clock: Clock
  log: Logger
  socketPath: string
}

export interface RunningDaemon {
  /** Stops the loops and the socket; the caller owns the store and closes it afterwards. */
  stop(): Promise<void>
}

export async function startDaemon(deps: DaemonDeps): Promise<RunningDaemon> {
  const { configs, github, store, clock, log } = deps
  const watchers = new Map(
    configs.map((c) => [c.repo, new Watcher(c, github, store, clock, log)] as const),
  )

  const server: RunningServer = await startServer(deps.socketPath, {
    store,
    repos: () => configs.map((c) => c.repo),
    status: () =>
      configs.map((c) => {
        const w = watchers.get(c.repo)
        return {
          ...summarizeRepo(store, c.repo),
          lastPollAt: w?.lastPollAt ?? null,
          lastError: w?.lastError ?? null,
        }
      }),
  })

  let stopNow: () => void = () => {}
  const stopped = new Promise<void>((resolve) => {
    stopNow = resolve
  })
  const loops = [...watchers.values()].map((w) => w.run(stopped))
  log.info('groundd started', { repos: configs.map((c) => c.repo) })

  return {
    async stop() {
      stopNow()
      await Promise.all(loops)
      await server.close()
      log.info('groundd stopped')
    },
  }
}
