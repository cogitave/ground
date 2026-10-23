import { mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { installService, renderUnit } from './install-service.js'

let dir: string | undefined
afterEach(() => {
  if (dir) rmSync(dir, { recursive: true, force: true })
  dir = undefined
})

describe('install-service', () => {
  it('writes the unit under <configHome>/systemd/user and returns the enable command', () => {
    dir = mkdtempSync(join(tmpdir(), 'ground-svc-'))
    const res = installService({
      daemonEntry: '/opt/ground/dist/daemon/main.js',
      nodePath: '/usr/bin/node',
      configHome: dir,
    })
    expect(res.unitPath).toBe(join(dir, 'systemd', 'user', 'groundd.service'))
    expect(statSync(res.unitPath).isFile()).toBe(true)
    const text = readFileSync(res.unitPath, 'utf8')
    expect(text).toContain('ExecStart=/usr/bin/node /opt/ground/dist/daemon/main.js\n')
    expect(text).toContain('WantedBy=default.target')
    expect(text).not.toContain('Environment=')
    expect(res.command).toContain('systemctl --user enable --now groundd.service')
  })

  it('quotes paths with spaces and carries a custom GROUND_HOME', () => {
    const text = renderUnit({
      daemonEntry: '/home/a b/dist/daemon/main.js',
      nodePath: '/usr/bin/node',
      groundHome: '/data/ground',
    })
    expect(text).toContain('ExecStart=/usr/bin/node "/home/a b/dist/daemon/main.js"')
    expect(text).toContain('Environment=GROUND_HOME=/data/ground')
  })
})
