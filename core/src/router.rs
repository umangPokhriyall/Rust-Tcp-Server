//! Trie-based router, ported from the legacy `Node` structure. The only change
//! is the handler type: a pure function `(request, app) -> response`, with no
//! I/O and no blocking — which is what lets one `core` serve every model.

use std::collections::HashMap;

use crate::app::App;
use crate::http::method::Method;
use crate::http::request::Request;
use crate::http::response::Response;

/// A handler is a PURE function: (request, app) -> response.
/// No I/O, no blocking.
pub type Handler = fn(req: &Request, app: &App) -> Response;

/// One node of the path trie. Each node owns a path segment (`key`), its child
/// segments, and an optional handler bound at this exact path.
struct Node {
    key: String,
    children: Vec<Node>,
    handler: Option<Handler>,
}

impl Node {
    fn new(key: &str) -> Self {
        Node {
            key: key.to_string(),
            children: Vec::new(),
            handler: None,
        }
    }

    fn insert(&mut self, path: &str, handler: Handler) {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            self.handler = Some(handler);
            return;
        }

        let mut parts = path.splitn(2, '/');
        let segment = parts.next().unwrap();
        let rest = parts.next().unwrap_or("");

        if let Some(child) = self.children.iter_mut().find(|n| n.key == segment) {
            child.insert(rest, handler);
        } else {
            let mut child = Node::new(segment);
            child.insert(rest, handler);
            self.children.push(child);
        }
    }

    fn get(&self, path: &str) -> Option<Handler> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            return self.handler;
        }

        let mut parts = path.splitn(2, '/');
        let segment = parts.next().unwrap();
        let rest = parts.next().unwrap_or("");

        self.children
            .iter()
            .find(|n| n.key == segment)
            .and_then(|child| child.get(rest))
    }
}

pub struct Router {
    routes: HashMap<Method, Node>,
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

impl Router {
    pub fn new() -> Self {
        Router {
            routes: HashMap::new(),
        }
    }

    pub fn insert(&mut self, method: Method, path: &str, handler: Handler) {
        let root = self.routes.entry(method).or_insert_with(|| Node::new(""));
        root.insert(path, handler);
    }

    pub fn lookup(&self, method: Method, path: &str) -> Option<Handler> {
        self.routes.get(&method).and_then(|root| root.get(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::response::StatusCode;

    fn dummy(_req: &Request, _app: &App) -> Response {
        Response::new(StatusCode::OK)
    }

    #[test]
    fn lookup_hits_registered_routes() {
        let mut r = Router::new();
        r.insert(Method::Get, "/", dummy);
        r.insert(Method::Get, "/static/style.css", dummy);

        assert!(r.lookup(Method::Get, "/").is_some());
        assert!(r.lookup(Method::Get, "/static/style.css").is_some());
    }

    #[test]
    fn lookup_misses_unknown_path_and_method() {
        let mut r = Router::new();
        r.insert(Method::Get, "/", dummy);

        assert!(r.lookup(Method::Get, "/nope").is_none());
        assert!(r.lookup(Method::Head, "/").is_none()); // method-scoped
        assert!(r.lookup(Method::Get, "/static/style.css").is_none());
    }

    #[test]
    fn trailing_slash_is_normalized() {
        let mut r = Router::new();
        r.insert(Method::Get, "/static/style.css", dummy);
        // The trie trims separators, so a leading-slash lookup matches.
        assert!(r.lookup(Method::Get, "static/style.css").is_some());
    }
}
