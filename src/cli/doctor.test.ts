import { describe, expect, it } from 'vitest'
import { type Check, formatReport, runChecks } from './doctor.js'

const ok = (name: string): Check => ({ name, run: async () => ({ ok: true, detail: 'fine' }) })

describe('doctor', () => {
  it('exits 0 when every check passes', async () => {
    const r = await runChecks([ok('a'), ok('b')])
    expect(r.exitCode).toBe(0)
    expect(r.rows.map((x) => x.status)).toEqual(['ok', 'ok'])
  })

  it('exits 1 on a failing or throwing check, and keeps running the rest', async () => {
    const r = await runChecks([
      { name: 'bad', run: async () => ({ ok: false, detail: 'broken' }) },
      {
        name: 'boom',
        run: async () => {
          throw new Error('kaput')
        },
      },
      ok('last'),
    ])
    expect(r.exitCode).toBe(1)
    expect(r.rows.map((x) => x.detail)).toEqual(['broken', 'kaput', 'fine'])
  })

  it('treats a failing warn check as non-fatal', async () => {
    const r = await runChecks([
      { name: 'socket', level: 'warn', run: async () => ({ ok: false, detail: 'down' }) },
    ])
    expect(r.exitCode).toBe(0)
    expect(r.rows[0]?.status).toBe('WARN')
  })

  it('prints a table with one line per check', async () => {
    const text = formatReport(await runChecks([ok('node'), ok('gh token')]))
    expect(text.split('\n')).toHaveLength(2)
    expect(text).toContain('gh token')
  })
})
