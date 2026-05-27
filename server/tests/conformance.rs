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

/// Grab a free port by binding to :0 and immediately releasing it.
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
    let port = free_port();
    let assets = format!("{}/assets", env!("CARGO_MANIFEST_DIR"));
    let child = Command::new(env!("CARGO_BIN_EXE_server"))
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
    ServerGuard { child, port }
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
