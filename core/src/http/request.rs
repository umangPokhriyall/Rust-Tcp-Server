//! The incremental, sans-IO HTTP request parser (the crux of Phase 0).

use super::headers::Headers;
use super::method::{Method, Version};
use super::response::StatusCode;

#[derive(Debug)]
pub struct Request {
    pub method: Method,
    pub path: String,
    pub version: Version,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    /// HTTP/1.1: keep-alive unless `Connection: close`.
    /// HTTP/1.0: close unless `Connection: keep-alive`.
    pub fn wants_keep_alive(&self) -> bool {
        todo!()
    }

    /// Content-Length value, or 0 if absent.
    pub fn content_length(&self) -> usize {
        todo!()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ParseError {
    MalformedRequestLine,
    UnsupportedVersion,
    MalformedHeader,
    RequestLineTooLong,
    HeadersTooLarge,
    TooManyHeaders,
    BodyTooLarge,
}

impl ParseError {
    /// The status to answer with before closing the connection.
    /// e.g. MalformedRequestLine->400, RequestLineTooLong->414,
    /// HeadersTooLarge/TooManyHeaders->431, BodyTooLarge->413,
    /// UnsupportedVersion->505.
    pub fn status(&self) -> StatusCode {
        todo!()
    }
}

#[derive(Debug)]
pub enum ParseStatus {
    /// Need more bytes. Caller reads more and calls `push` again.
    Incomplete,
    /// A full request is ready. `consumed` = bytes used from the input
    /// stream so far; bytes beyond it belong to the NEXT request.
    Complete { request: Request, consumed: usize },
    /// Fatal. Caller answers with `error.status()`, then closes.
    Failed(ParseError),
}

pub struct RequestParser {
    // Internal accumulation buffer + state machine (implemented in Session B).
}

impl RequestParser {
    pub fn new() -> Self {
        todo!()
    }

    /// SANS-IO. Append `bytes` to the internal buffer and advance parsing.
    /// MUST be safe to call repeatedly. MUST handle the request arriving
    /// one byte at a time. MUST NOT block or perform I/O.
    pub fn push(&mut self, _bytes: &[u8]) -> ParseStatus {
        todo!()
    }

    /// Prepare to parse the next request on a kept-alive connection.
    /// Bytes received after the previous `Complete { consumed }` MUST be
    /// retained (so a pipelined request is not lost).
    pub fn reset(&mut self) {
        todo!()
    }
}
