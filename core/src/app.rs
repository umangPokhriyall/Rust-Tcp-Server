//! Immutable shared application state: router + asset cache + metrics. Built
//! once at boot, wrapped in `Arc`, shared read-only by every worker. `Send + Sync`.

use crate::asset::AssetCache;
use crate::http::method::Method;
use crate::http::request::Request;
use crate::http::response::Response;
use crate::metrics::Metrics;
use crate::router::{Handler, Router};

pub struct App {
    // router, assets, metrics — all private. Built in Session C.
}

impl App {
    pub fn builder() -> AppBuilder {
        todo!()
    }

    pub fn router(&self) -> &Router {
        todo!()
    }

    pub fn assets(&self) -> &AssetCache {
        todo!()
    }

    pub fn metrics(&self) -> &Metrics {
        todo!()
    }

    /// Route a parsed request to a Response.
    /// No matching route -> 404. Unsupported method -> 405.
    pub fn handle(&self, _req: &Request) -> Response {
        todo!()
    }
}

pub struct AppBuilder {
    // Built in Session C.
}

impl AppBuilder {
    pub fn route(self, _method: Method, _path: &str, _handler: Handler) -> Self {
        todo!()
    }

    pub fn assets_dir(self, _dir: &std::path::Path) -> Self {
        todo!()
    }

    pub fn build(self) -> std::io::Result<App> {
        todo!()
    }
}
