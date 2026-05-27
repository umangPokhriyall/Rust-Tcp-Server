//! A `poll(2)` wrapper. Unlike epoll, `poll` is inherently level-triggered and
//! O(n): the caller hands it the entire fd set every call and scans every entry
//! afterward. That cost is the point — it is the readiness baseline the epoll
//! models are measured against — so it is exposed, not papered over.

use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::sys::epoll::Interest;
use crate::sys::syscall::{syscall, timeout_to_millis};

/// One entry in a `poll` set. `#[repr(transparent)]` over `libc::pollfd` so a
/// `&mut [PollFd]` can be handed to `poll(2)` directly.
#[repr(transparent)]
pub struct PollFd {
    inner: libc::pollfd,
}

impl PollFd {
    pub fn new(fd: RawFd, interest: Interest) -> Self {
        let events = match interest {
            Interest::Read => libc::POLLIN,
            Interest::Write => libc::POLLOUT,
            Interest::ReadWrite => libc::POLLIN | libc::POLLOUT,
        };
        PollFd {
            inner: libc::pollfd {
                fd,
                events,
                revents: 0,
            },
        }
    }

    pub fn readable(&self) -> bool {
        self.inner.revents & libc::POLLIN != 0
    }

    pub fn writable(&self) -> bool {
        self.inner.revents & libc::POLLOUT != 0
    }

    pub fn hup(&self) -> bool {
        self.inner.revents & libc::POLLHUP != 0
    }

    pub fn error(&self) -> bool {
        self.inner.revents & (libc::POLLERR | libc::POLLNVAL) != 0
    }
}

/// Block up to `timeout` (`None` = forever), updating each `PollFd`'s `revents`.
/// Returns the number of fds with non-zero `revents`.
pub fn poll(fds: &mut [PollFd], timeout: Option<Duration>) -> io::Result<usize> {
    let n = syscall!(poll(
        fds.as_mut_ptr() as *mut libc::pollfd,
        fds.len() as libc::nfds_t,
        timeout_to_millis(timeout)
    ))?;
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    #[test]
    fn reports_readable_after_write() {
        let (rd, wr) = pipe();
        let mut set = [PollFd::new(rd, Interest::Read)];

        // Nothing to read yet: a short timeout returns zero ready fds.
        let n = poll(&mut set, Some(Duration::from_millis(50))).unwrap();
        assert_eq!(n, 0);
        assert!(!set[0].readable());

        let byte = [1u8];
        assert_eq!(unsafe { libc::write(wr, byte.as_ptr() as *const _, 1) }, 1);
        let n = poll(&mut set, Some(Duration::from_millis(500))).unwrap();
        assert_eq!(n, 1);
        assert!(set[0].readable());
        assert!(!set[0].error());

        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }

    #[test]
    fn write_end_is_writable() {
        let (rd, wr) = pipe();
        let mut set = [PollFd::new(wr, Interest::Write)];
        let n = poll(&mut set, Some(Duration::from_millis(500))).unwrap();
        assert_eq!(n, 1);
        assert!(set[0].writable());
        unsafe {
            libc::close(rd);
            libc::close(wr);
        }
    }
}
