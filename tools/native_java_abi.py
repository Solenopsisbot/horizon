#!/usr/bin/env python3
"""Rank the Java ABI surface a mod jar or folder asks Horizon for.

Wraps ``horizon-port-pack census``: classes, members, body calls, field refs,
Mixin plans, nested jars, body errors, and compatibility-package flags
(``net/minecraft``, ``net/fabricmc``, ``net/neoforged``, ``net/minecraftforge``).

    python3 tools/native_java_abi.py mods/ [--top N] [--json]
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# Longest prefixes first: net/minecraftforge must not be read as net/minecraft.
FAMILIES = {"net/minecraftforge": "forge", "net/minecraft": "minecraft",
            "net/fabricmc": "fabric", "net/neoforged": "neoforge"}
DEFAULT_TOP = 15
TIMEOUT = 900


class ToolError(Exception): pass


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Rank Java mod ABI references from horizon-port-pack census.",
        epilog=(
            "Compatibility-relevant calls have an owner starting with "
            + ", ".join(FAMILIES)
            + ". Text output is bounded by --top."
        ),
    )
    parser.add_argument("inputs", nargs="+", metavar="DIR-OR-JAR",
                        help="a .jar, or a folder searched recursively for .jar files")
    parser.add_argument("--top", type=int, default=DEFAULT_TOP, metavar="N",
                        help=f"rows per ranking, at least 1 (default {DEFAULT_TOP})")
    parser.add_argument("--json", action="store_true", help="print a JSON summary")
    return parser.parse_args(argv)


def select_calls(inputs: list[str]) -> list[Path]:
    """Distinct census targets; a folder covers the jars inside it."""
    resolved: list[Path] = []
    for raw in inputs:
        path = Path(raw).expanduser()
        if not path.exists():
            raise ToolError(f"{raw}: no such file or directory")
        path = path.resolve()
        if not path.is_dir() and path.suffix.lower() != ".jar":
            raise ToolError(f"{raw}: not a .jar file")
        resolved.append(path)
    dirs = sorted({p for p in resolved if p.is_dir()}, key=str)
    outer = [d for d in dirs if not any(d != o and d.is_relative_to(o) for o in dirs)]
    for directory in outer:
        if not any(directory.rglob("*.jar")):
            raise ToolError(f"{directory}: no .jar files found")
    files = sorted(
        {p for p in resolved if p.is_file() and not any(p.is_relative_to(d) for d in outer)},
        key=str,
    )
    calls = outer + files
    if not calls:
        raise ToolError("no jar found")
    return calls


def error_detail(result: subprocess.CompletedProcess[str]) -> str:
    lines = [line.strip() for line in (result.stderr or result.stdout).splitlines()
             if line.strip()]
    for line in reversed(lines):
        if line.startswith("error:"):
            return line
    return lines[-1] if lines else "no output"


def run_census(target: Path, output: Path, timeout: int) -> dict:
    command = ["cargo", "run", "-q", "-p", "horizon-port-pack", "--",
               "census", str(target), "--out", str(output)]
    try:
        result = subprocess.run(command, cwd=ROOT, text=True, capture_output=True,
                                timeout=timeout)
    except subprocess.TimeoutExpired:
        raise ToolError(f"census of {target} timed out after {timeout}s") from None
    if result.returncode:
        raise ToolError(f"census of {target} failed ({result.returncode}): "
                        f"{error_detail(result)}")
    try:
        return json.loads(output.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ToolError(f"census of {target} wrote no readable JSON: {error}") from None


def merge(left: dict, right: dict) -> dict:
    """Add independent censuses without double-counting anything."""
    for key, value in right.items():
        if key not in left:
            left[key] = value
        elif isinstance(value, int) and isinstance(left[key], int):
            left[key] += value
        elif isinstance(value, list) and isinstance(left[key], list):
            rows = left[key] + value
            if rows and isinstance(rows[0], dict):
                if all("count" in row for row in rows):
                    # Plans and points carry a count: merge equal rows by
                    # summing it instead of deduplicating one of them away.
                    grouped = {}
                    for row in rows:
                        identity = json.dumps({name: item for name, item in row.items()
                                               if name != "count"}, sort_keys=True)
                        if identity in grouped:
                            grouped[identity]["count"] += int(row.get("count", 0))
                        else:
                            grouped[identity] = dict(row)
                    rows = list(grouped.values())
                else:
                    unique = {json.dumps(row, sort_keys=True): row for row in rows}
                    rows = list(unique.values())
                rows.sort(key=lambda row: json.dumps(row, sort_keys=True))
            else:
                rows = sorted(set(rows))
            left[key] = rows
        elif isinstance(value, dict) and isinstance(left[key], dict):
            left[key] = merge(left[key], value)
    return left


def rank(mapping: dict, top: int) -> list[tuple[str, int]]:
    return sorted(((str(name), count) for name, count in mapping.items()),
                  key=lambda row: (-row[1], row[0]))[:top]


def family(member: str) -> str | None:
    owner = member.split("#", 1)[0]
    for prefix, name in FAMILIES.items():
        if owner.startswith(prefix):
            return name
    return None


def summarise(census: dict, calls: list[Path], top: int | None) -> dict:
    totals = census.get("totals", {})
    jars = census.get("jars", [])
    plans = census.get("mixin_translation_plans", [])
    calls_map = census.get("body_calls", {})
    fields_map = census.get("body_field_refs", {})
    compat = {member: count for member, count in calls_map.items() if family(member)}
    family_counts = Counter(family(member) for member in compat)
    statuses = Counter(plan.get("status", "unknown") for plan in plans)
    modes = Counter(plan.get("mode", "unknown") for plan in plans)
    plan_rows = sorted(plans, key=lambda p: (-int(p.get("count", 0)), p.get("target", ""),
                                             p.get("method", ""), p.get("kind", ""),
                                             p.get("at", "")))
    nested = sorted(({"outer": jar.get("file", ""), **row} for jar in jars
                     for row in jar.get("nested_jars", [])),
                    key=lambda row: (row["outer"], row.get("entry", "")))
    nested_errors = sorted(({"outer": jar.get("file", ""), "entry": entry, "reason": reason}
                            for jar in jars for entry, reason in jar.get("nested_jar_errors", [])),
                           key=lambda row: (row["outer"], row["entry"]))
    limit = (lambda rows: rows[:top])
    return {
        "tool": "native_java_abi",
        "inputs": [str(call) for call in calls],
        "top": top,
        "totals": totals,
        "body": {"calls": sum(calls_map.values()), "methods": len(calls_map),
                 "field_refs": sum(fields_map.values()), "fields": len(fields_map),
                 "errors": int(totals.get("body_errors", 0))},
        "rankings": {
            "classes": rank(census.get("classes", {}), top),
            "members": rank(census.get("members", {}), top),
            "body_calls": rank(calls_map, top),
            "body_field_refs": rank(fields_map, top),
        },
        "compatibility": {
            "prefixes": list(FAMILIES), "families": dict(sorted(family_counts.items())),
            "members": rank(compat, top), "member_count": len(compat),
            "site_count": sum(compat.values()),
        },
        "mixin_plans": {"total": len(plans), "by_status": dict(sorted(statuses.items())),
                        "by_mode": dict(sorted(modes.items())), "plans": limit(plan_rows)},
        "nested_jars": {"kinds": dict(sorted(totals.get("nested_jar_kinds", {}).items())),
                        "entries": limit(nested), "entry_count": len(nested),
                        "errors": nested_errors},
        "body_errors": {"total": int(totals.get("body_errors", 0)), "by_jar": [
            {"file": jar.get("file", ""), "errors": int(jar.get("body_errors", 0))}
            for jar in jars if int(jar.get("body_errors", 0))]},
        "metadata_errors": [{"file": jar.get("file", ""), "error": jar["error"]}
                            for jar in jars if jar.get("error")],
    }


def block(title: str, rows: list[tuple[str, int]]) -> str:
    return "\n".join([title] + ([f"    {count:7} {name}" for name, count in rows]
                                or ["    (none)"]))


def render(summary: dict) -> str:
    totals, body = summary["totals"], summary["body"]
    calls = [
        f"    {count:7} {name}" + (f"  [{family(name)}]" if family(name) else "")
        for name, count in summary["rankings"]["body_calls"]
    ] or ["    (none)"]
    plans = [
        f"    {int(p.get('count', 0)):7} {p.get('target', '')}#{p.get('method', '')} "
        f"{p.get('kind', '')} {p.get('at', '') or '-'} {p.get('status', '')}"
        for p in summary["mixin_plans"]["plans"]
    ] or ["    (none)"]
    nested = [
        f"    {row['outer']} {row.get('entry', '')} {row.get('kind', 'unknown')}"
        for row in summary["nested_jars"]["entries"]
    ] or ["    (none)"]
    compat = summary["compatibility"]
    compat_rows = "\n".join(
        f"    {count:7} {name}" for name, count in compat["members"]
    ) or "    (none)"
    families = ", ".join(f"{name}={count}" for name, count in compat["families"].items())
    kinds = ", ".join(f"{name}={count}" for name, count in summary["nested_jars"]["kinds"].items())
    out = [
        f"native Java ABI: {len(summary['inputs'])} input(s), {totals.get('jars', 0)} jars, "
        f"{totals.get('classes', 0)} classes",
        f"  {totals.get('references', 0)} class refs, {totals.get('members', 0)} member refs, "
        f"{totals.get('mixins', 0)} mixins, {totals.get('injections', 0)} injections",
        f"  body: {body['calls']} call sites at {body['methods']} methods, "
        f"{body['field_refs']} field access sites at {body['fields']} fields, "
        f"{body['errors']} body errors",
    ]
    out += [
        block("\ntop classes", summary["rankings"]["classes"]),
        block("\ntop members", summary["rankings"]["members"]),
        "\ntop body calls\n" + "\n".join(calls),
        block("\ntop body field refs", summary["rankings"]["body_field_refs"]),
        f"\ncompatibility-relevant body calls: {compat['member_count']} members, "
        f"{compat['site_count']} call sites" + (f"\n    families: {families}" if families else "")
        + "\n" + compat_rows,
        f"\nmixin translation plans: {summary['mixin_plans']['total']} "
        f"(status: {summary['mixin_plans']['by_status']}, "
        f"mode: {summary['mixin_plans']['by_mode']})\n" + "\n".join(plans),
        f"\nnested jars: {summary['nested_jars']['entry_count']}"
        + (f" ({kinds})" if kinds else "") + "\n" + "\n".join(nested),
        "\nbody errors per jar\n" + ("\n".join(
            f"    {row['errors']:7} {row['file']}" for row in summary["body_errors"]["by_jar"]
        ) or "    (none)"),
    ]
    if summary["nested_jars"]["errors"]:
        out.append("\nnested jar metadata errors\n" + "\n".join(
            f"    {row['outer']} {row['entry']}: {row['reason']}"
            for row in summary["nested_jars"]["errors"]))
    if summary["metadata_errors"]:
        out.append("\nunreadable jar metadata\n" + "\n".join(
            f"    {row['file']}: {row['error']}" for row in summary["metadata_errors"]))
    return "\n".join(line for line in out if line)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if shutil.which("cargo") is None:
        print("error: cargo not found on PATH", file=sys.stderr)
        return 2
    try:
        calls = select_calls(args.inputs)
    except ToolError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    top = max(1, args.top)
    try:
        with tempfile.TemporaryDirectory(prefix="horizon-native-java-abi-") as scratch:
            census = {}
            for index, call in enumerate(calls):
                merge(census, run_census(call, Path(scratch) / f"census-{index:04d}.json",
                                         TIMEOUT))
    except ToolError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    summary = summarise(census, calls, top)
    if args.json:
        print(json.dumps(summary, indent=2, sort_keys=True))
    else:
        print(render(summary))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
