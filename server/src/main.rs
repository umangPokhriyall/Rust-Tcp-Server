//! Phase 0 server binary: `server --model <name> --port <n> --assets-dir <path>`.
//!
//! Builds the shared `App` (router + in-memory asset cache), then looks up the
//! requested concurrency model and runs it. Only `iterative` is wired in
//! Phase 0; any other name exits with a not-implemented message.

mod models;
mod reactor;
mod sys;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use core::{App, Method, Request, Response, ServerConfig, StatusCode};

use models::epoll::{EpollEt, EpollLt};
use models::event_loop::EventLoop;
use models::forking::Forking;
use models::io_uring::IoUring;
use models::iterative::Iterative;
use models::multireactor::Multireactor;
use models::poll::Poll;
use models::preforked::Preforked;
use models::thread_per_conn::ThreadPerConn;
use models::thread_pool::ThreadPool;

struct Cli {
    model: String,
    port: u16,
    assets_dir: String,
    verbose: bool,
    workers: Option<usize>,
    max_connections: Option<usize>,
}

fn main() {
    let cli = parse_args(std::env::args().skip(1));

    let app = match build_app(&cli) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("failed to load assets from {:?}: {e}", cli.assets_dir);
            std::process::exit(1);
        }
    };

    // Model dispatch (Phase 1 §9 DoD item 4: all 11 model names handled, with a
    // clear message for any unknown name). The two Phase 2 names route to an
    // explicit "deferred to Phase 2" arm so a user typing them gets a precise
    // signal rather than the generic unknown-model message.
    let server: Box<dyn core::Server> = match cli.model.as_str() {
        "iterative" => Box::new(Iterative::new(cli.verbose)),
        "forking" => Box::new(Forking::new(cli.verbose)),
        "preforked" => Box::new(Preforked::new(cli.verbose)),
        "thread-per-conn" => Box::new(ThreadPerConn::new(cli.verbose)),
        "thread-pool" => Box::new(ThreadPool::new(cli.verbose)),
        "poll" => Box::new(Poll::new(cli.verbose)),
        "epoll-lt" => Box::new(EpollLt::new(cli.verbose)),
        "epoll-et" => Box::new(EpollEt::new(cli.verbose)),
        "event-loop" => Box::new(EventLoop::new(cli.verbose)),
        "multireactor" => Box::new(Multireactor::new(cli.verbose)),
        "io-uring" => Box::new(IoUring::new(cli.verbose)),
        other => {
            eprintln!("unknown model '{other}' — expected one of: iterative, \
                       forking, preforked, thread-per-conn, thread-pool, \
                       poll, epoll-lt, epoll-et, event-loop, multireactor, \
                       io-uring");
            std::process::exit(2);
        }
    };

    let mut cfg = ServerConfig {
        addr: SocketAddr::from(([0, 0, 0, 0], cli.port)),
        assets_dir: PathBuf::from(&cli.assets_dir),
        ..ServerConfig::default()
    };
    if let Some(w) = cli.workers {
        cfg.workers = w.max(1);
    }
    if let Some(m) = cli.max_connections {
        cfg.max_connections = m.max(1);
    }

    if let Err(e) = server.serve(&cfg, Arc::new(app)) {
        eprintln!("server error: {e}");
        std::process::exit(1);
    }
}

/// Build the shared `App`: load assets once, register the two GET routes.
fn build_app(cli: &Cli) -> std::io::Result<App> {
    App::builder()
        .assets_dir(Path::new(&cli.assets_dir))
        .route(Method::Get, "/", serve_index)
        .route(Method::Get, "/static/style.css", serve_style)
        .build()
}

/// `GET /` -> the index page from the in-memory cache.
fn serve_index(_req: &Request, app: &App) -> Response {
    serve_asset(app, "index.html")
}

/// `GET /static/style.css` -> the stylesheet from the in-memory cache.
fn serve_style(_req: &Request, app: &App) -> Response {
    serve_asset(app, "static/style.css")
}

/// Pull `name` from the asset cache, cloning only the `Arc` (never the bytes).
fn serve_asset(app: &App, name: &str) -> Response {
    match app.assets().get(name) {
        Some(asset) => Response::ok(asset.content_type, asset.bytes.clone()),
        None => Response::text(StatusCode::INTERNAL_ERROR, "asset not found in cache"),
    }
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Cli {
    let mut cli = Cli {
        model: "iterative".to_string(),
        port: 8080,
        assets_dir: "server/assets".to_string(),
        verbose: false,
        workers: None,
        max_connections: None,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model" => cli.model = expect_value(&mut args, "--model"),
            "--assets-dir" => cli.assets_dir = expect_value(&mut args, "--assets-dir"),
            "--port" => {
                let raw = expect_value(&mut args, "--port");
                cli.port = raw.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --port value: {raw}");
                    std::process::exit(2);
                });
            }
            "--workers" => {
                let raw = expect_value(&mut args, "--workers");
                let w: usize = raw.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --workers value: {raw}");
                    std::process::exit(2);
                });
                cli.workers = Some(w);
            }
            "--max-connections" => {
                let raw = expect_value(&mut args, "--max-connections");
                let m: usize = raw.parse().unwrap_or_else(|_| {
                    eprintln!("invalid --max-connections value: {raw}");
                    std::process::exit(2);
                });
                cli.max_connections = Some(m);
            }
            "--verbose" => cli.verbose = true,
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                print_usage();
                std::process::exit(2);
            }
        }
    }

    cli
}

fn expect_value(args: &mut impl Iterator<Item = String>, flag: &str) -> String {
    args.next().unwrap_or_else(|| {
        eprintln!("missing value for {flag}");
        std::process::exit(2);
    })
}

fn print_usage() {
    eprintln!(
        "usage: server [--model <name>] [--port <n>] [--assets-dir <path>] [--workers <n>] [--max-connections <n>] [--verbose]\n\
         \n\
         --workers overrides cfg.workers (default: num_cores()); used by the\n\
         multireactor scaling study and preforked / thread-pool sizing.\n\
         --max-connections overrides cfg.max_connections (default: 1024); used\n\
         by the C10K study so single-reactor event-loop models can accept more\n\
         than 1024 concurrent connections.\n\
         defaults: --model iterative --port 8080 --assets-dir server/assets"
    );
}
