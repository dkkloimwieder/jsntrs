#!/usr/bin/env bash
# Memory profiling: go-gnata vs jsntrs across payload sizes and string-length
# variants.  Measures peak RSS via /usr/bin/time -v, Go runtime.MemStats and
# the DHAT allocation profile of the Rust engine.
# Runs each measurement ONCE (no parallelism, no hyperfine).
#
# Go is optional: if the go-gnata engine is unavailable (no toolchain, build
# or handshake failure — see bench/lib.sh) its columns print "—" and the Rust
# and DHAT numbers are still produced.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

ROOT="$BENCH_LIB_ROOT"
# RS_BIN / GO_BIN come from lib.sh; the DHAT bin is built here only.
DHAT_BIN="$ROOT/target/profiling/jsntrs-dhat"

# ── Gate: GNU time ───────────────────────────────────────────────────────────
# Peak RSS comes from `/usr/bin/time -v`.  The bash builtin `time` and
# busybox time do not support -v, so a non-GNU binary is a hard error.
TIME_BIN="/usr/bin/time"
if [[ ! -x "$TIME_BIN" ]]; then
    echo "error: $TIME_BIN not found — install GNU time (e.g. apt install time)" >&2
    exit 1
fi
if ! "$TIME_BIN" --version 2>&1 | grep -qi 'GNU'; then
    echo "error: $TIME_BIN is not GNU time (-v peak RSS unavailable)" >&2
    exit 1
fi

# ── Build ────────────────────────────────────────────────────────────────────
echo "=== Building ==="
(cd "$ROOT" && cargo build --release -p jsntrs --features bench-bin --bin jsntrs-bench) 2>&1 | tail -1
(cd "$ROOT" && cargo build -p jsntrs --features dhat-heap --bin jsntrs-dhat --profile profiling) 2>&1 | tail -1
[[ -x "$RS_BIN" ]]   || { echo "error: $RS_BIN missing after build" >&2; exit 1; }
[[ -x "$DHAT_BIN" ]] || { echo "error: $DHAT_BIN missing after build" >&2; exit 1; }

GO_OK=1
if ! engine_available go-gnata; then
    GO_OK=0
    echo "note: go-gnata unavailable (${ENGINE_REASON[go-gnata]}) — Go columns show —"
fi

# Short-key expression
EXPR_SHORT='Account.Order.Product[UnitPrice > 50].SKU'
# Long-key expression
EXPR_LONG='Account.Order.Product[UnitPriceWithTaxIncluded > 50].StockKeepingUnitIdentifier'

# Config: (tag, datafile, iterations, expression)
declare -a CONFIGS=(
    "tiny_short|$BENCH_DIR/data.json|10000|$EXPR_SHORT"
    "1k_short|$BENCH_DIR/data_1k.json|100|$EXPR_SHORT"
    "10k_short|$BENCH_DIR/data_10k.json|10|$EXPR_SHORT"
    "10k_long|$BENCH_DIR/data_10k_long.json|10|$EXPR_LONG"
    "10k_mixed|$BENCH_DIR/data_10k_mixed.json|10|$EXPR_SHORT"
    "100k_short|$BENCH_DIR/data_100k.json|1|$EXPR_SHORT"
    "100k_long|$BENCH_DIR/data_100k_long.json|1|$EXPR_LONG"
    "100k_mixed|$BENCH_DIR/data_100k_mixed.json|1|$EXPR_SHORT"
)

pad_dash() { printf '—%*s' "$(($1 - 1))" ""; }

echo ""
printf "%-14s %10s  %-12s %-12s  %-14s %-14s  %-12s %-12s\n" \
  "Tag" "Bytes" "Go RSS(KB)" "Rust RSS(KB)" "Go HeapInUse" "Go TotalAlloc" "DHAT Total" "DHAT Blocks"
printf '%.0s─' {1..120}; echo ""

for CFG in "${CONFIGS[@]}"; do
    IFS='|' read -r TAG DF N EXPR <<< "$CFG"
    # The 10k/100k payloads are regenerable and not committed.
    [[ -r "$DF" ]] || { echo "$TAG — SKIPPED: fixture missing"; continue; }
    FILE_BYTES=$(wc -c < "$DF" | tr -d ' ')

    # Go: peak RSS + memstats (optional engine).
    # printf pads %-Ns by bytes and "—" is 3 bytes wide for one column, so the
    # placeholder is pre-padded to its column width instead.
    GO_RSS="$(pad_dash 12)" GO_HEAP="$(pad_dash 14)" GO_TOTAL="$(pad_dash 14)"
    if [[ $GO_OK -eq 1 ]]; then
        if GO_OUT=$("$TIME_BIN" -v "$GO_BIN" -expr "$EXPR" -datafile "$DF" -n "$N" -memstats 2>&1 >/dev/null); then
            GO_RSS=$(echo "$GO_OUT" | grep "Maximum resident" | awk '{print $NF}')
            GO_HEAP=$(echo "$GO_OUT" | grep "MEMSTATS:" | sed 's/.*heap_inuse=\([0-9]*\).*/\1/')
            GO_TOTAL=$(echo "$GO_OUT" | grep "MEMSTATS:" | sed 's/.*total_alloc=\([0-9]*\).*/\1/')
        else
            GO_RSS="ERR" GO_HEAP="ERR" GO_TOTAL="ERR"
        fi
    fi

    # Rust: peak RSS
    RS_OUT=$("$TIME_BIN" -v "$RS_BIN" -expr "$EXPR" -datafile "$DF" -n "$N" 2>&1 >/dev/null)
    RS_RSS=$(echo "$RS_OUT" | grep "Maximum resident" | awk '{print $NF}')

    # Rust: DHAT allocation profile (writes dhat-heap.json in the cwd)
    cd "$ROOT"
    DHAT_OUT=$("$DHAT_BIN" eval -expr "$EXPR" -datafile "$DF" -n "$N" 2>&1)
    DHAT_TOTAL=$(echo "$DHAT_OUT" | grep "dhat: Total:" | sed 's/.*Total:\s*\([0-9,]*\) bytes.*/\1/' | tr -d ',')
    DHAT_BLOCKS=$(echo "$DHAT_OUT" | grep "dhat: Total:" | sed 's/.*in \([0-9,]*\) blocks/\1/' | tr -d ',')
    rm -f dhat-heap.json

    printf "%-14s %10s  %-12s %-12s  %-14s %-14s  %-12s %-12s\n" \
      "$TAG" "$FILE_BYTES" "$GO_RSS" "$RS_RSS" "$GO_HEAP" "$GO_TOTAL" "$DHAT_TOTAL" "$DHAT_BLOCKS"
done

echo ""
echo "RSS = peak resident set size in KB (/usr/bin/time -v)"
echo "Go HeapInUse/TotalAlloc = runtime.MemStats after GC (bytes)"
echo "DHAT Total/Blocks = all allocations during run (bytes/count)"
