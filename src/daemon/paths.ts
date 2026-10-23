import { homedir } from 'node:os'
import { join } from 'node:path'

export function groundHome(env: NodeJS.ProcessEnv = process.env): string {
  return env.GROUND_HOME && env.GROUND_HOME.length > 0
    ? env.GROUND_HOME
    : join(homedir(), '.ground')
}

export const reposDir = (home: string): string => join(home, 'repos')
export const socketPath = (home: string): string => join(home, 'groundd.sock')
export const dbPath = (home: string): string => join(home, 'state.db')
