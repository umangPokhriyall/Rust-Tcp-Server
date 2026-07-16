# bench/results/c10k_README.md — C10K resource curves

This directory holds the archived laptop C10K capture: each model driven at a
fixed concurrency for `DURATION` seconds while the harness samples
`/proc/<pid>/status` (VmRSS, voluntary/involuntary context switches) and
the live fd count every `SAMPLE_INTERVAL` seconds.

## Why the headline concurrency is 8000, not 10000

The nominal C10K target is 10,000 concurrent keep-alive connections. On
the benchmark host used for this run (8 GiB RAM, kernel 7.0.0-15), the
loadgen process cannot `pthread_create` 10,000 worker threads — every
attempt fails with `EAGAIN` on clone. The cause is heuristic overcommit
under existing memory pressure:

  /proc/meminfo at the time of the run:
    MemTotal:       7,322,952 kB
    MemAvailable:   3,701,244 kB
    CommitLimit:    7,855,776 kB
    Committed_AS:  10,155,988 kB   <-- already 2.3 GiB above CommitLimit

With `vm.overcommit_memory=0`, the kernel refuses any new private
allocation (including stack reservations for new pthreads) once the
existing commit charge exceeds `CommitLimit`. Lowering loadgen's
per-thread stack to 64 KiB via `RUST_MIN_STACK` plus `ulimit -s` is not
sufficient — the host is already over the line. Raising
`vm.overcommit_memory=1` would fix it but requires root and changes
kernel behaviour for every other process on the box, so we don't.

The highest concurrency that fits is 8000 — this is what `bench/c10k.sh`
drives. The c=10000 rung of the sweep CSVs (`<model>.csv`) records this
via two distinct sentinels in the `errors` column:

  * `errors=-1` — harness-side saturation (point exceeded the
                  `POINT_BUDGET` wall clock; loadgen never completed).
                  Applied to the seven models where the server itself
                  fell over at c=10000 in the connect/serve phase.
  * `errors=-2` — loadgen-side host limit (loadgen panicked on
                  `pthread_create`). Applied to `forking`,
                  `thread-pool`, `multireactor`, and `io-uring` — the
                  four models that would have served c=10000 but the
                  loadgen couldn't reach that scale on this host.

Both rows have `throughput_rps=0.0` and all latency columns `0`. The
plotter renders them as dropouts; readers see the gap and the README
explains it.

## Server-side max_connections

`ServerConfig::default().max_connections = 1024`. At C10K_CONNS=8000 the
default would cap single-reactor models at 1024 active connections —
inflating their "RSS flat" verdict but obscuring whether the model can
actually carry 8000 connections. `bench/c10k.sh` therefore passes
`--max-connections 16384` to the server (see
`server/src/main.rs --max-connections`). `multireactor` ignores the cap
in the obvious way: each of its N reactors enforces it independently,
giving N × max_connections total capacity.

## Files

  * `c10k_summary.csv` — one verdict row per model
    `model,connections,rate,duration_s,first_rss_kib,last_rss_kib,
     first_fds,last_fds,max_zombies,ctx_voluntary,ctx_involuntary,verdict`
    `verdict` is `ok | saturated | loadgen-error | no-steady-state |
                  server-startup-failed`.
  * `c10k_<model>.log` — per-sample resource curve
    `ts_s,rss_kib,fds,zombies,ctx_voluntary,ctx_involuntary`
  * `c10k_<model>.csv` — the loadgen result CSV for the c=8000 point
    (same schema as `<model>.csv`).
  * `c10k_server_<model>.log` — server stderr for that run.

## Reproducing

  bench/c10k.sh
  # Override defaults via env:
  C10K_CONNS=4000 C10K_RATE=20000 DURATION=60 bench/c10k.sh epoll-et multireactor
