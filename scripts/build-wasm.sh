#!/usr/bin/env bash
# Optimized WASM build: wasm-pack (--no-opt) plus a manual wasm-opt pass.
#
# Usage: scripts/build-wasm.sh [--allow-unoptimized]
#
# The wasm-opt pass is part of the deliverable, not a nicety — README.md
# advertises pkg/jsntrs_bg.wasm at ~830 KB, which is the post-wasm-opt size —
# so a missing wasm-opt is an error, checked before the slow cargo build.
# --allow-unoptimized builds anyway: the script then warns loudly and exits 3,
# so a caller (or a human skimming a log) can tell that artifact apart from a
# fully optimized build, which exits 0.
set -euo pipefail

ALLOW_UNOPT=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --allow-unoptimized) ALLOW_UNOPT=1; shift ;;
    -h | --help)
      echo "usage: scripts/build-wasm.sh [--allow-unoptimized]"
      echo "  exit 0: optimized build in pkg/"
      echo "  exit 3: --allow-unoptimized and wasm-opt missing — pkg/ is UNOPTIMIZED"
      exit 0
      ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

# Prefer newer wasm-opt from ~/.local/bin if available
if [[ -x "$HOME/.local/bin/wasm-opt" ]]; then
  export PATH="$HOME/.local/bin:$PATH"
fi

if ! command -v wasm-opt &>/dev/null && [[ $ALLOW_UNOPT -eq 0 ]]; then
  echo "error: wasm-opt not found in PATH — refusing to ship an unoptimized module." >&2
  echo "       Install binaryen (e.g. 'cargo install wasm-opt', or your package" >&2
  echo "       manager's binaryen), or re-run with --allow-unoptimized." >&2
  exit 1
fi

echo "Building jsntrs WASM (--target web)..."

# Use regex-lite for smaller binary (579KB vs 1.3MB) and opt-level=3 for
# runtime speed (30% faster eval vs opt-level="z", 830KB final).
# Build without wasm-opt (Rust 2024 emits bulk-memory ops that wasm-pack's
# built-in wasm-opt invocation doesn't handle). We run wasm-opt manually after.
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 \
  wasm-pack build --target web --out-dir ../../pkg crates/jsntrs --no-opt \
  -- --no-default-features --features regex-lite

artifacts() { ls -lh pkg/jsntrs_bg.wasm pkg/jsntrs.js pkg/jsntrs.d.ts; }

# Run wasm-opt with speed optimization (-O3) and bulk-memory enabled
if command -v wasm-opt &>/dev/null; then
  echo "Optimizing with wasm-opt ($(wasm-opt --version))..."
  wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int -o pkg/jsntrs_bg.wasm pkg/jsntrs_bg.wasm
  echo "Done. Artifacts in pkg/"
  artifacts
else
  # Only reachable with --allow-unoptimized: say so everywhere it can be seen.
  echo "" >&2
  echo "WARNING: wasm-opt not found — pkg/jsntrs_bg.wasm is UNOPTIMIZED." >&2
  echo "WARNING: it is markedly larger and slower than a release artifact." >&2
  echo "WARNING: do not publish or benchmark this build." >&2
  echo "Done (UNOPTIMIZED — wasm-opt missing). Artifacts in pkg/"
  artifacts
  exit 3
fi
