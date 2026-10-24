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
  its process tree opened for reading during a passing run, plus the toolchain
  fingerprint (runtime version, package-manager lockfile entries it resolved,
  the test runner's config files) and a declared environment allow-list.
- **Selection** for a change: a test runs when it has no manifest, when the test
  file itself changed, when any file in its manifest now hashes differently
  (build outputs are hashed after the build, so a `dist/` file that did not
  change does not select anything), or when the toolchain fingerprint changed.
  Global inputs (lockfile, root config, CI workflow, the engine's own version)
  select everything.
- **Gates** other than tests (typecheck, build, pack, publint) are keyed the
  same way, per package, from their declared inputs plus the outputs of the
  packages they depend on.
- **Fail closed**: a missing, unreadable or stale manifest selects the test. An
  error anywhere selects everything.

## Where manifests come from

- A full run records them: `ground trace -- <runner> <test file>` runs one test
  file under the tracer (Linux: `strace -f -e trace=openat,open,execve` or a
  seccomp-notify tracer) and writes its manifest.
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

- `ground select --base <ref> [--format vitest|nextest|json]` prints the tests
  to run (and `--all` when a global input changed).
- `ground trace -- <command>` records a manifest for one test invocation.
- `ground manifests pull|push` moves manifests between the local store and a CI
  artifact.

Adapters translate between the engine and a runner: vitest first (namzu), then
cargo-nextest, bun test, pytest, go test.
