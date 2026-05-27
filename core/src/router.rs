//! Trie-based router. The handler type is a pure function — no I/O, no
//! blocking — which is what lets one `core` serve every concurrency model.

use crate::app::App;
use crate::http::method::Method;
use crate::http::request::Request;
use crate::http::response::Response;

/// A handler is a PURE function: (request, app) -> response.
/// No I/O, no blocking.
pub type Handler = fn(req: &Request, app: &App) -> Response;

pub struct Router {
    // The trie (Node) structure is ported in Session C.
}

impl Router {
    pub fn new() -> Self {
        todo!()
    }

    pub fn insert(&mut self, _method: Method, _path: &str, _handler: Handler) {
        todo!()
    }

    pub fn lookup(&self, _method: Method, _path: &str) -> Option<Handler> {
        todo!()
    }
}
