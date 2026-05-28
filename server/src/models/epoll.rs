//! `epoll-lt` / `epoll-et` — single-thread `epoll(7)` event loop, parametrized
//! by trigger discipline.
//!
//! One `fn run_epoll(trigger, drain, ...)` body, two thin [`Server`] impls:
//!
//! * [`EpollLt`] — `Trigger::Level`, `drain = false`. Level-triggered: the
//!   kernel reports current readiness every wait, so a single `read`/`write`
//!   per event is enough. Behaves like `poll` but with O(ready) wakeups.
//!
//! * [`EpollEt`] — `Trigger::Edge`,  `drain = true`. Edge-triggered: the
//!   kernel fires once per transition, so reads and accepts MUST drain to
//!   `EAGAIN` or events are lost and the connection hangs. The drain
//!   discipline is the whole reason ET exists as its own model — see §5.6
//!   ("the understand-the-API-to-the-floor model").
//!
//! Sharing the body with the same `drive_io` routine that `poll` uses means
//! benchmarks isolate exactly *one* variable per pair: `poll` vs `epoll-lt`
//! measures `poll(2)`-O(n) vs `epoll(7)`-O(ready); `epoll-lt` vs `epoll-et`
//! measures the LT-vs-ET cost. No other code path differs.
//!
//! Backpressure: at `cfg.max_connections` the listener is `epoll_ctl(DEL)`'d,
//! so the kernel accept backlog absorbs further connections and ultimately
//! refuses them. The listener is re-added once `conns.len()` drops below the
//! cap — a deliberate, observable shed (same policy as `poll` and `reactor`).
//!
//! Expiry: each iteration scans `ConnTable` for connections past their read
//! deadline and closes them. The wait cap [`TICK`] bounds shutdown latency
//! *and* the staleness of that sweep.

use std::collections::HashMap;
use std::io;
use std::net::TcpStream;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use core::limits::READ_CHUNK;
use core::{bind_listener, App, ConnAction, Connection, Server, ServerConfig};

use crate::models::event_io::drive_io;
use crate::sys::conn_table::ConnTable;
use crate::sys::epoll::{Epoll, Event, Interest, Trigger};
use crate::sys::{signal, socket};

/// Flipped by the SIGINT/SIGTERM handler — checked each iteration of the loop.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Wakeup cap for `epoll_wait`. Bounds shutdown latency *and* the staleness of
/// the expired-connection sweep — `core::Connection` does not expose its
/// per-connection deadline (frozen API in Phase 1), so we can't wake on the
/// earliest expiry. 100 ms is small relative to the default 30 s timeouts.
const TICK: Duration = Duration::from_millis(100);

/// Cap on events returned per `epoll_wait`. Higher values reduce wait calls
/// under heavy load but inflate the events buffer.
const MAX_EVENTS: usize = 1024;

/// `epoll-lt` — level-triggered. No drain; one syscall per event.
pub struct EpollLt {
    verbose: bool,
}

impl EpollLt {
    pub fn new(verbose: bool) -> Self {
        EpollLt { verbose }
    }
}

impl Server for EpollLt {
    fn name(&self) -> &'static str {
        "epoll-lt"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        run_epoll(Trigger::Level, /* drain */ false, cfg, &app, self.verbose, "epoll-lt")
    }
}

/// `epoll-et` — edge-triggered. Drain reads/accepts to `EAGAIN`; resume
/// partial writes on the next writable event.
pub struct EpollEt {
    verbose: bool,
}

impl EpollEt {
    pub fn new(verbose: bool) -> Self {
        EpollEt { verbose }
    }
}

impl Server for EpollEt {
    fn name(&self) -> &'static str {
        "epoll-et"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> io::Result<()> {
        run_epoll(Trigger::Edge, /* drain */ true, cfg, &app, self.verbose, "epoll-et")
    }
}

/// The shared event loop. The two `Server` impls above are thin wrappers that
/// fix `trigger` and `drain` — every other line is identical, so the benchmark
/// isolates the trigger-discipline variable cleanly.
fn run_epoll(
    trigger: Trigger,
    drain: bool,
    cfg: &ServerConfig,
    app: &Arc<App>,
    verbose: bool,
    label: &'static str,
) -> io::Result<()> {
    signal::install_shutdown_flag(&SHUTDOWN);

    let listener = bind_listener(cfg.addr, false)?;
    let listener_fd = listener.as_raw_fd();
    socket::set_nonblocking(listener_fd)?;
    eprintln!("{label}: listening on http://{}", cfg.addr);

    let epoll = Epoll::new()?;
    epoll.add(listener_fd, Interest::Read, trigger)?;
    let mut listener_registered = true;

    let mut conns = ConnTable::new();
    // Current registered interest per fd. Tracked so we only issue
    // `epoll_ctl(MOD)` when the desired interest actually changes — under
    // keep-alive most read events leave a connection in `Read` interest, so
    // the MOD is genuinely conditional, not always-fire.
    let mut wants: HashMap<RawFd, Interest> = HashMap::new();
    let mut events: Vec<Event> = Vec::with_capacity(MAX_EVENTS);
    let mut buf = vec![0u8; READ_CHUNK];

    while !SHUTDOWN.load(Ordering::SeqCst) {
        match epoll.wait(&mut events, Some(TICK)) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if verbose {
                    eprintln!("{label}: epoll_wait error: {e}");
                }
                continue;
            }
        }

        // Snapshot the events as plain tuples so we can hand `&mut conns` to
        // the per-fd handlers without aliasing the events buffer (which
        // `epoll.wait` borrows mutably each iteration).
        let ready: Vec<(RawFd, bool, bool, bool, bool)> = events
            .iter()
            .map(|e| (e.fd, e.readable, e.writable, e.hup, e.error))
            .collect();
        for (fd, readable, writable, hup, err) in ready {
            if fd == listener_fd {
                if readable {
                    accept_ready(
                        AcceptCtx {
                            epoll: &epoll,
                            listener_fd,
                            cfg,
                            app,
                            trigger,
                            drain,
                            verbose,
                            label,
                        },
                        &mut conns,
                        &mut wants,
                    );
                }
                continue;
            }

            // HUP and ERR are terminal — drop the connection regardless of
            // whether read/write also fired. (A pending read could still be
            // surfaced under HUP, but `drive_io` would observe `read() == 0`
            // or a hard error on its next attempt anyway.)
            if err || hup {
                drop_conn(&epoll, fd, &mut conns, &mut wants);
                continue;
            }
            if readable || writable {
                drive(
                    &epoll, fd, &mut conns, &mut wants, &mut buf, app, trigger, drain, verbose,
                    label,
                );
            }
        }

        // Backpressure shedding: at the cap, pull the listener out of epoll
        // entirely so the kernel accept backlog absorbs new connections and
        // eventually starts refusing them — a deliberate, observable shed.
        if listener_registered && conns.len() >= cfg.max_connections {
            let _ = epoll.delete(listener_fd);
            listener_registered = false;
        } else if !listener_registered && conns.len() < cfg.max_connections {
            if let Err(e) = epoll.add(listener_fd, Interest::Read, trigger) {
                if verbose {
                    eprintln!("{label}: re-register listener failed: {e}");
                }
            } else {
                listener_registered = true;
            }
        }

        // Sweep idle-too-long connections. Same O(n) cost as `poll`'s sweep;
        // dwarfed by `epoll_wait`'s O(ready) win on the hot path because the
        // sweep does no syscall per non-expired entry.
        let now = Instant::now();
        let expired: Vec<RawFd> = conns
            .iter()
            .filter(|(_, c)| c.is_expired(now))
            .map(|(fd, _)| fd)
            .collect();
        for fd in expired {
            drop_conn(&epoll, fd, &mut conns, &mut wants);
        }
    }

    Ok(())
}

/// Bundle of read-only knobs the accept loop needs. Lives only to keep
/// `accept_ready`'s argument list under clippy's `too_many_arguments` cap
/// while still threading all the per-model parameters through.
struct AcceptCtx<'a> {
    epoll: &'a Epoll,
    listener_fd: RawFd,
    cfg: &'a ServerConfig,
    app: &'a App,
    trigger: Trigger,
    drain: bool,
    verbose: bool,
    label: &'a str,
}

/// Listener ready — accept either once (LT) or until `EAGAIN` (ET drain). New
/// fds land in `ConnTable`, registered for `Read` with the loop's trigger.
fn accept_ready(
    ctx: AcceptCtx<'_>,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
) {
    loop {
        if conns.len() >= ctx.cfg.max_connections {
            // Cap reached mid-drain — stop. The outer loop will deregister
            // the listener and re-add it once capacity frees.
            return;
        }
        match socket::accept_nonblocking(ctx.listener_fd) {
            Ok(Some((fd, _peer))) => {
                // `accept4(SOCK_NONBLOCK)` already gave us a non-blocking fd,
                // but defending here means a future change to that helper
                // cannot silently regress us into blocking sockets.
                if let Err(e) = socket::set_nonblocking(fd) {
                    if ctx.verbose {
                        eprintln!("{}: set_nonblocking failed: {e}", ctx.label);
                    }
                    unsafe { libc::close(fd) };
                    if !ctx.drain {
                        return;
                    }
                    continue;
                }
                // SAFETY: accept4 returned an owned fd; from_raw_fd takes it.
                let stream = unsafe { TcpStream::from_raw_fd(fd) };
                let conn = Connection::new(ctx.cfg.read_timeout);
                let fd = conns.insert(stream, conn);
                if let Err(e) = ctx.epoll.add(fd, Interest::Read, ctx.trigger) {
                    if ctx.verbose {
                        eprintln!("{}: epoll add failed: {e}", ctx.label);
                    }
                    conns.remove(fd);
                    if !ctx.drain {
                        return;
                    }
                    continue;
                }
                wants.insert(fd, Interest::Read);
                ctx.app.metrics().inc_connections();
                // LT: one accept per event — kernel will fire again next
                // iteration if more are pending. ET: must drain or lose
                // the next-transition wakeup.
                if !ctx.drain {
                    return;
                }
            }
            Ok(None) => return, // EAGAIN — backlog drained.
            Err(e) => {
                if ctx.verbose {
                    eprintln!("{}: accept error: {e}", ctx.label);
                }
                return;
            }
        }
    }
}

/// Drive one ready connection through [`drive_io`], then apply the resulting
/// action: `epoll_ctl(MOD)` to the new interest if it changed, `DEL` + remove
/// on Close.
#[allow(clippy::too_many_arguments)]
fn drive(
    epoll: &Epoll,
    fd: RawFd,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
    buf: &mut [u8],
    app: &App,
    trigger: Trigger,
    drain: bool,
    verbose: bool,
    label: &str,
) {
    let slot = match conns.get_mut(fd) {
        Some(s) => s,
        None => return,
    };
    let action = drive_io(slot, buf, app, drain, verbose, label);
    let next = match action {
        ConnAction::WantRead => Interest::Read,
        ConnAction::WantWrite => Interest::Write,
        ConnAction::Close => {
            drop_conn(epoll, fd, conns, wants);
            return;
        }
    };
    // Only MOD when the desired interest actually changes — under keep-alive
    // most read events leave the interest at Read, so the typical hot path
    // skips the syscall entirely.
    let current = wants.get(&fd).copied();
    if !matches_interest(current, next) {
        if let Err(e) = epoll.modify(fd, next, trigger) {
            if verbose {
                eprintln!("{label}: epoll mod failed: {e}");
            }
            drop_conn(epoll, fd, conns, wants);
            return;
        }
        wants.insert(fd, next);
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

/// Close `fd`: deregister, then drop from the table (which closes the fd via
/// the owned `TcpStream`). `EPOLL_CTL_DEL` is best-effort — the kernel removes
/// fds from epoll on close anyway, but doing it explicitly avoids a stale
/// reference if some other thread held the fd open (it does not in our
/// single-threaded model, but the discipline is worth $0).
fn drop_conn(
    epoll: &Epoll,
    fd: RawFd,
    conns: &mut ConnTable,
    wants: &mut HashMap<RawFd, Interest>,
) {
    let _ = epoll.delete(fd);
    wants.remove(&fd);
    conns.remove(fd);
}
