# Rust port

ground is written in Rust. M1 was first built in TypeScript (107 tests); the
Rust crate reached parity with every one of those scenarios
(`docs/rust-port-parity.md`) and the TypeScript code was then removed. Its
history is in git before the commit that removed it.

## Layout

```
Cargo.toml                 workspace
rust-toolchain.toml        pinned stable toolchain
crates/ground/             package `ground-cli` (lib + bins `ground`, `groundd`)
  src/lib.rs
  src/ports.rs             GitHubPort, RegistryPort, Clock + the data types
  src/config.rs            RepoConfig: serde, deny_unknown_fields, validation
  src/approval.rs          can_approve_run — the single hard-rule-4 predicate
  src/state/               SQLite store (rusqlite, bundled)
  src/adapters/            GitHub REST (reqwest), npm registry, system clock, gh token
  src/lanes/release/       ship state machine
  src/daemon/              watcher loops, unix socket server
  src/cli/                 command implementations
  src/testing/             fakes, behind the `testing` feature and cfg(test)
  src/bin/ground.rs, src/bin/groundd.rs
```

The crates.io name `ground` belongs to another project, so the package is
`ground-cli`; the binaries are still `ground` and `groundd`.

## Choices

- Edition 2024, `rust-toolchain.toml` pinned to the stable toolchain on the
  build machine.
- `tokio` for the daemon, polling and the socket. Ports are traits with
  `async fn`, used through generics (static dispatch), so no `async-trait`.
- `reqwest` (rustls) for the ~11 GitHub REST endpoints ground needs, instead of
  a full SDK. Admin merge shells out to `gh pr merge --admin --match-head-commit`
  because ruleset bypass is proven to work through `gh`.
- Errors from the ports distinguish `PortError::Transient` (a failed GET at the
  transport level, or a GET answered 5xx/429; its message carries the
  `source()` chain) from permanent ones. Only the adapters, which know the
  method, produce it, and never for a write. The ship wait loops treat it as an
  unanswered poll.
- `rusqlite` with the `bundled` feature: no system SQLite dependency. Busy
  timeout 5 s, WAL, directory 0700, database file 0600.
- `clap` (derive) for the CLI, `serde`/`serde_json` for config and the socket
  protocol, `thiserror` for library errors, `anyhow` only in the binaries,
  `tracing` for logs.
- Every dependency is maintained and MIT/Apache-2.0 compatible (checked by
  hand when added; an automated `cargo deny` gate is not wired yet).

## Gates

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

No test touches the network, GitHub, npm or real time: the `Clock` port is
faked, and waits advance virtual time.

## Parity

Every scenario covered by the TypeScript tests has a Rust test with the same
intent. `docs/rust-port-parity.md` maps each TS test to its Rust counterpart;
a TS scenario with no counterpart is a gap, not a choice.
