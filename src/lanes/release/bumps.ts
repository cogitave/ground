import type { PrFile } from '../../ports.js'
import type { BumpedPackage, BumpScan } from './types.js'

// Anchored to two-space indentation (or a tab) so a nested "name" or "version"
// inside dependencies, bin or an author block is never mistaken for the package's.
const TOP = String.raw`^[ +-](?: {2}|\t)`
const NAME = new RegExp(`${TOP}"name"\\s*:\\s*"([^"]+)"`)
const OLD_VERSION = /^-(?: {2}|\t)"version"\s*:\s*"([^"]+)"/
const NEW_VERSION = /^\+(?: {2}|\t)"version"\s*:\s*"([^"]+)"/
const PRIVATE = new RegExp(`${TOP}"private"\\s*:\\s*true`)

/**
 * Which packages a version PR bumps, read from the package.json patches only.
 *
 * GitHubPort has no file-content read, so the package name comes from the
 * patch's context lines. A version bump sits within three lines of "name" in
 * every npm package.json, so the default context shows both. When it does not
 * (or GitHub omitted the patch as too large) the file is reported as
 * unresolved instead of guessing a name from its directory: a wrong name would
 * make the registry check poll a package that is not ours.
 */
export function scanBumps(files: PrFile[]): BumpScan {
  const packages: BumpedPackage[] = []
  const unresolved: string[] = []
  for (const file of files) {
    if (!/(^|\/)package\.json$/.test(file.path)) continue
    if (file.status === 'removed' || !file.patch) {
      if (file.status !== 'removed') unresolved.push(file.path)
      continue
    }
    const lines = file.patch.split('\n')
    if (lines.some((l) => PRIVATE.test(l))) continue
    let from: string | null = null
    let to: string | null = null
    let name: string | null = null
    for (const line of lines) {
      name ??= NAME.exec(line)?.[1] ?? null
      from ??= OLD_VERSION.exec(line)?.[1] ?? null
      to ??= NEW_VERSION.exec(line)?.[1] ?? null
    }
    if (to === null || from === to) continue
    if (name === null) unresolved.push(file.path)
    else packages.push({ name, from, to })
  }
  return { packages, unresolved }
}
