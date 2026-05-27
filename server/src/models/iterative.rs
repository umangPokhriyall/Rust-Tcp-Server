//! The `iterative` reference model: a single thread that accepts one connection
//! at a time and serves it to completion before accepting the next.
//!
//! Correctness fixes over the legacy `iterative` crate (§1.3, §11):
//! - A per-connection error is caught and (optionally) logged, then the accept
//!   loop continues. One bad client never takes the server down.
//! - Read/write timeouts are set on every stream, so a slow-loris client is
//!   dropped instead of pinning the thread forever.
//! - No hot-path logging: per-connection logging is gated behind `--verbose`,
//!   off by default.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;

use core::limits::READ_CHUNK;
use core::{bind_listener, App, ConnAction, Connection, Server, ServerConfig};

pub struct Iterative {
    verbose: bool,
}

impl Iterative {
    pub fn new(verbose: bool) -> Self {
        Iterative { verbose }
    }

    /// Drive one connection to completion using the §8.1 blocking skeleton. Any
    /// I/O error is returned to the caller, which logs it and moves on — it is
    /// never propagated out of the accept loop.
    fn handle_conn(&self, mut stream: TcpStream, cfg: &ServerConfig, app: &App) -> std::io::Result<()> {
        stream.set_read_timeout(Some(cfg.read_timeout))?;
        stream.set_write_timeout(Some(cfg.write_timeout))?;

        let mut conn = Connection::new(cfg.read_timeout);
        let mut buf = [0u8; READ_CHUNK];
        let mut action = ConnAction::WantRead;
        loop {
            match action {
                ConnAction::WantRead => {
                    let n = stream.read(&mut buf)?;
                    if n == 0 {
                        break; // peer closed
                    }
                    action = conn.on_bytes(&buf[..n], app);
                }
                ConnAction::WantWrite => {
                    let w = stream.write(conn.pending_write())?;
                    action = conn.on_written(w);
                }
                ConnAction::Close => break,
            }
        }
        Ok(())
    }
}

impl Server for Iterative {
    fn name(&self) -> &'static str {
        "iterative"
    }

    fn serve(&self, cfg: &ServerConfig, app: Arc<App>) -> std::io::Result<()> {
        let listener = bind_listener(cfg.addr, false)?;
        // Startup log only — not on the hot path.
        eprintln!("iterative: listening on http://{}", cfg.addr);

        loop {
            match listener.accept() {
                Ok((stream, _peer)) => {
                    app.metrics().inc_connections();
                    if let Err(e) = self.handle_conn(stream, cfg, &app) {
                        app.metrics().inc_errors();
                        if self.verbose {
                            eprintln!("iterative: connection error: {e}");
                        }
                    }
                }
                Err(e) => {
                    // An accept() failure must not kill the server.
                    if self.verbose {
                        eprintln!("iterative: accept error: {e}");
                    }
                }
            }
        }
    }
}
