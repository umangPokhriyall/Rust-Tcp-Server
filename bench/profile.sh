#!/usr/bin/env bash
# bench/profile.sh — Phase 2 §7 profiling harness.
#
# Captures per-request syscall counts and context-switch counts for the
# three signal models: epoll-et (single-thread), multireactor (default
# workers), io-uring (single ring, single thread).
#
# Method (the documented fallback path — see profiles/README.md):
#   1. syscalls/req  : run the server under `strace -c -f -- server ...`,
#                      drive a fixed open-loop request budget, send SIGINT,
#                      let strace flush the summary to a file. Divide
#                      total syscalls by completed requests.
#   2. ctx-switches  : run the server normally, snapshot
#                      /proc/<pid>/status's voluntary_ctxt_switches and
#                      nonvoluntary_ctxt_switches before and after the
#                      load, divide the delta by completed requests.
#
# Top-down microarchitecture (perf stat --topdown) is unavailable: this
# host has /proc/sys/kernel/perf_event_paranoid = 4, which forbids all
# event access for non-CAP_PERFMON users. Documented in profiles/README.md.
#
# Outputs to bench/results/profiles/:
#   strace_<model>.txt              — `strace -c -f` summary
#   ctx_<model>.txt                 — pre/post /proc/<pid>/status deltas
#   loadgen_strace_<model>.csv      — loadgen results under strace
#   loadgen_ctx_<model>.csv         — loadgen results without strace
#   server_<model>.log              — server stdout/stderr
#   summary.csv                     — derived syscalls/req, ctxs/req table
#
# The script is idempotent: each run truncates its own outputs.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SERVER_BIN="${SERVER_BIN:-target/release/server}"
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
ASSETS_DIR="${ASSETS_DIR:-server/assets}"
OUT="${OUT:-bench/results/profiles}"
LOADGEN_WRAP="${LOADGEN_WRAP:-bench/_loadgen_wrap.sh}"
export LOADGEN_BIN
export LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"

# Workload knobs. Defaults chosen so the strace-attached server can keep
# up (low rate + low concurrency) and the request budget is large enough
# that the per-request denominators are statistically meaningful but the
# strace summary file stays small.
STRACE_RATE="${STRACE_RATE:-500}"
STRACE_CONNS="${STRACE_CONNS:-10}"
STRACE_DURATION="${STRACE_DURATION:-20}"

# Native (no strace) load for the ctx-switch capture: same shape, longer
# duration so the ctx-switch delta dominates the few hundred switches
# that fire on process startup/teardown.
CTX_RATE="${CTX_RATE:-2000}"
CTX_CONNS="${CTX_CONNS:-100}"
CTX_DURATION="${CTX_DURATION:-30}"

MODELS=("${@:-epoll-et multireactor io-uring}")
PORT_BASE="${PORT_BASE:-31000}"

mkdir -p "$OUT"
ulimit -n 65536 2>/dev/null || true

# ----- helpers -----

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

sum_ctxt() {
    # Sum voluntary + nonvoluntary context switches across the PID and
    # every thread under /proc/<pid>/task/*. Returns "vol nonvol total".
    local pid=$1
    awk '
        /^voluntary_ctxt_switches:/    { v += $2 }
        /^nonvoluntary_ctxt_switches:/ { n += $2 }
        END                            { printf "%d %d %d\n", v, n, v+n }
    ' /proc/"$pid"/task/*/status 2>/dev/null || echo "0 0 0"
}

requests_from_csv() {
    # The loadgen CSV row reports throughput_rps and the script knows the
    # duration — multiply for completed request count. Falls back to 0
    # when the CSV has no data row.
    local csv=$1 duration=$2
    awk -F, -v d="$duration" '
        NR==1 { next }
        { rps = $4; printf "%d\n", rps * d; exit }
    ' "$csv"
}

total_syscalls_from_strace() {
    # `strace -c -f` emits a summary table; the final " total" line carries
    # the grand syscall count in column 4 (after % time, seconds, usecs/call).
    local file=$1
    awk '
        /^[- ]+$/ { sep++; next }
        sep >= 2 && $NF == "total" { print $4; exit }
        sep >= 2 && /total$/       { print $4; exit }
    ' "$file"
}

# ----- per-model capture -----

start_under_strace() {
    local model=$1 port=$2 strace_out=$3 server_log=$4
    # strace as parent → ptrace_scope=1 is satisfied. -f follows clones so
    # multireactor's worker threads are traced too. -c gives the summary.
    strace -c -f -o "$strace_out" -- \
        "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        >"$server_log" 2>&1 &
    STRACE_PID=$!
    # The strace child is the server. Wait briefly for it to appear, then
    # for the port to come up.
    for _ in {1..200}; do
        SERVER_PID=$(pgrep -P "$STRACE_PID" 2>/dev/null | head -1 || true)
        [[ -n "$SERVER_PID" ]] && break
        sleep 0.05
    done
    if [[ -z "${SERVER_PID:-}" ]]; then
        echo "[$model] could not locate server PID under strace $STRACE_PID" >&2
        return 1
    fi
    wait_for_port "$port"
}

stop_under_strace() {
    # SIGINT the server; strace forwards it via PTRACE and exits, flushing
    # its -c summary to the -o file.
    local pid=$1
    if kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..60}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
    fi
    wait "$STRACE_PID" 2>/dev/null || true
}

start_native() {
    local model=$1 port=$2 server_log=$3
    "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        >"$server_log" 2>&1 &
    SERVER_PID=$!
    wait_for_port "$port"
}

stop_native() {
    local pid=$1
    if kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        for _ in {1..60}; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}

capture_model() {
    local model=$1 idx=$2
    local strace_port=$((PORT_BASE + idx*2))
    local ctx_port=$((PORT_BASE + idx*2 + 1))

    local strace_out="$OUT/strace_${model}.txt"
    local ctx_out="$OUT/ctx_${model}.txt"
    local strace_csv="$OUT/loadgen_strace_${model}.csv"
    local ctx_csv="$OUT/loadgen_ctx_${model}.csv"
    local s_log="$OUT/server_strace_${model}.log"
    local c_log="$OUT/server_ctx_${model}.log"

    : >"$strace_out"; : >"$ctx_out"; : >"$strace_csv"; : >"$ctx_csv"

    echo "=== $model: strace -c -f (rate=$STRACE_RATE c=$STRACE_CONNS dur=${STRACE_DURATION}s port=$strace_port) ==="
    start_under_strace "$model" "$strace_port" "$strace_out" "$s_log"
    sleep 0.2
    "$LOADGEN_WRAP" --target "127.0.0.1:$strace_port" --model "$model" \
        --rate "$STRACE_RATE" --connections "$STRACE_CONNS" \
        --duration "$STRACE_DURATION" --out "$strace_csv" \
        >>"$s_log" 2>&1 || echo "  loadgen exited nonzero (strace pass)"
    stop_under_strace "$SERVER_PID"

    echo "=== $model: native ctx-switch (rate=$CTX_RATE c=$CTX_CONNS dur=${CTX_DURATION}s port=$ctx_port) ==="
    start_native "$model" "$ctx_port" "$c_log"
    sleep 0.2
    local before; before=$(sum_ctxt "$SERVER_PID")
    "$LOADGEN_WRAP" --target "127.0.0.1:$ctx_port" --model "$model" \
        --rate "$CTX_RATE" --connections "$CTX_CONNS" \
        --duration "$CTX_DURATION" --out "$ctx_csv" \
        >>"$c_log" 2>&1 || echo "  loadgen exited nonzero (ctx pass)"
    local after; after=$(sum_ctxt "$SERVER_PID")
    {
        echo "# /proc/<pid>/task/*/status snapshots, summed across all threads."
        echo "# Columns: voluntary_ctxt_switches nonvoluntary_ctxt_switches total"
        echo "before $before"
        echo "after  $after"
        local bv bn bt av an at
        read -r bv bn bt <<<"$before"
        read -r av an at <<<"$after"
        echo "delta  $((av-bv)) $((an-bn)) $((at-bt))"
    } >"$ctx_out"
    stop_native "$SERVER_PID"
}

# ----- main -----

# Reflow MODELS from positional args without falling foul of "${@:-...}"
# splitting the default string token.
if [[ ${#MODELS[@]} -eq 1 && "${MODELS[0]}" == "epoll-et multireactor io-uring" ]]; then
    MODELS=(epoll-et multireactor io-uring)
fi

SERVER_PID=""
STRACE_PID=""
trap '[[ -n "$SERVER_PID" ]] && kill -KILL "$SERVER_PID" 2>/dev/null; [[ -n "$STRACE_PID" ]] && kill -KILL "$STRACE_PID" 2>/dev/null' EXIT

idx=0
for m in "${MODELS[@]}"; do
    capture_model "$m" "$idx"
    idx=$((idx + 1))
done

# ----- derive summary table -----

SUMMARY="$OUT/summary.csv"
echo "model,strace_total_syscalls,strace_requests,syscalls_per_req,ctx_vol_delta,ctx_nonvol_delta,ctx_total_delta,ctx_requests,ctx_switches_per_req" >"$SUMMARY"
for m in "${MODELS[@]}"; do
    strace_out="$OUT/strace_${m}.txt"
    strace_csv="$OUT/loadgen_strace_${m}.csv"
    ctx_out="$OUT/ctx_${m}.txt"
    ctx_csv="$OUT/loadgen_ctx_${m}.csv"

    sc=$(total_syscalls_from_strace "$strace_out" 2>/dev/null || echo 0)
    [[ -z "$sc" ]] && sc=0
    sr=$(requests_from_csv "$strace_csv" "$STRACE_DURATION" 2>/dev/null || echo 0)
    [[ -z "$sr" ]] && sr=0
    spr=$(awk -v s="$sc" -v r="$sr" 'BEGIN{if(r>0)printf "%.3f", s/r; else print "NA"}')

    read -r cv cn ct < <(awk '/^delta/ {print $2, $3, $4}' "$ctx_out")
    cv=${cv:-0}; cn=${cn:-0}; ct=${ct:-0}
    cr=$(requests_from_csv "$ctx_csv" "$CTX_DURATION" 2>/dev/null || echo 0)
    [[ -z "$cr" ]] && cr=0
    cpr=$(awk -v s="$ct" -v r="$cr" 'BEGIN{if(r>0)printf "%.3f", s/r; else print "NA"}')

    echo "$m,$sc,$sr,$spr,$cv,$cn,$ct,$cr,$cpr" >>"$SUMMARY"
done

echo
echo "--- summary ($SUMMARY) ---"
cat "$SUMMARY"
echo "done."
