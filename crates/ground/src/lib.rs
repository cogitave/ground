//! ground: a delivery service for any GitHub repository.
//!
//! The ports (`ports`) are the seam every lane is written against; `adapters`
//! hold the real implementations and `testing` the fakes.

pub mod adapters;
pub mod approval;
pub mod config;
pub mod daemon;
pub mod lanes;
pub mod ports;
pub mod state;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
