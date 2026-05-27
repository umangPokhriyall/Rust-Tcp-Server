//! HTTP method and version tokens.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Head,
    Unsupported,
}

impl Method {
    /// GET -> Get, HEAD -> Head, anything else -> Unsupported.
    pub fn parse(token: &[u8]) -> Method {
        match token {
            b"GET" => Method::Get,
            b"HEAD" => Method::Head,
            _ => Method::Unsupported,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Http10,
    Http11,
}

impl Version {
    /// "HTTP/1.0" / "HTTP/1.1" -> Some(..); anything else -> None.
    pub fn parse(token: &[u8]) -> Option<Version> {
        match token {
            b"HTTP/1.0" => Some(Version::Http10),
            b"HTTP/1.1" => Some(Version::Http11),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_parse() {
        assert_eq!(Method::parse(b"GET"), Method::Get);
        assert_eq!(Method::parse(b"HEAD"), Method::Head);
        assert_eq!(Method::parse(b"POST"), Method::Unsupported);
        assert_eq!(Method::parse(b"get"), Method::Unsupported); // case-sensitive token
        assert_eq!(Method::parse(b""), Method::Unsupported);
    }

    #[test]
    fn version_parse() {
        assert_eq!(Version::parse(b"HTTP/1.0"), Some(Version::Http10));
        assert_eq!(Version::parse(b"HTTP/1.1"), Some(Version::Http11));
        assert_eq!(Version::parse(b"HTTP/2.0"), None);
        assert_eq!(Version::parse(b"HTTP/1.2"), None);
        assert_eq!(Version::parse(b"garbage"), None);
    }
}
