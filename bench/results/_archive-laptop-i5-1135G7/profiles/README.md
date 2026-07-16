# Profiling deep-dive (archived laptop run)

Telemetry for the three signal models — `epoll-et` (single-thread),
`multireactor` (default workers), and `io-uring` (single ring,
single thread). Numbers in this directory are the source of truth for
`docs/BENCHMARKS.md` §9 and the io_uring verdict.

## Host

- Kernel: `Linux 7.0.0-15-generic` (Ubuntu 26.04 LTS) — io_uring fully
  supported, well above the §5 floor of 5.19.
- CPU: 11th Gen Intel(R) Core(TM) i5-1135G7 @ 2.40 GHz, 4 cores / 8 threads.
- RAM: 8 GiB. Loopback (`127.0.0.1`).

## Method

Two passes per model, both driven by `bench/profile.sh`.

### Pass 1 — `strace -c -f` for syscalls/req

`bench/profile.sh` launches the server as a child of `strace`:

    strace -c -f -o profiles/strace_<model>.txt -- \
        target/release/server --model <model> --port <p> --assets-dir <a>

The loadgen then drives an open-loop workload (`rate=500 rps`,
`connections=10`, `duration=20 s` → 10 000 scheduled requests). SIGINT
shuts the server; strace flushes its `-c` summary on tracee exit.
`syscalls/req = total_syscalls / completed_requests`.

The strace summary counts every syscall the process and its threads make
during its lifetime, including the few dozen startup syscalls
(`execve`, `mmap`, `openat` of `/proc/self/maps`, etc.) and the
clone/setaffinity calls multireactor issues per worker. At 10 000
requests these contribute well under 1 % of the total.

For io-uring the count includes `io_uring_setup`, `io_uring_register`,
and every `io_uring_enter` — the latter is the only call in the hot
path. Multishot accept and provided buffer rings collapse what would be
many readiness syscalls into a small number of `io_uring_enter` calls.

### Pass 2 — `/proc/<pid>/task/*/status` for ctx-switches/req

The server is then run **without** strace (strace itself injects
ptrace-stops that distort context-switch counts). `bench/profile.sh`
sums `voluntary_ctxt_switches` and `nonvoluntary_ctxt_switches` across
every thread under `/proc/<server_pid>/task/` immediately after the
listener is up and again immediately before SIGINT. Loadgen drives
`rate=2000 rps`, `connections=100`, `duration=30 s` → 60 000 requests.
`ctx-switches/req = (sum_after - sum_before) / completed_requests`.

### Top-down microarchitecture — unavailable on this host

The methodology calls for `perf stat` top-down
(retiring / bad-spec / frontend-bound / backend-bound). `perf` cannot
open any event on this kernel:

    $ cat /proc/sys/kernel/perf_event_paranoid
    4
    $ perf stat -e cycles /bin/true
    Error:
    No supported events found.
    Access to performance monitoring and observability operations is limited.

`perf_event_paranoid = 4` is stricter than the documented levels — it
denies event access to non-`CAP_PERFMON` users entirely. `sudo` is not
available and the setting cannot be lowered. §7 names a privilege
fallback for exactly this case: `/proc/<pid>/status` ctxt-switch
counters + `strace -c`. That is the path taken. The two metrics
recovered (syscalls/req, ctx-switches/req) are the load-bearing ones
for the io_uring story; top-down decomposition is documented as
omitted in `docs/BENCHMARKS.md`.

## Headline — syscalls per request

| Model        | total syscalls | completed requests | syscalls / req | source |
|--------------|---------------:|-------------------:|---------------:|--------|
| epoll-et     | 40 235         | 10 000             | **4.024**      | `strace_epoll-et.txt`, `loadgen_strace_epoll-et.csv` |
| multireactor | 40 595         | 10 000             | **4.059**      | `strace_multireactor.txt`, `loadgen_strace_multireactor.csv` |
| io-uring     | 20 154         | 10 000             | **2.015**      | `strace_io-uring.txt`, `loadgen_strace_io-uring.csv` |

io-uring sustains the same workload at **half** the syscalls/req of
epoll-et. That ratio is the §7 headline and the basis for the §9.7
io_uring verdict.

### Where the syscalls go

`epoll-et` (10 000 requests, totals from `strace_epoll-et.txt`):

| syscall    | calls  | per req |
|------------|-------:|--------:|
| epoll_wait | 10 007 | 1.001   |
| sendto     | 10 000 | 1.000   |
| recvfrom   | 20 010 | 2.001   |

The recv-twice pattern is edge-triggered epoll's drain-to-EAGAIN
convention: the first `recvfrom` consumes the request bytes, the second
returns `EAGAIN` (9 999 of the 10 006 `errors` in the summary) and
re-arms the readiness signal. Every request therefore costs roughly
`1 × epoll_wait + 1 × sendto + 2 × recvfrom = 4` syscalls. The model's
floor cost.

`multireactor` (10 000 requests, totals from
`strace_multireactor.txt`): the per-worker pattern is identical to
epoll-et — `epoll_wait 10 040`, `sendto 10 000`, `recvfrom 20 009`,
plus the startup cost split across 8 worker threads
(`clone3 8`, `bind 8`, `listen 8`, `sched_setaffinity 8`,
`epoll_create1 8`). Per-thread the hot-path syscall budget is the
same as epoll-et's — no shared state means no shared syscall, and
SO_REUSEPORT delivers a single connection's lifetime to a single
reactor.

`io-uring` (10 000 requests, totals from `strace_io-uring.txt`):

| syscall          | calls  | per req |
|------------------|-------:|--------:|
| io_uring_enter   | 19 998 | 2.000   |
| io_uring_setup   |      1 | —       |
| io_uring_register|      1 | —       |

Multishot accept (one submitted `IORING_OP_ACCEPT_MULTI` SQE delivers
every subsequent accept CQE) eliminates the per-accept syscall.
Provided buffer rings let the kernel pick a destination buffer for
each read CQE, so reads need no per-op setup. What remains in the hot
path is `io_uring_enter` — once to deliver SQEs and reap CQEs for the
read side, once for the write side. 2.0 enters per request is the
purpose-built minimum on this workload, and it is the exact mechanism
that the io_uring comparison requires.

## Context-switches per request

| Model        | vol Δ  | nonvol Δ | total Δ | requests | ctx-switches / req | source |
|--------------|-------:|---------:|--------:|---------:|-------------------:|--------|
| epoll-et     | 60 048 |      415 |  60 463 |   60 000 | **1.008**          | `ctx_epoll-et.txt`, `loadgen_ctx_epoll-et.csv` |
| multireactor | 59 618 |    9 592 |  69 210 |   60 000 | **1.153**          | `ctx_multireactor.txt`, `loadgen_ctx_multireactor.csv` |
| io-uring     | 60 033 |      317 |  60 350 |   60 000 | **1.006**          | `ctx_io-uring.txt`, `loadgen_ctx_io-uring.csv` |

All three sit at ≈ 1 voluntary context switch per request — the
epoll_wait / io_uring_enter park-and-wake cycle. The headline
deviation is multireactor's nonvoluntary count
(9 592 vs ~ 400 for the single-thread models): 8 pinned reactor
threads share an 8-thread CPU with loadgen's 100 connection threads,
and the scheduler preempts a reactor each time the loadgen worker on
its core wakes. The mechanism is real and is the same kernel-level
imbalance §4 warns about; on this host it is the cost of running the
load generator and the server side-by-side rather than across a
network.

## Files in this directory

| File | Provenance |
|------|-----------|
| `strace_<model>.txt`            | `strace -c -f` summary for each model |
| `loadgen_strace_<model>.csv`    | loadgen result for the strace pass (rate / completed throughput) |
| `ctx_<model>.txt`               | summed `/proc/<pid>/task/*/status` ctxt-switch counters, before / after / delta |
| `loadgen_ctx_<model>.csv`       | loadgen result for the native pass |
| `server_strace_<model>.log`     | server stdout / stderr under strace |
| `server_ctx_<model>.log`        | server stdout / stderr without strace |
| `summary.csv`                   | derived table feeding the §9.4 headline rows |

## Reproduction

    cargo build --release
    bash bench/profile.sh                  # all three models
    bash bench/profile.sh epoll-et         # one model
    STRACE_DURATION=60 bash bench/profile.sh

Environment overrides: `STRACE_RATE`, `STRACE_CONNS`, `STRACE_DURATION`,
`CTX_RATE`, `CTX_CONNS`, `CTX_DURATION`, `PORT_BASE`, `OUT`. The
defaults are recorded above and in `bench/profile.sh`.

## Caveats

- Startup syscalls (a few dozen `mmap`/`openat`/`execve` calls) are
  included in the strace totals. At 10 000 requests they round to
  ≈ 0.005 syscalls/req inflation — below the rounding of the headline
  table.
- The strace pass measures correctness of the syscall *count*, not
  throughput: ptrace stops add per-call latency and the server runs
  well below its native throughput during this capture. That is fine —
  syscalls/req is a per-event property.
- The ctx-switch pass excludes strace for the opposite reason: ptrace
  stops show up as context switches and would dominate any signal.
- Both passes are loopback-only, single-host. The loadgen runs on the
  same machine; multireactor's nonvoluntary ctx-switch tail is partly
  loadgen-induced.
- `perf stat` top-down is omitted (paranoid = 4). Acknowledged in
  `docs/BENCHMARKS.md`.
