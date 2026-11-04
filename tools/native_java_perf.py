#!/usr/bin/env python3
"""Measure bounded native Java probe startup and execution latency.

The harness accepts a real jar and records each probe's wall-clock duration,
exit status, host-call count, loader kind, entrypoint count, and bounded error
output. It is intentionally independent of a Minecraft installation so it can
run in CI against compiled fixtures or a pinned real-mod corpus jar.

Examples:
  python3 tools/native_java_perf.py --jar /tmp/mod.jar --runs 10
  python3 tools/native_java_perf.py --jar /tmp/mod.jar --binary target/fast/horizon-native-java --output report.json
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MAX_RUNS = 100
MAX_ERROR = 512


def summarize(samples: list[float]) -> dict[str, float | int | None]:
    if not samples:
        return {"n": 0, "mean_ms": None, "p50_ms": None, "p95_ms": None, "min_ms": None, "max_ms": None}
    ordered = sorted(samples)
    percentile = lambda fraction: ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))]
    return {
        "n": len(ordered),
        "mean_ms": round(statistics.fmean(ordered), 3),
        "p50_ms": round(percentile(0.50), 3),
        "p95_ms": round(percentile(0.95), 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
    }


def binary_path(explicit: str | None) -> list[str]:
    if explicit:
        return [explicit]
    fast = ROOT / "target" / "fast" / "horizon-native-java"
    debug = ROOT / "target" / "debug" / "horizon-native-java"
    if fast.exists():
        return [str(fast)]
    if debug.exists():
        return [str(debug)]
    if shutil.which("cargo") is None:
        raise RuntimeError("horizon-native-java binary is missing and cargo is unavailable")
    return ["cargo", "run", "--quiet", "-p", "horizon-native-java", "--"]


def run_probe(command: list[str], jar: Path, kind: str | None, timeout: float) -> tuple[float, dict, str]:
    args = [*command, "--json", str(jar)]
    if kind:
        args.append(kind)
    started = time.perf_counter()
    try:
        result = subprocess.run(args, cwd=ROOT, text=True, capture_output=True, timeout=timeout, check=False)
        elapsed = (time.perf_counter() - started) * 1000.0
    except subprocess.TimeoutExpired as error:
        elapsed = (time.perf_counter() - started) * 1000.0
        return elapsed, {"status": "timeout", "error": str(error)[:MAX_ERROR]}, "timeout"
    stdout = result.stdout.strip()
    try:
        payload = json.loads(stdout) if stdout else {"status": "empty"}
    except json.JSONDecodeError:
        payload = {"status": "invalid-json", "stdout": stdout[:MAX_ERROR]}
    if result.returncode:
        payload.setdefault("status", "error")
        payload["returncode"] = result.returncode
        if result.stderr:
            payload["stderr"] = result.stderr[:MAX_ERROR]
    return elapsed, payload, "ok" if result.returncode == 0 else "error"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jar", required=True, type=Path)
    parser.add_argument("--binary")
    parser.add_argument("--kind")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args(argv)
    if not args.jar.is_file():
        parser.error(f"jar does not exist: {args.jar}")
    if not 1 <= args.runs <= MAX_RUNS:
        parser.error(f"--runs must be between 1 and {MAX_RUNS}")
    if args.timeout <= 0 or args.timeout > 300:
        parser.error("--timeout must be between 0 and 300 seconds")
    command = binary_path(args.binary)
    samples: list[float] = []
    reports: list[dict] = []
    for _ in range(args.runs):
        elapsed, report, outcome = run_probe(command, args.jar, args.kind, args.timeout)
        samples.append(elapsed)
        reports.append({"elapsed_ms": round(elapsed, 3), "outcome": outcome, "probe": report})
    result = {
        "schema": 1,
        "tool": "horizon-native-java-perf",
        "jar": str(args.jar),
        "runs": args.runs,
        "summary": summarize(samples),
        "samples": reports,
    }
    encoded = json.dumps(result, sort_keys=True, indent=2)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded + "\n", encoding="utf-8")
    print(encoded)
    return 0 if all(row["outcome"] == "ok" for row in reports) else 1


if __name__ == "__main__":
    raise SystemExit(main())
