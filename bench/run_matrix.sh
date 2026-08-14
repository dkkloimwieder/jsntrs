#!/usr/bin/env bash
# Cross-engine JSONata benchmark matrix (replaces the old run.sh /
# run_scaled.sh / run_full.sh trio).
#
# Usage: bench/run_matrix.sh [--preset tiny|scaled|full] [options]
#
#   --preset P            tiny (default): 8 expressions on the 477-byte fixture
#                         scaled: 10k/100k payloads, short/long/mixed variants
#                         full:   1k/10k/100k grid -> tidy CSV
#   --engines a,b         explicit engine set; unavailable => hard error
#   --exclude-engines a,b drop engines from the auto-detected set
#   --payloads 1k,10k     filter the preset's payload grid
#   --variants short,long filter the preset's variant grid
#   --iters N             override per-row inner iterations
#   --probe-budget S      per-row probe wall-clock cap (default 5s)
#   --smoke               run each engine once per row and diff outputs
#                         (parsed as JSON) instead of timing
#   --strict              abort on the first hyperfine failure
#   --no-autogen          never regenerate missing fixtures
#   --csv PATH            tidy CSV output (default bench/results/matrix.csv)
#   --list-engines        print the availability banner and exit
#
# Engines: jsntrs jsntrs-wasi go-gnata jsonata-js jsonata-core jsonata-rs
# (see bench/lib.sh for the five availability gates; bench/README.md for
# methodology and known asymmetries).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

PRESET=tiny
ENGINES_ARG="" EXCLUDE_ARG="" PAYLOADS_ARG="" VARIANTS_ARG=""
ITERS_ARG="" PROBE_BUDGET=5 SMOKE=0 STRICT=0 NO_AUTOGEN=0 LIST_ONLY=0
CSV="$RESULTS_DIR/matrix.csv"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --preset) PRESET="$2"; shift 2 ;;
    --engines) ENGINES_ARG="$2"; shift 2 ;;
    --exclude-engines) EXCLUDE_ARG="$2"; shift 2 ;;
    --payloads) PAYLOADS_ARG="$2"; shift 2 ;;
    --variants) VARIANTS_ARG="$2"; shift 2 ;;
    --iters) ITERS_ARG="$2"; shift 2 ;;
    --probe-budget) PROBE_BUDGET="$2"; shift 2 ;;
    --smoke) SMOKE=1; shift ;;
    --strict) STRICT=1; shift ;;
    --no-autogen) NO_AUTOGEN=1; shift ;;
    --csv) CSV="$2"; shift 2 ;;
    --list-engines) LIST_ONLY=1; shift ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

command -v hyperfine >/dev/null || { echo "error: hyperfine is required" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 is required" >&2; exit 1; }

# ── Engine set ───────────────────────────────────────────────────────────────

if [[ -n "$ENGINES_ARG" ]]; then
  IFS=',' read -ra REQ <<<"$ENGINES_ARG"
  engines_detect "${REQ[@]}"
else
  engines_detect
  if [[ -n "$EXCLUDE_ARG" ]]; then
    KEPT=()
    for e in "${DETECTED_ENGINES[@]}"; do
      [[ ",$EXCLUDE_ARG," == *",$e,"* ]] || KEPT+=("$e")
    done
    DETECTED_ENGINES=("${KEPT[@]}")
  fi
fi
[[ $LIST_ONLY -eq 1 ]] && exit 0
echo ""

# ── Expression sets ──────────────────────────────────────────────────────────

declare -A EXPRS_TINY=(
  [simple_path]="Account.Name"
  [nested_path]="Account.Order.Product.SKU"
  [filter_path]='Account.Order.Product[UnitPrice > 50].SKU'
  [aggregation]='$sum(Account.Order.Product.(UnitPrice * Quantity * (1 - Discount)))'
  [comparison]='Account.Name = "Firefly"'
  [func_exists]='$exists(Account.Order)'
  [func_count]='$count(Account.Order.Product.SKU)'
  [string_concat]='Account.Name & " Corp"'
)
TINY_ORDER=(simple_path nested_path filter_path aggregation comparison func_exists func_count string_concat)

declare -A EXPRS_SHORT=(
  [nested_path]="Account.Order.Product.SKU"
  [filter_path]='Account.Order.Product[UnitPrice > 50].SKU'
  [aggregation]='$sum(Account.Order.Product.(UnitPrice * Quantity * (1 - Discount)))'
  [func_count]='$count(Account.Order.Product.SKU)'
)
declare -A EXPRS_LONG=(
  [nested_path]="Account.Order.Product.StockKeepingUnitIdentifier"
  [filter_path]='Account.Order.Product[UnitPriceWithTaxIncluded > 50].StockKeepingUnitIdentifier'
  [aggregation]='$sum(Account.Order.Product.(UnitPriceWithTaxIncluded * QuantityInWarehouseStock * (1 - ApplicableDiscountPercentage)))'
  [func_count]='$count(Account.Order.Product.StockKeepingUnitIdentifier)'
)
GRID_ORDER=(nested_path filter_path aggregation func_count)

# ── Row grid per preset: payload variant datafile default_iters expr_set ────

ROWS=()  # "payload|variant|datafile|iters|exprset"
case "$PRESET" in
  tiny)
    ROWS+=("tiny|short|$BENCH_DIR/data.json|${ITERS_ARG:-10000}|TINY")
    ;;
  scaled)
    ROWS+=("10k|short|$BENCH_DIR/data_10k.json|${ITERS_ARG:-100}|SHORT")
    ROWS+=("10k|long|$BENCH_DIR/data_10k_long.json|${ITERS_ARG:-100}|LONG")
    ROWS+=("10k|mixed|$BENCH_DIR/data_10k_mixed.json|${ITERS_ARG:-100}|SHORT")
    ROWS+=("100k|short|$BENCH_DIR/data_100k.json|${ITERS_ARG:-10}|SHORT")
    ROWS+=("100k|long|$BENCH_DIR/data_100k_long.json|${ITERS_ARG:-10}|LONG")
    ROWS+=("100k|mixed|$BENCH_DIR/data_100k_mixed.json|${ITERS_ARG:-10}|SHORT")
    ;;
  full)
    ROWS+=("1k|short|$BENCH_DIR/data_1k.json|${ITERS_ARG:-1000}|SHORT")
    ROWS+=("1k|long|$BENCH_DIR/data_1k_long.json|${ITERS_ARG:-1000}|LONG")
    ROWS+=("10k|short|$BENCH_DIR/data_10k.json|${ITERS_ARG:-100}|SHORT")
    ROWS+=("10k|long|$BENCH_DIR/data_10k_long.json|${ITERS_ARG:-100}|LONG")
    ROWS+=("10k|mixed|$BENCH_DIR/data_10k_mixed.json|${ITERS_ARG:-100}|SHORT")
    ROWS+=("100k|short|$BENCH_DIR/data_100k.json|${ITERS_ARG:-10}|SHORT")
    ROWS+=("100k|long|$BENCH_DIR/data_100k_long.json|${ITERS_ARG:-10}|LONG")
    ROWS+=("100k|mixed|$BENCH_DIR/data_100k_mixed.json|${ITERS_ARG:-10}|SHORT")
    ;;
  *) echo "unknown preset: $PRESET" >&2; exit 2 ;;
esac

# ── Fixture presence / autogen ───────────────────────────────────────────────

ensure_fixture() {
  local df="$1"
  [[ -r "$df" ]] && return 0
  if [[ $NO_AUTOGEN -eq 0 && -x "$(command -v python3)" && -f "$SCRIPT_DIR/generate_fixtures.py" ]]; then
    echo "fixture $df missing — regenerating (disable with --no-autogen)"
    python3 "$SCRIPT_DIR/generate_fixtures.py" >/dev/null || true
  fi
  [[ -r "$df" ]]
}

# ── CSV ──────────────────────────────────────────────────────────────────────

mkdir -p "$RESULTS_DIR/json"
CSV_HEADER="preset,payload,variant,expression,engine,iters,status,mean_s,stddev_s,median_s,min_s,max_s,method,note"
[[ -f "$CSV" ]] || echo "$CSV_HEADER" >"$CSV"
grep -q "^preset," "$CSV" || sed -i "1i $CSV_HEADER" "$CSV"

csv_row() { # payload variant expr engine iters status mean stddev median min max note
  echo "$PRESET,$1,$2,$3,$4,$5,$6,$7,$8,$9,${10},${11},$(engine_method "$4"),\"${12}\"" >>"$CSV"
}

# ── Main loop ────────────────────────────────────────────────────────────────

for ROW in "${ROWS[@]}"; do
  IFS='|' read -r PAYLOAD VARIANT DATAFILE ITERS EXPRSET <<<"$ROW"
  [[ -n "$PAYLOADS_ARG" && ",$PAYLOADS_ARG," != *",$PAYLOAD,"* ]] && continue
  [[ -n "$VARIANTS_ARG" && ",$VARIANTS_ARG," != *",$VARIANT,"* ]] && continue

  if ! ensure_fixture "$DATAFILE"; then
    echo "── $PAYLOAD/$VARIANT: fixture $(basename "$DATAFILE") missing — rows skipped"
    for e in "${DETECTED_ENGINES[@]}"; do
      for NAME in "${GRID_ORDER[@]}"; do
        csv_row "$PAYLOAD" "$VARIANT" "$NAME" "$e" "$ITERS" skipped "" "" "" "" "" "fixture missing"
      done
    done
    continue
  fi

  case "$EXPRSET" in
    TINY) NAMES=("${TINY_ORDER[@]}") ;;
    *) NAMES=("${GRID_ORDER[@]}") ;;
  esac

  for NAME in "${NAMES[@]}"; do
    case "$EXPRSET" in
      TINY) EXPR="${EXPRS_TINY[$NAME]}" ;;
      SHORT) EXPR="${EXPRS_SHORT[$NAME]}" ;;
      LONG) EXPR="${EXPRS_LONG[$NAME]}" ;;
    esac
    TAG="${PAYLOAD}_${VARIANT}"
    echo "── $TAG / $NAME ($(wc -c <"$DATAFILE" | tr -d ' ') bytes, $ITERS iters) ──"
    EF="$(write_expr_file "$EXPR")"

    # Gate 4: per-row probe. Engines that fail or exceed the budget are
    # dropped for this row only, with the reason recorded in the CSV.
    ROW_ENGINES=() WRAPPERS=()
    for e in "${DETECTED_ENGINES[@]}"; do
      if reason="$(probe_engine "$e" "$EF" "$DATAFILE" "$PROBE_BUDGET")"; then
        ROW_ENGINES+=("$e")
      else
        echo "   $e: dropped — $reason"
        csv_row "$PAYLOAD" "$VARIANT" "$NAME" "$e" "$ITERS" skipped "" "" "" "" "" "$reason"
      fi
    done
    if [[ ${#ROW_ENGINES[@]} -eq 0 ]]; then
      rm -f "$EF"
      continue
    fi

    if [[ $SMOKE -eq 1 ]]; then
      # Run each engine once; compare outputs parsed as JSON against the
      # first engine's. Differences are warnings (number formatting may
      # legitimately drift between engines), never failures.
      REF="" REF_ENGINE=""
      for e in "${ROW_ENGINES[@]}"; do
        w="$(engine_wrapper "$e" "$EF" "$DATAFILE" 1)"
        out="$("$w" 2>/dev/null || echo "__ENGINE_ERROR__")"
        rm -f "$w"
        if [[ -z "$REF_ENGINE" ]]; then
          REF="$out" REF_ENGINE="$e"
          echo "   $e: $(head -c 60 <<<"$out")"
          continue
        fi
        python3 - "$REF" "$out" "$REF_ENGINE" "$e" <<'PYEOF' || true
import json, sys
a, b, na, nb = sys.argv[1:5]
def parse(s):
    s = s.strip()
    if not s: return None
    try: return json.loads(s)
    except Exception: return ("UNPARSEABLE", s)
pa, pb = parse(a), parse(b)
if pa == pb:
    print(f"   {nb}: matches {na}")
else:
    print(f"   WARNING: {nb} differs from {na}: {b[:60]!r} vs {a[:60]!r}")
PYEOF
      done
      rm -f "$EF"
      continue
    fi

    # Gate 5: hyperfine, no --ignore-failure.
    HF_ARGS=(--warmup 3 --min-runs 10 --shell=none)
    RESULT_FILE="$RESULTS_DIR/json/${TAG}_${NAME}.json"
    HF_ARGS+=(--export-json "$RESULT_FILE")
    for e in "${ROW_ENGINES[@]}"; do
      w="$(engine_wrapper "$e" "$EF" "$DATAFILE" "$ITERS")"
      WRAPPERS+=("$w")
      HF_ARGS+=(-n "$e" "$w")
    done
    if hyperfine "${HF_ARGS[@]}"; then
      METHODS=""
      for e in "${ROW_ENGINES[@]}"; do
        METHODS+="$e=$(engine_method "$e") "
      done
      python3 - "$RESULT_FILE" "$PRESET" "$PAYLOAD" "$VARIANT" "$NAME" "$ITERS" "$METHODS" <<'PYEOF' >>"$CSV"
import json, sys
path, preset, payload, variant, name, iters, methods_str = sys.argv[1:8]
data = json.load(open(path))
methods = dict(item.split("=", 1) for item in methods_str.split())
for r in data["results"]:
    e = r["command"]
    print(f'{preset},{payload},{variant},{name},{e},{iters},ok,'
          f'{r["mean"]:.6f},{r.get("stddev") or 0:.6f},{r["median"]:.6f},'
          f'{r["min"]:.6f},{r["max"]:.6f},{methods.get(e, "parse_once")},""')
PYEOF
    else
      echo "   hyperfine FAILED for $TAG/$NAME"
      for e in "${ROW_ENGINES[@]}"; do
        csv_row "$PAYLOAD" "$VARIANT" "$NAME" "$e" "$ITERS" error "" "" "" "" "" "hyperfine run failed"
      done
      [[ $STRICT -eq 1 ]] && { rm -f "$EF" "${WRAPPERS[@]}"; exit 1; }
    fi
    rm -f "$EF" "${WRAPPERS[@]}"
    echo ""
  done
done

if [[ $SMOKE -eq 0 ]]; then
  echo "════════════════════════════════════════════════════════════"
  echo "  tidy CSV: $CSV"
  echo "════════════════════════════════════════════════════════════"
  python3 "$SCRIPT_DIR/summarize.py" "$CSV" --format table || true
fi
