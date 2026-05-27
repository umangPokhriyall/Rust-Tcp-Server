//! HTTP header collection.
//!
//! Backed by a linear-scan `Vec`, **not** a `HashMap`: header counts are tiny
//! (< 100), a `Vec` is cache-friendlier, and avoiding a hash per lookup is the
//! correct mechanical-sympathy call at this scale.

#[derive(Debug, Default, Clone)]
pub struct Headers {
    inner: Vec<(String, String)>,
}

impl Headers {
    pub fn new() -> Self {
        Headers { inner: Vec::new() }
    }

    /// Case-INSENSITIVE name lookup. Returns the first matching value.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.inner
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn insert(&mut self, name: &str, value: &str) {
        self.inner.push((name.to_string(), value.to_string()));
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_lookup() {
        let mut h = Headers::new();
        h.insert("Host", "example.com");
        h.insert("Content-Length", "42");
        assert_eq!(h.get("host"), Some("example.com"));
        assert_eq!(h.get("HOST"), Some("example.com"));
        assert_eq!(h.get("Content-Length"), Some("42"));
        assert_eq!(h.get("content-length"), Some("42"));
        assert_eq!(h.get("missing"), None);
    }

    #[test]
    fn iter_yields_in_insertion_order() {
        let mut h = Headers::new();
        h.insert("A", "1");
        h.insert("B", "2");
        let collected: Vec<(&str, &str)> = h.iter().collect();
        assert_eq!(collected, vec![("A", "1"), ("B", "2")]);
    }
}
