//! `Reactor` — the production-shaped event loop assembly.
//!
//! `epoll-et` (§5.6) implemented as a reusable building block: one
//! edge-triggered `epoll` fd, a [`ConnTable`] of live connections, idle-timeout
//! enforcement, connection-cap backpressure, and a single read buffer that is
//! reused across every event for the lifetime of the loop.
//!
//! The `event-loop` model (§5.7) is a thin user of this struct — it just binds
//! a listener, constructs a `Reactor`, and calls [`Reactor::run`]. Phase 2's
//! `multireactor` will instantiate the same struct N times, one per worker
//! thread, each owning a `SO_REUSEPORT` listener. Putting the assembly here
//! (instead of duplicating it in two model files) is the §1.1 layering rule:
//! one abstraction, many implementations.
//!
//! ## What differs from `models::epoll`
//!
//! Functionally the runtime behaviour matches `EpollEt`: edge-triggered, drain
//! to `EAGAIN`, the same `drive_io` per-connection routine. The differences are
//! shape, not semantics:
//!
//! * State is a `struct` rather than locals on `run_epoll`'s stack. Phase 2's
//!   multireactor will hand one `Reactor` per worker thread, so it has to be
//!   constructible without a model crate.
//! * The read buffer lives on the struct (`read_buf: Vec<u8>`), reused across
//!   every event — buffer reuse is called out explicitly in §4 as a Reactor
//!   characteristic.
//! * Shutdown is passed in by the caller (`&AtomicBool`), not held as a static.
//!   The event-loop model still owns the SIGINT/SIGTERM installer, but the
//!   Reactor itself is signal-agnostic — that is what lets multireactor wire
//!   one flag across many reactors.
//!
//! ## Backpressure policy (§4)
//!
//! At `cfg.max_connections` the listener is `epoll_ctl(DEL)`'d, so the kernel
//! accept backlog absorbs new connections and ultimately refuses them — a
//! deliberate, observable shed rather than uncontrolled memory growth. The
//! listener is re-added when `conns.len()` falls below the cap.
//!
//! ## Expiry sweep
//!
//! Each iteration scans `ConnTable` for connections past their read deadline
//! and drops them. `core::Connection` does not expose its deadline (the API is
//! frozen in Phase 1), so the wait cap [`TICK`] bounds how stale that sweep can
//! be. 100 ms is small relative to the default 30 s read timeout — small
//! enough that an idle slow-loris client is closed promptly, large enough that
//! the no-activity wakeup rate stays in single digits per second.

use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use core::limits::READ_CHUNK;
use core::{App, ConnAction, Connection, ServerConfig};

use crate::models::event_io::drive_io;
use crate::sys::conn_table::ConnTable;
use crate::sys::epoll::{Epoll, Event, Interest, Trigger};
use crate::sys::socket;

/// `epoll_wait` cap. Bounds shutdown latency *and* the staleness of the
/// expired-connection sweep — `core::Connection` does not expose its deadline,
/// so we cannot wake exactly on the earliest expiry. 100 ms is small relative
/// to the default 30 s timeouts.
const TICK: Duration = Duration::from_millis(100);

/// Events buffer capacity per `epoll_wait`. Higher reduces syscall count under
/// burst load at the cost of a larger struct.
const MAX_EVENTS: usize = 1024;

/// The production-shaped reactor: epoll-ET + timeout enforcement + connection
/// cap + read-buffer reuse. The `event-loop` model is a thin user of this; the
/// Phase 2 `multireactor` will instantiate one per worker thread.
pub struct Reactor {
    epoll: Epoll,
    conns: ConnTable,
    listener: TcpListener,
    cfg: ServerConfig,
    /// Reused across every read — never reallocated per event.
    read_buf: Vec<u8>,
    /// Currently registered interest per fd. We only issue `epoll_ctl(MOD)`
    /// when the *desired* interest actually changes, so the keep-alive hot path
    /// (read -> respond -> read again, all `Read` interest after the response
    /// drains) avoids the syscall entirely.
    wants: HashMap<RawFd, Interest>,
    /// Pre-allocated events buffer for `epoll_wait` — kept on the struct so the
    /// capacity is paid for once at construction.
    events: Vec<Event>,
    /// True while the listener fd is in epoll. Flipped under backpressure.
    listener_registered: bool,
    /// Label for verbose error messages (`"event-loop"`, etc.).
    label: &'static str,
    verbose: bool,
}

impl Reactor {
    /// Build the reactor around an already-bound listener. Sets the listener
    /// non-blocking and registers it edge-triggered for `Read`. The caller owns
    /// the binding choice (`reuse_port` true/false) so multireactor can pass a
    /// per-thread listener.
    pub fn new(
        listener: TcpListener,
        cfg: ServerConfig,
        label: &'static str,
        verbose: bool,
    ) -> io::Result<Self> {
        let listener_fd = listener.as_raw_fd();
        socket::set_nonblocking(listener_fd)?;
        let epoll = Epoll::new()?;
        epoll.add(listener_fd, Interest::Read, Trigger::Edge)?;
        Ok(Reactor {
            epoll,
            conns: ConnTable::new(),
            listener,
            cfg,
            read_buf: vec![0u8; READ_CHUNK],
            wants: HashMap::new(),
            events: Vec::with_capacity(MAX_EVENTS),
            listener_registered: true,
            label,
            verbose,
        })
    }

    /// Run the event loop until `shutdown` is set. Each iteration:
    ///   1. `epoll_wait` for up to [`TICK`] (the cap on time-to-next-deadline).
    ///   2. Drain the listener (accept until `EAGAIN`) on a listener event.
    ///   3. Drive each ready connection through `drive_io`, applying the
    ///      returned [`ConnAction`] via `epoll_ctl(MOD)` or remove on `Close`.
    ///   4. Sweep expired connections.
    ///   5. Toggle listener registration to apply the connection cap.
    pub fn run(&mut self, shutdown: &AtomicBool, app: &App) -> io::Result<()> {
        let listener_fd = self.listener.as_raw_fd();

        while !shutdown.load(Ordering::SeqCst) {
            match self.epoll.wait(&mut self.events, Some(TICK)) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.verbose {
                        eprintln!("{}: epoll_wait error: {e}", self.label);
                    }
                    continue;
                }
            }

            // Snapshot the events as plain tuples — `self.events` is borrowed
            // by `epoll.wait` each iteration; copying lets the per-fd handlers
            // take `&mut self` without aliasing the events buffer.
            let ready: Vec<(RawFd, bool, bool, bool, bool)> = self
                .events
                .iter()
                .map(|e| (e.fd, e.readable, e.writable, e.hup, e.error))
                .collect();

            for (fd, readable, writable, hup, err) in ready {
                if fd == listener_fd {
                    if readable {
                        self.accept_ready(app);
                    }
                    continue;
                }
                // HUP and ERR are terminal — drop the connection. A pending
                // read would be reported here too, but `drive_io` would just
                // observe `read() == 0` or a hard error on its next attempt.
                if err || hup {
                    self.drop_conn(fd);
                    continue;
                }
                if readable || writable {
                    self.drive(fd, app);
                }
            }

            self.apply_backpressure();
            self.sweep_expired();
        }
        Ok(())
    }

    /// Listener ready — ET discipline: accept until `EAGAIN`. Each new fd lands
    /// in `conns` registered for `Read`+`Edge`. Stops mid-drain if the cap is
    /// reached; the outer `apply_backpressure` will then deregister the
    /// listener until capacity frees.
    fn accept_ready(&mut self, app: &App) {
        let listener_fd = self.listener.as_raw_fd();
        loop {
            if self.conns.len() >= self.cfg.max_connections {
                return;
            }
            match socket::accept_nonblocking(listener_fd) {
                Ok(Some((fd, _peer))) => {
                    // `accept4(SOCK_NONBLOCK)` already gave us a non-blocking
                    // fd — defending anyway means a future change to that
                    // helper cannot silently regress us into blocking sockets.
                    if let Err(e) = socket::set_nonblocking(fd) {
                        if self.verbose {
                            eprintln!("{}: set_nonblocking failed: {e}", self.label);
                        }
                        unsafe { libc::close(fd) };
                        continue;
                    }
                    // SAFETY: accept4 returned an owned fd; from_raw_fd takes it.
                    let stream = unsafe { TcpStream::from_raw_fd(fd) };
                    let conn = Connection::new(self.cfg.read_timeout);
                    let fd = self.conns.insert(stream, conn);
                    if let Err(e) = self.epoll.add(fd, Interest::Read, Trigger::Edge) {
                        if self.verbose {
                            eprintln!("{}: epoll add failed: {e}", self.label);
                        }
                        self.conns.remove(fd);
                        continue;
                    }
                    self.wants.insert(fd, Interest::Read);
                    app.metrics().inc_connections();
                }
                Ok(None) => return, // EAGAIN — backlog drained.
                Err(e) => {
                    if self.verbose {
                        eprintln!("{}: accept error: {e}", self.label);
                    }
                    return;
                }
            }
        }
    }

    /// Drive one ready connection through [`drive_io`], then `epoll_ctl(MOD)`
    /// to the new interest if it changed, or remove on `Close`.
    fn drive(&mut self, fd: RawFd, app: &App) {
        let slot = match self.conns.get_mut(fd) {
            Some(s) => s,
            None => return,
        };
        let action = drive_io(slot, &mut self.read_buf, app, /* drain */ true, self.verbose, self.label);
        let next = match action {
            ConnAction::WantRead => Interest::Read,
            ConnAction::WantWrite => Interest::Write,
            ConnAction::Close => {
                self.drop_conn(fd);
                return;
            }
        };
        let current = self.wants.get(&fd).copied();
        if !matches_interest(current, next) {
            if let Err(e) = self.epoll.modify(fd, next, Trigger::Edge) {
                if self.verbose {
                    eprintln!("{}: epoll mod failed: {e}", self.label);
                }
                self.drop_conn(fd);
                return;
            }
            self.wants.insert(fd, next);
        }
    }

    /// Toggle the listener's epoll registration based on the connection count.
    /// At the cap, removing the listener pushes accept queueing into the kernel
    /// backlog; once the backlog fills, the kernel refuses further connects —
    /// a deliberate, observable shed (§4).
    fn apply_backpressure(&mut self) {
        let listener_fd = self.listener.as_raw_fd();
        if self.listener_registered && self.conns.len() >= self.cfg.max_connections {
            let _ = self.epoll.delete(listener_fd);
            self.listener_registered = false;
        } else if !self.listener_registered && self.conns.len() < self.cfg.max_connections {
            match self.epoll.add(listener_fd, Interest::Read, Trigger::Edge) {
                Ok(()) => self.listener_registered = true,
                Err(e) => {
                    if self.verbose {
                        eprintln!("{}: re-register listener failed: {e}", self.label);
                    }
                }
            }
        }
    }

    /// Drop connections whose read deadline has passed. O(n) in `conns`, same
    /// as the `epoll-et` model — dwarfed by `epoll_wait`'s O(ready) advantage
    /// on the hot path because the sweep does no syscall per non-expired entry.
    fn sweep_expired(&mut self) {
        let now = Instant::now();
        let expired: Vec<RawFd> = self
            .conns
            .iter()
            .filter(|(_, c)| c.is_expired(now))
            .map(|(fd, _)| fd)
            .collect();
        for fd in expired {
            self.drop_conn(fd);
        }
    }

    /// Deregister + drop the slot (which closes the fd via the owned
    /// `TcpStream`). `EPOLL_CTL_DEL` is best-effort: the kernel removes fds
    /// from epoll on close anyway, but doing it explicitly keeps the discipline
    /// tidy if a future change ever holds the fd open elsewhere.
    fn drop_conn(&mut self, fd: RawFd) {
        let _ = self.epoll.delete(fd);
        self.wants.remove(&fd);
        self.conns.remove(fd);
    }
}

fn matches_interest(a: Option<Interest>, b: Interest) -> bool {
    matches!(
        (a, b),
        (Some(Interest::Read), Interest::Read)
            | (Some(Interest::Write), Interest::Write)
            | (Some(Interest::ReadWrite), Interest::ReadWrite)
    )
}
