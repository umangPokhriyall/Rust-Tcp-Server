//! Immutable shared application state: router + asset cache + metrics. Built
//! once at boot, wrapped in `Arc`, shared read-only by every worker. `Send + Sync`.

use std::path::{Path, PathBuf};

use crate::asset::AssetCache;
use crate::http::method::Method;
use crate::http::request::Request;
use crate::http::response::{Response, StatusCode};
use crate::metrics::Metrics;
use crate::router::{Handler, Router};

pub struct App {
    router: Router,
    assets: AssetCache,
    metrics: Metrics,
}

// `App` is shared read-only across threads / forked children, so it must be
// `Send + Sync`. This fails to compile if a future field breaks that.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<App>();
};

impl App {
    pub fn builder() -> AppBuilder {
        AppBuilder {
            router: Router::new(),
            assets_dir: None,
        }
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn assets(&self) -> &AssetCache {
        &self.assets
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Route a parsed request to a Response.
    /// No matching route -> 404. Unsupported method -> 405.
    pub fn handle(&self, req: &Request) -> Response {
        // HEAD shares GET's routing table; the body is dropped at encode time
        // by the connection layer.
        let method = match req.method {
            Method::Get | Method::Head => Method::Get,
            Method::Unsupported => {
                return Response::text(StatusCode::METHOD_NOT_ALLOWED, "405 Method Not Allowed");
            }
        };

        match self.router.lookup(method, &req.path) {
            Some(handler) => handler(req, self),
            None => Response::text(StatusCode::NOT_FOUND, "404 Not Found"),
        }
    }
}

pub struct AppBuilder {
    router: Router,
    assets_dir: Option<PathBuf>,
}

impl AppBuilder {
    pub fn route(mut self, method: Method, path: &str, handler: Handler) -> Self {
        self.router.insert(method, path, handler);
        self
    }

    pub fn assets_dir(mut self, dir: &Path) -> Self {
        self.assets_dir = Some(dir.to_path_buf());
        self
    }

    pub fn build(self) -> std::io::Result<App> {
        let assets = match self.assets_dir {
            Some(dir) => AssetCache::load_dir(&dir)?,
            None => AssetCache::default(),
        };
        Ok(App {
            router: self.router,
            assets,
            metrics: Metrics::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::headers::Headers;
    use crate::http::method::Version;

    fn index(_req: &Request, _app: &App) -> Response {
        Response::text(StatusCode::OK, "index")
    }

    fn req(method: Method, path: &str) -> Request {
        Request {
            method,
            path: path.to_string(),
            version: Version::Http11,
            headers: Headers::new(),
            body: Vec::new(),
        }
    }

    fn app() -> App {
        App::builder().route(Method::Get, "/", index).build().unwrap()
    }

    #[test]
    fn routes_known_get_to_handler() {
        let app = app();
        assert_eq!(app.handle(&req(Method::Get, "/")).status, StatusCode::OK);
    }

    #[test]
    fn unknown_path_is_404() {
        let app = app();
        assert_eq!(app.handle(&req(Method::Get, "/missing")).status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn unsupported_method_is_405() {
        let app = app();
        assert_eq!(
            app.handle(&req(Method::Unsupported, "/")).status,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[test]
    fn head_routes_to_get_handler() {
        let app = app();
        assert_eq!(app.handle(&req(Method::Head, "/")).status, StatusCode::OK);
    }

    #[test]
    fn builder_without_assets_dir_builds_empty_cache() {
        let app = App::builder().build().unwrap();
        assert!(app.assets().get("anything").is_none());
    }
}
