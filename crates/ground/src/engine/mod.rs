//! The incremental engine (`docs/incremental.md`): manifests, their store and
//! selection.

pub mod manifest;
pub mod select;
pub mod store;
pub mod trace;

/// The engine's own version. A manifest that records a different one selects
/// everything (the engine is a global input).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
