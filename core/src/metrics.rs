//! Minimal counters for Phase 0. Latency histograms are NOT in Phase 0 (the
//! load generator measures latency client-side) — do not add them here.

use std::sync::atomic::AtomicU64;

#[derive(Default)]
pub struct Metrics {
    pub connections: AtomicU64,
    pub requests: AtomicU64,
    pub errors: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        todo!()
    }

    pub fn inc_connections(&self) {
        todo!()
    }

    pub fn inc_requests(&self) {
        todo!()
    }

    pub fn inc_errors(&self) {
        todo!()
    }
}
