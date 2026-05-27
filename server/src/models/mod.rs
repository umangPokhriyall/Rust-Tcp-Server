//! One module per concurrency model, each implementing `core::Server`.
//!
//! Phase 0 wires only `iterative`. Other models belong to later phases — do not
//! implement them here.

pub mod iterative;
