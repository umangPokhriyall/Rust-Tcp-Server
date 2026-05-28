# x-thread.md — distribution strategy + thread draft

This file has two parts: a strategy brief for the repo owner (not for posting),
and a 9-post thread draft with every number filled from committed results under
`bench/results/`. The owner edits the draft for voice before posting; the
numbers and their source files are fixed.

---

## Part 1 — Strategy brief (for the owner; do not post)

- **Why X.** With no professional network, this thread is the distribution
  channel that puts a cold artifact in front of the people who hire — systems
  and AI-infra engineers. It is not a vanity post; it is how the repo gets
  read.
- **Governing principle.** Post findings, not hype. This is a technical result,
  not a launch. The numbers do the persuading; the prose stays out of their
  way. Every claim in the thread traces to a committed CSV, including the
  negative results.
- **Targeting / engagement.** Be discoverable to the Rust-systems, io_uring,
  and AI-infra (sandbox / inference / voice) communities. Earn that reach by
  replying with technical substance to *their* posts over time — never cold-@ a
  famous account inside the thread to borrow its audience.
- **Cadence.** Post only when able to be present for the 2–3 hours after;
  threads die without author engagement. Reply to every substantive reply. One
  day later, quote-tweet the single best plot (the C10K resource curve or the
  syscalls/req comparison) as a standalone.
- **Conversion path.** thread → profile → repo → `docs/BENCHMARKS.md` →
  conversation. The only CTA is the repo link. No "looking for work" in the
  thread; availability lives in the bio and pinned post, proof-first.
- **Do not.** Hashtag-stuff, cold-tag, or editorialize beyond the data. If a
  number is not in `bench/results/`, it does not go in the thread.

---

## Part 2 — Thread draft (9 posts, each ≤280 chars)

Source files are noted in brackets after each post for the owner's reference;
strip the brackets before posting. Numbers obey the §8 Writing Standard:
declarative, sourced, units and conditions on every figure.

**1 / hook**

> Built every TCP server concurrency model in Rust — from `accept()`-in-a-loop
> to a purpose-built io_uring completion engine, 11 models behind one trait —
> and benchmarked all of them honestly. No async runtime, one frozen sans-IO
> state machine. Thread.

**2 / method**

> The load is open-loop and corrected for coordinated omission: every request's
> latency is measured from when it *should* have been sent, not from when a
> connection freed up. Most server benchmarks quietly hide their tail. This one
> puts the backlog in the p99.

**3 / throughput + tail**
> [`bench/results/c10k_summary.csv`, `bench/results/c10k_multireactor.csv`]

> At 8000 keep-alive connections / 50000 req/s offered: all 5 event-loop models
> + multireactor hold zero errors in a flat ~10 MiB RSS (~1.3 KiB/conn).
> multireactor runs the lowest median, p50 = 86 µs. The p99s are hundreds of
> ms — that's the open-loop backlog, shown not hidden.

**4 / syscalls per request**
> [`bench/results/profiles/summary.csv`, `bench/results/profiles/strace_*.txt`]

> The io_uring story is syscalls. On the fair single-thread axis: epoll-et =
> 4.024 syscalls/req, purpose-built single-ring io_uring = 2.015. A 2.00× cut.
> Multishot accept + provided buffer rings remove the per-accept and per-read
> syscalls; completion replaces the readiness wait.

**5 / C10K**
> [`bench/results/c10k_server_thread-per-conn.log`, `bench/results/c10k_summary.csv`]

> C10K, honestly: thread-per-conn and forking *panic* with EAGAIN allocating
> the per-connection thread (thread-per-conn dies at 4608 fds / 118 MiB RSS).
> The event-loop models hold 8000 connections flat at ~10 MiB. Connection state
> is an fd + a slab entry, not a stack.

**6 / honest verdict**
> [`bench/results/io-uring.csv`, `bench/results/c10k_io-uring.csv`]

> Honest verdict: single-ring io_uring wins syscalls/req by 2× but *sheds* load
> above C≈1000 (18402 req/s, 947927 errors at C=8000) — one ring on one thread
> is a single serialization point. multireactor wins absolute throughput, but
> on 8 cores, not by magic. Name the axis.

**7 / reactor finding**
> [`bench/results/event-loop.csv`, `bench/results/epoll-et.csv`]

> Surprise that killed my hypothesis: the reactor abstraction is NOT zero-cost
> over hand-rolled epoll-et. They cross over — at C=100 the reactor's tail is
> worse (p99 71.7 ms vs 23.1 ms), at C=1000 far better (28.2 ms vs 214.8 ms).
> It reshapes the latency profile, doesn't vanish.

**8 / the bridge**

> Why build this: it's the shape of an AI-agent sandbox control plane —
> acceptor-free SO_REUSEPORT reactors, one pinned per core, shared-nothing,
> dispatching to isolated workers. The frozen sans-IO Connection is the guest
> lifecycle machine, unchanged across all 3 I/O backends.

**9 / CTA**

> Full teardown — every number sourced to a committed CSV, a threats-to-validity
> section, the negative results kept in, and a one-command reproduction harness:
> https://github.com/umangPokhriyall/Rust-Tcp-Server

---

### Quote-tweet follow-up (day +1, standalone)

> The single clearest result, one plot: level-triggered epoll carries 8000
> connections at p50 = 457 ms; edge-triggered epoll carries the same load at
> p50 = 157 µs — a ~2900× median gap from re-notification cost alone, same host,
> same rung.

Attach `bench/results/p99_vs_concurrency.png` (or the C10K resource curve).
Sources: [`bench/results/c10k_epoll-lt.csv`, `bench/results/c10k_epoll-et.csv`].
