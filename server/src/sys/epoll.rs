//! A minimal `epoll` wrapper. The registered `fd` is stored directly as the
//! epoll `u64` user-data, so `wait` reports it straight back — no side table.
//!
//! `Trigger::Edge` sets `EPOLLET`; the level-vs-edge distinction is deliberately
//! exposed (not hidden) because `epoll-lt` and `epoll-et` are separate models.

use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::sys::syscall::{syscall, timeout_to_millis};

/// Which readiness an fd is registered for.
#[derive(Clone, Copy)]
pub enum Interest {
    Read,
    Write,
    ReadWrite,
}

/// Level- vs edge-triggered. `Edge` maps to `EPOLLET`.
#[derive(Clone, Copy)]
pub enum Trigger {
    Level,
    Edge,
}

/// One ready fd as reported by [`Epoll::wait`].
pub struct Event {
    pub fd: RawFd,
    pub readable: bool,
    pub writable: bool,
    pub hup: bool,
    pub error: bool,
}

/// Owns the epoll fd; closes it on `Drop`.
pub struct Epoll {
    fd: RawFd,
}

impl Epoll {
    /// `epoll_create1(EPOLL_CLOEXEC)`.
    pub fn new() -> io::Result<Self> {
        let fd = syscall!(epoll_create1(libc::EPOLL_CLOEXEC))?;
        Ok(Epoll { fd })
    }

    /// Register `fd`, carrying it as the epoll user-data.
    pub fn add(&self, fd: RawFd, interest: Interest, trigger: Trigger) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_ADD, fd, interest, trigger)
    }

    /// Change the interest/trigger of an already-registered `fd`.
    pub fn modify(&self, fd: RawFd, interest: Interest, trigger: Trigger) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_MOD, fd, interest, trigger)
    }

    /// Deregister `fd`.
    pub fn delete(&self, fd: RawFd) -> io::Result<()> {
        syscall!(epoll_ctl(
            self.fd,
            libc::EPOLL_CTL_DEL,
            fd,
            std::ptr::null_mut()
        ))?;
        Ok(())
    }

    /// Block up to `timeout` (`None` = forever). Clears and fills `events`,
    /// returning the count. The buffer is sized from `events`' capacity, so a
    /// caller that reserves up front avoids per-call allocation.
    pub fn wait(&self, events: &mut Vec<Event>, timeout: Option<Duration>) -> io::Result<usize> {
        let capacity = events.capacity().max(1);
        let mut raw: Vec<libc::epoll_event> = Vec::with_capacity(capacity);
        let timeout_ms = timeout_to_millis(timeout);

        let n = syscall!(epoll_wait(
            self.fd,
            raw.as_mut_ptr(),
            capacity as libc::c_int,
            timeout_ms
        ))?;
        // SAFETY: epoll_wait filled `n` events into the buffer we sized above.
        unsafe { raw.set_len(n as usize) };

        events.clear();
        for ev in &raw {
            let flags = ev.events;
            events.push(Event {
                fd: ev.u64 as RawFd,
                readable: flags & libc::EPOLLIN as u32 != 0,
                writable: flags & libc::EPOLLOUT as u32 != 0,
                hup: flags & libc::EPOLLHUP as u32 != 0,
                error: flags & libc::EPOLLERR as u32 != 0,
            });
        }
        Ok(n as usize)
    }

    fn ctl(&self, op: libc::c_int, fd: RawFd, interest: Interest, trigger: Trigger) -> io::Result<()> {
        let mut event = libc::epoll_event {
            events: events_mask(interest, trigger),
            u64: fd as u64,
        };
        syscall!(epoll_ctl(self.fd, op, fd, &mut event))?;
        Ok(())
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// Build the `events` bitmask for a registration.
fn events_mask(interest: Interest, trigger: Trigger) -> u32 {
    let mut mask = match interest {
        Interest::Read => libc::EPOLLIN,
        Interest::Write => libc::EPOLLOUT,
        Interest::ReadWrite => libc::EPOLLIN | libc::EPOLLOUT,
    } as u32;
    if let Trigger::Edge = trigger {
        mask |= libc::EPOLLET as u32;
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a pipe; returns (read_fd, write_fd).
    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    #[test]
    fn waits_then_reports_readable_pipe() {
        let (rd, wr) = pipe();
        let epoll = Epoll::new().unwrap();
        epoll.add(rd, Interest::Read, Trigger::Level).unwrap();

        let mut events = Vec::with_capacity(8);

        // Nothing written: a short timeout yields zero events.
        let n = epoll
            .wait(&mut events, Some(Duration::from_millis(50)))
            .unwrap();
        assert_eq!(n, 0);
        assert!(events.is_empty());

        // After a write the read end is readable, reported with its fd.
        let byte = [7u8];
        assert_eq!(unsafe { libc::write(wr, byte.as_ptr() as *const _, 1) }, 1);
        let n = epoll
            .wait(&mut events, Some(Duration::from_millis(500)))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(events[0].fd, rd);
        assert!(events[0].readable);
        assert!(!events[0].writable);

        epoll.delete(rd).unwrap();
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    #[test]
    fn modify_to_write_interest_fires() {
        let (rd, wr) = pipe();
        let epoll = Epoll::new().unwrap();
        // A pipe's write end is writable while there is buffer space.
        epoll.add(wr, Interest::Read, Trigger::Level).unwrap();
        epoll.modify(wr, Interest::Write, Trigger::Level).unwrap();

        let mut events = Vec::with_capacity(4);
        let n = epoll
            .wait(&mut events, Some(Duration::from_millis(500)))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(events[0].fd, wr);
        assert!(events[0].writable);

        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }
}
