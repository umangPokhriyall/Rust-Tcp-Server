//! HTTP response model and sans-IO encoder.

use super::headers::Headers;

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
        todo!()
    }
}

#[derive(Debug, Clone)]
pub enum Body {
    Empty,
    /// Asset bodies are Arc-shared — handlers clone the Arc, never the bytes.
    Bytes(std::sync::Arc<[u8]>),
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: Headers,
    pub body: Body,
}

impl Response {
    pub fn new(_status: StatusCode) -> Self {
        todo!()
    }

    pub fn ok(_content_type: &str, _body: std::sync::Arc<[u8]>) -> Self {
        todo!()
    }

    pub fn text(_status: StatusCode, _msg: &str) -> Self {
        todo!()
    }

    pub fn with_header(self, _name: &str, _value: &str) -> Self {
        todo!()
    }

    /// SANS-IO. Serialize status line + headers + body into `out`.
    /// MUST write a correct `Content-Length` and a `Connection` header
    /// consistent with `keep_alive`. When `include_body` is false (HEAD),
    /// write headers + Content-Length but omit the body bytes.
    pub fn encode(&self, _keep_alive: bool, _include_body: bool, _out: &mut Vec<u8>) {
        todo!()
    }
}
