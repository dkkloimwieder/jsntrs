#!/usr/bin/env bash
# Engine registry and availability gates shared by run_matrix.sh and
# run_bench.sh. Source this file; do not execute it.
#
# Engine ids: jsntrs jsntrs-wasi go-gnata jsonata-js jsonata-core jsonata-rs
#
# An engine only produces a number after clearing five gates:
#   1. tool presence (command -v)
#   2. build (log in results/build-<id>.log)
#   3. handshake probe (-n 1 on a trivial expression, output must match)
#   4. per-row probe (-n 1 on the row's real expression/datafile, wall-clock
#      capped by --probe-budget) — run by the orchestrators before hyperfine
#   5. hyperfine itself, WITHOUT --ignore-failure: everything that may
#      legitimately fail was filtered by gate 4, so a failure here is a real
#      regression and must abort the row loudly.
#
# Test hook: JSNTRS_FAKE_MISSING=go-gnata,jsonata-js forces those engines to
# report unavailable so the skip path is testable without mangling PATH.

BENCH_LIB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BENCH_DIR="$BENCH_LIB_ROOT/bench"
RESULTS_DIR="$BENCH_DIR/results"

ALL_ENGINES=(jsntrs jsntrs-wasi go-gnata jsonata-js jsonata-core jsonata-rs)

# Workspace builds land in the root target/, not crates/jsntrs/target/.
RS_BIN="$BENCH_LIB_ROOT/target/release/jsntrs-bench"
RS_WASM="$BENCH_LIB_ROOT/target/wasm32-wasip2/release/jsntrs-bench.wasm"
GO_BIN="$BENCH_DIR/bin/go-gnata"
JS_BENCH="$BENCH_DIR/engines/js/bench.js"
JC_BIN="$BENCH_DIR/engines/jsonata-core/target/release/jsonata-core-bench"
JR_BIN="$BENCH_DIR/engines/jsonata-rs/target/release/jsonata-rs-bench"

# method label per engine, propagated into the CSV: every engine parses the
# input document once except jsonata-rs, whose public API re-parses per eval.
engine_method() {
  case "$1" in
    jsonata-rs) echo "parse_per_iter" ;;
    *) echo "parse_once" ;;
  esac
}

declare -A ENGINE_STATE   # id -> ok | skip
declare -A ENGINE_REASON  # id -> short reason / version note

_fake_missing() {
  [[ -n "${JSNTRS_FAKE_MISSING:-}" ]] && [[ ",$JSNTRS_FAKE_MISSING," == *",$1,"* ]]
}

_tool_missing() {
  # echoes the missing tool name, if any
  case "$1" in
    jsntrs) command -v cargo >/dev/null || echo "cargo" ;;
    jsntrs-wasi)
      command -v cargo >/dev/null || { echo "cargo"; return; }
      command -v wasmtime >/dev/null || { echo "wasmtime"; return; }
      rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip2$' \
        || echo "rust target wasm32-wasip2 (rustup target add wasm32-wasip2)"
      ;;
    go-gnata) command -v go >/dev/null || echo "go" ;;
    jsonata-js)
      command -v node >/dev/null || { echo "node"; return; }
      command -v npm >/dev/null || echo "npm"
      ;;
    jsonata-core | jsonata-rs) command -v cargo >/dev/null || echo "cargo" ;;
  esac
}

_engine_build() {
  local id="$1" log="$RESULTS_DIR/build-$1.log"
  mkdir -p "$RESULTS_DIR"
  case "$id" in
    jsntrs)
      (cd "$BENCH_LIB_ROOT" \
        && cargo build --release -p jsntrs --features bench-bin --bin jsntrs-bench) \
        >"$log" 2>&1
      ;;
    jsntrs-wasi)
      (cd "$BENCH_LIB_ROOT" \
        && cargo build --release -p jsntrs --target wasm32-wasip2 \
          --features bench-bin --bin jsntrs-bench) >"$log" 2>&1
      ;;
    go-gnata)
      (cd "$BENCH_DIR/engines/go" && mkdir -p "$BENCH_DIR/bin" \
        && go build -o "$GO_BIN" .) >"$log" 2>&1
      ;;
    jsonata-js)
      (cd "$BENCH_DIR/engines/js" \
        && { [[ -d node_modules/jsonata ]] || npm ci; }) >"$log" 2>&1
      ;;
    jsonata-core)
      (cd "$BENCH_DIR/engines/jsonata-core" && cargo build --release) >"$log" 2>&1
      ;;
    jsonata-rs)
      (cd "$BENCH_DIR/engines/jsonata-rs" && cargo build --release) >"$log" 2>&1
      ;;
  esac
}

_engine_version_note() {
  case "$1" in
    jsntrs) echo "crates/jsntrs release, mimalloc" ;;
    jsntrs-wasi) echo "$(wasmtime --version 2>/dev/null | head -1)" ;;
    go-gnata) echo "recolabs/gnata $(grep -oP 'recolabs/gnata \Kv[0-9.]+' "$BENCH_DIR/engines/go/go.mod")" ;;
    jsonata-js) echo "jsonata $(node -e 'console.log(require(process.argv[1]+"/engines/js/node_modules/jsonata/package.json").version)' "$BENCH_DIR" 2>/dev/null)" ;;
    jsonata-core) echo "$(grep -oP 'jsonata-core = "=\K[0-9.]+' "$BENCH_DIR/engines/jsonata-core/Cargo.toml")" ;;
    jsonata-rs) echo "$(grep -oP 'jsonata-rs = "=\K[0-9.]+' "$BENCH_DIR/engines/jsonata-rs/Cargo.toml")  [input re-parsed per iteration]" ;;
  esac
}

# engine_cmdline ID EXPR_FILE DATAFILE N -> prints the shell command line.
# The expression is read from a file to dodge quoting hazards ($-prefixed
# JSONata functions, quotes) — callers write it with write_expr_file.
engine_cmdline() {
  local id="$1" expr_file="$2" datafile="$3" n="$4"
  case "$id" in
    jsntrs)
      printf '%s -expr "$(cat %q)" -datafile %q -n %s' "$RS_BIN" "$expr_file" "$datafile" "$n"
      ;;
    jsntrs-wasi)
      # wasmtime sandbox: bench/ is mapped to /bench. A datafile that lives
      # elsewhere (e.g. resolved through a symlink) gets its real directory
      # mapped to /data.
      local real dir base extra=""
      real="$(readlink -f "$datafile")"
      dir="$(dirname "$real")" base="$(basename "$real")"
      if [[ "$dir" == "$BENCH_DIR" ]]; then
        printf '%s --dir %q::/bench %q -- -expr "$(cat %q)" -datafile /bench/%q -n %s' \
          "$(command -v wasmtime)" "$BENCH_DIR" "$RS_WASM" "$expr_file" "$base" "$n"
      else
        printf '%s --dir %q::/bench --dir %q::/data %q -- -expr "$(cat %q)" -datafile /data/%q -n %s' \
          "$(command -v wasmtime)" "$BENCH_DIR" "$dir" "$RS_WASM" "$expr_file" "$base" "$n"
      fi
      ;;
    go-gnata)
      printf '%s -expr "$(cat %q)" -datafile %q -n %s' "$GO_BIN" "$expr_file" "$datafile" "$n"
      ;;
    jsonata-js)
      printf '%s %q -expr "$(cat %q)" -datafile %q -n %s' "$(command -v node)" "$JS_BENCH" "$expr_file" "$datafile" "$n"
      ;;
    jsonata-core)
      printf '%s -expr "$(cat %q)" -datafile %q -n %s' "$JC_BIN" "$expr_file" "$datafile" "$n"
      ;;
    jsonata-rs)
      printf '%s -expr "$(cat %q)" -datafile %q -n %s' "$JR_BIN" "$expr_file" "$datafile" "$n"
      ;;
  esac
}

# engine_wrapper ID EXPR_FILE DATAFILE N -> writes an executable mktemp
# wrapper running the engine once, echoes its path. hyperfine runs these with
# --shell=none, so each wrapper is a self-contained bash script.
engine_wrapper() {
  local id="$1" w
  w="$(mktemp "${TMPDIR:-/tmp}/jsntrs-bench-$id-XXXXXX.sh")"
  {
    echo '#!/usr/bin/env bash'
    echo "exec $(engine_cmdline "$@")"
  } >"$w"
  chmod +x "$w"
  echo "$w"
}

write_expr_file() {
  # write_expr_file EXPR -> echoes a mktemp file containing it
  local f
  f="$(mktemp "${TMPDIR:-/tmp}/jsntrs-expr-XXXXXX.jsonata")"
  printf '%s' "$1" >"$f"
  echo "$f"
}

# engine_available ID -> 0/1, caches in ENGINE_STATE/ENGINE_REASON.
# Gates 1-3 (tool, build, handshake).
engine_available() {
  local id="$1"
  [[ -n "${ENGINE_STATE[$id]:-}" ]] && [[ "${ENGINE_STATE[$id]}" == ok ]] && return 0
  [[ -n "${ENGINE_STATE[$id]:-}" ]] && return 1

  if _fake_missing "$id"; then
    ENGINE_STATE[$id]=skip ENGINE_REASON[$id]="faked missing (JSNTRS_FAKE_MISSING)"
    return 1
  fi
  local missing
  missing="$(_tool_missing "$id")"
  if [[ -n "$missing" ]]; then
    ENGINE_STATE[$id]=skip ENGINE_REASON[$id]="$missing not found"
    return 1
  fi
  if ! _engine_build "$id"; then
    ENGINE_STATE[$id]=skip ENGINE_REASON[$id]="build failed (see results/build-$id.log)"
    return 1
  fi
  # gate 3: handshake
  local ef out
  ef="$(write_expr_file 'Account.Name')"
  out="$(eval "$(engine_cmdline "$id" "$ef" /dev/stdin 1)" <<<'{"Account":{"Name":"probe"}}' 2>/dev/null)" || out=""
  if [[ "$out" != *probe* ]]; then
    # /dev/stdin does not survive every engine (wasmtime sandbox); fall back
    # to a temp datafile in bench/ before declaring the handshake failed.
    local df="$BENCH_DIR/.handshake.json"
    printf '{"Account":{"Name":"probe"}}' >"$df"
    out="$(eval "$(engine_cmdline "$id" "$ef" "$df" 1)" 2>/dev/null)" || out=""
    rm -f "$df"
  fi
  rm -f "$ef"
  if [[ "$out" != *probe* ]]; then
    ENGINE_STATE[$id]=skip ENGINE_REASON[$id]="handshake probe failed"
    return 1
  fi
  ENGINE_STATE[$id]=ok ENGINE_REASON[$id]="$(_engine_version_note "$id")"
  return 0
}

# probe_engine ID EXPR_FILE DATAFILE BUDGET_SECONDS -> gate 4.
# 0 = keep; 1 = drop (reason echoed).
probe_engine() {
  local id="$1" ef="$2" df="$3" budget="$4" w rc
  w="$(engine_wrapper "$id" "$ef" "$df" 1)"
  timeout "$budget" "$w" >/dev/null 2>&1
  rc=$?
  rm -f "$w"
  if [[ $rc -eq 124 ]]; then
    echo "probe exceeded ${budget}s budget"
    return 1
  elif [[ $rc -ne 0 ]]; then
    echo "probe failed (exit $rc)"
    return 1
  fi
  return 0
}

# engines_detect [id ...] — resolve the engine set and print the banner.
# With ids: every requested engine must be available or this exits 1.
# Without: auto-detect over ALL_ENGINES. Result in DETECTED_ENGINES[@].
engines_detect() {
  local requested=("$@") id hard=0
  DETECTED_ENGINES=()
  if [[ ${#requested[@]} -gt 0 ]]; then
    hard=1
  else
    requested=("${ALL_ENGINES[@]}")
  fi
  echo "Engine availability"
  for id in "${requested[@]}"; do
    if engine_available "$id"; then
      printf '  %-13s ok    %s\n' "$id" "${ENGINE_REASON[$id]}"
      DETECTED_ENGINES+=("$id")
    else
      printf '  %-13s SKIP  %s\n' "$id" "${ENGINE_REASON[$id]}"
      if [[ $hard -eq 1 ]]; then
        echo "error: requested engine '$id' is unavailable" >&2
        exit 1
      fi
    fi
  done
  if [[ ${#DETECTED_ENGINES[@]} -eq 0 ]]; then
    echo "error: no engines available" >&2
    exit 1
  fi
}
