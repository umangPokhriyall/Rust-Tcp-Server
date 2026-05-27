//! HTTP method and version tokens.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Head,
    Unsupported,
}

impl Method {
    /// GET -> Get, HEAD -> Head, anything else -> Unsupported.
    pub fn parse(_token: &[u8]) -> Method {
        todo!()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Http10,
    Http11,
}

impl Version {
    /// "HTTP/1.0" / "HTTP/1.1" -> Some(..); anything else -> None.
    pub fn parse(_token: &[u8]) -> Option<Version> {
        todo!()
    }
}
