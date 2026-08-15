#!/usr/bin/env bash
# Configurable catalog benchmark runner for the jsntrs JSONata engine.
# Drives hyperfine over ~102 expressions in 16 categories, comparing every
# engine in the registry (bench/lib.sh): jsntrs, jsntrs-wasi, go-gnata,
# jsonata-js, jsonata-core, jsonata-rs.
#
# Usage:
#   ./bench/run_bench.sh                        # all categories, tiny data
#   ./bench/run_bench.sh --quick                # ~17 tagged benchmarks, fast
#   ./bench/run_bench.sh --full                 # all categories, all sizes
#   ./bench/run_bench.sh --section path,hof     # specific categories
#   ./bench/run_bench.sh --id path.simple       # specific benchmark
#   ./bench/run_bench.sh --size 1k,10k          # specific data sizes
#   ./bench/run_bench.sh --runner jsntrs,go-gnata   # explicit engine set
#   ./bench/run_bench.sh --dry-run              # print what would run
#
# Full flag set: --section --id --tag --size --warmup --min-runs --iters
#                --runner --probe-budget (default 5s) --csv PATH
#                --quick --full --dry-run
# Per-row hyperfine JSON lands in bench/results/json/<id>_<size>.json; --csv
# aggregates the rows THIS run timed into one wide row per (id, size).  That
# directory outlives the run and is shared with run_matrix.sh, so it is never
# globbed: a stale file must not become a row of the current run's CSV.
#
# Engines: with no --runner the registry auto-detects the whole set (gates
# 1-3: tool presence, build, handshake).  --runner takes the engine ids
# above or the legacy aliases go=go-gnata rust=jsntrs wasi=jsntrs-wasi
# js=jsonata-js, and hard-errors when a requested engine is unavailable.
#
# NOTE on hyperfine flags: --ignore-failure is deliberately NOT used.  A
# crashing runner (e.g. a WASI sandbox violation) was previously recorded as
# a ~2 ms "result", masking real performance data.  Hyperfine must abort on a
# non-zero exit so failures stay visible.  Engines that legitimately cannot
# run a given row are dropped beforehand by the per-row probe (gate 4, see
# --probe-budget), so anything reaching hyperfine is expected to succeed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

FIXTURES_DIR="$BENCH_DIR/fixtures"
JSON_DIR="$RESULTS_DIR/json"

# Expression files and wrapper scripts are mktemp'd per row; an interrupted
# run (Ctrl-C, SIGPIPE from a pager) must not leave them behind.
CLEANUP_FILES=()
cleanup() { [[ ${#CLEANUP_FILES[@]} -eq 0 ]] || rm -f "${CLEANUP_FILES[@]}"; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ── Defaults ─────────────────────────────────────────────────────────────────
SECTIONS=""
BENCH_ID=""
TAG=""
SIZES="tiny"
ITERS=10
WARMUP=5
MIN_RUNS=20
DRY_RUN=false
RUNNERS=""          # empty => auto-detect every registry engine
PROBE_BUDGET=5
EXPORT_CSV=""

# Legacy runner aliases from the gnata-era script; new ids pass through.
map_runner() {
    case "$1" in
        go)   echo go-gnata ;;
        rust) echo jsntrs ;;
        wasi) echo jsntrs-wasi ;;
        js)   echo jsonata-js ;;
        jsntrs|jsntrs-wasi|go-gnata|jsonata-js|jsonata-core|jsonata-rs) echo "$1" ;;
        *) return 1 ;;
    esac
}

# ── Parse args ───────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --section)  SECTIONS="$2"; shift 2 ;;
        --id)       BENCH_ID="$2"; shift 2 ;;
        --tag)      TAG="$2"; shift 2 ;;
        --size)     SIZES="$2"; shift 2 ;;
        --warmup)   WARMUP="$2"; shift 2 ;;
        --min-runs) MIN_RUNS="$2"; shift 2 ;;
        --iters)    ITERS="$2"; shift 2 ;;
        --runner)   RUNNERS="${2//,/ }"; shift 2 ;;
        --probe-budget) PROBE_BUDGET="$2"; shift 2 ;;
        --dry-run)  DRY_RUN=true; shift ;;
        --quick)    TAG="quick"; SIZES="tiny"; WARMUP=3; MIN_RUNS=10; shift ;;
        --full)     SIZES="tiny,1k,10k,100k"; WARMUP=5; MIN_RUNS=20; shift ;;
        --csv)      EXPORT_CSV="$2"; shift 2 ;;
        *)          echo "Unknown option: $1" >&2; exit 1 ;;
    esac
done

REQUESTED_ENGINES=()
if [[ -n "$RUNNERS" ]]; then
    for r in $RUNNERS; do
        mapped="$(map_runner "$r")" || { echo "Unknown runner: $r (want one of: ${ALL_ENGINES[*]} | go rust wasi js)" >&2; exit 1; }
        REQUESTED_ENGINES+=("$mapped")
    done
fi

# ── Fixture resolution ───────────────────────────────────────────────────────
# Prints "<size>\t<path>" for a fixture type and requested size, or nothing
# when no fixture exists at all.
#
# The printed size is the one actually resolved, which is not always the one
# asked for: when the requested size is missing (a dangling fixtures/account
# symlink is the common case) the fallback chain picks the smallest fixture
# present.  Callers must label the row with the returned size — labelling it
# with the requested size made the run report a payload it never benchmarked.
# The 21 MB inventory payload lives outside the repo: JSNTRS_INV_JSON points
# at it.
resolve_fixture() {
    local fixture_type="$1" size="$2"
    if [[ "$fixture_type" == "inventory" && -n "${JSNTRS_INV_JSON:-}" && -r "${JSNTRS_INV_JSON}" ]]; then
        printf '%s\t%s\n' "$size" "$JSNTRS_INV_JSON"
        return
    fi
    local candidate="$FIXTURES_DIR/$fixture_type/$size.json"
    if [[ -e "$candidate" ]]; then
        printf '%s\t%s\n' "$size" "$candidate"
        return
    fi
    # Fallback: smallest available
    for fallback in tiny 1k 10k 100k full; do
        local f="$FIXTURES_DIR/$fixture_type/$fallback.json"
        if [[ -e "$f" ]]; then
            printf '%s\t%s\n' "$fallback" "$f"
            return
        fi
    done
}

# ── Benchmark catalog ────────────────────────────────────────────────────────
# Each benchmark: "id|category|fixture|sizes|tags|expression"
# sizes and tags are semicolon-separated within their field.
BENCHMARKS=()

bench() {
    local id="$1" cat="$2" fixture="$3" sizes="$4" tags="$5" expr="$6"
    BENCHMARKS+=("$id|$cat|$fixture|$sizes|$tags|$expr")
}

# ── PATH ─────────────────────────────────────────────────────────────────────
bench "path.simple"      path account "tiny;1k;10k;100k" "quick" 'Account.Name'
bench "path.nested"      path account "tiny;1k;10k;100k" "quick" 'Account.Order.Product.SKU'
bench "path.wildcard"    path account "tiny;1k;10k"      ""      'Account.Order.*'
bench "path.descendant"  path account "tiny;1k;10k"      ""      '**[SKU]'
bench "path.parent"      path account "tiny;1k"          ""      'Account.Order.Product.%.OrderID'
bench "path.filter"      path account "tiny;1k;10k;100k" "quick" 'Account.Order.Product[UnitPrice > 50].SKU'
bench "path.index"       path account "tiny;1k;10k"      ""      'Account.Order[0].Product[0].SKU'
bench "path.keep_array"  path account "tiny;1k;10k"      ""      'Account.Order[].Product.SKU'

# ── STRING ───────────────────────────────────────────────────────────────────
bench "string.substring" string strings "1k" "quick" '$substring(items[0].text, 0, 10)'
bench "string.replace"   string strings "1k" ""      '$replace(items[0].text, '\''error'\'', '\''warning'\'')'
bench "string.split"     string strings "1k" ""      '$split(items[0].text, '\'' '\'')'
bench "string.join"      string strings "1k" ""      '$join($split(items[0].text, '\'' '\''), '\''-'\'')'
bench "string.trim"      string strings "1k" ""      '$trim(items[0].text)'
bench "string.pad"       string strings "1k" ""      '$pad(items[0].code, 10, '\''#'\'')'
bench "string.match"     string strings "1k" ""      '$match(items[0].text, /[A-Z][a-z]+/)'
bench "string.contains"  string strings "1k" ""      '$contains(items[0].text, '\''critical'\'')'
bench "string.lowercase" string strings "1k" ""      '$lowercase(items[0].text)'
bench "string.uppercase" string strings "1k" ""      '$uppercase(items[0].text)'
bench "string.length"    string strings "1k" ""      '$length(items[0].text)'
bench "string.before"    string strings "1k" ""      '$substringBefore(items[0].text, '\'' '\'')'
bench "string.after"     string strings "1k" ""      '$substringAfter(items[0].text, '\'' '\'')'

# ── NUMERIC ──────────────────────────────────────────────────────────────────
bench "numeric.abs"          numeric account "tiny;1k;10k" ""      'Account.Order.Product.($abs(UnitPrice - 50))'
bench "numeric.floor"        numeric account "tiny;1k;10k" ""      'Account.Order.Product.($floor(UnitPrice))'
bench "numeric.ceil"         numeric account "tiny;1k;10k" ""      'Account.Order.Product.($ceil(UnitPrice))'
bench "numeric.round"        numeric account "tiny;1k;10k" ""      'Account.Order.Product.($round(UnitPrice, 1))'
bench "numeric.power"        numeric account "tiny;1k;10k" ""      'Account.Order.Product.($power(Quantity, 2))'
bench "numeric.sqrt"         numeric account "tiny;1k;10k" ""      'Account.Order.Product.($sqrt(UnitPrice))'
bench "numeric.sum"          numeric account "tiny;1k;10k" "quick" '$sum(Account.Order.Product.(UnitPrice * Quantity))'
bench "numeric.max"          numeric account "tiny;1k;10k" ""      '$max(Account.Order.Product.UnitPrice)'
bench "numeric.min"          numeric account "tiny;1k;10k" ""      '$min(Account.Order.Product.UnitPrice)'
bench "numeric.average"      numeric account "tiny;1k;10k" ""      '$average(Account.Order.Product.UnitPrice)'
bench "numeric.formatNumber" numeric account "tiny;1k;10k" ""      'Account.Order.Product.($formatNumber(UnitPrice, '\''#,##0.00'\''))'
bench "numeric.formatBase"   numeric account "tiny;1k;10k" ""      '$formatBase(255, 16)'

# ── ARRAY ────────────────────────────────────────────────────────────────────
bench "array.append"   array account "tiny;1k;10k" ""      '$append(Account.Order[0].Product, Account.Order[1].Product)'
bench "array.sort"     array account "tiny;1k;10k" "quick" '$sort(Account.Order.Product, function($a,$b){$a.UnitPrice > $b.UnitPrice})'
bench "array.reverse"  array account "tiny;1k;10k" ""      '$reverse(Account.Order.Product)'
bench "array.distinct" array account "tiny;1k;10k" ""      '$distinct(Account.Order.Product.SKU)'
bench "array.flatten"  array account "tiny;1k;10k" ""      '$flatten([Account.Order.Product])'
bench "array.zip"      array account "tiny;1k;10k" ""      '$zip(Account.Order.Product.SKU, Account.Order.Product.UnitPrice)'
bench "array.count"    array account "tiny;1k;10k" ""      '$count(Account.Order.Product)'
bench "array.shuffle"  array account "tiny;1k;10k" ""      '$shuffle(Account.Order.Product)'

# ── OBJECT ───────────────────────────────────────────────────────────────────
bench "object.keys"   object account "tiny;1k;10k" "" '$keys(Account.Order[0].Product[0])'
bench "object.values" object account "tiny;1k;10k" "" '$values(Account.Order[0].Product[0])'
bench "object.spread" object account "tiny;1k;10k" "" '$spread(Account.Order[0].Product[0])'
bench "object.merge"  object account "tiny;1k;10k" "" '$merge(Account.Order.Product)'
bench "object.lookup" object account "tiny;1k;10k" "" '$lookup(Account.Order[0].Product[0], '\''SKU'\'')'
bench "object.each"   object account "tiny;1k;10k" "" '$each(Account.Order[0].Product[0], function($v, $k) { $k & '\''='\'' & $string($v) })'
bench "object.sift"   object account "tiny;1k;10k" "" '$sift(Account.Order[0].Product[0], function($v) { $type($v) = '\''number'\'' })'

# ── HOF ──────────────────────────────────────────────────────────────────────
bench "hof.map"      hof account "tiny;1k;10k" "quick" '$map(Account.Order.Product, function($v){$v.SKU & '\'': $'\'' & $string($v.UnitPrice)})'
bench "hof.filter"   hof account "tiny;1k;10k" "quick" '$filter(Account.Order.Product, function($v){$v.UnitPrice > 50})'
bench "hof.reduce"   hof account "tiny;1k;10k" "quick" '$reduce(Account.Order.Product, function($prev,$curr){$prev + $curr.UnitPrice}, 0)'
bench "hof.each"     hof account "tiny;1k;10k" ""      '$each(Account.Order[0].Product[0], function($v,$k){$k})'
bench "hof.sort_cmp" hof account "tiny;1k;10k" ""      '$sort(Account.Order.Product, function($a,$b){$a.Quantity > $b.Quantity})'
bench "hof.single"   hof account "tiny" ""      '$single(Account.Order.Product, function($v){$v.SKU = '\''040657863'\''})'
bench "hof.sift"     hof account "tiny;1k;10k" ""      '$sift(Account.Order[0].Product[0], function($v){$type($v) = '\''string'\''})'

# ── TYPE ─────────────────────────────────────────────────────────────────────
bench "type.type"    type account "tiny;1k;10k" ""      '$type(Account.Order)'
bench "type.string"  type account "tiny;1k;10k" "quick" 'Account.Order.Product.($string($))'
bench "type.number"  type account "tiny;1k;10k" ""      'Account.Order.Product.($number(UnitPrice))'
bench "type.boolean" type account "tiny;1k;10k" ""      '$boolean(Account.Order)'
bench "type.exists"  type account "tiny;1k;10k" ""      '$exists(Account.Order.Product)'

# ── DATETIME ─────────────────────────────────────────────────────────────────
bench "datetime.now"        datetime account  "tiny" ""  '$now()'
bench "datetime.toMillis"   datetime datetime "1k"   ""  '$toMillis(events[0].timestamp)'
bench "datetime.fromMillis" datetime datetime "1k"   ""  '$fromMillis(events[0].epoch_ms, '\''[Y]-[M01]-[D01]'\'')'

# ── REGEX ────────────────────────────────────────────────────────────────────
bench "regex.match"    regex strings "1k" "quick" '$match(items[0].text, /\b[A-Z]{2,}\b/)'
bench "regex.replace"  regex strings "1k" ""      '$replace(items[0].text, /[0-9]+/, '\''N'\'')'
bench "regex.split"    regex strings "1k" ""      '$split(items[0].text, /\s+/)'
bench "regex.contains" regex strings "1k" ""      '$contains(items[0].text, /error|fail/i)'

# ── ENCODING ─────────────────────────────────────────────────────────────────
bench "encoding.b64enc"      encoding strings "1k" "" '$base64encode(items[0].text)'
bench "encoding.b64dec"      encoding strings "1k" "" '$base64decode(items[0].b64)'
bench "encoding.urlenc"      encoding strings "1k" "" '$encodeUrlComponent(items[0].text)'
bench "encoding.urldec"      encoding strings "1k" "" '$decodeUrlComponent(items[0].urlencoded)'
bench "encoding.urlenc_full" encoding strings "1k" "" '$encodeUrl(items[0].url)'
bench "encoding.urldec_full" encoding strings "1k" "" '$decodeUrl(items[0].urlencoded_full)'

# ── OPERATOR ─────────────────────────────────────────────────────────────────
bench "op.arithmetic" operator account "tiny;1k;10k" "quick" 'Account.Order.Product.(UnitPrice * Quantity * (1 - Discount))'
bench "op.comparison" operator account "tiny;1k;10k" ""      'Account.Name = '\''Firefly'\'''
bench "op.lt_chain"   operator account "tiny;1k;10k" ""      'Account.Order.Product[UnitPrice > 20 and UnitPrice < 100]'
bench "op.logical"    operator account "tiny;1k;10k" ""      '$exists(Account.Order) and $count(Account.Order.Product) > 0'
bench "op.concat"     operator account "tiny;1k;10k" ""      'Account.Order.Product.(Description & '\'' (SKU: '\'' & SKU & '\'')'\'')'
bench "op.range"      operator account "tiny;1k;10k" ""      '[1..10]'
bench "op.coalesce"   operator account "tiny;1k;10k" ""      'Account.Missing ?? '\''default'\'''
bench "op.in"         operator account "tiny;1k;10k" ""      'Account.Order[0].Product[0].SKU in Account.Order.Product.SKU'
bench "op.ternary"    operator account "tiny;1k;10k" ""      '$count(Account.Order) > 1 ? '\''multi'\'' : '\''single'\'''
bench "op.compound"   operator account "tiny;1k;10k" ""      '$sum([1..100])'

# ── LAMBDA ───────────────────────────────────────────────────────────────────
bench "lambda.basic"   lambda account "tiny;1k;10k" "quick" '($f := function($x){$x * 2}; $f(21))'
bench "lambda.closure" lambda account "tiny;1k;10k" ""      '($factor := 1.1; $map(Account.Order.Product.UnitPrice, function($p){$p * $factor}))'
bench "lambda.nested"  lambda account "tiny;1k;10k" ""      '$map(Account.Order, function($o){$map($o.Product, function($p){$p.SKU})})'
bench "lambda.partial" lambda account "tiny;1k;10k" ""      '($add := function($a,$b){$a+$b}; $p := $add(10, ?); $p(5))'

# ── TRANSFORM ────────────────────────────────────────────────────────────────
bench "transform.chain"       transform account "tiny;1k;10k" "quick" 'Account.Order.Product ~> $map(function($v){$v.UnitPrice}) ~> $sum()'
bench "transform.pipe_filter" transform account "tiny;1k;10k" ""      'Account.Order.Product ~> $filter(function($v){$v.UnitPrice > 30}) ~> $count()'

# ── BLOCK ────────────────────────────────────────────────────────────────────
bench "block.binding"     block account "tiny;1k;10k" ""      '($x := Account.Name; $y := $count(Account.Order); $x & '\'': '\'' & $string($y) & '\'' orders'\'')'
bench "block.conditional" block account "tiny;1k;10k" ""      'Account.Order.Product.(UnitPrice > 50 ? '\''expensive'\'' : '\''cheap'\'')'
bench "block.compound"    block account "tiny;1k;10k" "quick" '($items := Account.Order.Product; $total := $sum($items.UnitPrice); $avg := $total / $count($items); {'\''total'\'': $total, '\''avg'\'': $avg})'

# ── COMPLEX ──────────────────────────────────────────────────────────────────
bench "complex.chained_hof"  complex account "tiny;1k;10k" "quick" 'Account.Order.Product ~> $filter(function($v){$v.UnitPrice > 30}) ~> $map(function($v){$v.SKU}) ~> $count()'
bench "complex.group_by"     complex account "tiny;1k;10k" "quick" 'Account.Order{OrderID: $sum(Product.UnitPrice)}'
bench "complex.nested_group" complex account "tiny;1k;10k" ""      'Account.Order{OrderID: Product{SKU: UnitPrice}}'
bench "complex.multi_sort"   complex account "tiny;1k;10k" ""      '$sort(Account.Order.Product, function($a,$b){$a.UnitPrice > $b.UnitPrice})'
bench "complex.recur_agg"    complex account "tiny;1k;10k" ""      '$sum(**.UnitPrice)'

# ── INVENTORY (real-world, 21MB) ─────────────────────────────────────────────
bench "inv.deep_path"     inventory inventory "full" "" 'data.inventory.edges.node.grn.grnPurchaseOrders.edges.node.purchaseOrder.number'
bench "inv.filter_active" inventory inventory "full" "" 'data.inventory.edges[node.active = true].node.number'
bench "inv.weight_sum"    inventory inventory "full" "" '$sum(data.inventory.edges.node.grossWeight)'
bench "inv.group_status"  inventory inventory "full" "" 'data.inventory.edges.node{inventoryStatusId: $count($)}'
bench "inv.address"       inventory inventory "full" "" 'data.inventory.edges.node.grn.grnPurchaseOrders.edges.node.purchaseOrder.shipToAddress.city'

# ── Filter + count ───────────────────────────────────────────────────────────
FILTERED=()
for entry in "${BENCHMARKS[@]}"; do
    IFS='|' read -r id cat fixture sizes tags expr <<< "$entry"

    # Filter by --section
    if [[ -n "$SECTIONS" ]]; then
        match=false
        IFS=',' read -ra sec_arr <<< "$SECTIONS"
        for s in "${sec_arr[@]}"; do
            [[ "$cat" == "$s" ]] && match=true
        done
        $match || continue
    fi

    # Filter by --id
    if [[ -n "$BENCH_ID" ]]; then
        match=false
        IFS=',' read -ra id_arr <<< "$BENCH_ID"
        for i in "${id_arr[@]}"; do
            [[ "$id" == "$i" ]] && match=true
        done
        $match || continue
    fi

    # Filter by --tag
    if [[ -n "$TAG" ]]; then
        [[ ";$tags;" == *";$TAG;"* ]] || continue
    fi

    FILTERED+=("$entry")
done

if [[ ${#FILTERED[@]} -eq 0 ]]; then
    echo "No benchmarks matched the selection." >&2
    exit 1
fi

echo ""
echo "  Benchmark suite: ${#FILTERED[@]} expressions"
echo "  Sizes: $SIZES"
$DRY_RUN && echo "  Mode: DRY RUN"
echo ""

# ── Engines ──────────────────────────────────────────────────────────────────
# Gates 1-3 live in lib.sh: engines_detect with no arguments auto-detects the
# whole registry; with arguments every requested engine must be available or
# it exits 1.  A dry run neither builds nor probes anything, so it just echoes
# the engine set that would be used.
if $DRY_RUN; then
    if [[ ${#REQUESTED_ENGINES[@]} -gt 0 ]]; then
        DETECTED_ENGINES=("${REQUESTED_ENGINES[@]}")
    else
        DETECTED_ENGINES=("${ALL_ENGINES[@]}")
    fi
    echo "  Engines (not detected in dry run): ${DETECTED_ENGINES[*]}"
    echo ""
else
    command -v hyperfine >/dev/null || { echo "error: hyperfine is required" >&2; exit 1; }
    if [[ ${#REQUESTED_ENGINES[@]} -gt 0 ]]; then
        engines_detect "${REQUESTED_ENGINES[@]}"
    else
        engines_detect
    fi
    echo ""
    mkdir -p "$JSON_DIR"
fi

# ── Run ──────────────────────────────────────────────────────────────────────
run_count=0
total_filtered=${#FILTERED[@]}

# Rows this invocation actually timed, as "id|size|hyperfine-json".  results/
# json is a persistent cache shared with run_matrix.sh and with earlier,
# differently scoped runs of this script, so --csv aggregates this list rather
# than globbing the directory.
RUN_ROWS=()
# id/size pairs already benchmarked, so a fallback that maps two requested
# sizes onto the same fixture does not run (and export) the same row twice.
declare -A ROW_DONE=()

for entry in "${FILTERED[@]}"; do
    IFS='|' read -r id cat fixture bench_sizes tags expr <<< "$entry"
    run_count=$((run_count + 1))

    # Resolve which sizes to run
    IFS=',' read -ra requested_sizes <<< "$SIZES"
    IFS=';' read -ra available_sizes <<< "$bench_sizes"
    run_sizes=()
    for rs in "${requested_sizes[@]}"; do
        for as in "${available_sizes[@]}"; do
            [[ "$rs" == "$as" ]] && run_sizes+=("$rs")
        done
    done
    # Fallback: if no overlap, use smallest available
    if [[ ${#run_sizes[@]} -eq 0 ]]; then
        run_sizes=("${available_sizes[0]}")
    fi

    for size in "${run_sizes[@]}"; do
        resolved=$(resolve_fixture "$fixture" "$size")
        if [[ -z "$resolved" ]]; then
            echo "  [$run_count/$total_filtered] $id ($size) — SKIPPED: no fixture" >&2
            continue
        fi
        # row_size is what was actually benchmarked; every label, the exported
        # JSON filename and the CSV all use it, never the requested size.
        IFS=$'\t' read -r row_size datafile <<< "$resolved"
        if [[ "$row_size" != "$size" ]]; then
            echo "  [$run_count/$total_filtered] $id: no $fixture/$size.json fixture — falling back to $row_size (row reported as $row_size)" >&2
        fi
        if [[ -n "${ROW_DONE[$id/$row_size]:-}" ]]; then
            echo "  [$run_count/$total_filtered] $id ($size) — SKIPPED: already benchmarked as $row_size" >&2
            continue
        fi
        ROW_DONE["$id/$row_size"]=1
        iters=$ITERS

        echo "── [$run_count/$total_filtered] $id ($row_size, $iters iters) ──"

        if $DRY_RUN; then
            for runner in "${DETECTED_ENGINES[@]}"; do
                echo "  DRY-RUN: would run $runner: $expr"
            done
            continue
        fi

        # Write expr to a temp file to avoid all shell quoting issues
        EXPR_FILE="$(write_expr_file "$expr")"
        CLEANUP_FILES+=("$EXPR_FILE")

        # Gate 4: per-row probe.  Run the row's real expression once against
        # its real datafile; engines that fail or blow the budget are dropped
        # for this row only, so hyperfine never sees a command that is known
        # to fail (which is why --ignore-failure stays off).
        ROW_ENGINES=() WRAPPERS=()
        for runner in "${DETECTED_ENGINES[@]}"; do
            if reason="$(probe_engine "$runner" "$EXPR_FILE" "$datafile" "$PROBE_BUDGET")"; then
                ROW_ENGINES+=("$runner")
            else
                echo "   $runner: dropped — $reason"
            fi
        done
        if [[ ${#ROW_ENGINES[@]} -eq 0 ]]; then
            echo "   SKIPPED: no engine survived the probe"
            echo ""
            rm -f "$EXPR_FILE"
            continue
        fi

        # Gate 5: hyperfine.  NOTE: --ignore-failure removed intentionally,
        # see the header comment.
        ROW_JSON="$JSON_DIR/${id}_${row_size}.json"
        HF_ARGS=(--warmup "$WARMUP" --min-runs "$MIN_RUNS" --shell=none)
        HF_ARGS+=(--export-json "$ROW_JSON")
        for runner in "${ROW_ENGINES[@]}"; do
            WRAPPER="$(engine_wrapper "$runner" "$EXPR_FILE" "$datafile" "$iters")"
            WRAPPERS+=("$WRAPPER")
            CLEANUP_FILES+=("$WRAPPER")
            HF_ARGS+=(-n "$runner" "$WRAPPER")
        done

        hyperfine "${HF_ARGS[@]}"
        RUN_ROWS+=("$id|$row_size|$ROW_JSON")
        echo ""

        rm -f "$EXPR_FILE" "${WRAPPERS[@]}"
    done
done

echo "=== Done: ${#FILTERED[@]} expressions benchmarked ==="

# ── CSV export ──────────────────────────────────────────────────────────────
# Wide format: one row per (benchmark id, size), three columns per engine.
# An engine dropped by the probe for that row has EMPTY cells, never 0.
#
# Only the rows THIS invocation timed are exported.  results/json persists
# between runs and is shared with run_matrix.sh, so globbing it folded stale
# JSON — earlier sizes, earlier engine sets, earlier code — into a CSV that
# claimed to describe the current run.
if [[ -n "$EXPORT_CSV" ]]; then
    echo ""
    if [[ ${#RUN_ROWS[@]} -eq 0 ]]; then
        echo "=== No rows were timed in this run — $EXPORT_CSV left untouched ===" >&2
    else
        echo "=== Generating CSV: $EXPORT_CSV (${#RUN_ROWS[@]} rows from this run) ==="
        python3 - "$EXPORT_CSV" "$ITERS" "${ALL_ENGINES[*]}" "${RUN_ROWS[@]}" <<'PYEOF'
import json, csv, sys

csv_path, iters, engines_str = sys.argv[1:4]
engine_order = engines_str.split()

rows = []
for entry in sys.argv[4:]:
    # "id|size|path", assembled by the run loop: no filename re-parsing, and
    # no chance of picking up a file this run did not write.
    bench_id, size, path = entry.split('|', 2)
    with open(path) as fh:
        data = json.load(fh)
    row = {'id': bench_id, 'size': size, 'iters': iters}
    for r in data['results']:
        name = r['command']
        row[f'{name}_mean_s'] = f"{r['mean']:.6f}"
        row[f'{name}_stddev_s'] = f"{(r.get('stddev') or 0):.6f}"
        row[f'{name}_median_s'] = f"{r['median']:.6f}"
    rows.append(row)

# Collect all engine columns actually present
engines = [n for n in engine_order if any(f'{n}_mean_s' in r for r in rows)]

fieldnames = ['id', 'size', 'iters']
for name in engines:
    fieldnames.extend([f'{name}_mean_s', f'{name}_stddev_s', f'{name}_median_s'])

# restval='' keeps probe-dropped engines as empty cells rather than zeros.
with open(csv_path, 'w', newline='') as csvfile:
    writer = csv.DictWriter(csvfile, fieldnames=fieldnames, restval='', extrasaction='ignore')
    writer.writeheader()
    for row in rows:
        writer.writerow(row)

print(f'Wrote {len(rows)} rows to {csv_path}')
PYEOF
        echo ""
        head -5 "$EXPORT_CSV"
        echo "..."
    fi
fi
