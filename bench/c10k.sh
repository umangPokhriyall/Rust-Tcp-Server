#!/usr/bin/env bash
# bench/c10k.sh — Phase 2 §6 C10K resource-curve capture.
#
# For every model in C10K_MODELS, drive a fixed C=10000 load for DURATION
# seconds while sampling /proc/<pid>/status (VmRSS) and the fd count every
# SAMPLE_INTERVAL seconds. Records:
#   bench/results/c10k_<model>.log         — header + ts_s,rss_kib,fds,zombies,ctx_voluntary,ctx_involuntary
#   bench/results/c10k_summary.csv         — one verdict row per model
#
# The thread/process models are expected to underperform here — record the
# symptom rather than skip. We rely on bench/run.sh's per-point CSV for the
# final throughput/p99 numbers; this script focuses on resource curves.
#
# Env overrides:
#   DURATION         per-model load duration (default 30)
#   POINT_BUDGET     wall-clock cap (default DURATION+120)
#   C10K_CONNS       concurrency (default 10000)
#   C10K_RATE        offered rate (default 50000)
#   PORT             starting TCP port (default 20080)
#   SAMPLE_INTERVAL  resource-sampling cadence in seconds (default 2)
#   C10K_MODELS      space-separated model list (default: all 11)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DURATION="${DURATION:-30}"
POINT_BUDGET="${POINT_BUDGET:-$((DURATION + 120))}"
# The nominal C10K target is 10000 concurrent keep-alive connections.
# On the benchmark host used for this run, the loadgen process cannot
# pthread_create 10000 worker threads — heuristic overcommit refuses
# the allocations once Committed_AS is already above CommitLimit
# (8 GiB physical / no headroom). C10K_CONNS=8000 is the highest
# concurrency the loadgen can sustain here; document the limit in
# bench/results/c10k_summary.csv and report it in the BENCHMARKS.
C10K_CONNS="${C10K_CONNS:-8000}"
C10K_RATE="${C10K_RATE:-50000}"
# Bump the server-side max_connections so single-reactor event-loop models
# can accept more than the default 1024 concurrent connections at C10K_CONNS.
C10K_MAX_CONNS="${C10K_MAX_CONNS:-16384}"
PORT="${PORT:-20080}"
SAMPLE_INTERVAL="${SAMPLE_INTERVAL:-2}"
SERVER_BIN="${SERVER_BIN:-target/release/server}"
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
RESULTS_DIR="${RESULTS_DIR:-bench/results}"
ASSETS_DIR="${ASSETS_DIR:-server/assets}"

DEFAULT_MODELS="iterative forking preforked thread-per-conn thread-pool poll epoll-lt epoll-et event-loop multireactor io-uring"
C10K_MODELS="${C10K_MODELS:-$DEFAULT_MODELS}"

mkdir -p "$RESULTS_DIR"
ulimit -n 65536 2>/dev/null || true

# Identical loadgen conditions to bench/run.sh — see _loadgen_wrap.sh.
LOADGEN_WRAP="${LOADGEN_WRAP:-bench/_loadgen_wrap.sh}"
export LOADGEN_BIN
export LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"

# Optional NUMA pinning (Phase 3 §3). Unset = empty prefix, byte-identical.
SERVER_NUMA=()
LOADGEN_NUMA=()
# Core-pin (SERVER_CPUS/LOADGEN_CPUS via --physcpubind) takes precedence over
# node-pin (SERVER_NUMA_NODE). Required under NPS1: with one NUMA node, disjoint-
# node isolation is impossible, so --physcpubind gives disjoint cores + disjoint
# per-CCD L3 (memory interleaved — the documented NPS1 caveat).
if [[ -n "${SERVER_CPUS:-}" ]]; then
    SERVER_NUMA=(numactl --physcpubind="$SERVER_CPUS" --membind="${MEMBIND_NODE:-0}")
elif [[ -n "${SERVER_NUMA_NODE:-}" ]]; then
    SERVER_NUMA=(numactl --cpunodebind="$SERVER_NUMA_NODE" --membind="$SERVER_NUMA_NODE")
fi
if [[ -n "${LOADGEN_CPUS:-}" ]]; then
    LOADGEN_NUMA=(numactl --physcpubind="$LOADGEN_CPUS" --membind="${MEMBIND_NODE:-0}")
elif [[ -n "${LOADGEN_NUMA_NODE:-}" ]]; then
    LOADGEN_NUMA=(numactl --cpunodebind="$LOADGEN_NUMA_NODE" --membind="$LOADGEN_NUMA_NODE")
fi

SUMMARY="$RESULTS_DIR/c10k_summary.csv"
echo "model,connections,rate,duration_s,first_rss_kib,last_rss_kib,first_fds,last_fds,max_zombies,ctx_voluntary,ctx_involuntary,verdict" >"$SUMMARY"

SERVER_PID=""
cleanup() {
    local pid="${SERVER_PID:-}"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..60}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
        # Also reap any forked children that may have been spawned.
        pkill -KILL -P "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
    SERVER_PID=""
}
trap cleanup EXIT INT TERM

wait_for_port() {
    local port=$1
    for _ in {1..400}; do
        if (exec 3<>/dev/tcp/127.0.0.1/"$port") 2>/dev/null; then
            exec 3<&- 3>&-
            return 0
        fi
        sleep 0.05
    done
    return 1
}

PORT_OFFSET=0
for model in $C10K_MODELS; do
    PORT_OFFSET=$((PORT_OFFSET + 1))
    port=$((PORT + PORT_OFFSET))
    log="$RESULTS_DIR/c10k_${model}.log"
    csv="$RESULTS_DIR/c10k_${model}.csv"
    server_log="$RESULTS_DIR/c10k_server_${model}.log"
    : >"$csv"
    echo "=== c10k: $model conns=$C10K_CONNS rate=$C10K_RATE port=$port dur=${DURATION}s ==="
    "${SERVER_NUMA[@]}" "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        --max-connections "$C10K_MAX_CONNS" \
        >"$server_log" 2>&1 &
    SERVER_PID=$!
    if ! wait_for_port "$port"; then
        echo "  server failed to come up; see $server_log" >&2
        echo "$model,$C10K_CONNS,$C10K_RATE,$DURATION,0,0,0,0,0,0,0,server-startup-failed" >>"$SUMMARY"
        cleanup
        continue
    fi
    sleep 0.2
    pid=$SERVER_PID

    echo "ts_s,rss_kib,fds,zombies,ctx_voluntary,ctx_involuntary" >"$log"
    start_ts=$(date +%s)
    (
        while kill -0 "$pid" 2>/dev/null; do
            now=$(date +%s)
            elapsed=$(( now - start_ts ))
            rss="?" fds="?" zombies=0 ctxv=0 ctxi=0
            if [[ -r /proc/$pid/status ]]; then
                rss=$(awk '/^VmRSS:/{print $2}' /proc/$pid/status 2>/dev/null || echo "?")
                ctxv=$(awk '/^voluntary_ctxt_switches:/{print $2}' /proc/$pid/status 2>/dev/null || echo 0)
                ctxi=$(awk '/^nonvoluntary_ctxt_switches:/{print $2}' /proc/$pid/status 2>/dev/null || echo 0)
            fi
            if [[ -d /proc/$pid/fd ]]; then
                fds=$(ls /proc/$pid/fd 2>/dev/null | wc -l)
            fi
            zombies=$(ps -A -o pid=,ppid=,stat= 2>/dev/null \
                | awk -v p=$pid '$2==p && $3 ~ /^Z/' | wc -l)
            echo "$elapsed,$rss,$fds,$zombies,$ctxv,$ctxi" >>"$log"
            sleep "$SAMPLE_INTERVAL"
        done
    ) &
    sampler_pid=$!

    rc=0
    timeout --kill-after=5 "$POINT_BUDGET" \
        "${LOADGEN_NUMA[@]}" "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "$model" \
            --rate "$C10K_RATE" --connections "$C10K_CONNS" \
            --duration "$DURATION" --out "$csv" \
            >>"$server_log" 2>&1 || rc=$?

    kill "$sampler_pid" 2>/dev/null || true
    wait "$sampler_pid" 2>/dev/null || true
    cleanup
    sleep 0.5

    # Build summary row from the resource log (skip header + sample #1 which
    # catches the server before connections arrive).
    awk -v model="$model" -v conns="$C10K_CONNS" -v rate="$C10K_RATE" \
        -v dur="$DURATION" -v summary="$SUMMARY" -v rc="$rc" '
        BEGIN { FS=OFS="," }
        NR>2 && $2 != "?" {
            if (first_rss=="") { first_rss=$2; first_fd=$3 }
            last_rss=$2; last_fd=$3
            if ($4+0 > max_z+0) max_z=$4
            ctxv=$5; ctxi=$6
        }
        END {
            if (first_rss=="") {
                verdict = "no-steady-state"
                first_rss=0; last_rss=0; first_fd=0; last_fd=0; max_z=0; ctxv=0; ctxi=0
            } else if (rc==124) {
                verdict = "saturated"
            } else if (rc!=0) {
                verdict = "loadgen-error"
            } else {
                verdict = "ok"
            }
            printf "%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n",
                model,conns,rate,dur,first_rss,last_rss,first_fd,last_fd,max_z,ctxv,ctxi,verdict >> summary
        }
    ' "$log"
done

echo "wrote $SUMMARY"
