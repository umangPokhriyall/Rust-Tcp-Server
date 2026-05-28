//! `multireactor` — shared-nothing, `SO_REUSEPORT`, pinned reactors (§4).
//!
//! `cfg.workers` reactor threads. Each worker:
//!
//!   1. pins itself to logical core `i` via [`affinity::pin_to_core`]
//!      (warn-and-continue if `i >= num_cores()`),
//!   2. binds its own `SO_REUSEPORT` listener via [`bind_listener`]`(addr, true)`,
//!   3. builds a [`Reactor`] (Phase 1 §4) and runs it until the shared
//!      shutdown flag flips.
//!
//! No acceptor thread, no fd handoff, no shared state on the hot path. The
//! kernel hashes incoming 4-tuples and delivers each accept to exactly one of
//! the per-worker listeners — `SO_REUSEPORT` makes the thundering herd of a
//! single shared listener fd impossible by construction.
//!
//! **Defining characteristic (§4):** near-linear multicore scaling, zero shared
//! state, zero hot-path contention. **Caveat:** kernel-hash 4-tuple balancing
//! imbalances under skewed connection lifetimes, and there is no work-stealing
//! between reactors — the same tradeoff `preforked` accepts at the process
//! level.
//!
//! **Why not "one acceptor + N reactors":** `SO_REUSEPORT` supersedes it.
//! Shared-nothing reactors eliminate both the shared listener fd and the
//! acceptor→reactor fd-handoff path. The rejected alternative is recorded in
//! `docs/BENCHMARKS.md` §9 and `docs/ARCHITECTURE.md` §11 (Phase 2 sessions
//! 5–6).

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use core::{bind_listener, App, Server, ServerConfig};

use crate::reactor::Reactor;
use crate::sys::{affinity, signal};

/// Shared shutdown flag — one static so a single SIGINT/SIGTERM lifts every
/// reactor at once. `Reactor::run` takes `&AtomicBool`, which is the same
/// indirection `event-loop` uses; here the indirection earns its keep by
/// letting N reactor threads observe one source of truth without any per-thread
/// signal handler.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

pub struct Multireactor {
    verbose: bool,
}

impl Multireactor {
    pub fn new(verbose: bool) -> Self {
        Multireactor { verbose }
    }
}

impl Server for Multireactor {
    fn name(&self) -> &'static str {
        "multireactor"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        signal::install_shutdown_flag(&SHUTDOWN);
        // Reset on re-entry — a previous run in the same process (e.g. tests)
        // may have left it raised. Without this the second `serve` would exit
        // before any reactor's first `epoll_wait`.
        SHUTDOWN.store(false, Ordering::SeqCst);

        let workers = cfg.workers.max(1);
        let owned_cfg = crate::models::shared_config(cfg);
        let verbose = self.verbose;

        eprintln!(
            "multireactor: {workers} reactors on http://{} (cores={})",
            cfg.addr,
            affinity::num_cores()
        );

        let mut handles = Vec::with_capacity(workers);
        for i in 0..workers {
            let app = Arc::clone(&app);
            let cfg = Arc::clone(&owned_cfg);
            let handle = thread::Builder::new()
                .name(format!("multireactor-{i}"))
                .spawn(move || -> io::Result<()> {
                    // Pin first so the listener bind, the epoll fd, and every
                    // page touched by this reactor land on the chosen core.
                    if let Err(e) = affinity::pin_to_core(i) {
                        if verbose {
                            eprintln!("multireactor[{i}]: pin_to_core({i}) failed: {e}");
                        }
                    }
                    // Each reactor owns its own SO_REUSEPORT listener — the
                    // kernel load-balances accepts across the set, no shared fd.
                    let listener = bind_listener(cfg.addr, true)?;
                    let reactor_cfg = ServerConfig {
                        addr: cfg.addr,
                        workers: cfg.workers,
                        read_timeout: cfg.read_timeout,
                        write_timeout: cfg.write_timeout,
                        max_connections: cfg.max_connections,
                        assets_dir: cfg.assets_dir.clone(),
                    };
                    let mut reactor =
                        Reactor::new(listener, reactor_cfg, "multireactor", verbose)?;
                    reactor.run(&SHUTDOWN, &app)
                })?;
            handles.push(handle);
        }

        // Join every worker. A reactor can exit on an `io::Error` or panic; we
        // surface both through stderr but do not let one bad worker abort the
        // others — the rest still have live connections to drain.
        let mut first_err: Option<io::Error> = None;
        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    if verbose {
                        eprintln!("multireactor: worker error: {e}");
                    }
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
                Err(_) => {
                    if verbose {
                        eprintln!("multireactor: worker panicked");
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}
