//! `ConnTable` — one event loop's set of live connections, keyed by fd.
//!
//! The `TcpStream` owns the fd, so `remove` (and `Drop`) closes it. `core`'s
//! sans-IO [`Connection`] state machine rides alongside each stream; the loop
//! reads/writes the stream and feeds bytes to the `Connection`.

use std::collections::HashMap;
use std::collections::hash_map::Values;
use std::net::TcpStream;
use std::os::unix::io::{AsRawFd, RawFd};

use core::Connection;

/// A live connection: the owned socket plus its protocol state.
pub struct Slot {
    pub stream: TcpStream,
    pub conn: Connection,
}

pub struct ConnTable {
    slots: HashMap<RawFd, Slot>,
}

impl ConnTable {
    pub fn new() -> Self {
        ConnTable {
            slots: HashMap::new(),
        }
    }

    /// Take ownership of `stream` + `conn`, keyed by the stream's fd. Returns
    /// the fd so the caller can register it with epoll/poll.
    pub fn insert(&mut self, stream: TcpStream, conn: Connection) -> RawFd {
        let fd = stream.as_raw_fd();
        self.slots.insert(fd, Slot { stream, conn });
        fd
    }

    pub fn get_mut(&mut self, fd: RawFd) -> Option<&mut Slot> {
        self.slots.get_mut(&fd)
    }

    /// Drop the slot — closing the fd via the `TcpStream`.
    pub fn remove(&mut self, fd: RawFd) {
        self.slots.remove(&fd);
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// For timeout scans: `(fd, &Connection)` over every live connection.
    pub fn iter(&self) -> impl Iterator<Item = (RawFd, &Connection)> {
        Iter {
            inner: self.slots.values(),
        }
    }
}

impl Default for ConnTable {
    fn default() -> Self {
        Self::new()
    }
}

/// `iter` adapter. The fd is read from each `Slot`'s stream rather than the map
/// key, so the yielded fd and stream cannot drift apart.
struct Iter<'a> {
    inner: Values<'a, RawFd, Slot>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (RawFd, &'a Connection);

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.inner.next()?;
        Some((slot.stream.as_raw_fd(), &slot.conn))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    /// A real connected server-side stream (so the fd is a live socket).
    fn server_stream() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).unwrap();
        let (server, _peer) = listener.accept().unwrap();
        (server, client)
    }

    #[test]
    fn insert_get_iter_remove() {
        let (server, _client) = server_stream();
        let mut table = ConnTable::new();
        assert!(table.is_empty());

        let fd = table.insert(server, Connection::new(Duration::from_secs(5)));
        assert_eq!(table.len(), 1);
        assert!(table.get_mut(fd).is_some());

        let collected: Vec<RawFd> = table.iter().map(|(fd, _)| fd).collect();
        assert_eq!(collected, vec![fd]);

        table.remove(fd);
        assert_eq!(table.len(), 0);
        assert!(table.get_mut(fd).is_none());
    }
}
