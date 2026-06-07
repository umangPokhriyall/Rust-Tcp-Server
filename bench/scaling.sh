#!/usr/bin/env bash
# bench/scaling.sh — Phase 2 §6 multireactor scaling study.
#
# Sweeps `multireactor --workers` over WORKERS_LADDER at a fixed high
# concurrency / rate. Each point launches the server, runs loadgen for
# DURATION seconds, then parses the loadgen CSV the rest of the harness
# writes. Emits bench/results/multireactor_scaling.csv with columns:
#   workers,connections,rate,throughput_rps,p50,p99,p999
#
# Env overrides:
#   DURATION         per-point load duration in seconds (default 15)
#   POINT_BUDGET     wall-clock cap per point (default DURATION+90)
#   SCALING_CONNS    fixed concurrency (default 1000)
#   SCALING_RATE     fixed offered rate (default 80000)
#   PORT             starting TCP port (default 19080)
#   WORKERS_LADDER   space-separated worker counts (default 1 2 4 8)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DURATION="${DURATION:-15}"
POINT_BUDGET="${POINT_BUDGET:-$((DURATION + 90))}"
SCALING_CONNS="${SCALING_CONNS:-1000}"
SCALING_RATE="${SCALING_RATE:-80000}"
PORT="${PORT:-19080}"
WORKERS_LADDER="${WORKERS_LADDER:-1 2 4 8}"
SERVER_BIN="${SERVER_BIN:-target/release/server}"
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
RESULTS_DIR="${RESULTS_DIR:-bench/results}"
ASSETS_DIR="${ASSETS_DIR:-server/assets}"

mkdir -p "$RESULTS_DIR"
ulimit -n 65536 2>/dev/null || true

# Identical loadgen conditions to bench/run.sh — see _loadgen_wrap.sh.
LOADGEN_WRAP="${LOADGEN_WRAP:-bench/_loadgen_wrap.sh}"
export LOADGEN_BIN
export LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"

# Optional NUMA pinning (Phase 3 §3). Unset = empty prefix, byte-identical.
SERVER_NUMA=()
LOADGEN_NUMA=()
if [[ -n "${SERVER_NUMA_NODE:-}" ]]; then
    SERVER_NUMA=(numactl --cpunodebind="$SERVER_NUMA_NODE" --membind="$SERVER_NUMA_NODE")
fi
if [[ -n "${LOADGEN_NUMA_NODE:-}" ]]; then
    LOADGEN_NUMA=(numactl --cpunodebind="$LOADGEN_NUMA_NODE" --membind="$LOADGEN_NUMA_NODE")
fi

OUT="$RESULTS_DIR/multireactor_scaling.csv"
echo "workers,connections,rate,throughput_rps,p50,p99,p999" >"$OUT"

SERVER_PID=""
cleanup() {
    local pid="${SERVER_PID:-}"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..40}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.05
        done
        kill -KILL "$pid" 2>/dev/null || true
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
for W in $WORKERS_LADDER; do
    PORT_OFFSET=$((PORT_OFFSET + 1))
    port=$((PORT + PORT_OFFSET))
    log="$RESULTS_DIR/scaling_w${W}.log"
    csv="$RESULTS_DIR/scaling_w${W}.csv"
    : >"$csv"
    echo "=== scaling: workers=$W conns=$SCALING_CONNS rate=$SCALING_RATE port=$port dur=${DURATION}s ==="
    "${SERVER_NUMA[@]}" "$SERVER_BIN" --model multireactor --port "$port" \
        --assets-dir "$ASSETS_DIR" --workers "$W" \
        >"$log" 2>&1 &
    SERVER_PID=$!
    if ! wait_for_port "$port"; then
        echo "  server failed to come up; see $log" >&2
        cleanup
        continue
    fi
    sleep 0.2
    rc=0
    timeout --kill-after=5 "$POINT_BUDGET" \
        "${LOADGEN_NUMA[@]}" "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "multireactor-w${W}" \
            --rate "$SCALING_RATE" --connections "$SCALING_CONNS" \
            --duration "$DURATION" --out "$csv" \
            >>"$log" 2>&1 || rc=$?
    cleanup
    sleep 0.5
    if [[ $rc -ne 0 && $rc -ne 124 ]]; then
        echo "  loadgen exited rc=$rc"
    fi
    # Parse the last data row of the per-point CSV.
    if [[ -s "$csv" ]]; then
        row=$(tail -n 1 "$csv")
        IFS=',' read -r _model _rate _conns tp _errs p50 _p90 p99 p999 _p9999 _max <<<"$row"
        echo "$W,$SCALING_CONNS,$SCALING_RATE,$tp,$p50,$p99,$p999" >>"$OUT"
        echo "  workers=$W throughput=$tp p50=${p50}us p99=${p99}us"
    else
        echo "$W,$SCALING_CONNS,$SCALING_RATE,0.0,0,0,0" >>"$OUT"
        echo "  workers=$W: no data"
    fi
done

echo "wrote $OUT"
