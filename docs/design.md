# ground — design

ground is a local service that drives merge, trunk validation and release for
cogitave's repositories. It runs on the owner's machine, watches GitHub, and
does the waiting, approving, re-running, bisecting and confirming that a person
otherwise babysits by hand.

It exists because at 6-9 agent-authored merges a day, `namzu`'s path from
merge to npm took 90-120 minutes and needed someone at every step. The measured
causes are in "Why" below; every component here answers one of them.

## Why (measured on namzu, 2026-10-23)

1. Version-PR CI: 9 of 9 first-attempt runs whose actor was
   `github-actions[bot]` stopped at `action_required` (repo approval policy
   `first_time_contributors`). Only re-runs by the owner executed.
2. `pnpm -r test` (17 min) is serialised by topological order:
   sdk 207 s → sandbox 196 s → cli 500 s alone at the end.
3. The SDK coverage step re-runs the same 998-file suite (264 s).
4. The consumer install check (6 min) and publint (3 min) run serially.
5. 21 of 60 main commits are version-PR merges that change only version
   lines, CHANGELOGs and consumed changesets, yet run the full 38-minute CI.
6. Two merges close together invalidate each other's validated-tree record,
   so `release.yml` re-runs every gate inline (25-45 min; 210 min in 3 days).
7. File-level test selection in the SDK pays little (a typical change reaches
   ~80% of test files through directory barrels); package-level selection
   saves ~29% on code commits.
8. 7 of 34 CI runs failed in 3 days, some of them flakes; a failed job leaves
   no validation record.

Problems 2-4 are fixed inside namzu's own workflows. ground owns the rest.

## Principles

- **The server does the work.** Agents and the owner merge; ground watches
  every repository and moves each change along. Polling, not webhooks: the
  machine has no public address and needs none.
- **Validate trunk after merge, find the culprit, revert by PR.** A global
  merge lock does not survive agent throughput. ground validates main's tip
  after each push, bisects a red range over commits that were green on their
  own PR, and opens a revert PR with the evidence.
- **Run only what changed, at package granularity.** Changed packages and their
  dependents; a lockfile, root config or workflow change runs everything.
- **Never weaken a trust chain.** ground accelerates and orchestrates; it never
  produces the evidence a release trusts.
- **Fail open.** If ground is down or the machine is off, every repository
  still works by hand exactly as it did before ground existed.
- **Use maintained tools.** pnpm filters, vitest sharding, changesets, gh/octokit.
  Custom code only where nothing maintained fits.

## Hard rules

These are invariants, enforced in code and covered by tests.

1. **No release trust.** ground never writes, signs, uploads or forges a
   validated-tree record or any artifact a release workflow reads as proof.
   Local results are advisory.
2. **Not a self-hosted runner.** The machine is never registered as a GitHub
   Actions runner for a public repository; a fork PR would execute code here.
3. **Merges only on an explicit command.** `ground ship <repo> --confirm` is the
   owner's go for that one release. The daemon never merges on its own.
4. **Approve only our own runs.** ground approves an `action_required` workflow
   run only when the PR's head repository is the base repository (never a
   fork), the head branch is on the repo's allow-list (for namzu:
   `changeset-release/main`), and the run's head SHA is the PR's current head.
   This is opt-in per repository.
5. **Revert by PR, never by push.** A revert is a branch plus a PR with the
   evidence; a human merges it.
6. **Never push to a default branch, never force-push, never delete a branch
   ground did not create.**
7. **Registry is the source of truth for a publish.** A green release run is
   not a publish; ground confirms with the registry and tolerates propagation
   lag before calling anything failed.

## Architecture

```
            ┌──────────── owner / agents ────────────┐
            │  ground status | ship | check | bisect  │
            └──────────────────┬─────────────────────┘
                               │ unix socket (JSON lines)
┌──────────────────────────────▼──────────────────────────────┐
│ groundd (systemd --user)                                     │
│  watcher ─ polls GitHub per repo with ETags, records events  │
│  lanes   ─ release · trunk · pre-flight                      │
│  runner  ─ local isolated worktrees for trunk/bisect runs    │
│  state   ─ SQLite (~/.ground/state.db)                       │
└──────────┬───────────────────────────────────┬──────────────┘
           │ REST (token from `gh auth token`) │ npm registry
           ▼                                   ▼
        GitHub                                npm
```

- **CLI** (`ground`): thin; talks to the daemon over a unix socket, or runs a
  lane directly in the foreground when the daemon is not running.
- **Daemon** (`groundd`): one process, one loop per repository, all state in
  SQLite so a restart resumes where it stopped.
- **GitHub client**: one interface (`GitHubPort`) so every lane is tested
  against a fake. Conditional requests (ETag) keep polling inside rate limits.
- **Ecosystem adapters**: `pnpm-changesets` first (namzu), then `bun`
  (namzu-web) and `cargo` (yuva). An adapter knows how to list packages,
  compute affected packages, run gates locally, and which packages a version
  PR bumps.
- **Repository config**: `~/.ground/repos/<owner>__<repo>.json`, owned by the
  owner, not by the consumer repository, so nothing in a public repo changes
  what ground is allowed to do.

## Lanes and milestones

| Milestone | Lane | Delivers |
|---|---|---|
| M1 | release | CLI + daemon skeleton, state, GitHub port, `ground ship` end to end (dry-run by default) |
| M2 | trunk | postsubmit of main's tip in a local worktree; bisect over green-on-PR commits; revert PR |
| M3 | pre-flight | `ground check`: affected packages + the repo's gates locally, for agents before push |
| M4 | classify | version-only classifier; CI-side pieces vendored into consumer repos by a reviewed sync PR |
| M5 | adapters | bun and cargo adapters; onboard namzu-web |

The namzu-internal CI fixes (problems 2-4) run in parallel in namzu itself.

## M1 specification

### `ground ship <repo>` state machine

Each step is idempotent and persisted, so `ground ship` can be interrupted and
re-run. Without `--confirm` it stops before the first GitHub write and prints
what it would do.

1. **Find the version PR**: open PR from `releaseBranch` (namzu:
   `changeset-release/main`) into the default branch. If none: when the default
   branch has pending changesets and a release run is in progress, wait for it
   (the version PR appears when it ends, up to `versionPrWaitMinutes`);
   otherwise report "nothing to ship".
2. **Snapshot check**: the set of `.changeset/*.md` on the default branch
   (excluding `README.md`, `config.json`) must equal the set the PR deletes.
   If not, the PR is stale: wait for the release run to refresh it, then loop.
3. **Gate the head SHA**: find the CI workflow runs (`ciWorkflow`, event
   `pull_request`) whose head SHA is the PR's current head. Never act on a run
   for an older SHA. If one is `action_required` and rule 4 allows it,
   approve it (opt-in); otherwise report and wait.
4. **Wait for green**: poll until the run completes. On failure, re-run failed
   jobs up to `maxRerunsPerRun` (default 1) and record the attempt in the flake
   ledger; after that, stop and report the failing jobs.
5. **Merge** (requires `--confirm`): squash merge, admin if `adminMerge`. Before
   merging, re-check that the head SHA did not move. Report whether the base is
   still the default-branch tip (if not, the release will re-validate inline).
6. **Release run**: find the `releaseWorkflow` run for the merge commit SHA; wait;
   re-run failed jobs once on failure; report which validation path it took if
   the log says so.
7. **Registry**: for every package the PR bumps (parsed from the PR's
   `package.json` diffs), poll the registry until it reports the new version,
   tolerating `registryLagMinutes` (default 10) of propagation lag.
8. **Summary**: write the outcome to state and print it.

### Repository config (`~/.ground/repos/cogitave__namzu.json`)

```json
{
  "repo": "cogitave/namzu",
  "defaultBranch": "main",
  "adapter": "pnpm-changesets",
  "ciWorkflow": "ci.yml",
  "releaseWorkflow": "release.yml",
  "releaseBranch": "changeset-release/main",
  "approveBotRuns": false,
  "approveBranches": ["changeset-release/main"],
  "adminMerge": true,
  "mergeMethod": "squash",
  "maxRerunsPerRun": 1,
  "versionPrWaitMinutes": 45,
  "registryLagMinutes": 10,
  "pollSeconds": 30
}
```

### Stack

TypeScript on Node 24 (ESM), pnpm, vitest, biome. SQLite through `node:sqlite`.
GitHub through `@octokit/rest` with the token from `gh auth token`. No other
runtime dependency without a reason written next to it.

### Tests

Every lane runs against a fake `GitHubPort` and a fake registry; no test
touches the network or GitHub. Time is a port too (`Clock`), so waits are
driven by fake timers and no test races real time.
