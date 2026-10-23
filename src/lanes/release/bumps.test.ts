import { describe, expect, it } from 'vitest'
import { scanBumps } from './bumps.js'

const patch = (lines: string[]) => lines.join('\n')

describe('scanBumps', () => {
  it('reads name and versions from a standard hunk', () => {
    const files = [
      {
        path: 'packages/sdk/package.json',
        status: 'modified' as const,
        patch: patch([
          '@@ -1,5 +1,5 @@',
          ' {',
          '   "name": "@namzu/sdk",',
          '-  "version": "1.0.0",',
          '+  "version": "1.1.0",',
          '   "type": "module",',
        ]),
      },
    ]
    expect(scanBumps(files)).toEqual({
      packages: [{ name: '@namzu/sdk', from: '1.0.0', to: '1.1.0' }],
      unresolved: [],
    })
  })

  it('ignores nested versions, private packages, non-package files and unchanged versions', () => {
    const files = [
      {
        path: 'CHANGELOG.md',
        status: 'modified' as const,
        patch: '-  "version": "1"\n+  "version": "2"',
      },
      {
        path: 'apps/x/package.json',
        status: 'modified' as const,
        patch: patch([
          ' {',
          '   "name": "x",',
          '   "private": true,',
          '-  "version": "1.0.0",',
          '+  "version": "1.0.1",',
        ]),
      },
      {
        path: 'packages/a/package.json',
        status: 'modified' as const,
        patch: patch(['   "name": "a",', '     "version": "9.9.9",', '+    "version": "9.9.10",']),
      },
      {
        path: 'packages/b/package.json',
        status: 'modified' as const,
        patch: patch(['   "name": "b",', '-  "left": "1",', '+  "left": "2",']),
      },
    ]
    expect(scanBumps(files)).toEqual({ packages: [], unresolved: [] })
  })

  it('reports a package whose name is not in the diff instead of guessing', () => {
    const files = [
      {
        path: 'packages/c/package.json',
        status: 'modified' as const,
        patch: patch(['-  "version": "1.0.0",', '+  "version": "1.0.1",']),
      },
      { path: 'packages/d/package.json', status: 'modified' as const },
    ]
    expect(scanBumps(files)).toEqual({
      packages: [],
      unresolved: ['packages/c/package.json', 'packages/d/package.json'],
    })
  })

  it('treats a new package as a bump from nothing', () => {
    const files = [
      {
        path: 'packages/n/package.json',
        status: 'added' as const,
        patch: patch(['+{', '+  "name": "n",', '+  "version": "0.1.0",']),
      },
    ]
    expect(scanBumps(files).packages).toEqual([{ name: 'n', from: null, to: '0.1.0' }])
  })
})
