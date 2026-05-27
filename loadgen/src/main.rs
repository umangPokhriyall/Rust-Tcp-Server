//! `loadgen` — an open-loop, coordinated-omission-correct HTTP load generator.
//!
//! The defining property (phase1-spec §6): requests are *scheduled* at a fixed
//! cadence `t0, t0 + 1/R, t0 + 2/R, …`, and each request's latency is measured
//! as `response_received − scheduled_time` — **never** `received − sent`. If the
//! server stalls and a request cannot even be put on the wire on schedule, that
//! queueing delay still counts toward its latency, because a real user does not
//! wait politely for the server to free up. A closed-loop loader (send, wait for
//! reply, then send the next) silently omits exactly this delay and so
//! under-reports the tail — which is the whole point of measuring it here.
//!
//! Design:
//!   * `M` persistent keep-alive connections, established before timing starts.
//!   * One scheduler thread emits scheduled-times into a shared queue at rate R.
//!   * Each connection thread pops a scheduled-time, sends `GET /`, reads the
//!     full response, and records `now − scheduled` into a per-thread histogram.
//!     If every connection is busy the request waits in the queue, and the wait
//!     is included because the clock started at the (fixed) scheduled-time.
//!   * Per-thread `hdrhistogram`s are merged at the end; one CSV row plus a
//!     percentile dump are written for distribution plots.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hdrhistogram::Histogram;

/// Histogram bounds: 1µs .. 60s, 3 significant figures. Latencies above the
/// ceiling saturate rather than panic.
const HIST_LOW_US: u64 = 1;
const HIST_HIGH_US: u64 = 60_000_000;
const HIST_SIGFIG: u8 = 3;

/// Per-socket read/write timeout. A stalled connection errors out (counted)
/// instead of hanging the run forever.
const SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

struct Config {
    target: String,
    model: String,
    rate: f64,
    connections: usize,
    duration: Duration,
    out: PathBuf,
}

fn main() {
    let cfg = match parse_args(std::env::args().skip(1)) {
        Ok(cfg) => cfg,
        Err(msg) => {
            eprintln!("loadgen: {msg}");
            print_usage();
            std::process::exit(2);
        }
    };

    match run(&cfg) {
        Ok(summary) => {
            if let Err(e) = write_output(&cfg, &summary) {
                eprintln!("loadgen: writing output failed: {e}");
                std::process::exit(1);
            }
            eprintln!(
                "loadgen: {} ok, {} errors, throughput {:.1} rps, p50 {}µs p99 {}µs p99.99 {}µs max {}µs",
                summary.ok,
                summary.errors,
                summary.throughput_rps,
                summary.hist.value_at_quantile(0.50),
                summary.hist.value_at_quantile(0.99),
                summary.hist.value_at_quantile(0.9999),
                summary.hist.max(),
            );
        }
        Err(e) => {
            eprintln!("loadgen: {e}");
            std::process::exit(1);
        }
    }
}

/// Aggregate result of a run.
struct Summary {
    hist: Histogram<u64>,
    ok: u64,
    errors: u64,
    throughput_rps: f64,
}

/// The shared work queue: scheduled-times waiting for a free connection.
/// Unbounded on purpose — a backlog is real load, and dropping it would be the
/// coordinated-omission bug we exist to avoid.
struct Queue {
    state: Mutex<QueueState>,
    not_empty: Condvar,
}

struct QueueState {
    pending: VecDeque<Instant>,
    scheduling_done: bool,
}

impl Queue {
    fn new() -> Self {
        Queue {
            state: Mutex::new(QueueState {
                pending: VecDeque::new(),
                scheduling_done: false,
            }),
            not_empty: Condvar::new(),
        }
    }

    /// Enqueue a request scheduled for `scheduled_time`.
    fn push(&self, scheduled_time: Instant) {
        let mut state = self.state.lock().unwrap();
        state.pending.push_back(scheduled_time);
        drop(state);
        self.not_empty.notify_one();
    }

    /// Block until a request is available; returns `None` once scheduling is
    /// done *and* the backlog is fully drained.
    fn pop(&self) -> Option<Instant> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(t) = state.pending.pop_front() {
                return Some(t);
            }
            if state.scheduling_done {
                return None;
            }
            state = self.not_empty.wait(state).unwrap();
        }
    }

    /// Signal that no more requests will be scheduled; wake all consumers so
    /// they can drain the remaining backlog and exit.
    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.scheduling_done = true;
        drop(state);
        self.not_empty.notify_all();
    }
}

fn run(cfg: &Config) -> io::Result<Summary> {
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\n\r\n",
        cfg.target
    );
    let request = Arc::new(request.into_bytes());

    // Establish every connection up front so connection-setup cost is excluded
    // from the measured latencies.
    let mut streams = Vec::with_capacity(cfg.connections);
    for i in 0..cfg.connections {
        let stream = TcpStream::connect(&cfg.target).map_err(|e| {
            io::Error::new(e.kind(), format!("connect #{i} to {}: {e}", cfg.target))
        })?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
        stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
        streams.push(stream);
    }

    let queue = Arc::new(Queue::new());

    // Spawn one worker per connection. They block on the (empty) queue until
    // the scheduler starts emitting.
    let mut workers = Vec::with_capacity(cfg.connections);
    for stream in streams {
        let queue = Arc::clone(&queue);
        let request = Arc::clone(&request);
        workers.push(thread::spawn(move || worker(stream, &queue, &request)));
    }

    // Scheduler: emit scheduled-times at the fixed cadence for `duration`.
    let period = Duration::from_secs_f64(1.0 / cfg.rate);
    let start = Instant::now();
    let end = start + cfg.duration;
    let mut i: u64 = 0;
    loop {
        let target = start + period.mul_f64(i as f64);
        if target >= end {
            break;
        }
        sleep_until(target);
        queue.push(target);
        i += 1;
    }
    queue.close();

    // Merge per-worker results.
    let mut hist = new_histogram();
    let mut ok = 0u64;
    let mut errors = 0u64;
    for w in workers {
        let result = w.join().expect("worker thread panicked");
        hist.add(&result.hist).expect("histogram merge");
        ok += result.ok;
        errors += result.errors;
    }

    let throughput_rps = ok as f64 / cfg.duration.as_secs_f64();
    Ok(Summary {
        hist,
        ok,
        errors,
        throughput_rps,
    })
}

struct WorkerResult {
    hist: Histogram<u64>,
    ok: u64,
    errors: u64,
}

/// One connection's loop: pop a scheduled request, send it, read the full
/// response, and record `now − scheduled`. On the first I/O error the worker
/// stops using its (now-suspect) connection — the remaining workers carry on.
fn worker(stream: TcpStream, queue: &Queue, request: &[u8]) -> WorkerResult {
    let mut hist = new_histogram();
    let mut ok = 0u64;
    let mut errors = 0u64;

    // A reader over a dup of the fd, kept across requests so keep-alive bytes
    // buffered past one response are not lost.
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => {
            // Could not set up reading; account every scheduled request as an error.
            while queue.pop().is_some() {
                errors += 1;
            }
            return WorkerResult { hist, ok, errors };
        }
    };
    let mut writer = stream;
    let mut reader = BufReader::new(reader_stream);

    while let Some(scheduled) = queue.pop() {
        match send_and_read(&mut writer, &mut reader, request) {
            Ok(()) => {
                let micros = scheduled.elapsed().as_micros() as u64;
                hist.saturating_record(micros);
                ok += 1;
            }
            Err(_) => {
                // Drain the rest of this worker's share as errors: a broken
                // keep-alive connection cannot be trusted for further timing.
                errors += 1;
                while queue.pop().is_some() {
                    errors += 1;
                }
                break;
            }
        }
    }

    WorkerResult { hist, ok, errors }
}

/// Write one request and consume exactly one full response (status line +
/// headers + `Content-Length` body).
fn send_and_read(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    request: &[u8],
) -> io::Result<()> {
    writer.write_all(request)?;
    writer.flush()?;

    let mut content_length = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed mid-response",
            ));
        }
        if line == "\r\n" || line == "\n" {
            break; // end of header block
        }
        // The status line falls through harmlessly (no `content-length:` prefix).
        if let Some(value) = header_value(&line, "content-length") {
            content_length = value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length")
            })?;
        }
    }

    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        reader.read_exact(&mut body)?;
    }
    Ok(())
}

/// Case-insensitive `Name: value` extractor for a single header line.
fn header_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (raw_name, raw_value) = line.split_once(':')?;
    if raw_name.trim().eq_ignore_ascii_case(name) {
        Some(raw_value)
    } else {
        None
    }
}

fn new_histogram() -> Histogram<u64> {
    Histogram::new_with_bounds(HIST_LOW_US, HIST_HIGH_US, HIST_SIGFIG)
        .expect("valid histogram bounds")
}

/// Sleep until `deadline`; returns immediately if already past it (the
/// scheduler has fallen behind — the request is simply emitted late, but its
/// scheduled-time is unchanged, so its latency still counts the lag).
fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

/// Append the summary CSV row (writing the header first if the file is new) and
/// dump the full percentile distribution next to it for plotting.
fn write_output(cfg: &Config, summary: &Summary) -> io::Result<()> {
    if let Some(parent) = cfg.out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let needs_header = match std::fs::metadata(&cfg.out) {
        Ok(m) => m.len() == 0,
        Err(_) => true,
    };
    let mut csv = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.out)?;
    if needs_header {
        writeln!(
            csv,
            "model,rate,connections,throughput_rps,errors,p50,p90,p99,p999,p9999,max"
        )?;
    }
    let h = &summary.hist;
    writeln!(
        csv,
        "{},{},{},{:.1},{},{},{},{},{},{},{}",
        cfg.model,
        cfg.rate as u64,
        cfg.connections,
        summary.throughput_rps,
        summary.errors,
        h.value_at_quantile(0.50),
        h.value_at_quantile(0.90),
        h.value_at_quantile(0.99),
        h.value_at_quantile(0.999),
        h.value_at_quantile(0.9999),
        h.max(),
    )?;

    write_histogram_dump(cfg, summary)
}

/// Write a percentile CSV (`.hgrm`) — value(µs) vs percentile on the doubling
/// `1/(1-p)` ladder, so the tail is resolved for log-scale distribution plots.
fn write_histogram_dump(cfg: &Config, summary: &Summary) -> io::Result<()> {
    let dump_path = dump_path(cfg);
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&dump_path)?;
    writeln!(f, "value_us,percentile,total_count,inverse_1_minus_p")?;

    let h = &summary.hist;
    // Quantile ladder: 0.0, then the gap to 1.0 halved each step down to ~1e-7,
    // matching HdrHistogram's standard percentile output.
    let mut quantile = 0.0_f64;
    let mut half_distance = 1.0_f64;
    loop {
        let value = h.value_at_quantile(quantile);
        let count = h.count_between(0, value);
        let inverse = if quantile >= 1.0 {
            f64::INFINITY
        } else {
            1.0 / (1.0 - quantile)
        };
        writeln!(f, "{value},{quantile:.7},{count},{inverse:.1}")?;
        if quantile >= 1.0 {
            break;
        }
        half_distance /= 2.0;
        quantile = 1.0 - half_distance;
        if quantile > 0.9999999 {
            quantile = 1.0;
        }
    }
    Ok(())
}

/// Dump path: `<out_dir>/<out_stem>_r<rate>_c<conns>.hgrm`.
fn dump_path(cfg: &Config) -> PathBuf {
    let dir = cfg.out.parent().unwrap_or_else(|| Path::new("."));
    let stem = cfg
        .out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "loadgen".to_string());
    dir.join(format!(
        "{stem}_r{}_c{}.hgrm",
        cfg.rate as u64, cfg.connections
    ))
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Config, String> {
    let mut target = "127.0.0.1:8080".to_string();
    let mut model = "unknown".to_string();
    let mut rate = 1000.0_f64;
    let mut connections = 10usize;
    let mut duration_secs = 10u64;
    let mut out = PathBuf::from("loadgen-results.csv");

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--target" => target = expect(&mut args, "--target")?,
            "--model" => model = expect(&mut args, "--model")?,
            "--rate" => {
                rate = expect(&mut args, "--rate")?
                    .parse()
                    .map_err(|_| "invalid --rate".to_string())?;
                if rate <= 0.0 {
                    return Err("--rate must be > 0".to_string());
                }
            }
            "--connections" => {
                connections = expect(&mut args, "--connections")?
                    .parse()
                    .map_err(|_| "invalid --connections".to_string())?;
                if connections == 0 {
                    return Err("--connections must be >= 1".to_string());
                }
            }
            "--duration" => {
                duration_secs = expect(&mut args, "--duration")?
                    .parse()
                    .map_err(|_| "invalid --duration".to_string())?;
                if duration_secs == 0 {
                    return Err("--duration must be >= 1".to_string());
                }
            }
            "--out" => out = PathBuf::from(expect(&mut args, "--out")?),
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Config {
        target,
        model,
        rate,
        connections,
        duration: Duration::from_secs(duration_secs),
        out,
    })
}

fn expect(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("missing value for {flag}"))
}

fn print_usage() {
    eprintln!(
        "usage: loadgen --target <host:port> --rate <req/s> --connections <M> \\\n\
         \x20      --duration <secs> --out <csv> [--model <name>]\n\
         \n\
         Open-loop, coordinated-omission-correct: latency = received - scheduled.\n\
         defaults: --target 127.0.0.1:8080 --rate 1000 --connections 10 \\\n\
         \x20         --duration 10 --out loadgen-results.csv --model unknown"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_value_is_case_insensitive_and_selective() {
        assert_eq!(
            header_value("Content-Length: 42\r\n", "content-length"),
            Some(" 42\r\n")
        );
        assert_eq!(
            header_value("CONTENT-LENGTH:7\r\n", "content-length"),
            Some("7\r\n")
        );
        assert_eq!(header_value("Host: example\r\n", "content-length"), None);
        assert_eq!(header_value("no-colon-here\r\n", "content-length"), None);
    }

    #[test]
    fn dump_path_encodes_rate_and_connections() {
        let cfg = Config {
            target: "127.0.0.1:8080".into(),
            model: "iterative".into(),
            rate: 1500.0,
            connections: 64,
            duration: Duration::from_secs(10),
            out: PathBuf::from("bench/results/iterative.csv"),
        };
        assert_eq!(
            dump_path(&cfg),
            PathBuf::from("bench/results/iterative_r1500_c64.hgrm")
        );
    }

    #[test]
    fn sleep_until_in_the_past_returns_immediately() {
        let past = Instant::now() - Duration::from_millis(50);
        let before = Instant::now();
        sleep_until(past);
        assert!(before.elapsed() < Duration::from_millis(20));
    }
}
