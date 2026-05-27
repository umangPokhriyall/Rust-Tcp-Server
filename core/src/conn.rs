//! The per-connection state machine driven by every model. Sans-IO: the model
//! performs all reads/writes and feeds/drains bytes here.

use crate::app::App;

/// What the connection wants the model to do next.
#[derive(Debug, PartialEq, Eq)]
pub enum ConnAction {
    /// Wait for readability, then call `on_bytes`.
    WantRead,
    /// Wait for writability, then write `pending_write` + call `on_written`.
    WantWrite,
    /// Close the fd and drop this Connection.
    Close,
}

pub struct Connection {
    // parser, state, keep_alive flag, deadline — all private. Built in Session D.
}

impl Connection {
    pub fn new(_read_timeout: std::time::Duration) -> Self {
        todo!()
    }

    /// Feed bytes the model just read from the socket. Parses; on a complete
    /// request, routes via `app` and encodes the response into the internal
    /// write buffer. On a parse error, internally builds the error response,
    /// so the model never has to. Returns the next action.
    pub fn on_bytes(&mut self, _bytes: &[u8], _app: &App) -> ConnAction {
        todo!()
    }

    /// Bytes the model should write now (empty slice if nothing pending).
    pub fn pending_write(&self) -> &[u8] {
        todo!()
    }

    /// Model reports it wrote `n` bytes of `pending_write`. Advances the
    /// write offset; when fully drained, transitions to Reading (keep-alive,
    /// and the read deadline is refreshed) or Closing. Returns next action.
    pub fn on_written(&mut self, _n: usize) -> ConnAction {
        todo!()
    }

    /// True if the connection exceeded its deadline.
    pub fn is_expired(&self, _now: std::time::Instant) -> bool {
        todo!()
    }
}
