//! One module per concurrency model, each implementing `core::Server`, plus the
//! shared blocking serve loop they reuse.
//!
//! Implemented so far: `iterative` (Phase 0), `forking`, `preforked`,
//! `thread-per-conn`, `thread-pool`. The remaining models arrive in later
//! Phase 1 sessions — do not implement them ahead of their session.

use std::sync::Arc;

use core::ServerConfig;

pub mod blocking;
pub mod epoll;
pub mod event_io;
pub mod event_loop;
pub mod forking;
pub mod iterative;
pub mod multireactor;
pub mod poll;
pub mod preforked;
pub mod thread_per_conn;
pub mod thread_pool;

/// Clone the `ServerConfig` into an owned, shareable value for detached workers.
///
/// `core` is frozen in Phase 1, so `ServerConfig` is not `Clone`; the thread
/// models (which `thread::spawn` `'static` closures that outlive the borrowed
/// `&ServerConfig`) build this once at startup and hand each worker an `Arc`
/// clone — the `PathBuf` is cloned exactly once, never per connection.
pub(crate) fn shared_config(cfg: &ServerConfig) -> Arc<ServerConfig> {
    Arc::new(ServerConfig {
        addr: cfg.addr,
        workers: cfg.workers,
        read_timeout: cfg.read_timeout,
        write_timeout: cfg.write_timeout,
        max_connections: cfg.max_connections,
        assets_dir: cfg.assets_dir.clone(),
    })
}
