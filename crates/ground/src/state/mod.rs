//! Durable state: ship sessions, events and the flake ledger, in SQLite.

mod store;

pub use store::{
    Flake, NewShipSession, ShipSession, ShipSessionPatch, StateError, StateStore, StoredEvent,
    default_db_path,
};
