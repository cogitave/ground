import { readdirSync } from 'node:fs'
import { join } from 'node:path'
import { loadRepoConfig, type RepoConfig } from '../config.js'
import { reposDir } from './paths.js'

export interface LoadedRepos {
  configs: RepoConfig[]
  /** One message per file that did not load; other repos still run. */
  errors: string[]
}

export function loadRepos(home: string): LoadedRepos {
  const dir = reposDir(home)
  let names: string[]
  try {
    names = readdirSync(dir)
      .filter((n) => n.endsWith('.json'))
      .sort()
  } catch (err) {
    return { configs: [], errors: [`cannot read ${dir} (${(err as Error).message})`] }
  }
  const out: LoadedRepos = { configs: [], errors: [] }
  for (const name of names) {
    try {
      out.configs.push(loadRepoConfig(join(dir, name)))
    } catch (err) {
      out.errors.push((err as Error).message)
    }
  }
  return out
}
