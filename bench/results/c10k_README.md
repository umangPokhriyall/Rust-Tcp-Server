# C10K capture — a true 10,000 concurrent connections

This directory's `c10k_*` files are the C10K resource-curve and served-latency
capture produced by `bench/c10k.sh` on the AMD EPYC 9254 run of record
(`bench/results/rig.txt`, git `bf67267`). Every number here is from that run.

## What was measured

`bench/c10k.sh` holds each of the eleven models at a **true 10,000 concurrent
keep-alive connections** at **50,000 rps offered** for 30 s, with the server's
`--max-connections` raised to 16384 so the event-loop models accept all of them.
While the load runs it samples `/proc/<pid>/status` (VmRSS, voluntary/nonvoluntary
context-switch counters) and the live fd count every 2 s.

- `c10k_<model>.csv` — served throughput, error count, and latency percentiles
  (the authoritative 10,000-connection latency numbers; the `c=10000` rows in the
  sweep `<model>.csv` are capped at the server's default `max_connections` and are
  superseded by these).
- `c10k_<model>.log` — the per-sample resource curve (`ts_s,rss_kib,fds,zombies,
  ctx_voluntary,ctx_involuntary`).
- `c10k_<model>_r50000_c10000.hgrm` — the full HDR interior-latency histogram.
- `c10k_server_<model>.log` — server stdout/stderr for the run.
- `c10k_summary.csv` — one verdict row per model (first/last RSS, first/last fds,
  ctx-switch counters, verdict).

## The true 10,000-connection result

384 GiB of RAM clears the commit limit that capped the archived laptop run at
8,000 connections, so this is a real C=10000 rung with **no sentinels**. From
`c10k_summary.csv` and `c10k_<model>.csv`:

| Model | Verdict | Served (req/s) | Errors | p50 (µs) | RSS last (KiB) |
|---|---|---|---|---|---|
| iterative | saturated (no completion) | — | — | — | 2416 |
| forking | ok | 50000 | 0 | 73 | 2472 (parent) |
| preforked | ok (shedding) | 17008.8 | 989,735 | 60 | 2472 |
| thread-per-conn | ok | 50000 | 0 | 71 | 267,284 |
| thread-pool | ok (shedding) | 17022.4 | 989,328 | 62 | 3120 |
| poll | ok | 50000 | 0 | 7231 | 11004 |
| epoll-lt | ok | 50000 | 0 | 195 | 10748 |
| epoll-et | ok | 50000 | 0 | 94 | 10744 |
| event-loop | ok | 50000 | 0 | 98 | 10748 |
| multireactor | ok | 50000 | 0 | 70 | 10952 |
| io-uring | ok | 50000 | 0 | 80 | 11604 |

**Eight of eleven models carry 10,000 connections at the full 50,000 rps with
zero errors.** The event-loop models and `multireactor` hold flat at ≈ 10.7 MiB
resident (≈ 1.1 KiB per connection) — connection state is a slab entry and an fd,
not a stack. `io_uring` is now in this group with 0 errors, unlike the archived
laptop where its single ring shed load.

The three that fall short:
- **iterative** — one blocking serving thread cannot drain 10,000 connections
  inside the budget; recorded `saturated`, `c10k_iterative.csv` is empty.
- **preforked**, **thread-pool** — a fixed blocking worker pool can *serve* but
  cannot *hold* 10,000 concurrent connections; ~99% of requests shed as errors.
  On this host the C10K failure mode is a bounded worker set, not memory.

`poll` survives on capacity but its O(n) per-wakeup fd scan drags the median to
7231 µs, ~77× `epoll-et`'s 94 µs at the identical rung — a latency tax, not a wall.

## Note on the process/thread-per-connection models

`forking` (one process per connection) and `thread-per-conn` (one thread per
connection) **sustain** 10,000 connections at 50,000 rps with 0 errors here.
`thread-per-conn`'s RSS climbs to 267,284 KiB (261 MiB) for the thread stacks;
`forking`'s parent RSS stays flat because the children are separate processes.
On the archived 8 GiB laptop both panicked with `EAGAIN` on clone. The C10K wall
for these models is memory, and 384 GiB has it.

## Residual caveat

All transport is loopback (`127.0.0.1`) within a single socket over Infinity
Fabric — there is no NIC, no real RTT, no loss, and (under NPS1) no
memory-controller split between server and loadgen (server pinned to CPUs 0–11,
loadgen to 12–23; `bench/results/rig.txt`). Absolute latencies are loopback
latencies. The transferable results are the ordering between models and the
resource footprints, not the absolute microseconds.

See `docs/BENCHMARKS.md` §9 for the full C10K narrative and §5A for the AMD Zen4
pipeline behaviour of the signal models under this load.
