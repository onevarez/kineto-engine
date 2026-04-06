#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
bench_summary.py — kineto export-pipeline benchmark helper

Subcommands
-----------
  append   Append the current run's results into history.json
  report   Write a full GitHub-flavoured markdown job summary to stdout
           (current-run table + commit history trend table)
"""

import argparse
import io
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

# Ensure UTF-8 output so emoji in GitHub job summaries render correctly
if hasattr(sys.stdout, "reconfigure"):
    sys.stdout.reconfigure(encoding="utf-8")


# ── helpers ───────────────────────────────────────────────────────────────────

def load_env(path: str) -> dict:
    out = {}
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if "=" in line:
            k, _, v = line.partition("=")
            out[k.strip()] = v.strip()
    return out


def fmt_bytes(b: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if b < 1024.0:
            return f"{b:.1f} {unit}"
        b /= 1024.0
    return f"{b:.1f} TB"


def fmt_kb(kb: int) -> str:
    if not kb:
        return "n/a"
    return f"{kb / 1024:.0f} MB" if kb >= 1024 else f"{kb} KB"


def delta_str(current: float, previous: float) -> str:
    """Return a '+X.X%' / '-X.X%' string with emoji indicator."""
    if previous == 0:
        return ""
    pct = (current - previous) / previous * 100
    if abs(pct) < 0.5:
        return f"±0%"
    sign = "+" if pct > 0 else ""
    icon = " 🟢" if pct > 3 else (" 🔴" if pct < -3 else "")
    return f"{sign}{pct:.1f}%{icon}"


def parse_results(results_path: str) -> dict:
    """Return {command_name: {mean, stddev, min, max, runs}} from hyperfine JSON."""
    raw = json.loads(Path(results_path).read_text())
    return {
        r["command"]: {
            "mean":   r["mean"],
            "stddev": r.get("stddev", 0.0),
            "min":    r["min"],
            "max":    r["max"],
            "runs":   len(r.get("times", [])),
        }
        for r in raw["results"]
    }


# ── subcommand: append ────────────────────────────────────────────────────────

def cmd_append(args) -> None:
    """Append the current run into history.json (creates it if absent)."""
    hist_path = Path(args.history)
    history = json.loads(hist_path.read_text()) if hist_path.exists() else {"runs": []}

    results = parse_results(args.results)
    mem     = load_env(args.memory)
    meta    = load_env(args.meta)

    frames = int(meta.get("fixture_frames", 600))

    scenarios = {}
    for name, r in results.items():
        rss_kb = int(mem.get(name, 0) or 0)
        fps = frames / r["mean"] if r["mean"] > 0 else 0
        scenarios[name] = {
            "mean":    round(r["mean"],   3),
            "stddev":  round(r["stddev"], 3),
            "fps":     round(fps,         1),
            "peak_kb": rss_kb,
        }

    run = {
        "sha":         meta.get("git_sha", "")[:7],
        "sha_full":    meta.get("git_sha", ""),
        "date":        datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "branch":      meta.get("branch", ""),
        "run_url":     meta.get("run_url", ""),
        "platform":    meta.get("platform", "linux-x64"),
        "binary_size": int(meta.get("binary_size", 0)),
        "scenarios":   scenarios,
    }

    # Prepend so newest is first
    history["runs"].insert(0, run)

    hist_path.write_text(json.dumps(history, indent=2) + "\n")
    print(f"Appended run {run['sha']} to {hist_path}", file=sys.stderr)


# ── subcommand: report ────────────────────────────────────────────────────────

# Ordered display groups: each tuple is (label, [scenario-suffixes])
RESOLUTION_GROUPS = [
    ("360p  · 640×360",   ["360p-plain",  "360p-shadow",  "360p-zoom",  "360p-full"]),
    ("720p  · 1280×720",  ["720p-plain",  "720p-shadow",  "720p-zoom",  "720p-full"]),
    ("1080p · 1920×1080", ["1080p-plain", "1080p-shadow", "1080p-zoom", "1080p-full"]),
]

# Columns shown in the commit-history trend table (scenario → column label)
TREND_COLS = {
    "360p-plain":  "360p",
    "720p-plain":  "720p",
    "1080p-plain": "1080p",
    "1080p-full":  "1080p full",
}


def cmd_report(args) -> None:
    results = parse_results(args.results)
    mem     = load_env(args.memory)
    meta    = load_env(args.meta)

    git_sha     = meta.get("git_sha", "unknown")
    git_short   = git_sha[:7]
    binary_size = fmt_bytes(float(meta.get("binary_size", 0)))
    platform    = meta.get("platform", "unknown")
    run_url     = meta.get("run_url", "")
    frames      = int(meta.get("fixture_frames", 600))
    branch      = meta.get("branch", "")
    runs_count  = next(iter(results.values()), {}).get("runs", 0) if results else 0

    # Load history for "vs prev" deltas
    hist_path = Path(args.history) if args.history else None
    history   = json.loads(hist_path.read_text()) if (hist_path and hist_path.exists()) else {"runs": []}
    # previous run is runs[0] if it exists and doesn't match current sha
    prev_run = None
    for r in history["runs"]:
        if r["sha"] != git_short:
            prev_run = r
            break

    # ── Header ────────────────────────────────────────────────────────────────
    run_link = f"[#{meta.get('run_id', '')}]({run_url})" if run_url else meta.get("run_id", "")
    print("## Export Pipeline Benchmarks\n")
    print(
        f"> **Platform:** `{platform}` · "
        f"**Commit:** `{git_short}`"
        + (f" (`{branch}`)" if branch else "")
        + f" · **Binary:** `{binary_size}` · "
        f"**Run:** {run_link}\n"
    )

    # ── Current-run results table ─────────────────────────────────────────────
    print("### Results\n")
    print("| Scenario | Mean (s) | +/-sd | Min | Max | FPS | vs prev | Peak RAM |")
    print("|----------|----------|-------|-----|-----|-----|---------|----------|")

    for group_label, scenario_names in RESOLUTION_GROUPS:
        first = True
        for name in scenario_names:
            r = results.get(name)
            if r is None:
                continue
            rss_kb = int(mem.get(name, 0) or 0)
            fps    = frames / r["mean"] if r["mean"] > 0 else 0.0

            # delta vs previous run
            vs = ""
            if prev_run and name in prev_run.get("scenarios", {}):
                prev_fps = prev_run["scenarios"][name].get("fps", 0)
                vs = delta_str(fps, prev_fps)

            # first row of group carries the group label
            label = f"**{group_label}** · `{name}`" if first else f"`{name}`"
            first = False

            print(
                f"| {label} "
                f"| {r['mean']:.2f} | {r['stddev']:.2f} "
                f"| {r['min']:.2f} | {r['max']:.2f} "
                f"| {fps:.1f} | {vs} | {fmt_kb(rss_kb)} |"
            )

    print()
    print(
        f"_Fixture: Big Buck Bunny · H.264 input → H.264 output · "
        f"10 s · 60 fps · {frames} frames · {runs_count} runs (1 warmup)_"
    )

    # ── Commit history trend table ────────────────────────────────────────────
    runs = history.get("runs", [])
    if not runs:
        return

    print("\n---\n")
    print("### Commit History\n")

    trend_keys = list(TREND_COLS.keys())
    col_labels = list(TREND_COLS.values())

    header = "| Commit | Branch | " + " | ".join(f"{c} (fps)" for c in col_labels) + " | Binary |"
    sep    = "|--------|--------|" + "|".join("---" for _ in col_labels) + "|--------|"
    print(header)
    print(sep)

    for i, run in enumerate(runs[:15]):
        sha_cell = f"[`{run['sha']}`]({run['run_url']})" if run.get("run_url") else f"`{run['sha']}`"
        if run["sha"] == git_short:
            sha_cell += " ← current"

        fps_cells = []
        for key in trend_keys:
            scen = run.get("scenarios", {}).get(key)
            if scen is None:
                fps_cells.append("—")
                continue
            fps_val = scen.get("fps", 0)
            # delta vs next (older) row
            delta = ""
            if i + 1 < len(runs):
                prev_scen = runs[i + 1].get("scenarios", {}).get(key)
                if prev_scen:
                    delta = " " + delta_str(fps_val, prev_scen.get("fps", 0))
            fps_cells.append(f"{fps_val:.1f}{delta}")

        bin_cell = fmt_bytes(float(run.get("binary_size", 0)))
        branch_cell = f"`{run.get('branch', '')}`" if run.get("branch") else "—"
        print(f"| {sha_cell} | {branch_cell} | " + " | ".join(fps_cells) + f" | {bin_cell} |")

    print()
    print("_FPS delta relative to the previous entry. 🟢 > +3 %  🔴 < −3 %_")


# ── main ──────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)

    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--results",  required=True, help="hyperfine --export-json output")
    common.add_argument("--memory",   required=True, help="KEY=VALUE file with peak RSS in KB")
    common.add_argument("--meta",     required=True, help="KEY=VALUE file with build metadata")
    common.add_argument("--history",  default=None,  help="path to history.json")

    sub.add_parser("append", parents=[common],
                   help="Append current run to history.json")
    sub.add_parser("report", parents=[common],
                   help="Write full markdown summary to stdout")

    return p


def main() -> None:
    args = build_parser().parse_args()
    if args.cmd == "append":
        cmd_append(args)
    elif args.cmd == "report":
        cmd_report(args)


if __name__ == "__main__":
    main()
