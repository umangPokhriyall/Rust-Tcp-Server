//! `poll` — a single-threaded `poll(2)` event loop. The readiness-I/O baseline.
//!
//! Design (§5.5): one thread, non-blocking listener + non-blocking client
//! sockets, a [`ConnTable`] of live connections plus a parallel map of each
//! connection's current interest (`Read` while waiting for a request,
//! `Write` while draining a response). Each iteration the loop rebuilds the
//! full [`PollFd`] vector, calls [`sys::poll`], then scans every returned fd —
//! O(n) per wakeup whether one or every connection is ready.
//!
//! That O(n) cost is exactly the point: `poll` is the readiness baseline the
//! `epoll` models are measured against. It is inherently level-triggered
//! (`poll` always reports current readiness, no edges), so reads need no drain
//! loop — partial work resumes on the next iteration.
//!
//! Backpressure: at `cfg.max_connections` the listener is omitted from the
//! `PollFd` set, so the kernel accept backlog absorbs new connections and
//! eventually refuses them. The listener re-enters the set the next iteration
//! after the connection count drops.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core::limits::READ_CHUNK;
use core::{bind_listener, App, ConnAction, Connection, Server, ServerConfig};

use crate::sys::conn_table::ConnTable;
use crate::sys::epoll::Interest;
use crate::sys::poll::{poll, PollFd};
use crate::sys::{signal, socket};

/// Flipped by the SIGINT/SIGTERM handler — the loop polls it each iteration.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Wakeup cap for [`poll`]. Bounds shutdown latency *and* the staleness of the
/// expired-connection sweep — without exposing each `Connection`'s deadline
/// (frozen `core` API) we cannot wake on the earliest expiry, so a fixed cap
/// is the right choice. 100 ms is small compared to the default 30 s timeouts.
const TICK: Duration = Duration::from_millis(100);

pub struct Poll {
    verbose: bool,
}

impl Poll {
    pub fn new(verbose: bool) -> Self {
        Poll { verbose }
    }
}

impl Server for Poll {
    fn name(&self) -> &'static str {
        "poll"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        signal::install_shutdown_flag(&SHUTDOWN);

        let listener = bind_listener(cfg.addr, false)?;
        let listener_fd = listener.as_raw_fd();
        socket::set_nonblocking(listener_fd)?;
        eprintln!("poll: listening on http://{}", cfg.addr);

        let mut conns = ConnTable::new();
        // Per-connection interest. Mirrors the last `ConnAction` so the loop
        // can hand `poll` the right `Interest` for every fd each iteration
        // without re-deriving it from `Connection` state every time.
        let mut wants: HashMap<RawFd, Interest> = HashMap::new();
        let mut buf = vec![0u8; READ_CHUNK];

        while !SHUTDOWN.load(Ordering::SeqCst) {
            // 1. Build the poll set. Listener first iff we are below the cap;
            //    each connection follows with its current interest. The vector
            //    is rebuilt every iteration on purpose — that O(n) work is the
            //    cost `poll` is here to expose.
            let mut fds: Vec<PollFd> = Vec::with_capacity(conns.len() + 1);
            let listener_in_set = conns.len() < cfg.max_connections;
            if listener_in_set {
                fds.push(PollFd::new(listener_fd, Interest::Read));
            }
            let mut fd_order: Vec<RawFd> = Vec::with_capacity(conns.len());
            for (fd, &interest) in wants.iter() {
                fds.push(PollFd::new(*fd, interest));
                fd_order.push(*fd);
            }

            // 2. Wait.
            match poll(&mut fds, Some(TICK)) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    if self.verbose {
                        eprintln!("poll: wait error: {e}");
                    }
                    continue;
                }
            }

            // 3. Listener: a single non-blocking accept (LT — no drain).
            if listener_in_set && fds[0].readable() {
                accept_one(listener_fd, &mut conns, &mut wants, cfg, &app, self.verbose);
            }

            // 4. Drive every reported connection fd. Iterate over the snapshot
            //    we recorded in step 1, not `wants`, because `wants` may have
            //    changed mid-iteration if accept_one inserted a new connection.
            let conn_offset = if listener_in_set { 1 } else { 0 };
            for (i, fd) in fd_order.into_iter().enumerate() {
                let pollfd = &fds[conn_offset + i];
                if pollfd.error() || pollfd.hup() {
                    drop_conn(fd, &mut conns, &mut wants);
                    continue;
                }
                if pollfd.readable() || pollfd.writable() {
                    drive(
                        fd,
                        &mut conns,
                        &mut wants,
                        &mut buf,
                        &app,
                        self.verbose,
                        /* drain */ false,
                    );
                }
            }

            // 5. Expire idle connections. The sweep is O(n) in `conns`, which is
            //    proportional to `poll`'s own O(n) wakeup cost — no extra big-O.
            let now = Instant::now();
            let expired: Vec<RawFd> = conns
                .iter()
                .filter(|(_, c)| c.is_expired(now))
                .map(|(fd, _)| fd)
                .collect();
            for fd in expired {
                drop_conn(fd, &mut conns, &mut wants);
            }
        }

        Ok(())
    }
}

/// Accept one ready connection (LT discipline — no drain loop). Sets the new fd
/// non-blocking, wraps it as a `TcpStream` (which owns and will close the fd),
/// and registers it in `conns`/`wants` with initial `Read` interest.
fn accept_one(
    listener_fd: RawFd,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
    cfg: &ServerConfig,
    app: &App,
    verbose: bool,
) {
    match socket::accept_nonblocking(listener_fd) {
        Ok(Some((fd, _peer))) => {
            if let Err(e) = socket::set_nonblocking(fd) {
                if verbose {
                    eprintln!("poll: set_nonblocking failed: {e}");
                }
                unsafe { libc::close(fd) };
                return;
            }
            // SAFETY: `accept4` returned an owned fd; `from_raw_fd` takes it.
            let stream = unsafe { TcpStream::from_raw_fd(fd) };
            let conn = Connection::new(cfg.read_timeout);
            let fd = conns.insert(stream, conn);
            wants.insert(fd, Interest::Read);
            app.metrics().inc_connections();
        }
        Ok(None) => {} // EAGAIN — no pending connections, fine.
        Err(e) => {
            if verbose {
                eprintln!("poll: accept error: {e}");
            }
        }
    }
}

/// Drive one connection's readable/writable event through to a new action,
/// updating its registered interest. `drain` is `false` for `poll` (LT), but
/// the routine is shared so `epoll` (which sets `drain` per-trigger) can reuse
/// the same write/read shape — see [`models::epoll`].
pub(crate) fn drive(
    fd: RawFd,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
    buf: &mut [u8],
    app: &App,
    verbose: bool,
    drain: bool,
) {
    let slot = match conns.get_mut(fd) {
        Some(s) => s,
        None => return,
    };

    let mut action = match wants.get(&fd).copied().unwrap_or(Interest::Read) {
        Interest::Read => ConnAction::WantRead,
        Interest::Write => ConnAction::WantWrite,
        Interest::ReadWrite => ConnAction::WantRead,
    };

    loop {
        match action {
            ConnAction::WantRead => {
                match slot.stream.read(buf) {
                    Ok(0) => {
                        action = ConnAction::Close;
                        break;
                    }
                    Ok(n) => {
                        action = slot.conn.on_bytes(&buf[..n], app);
                        // ET (drain=true): keep reading while the loop still
                        // wants more, until EAGAIN. LT (drain=false): one read
                        // per readable event, then yield back to the poller.
                        if !drain || !matches!(action, ConnAction::WantRead) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        if verbose {
                            eprintln!("poll: read error: {e}");
                        }
                        app.metrics().inc_errors();
                        action = ConnAction::Close;
                        break;
                    }
                }
            }
            ConnAction::WantWrite => {
                let pending = slot.conn.pending_write();
                if pending.is_empty() {
                    // Defensive — state says Write but nothing buffered.
                    action = ConnAction::WantRead;
                    break;
                }
                match slot.stream.write(pending) {
                    Ok(0) => {
                        action = ConnAction::Close;
                        break;
                    }
                    Ok(n) => {
                        action = slot.conn.on_written(n);
                        // If we still WantWrite (partial response), let the
                        // poller wake us when writable again — applies to both
                        // LT and ET, and is the ET partial-write resumption.
                        if matches!(action, ConnAction::WantWrite) {
                            break;
                        }
                        // WantRead/Close — fall through. WantRead jumps back to
                        // the read arm above only in `drain` mode, otherwise we
                        // just update the registration and yield.
                        if !drain {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) => {
                        if verbose {
                            eprintln!("poll: write error: {e}");
                        }
                        app.metrics().inc_errors();
                        action = ConnAction::Close;
                        break;
                    }
                }
            }
            ConnAction::Close => break,
        }
    }

    // Apply the resulting action to the per-fd interest map.
    match action {
        ConnAction::WantRead => {
            wants.insert(fd, Interest::Read);
        }
        ConnAction::WantWrite => {
            wants.insert(fd, Interest::Write);
        }
        ConnAction::Close => {
            drop_conn(fd, conns, wants);
        }
    }
}

/// Close a connection: drop it from the table (which closes the fd) and from
/// the interest map. Idempotent — safe to call on an already-removed fd.
pub(crate) fn drop_conn(
    fd: RawFd,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
) {
    wants.remove(&fd);
    conns.remove(fd);
}
