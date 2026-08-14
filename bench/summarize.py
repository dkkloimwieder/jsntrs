#!/usr/bin/env python3
"""Summarize the tidy benchmark CSV produced by run_matrix.sh.

Usage:
    python3 bench/summarize.py bench/results/matrix.csv [--format table|markdown|wide-csv]

The tidy CSV has one row per (preset, payload, variant, expression, engine).
Missing/skipped engines are absent or status=skipped rows — they render as
"—" and never as 0, and no ratio is ever computed against a missing engine.
Engines labelled method=parse_per_iter (jsonata-rs re-parses its input every
evaluation) are footnoted.
"""

import argparse
import csv
import sys
from collections import OrderedDict

ENGINE_ORDER = ["jsntrs", "jsntrs-wasi", "go-gnata", "jsonata-js", "jsonata-core", "jsonata-rs"]


def load(path):
    rows = []
    with open(path, newline="") as f:
        for row in csv.DictReader(f):
            rows.append(row)
    return rows


def pivot(rows):
    """-> OrderedDict[(preset,payload,variant,expression)] = {engine: row}"""
    table = OrderedDict()
    for r in rows:
        key = (r["preset"], r["payload"], r["variant"], r["expression"])
        table.setdefault(key, {})[r["engine"]] = r
    return table


def engines_present(rows):
    seen = {r["engine"] for r in rows}
    ordered = [e for e in ENGINE_ORDER if e in seen]
    return ordered + sorted(seen - set(ENGINE_ORDER))


def fmt_cell(cell):
    if cell is None or cell.get("status") != "ok" or not cell.get("mean_s"):
        return "—"
    mean = float(cell["mean_s"])
    if mean >= 1:
        return f"{mean:.3f}s"
    return f"{mean * 1000:.1f}ms"


def render_table(rows, out=sys.stdout, markdown=False):
    engines = engines_present(rows)
    per_iter = sorted(
        {r["engine"] for r in rows if r.get("method") == "parse_per_iter"}
    )
    table = pivot(rows)
    header = ["payload", "variant", "expression"] + engines
    widths = [max(len(h), 10) for h in header]

    def line(cells):
        if markdown:
            out.write("| " + " | ".join(cells) + " |\n")
        else:
            out.write("  ".join(c.ljust(w) for c, w in zip(cells, widths)).rstrip() + "\n")

    line(header)
    if markdown:
        out.write("|" + "|".join("---" for _ in header) + "|\n")
    else:
        out.write("  ".join("-" * w for w in widths) + "\n")
    for (_preset, payload, variant, expression), cells in table.items():
        line([payload, variant, expression] + [fmt_cell(cells.get(e)) for e in engines])
    skipped = [
        (k, e, c.get("note", ""))
        for k, cells in table.items()
        for e, c in cells.items()
        if c.get("status") == "skipped"
    ]
    if skipped:
        out.write("\nskipped cells:\n")
        for (_, payload, variant, expression), engine, note in skipped:
            out.write(f"  {payload}/{variant}/{expression} {engine}: {note}\n")
    for e in per_iter:
        out.write(f"\nnote: {e} re-parses the input document on every evaluation "
                  f"(no pre-parsed-input API); its timings include per-iteration JSON parsing.\n")


def render_wide_csv(rows, out=sys.stdout):
    engines = engines_present(rows)
    table = pivot(rows)
    w = csv.writer(out)
    header = ["preset", "payload", "variant", "expression", "iters"]
    for e in engines:
        header += [f"{e}_mean_s", f"{e}_stddev_s"]
    w.writerow(header)
    for (preset, payload, variant, expression), cells in table.items():
        iters = next((c["iters"] for c in cells.values() if c.get("iters")), "")
        row = [preset, payload, variant, expression, iters]
        for e in engines:
            c = cells.get(e)
            if c is None or c.get("status") != "ok" or not c.get("mean_s"):
                row += ["", ""]
            else:
                row += [c["mean_s"], c["stddev_s"]]
        w.writerow(row)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv_path")
    ap.add_argument("--format", choices=["table", "markdown", "wide-csv"], default="table")
    args = ap.parse_args()
    rows = load(args.csv_path)
    if not rows:
        print("no rows", file=sys.stderr)
        return 1
    if args.format == "wide-csv":
        render_wide_csv(rows)
    else:
        render_table(rows, markdown=(args.format == "markdown"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
