//! Non-blocking socket primitives: `O_NONBLOCK` via `fcntl`, and an `accept4`
//! that returns `Ok(None)` on `EAGAIN` instead of erroring — the shape an
//! event loop wants when draining a listener to exhaustion.

use std::io;
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::unix::io::RawFd;

use crate::sys::syscall::{cvt, syscall};

/// Set `O_NONBLOCK` on `fd` without clobbering its other status flags.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = syscall!(fcntl(fd, libc::F_GETFL))?;
    syscall!(fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK))?;
    Ok(())
}

/// `accept4(SOCK_NONBLOCK)` — the accepted fd is born non-blocking, saving a
/// follow-up `fcntl`. Returns `Ok(None)` on `EAGAIN`/`EWOULDBLOCK` (the
/// listener is drained), `Err` on any other failure.
pub fn accept_nonblocking(listener_fd: RawFd) -> io::Result<Option<(RawFd, SocketAddr)>> {
    let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    let raw = unsafe {
        libc::accept4(
            listener_fd,
            &mut storage as *mut _ as *mut libc::sockaddr,
            &mut len,
            libc::SOCK_NONBLOCK,
        )
    };
    match cvt(raw) {
        Ok(fd) => Ok(Some((fd, sockaddr_to_socketaddr(&storage)?))),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e),
    }
}

/// Decode a kernel `sockaddr_storage` into a `std::net::SocketAddr`.
fn sockaddr_to_socketaddr(storage: &libc::sockaddr_storage) -> io::Result<SocketAddr> {
    match storage.ss_family as libc::c_int {
        libc::AF_INET => {
            let addr = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr));
            let port = u16::from_be(addr.sin_port);
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        }
        libc::AF_INET6 => {
            let addr = unsafe { &*(storage as *const _ as *const libc::sockaddr_in6) };
            let ip = Ipv6Addr::from(addr.sin6_addr.s6_addr);
            let port = u16::from_be(addr.sin6_port);
            Ok(SocketAddr::V6(SocketAddrV6::new(
                ip,
                port,
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        other => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("unsupported address family {other}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::io::AsRawFd;
    use std::time::Duration;

    #[test]
    fn accept_nonblocking_eagain_then_accepts() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let lfd = listener.as_raw_fd();
        set_nonblocking(lfd).unwrap();

        // Nothing pending yet: EAGAIN maps to Ok(None), not an error.
        assert!(accept_nonblocking(lfd).unwrap().is_none());

        let _client = TcpStream::connect(addr).unwrap();

        // The connection lands asynchronously; poll a bounded number of times.
        let mut accepted = None;
        for _ in 0..200 {
            if let Some(pair) = accept_nonblocking(lfd).unwrap() {
                accepted = Some(pair);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (cfd, peer) = accepted.expect("client should be accepted");
        assert!(peer.ip().is_loopback());
        unsafe { libc::close(cfd) };
    }
}
