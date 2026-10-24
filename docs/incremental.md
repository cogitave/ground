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
- **Installed packages** are recorded as packages, not files. pnpm's store
  entry `node_modules/.pnpm/<name>@<version>[_<peers>]/node_modules/<name>`
  has a directory name that pins name, version and peer set, and pnpm verifies
  it against the lockfile's integrity, so a read of any file in it is recorded
  as `{dep: "<store dir>/<name>", store: "<path to .pnpm>"}` and nothing
  else. This also covers a read through a symlink to such a package (the
  `packages/x/node_modules/vitest` link, a store entry's dependency links): the
  link's target text stays a `link` record, so a retarget is seen, and the
  package it resolved to becomes the dep. What the name does not pin stays per
  file: a version that is not a registry semver (`file+`, `https+`, git store
  names), a link that does not resolve to exactly a package directory. Another
  layout (npm, yarn) keeps its files and gains the owning `package.json`'s
  hash. `deps_format` marks a manifest that records packages this way.
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
- **The lockfile is global only for older manifests.** Once every manifest has
  `deps_format` >= 1, `pnpm-lock.yaml` stops being a global glob: select asks,
  for each recorded package, whether its directory is still installed (a real
  directory holding a `package.json`; gone, replaced by a file or a link, or
  emptied selects the tests that recorded it). If a recorded store cannot be
  listed at all (no install, no permission) everything is selected, and a
  manifest without package records keeps the lockfile global. A test that read
  `pnpm-lock.yaml` itself has it as a hashed file input. Install must have run
  before select; the check trusts the directory name, not the files in it.
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
- `ground trace-tests --runner vitest --package <dir> [--jobs N] [--out <dir>]
  [--filter <glob>] [-- <wrapper...>]` records one manifest per test file of a
  package. It lists the files with the runner's own enumeration (`vitest list
  --filesOnly --json` with the package config), then traces each in its own
  `ground trace` process with **no runner discovery**: the run uses a generated
  config, kept outside the package, that imports the package's config (plugins,
  aliases, setup files behave as in a normal run) and sets `test.include` to
  the one literal file. vitest then walks no directories to find tests, so no
  manifest holds the ~60 listings that made every manifest stale whenever a
  file was added anywhere. Manifests land in `--out` under the store's own file
  names, so `ground select --manifests <dir>` (or `manifests import`) reads
  them directly. A wrapper after `--` runs in front of vitest for each test (a
  repository whose tests need an owned temp directory or home creates it
  there). A multi-project vitest config (`test.projects`) is refused rather
  than traced wrongly. A test that failed while traced keeps its manifest
  (select always picks it) and is reported; a test that could not be traced
  leaves none.
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
- `ground trace-tests --runner vitest --package <dir>` traces a whole package
  one test per process (see above).
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

**The dominant loss was directory inputs, and they are gone.** vitest
discovery listed ~60 `src` directories, so every manifest held those listing
hashes and any commit that added or removed a file anywhere in `src`
invalidated all of them: 34 of the 43 select-everything commits. 9 were global
globs (7 `pnpm-lock.yaml`). `trace-tests` (no discovery) and package-level
records (lockfile no longer global) remove both causes without ignoring
listings: a test that lists a directory itself still has that directory as an
input.

Re-measured with the same 60 commits and the same emulation harness, against
manifests traced with `trace-tests` (1000 files, 28 workers, 174 s wall, all
passing but one that timed out under that load and passes alone):

| rule | files mean | time mean | time median | commits selecting everything | <10% of time |
|---|---|---|---|---|---|
| before (strict, discovery traced) | 74% | 77% | 100% | 43 | 12 |
| now | 33% | 50% | 59% | 5 | 18 |

The 5 left: two bump `packages/sdk/package.json` (every test reads it) and
three change a `tsconfig` or `vite.config` of another package (still global).
The emulation cannot model a lockfile commit that changes a dependency's
version (the reinstall would replace the package directories and select its
dependents), so the 7 lockfile commits are optimistic. Real `ground select`
on the tip: unchanged tree 0 of 1000 (the one test that failed while traced is
always picked), a new file in `src` 0, `pnpm-lock.yaml` touched 0, one source
file edited 10.

Manifest size for the same 1000 tests: 59 MB total, p50 41 KB, max 223 KB
(was 215 MB, p50 192 KB, max 427 KB): no directory listings, packages as one
record each, and the store writes compact JSON.

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
- A package record trusts the pnpm directory name: a file edited by hand
  inside `node_modules/.pnpm/...`, or a different registry serving the same
  version, is not seen. `node_modules/.modules.yaml` and
  `.pnpm-workspace-state-v1.json` (a prune timestamp, the store location) are
  not recorded; the packages a test read are checked on their own.
- `trace-tests` keeps the test file's own directory as an input (vitest lists
  the parent of the literal include), so a file added next to a test selects
  that test only.
- x32 and 32-bit syscalls fail the trace. aarch64 compiles from the same
  tables but has not been built here.

Adapters translate between the engine and a runner: vitest first (namzu), then
cargo-nextest, bun test, pytest, go test.
