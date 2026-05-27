//! The per-connection state machine driven by every model. Sans-IO: the model
//! performs all reads/writes and feeds/drains bytes here.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::http::method::Method;
use crate::http::request::{ParseStatus, RequestParser};
use crate::http::response::Response;

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

#[derive(Debug, PartialEq, Eq)]
enum State {
    /// Waiting for (more) request bytes.
    Reading,
    /// A response is buffered and being drained to the socket.
    Writing,
    /// Fully done; the model should close the fd.
    Closed,
}

pub struct Connection {
    parser: RequestParser,
    state: State,
    /// Encoded response awaiting the socket, and how far we have written.
    write_buf: Vec<u8>,
    write_pos: usize,
    /// Whether to close once the current response is fully written.
    close_after_write: bool,
    /// How long a kept-alive connection may idle before the read deadline.
    read_timeout: Duration,
    /// When the current read phase expires.
    deadline: Instant,
}

impl Connection {
    pub fn new(read_timeout: std::time::Duration) -> Self {
        Connection {
            parser: RequestParser::new(),
            state: State::Reading,
            write_buf: Vec::new(),
            write_pos: 0,
            close_after_write: false,
            read_timeout,
            deadline: Instant::now() + read_timeout,
        }
    }

    /// Feed bytes the model just read from the socket. Parses; on a complete
    /// request, routes via `app` and encodes the response into the internal
    /// write buffer. On a parse error, internally builds the error response
    /// (status from `ParseError::status()`), so the model never has to.
    /// Returns the next action.
    pub fn on_bytes(&mut self, bytes: &[u8], app: &App) -> ConnAction {
        if self.state != State::Reading {
            return self.action_for_state();
        }
        let status = self.parser.push(bytes);
        self.handle_status(status, app)
    }

    /// Bytes the model should write now (empty slice if nothing pending).
    pub fn pending_write(&self) -> &[u8] {
        match self.state {
            State::Writing => &self.write_buf[self.write_pos..],
            _ => &[],
        }
    }

    /// Model reports it wrote `n` bytes of `pending_write`. Advances the write
    /// offset; when fully drained, transitions to Reading (keep-alive, refreshing
    /// the read deadline) or Closed. Returns the next action.
    pub fn on_written(&mut self, n: usize) -> ConnAction {
        if self.state != State::Writing {
            return self.action_for_state();
        }
        self.write_pos += n;
        if self.write_pos < self.write_buf.len() {
            // Partial write — keep draining.
            return ConnAction::WantWrite;
        }

        if self.close_after_write {
            self.state = State::Closed;
            return ConnAction::Close;
        }

        // Keep-alive: ready for the next request. `reset` retains any bytes that
        // arrived past this request (a pipelined request is not lost), and the
        // read deadline is refreshed.
        self.parser.reset();
        self.write_buf.clear();
        self.write_pos = 0;
        self.state = State::Reading;
        self.deadline = Instant::now() + self.read_timeout;
        ConnAction::WantRead
    }

    /// True if the connection exceeded its read deadline.
    pub fn is_expired(&self, now: std::time::Instant) -> bool {
        now >= self.deadline
    }

    fn handle_status(&mut self, status: ParseStatus, app: &App) -> ConnAction {
        match status {
            ParseStatus::Incomplete => {
                self.state = State::Reading;
                ConnAction::WantRead
            }
            ParseStatus::Complete { request, .. } => {
                let include_body = request.method != Method::Head;
                let keep_alive = request.wants_keep_alive();
                let response = app.handle(&request);
                self.start_writing(&response, keep_alive, include_body);
                ConnAction::WantWrite
            }
            ParseStatus::Failed(err) => {
                // One bad request never reaches the model: we answer with the
                // mapped status and close. This is the one-place fix for the
                // legacy "one malformed request kills the server" bug.
                let status = err.status();
                let body = format!("{} {}\n", status.0, status.reason());
                let response = Response::text(status, &body);
                self.start_writing(&response, false, true);
                ConnAction::WantWrite
            }
        }
    }

    fn start_writing(&mut self, response: &Response, keep_alive: bool, include_body: bool) {
        self.write_buf.clear();
        self.write_pos = 0;
        response.encode(keep_alive, include_body, &mut self.write_buf);
        self.close_after_write = !keep_alive;
        self.state = State::Writing;
    }

    fn action_for_state(&self) -> ConnAction {
        match self.state {
            State::Reading => ConnAction::WantRead,
            State::Writing => ConnAction::WantWrite,
            State::Closed => ConnAction::Close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::method::Method;
    use crate::http::request::Request;
    use crate::http::response::{Response, StatusCode};

    fn ok_handler(_req: &Request, _app: &App) -> Response {
        Response::text(StatusCode::OK, "hello")
    }

    fn test_app() -> App {
        App::builder()
            .route(Method::Get, "/", ok_handler)
            .build()
            .unwrap()
    }

    /// Drive a connection's pending write to completion in one `on_written`,
    /// returning the bytes written and the resulting action.
    fn flush(conn: &mut Connection) -> (Vec<u8>, ConnAction) {
        let bytes = conn.pending_write().to_vec();
        let action = conn.on_written(bytes.len());
        (bytes, action)
    }

    #[test]
    fn serves_get_then_keeps_alive() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));

        let action = conn.on_bytes(b"GET / HTTP/1.1\r\n\r\n", &app);
        assert_eq!(action, ConnAction::WantWrite);

        let (bytes, action) = flush(&mut conn);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"), "{s:?}");
        assert!(s.contains("Connection: keep-alive\r\n"));
        assert!(s.ends_with("\r\n\r\nhello"));
        // Keep-alive: back to reading for the next request.
        assert_eq!(action, ConnAction::WantRead);

        // Second request reuses the same connection.
        let action = conn.on_bytes(b"GET / HTTP/1.1\r\n\r\n", &app);
        assert_eq!(action, ConnAction::WantWrite);
    }

    #[test]
    fn connection_close_closes_after_write() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));

        let action = conn.on_bytes(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n", &app);
        assert_eq!(action, ConnAction::WantWrite);

        let (bytes, action) = flush(&mut conn);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("Connection: close\r\n"));
        assert_eq!(action, ConnAction::Close);
    }

    #[test]
    fn malformed_request_answers_400_then_closes() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));

        // Only two tokens on the request line.
        let action = conn.on_bytes(b"GET /\r\n\r\n", &app);
        assert_eq!(action, ConnAction::WantWrite);

        let (bytes, action) = flush(&mut conn);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 400 Bad Request\r\n"), "{s:?}");
        assert!(s.contains("Connection: close\r\n"));
        assert_eq!(action, ConnAction::Close);
    }

    #[test]
    fn head_omits_body_but_keeps_content_length() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));

        let action = conn.on_bytes(b"HEAD / HTTP/1.1\r\n\r\n", &app);
        assert_eq!(action, ConnAction::WantWrite);

        let (bytes, _) = flush(&mut conn);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Length: 5\r\n"));
        assert!(s.ends_with("\r\n\r\n"), "body must be omitted: {s:?}");
        assert!(!s.contains("hello"));
    }

    #[test]
    fn incomplete_request_wants_more() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));
        let action = conn.on_bytes(b"GET / HTTP/1.1\r\nHost: ", &app);
        assert_eq!(action, ConnAction::WantRead);
        assert!(conn.pending_write().is_empty());
    }

    #[test]
    fn partial_write_keeps_draining() {
        let app = test_app();
        let mut conn = Connection::new(Duration::from_secs(5));
        conn.on_bytes(b"GET / HTTP/1.1\r\n\r\n", &app);

        let total = conn.pending_write().len();
        assert!(total > 4);
        let action = conn.on_written(4); // wrote only part
        assert_eq!(action, ConnAction::WantWrite);
        assert_eq!(conn.pending_write().len(), total - 4);
    }

    #[test]
    fn deadline_expires_in_the_future() {
        let conn = Connection::new(Duration::from_secs(5));
        assert!(!conn.is_expired(Instant::now()));
        assert!(conn.is_expired(Instant::now() + Duration::from_secs(10)));
    }
}

#[cfg(test)]
mod skeletons {
    //! These functions exist only to prove the §8.1 usage skeletons type-check
    //! against the `Connection` API. They are never executed.
    #![allow(dead_code)]

    use super::*;
    use std::io::{Read, Write};

    /// Blocking model (`iterative`, `forking`, `thread-per-conn`, `thread-pool`).
    fn blocking_model<S: Read + Write>(
        mut stream: S,
        app: &App,
        read_timeout: Duration,
    ) -> std::io::Result<()> {
        let mut conn = Connection::new(read_timeout);
        let mut buf = [0u8; 16 * 1024];
        let mut action = ConnAction::WantRead;
        loop {
            match action {
                ConnAction::WantRead => {
                    let n = stream.read(&mut buf)?;
                    if n == 0 {
                        break;
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

    /// Event-loop model (`poll`, `epoll-*`, `event-loop`, `multireactor`,
    /// `io-uring`): the readiness callbacks map an action to epoll interest.
    fn event_loop_readable(conn: &mut Connection, app: &App, chunk: &[u8]) -> ConnAction {
        conn.on_bytes(chunk, app)
    }

    fn event_loop_writable(conn: &mut Connection, written: usize) -> ConnAction {
        let _pending: &[u8] = conn.pending_write();
        conn.on_written(written)
    }

    fn event_loop_tick(conn: &Connection, now: Instant) -> bool {
        conn.is_expired(now)
    }
}
