import { describe, expect, it } from 'vitest'
import { canApproveRun } from './approval.js'
import { makePr, makeRun } from './testing/index.js'

const config = {
  repo: 'cogitave/namzu' as const,
  approveBotRuns: true,
  approveBranches: ['changeset-release/main'],
}

describe('canApproveRun', () => {
  const cases: [
    string,
    Partial<typeof config>,
    Parameters<typeof makePr>[0],
    Parameters<typeof makeRun>[0],
    boolean,
  ][] = [
    ['all conditions hold', {}, {}, {}, true],
    ['opt-in is off', { approveBotRuns: false }, {}, {}, false],
    ['head is a fork', {}, { headRepo: 'mallory/namzu' }, {}, false],
    ['branch not allow-listed', {}, { headRef: 'feature/x' }, { headBranch: 'feature/x' }, false],
    ['empty allow-list', { approveBranches: [] }, {}, {}, false],
    ['run is for an older SHA', {}, {}, { headSha: 'old-sha' }, false],
    ['run is for another branch', {}, {}, { headBranch: 'other' }, false],
    ['PR is closed', {}, { state: 'closed' }, {}, false],
    ['repo slug differs only by case', {}, { headRepo: 'Cogitave/Namzu' }, {}, true],
  ]
  for (const [name, cfg, pr, run, allowed] of cases) {
    it(`${allowed ? 'allows' : 'refuses'}: ${name}`, () => {
      const d = canApproveRun({ ...config, ...cfg }, makePr(pr), makeRun(run))
      expect(d.allowed).toBe(allowed)
      if (!d.allowed) expect(d.reason.length).toBeGreaterThan(0)
    })
  }
})
