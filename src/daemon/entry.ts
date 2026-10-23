import { realpathSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

/** True when `metaUrl` is the script node was started with, also through a bin symlink. */
export function isEntry(metaUrl: string): boolean {
  const argv1 = process.argv[1]
  if (!argv1) return false
  try {
    return realpathSync(argv1) === realpathSync(fileURLToPath(metaUrl))
  } catch {
    return false
  }
}
