import { mkdirSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

export interface InstallServiceOptions {
  /** Absolute path of the built daemon entry (dist/daemon/main.js). */
  daemonEntry: string
  nodePath: string
  /** Base for the user unit directory; default ~/.config. */
  configHome?: string
  /** Written into the unit only when it is not the default. */
  groundHome?: string
}

function quote(arg: string): string {
  return /[\s"'\\$%]/.test(arg) ? `"${arg.replace(/(["\\])/g, '\\$1').replace(/%/g, '%%')}"` : arg
}

export function renderUnit(
  opts: Pick<InstallServiceOptions, 'daemonEntry' | 'nodePath' | 'groundHome'>,
): string {
  const env = opts.groundHome ? `Environment=${quote(`GROUND_HOME=${opts.groundHome}`)}\n` : ''
  return `[Unit]
Description=ground: drives merge, validation and release
After=network-online.target

[Service]
Type=simple
ExecStart=${quote(opts.nodePath)} ${quote(opts.daemonEntry)}
${env}Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
`
}

/** The built daemon next to this file's dist directory, or an error when running from source. */
export function builtDaemonEntry(): string {
  const entry = join(dirname(fileURLToPath(import.meta.url)), '..', 'daemon', 'main.js')
  if (!import.meta.url.endsWith('.js')) {
    throw new Error(
      'install-service needs the built package: run `pnpm build` and use dist/cli/main.js',
    )
  }
  return entry
}

export function installService(opts: InstallServiceOptions): { unitPath: string; command: string } {
  const dir = join(opts.configHome ?? join(homedir(), '.config'), 'systemd', 'user')
  mkdirSync(dir, { recursive: true })
  const unitPath = join(dir, 'groundd.service')
  writeFileSync(unitPath, renderUnit(opts), { mode: 0o644 })
  return {
    unitPath,
    command: 'systemctl --user daemon-reload && systemctl --user enable --now groundd.service',
  }
}
