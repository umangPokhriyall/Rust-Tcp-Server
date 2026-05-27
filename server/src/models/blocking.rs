//! The shared blocking per-connection serve loop (phase0-spec §8.1).
//!
//! Every model that dedicates a *process* or *thread* to a connection runs this
//! same loop — `forking`'s child, each `preforked` worker, and (later)
//! `thread-per-conn` / `thread-pool`. Keeping it in one place is the
//! "one abstraction, many implementations" rule: the models differ only in how
//! they obtain the thread/process to run it on, never in the loop itself.

use std::io::{Read, Write};
use std::net::TcpStream;

use core::limits::READ_CHUNK;
use core::{App, ConnAction, Connection, ServerConfig};

/// Drive one connection to completion. Read/write timeouts come from `cfg`, so a
/// slow-loris client is dropped rather than pinning the worker forever. Any I/O
/// error is returned to the caller, which logs it off the hot path and moves on
/// — a per-client failure never escapes to kill the accept loop.
pub fn serve_connection(
    mut stream: TcpStream,
    cfg: &ServerConfig,
    app: &App,
) -> std::io::Result<()> {
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
