//! The incremental, sans-IO HTTP request parser (the crux of Phase 0).

use super::headers::Headers;
use super::method::{Method, Version};
use super::response::StatusCode;
use crate::limits::{MAX_BODY_BYTES, MAX_HEADER_BYTES, MAX_HEADER_COUNT, MAX_REQUEST_LINE};

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
        match self.version {
            Version::Http11 => !self.connection_has("close"),
            Version::Http10 => self.connection_has("keep-alive"),
        }
    }

    /// Content-Length value, or 0 if absent (or unparseable).
    pub fn content_length(&self) -> usize {
        self.headers
            .get("content-length")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    }

    /// True if the `Connection` header carries `token` as one of its
    /// comma-separated values (case-insensitive).
    fn connection_has(&self, token: &str) -> bool {
        self.headers.get("connection").is_some_and(|v| {
            v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token))
        })
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
    pub fn status(&self) -> StatusCode {
        match self {
            ParseError::MalformedRequestLine => StatusCode::BAD_REQUEST,
            ParseError::MalformedHeader => StatusCode::BAD_REQUEST,
            ParseError::RequestLineTooLong => StatusCode::URI_TOO_LONG,
            ParseError::HeadersTooLarge => StatusCode::HEADER_FIELDS_TOO_LARGE,
            ParseError::TooManyHeaders => StatusCode::HEADER_FIELDS_TOO_LARGE,
            ParseError::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ParseError::UnsupportedVersion => StatusCode::VERSION_NOT_SUPPORTED,
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    RequestLine,
    Headers,
    Body,
    Done,
}

pub struct RequestParser {
    /// Accumulation buffer holding all bytes of the *current* request (plus any
    /// leftover bytes belonging to the next request, after a `Complete`).
    buf: Vec<u8>,
    state: State,
    /// Start of the line currently being parsed (request line or a header).
    line_start: usize,
    /// Resume index for the CRLF search, so byte-at-a-time feeding stays linear
    /// instead of rescanning the whole buffer on every `push`.
    scanned: usize,
    /// Index in `buf` where the header block begins (used for MAX_HEADER_BYTES).
    header_block_start: usize,
    /// Index in `buf` where the body begins.
    body_start: usize,
    /// Number of header fields parsed so far.
    header_count: usize,
    /// Declared Content-Length for the current request.
    content_length: usize,
    // Accumulated request parts:
    method: Method,
    path: String,
    version: Version,
    headers: Headers,
}

impl Default for RequestParser {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestParser {
    pub fn new() -> Self {
        RequestParser {
            buf: Vec::new(),
            state: State::RequestLine,
            line_start: 0,
            scanned: 0,
            header_block_start: 0,
            body_start: 0,
            header_count: 0,
            content_length: 0,
            method: Method::Unsupported,
            path: String::new(),
            version: Version::Http11,
            headers: Headers::new(),
        }
    }

    /// SANS-IO. Append `bytes` to the internal buffer and advance parsing.
    pub fn push(&mut self, bytes: &[u8]) -> ParseStatus {
        self.buf.extend_from_slice(bytes);
        self.advance()
    }

    /// Prepare to parse the next request on a kept-alive connection. The bytes
    /// retained after the previous `Complete { consumed }` (already drained to
    /// the front of `buf`) are kept so a pipelined request is not lost.
    pub fn reset(&mut self) {
        self.state = State::RequestLine;
        self.line_start = 0;
        self.scanned = 0;
        self.header_block_start = 0;
        self.body_start = 0;
        self.header_count = 0;
        self.content_length = 0;
        self.method = Method::Unsupported;
        self.path = String::new();
        self.version = Version::Http11;
    }

    fn advance(&mut self) -> ParseStatus {
        loop {
            match self.state {
                State::RequestLine => match self.parse_request_line() {
                    Ok(true) => continue,
                    Ok(false) => return ParseStatus::Incomplete,
                    Err(e) => return self.fail(e),
                },
                State::Headers => match self.parse_headers() {
                    Ok(true) => continue,
                    Ok(false) => return ParseStatus::Incomplete,
                    Err(e) => return self.fail(e),
                },
                State::Body => match self.parse_body() {
                    Ok(Some(status)) => return status,
                    Ok(None) => return ParseStatus::Incomplete,
                    Err(e) => return self.fail(e),
                },
                // A request was already produced; nothing more to do until reset.
                State::Done => return ParseStatus::Incomplete,
            }
        }
    }

    fn fail(&mut self, e: ParseError) -> ParseStatus {
        self.state = State::Done;
        ParseStatus::Failed(e)
    }

    /// Index of the `\r` of the next CRLF at or after `self.line_start`,
    /// resuming from `self.scanned`. Returns None if no complete CRLF is
    /// buffered yet.
    fn next_crlf(&mut self) -> Option<usize> {
        let mut i = self.scanned.max(self.line_start);
        while i + 1 < self.buf.len() {
            if self.buf[i] == b'\r' && self.buf[i + 1] == b'\n' {
                return Some(i);
            }
            i += 1;
        }
        // No CRLF yet; resume here next time (i may point at a lone trailing
        // '\r' whose '\n' has not arrived).
        self.scanned = i;
        None
    }

    fn parse_request_line(&mut self) -> Result<bool, ParseError> {
        let eol = match self.next_crlf() {
            None => {
                if self.buf.len() - self.line_start > MAX_REQUEST_LINE {
                    return Err(ParseError::RequestLineTooLong);
                }
                return Ok(false);
            }
            Some(eol) => eol,
        };

        if eol - self.line_start > MAX_REQUEST_LINE {
            return Err(ParseError::RequestLineTooLong);
        }

        let line = &self.buf[self.line_start..eol];
        let mut parts = line.split(|&b| b == b' ');
        let (m, p, v) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(m), Some(p), Some(v), None)
                if !m.is_empty() && !p.is_empty() && !v.is_empty() =>
            {
                (m, p, v)
            }
            _ => return Err(ParseError::MalformedRequestLine),
        };

        let path = std::str::from_utf8(p).map_err(|_| ParseError::MalformedRequestLine)?;
        let version = Version::parse(v).ok_or(ParseError::UnsupportedVersion)?;

        self.method = Method::parse(m);
        self.path = path.to_string();
        self.version = version;

        self.line_start = eol + 2;
        self.scanned = self.line_start;
        self.header_block_start = self.line_start;
        self.state = State::Headers;
        Ok(true)
    }

    fn parse_headers(&mut self) -> Result<bool, ParseError> {
        loop {
            let eol = match self.next_crlf() {
                None => {
                    if self.buf.len() - self.header_block_start > MAX_HEADER_BYTES {
                        return Err(ParseError::HeadersTooLarge);
                    }
                    return Ok(false);
                }
                Some(eol) => eol,
            };

            if (eol + 2) - self.header_block_start > MAX_HEADER_BYTES {
                return Err(ParseError::HeadersTooLarge);
            }

            // An empty line terminates the header block.
            if eol == self.line_start {
                self.body_start = eol + 2;
                self.determine_content_length()?;
                self.state = State::Body;
                return Ok(true);
            }

            let line = &self.buf[self.line_start..eol];
            let colon = line
                .iter()
                .position(|&b| b == b':')
                .ok_or(ParseError::MalformedHeader)?;
            let name = std::str::from_utf8(&line[..colon])
                .map_err(|_| ParseError::MalformedHeader)?
                .trim();
            let value = std::str::from_utf8(&line[colon + 1..])
                .map_err(|_| ParseError::MalformedHeader)?
                .trim();
            if name.is_empty() {
                return Err(ParseError::MalformedHeader);
            }

            self.header_count += 1;
            if self.header_count > MAX_HEADER_COUNT {
                return Err(ParseError::TooManyHeaders);
            }
            self.headers.insert(name, value);

            self.line_start = eol + 2;
            self.scanned = self.line_start;
        }
    }

    fn determine_content_length(&mut self) -> Result<(), ParseError> {
        match self.headers.get("content-length") {
            None => self.content_length = 0,
            Some(v) => {
                let n: usize = v.trim().parse().map_err(|_| ParseError::MalformedHeader)?;
                if n > MAX_BODY_BYTES {
                    return Err(ParseError::BodyTooLarge);
                }
                self.content_length = n;
            }
        }
        Ok(())
    }

    fn parse_body(&mut self) -> Result<Option<ParseStatus>, ParseError> {
        let available = self.buf.len() - self.body_start;
        if available < self.content_length {
            return Ok(None);
        }

        let body_end = self.body_start + self.content_length;
        let body = self.buf[self.body_start..body_end].to_vec();
        let request = Request {
            method: self.method,
            path: std::mem::take(&mut self.path),
            version: self.version,
            headers: std::mem::take(&mut self.headers),
            body,
        };

        let consumed = body_end;
        // Drop the consumed bytes; retain everything after for the next request.
        self.buf.drain(..consumed);
        self.state = State::Done;
        Ok(Some(ParseStatus::Complete { request, consumed }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete(status: ParseStatus) -> (Request, usize) {
        match status {
            ParseStatus::Complete { request, consumed } => (request, consumed),
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    fn failed(status: ParseStatus) -> ParseError {
        match status {
            ParseStatus::Failed(e) => e,
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn well_formed_request() {
        let raw = b"GET /index.html HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\n\r\n";
        let mut p = RequestParser::new();
        let (req, consumed) = complete(p.push(raw));
        assert_eq!(req.method, Method::Get);
        assert_eq!(req.path, "/index.html");
        assert_eq!(req.version, Version::Http11);
        assert_eq!(req.headers.get("host"), Some("example.com"));
        assert_eq!(req.headers.get("accept"), Some("*/*"));
        assert!(req.body.is_empty());
        assert_eq!(consumed, raw.len());
        assert!(req.wants_keep_alive());
    }

    #[test]
    fn head_request() {
        let raw = b"HEAD / HTTP/1.1\r\n\r\n";
        let mut p = RequestParser::new();
        let (req, _) = complete(p.push(raw));
        assert_eq!(req.method, Method::Head);
        assert_eq!(req.path, "/");
    }

    #[test]
    fn byte_at_a_time() {
        let raw = b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n";
        let mut p = RequestParser::new();
        for (idx, byte) in raw.iter().enumerate() {
            let status = p.push(&[*byte]);
            if idx + 1 < raw.len() {
                assert!(
                    matches!(status, ParseStatus::Incomplete),
                    "byte {idx} should be Incomplete, got {status:?}"
                );
            } else {
                let (req, consumed) = complete(status);
                assert_eq!(req.method, Method::Get);
                assert_eq!(req.path, "/a");
                assert_eq!(req.headers.get("host"), Some("x"));
                assert_eq!(consumed, raw.len());
            }
        }
    }

    #[test]
    fn body_honored_and_leftover_retained() {
        // Two pipelined requests in one buffer; first carries a 5-byte body.
        let raw = b"GET /a HTTP/1.1\r\nContent-Length: 5\r\n\r\nhelloGET /b HTTP/1.1\r\n\r\n";
        let mut p = RequestParser::new();
        let (req1, consumed) = complete(p.push(raw));
        assert_eq!(req1.path, "/a");
        assert_eq!(req1.body, b"hello");
        assert_eq!(req1.content_length(), 5);
        assert_eq!(consumed, "GET /a HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello".len());

        // Leftover bytes are retained; reset + advance yields the second request.
        p.reset();
        let (req2, _) = complete(p.push(&[]));
        assert_eq!(req2.path, "/b");
        assert!(req2.body.is_empty());
    }

    #[test]
    fn keep_alive_rules() {
        let cases: &[(&[u8], bool)] = &[
            (b"GET / HTTP/1.1\r\n\r\n", true),
            (b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n", false),
            (b"GET / HTTP/1.0\r\n\r\n", false),
            (b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n", true),
            (b"GET / HTTP/1.1\r\nConnection: keep-alive, foo\r\n\r\n", true),
        ];
        for (raw, expected) in cases {
            let mut p = RequestParser::new();
            let (req, _) = complete(p.push(raw));
            assert_eq!(req.wants_keep_alive(), *expected, "case {raw:?}");
        }
    }

    #[test]
    fn malformed_request_line() {
        let mut p = RequestParser::new();
        let e = failed(p.push(b"GET /\r\n\r\n")); // only two tokens
        assert!(matches!(e, ParseError::MalformedRequestLine));
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn unsupported_version() {
        let mut p = RequestParser::new();
        let e = failed(p.push(b"GET / HTTP/2.0\r\n\r\n"));
        assert!(matches!(e, ParseError::UnsupportedVersion));
        assert_eq!(e.status(), StatusCode::VERSION_NOT_SUPPORTED);
    }

    #[test]
    fn malformed_header() {
        let mut p = RequestParser::new();
        let e = failed(p.push(b"GET / HTTP/1.1\r\nNoColonHere\r\n\r\n"));
        assert!(matches!(e, ParseError::MalformedHeader));
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn request_line_too_long() {
        let mut raw = Vec::from(&b"GET /"[..]);
        raw.extend(std::iter::repeat_n(b'a', MAX_REQUEST_LINE + 10));
        // No CRLF yet: the ceiling must trip on accumulation alone.
        let mut p = RequestParser::new();
        let e = failed(p.push(&raw));
        assert!(matches!(e, ParseError::RequestLineTooLong));
        assert_eq!(e.status(), StatusCode::URI_TOO_LONG);
    }

    #[test]
    fn headers_too_large() {
        let mut raw = Vec::from(&b"GET / HTTP/1.1\r\nX: "[..]);
        raw.extend(std::iter::repeat_n(b'a', MAX_HEADER_BYTES + 10));
        // No terminating empty line: ceiling trips on accumulation.
        let mut p = RequestParser::new();
        let e = failed(p.push(&raw));
        assert!(matches!(e, ParseError::HeadersTooLarge));
        assert_eq!(e.status(), StatusCode::HEADER_FIELDS_TOO_LARGE);
    }

    #[test]
    fn too_many_headers() {
        let mut raw = Vec::from(&b"GET / HTTP/1.1\r\n"[..]);
        for i in 0..(MAX_HEADER_COUNT + 1) {
            raw.extend_from_slice(format!("H{i}: v\r\n").as_bytes());
        }
        raw.extend_from_slice(b"\r\n");
        let mut p = RequestParser::new();
        let e = failed(p.push(&raw));
        assert!(matches!(e, ParseError::TooManyHeaders));
        assert_eq!(e.status(), StatusCode::HEADER_FIELDS_TOO_LARGE);
    }

    #[test]
    fn body_too_large() {
        let raw = format!("GET / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1);
        let mut p = RequestParser::new();
        let e = failed(p.push(raw.as_bytes()));
        assert!(matches!(e, ParseError::BodyTooLarge));
        assert_eq!(e.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn invalid_content_length_is_malformed() {
        let mut p = RequestParser::new();
        let e = failed(p.push(b"GET / HTTP/1.1\r\nContent-Length: abc\r\n\r\n"));
        assert!(matches!(e, ParseError::MalformedHeader));
    }
}
