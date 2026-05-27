//! The `Server` trait — the one swappable interface every concurrency model
//! implements — plus shared server configuration and the listener binder.

use crate::app::App;

pub struct ServerConfig {
    pub addr: std::net::SocketAddr,
    pub workers: usize,
    pub read_timeout: std::time::Duration,
    pub write_timeout: std::time::Duration,
    pub max_connections: usize,
    pub assets_dir: std::path::PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        todo!()
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
/// the `true` path is implemented in Session D.
pub fn bind_listener(
    _addr: std::net::SocketAddr,
    _reuse_port: bool,
) -> std::io::Result<std::net::TcpListener> {
    todo!()
}
