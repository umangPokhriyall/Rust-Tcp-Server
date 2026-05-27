//! Minimal counters for Phase 0. Latency histograms are NOT in Phase 0 (the
//! load generator measures latency client-side) — do not add them here.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct Metrics {
    pub connections: AtomicU64,
    pub requests: AtomicU64,
    pub errors: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Metrics::default()
    }

    pub fn inc_connections(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_requests(&self) {
        self.requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_increment() {
        let m = Metrics::new();
        assert_eq!(m.connections.load(Ordering::Relaxed), 0);
        assert_eq!(m.requests.load(Ordering::Relaxed), 0);
        assert_eq!(m.errors.load(Ordering::Relaxed), 0);

        m.inc_connections();
        m.inc_requests();
        m.inc_requests();
        m.inc_errors();

        assert_eq!(m.connections.load(Ordering::Relaxed), 1);
        assert_eq!(m.requests.load(Ordering::Relaxed), 2);
        assert_eq!(m.errors.load(Ordering::Relaxed), 1);
    }
}
