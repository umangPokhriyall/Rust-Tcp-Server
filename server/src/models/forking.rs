//! `forking` — one child process per connection.
//!
//! Phase 0 audit bugs this fixes (§5.1):
//!   * no `waitpid` → zombies pile up until `fork()` fails with `EAGAIN`;
//!   * no cap → a connection flood becomes a fork bomb.
//!
//! Fix: a `sys::signal` SIGCHLD reaper keeps a live-child `AtomicUsize` current,
//! and the accept loop refuses to fork past `cfg.max_connections` (reject fast
//! by closing the just-accepted socket). On fork the child drops the listener,
//! serves exactly one connection with the shared blocking loop, and `_exit`s;
//! the parent closes its copy of the socket and loops.
//!
//! Defining characteristic: bulletproof process isolation per connection; the
//! `fork()` page-table COW setup dominates cost, so it collapses under
//! connection churn.

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use core::{bind_listener, App, Server, ServerConfig};

use super::blocking::serve_connection;
use crate::sys::signal;

/// Live child processes. The SIGCHLD reaper decrements this as each child is
/// reaped; the accept loop reads it to enforce the connection cap.
static LIVE_CHILDREN: AtomicUsize = AtomicUsize::new(0);

pub struct Forking {
    verbose: bool,
}

impl Forking {
    pub fn new(verbose: bool) -> Self {
        Forking { verbose }
    }
}

impl Server for Forking {
    fn name(&self) -> &'static str {
        "forking"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> std::io::Result<()> {
        signal::install_sigchld_reaper(&LIVE_CHILDREN);

        let listener = bind_listener(cfg.addr, false)?;
        let listener_fd = listener.as_raw_fd();
        eprintln!("forking: listening on http://{}", cfg.addr);

        loop {
            let (stream, _peer) = match listener.accept() {
                Ok(pair) => pair,
                // SA_RESTART means SIGCHLD should not surface as EINTR, but stay
                // defensive: a transient accept error must never kill the server.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.verbose {
                        eprintln!("forking: accept error: {e}");
                    }
                    continue;
                }
            };
            app.metrics().inc_connections();

            // Backpressure: at the cap, reject fast — close immediately rather
            // than fork past the ceiling.
            if LIVE_CHILDREN.load(Ordering::SeqCst) >= cfg.max_connections {
                app.metrics().inc_errors();
                drop(stream);
                continue;
            }

            // Reserve the slot *before* forking: if the child exits so fast the
            // SIGCHLD reaper runs before we could increment, the counter would
            // underflow.
            LIVE_CHILDREN.fetch_add(1, Ordering::SeqCst);

            match unsafe { libc::fork() } {
                -1 => {
                    // Fork failed: release the reservation and reject this one.
                    LIVE_CHILDREN.fetch_sub(1, Ordering::SeqCst);
                    app.metrics().inc_errors();
                    if self.verbose {
                        eprintln!("forking: fork failed: {}", std::io::Error::last_os_error());
                    }
                    drop(stream);
                }
                0 => {
                    // Child. If the parent dies, die with it (the parent may be
                    // SIGKILLed in tests/teardown, which it cannot forward).
                    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) };
                    unsafe { libc::close(listener_fd) }; // drop our listener copy
                    let _ = serve_connection(stream, cfg, &app);
                    // `_exit`: skip atexit hooks / destructors so we never flush
                    // buffers the parent still owns.
                    unsafe { libc::_exit(0) };
                }
                _child_pid => {
                    // Parent. The child owns the connection now; close our copy
                    // of the fd so descriptors do not accumulate.
                    drop(stream);
                }
            }
        }
    }
}
