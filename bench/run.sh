#!/usr/bin/env bash
# bench/run.sh — Phase 1 §7 harness.
#
# Two modes:
#   sweep (default): for each MODEL, run loadgen across the CONCURRENCY ladder
#                    (1, 10, 100, 1000, 10000), append rows to
#                    bench/results/<model>.csv, write per-point .hgrm dumps.
#   soak           : run a single long load against MODEL(s) to verify flat
#                    RSS / fd count / no zombies, sampling every SAMPLE_INTERVAL
#                    seconds into bench/results/soak_<model>.log.
#
# Usage:
#   bench/run.sh                             # sweep, all 9 models
#   bench/run.sh iterative epoll-et          # sweep, subset
#   bench/run.sh --perf                      # sweep + perf stat on one mid-range
#                                            # point per model (c=100)
#   MODE=soak DURATION=600 bench/run.sh      # 10-minute soak, all 9 models
#   MODE=soak DURATION=600 bench/run.sh iterative
#
# Env overrides:
#   DURATION         per-point load duration in seconds (default 10; soak 600)
#   POINT_BUDGET     per-point wall-clock cap in seconds, including loadgen's
#                    connect phase (default DURATION+90). Blocking models can
#                    take minutes to even establish 10 000 connections — the
#                    cap ensures the sweep finishes in bounded time.
#   PORT             starting TCP port (default 18080)
#   SOAK_CONNS       soak concurrency (default 100)
#   SOAK_RATE        soak rate rps (default 5000)
#   SAMPLE_INTERVAL  soak sampling cadence in seconds (default 10)
#   SERVER_BIN       path to the release server (default target/release/server)
#   LOADGEN_BIN      path to the release loadgen (default target/release/loadgen)
#   RESULTS_DIR      output directory (default bench/results)
#   ASSETS_DIR       server assets (default server/assets)
#
# The script is idempotent: each sweep run truncates <model>.csv before
# appending. It traps EXIT/INT/TERM and kills any spawned server, so Ctrl-C
# leaves no stragglers.

set -euo pipefail

# ----- Defaults -----

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DURATION="${DURATION:-10}"
POINT_BUDGET="${POINT_BUDGET:-$((DURATION + 90))}"
PORT="${PORT:-18080}"
SERVER_BIN="${SERVER_BIN:-target/release/server}"
LOADGEN_BIN="${LOADGEN_BIN:-target/release/loadgen}"
RESULTS_DIR="${RESULTS_DIR:-bench/results}"
ASSETS_DIR="${ASSETS_DIR:-server/assets}"
SOAK_CONNS="${SOAK_CONNS:-100}"
SOAK_RATE="${SOAK_RATE:-5000}"
SAMPLE_INTERVAL="${SAMPLE_INTERVAL:-10}"
MODE="${MODE:-sweep}"
PERF=0

ALL_MODELS=(iterative forking preforked thread-per-conn thread-pool poll epoll-lt epoll-et event-loop multireactor io-uring)
CONCURRENCY=(1 10 100 1000 10000)

# rate ladder paired with the concurrency ladder. Chosen so the higher rungs
# overload the blocking models (visible saturation) while staying within
# reach of the event-loop models.
declare -A RATE_FOR=(
    [1]=500
    [10]=5000
    [100]=20000
    [1000]=40000
    [10000]=50000
)

# ----- Arg parsing -----

MODELS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --perf) PERF=1 ;;
        --soak) MODE=soak ;;
        --sweep) MODE=sweep ;;
        --duration) DURATION="$2"; shift ;;
        -h|--help)
            sed -n '1,40p' "$0"
            exit 0
            ;;
        *) MODELS+=("$1") ;;
    esac
    shift
done
if [[ ${#MODELS[@]} -eq 0 ]]; then
    MODELS=("${ALL_MODELS[@]}")
fi
if [[ "$MODE" == "soak" && "$DURATION" == "10" ]]; then
    DURATION=600
fi

mkdir -p "$RESULTS_DIR"

# Bump open-file limit best-effort: c=10000 needs ~20k fds on both sides.
ulimit -n 65536 2>/dev/null || true

# Loadgen spawns one thread per connection. With the default 12.5 MiB stack
# (inherited from the shell's RLIMIT_STACK on Ubuntu), c=10000 reserves
# ~125 GiB of virtual memory and trips heuristic overcommit on smaller hosts
# (EAGAIN on pthread_create). bench/_loadgen_wrap.sh lowers the per-thread
# stack to LOADGEN_STACK_KIB (default 128 KiB) — safe for the worker
# function (small send/recv loop) — and exports RUST_MIN_STACK so Rust's
# thread::Builder honors it.
LOADGEN_WRAP="${LOADGEN_WRAP:-bench/_loadgen_wrap.sh}"
export LOADGEN_BIN
export LOADGEN_STACK_KIB="${LOADGEN_STACK_KIB:-128}"

# ----- Process lifecycle -----

SERVER_PID=""

cleanup() {
    local pid="${SERVER_PID:-}"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill -INT "$pid" 2>/dev/null || true
        # Wait briefly for graceful shutdown, then SIGKILL.
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

start_server() {
    local model=$1 port=$2
    local log="$RESULTS_DIR/server_${model}.log"
    "$SERVER_BIN" --model "$model" --port "$port" --assets-dir "$ASSETS_DIR" \
        >"$log" 2>&1 &
    SERVER_PID=$!
    if ! wait_for_port "$port"; then
        echo "[$model] server failed to come up on $port; see $log" >&2
        cleanup
        return 1
    fi
}

stop_server() { cleanup; }

# ----- Sweep -----

PORT_OFFSET=0

run_sweep_one() {
    local model=$1
    local csv="$RESULTS_DIR/${model}.csv"
    : >"$csv"   # truncate for idempotency
    echo "=== sweep: $model ==="
    for c in "${CONCURRENCY[@]}"; do
        local rate=${RATE_FOR[$c]}
        # Rotate the port across every point (globally, not per-model):
        # bind_listener on the non-reuse_port models doesn't set SO_REUSEADDR,
        # so TIME_WAIT from a finished point blocks bind for ~60s on the same
        # port. A fresh port per point sidesteps it without editing the
        # frozen `core`.
        local port=$((PORT + PORT_OFFSET))
        PORT_OFFSET=$((PORT_OFFSET + 1))
        echo "  -- c=$c rate=$rate dur=${DURATION}s port=$port"
        if ! start_server "$model" "$port"; then
            echo "     skip"
            continue
        fi
        # Give the event loop one extra epoll_wait cycle before opening fire.
        sleep 0.1
        local rc=0
        if [[ "$PERF" == "1" && "$c" == "100" ]] && command -v perf >/dev/null 2>&1; then
            timeout --kill-after=5 "$POINT_BUDGET" \
                perf stat -o "$RESULTS_DIR/perf_${model}_c${c}.txt" -- \
                "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "$model" \
                    --rate "$rate" --connections "$c" \
                    --duration "$DURATION" --out "$csv" \
                    >>"$RESULTS_DIR/server_${model}.log" 2>&1 || rc=$?
        else
            timeout --kill-after=5 "$POINT_BUDGET" \
                "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "$model" \
                    --rate "$rate" --connections "$c" \
                    --duration "$DURATION" --out "$csv" \
                    >>"$RESULTS_DIR/server_${model}.log" 2>&1 || rc=$?
        fi
        if [[ $rc -eq 124 ]]; then
            echo "     point exceeded ${POINT_BUDGET}s budget — recording a saturation row and moving on"
            # Loadgen never finished — write a row marking the point as saturated
            # so the sweep is dense and the plotter can render it. errors = -1
            # acts as a sentinel; the CSV header is unchanged.
            if [[ ! -s "$csv" ]]; then
                echo "model,rate,connections,throughput_rps,errors,p50,p90,p99,p999,p9999,max" >"$csv"
            fi
            echo "$model,$rate,$c,0.0,-1,0,0,0,0,0,0" >>"$csv"
        elif [[ $rc -ne 0 ]]; then
            echo "     loadgen exited rc=$rc — continuing"
        fi
        stop_server
        # Brief pause so the kernel reaps TIME_WAIT sockets between runs.
        sleep 0.5
    done
}

# ----- Soak -----

soak_one() {
    local model=$1
    # Rotate the soak port too — consecutive soaks of different models on the
    # same port hit TIME_WAIT on the non-reuse_port models.
    local port=$((PORT + 100 + PORT_OFFSET))
    PORT_OFFSET=$((PORT_OFFSET + 1))
    local log="$RESULTS_DIR/soak_${model}.log"
    local csv="$RESULTS_DIR/soak_${model}.csv"
    : >"$csv"
    echo "=== soak: $model dur=${DURATION}s c=$SOAK_CONNS rate=$SOAK_RATE ==="
    if ! start_server "$model" "$port"; then
        echo "[$model] soak: server did not start"
        return 1
    fi
    sleep 0.2

    echo "ts_s,rss_kib,fds,zombies" >"$log"
    local pid=$SERVER_PID
    local start_ts; start_ts=$(date +%s)

    # Sampler in the background.
    (
        while kill -0 "$pid" 2>/dev/null; do
            local now; now=$(date +%s)
            local elapsed=$(( now - start_ts ))
            local rss="?" fds="?" zombies=0
            if [[ -r /proc/$pid/status ]]; then
                rss=$(awk '/^VmRSS:/{print $2}' /proc/$pid/status 2>/dev/null || echo "?")
            fi
            if [[ -d /proc/$pid/fd ]]; then
                fds=$(ls /proc/$pid/fd 2>/dev/null | wc -l)
            fi
            # Count zombie children (state 'Z') of $pid plus its descendants.
            zombies=$(ps -A -o pid=,ppid=,stat= 2>/dev/null \
                | awk -v p=$pid '$2==p && $3 ~ /^Z/' | wc -l)
            echo "$elapsed,$rss,$fds,$zombies" >>"$log"
            sleep "$SAMPLE_INTERVAL"
        done
    ) &
    local sampler_pid=$!

    "$LOADGEN_WRAP" --target "127.0.0.1:$port" --model "$model" \
        --rate "$SOAK_RATE" --connections "$SOAK_CONNS" \
        --duration "$DURATION" --out "$csv" \
        >>"$RESULTS_DIR/server_${model}.log" 2>&1 \
        || echo "[$model] loadgen exited nonzero during soak"

    kill "$sampler_pid" 2>/dev/null || true
    wait "$sampler_pid" 2>/dev/null || true
    stop_server

    # Quick verdict over steady state (skipping the header row AND the first
    # data sample, which catches the server before loadgen's connections have
    # arrived — fd count jumps from `listener-only` to `listener + N conns`
    # between sample #1 and #2 and is not a leak).
    if [[ $(wc -l <"$log") -ge 4 ]]; then
        awk -F, 'NR>2 && $2 != "?" {
            if (!first_rss) { first_rss=$2; first_fd=$3 }
            last_rss=$2; last_fd=$3; max_z=(max_z>$4?max_z:$4)
        }
        END {
            if (first_rss=="" || first_rss==0) {
                printf "  verdict: insufficient steady-state samples\n"
            } else {
                d_rss=(last_rss-first_rss)*100.0/first_rss
                d_fd =(last_fd -first_fd )*100.0/(first_fd ?first_fd :1)
                printf "  verdict (steady): rss %d -> %d KiB (%.1f%%), fds %d -> %d (%.1f%%), zombies max %d\n",
                       first_rss, last_rss, d_rss, first_fd, last_fd, d_fd, max_z
            }
        }' "$log"
    fi
}

# ----- Main -----

case "$MODE" in
    sweep)
        for m in "${MODELS[@]}"; do
            run_sweep_one "$m"
        done
        ;;
    soak)
        for m in "${MODELS[@]}"; do
            soak_one "$m"
        done
        ;;
    *)
        echo "unknown MODE=$MODE (expected: sweep | soak)" >&2
        exit 2
        ;;
esac

echo "done."
