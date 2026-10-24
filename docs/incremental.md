# Incremental CI

A test or gate whose inputs did not change is not run again. "Inputs" means
the files it actually touched when it last ran, not a list someone declared.

## Why this shape (measured on namzu, 2026-10-24)

- Module-graph selection (the same answer `vitest list --changed` gives) runs a
  median 17% and a mean 32.5% of test time. Commits that touch only the CLI
  package run ~13%; commits that touch the SDK run ~66%.
- Function-level selection from coverage does not beat it on time (~66% for SDK
  commits): 82% of SDK commits change top-level code, and 73 test files spawn
  child processes or import dynamically, which coverage cannot see. Those 73 are
  26% of the suite's time and had to run every time.
- Directory barrels barely matter. A handful of hub modules reached by more
  than 1000 of 1709 test files do.
- Typecheck in build mode with a kept `tsbuildinfo` goes from 152 s cold to
  1-7 s; build has a ~30 s floor; lint is 2 s and not worth caching.

So the engine traces what a test opens, including in its child processes, and
hashes those files. That makes the opaque tests selectable too, which is where
coverage-based selection lost a quarter of the suite.

## Model

- **Manifest**: for each test file, the set of `(path, blake3)` of every file
  its process tree opened for reading during a passing run, the content hash of
  every binary it executed (the toolchain fingerprint), the values of a declared
  environment allow-list (`ground trace --env NAME`, repeatable), the commit it
  was traced at and the engine version. A directory the test listed is hashed
  over its entry names; a symlink the test examined (`lstat`, `readlink`, a
  dangling target) is hashed over its target text, so a retarget to identical
  bytes is still a change.
- A file the tree rewrote while it was reading it (its inode, size or
  timestamps moved between the first read and the end of the run) is recorded
  as `volatile`, a file gone by the end as `missing`; neither equals any hash,
  so the test is always selected rather than trusted on an end-of-run hash.
- **Selection** for a change: a test runs when it has no manifest, when the test
  file itself changed, or when any file in its manifest now hashes differently
  (build outputs are hashed after the build, so a `dist/` file that did not
  change does not select anything). Everything is selected when a global input
  changed (lockfile, root config, CI workflow, matched against paths relative
  to the git toplevel, diffed from the base **and** from every commit the
  manifests were traced at, so a change that landed on the base after they were
  recorded is seen), when any executed binary hashes differently or is gone,
  when a declared environment variable now differs, when a manifest records no
  engine, toolchain or commit, or when the engine version differs.
- **Tests the globs cannot name.** Selection can only choose among the files
  the test globs list (`*.test.*`, `*.spec.*`, `*.test-d.*` by default). Unless
  `--tests` declares the list, a changed code file under `__tests__/`, `test/`
  or `tests/` that matches no glob and that no manifest read selects
  everything: the runner may run it and nothing could ever select it. Declare
  `--tests` to match the runner's `include` exactly and the guard is off.
- **Gates** other than tests (typecheck, build, pack, publint) are keyed the
  same way, per package, from their declared inputs plus the outputs of the
  packages they depend on.
- **Fail closed**: a missing, unreadable or stale manifest selects the test. An
  error anywhere selects everything, and `ground select` exits non-zero when
  everything must run but the test list could not be produced (a broken
  checkout), in every output format, instead of printing an empty list.

## Where manifests come from

- A full run records them: `ground trace -- <runner> <test file>` runs one test
  file under the tracer (Linux: a ptrace tracer with a seccomp filter that stops only on open, exec,
  stat, access and readlink calls, following every thread and process; elsewhere
  it fails closed and everything is selected) and writes its manifest. The
  trace ends when the root command exits: anything it left running (a dev
  server, a worker daemon) is killed, because a task whose tracer is gone would
  fail its filtered calls with ENOSYS, and its later reads are not recorded.
- On the owner's machine, groundd re-traces main's tip in a local worktree after
  each push (32 cores: 1000 test files took 383 s with coverage; tracing is in
  the same range). CI full runs can produce them too and publish them as an
  artifact, so CI never depends on anyone's machine.
- Manifests are a selection hint, not evidence. A wrong manifest can only make a
  pull-request run skip a test; it can never make a release trust a tree.

## Trust

- A run that skipped anything is **partial**. Partial runs never produce a
  release validation record: the adopter's record step embeds `selection:
  partial|full`, and the release-side verifier rejects partial records. This
  only ever tightens a trust chain.
- The release candidate (for Changesets repositories, the version PR) is always
  validated in full, on hosted runners.
- Trunk validation (M2) runs everything on main's tip after merge and bisects a
  red range, so a test that selection wrongly skipped is caught within one
  trunk run, before any release.

## Interfaces

- `ground select --base <ref> [--format json|lines|vitest]` prints the tests
  to run; `json` carries `all` and `all_reason` when everything was selected.
  `vitest` prints `GROUND_NO_TESTS_SELECTED` for an empty selection.
- `ground trace -- <command>` records a manifest for one test invocation.
- `ground manifests import <dir>` copies manifests into the local store
  (`pull|push` against a CI artifact is not built yet).
- `ground trace --env NAME` records `NAME` into the manifest; `ground select`
  compares it with the current environment. `--tests` declares the test globs.

## Measured (namzu `@namzu/sdk`, 1000 test files, 2026-10-24)

Tracing cost, every file in isolation through the repo's own runner, 28
workers, same harness traced and untraced, all passing: wall 125 s untraced,
144 s traced (+15%); per-file slowdown p50 x1.18, p90 x1.32, worst x1.94
(`shell-lexer-bash.test.ts`, 21 s to 43 s, one stop per spawned bash). A
manifest is p50 192 KB (max 427 KB); 215 MB for the package.

Selection over the last 60 first-parent commits touching `packages/sdk/src`,
share of sdk test time selected:

| rule | mean | median | commits selecting everything |
|---|---|---|---|
| strict (this engine) | 77% | 100% | 43 of 60 |
| directory listings ignored (not sound in general) | 53% | 58% | 9 |
| reference: module graph, SDK commits | ~66% | | |
| reference: coverage, SDK commits | ~66% | | |

When strict does not select everything (17 commits) it selects 10% of files and
18% of time. The 73 tests that spawn processes or import dynamically (26% of
suite time) are selectable and none is always-run: each was selected in 2 to 56
of the 60 commits. Soundness spot check on three real commits: the selected set
and the full run agree with zero misses, but no test failed in any full run, so
that is weak evidence; a differential trial with deliberately broken inputs has
not been done. These numbers predate the fixes above (commit recording, the
toolchain and environment checks, volatile inputs); none of them changes which
files the strict rule selects, so they were not re-measured.

**The dominant loss is directory inputs.** Vitest discovery lists ~60 `src`
directories, so every manifest holds those listing hashes and any commit that
adds or removes a file anywhere in `src` invalidates all of them: 34 of the 43
select-everything commits. 9 are global globs (7 are `pnpm-lock.yaml`, which is
redundant once install runs before select and the `node_modules` inputs are
hashed). Ignoring listings is unsound (a test that globs a directory would be
missed); the sound fix is to compare a listing filtered by the pattern the
runner or test actually used. That is the next step, not done.

## Limits

Known and deliberate; each is a way a trace can under-record.

- Not trapped, so not recorded: `rename`, `link`, `symlink`, `unlink`,
  `truncate`, `chdir`, xattr calls, `inotify_add_watch`. A file made by
  `rename(2)` is therefore classified an input (over-selection, not unsound).
  Files read through descriptors inherited from outside the traced tree, or
  served by a daemon outside it, are invisible.
- `ground trace` uses `PTRACE_TRACEME`, not `PTRACE_SEIZE`: a group-stop
  (`SIGSTOP`, `SIGTSTP`, `SIGTTIN`, `SIGTTOU` aimed at a task) is not told apart
  from a signal and the task is resumed. Test runners do not stop themselves.
- A path in tracee memory can change between the stop and the syscall.
- `manifest.command` is recorded for provenance but not compared; the adapter
  decides which command a test runs with.
- A toolchain or global file that was dirty in the working tree when the
  manifests were traced and has since been reverted is not seen as changed
  (the diff is against commits). Trace from a clean checkout.
- x32 and 32-bit syscalls fail the trace. aarch64 compiles from the same
  tables but has not been built here.

Adapters translate between the engine and a runner: vitest first (namzu), then
cargo-nextest, bun test, pytest, go test.
