//! HTTP response model and sans-IO encoder.

use super::headers::Headers;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCode(pub u16);

impl StatusCode {
    pub const OK: StatusCode = StatusCode(200);
    pub const BAD_REQUEST: StatusCode = StatusCode(400);
    pub const NOT_FOUND: StatusCode = StatusCode(404);
    pub const METHOD_NOT_ALLOWED: StatusCode = StatusCode(405);
    pub const URI_TOO_LONG: StatusCode = StatusCode(414);
    pub const PAYLOAD_TOO_LARGE: StatusCode = StatusCode(413);
    pub const HEADER_FIELDS_TOO_LARGE: StatusCode = StatusCode(431);
    pub const INTERNAL_ERROR: StatusCode = StatusCode(500);
    pub const VERSION_NOT_SUPPORTED: StatusCode = StatusCode(505);

    pub fn reason(&self) -> &'static str {
        match self.0 {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            405 => "Method Not Allowed",
            413 => "Payload Too Large",
            414 => "URI Too Long",
            431 => "Request Header Fields Too Large",
            500 => "Internal Server Error",
            505 => "HTTP Version Not Supported",
            _ => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Body {
    Empty,
    /// Asset bodies are Arc-shared — handlers clone the Arc, never the bytes.
    Bytes(Arc<[u8]>),
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: Headers,
    pub body: Body,
}

impl Response {
    pub fn new(status: StatusCode) -> Self {
        Response {
            status,
            headers: Headers::new(),
            body: Body::Empty,
        }
    }

    pub fn ok(content_type: &str, body: Arc<[u8]>) -> Self {
        let mut r = Response::new(StatusCode::OK);
        r.headers.insert("Content-Type", content_type);
        r.body = Body::Bytes(body);
        r
    }

    pub fn text(status: StatusCode, msg: &str) -> Self {
        let mut r = Response::new(status);
        r.headers.insert("Content-Type", "text/plain; charset=utf-8");
        r.body = Body::Bytes(Arc::from(msg.as_bytes()));
        r
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// SANS-IO. Serialize status line + headers + body into `out`.
    /// Writes a correct `Content-Length` and a `Connection` header consistent
    /// with `keep_alive`. When `include_body` is false (HEAD), writes headers +
    /// Content-Length but omits the body bytes.
    pub fn encode(&self, keep_alive: bool, include_body: bool, out: &mut Vec<u8>) {
        let body_bytes: &[u8] = match &self.body {
            Body::Empty => &[],
            Body::Bytes(b) => &b[..],
        };

        // Status line. We always respond as HTTP/1.1.
        out.extend_from_slice(b"HTTP/1.1 ");
        write_u16(out, self.status.0);
        out.push(b' ');
        out.extend_from_slice(self.status.reason().as_bytes());
        out.extend_from_slice(b"\r\n");

        // User headers, minus the ones we own (Content-Length / Connection),
        // so they are never duplicated or contradicted.
        for (k, v) in self.headers.iter() {
            if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("connection") {
                continue;
            }
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }

        // Content-Length is sent even for HEAD (it describes the entity).
        out.extend_from_slice(b"Content-Length: ");
        write_u16_usize(out, body_bytes.len());
        out.extend_from_slice(b"\r\n");

        out.extend_from_slice(b"Connection: ");
        out.extend_from_slice(if keep_alive { b"keep-alive" } else { b"close" });
        out.extend_from_slice(b"\r\n");

        // End of header block.
        out.extend_from_slice(b"\r\n");

        if include_body {
            out.extend_from_slice(body_bytes);
        }
    }
}

/// Append the decimal form of a `u16` to `out` without allocating.
fn write_u16(out: &mut Vec<u8>, n: u16) {
    write_u16_usize(out, n as usize);
}

/// Append the decimal form of a `usize` to `out` without allocating.
fn write_u16_usize(out: &mut Vec<u8>, mut n: usize) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut tmp = [0u8; 20]; // usize::MAX is 20 decimal digits
    let mut i = tmp.len();
    while n > 0 {
        i -= 1;
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    out.extend_from_slice(&tmp[i..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(resp: &Response, keep_alive: bool, include_body: bool) -> String {
        let mut out = Vec::new();
        resp.encode(keep_alive, include_body, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn reason_phrases() {
        assert_eq!(StatusCode::OK.reason(), "OK");
        assert_eq!(StatusCode::NOT_FOUND.reason(), "Not Found");
        assert_eq!(StatusCode::URI_TOO_LONG.reason(), "URI Too Long");
        assert_eq!(StatusCode::VERSION_NOT_SUPPORTED.reason(), "HTTP Version Not Supported");
    }

    #[test]
    fn encode_ok_with_body() {
        let resp = Response::ok("text/html", Arc::from(b"<h1>hi</h1>".as_slice()));
        let s = encoded(&resp, true, true);
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"), "status line: {s:?}");
        assert!(s.contains("Content-Type: text/html\r\n"));
        assert!(s.contains("Content-Length: 11\r\n"));
        assert!(s.contains("Connection: keep-alive\r\n"));
        assert!(s.ends_with("\r\n\r\n<h1>hi</h1>"));
    }

    #[test]
    fn encode_head_omits_body_but_keeps_length() {
        let resp = Response::ok("text/html", Arc::from(b"<h1>hi</h1>".as_slice()));
        let s = encoded(&resp, false, false);
        assert!(s.contains("Content-Length: 11\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.ends_with("\r\n\r\n"), "body must be omitted: {s:?}");
        assert!(!s.contains("<h1>hi</h1>"));
    }

    #[test]
    fn encode_empty_body_has_zero_length() {
        let resp = Response::new(StatusCode::NOT_FOUND);
        let s = encoded(&resp, true, true);
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("Content-Length: 0\r\n"));
    }

    #[test]
    fn encode_drops_user_supplied_content_length_and_connection() {
        let resp = Response::new(StatusCode::OK)
            .with_header("Content-Length", "999")
            .with_header("Connection", "upgrade")
            .with_header("X-Custom", "yes");
        let s = encoded(&resp, true, true);
        assert!(s.contains("Content-Length: 0\r\n"));
        assert!(!s.contains("999"));
        assert!(s.contains("Connection: keep-alive\r\n"));
        assert!(!s.contains("upgrade"));
        assert!(s.contains("X-Custom: yes\r\n"));
    }

    #[test]
    fn text_sets_plain_content_type() {
        let resp = Response::text(StatusCode::BAD_REQUEST, "bad");
        let s = encoded(&resp, false, true);
        assert!(s.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(s.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(s.contains("Content-Length: 3\r\n"));
        assert!(s.ends_with("\r\n\r\nbad"));
    }
}
