# AGENTS.md

Instructions for AI agents working in this repository. `CLAUDE.md` imports this file.

## Project

ground is a general-purpose delivery service for any GitHub repository: merge,
trunk validation, incremental CI and release, run locally by whoever uses it.
Nothing in it may assume a particular organisation or project; namzu is only the
first adopter. Read `docs/design.md` before any non-trivial change;
its "Hard rules" are invariants, not preferences.

## Build & Test

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
```

A test must not decide its outcome by racing real time. Time is the `Clock`
port; drive it with fakes. No test touches the network, GitHub or the npm
registry; use the fake ports.

## Hard rules

- Required author: `bahadirarda <bahadirarda@users.noreply.github.com>`. Verify with `git config --show-origin user.email` before committing.
- Conventional Commits: `<type>(<scope>): <subject>`. No AI co-author trailers.
- Never push without explicit approval from the owner. Approval for one push does not extend to the next.
- Never run destructive git operations without approval.
- Stage explicit paths. Never `git add -A` or `git add .`.
- Never run ground against a real repository with `--confirm` during development; use the fakes.
