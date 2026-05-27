//! `thread-per-conn` — one detached OS thread per connection.
//!
//! Phase 0 audit bugs this fixes (§5.3):
//!   * a confused fork+thread hybrid — drop the fork, this is one process;
//!   * `JoinHandle`s pushed inside the infinite accept loop and never joined →
//!     unbounded memory growth;
//!   * unbounded `thread::spawn` → a connection flood is a thread bomb, each
//!     thread reserving ~8 MiB of virtual stack.
//!
//! Fix: a single process whose accept loop spawns one *detached* `std::thread`
//! per connection (the `JoinHandle` is dropped, never retained). Concurrency is
//! capped by a counting semaphore — `std` has no `Semaphore`, so it is built
//! from `Arc<(Mutex<usize>, Condvar)>`: the accept loop acquires a permit before
//! spawning and the worker releases it when the connection finishes. At the cap
//! the accept loop blocks on the `Condvar` rather than spawning, so the kernel
//! accept backlog absorbs the overflow — observable backpressure, not a bomb.
//!
//! Defining characteristic: the simplest correct concurrency — the kernel
//! scheduler does the multiplexing. Per-thread stack and context-switch cost
//! make it the model that visibly degrades at C10K; it exists to motivate the
//! event loop.

use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use core::{bind_listener, App, Server, ServerConfig};

use super::blocking::serve_connection;
use super::shared_config;

/// A counting semaphore over a permit count. `acquire` blocks on the `Condvar`
/// while no permit is free; `release` returns one and wakes a single waiter.
struct Semaphore {
    permits: Mutex<usize>,
    available: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Self {
        Semaphore {
            permits: Mutex::new(permits),
            available: Condvar::new(),
        }
    }

    /// Take a permit, blocking until one is free.
    fn acquire(&self) {
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.available.wait(permits).unwrap();
        }
        *permits -= 1;
    }

    /// Return a permit and wake one waiter (if any).
    fn release(&self) {
        let mut permits = self.permits.lock().unwrap();
        *permits += 1;
        self.available.notify_one();
    }
}

pub struct ThreadPerConn {
    verbose: bool,
}

impl ThreadPerConn {
    pub fn new(verbose: bool) -> Self {
        ThreadPerConn { verbose }
    }
}

impl Server for ThreadPerConn {
    fn name(&self) -> &'static str {
        "thread-per-conn"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> std::io::Result<()> {
        let listener = bind_listener(cfg.addr, false)?;
        eprintln!("thread-per-conn: listening on http://{}", cfg.addr);

        let cfg = shared_config(cfg);
        let sem = Arc::new(Semaphore::new(cfg.max_connections.max(1)));

        loop {
            let (stream, _peer) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) => {
                    // An accept() failure must never kill the server.
                    if self.verbose {
                        eprintln!("thread-per-conn: accept error: {e}");
                    }
                    continue;
                }
            };
            app.metrics().inc_connections();

            // Backpressure: at the cap, block here until a worker frees a permit.
            // The kernel accept backlog absorbs new connections meanwhile.
            sem.acquire();

            let sem = Arc::clone(&sem);
            let app = Arc::clone(&app);
            let cfg = Arc::clone(&cfg);
            let verbose = self.verbose;

            // Detached: the handle is dropped, never retained — no unbounded
            // `Vec<JoinHandle>`. The permit is released on every exit path.
            thread::spawn(move || {
                if let Err(e) = serve_connection(stream, &cfg, &app) {
                    app.metrics().inc_errors();
                    if verbose {
                        eprintln!("thread-per-conn: connection error: {e}");
                    }
                }
                sem.release();
            });
        }
    }
}
