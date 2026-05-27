//! One module per concurrency model, each implementing `core::Server`, plus the
//! shared blocking serve loop they reuse.
//!
//! Implemented so far: `iterative` (Phase 0), `forking`, `preforked`. The
//! remaining models arrive in later Phase 1 sessions — do not implement them
//! ahead of their session.

pub mod blocking;
pub mod forking;
pub mod iterative;
pub mod preforked;
