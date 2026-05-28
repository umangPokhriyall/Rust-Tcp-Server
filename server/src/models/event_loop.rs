//! `event-loop` — the `Reactor`-based model (§5.7).
//!
//! Thin wrapper: bind the listener, install the SIGINT/SIGTERM flag, construct
//! a [`crate::reactor::Reactor`], and run it. Every event-loop concern —
//! epoll-ET, drain discipline, timeout enforcement, connection-cap
//! backpressure, read-buffer reuse — lives in `reactor.rs`. **Defining
//! characteristic:** the production-shaped reactor packaged as a reusable
//! struct. Benchmarked against bare `epoll-et`, it answers: does adding
//! production concerns (timeout sweeps + backpressure + buffer reuse) cost
//! latency? A near-zero delta is the expected, reportable result. This is the
//! same `Reactor` that Phase 2's `multireactor` will instantiate N times.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use core::{bind_listener, App, Server, ServerConfig};

use crate::reactor::Reactor;
use crate::sys::signal;

/// Flipped by the SIGINT/SIGTERM handler. The `Reactor` checks `&AtomicBool`
/// itself, so we just hand it a reference to this static; that same indirection
/// is what lets `multireactor` (Phase 2) share one shutdown flag across many
/// reactor threads.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub struct EventLoop {
    verbose: bool,
}

impl EventLoop {
    pub fn new(verbose: bool) -> Self {
        EventLoop { verbose }
    }
}

impl Server for EventLoop {
    fn name(&self) -> &'static str {
        "event-loop"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        signal::install_shutdown_flag(&SHUTDOWN);
        // Reset on re-entry — a previous run in the same process (e.g. tests)
        // may have left it raised. Without this the second `serve` would exit
        // before the first iteration of `epoll_wait`.
        SHUTDOWN.store(false, Ordering::SeqCst);

        let listener = bind_listener(cfg.addr, false)?;
        eprintln!("event-loop: listening on http://{}", cfg.addr);

        let owned_cfg = crate::models::shared_config(cfg);
        // `Reactor` owns its config; clone the fields out of the `Arc` so the
        // reactor does not need to be parametrized over a borrow.
        let cfg_for_reactor = ServerConfig {
            addr: owned_cfg.addr,
            workers: owned_cfg.workers,
            read_timeout: owned_cfg.read_timeout,
            write_timeout: owned_cfg.write_timeout,
            max_connections: owned_cfg.max_connections,
            assets_dir: owned_cfg.assets_dir.clone(),
        };

        let mut reactor = Reactor::new(listener, cfg_for_reactor, "event-loop", self.verbose)?;
        reactor.run(&SHUTDOWN, &app)
    }
}
