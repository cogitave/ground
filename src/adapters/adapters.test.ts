import { describe, expect, it } from 'vitest'
import { SystemClock } from './clock-system.js'
import {
  ghMergeArgs,
  mapFile,
  mapJob,
  mapPr,
  mapRun,
  OctokitGitHub,
  tokenFromGh,
} from './github-octokit.js'
import { NpmRegistry, parseLatest, registryUrl } from './registry-npm.js'

describe('mapPr', () => {
  const raw = {
    number: 5,
    title: 't',
    state: 'open',
    merged: false,
    head: { ref: 'b', sha: 'h', repo: { full_name: 'o/r' } },
    base: { ref: 'main', sha: 'bs' },
    user: { login: 'bot' },
    mergeable: null,
  }
  it('maps the fields', () => {
    expect(mapPr(raw)).toEqual({
      number: 5,
      title: 't',
      state: 'open',
      merged: false,
      headRef: 'b',
      headSha: 'h',
      headRepo: 'o/r',
      baseRef: 'main',
      baseSha: 'bs',
      author: 'bot',
      mergeable: null,
    })
  })
  it('survives a deleted fork and derives merged from merged_at', () => {
    const pr = mapPr({
      ...raw,
      state: 'closed',
      merged: undefined,
      merged_at: '2026-01-01',
      head: { ref: 'b', sha: 'h', repo: null },
      user: null,
    })
    expect(pr).toMatchObject({ state: 'closed', merged: true, headRepo: '/', author: '' })
  })
})

describe('mapFile / mapJob / mapRun', () => {
  it('normalises file status and keeps the patch only when present', () => {
    expect(mapFile({ filename: 'a', status: 'changed' })).toEqual({ path: 'a', status: 'modified' })
    expect(mapFile({ filename: 'a', status: 'renamed', patch: 'p' })).toEqual({
      path: 'a',
      status: 'renamed',
      patch: 'p',
    })
  })
  it('maps jobs', () => {
    expect(mapJob({ name: 'x', status: 'completed', conclusion: 'failure' })).toEqual({
      name: 'x',
      status: 'completed',
      conclusion: 'failure',
    })
  })
  it('maps runs, reducing the workflow path to a file name', () => {
    const run = mapRun({
      id: 1,
      path: '.github/workflows/ci.yml',
      event: 'pull_request',
      head_sha: 's',
      head_branch: null,
      status: 'completed',
      conclusion: 'success',
      created_at: 'c',
      actor: { login: 'a' },
      html_url: 'u',
    })
    expect(run).toMatchObject({
      workflowFile: 'ci.yml',
      headBranch: '',
      runAttempt: 1,
      actor: 'a',
      triggeringActor: 'a',
    })
  })
})

describe('gh helpers', () => {
  it('builds the admin merge command', () => {
    expect(ghMergeArgs('o/r', 5, { method: 'squash', expectedHeadSha: 'abc' })).toEqual([
      'pr',
      'merge',
      '5',
      '--repo',
      'o/r',
      '--squash',
      '--admin',
      '--match-head-commit',
      'abc',
    ])
  })
  it('reads the token through the injected exec', async () => {
    const calls: string[][] = []
    const token = await tokenFromGh(async (cmd, args) => {
      calls.push([cmd, ...args])
      return { stdout: 'tok\n' }
    })
    expect(token).toBe('tok')
    expect(calls).toEqual([['gh', 'auth', 'token']])
    await expect(tokenFromGh(async () => ({ stdout: '\n' }))).rejects.toThrow(/gh auth login/)
  })
  it('shells out to gh for an admin merge and reads the merge commit', async () => {
    const calls: string[][] = []
    const octokit = {
      rest: { pulls: { get: async () => ({ data: { merge_commit_sha: 'mc' } }) } },
    }
    const gh = new OctokitGitHub({
      exec: async (cmd, args) => {
        calls.push([cmd, ...args])
        return { stdout: '' }
      },
      // Only the one method the admin path touches is faked.
      octokit: octokit as never,
    })
    const res = await gh.mergePr('o/r', 5, { method: 'squash', admin: true, expectedHeadSha: 'h' })
    expect(res).toEqual({ mergeCommitSha: 'mc' })
    expect(calls[0]?.slice(0, 3)).toEqual(['gh', 'pr', 'merge'])
  })
})

describe('npm registry', () => {
  it('reads dist-tags.latest', () => {
    expect(parseLatest({ 'dist-tags': { latest: '1.2.3' } })).toBe('1.2.3')
    expect(parseLatest({})).toBeNull()
    expect(parseLatest(null)).toBeNull()
  })
  it('escapes scoped names', () => {
    expect(registryUrl('@namzu/sdk')).toBe('https://registry.npmjs.org/@namzu%2Fsdk')
  })
  it('maps 404 to null, errors to throws, and success to the version, via an injected fetch', async () => {
    const answer = (status: number, body: unknown = {}) =>
      (async () => new Response(JSON.stringify(body), { status })) as typeof fetch
    expect(await new NpmRegistry(answer(404)).latestVersion('x')).toBeNull()
    expect(
      await new NpmRegistry(answer(200, { 'dist-tags': { latest: '2.0.0' } })).latestVersion('x'),
    ).toBe('2.0.0')
    await expect(new NpmRegistry(answer(500)).latestVersion('x')).rejects.toThrow(/500/)
  })
})

describe('SystemClock', () => {
  it('reports a real date', () => {
    expect(new SystemClock().now()).toBeInstanceOf(Date)
  })
})
