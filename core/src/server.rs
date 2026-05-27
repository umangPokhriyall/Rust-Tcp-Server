//! The `Server` trait — the one swappable interface every concurrency model
//! implements — plus shared server configuration and the listener binder.

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use crate::app::App;

/// Listen backlog. Models accept from this queue; it is the first line of
/// connection backpressure before `max_connections`.
const LISTEN_BACKLOG: i32 = 1024;

pub struct ServerConfig {
    pub addr: SocketAddr,
    pub workers: usize,
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub max_connections: usize,
    pub assets_dir: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
            workers: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(30),
            max_connections: 1024,
            assets_dir: PathBuf::from("server/assets"),
        }
    }
}

/// Every concurrency model implements this ONE trait.
pub trait Server {
    fn name(&self) -> &'static str;
    /// Runs until the process is signalled to stop.
    fn serve(&self, cfg: &ServerConfig, app: std::sync::Arc<App>) -> std::io::Result<()>;
}

/// Bind a TCP listener. `reuse_port = true` sets SO_REUSEPORT + SO_REUSEADDR
/// (via `socket2`) so preforked children / multireactor threads can each own a
/// listener on the same address. Phase 0 only ever calls it with `false`, but
/// the `true` path is fully implemented.
pub fn bind_listener(addr: SocketAddr, reuse_port: bool) -> std::io::Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if reuse_port {
        // Each child/reactor binds its own listener on the same address; the
        // kernel load-balances accepts across them (avoids the thundering herd
        // of a single shared listener fd).
        socket.set_reuse_address(true)?;
        socket.set_reuse_port(true)?;
    }
    socket.bind(&addr.into())?;
    socket.listen(LISTEN_BACKLOG)?;
    Ok(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_any() -> SocketAddr {
        "127.0.0.1:0".parse().unwrap()
    }

    #[test]
    fn default_config_is_sane() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.addr.port(), 8080);
        assert!(cfg.workers >= 1);
        assert!(cfg.read_timeout > Duration::ZERO);
        assert!(cfg.write_timeout > Duration::ZERO);
        assert!(cfg.max_connections > 0);
    }

    #[test]
    fn binds_and_reports_an_assigned_port() {
        let listener = bind_listener(loopback_any(), false).unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }

    #[test]
    fn reuse_port_allows_two_listeners_on_the_same_addr() {
        let first = bind_listener(loopback_any(), true).unwrap();
        let port = first.local_addr().unwrap().port();
        let same: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

        // Without SO_REUSEPORT this second bind would fail with EADDRINUSE.
        let second = bind_listener(same, true).unwrap();
        assert_eq!(second.local_addr().unwrap().port(), port);
    }
}
