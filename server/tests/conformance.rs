//! Parametrized conformance suite (§8). For each implemented model it spawns the
//! real `server` binary and drives it over raw TCP (std only — no external HTTP
//! client), asserting the common bar:
//!   * 200 on `GET /`
//!   * 404 on an unknown path
//!   * 405 on a non-GET method
//!   * keep-alive reuse (200, 404, 405 on one connection)
//!   * 400 on a malformed request line, connection closed
//!   * the server SURVIVES the malformed request and keeps serving
//!
//! Each model session adds its model(s) to the parametrized list below.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Grab a likely-free port by binding to :0 and immediately releasing it.
///
/// This is inherently racy: the port is released before the server rebinds it,
/// so under a parallel test run a concurrent call (or the loadgen, or any other
/// process) can be handed the same ephemeral port in the gap. That race is not
/// resolved here — it is absorbed by `spawn_server`, which retries on a fresh
/// port until the server proves it owns one. Do not rely on this port being
/// free by the time you use it.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Owns the spawned server process and kills it on drop, even if a test panics.
/// `kill` sends SIGKILL to the parent; the process models set `PR_SET_PDEATHSIG`
/// on their children, so workers/children die with the parent — no leaked procs.
struct ServerGuard {
    child: Child,
    port: u16,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server(model: &str) -> ServerGuard {
    // `free_port` races (see its doc): a concurrent test can grab the same
    // ephemeral port before this server binds it. The loser then either exits
    // with EADDRINUSE — the single-socket models set neither SO_REUSEADDR nor
    // SO_REUSEPORT, so a colliding bind fails hard — or a later connect lands on
    // the other test's transient probe listener and gets reset. Both are
    // absorbed here: retry the whole spawn on a fresh port until the server
    // proves it exclusively owns the port by answering a real request. This is
    // what makes the suite safe to run in parallel (default) and on the metal
    // box, not just under `--test-threads=1`.
    const ATTEMPTS: usize = 25;
    let assets = format!("{}/assets", env!("CARGO_MANIFEST_DIR"));
    for _ in 0..ATTEMPTS {
        let port = free_port();
        let mut child = Command::new(env!("CARGO_BIN_EXE_server"))
            .args([
                "--model",
                model,
                "--port",
                &port.to_string(),
                "--assets-dir",
                &assets,
            ])
            .spawn()
            .expect("spawn server binary");
        if server_owns_port(&mut child, port) {
            return ServerGuard { child, port };
        }
        // Lost the port (child exited on EADDRINUSE, or never answered as ours).
        let _ = child.kill();
        let _ = child.wait();
    }
    panic!("[{model}] server never bound an exclusive port after {ATTEMPTS} attempts");
}

/// Readiness check that distinguishes *our* server from a stray probe listener.
///
/// A bare TCP connect is not enough: another test's `free_port` listener also
/// accepts connections (into its backlog) without ever answering, so a connect
/// can succeed against a socket that is not our server at all. This polls until
/// the child either exits (its bind lost the race) or answers `GET /` with a
/// `200` status line — proof the port is ours. Returns false on early exit or
/// deadline so `spawn_server` retries on a fresh port.
fn server_owns_port(child: &mut Child, port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // Server process exited — e.g. EADDRINUSE from a colliding port pick.
        if matches!(child.try_wait(), Ok(Some(_))) {
            return false;
        }
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
            if stream
                .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .is_ok()
            {
                let mut buf = [0u8; 16];
                // A real server answers "HTTP/1.1 200 ..."; a stray probe
                // listener returns EOF/reset or nothing — fall through and retry.
                if let Ok(n) = stream.read(&mut buf) {
                    if n >= 12 && buf.starts_with(b"HTTP/1.1 200") {
                        return true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Connect to the server, retrying until it is accepting (or a deadline).
fn connect(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("server never came up on port {port}: {e}"),
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read exactly one HTTP response (header block + Content-Length body) from a
/// keep-alive connection without over-reading into the next response.
fn read_response(stream: &mut TcpStream) -> Response {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];

    let header_end = loop {
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut tmp).expect("read headers");
        assert!(n != 0, "connection closed before a full header block");
        buf.extend_from_slice(&tmp[..n]);
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = content_length(&header_text);

    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp).expect("read body");
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }

    let body = buf[header_end..(header_end + content_length).min(buf.len())].to_vec();
    Response {
        status: status_code(&header_text),
        headers: header_text,
        body,
    }
}

struct Response {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

fn status_code(headers: &str) -> u16 {
    headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status code on the response line")
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

/// The full common bar for one model.
fn run_conformance(model: &str) {
    let server = spawn_server(model);

    // --- One keep-alive connection carries three requests in sequence. ---
    let mut stream = connect(server.port);

    // 1. GET / -> 200, keep-alive, served from the in-memory index asset.
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let resp = read_response(&mut stream);
    assert_eq!(resp.status, 200, "[{model}] GET / headers:\n{}", resp.headers);
    assert!(
        resp.headers.contains("Connection: keep-alive"),
        "[{model}] expected keep-alive:\n{}",
        resp.headers
    );
    assert!(
        resp.body.windows(8).any(|w| w == b"it works"),
        "[{model}] index body: {:?}",
        String::from_utf8_lossy(&resp.body)
    );

    // 2. SAME connection (keep-alive reuse): unknown path -> 404.
    stream
        .write_all(b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let resp = read_response(&mut stream);
    assert_eq!(resp.status, 404, "[{model}] GET /missing headers:\n{}", resp.headers);

    // 3. SAME connection again: a non-GET method -> 405.
    stream
        .write_all(b"DELETE / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let resp = read_response(&mut stream);
    assert_eq!(resp.status, 405, "[{model}] DELETE / headers:\n{}", resp.headers);
    drop(stream);

    // --- A malformed request line -> 400, connection closed. ---
    {
        let mut bad = connect(server.port);
        bad.write_all(b"GET /\r\n\r\n").unwrap();
        let resp = read_response(&mut bad);
        assert_eq!(resp.status, 400, "[{model}] malformed headers:\n{}", resp.headers);
        assert!(
            resp.headers.contains("Connection: close"),
            "[{model}] malformed should close:\n{}",
            resp.headers
        );
    }

    // --- The server must have survived the bad request: a fresh 200. ---
    {
        let mut good = connect(server.port);
        good.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let resp = read_response(&mut good);
        assert_eq!(
            resp.status, 200,
            "[{model}] server did not survive the bad request"
        );
    }
}

#[test]
fn iterative_conformance() {
    run_conformance("iterative");
}

#[test]
fn forking_conformance() {
    run_conformance("forking");
}

#[test]
fn preforked_conformance() {
    run_conformance("preforked");
}

#[test]
fn thread_per_conn_conformance() {
    run_conformance("thread-per-conn");
}

#[test]
fn thread_pool_conformance() {
    run_conformance("thread-pool");
}

#[test]
fn poll_conformance() {
    run_conformance("poll");
}

#[test]
fn epoll_lt_conformance() {
    run_conformance("epoll-lt");
}

#[test]
fn epoll_et_conformance() {
    run_conformance("epoll-et");
}

#[test]
fn event_loop_conformance() {
    run_conformance("event-loop");
}

#[test]
fn multireactor_conformance() {
    run_conformance("multireactor");
}

/// `io-uring` is kernel-gated (≥ 5.19, §5). On older kernels the server exits
/// non-zero with `io_uring unavailable: kernel X.Y < 5.19`; the test records
/// N/A and passes instead of failing — the same N/A convention `bench/run.sh`
/// uses for the sweep (§6).
#[test]
fn io_uring_conformance() {
    if let Some(reason) = io_uring_unavailable() {
        eprintln!("io-uring: skipping conformance — {reason}");
        return;
    }
    run_conformance("io-uring");
}

/// Returns `Some(reason)` if the host kernel cannot run io_uring per §5; the
/// check is the same `uname.release` parse the server itself runs at startup,
/// duplicated here so the test can skip without spawning a binary that exits
/// non-zero.
fn io_uring_unavailable() -> Option<String> {
    let mut un: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut un) } != 0 {
        return Some("uname() failed".to_string());
    }
    let raw = un.release.as_ptr();
    let mut len = 0usize;
    while len < un.release.len() && unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(raw as *const u8, len) };
    let s = std::str::from_utf8(bytes).unwrap_or("");
    let mut parts = s.split(|c: char| !c.is_ascii_digit());
    let major: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    if (major, minor) < (5, 19) {
        Some(format!("kernel {major}.{minor} < 5.19"))
    } else {
        None
    }
}
