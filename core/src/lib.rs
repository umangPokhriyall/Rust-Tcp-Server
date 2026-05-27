//! `core` — the sans-IO foundation shared by every concurrency model.
//!
//! `core` contains **zero socket I/O**: no `read`, `write`, `accept`, or
//! `epoll`. It is a pure library of an incremental HTTP parser, a response
//! encoder, a router, an asset cache, and a per-connection state machine —
//! all operating only on byte buffers in memory. The models own every syscall.
//!
//! See `docs/specs/phase0-spec.md` §10 for the exact public surface.

pub mod limits;

mod app;
mod asset;
mod conn;
mod http;
mod metrics;
mod router;
mod server;

pub use app::{App, AppBuilder};
pub use asset::{Asset, AssetCache};
pub use conn::{ConnAction, Connection};
pub use http::headers::Headers;
pub use http::method::{Method, Version};
pub use http::request::{ParseError, ParseStatus, Request, RequestParser};
pub use http::response::{Body, Response, StatusCode};
pub use metrics::Metrics;
pub use router::{Handler, Router};
pub use server::{bind_listener, Server, ServerConfig};
