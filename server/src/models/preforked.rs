//! `preforked` — a fixed set of `cfg.workers` worker processes.
//!
//! Phase 0 audit bugs this fixes (§5.2):
//!   * children looped on `incoming()` forever, so the post-loop `break` was
//!     unreachable and children never exited → the parent's `waitpid` blocked
//!     forever;
//!   * a single shared listener fd → an `accept` thundering herd.
//!
//! Fix: each child calls `bind_listener(addr, reuse_port = true)`, so every
//! worker owns its own `SO_REUSEPORT` listener and the kernel load-balances
//! accepts — no shared fd, no thundering herd. Each child runs the blocking
//! accept loop, rechecking a shared shutdown flag every iteration. The parent
//! installs the shutdown handler before forking (children inherit it), idles
//! until signalled, forwards the signal to every child, then reaps them.
//!
//! Defining characteristic: near-linear multicore scaling, zero shared state,
//! zero lock contention; the cost is N independent accept queues — a connection
//! hashed to a busy worker cannot be stolen by an idle one (load imbalance under
//! uneven connection lifetimes).

use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use core::{bind_listener, App, Server, ServerConfig};

use super::blocking::serve_connection;
use crate::sys::{signal, socket};

/// Flipped by the SIGINT/SIGTERM handler. Inherited across `fork`, so each child
/// gets its own copy that its own handler sets when the signal is forwarded.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// How often the idle parent and the workers recheck the shutdown flag. Bounds
/// worst-case shutdown latency (`std::thread::sleep` resumes its full nap after
/// an EINTR, so this is also the parent's effective poll period).
const SHUTDOWN_POLL: Duration = Duration::from_millis(100);

pub struct Preforked {
    verbose: bool,
}

impl Preforked {
    pub fn new(verbose: bool) -> Self {
        Preforked { verbose }
    }
}

impl Server for Preforked {
    fn name(&self) -> &'static str {
        "preforked"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> std::io::Result<()> {
        // Install before forking so every child inherits the handler. No
        // SA_RESTART (see sys::signal), so a child blocked in accept() wakes
        // with EINTR and rechecks SHUTDOWN.
        signal::install_shutdown_flag(&SHUTDOWN);

        let workers = cfg.workers.max(1);
        let mut children = Vec::with_capacity(workers);
        for _ in 0..workers {
            match unsafe { libc::fork() } {
                -1 => {
                    let err = std::io::Error::last_os_error();
                    // Tear down the workers already spawned, then fail.
                    forward_signal(&children);
                    reap(&children);
                    return Err(err);
                }
                0 => {
                    // Child: if the parent dies (e.g. SIGKILL in teardown), take
                    // SIGTERM and run the same graceful exit path.
                    unsafe {
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong)
                    };
                    child_main(cfg, &app, self.verbose);
                    unsafe { libc::_exit(0) };
                }
                child_pid => children.push(child_pid),
            }
        }

        eprintln!("preforked: {workers} workers on http://{}", cfg.addr);

        // Parent idles until signalled. A short poll sidesteps the classic
        // pause()/signal race (a signal arriving just before pause would be lost).
        while !SHUTDOWN.load(Ordering::SeqCst) {
            std::thread::sleep(SHUTDOWN_POLL);
        }

        forward_signal(&children);
        reap(&children);
        Ok(())
    }
}

/// One worker: its own `SO_REUSEPORT` listener + the blocking accept loop.
fn child_main(cfg: &ServerConfig, app: &App, verbose: bool) {
    let listener = match bind_listener(cfg.addr, true) {
        Ok(l) => l,
        Err(e) => {
            if verbose {
                eprintln!("preforked worker: bind failed: {e}");
            }
            unsafe { libc::_exit(1) };
        }
    };
    // Bound the idle accept wait so the loop polls SHUTDOWN even with no traffic.
    // Without this the worker would park in accept() forever (std retries the
    // signal's EINTR), and the parent's waitpid would hang on shutdown.
    if let Err(e) = socket::set_accept_timeout(listener.as_raw_fd(), SHUTDOWN_POLL) {
        if verbose {
            eprintln!("preforked worker: set_accept_timeout failed: {e}");
        }
        unsafe { libc::_exit(1) };
    }

    while !SHUTDOWN.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                app.metrics().inc_connections();
                if let Err(e) = serve_connection(stream, cfg, app) {
                    app.metrics().inc_errors();
                    if verbose {
                        eprintln!("preforked worker: connection error: {e}");
                    }
                }
            }
            // SO_RCVTIMEO wakeup (WouldBlock) or a signal (Interrupted): loop
            // back and recheck SHUTDOWN. Neither is a real error.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) || e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                if verbose {
                    eprintln!("preforked worker: accept error: {e}");
                }
            }
        }
    }
}

/// Forward the shutdown signal to each child by PID. Deliberately *not*
/// `kill(0, …)` (the whole process group): the server may share a group with a
/// test runner or shell, which must not be signalled. A child that already
/// caught the terminal's signal directly yields `ESRCH`, which is harmless.
fn forward_signal(children: &[libc::pid_t]) {
    for &pid in children {
        unsafe { libc::kill(pid, libc::SIGTERM) };
    }
}

/// Reap every child so none is left a zombie.
fn reap(children: &[libc::pid_t]) {
    for &pid in children {
        let mut status: libc::c_int = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
    }
}
