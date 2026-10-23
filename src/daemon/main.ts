import { SystemClock } from '../adapters/clock-system.js'
import { OctokitGitHub } from '../adapters/github-octokit.js'
import { StateStore } from '../state/index.js'
import { ConsoleLogger } from './console-logger.js'
import { startDaemon } from './daemon.js'
import { isEntry } from './entry.js'
import { dbPath, groundHome, socketPath } from './paths.js'
import { loadRepos } from './repos.js'

export async function runDaemon(env: NodeJS.ProcessEnv = process.env): Promise<number> {
  const clock = new SystemClock()
  const log = new ConsoleLogger(clock, undefined, env.GROUND_DEBUG === '1')
  const home = groundHome(env)
  const { configs, errors } = loadRepos(home)
  for (const e of errors) log.error('skipping repo config', { error: e })
  if (configs.length === 0) {
    log.error('no valid repo configs, nothing to watch', { home })
    return 1
  }

  const store = new StateStore(clock, dbPath(home))
  let daemon: Awaited<ReturnType<typeof startDaemon>>
  try {
    daemon = await startDaemon({
      configs,
      github: new OctokitGitHub(),
      store,
      clock,
      log,
      socketPath: socketPath(home),
    })
  } catch (err) {
    log.error('cannot start', { error: (err as Error).message })
    store.close()
    return 1
  }

  await new Promise<void>((resolve) => {
    const onSignal = (sig: string) => {
      log.info('shutting down', { signal: sig })
      resolve()
    }
    process.once('SIGTERM', () => onSignal('SIGTERM'))
    process.once('SIGINT', () => onSignal('SIGINT'))
  })
  await daemon.stop()
  store.close()
  return 0
}

if (isEntry(import.meta.url)) {
  runDaemon().then((code) => {
    process.exitCode = code
  })
}
