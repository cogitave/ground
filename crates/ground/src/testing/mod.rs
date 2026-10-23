//! Fakes for every port. Compiled under `cfg(test)` and the `testing` feature.
//!
//! All fakes use interior mutability and are cheap to `Clone` (clones share
//! state), so a test can keep a handle while a lane owns another.

mod fake_clock;
mod fake_github;
mod fake_registry;
mod memory_logger;
mod recording_exec;

pub use fake_clock::FakeClock;
pub use fake_github::{FakeGitHub, RunScript, RunStage, WriteCall, WriteMethod, make_pr, make_run};
pub use fake_registry::FakeRegistry;
pub use memory_logger::{LogEntry, MemoryLogger};
pub use recording_exec::RecordingExec;

#[cfg(test)]
mod tests;
