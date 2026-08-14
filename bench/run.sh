#!/usr/bin/env bash
# Alias for the tiny preset of the benchmark matrix (was the old run.sh).
exec "$(dirname "$0")/run_matrix.sh" --preset tiny "$@"
