#!/usr/bin/env bash
# bench/_loadgen_wrap.sh — invoke loadgen under a tight per-thread stack so
# high-concurrency points (c=1000+) fit on a small-RAM host.
#
# Background: loadgen spawns one thread per connection. With the default
# 12.5 MiB stack (inherited from the shell's RLIMIT_STACK), c=10000 reserves
# ~125 GiB of virtual memory and trips heuristic overcommit on hosts with
# limited RAM (EAGAIN on pthread_create). 128 KiB per thread is safe for
# the worker function (small send/recv loop) and brings the reservation
# under 1.3 GiB.
#
# Env:
#   LOADGEN_BIN         path to loadgen binary (default target/release/loadgen)
#   LOADGEN_STACK_KIB   per-thread stack in KiB (default 128)
#
# Usage:
#   bench/_loadgen_wrap.sh <loadgen args...>
set -euo pipefail
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"
ulimit -s "$LOADGEN_STACK_KIB" 2>/dev/null || true
export RUST_MIN_STACK=$((LOADGEN_STACK_KIB * 1024))
exec "$LOADGEN_BIN" "$@"
