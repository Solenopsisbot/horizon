#!/usr/bin/env python3
"""Measure bounded native Java probe startup and execution latency.

The harness accepts a real jar and records each probe's wall-clock duration,
exit status, host-call count, loader kind, entrypoint count, and bounded error
output. It is intentionally independent of a Minecraft installation so it can
run in CI against compiled fixtures or a pinned real-mod corpus jar.

Two modes exist, and neither changes what the probe itself does:

``--runs`` (default)
  Run the jar's discovered entrypoint ``N`` times and summarize the wall clock.
  Every sample is one whole probe process, so this is the startup+execute cost.

``--method CLASS[#NAME]`` (benchmark mode, may be repeated)
  Time each named class/method on its own: the harness passes it as the probe's
  explicit entrypoint (``horizon-native-java --json <jar> <kind> <class>``),
  which is exactly the address the CLI already accepts. Each method gets its
  own warmup runs (discarded) followed by bounded measured runs, reported as
  ``warmup``, ``p50``/``p95``/``p99`` and the run metadata a number is only
  meaningful with: the ``backend`` (how the probe was started) and the
  ``cache`` label (whether this method paid the loader cost first).

Examples:
  python3 tools/native_java_perf.py --jar /tmp/mod.jar --runs 10
  python3 tools/native_java_perf.py --jar /tmp/mod.jar --binary target/fast/horizon-native-java --output report.json
  python3 tools/native_java_perf.py --jar /tmp/mod.jar --method demo.Main#onInitialize --method demo.Main#tick
"""
from __future__ import annotations

import argparse
import json
import math
import re
import shutil
import statistics
import subprocess
import sys
import time
from collections.abc import Callable, Sequence
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MAX_RUNS = 100
MAX_ERROR = 512
# Benchmark mode bounds. A per-method table is read by humans, so the number of
# methods is capped the same way runs are, and warmup stays below the measured
# sample count so a report can never be all warmup.
MAX_METHODS = 32
MAX_WARMUP = 50
DEFAULT_WARMUP = 3
# `owner#name` or `owner$Inner#name`, then an optional descriptor, then nothing:
# a trailing `:42` is the line-number form some tools copy from a stack trace.
METHOD_PATTERN = re.compile(r"^[A-Za-z_$][\w$]*(?:[./][\w$]+)*(?:#[\w$<>]+(?:\([^)\s]*\)[\w/\[;.$]*)?)?$")
CACHE_COLD = "cold"
CACHE_WARM = "warm"


def summarize(samples: list[float]) -> dict[str, float | int | None]:
    if not samples:
        return {
            "n": 0, "mean_ms": None, "p50_ms": None, "p95_ms": None,
            "p99_ms": None, "min_ms": None, "max_ms": None,
        }
    ordered = sorted(samples)
    percentile = lambda fraction: ordered[min(len(ordered) - 1, int((len(ordered) - 1) * fraction))]
    return {
        "n": len(ordered),
        "mean_ms": round(statistics.fmean(ordered), 3),
        "p50_ms": round(percentile(0.50), 3),
        "p95_ms": round(percentile(0.95), 3),
        "p99_ms": round(percentile(0.99), 3),
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


def backend_label(explicit: str | None, command: Sequence[str]) -> str:
    """Name how the probe runs, so a timing never floats free of its cause.

    ``cargo`` is a build-and-run wrapper (its own overhead dominates), a
    ``target/fast`` binary is optimized, ``target/debug`` is not, and anything
    else is reported verbatim so a caller's custom build keeps its identity.
    """
    if command[:1] == ["cargo"]:
        return "cargo-run"
    path = Path(command[-1])
    if explicit:
        return "explicit"
    parts = path.parts
    if "target" in parts:
        index = parts.index("target")
        if index + 1 < len(parts):
            profile = parts[index + 1]
            if profile in ("fast", "release", "debug"):
                return f"target-{profile}"
    return "external"


def parse_method(raw: str) -> dict[str, str]:
    """Split a ``--method`` value into the class the probe is handed and a label.

    Only the fields this harness reports are kept; the descriptor, when given,
    stays in ``method`` so two overloads never collapse into one row, and in
    ``label`` so a report is readable without the descriptor.
    """
    value = raw.strip()
    if not value or not METHOD_PATTERN.match(value):
        raise ValueError(
            f"invalid method {raw!r}: expected CLASS[#NAME] (for example demo.Main#onInitialize)"
        )
    owner, _, named = value.partition("#")
    if not named:
        return {"class": owner, "method": None, "label": owner}
    name, _, descriptor = named.partition("(")
    descriptor = f"({descriptor}" if descriptor else ""
    return {"class": owner, "method": name, "label": f"{owner}#{name}{descriptor}"}


def method_cache_labels(methods: Sequence[dict[str, str]]) -> list[dict[str, str]]:
    """Label each method's loader state for this process sequence.

    The probe is a fresh process per sample, so nothing survives between runs.
    The first method to run still pays for warming the host's file cache, so it
    is ``cold``; every method after it finds that cache populated and is
    ``warm``. This is a statement about the harness's run order, not a claim
    about the probe's internal caches.
    """
    return [
        {"label": entry["label"], "cache": CACHE_COLD if index == 0 else CACHE_WARM}
        for index, entry in enumerate(methods)
    ]


def run_probe(
    command: list[str],
    jar: Path,
    kind: str | None,
    timeout: float,
    class_name: str | None = None,
) -> tuple[float, dict, str]:
    """Run the probe once and return ``(elapsed_ms, payload, outcome)``.

    ``class_name`` selects the explicit-entrypoint form the CLI already takes
    (``<jar> <kind> <class>``); it never changes the ``--json`` payload shape.
    """
    args = [*command, "--json", str(jar)]
    if class_name:
        # The CLI reads positionals in order: `<jar> [kind] [class]`. The kind
        # slot must exist for the class to land in the right place, and `main`
        # is the probe's own default, so filling it changes nothing.
        args.extend([kind or "main", class_name])
    elif kind:
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


def stash_jar(jar: Path, directory: Path) -> Path:
    """Copy ``jar`` under a path with no spaces or separators.

    Benchmark callers pick the cache label they want per method, which means the
    first measured run of a method has to be as cold as the first measured run
    of the run (a ``--method`` list warmed by a ``--runs`` pass would otherwise
    silently report warm numbers as cold). A private copy under a short, clean
    path makes that true on filesystems whose cache is keyed by path.
    """
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / jar.name
    shutil.copyfile(jar, target)
    return target


def benchmark_method(
    command: Sequence[str],
    jar: Path,
    kind: str | None,
    method: dict[str, str],
    cache: str,
    warmup: int,
    runs: int,
    timeout: float,
    runner: Callable[..., tuple[float, dict, str]] = run_probe,
) -> dict:
    """Time one method: ``warmup`` discarded runs, then ``runs`` measured ones.

    A failing run still produces a sample (its wall clock is real), so the
    ``outcomes`` column is what tells a reader whether to trust the numbers.
    """
    warmup_samples: list[float] = []
    samples: list[float] = []
    outcomes: list[str] = []
    for index in range(warmup + runs):
        elapsed, _report, outcome = runner(list(command), jar, kind, timeout, method["class"])
        outcomes.append(outcome)
        if index < warmup:
            warmup_samples.append(elapsed)
        else:
            samples.append(elapsed)
    return {
        "label": method["label"],
        "class": method["class"],
        "method": method["method"],
        "cache": cache,
        "warmup": {
            "n": len(warmup_samples),
            "mean_ms": round(statistics.fmean(warmup_samples), 3) if warmup_samples else None,
        },
        "summary": summarize(samples),
        "outcomes": sorted(set(outcomes)),
    }


def run_benchmark(
    command: Sequence[str],
    jar: Path,
    kind: str | None,
    methods: Sequence[dict[str, str]],
    warmup: int,
    runs: int,
    timeout: float,
    backend: str = "external",
    runner: Callable[..., tuple[float, dict, str]] = run_probe,
    stash: bool = True,
) -> dict:
    """Benchmark every method and return the report body (no schema fields)."""
    labels = method_cache_labels(methods)
    warm_jar = jar
    if stash:
        warm_jar = stash_jar(jar, ROOT / ".local" / "native-java-perf")
    rows = []
    for entry, label in zip(methods, labels):
        rows.append(
            benchmark_method(command, warm_jar, kind, entry, label["cache"], warmup, runs, timeout, runner)
        )
    return {
        "backend": backend,
        "warmup_runs": warmup,
        "measured_runs": runs,
        "methods": rows,
    }


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--jar", required=True, type=Path)
    parser.add_argument("--binary")
    parser.add_argument("--kind")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--method",
        action="append",
        default=[],
        metavar="CLASS[#NAME]",
        help=f"benchmark this class or method instead of a whole-jar timing (repeatable, at most {MAX_METHODS})",
    )
    parser.add_argument(
        "--warmup",
        type=int,
        default=DEFAULT_WARMUP,
        metavar="N",
        help=f"runs per method discarded before measuring, 0 to {MAX_WARMUP} (default {DEFAULT_WARMUP})",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if not args.jar.is_file():
        parser.error(f"jar does not exist: {args.jar}")
    if not 1 <= args.runs <= MAX_RUNS:
        parser.error(f"--runs must be between 1 and {MAX_RUNS}")
    if not math.isfinite(args.timeout) or args.timeout <= 0 or args.timeout > 300:
        parser.error("--timeout must be a finite value between 0 and 300 seconds")
    if args.method and not 1 <= len(args.method) <= MAX_METHODS:
        parser.error(f"--method may be repeated at most {MAX_METHODS} times")
    if not 0 <= args.warmup <= MAX_WARMUP:
        parser.error(f"--warmup must be between 0 and {MAX_WARMUP}")
    if args.method and args.warmup + args.runs > MAX_RUNS + MAX_WARMUP:
        parser.error(f"--warmup + --runs must stay within {MAX_RUNS + MAX_WARMUP} per method")
    methods = []
    for raw in args.method:
        try:
            methods.append(parse_method(raw))
        except ValueError as error:
            parser.error(str(error))
    command = binary_path(args.binary)
    if methods:
        body = run_benchmark(
            command, args.jar, args.kind, methods, args.warmup, args.runs, args.timeout,
            backend=backend_label(args.binary, command),
        )
        result = {
            "schema": 1,
            "tool": "horizon-native-java-perf",
            "mode": "benchmark",
            "jar": str(args.jar),
            **body,
        }
        ok = all(row["outcomes"] == ["ok"] for row in body["methods"])
    else:
        samples: list[float] = []
        reports: list[dict] = []
        for _ in range(args.runs):
            elapsed, report, outcome = run_probe(command, args.jar, args.kind, args.timeout)
            samples.append(elapsed)
            reports.append({"elapsed_ms": round(elapsed, 3), "outcome": outcome, "probe": report})
        result = {
            "schema": 1,
            "tool": "horizon-native-java-perf",
            "mode": "runs",
            "jar": str(args.jar),
            "runs": args.runs,
            "backend": backend_label(args.binary, command),
            "summary": summarize(samples),
            "samples": reports,
        }
        ok = all(row["outcome"] == "ok" for row in reports)
    encoded = json.dumps(result, sort_keys=True, indent=2)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded + "\n", encoding="utf-8")
    print(encoded)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
