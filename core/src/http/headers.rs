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
        todo!()
    }

    /// Case-INSENSITIVE name lookup.
    pub fn get(&self, _name: &str) -> Option<&str> {
        todo!()
    }

    pub fn insert(&mut self, _name: &str, _value: &str) {
        todo!()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.inner.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}
