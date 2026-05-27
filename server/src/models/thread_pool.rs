//! `thread-pool` — a fixed bounded pool of worker threads.
//!
//! State this replaces (§5.4): the old `server/lib.rs` `ThreadPool` was dead
//! code; this is a fresh build in the Phase 1 architecture.
//!
//! Design: `cfg.workers` worker threads are spawned at startup. The job queue is
//! a **bounded** `std::sync::mpsc::sync_channel(capacity)` carrying accepted
//! `TcpStream`s. The acceptor thread `try_send`s each stream; workers `recv` and
//! run the shared blocking skeleton. A single `Receiver` is shared behind a
//! `Mutex` (mpsc is single-consumer) — a worker locks only long enough to pull
//! one job, then serves it with the lock released, so the pool drains in
//! parallel.
//!
//! Backpressure (explicit, documented): on `TrySendError::Full` the acceptor
//! closes the connection immediately — a fast reject under overload, never a
//! blocked acceptor. Graceful shutdown: drop the `SyncSender` → every worker's
//! `recv` returns `Err(Disconnected)` → workers exit.
//!
//! Defining characteristic: bounded, predictable resource use with explicit
//! fast-reject backpressure; the ceiling is `workers` concurrent slow requests
//! — `workers` slow clients cause head-of-line blocking behind the queue.

use std::net::TcpStream;
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use core::{bind_listener, App, Server, ServerConfig};

use super::blocking::serve_connection;
use super::shared_config;

/// Depth of the bounded job queue. Sized to the connection cap so the pool can
/// hold a full cap's worth of accepted-but-unstarted work before fast-rejecting.
fn queue_capacity(cfg: &ServerConfig) -> usize {
    cfg.max_connections.max(1)
}

pub struct ThreadPool {
    verbose: bool,
}

impl ThreadPool {
    pub fn new(verbose: bool) -> Self {
        ThreadPool { verbose }
    }
}

impl Server for ThreadPool {
    fn name(&self) -> &'static str {
        "thread-pool"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> std::io::Result<()> {
        let listener = bind_listener(cfg.addr, false)?;
        let workers = cfg.workers.max(1);
        eprintln!("thread-pool: {workers} workers on http://{}", cfg.addr);

        let cfg = shared_config(cfg);
        let (tx, rx) = sync_channel::<TcpStream>(queue_capacity(&cfg));
        let rx = Arc::new(Mutex::new(rx));

        for _ in 0..workers {
            let rx = Arc::clone(&rx);
            let app = Arc::clone(&app);
            let cfg = Arc::clone(&cfg);
            let verbose = self.verbose;
            // Detached: worker lifetime is the process lifetime. On shutdown the
            // dropped sender disconnects the channel and each worker exits.
            thread::spawn(move || worker_loop(&rx, &cfg, &app, verbose));
        }

        loop {
            let (stream, _peer) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) => {
                    if self.verbose {
                        eprintln!("thread-pool: accept error: {e}");
                    }
                    continue;
                }
            };
            app.metrics().inc_connections();

            match tx.try_send(stream) {
                Ok(()) => {}
                // Queue full: reject fast — close the connection now rather than
                // block the acceptor behind a saturated pool.
                Err(TrySendError::Full(stream)) => {
                    app.metrics().inc_errors();
                    drop(stream);
                }
                // Every worker is gone; the pool can serve nothing more.
                Err(TrySendError::Disconnected(_)) => {
                    return Ok(());
                }
            }
        }
    }
}

/// One worker: pull a stream from the shared queue and serve it to completion.
/// The lock is held only across the `recv` — released before serving, so other
/// workers pull concurrently. A disconnected channel (sender dropped) ends the
/// loop.
fn worker_loop(rx: &Mutex<Receiver<TcpStream>>, cfg: &ServerConfig, app: &App, verbose: bool) {
    loop {
        let stream = {
            let rx = rx.lock().unwrap();
            match rx.recv() {
                Ok(stream) => stream,
                Err(_) => break, // sender dropped → shut down
            }
        };
        if let Err(e) = serve_connection(stream, cfg, app) {
            app.metrics().inc_errors();
            if verbose {
                eprintln!("thread-pool: connection error: {e}");
            }
        }
    }
}
