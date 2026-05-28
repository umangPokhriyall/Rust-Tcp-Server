//! Shared per-connection read/write routine for the event-loop models
//! (`poll`, `epoll-lt`, `epoll-et`). Given a [`Slot`] whose fd has just fired
//! a readable/writable event, drives the socket as far as the trigger
//! discipline permits and returns the next [`ConnAction`].
//!
//! The model layer owns registration — `PollFd` rebuild for `poll`,
//! `epoll_ctl` for the epoll models — and applies the returned action there.
//! This module is the "one abstraction, many implementations" the spec calls
//! for (§1.1 layering, hard rule 5): the same `Connection` state machine and
//! the same syscall pattern, parametrized only by trigger discipline.
//!
//! ## Trigger discipline
//!
//! * `drain = false` (level-triggered: `poll`, `epoll-lt`) — one `read` *or*
//!   one `write` per event, then yield back to the poller. The kernel will
//!   re-fire on the next iteration if the fd is still ready, so a drain loop
//!   here would just hog the worker.
//!
//! * `drain = true` (edge-triggered: `epoll-et`) — loop reads to `EAGAIN`,
//!   loop writes to `EAGAIN`, and **continue across state transitions**
//!   inside the same call (a read that completes a request falls straight
//!   through to the write arm). ET fires once per transition; missing the
//!   drain hangs the connection. This is the "understand the API to the
//!   floor" rule from §5.6.
//!
//! Partial-write resumption is the same shape in both modes: a `WantWrite`
//! action means the socket buffer filled; return and let the next writable
//! event resume from `pending_write`'s offset (`Connection` keeps the cursor).

use std::io::{self, Read, Write};

use core::{App, ConnAction};

use crate::sys::conn_table::Slot;

/// Drive one connection's socket through its current event.
///
/// The starting direction is read straight off the [`Connection`] state — a
/// non-empty `pending_write` means we are mid-response (Writing), otherwise
/// we are between requests (Reading). The `Connection` is the single source
/// of truth; no parallel "current direction" flag is kept, so the two cannot
/// drift apart.
///
/// `label` appears only in the verbose error log line.
pub(crate) fn drive_io(
    slot: &mut Slot,
    buf: &mut [u8],
    app: &App,
    drain: bool,
    verbose: bool,
    label: &str,
) -> ConnAction {
    let mut action = if slot.conn.pending_write().is_empty() {
        ConnAction::WantRead
    } else {
        ConnAction::WantWrite
    };

    loop {
        match action {
            ConnAction::WantRead => match slot.stream.read(buf) {
                Ok(0) => return ConnAction::Close,
                Ok(n) => {
                    action = slot.conn.on_bytes(&buf[..n], app);
                    // LT: yield after one read. ET: keep going — if `on_bytes`
                    // produced a response (WantWrite) the loop falls into the
                    // write arm in the same call, avoiding a round trip
                    // through `epoll_wait`.
                    if !drain {
                        return action;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // Receive queue empty. Stay registered for Read; the
                    // Connection's pending_write is empty so the next call
                    // will start in WantRead again.
                    return ConnAction::WantRead;
                }
                Err(e) => {
                    if verbose {
                        eprintln!("{label}: read error: {e}");
                    }
                    app.metrics().inc_errors();
                    return ConnAction::Close;
                }
            },
            ConnAction::WantWrite => {
                let pending = slot.conn.pending_write();
                if pending.is_empty() {
                    // Defensive — state machine said WantWrite but nothing is
                    // buffered. Fall back to Read rather than busy-loop.
                    return ConnAction::WantRead;
                }
                match slot.stream.write(pending) {
                    Ok(0) => return ConnAction::Close,
                    Ok(n) => {
                        action = slot.conn.on_written(n);
                        if matches!(action, ConnAction::WantWrite) {
                            // Partial write — the kernel send buffer filled.
                            // Wait for the next writable event before
                            // resuming. This is the ET partial-write
                            // resumption path; same shape under LT.
                            return action;
                        }
                        if !drain {
                            return action;
                        }
                        // ET: response fully sent, loop again — a pipelined
                        // next request may already be buffered in the
                        // Connection's parser.
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        return ConnAction::WantWrite;
                    }
                    Err(e) => {
                        if verbose {
                            eprintln!("{label}: write error: {e}");
                        }
                        app.metrics().inc_errors();
                        return ConnAction::Close;
                    }
                }
            }
            ConnAction::Close => return ConnAction::Close,
        }
    }
}
