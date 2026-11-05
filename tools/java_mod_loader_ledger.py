#!/usr/bin/env python3
"""Run Fabric's real server loader against a mod corpus and write a ledger.

The runner intentionally uses one fresh process and home per bounded batch. It
never copies or modifies jars; the only output is the deterministic ledger.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import tempfile
import zipfile
from pathlib import Path

DEFAULT_MODS = Path("/tmp/opencode/mods/Better Realism - MC 26.3 - 0/mods")
DEFAULT_GAME = Path("/tmp/opencode/server-26.3-pinned.jar")
DEFAULT_LOADER = Path("/tmp/opencode/fabric-loader-0.19.5.jar")
DEFAULT_LIBS = Path("/tmp/opencode/knotrun/libraries")


def metadata(path: Path) -> tuple[str, str]:
    try:
        with zipfile.ZipFile(path) as jar:
            raw = jar.read("fabric.mod.json")
        value = json.loads(raw)
        return str(value.get("id", path.stem)), str(value.get("version", "?"))
    except (OSError, KeyError, ValueError, json.JSONDecodeError):
        return path.stem, "?"


def discover(explicit: Path | None, fallback: Path, pattern: str) -> Path:
    if explicit:
        return explicit
    if fallback.exists():
        return fallback
    roots = [Path("/tmp/opencode")]
    found = sorted(p for root in roots for p in root.rglob(pattern) if p.is_file() or p.is_dir())
    if not found:
        raise SystemExit(f"could not discover {pattern}; pass the corresponding option")
    return found[0]


def run_batch(binary: Path, game: Path, loader: Path, libs: Path, jars: list[Path], timeout: int) -> tuple[int, str]:
    with tempfile.TemporaryDirectory(prefix="horizon-loader-") as scratch:
        env = os.environ.copy()
        for name in ("HORIZON_HOME", "HORIZON_LAUNCHER_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME"):
            env[name] = str(Path(scratch) / name.lower())
        env.update({
            "HORIZON_REAL_LAUNCH": "1",
            "HORIZON_NO_AUDIO": "1",
            "HORIZON_MINECRAFT_JAR": str(game),
            "FABRIC_LOADER_JAR": str(loader),
            "FABRIC_LIBS_DIR": str(libs),
        })
        command = [str(binary), "--real-loader", "--entry-kind", "server",
                   "--launch-game", "--accept-eula", "--loader", str(loader),
                   "--libs", str(libs), "--game-jar", str(game)] + [str(j) for j in jars]
        process = subprocess.Popen(command, cwd=scratch, env=env, start_new_session=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        try:
            output, _ = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            output, _ = process.communicate()
            output += f"\nledger: timeout after {timeout}s\n"
            return 124, output
        return process.returncode, output


def parse_log(output: str, mods: list[tuple[Path, str, str]]) -> dict:
    lower = output.lower()
    listed = set()
    for line in output.splitlines():
        listed_match = re.match(r"^\s*-\s+([a-z0-9][a-z0-9_-]{1,63})(?:\s+|$)", line, re.I)
        if listed_match:
            listed.add(listed_match.group(1).lower())
    mixin_lines = [line.strip() for line in output.splitlines() if "mixin" in line.lower() and ("apply" in line.lower() or "error" in line.lower() or "fail" in line.lower())]
    crashes = [line.strip() for line in output.splitlines() if re.search(r"exception|crash|fatal|error", line, re.I)]
    rows = []
    for path, mod_id, version in sorted(mods, key=lambda item: (item[1], str(item[0]))):
        mentions = [line.strip() for line in output.splitlines() if mod_id.lower() in line.lower() and re.search(r"exception|crash|fatal|error", line, re.I)]
        error = mentions[:1]
        present = mod_id.lower() in listed or re.search(rf"\b{re.escape(mod_id)}\b", lower) is not None
        status = "error" if error else ("listed" if present else "unlisted")
        rows.append({"id": mod_id, "version": version, "jar": path.name, "status": status,
                     "first_exception": error[0] if error else ""})
    return {"rows": rows, "mods_found": len(rows), "mods_listed": sum(r["status"] == "listed" for r in rows),
            "mixin_lines": mixin_lines[:20], "crash_lines": crashes[:20],
            "done": "Done (" in output, "exit": None}


def render(result: dict) -> str:
    lines = ["# Horizon real Fabric loader ledger", "# Deterministic rows sorted by mod id", ""]
    for row in result["rows"]:
        suffix = f"\tfirst_exception={row['first_exception']}" if row["first_exception"] else ""
        lines.append(f"mod={row['id']}\tversion={row['version']}\tstatus={row['status']}\tjar={row['jar']}{suffix}")
    lines += ["", f"summary\tmods_found={result['mods_found']}\tmods_listed={result['mods_listed']}\tdone={str(result['done']).lower()}"]
    if result.get("mixin_lines"):
        lines.append("mixin_lines\t" + " | ".join(result["mixin_lines"]))
    if result.get("crash_lines"):
        lines.append("crash_lines\t" + " | ".join(result["crash_lines"]))
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mods-dir", type=Path, default=DEFAULT_MODS)
    parser.add_argument("--mod", action="append", type=Path, dest="extra_mods", help="additional mod jar (repeatable)")
    parser.add_argument("--binary", type=Path, default=Path("target/fast/horizon-jvm"))
    parser.add_argument("--game", type=Path)
    parser.add_argument("--loader", type=Path)
    parser.add_argument("--libs", type=Path)
    parser.add_argument("--batch-size", type=int, default=12)
    parser.add_argument("--timeout", type=int, default=90)
    parser.add_argument("--output", type=Path, default=Path("tools/real_mod_fixtures/LAST_LOADER_LEDGER.txt"))
    parser.add_argument("--json", action="store_true", help="emit JSON instead of the text ledger")
    args = parser.parse_args()
    if args.batch_size < 1 or args.timeout < 1:
        parser.error("--batch-size and --timeout must be positive")
    binary = args.binary if args.binary.is_absolute() else Path.cwd() / args.binary
    game = discover(args.game, DEFAULT_GAME, "server-26.3-pinned.jar")
    loader = discover(args.loader, DEFAULT_LOADER, "fabric-loader*.jar")
    if args.libs:
        libs = args.libs
    else:
        candidates = [DEFAULT_LIBS] + sorted(Path("/tmp/opencode").glob("*/libraries"))
        libs = next((candidate for candidate in candidates
                     if candidate.is_dir() and any(candidate.rglob("asm-*.jar"))), None)
        if libs is None:
            libs = discover(None, DEFAULT_LIBS, "libraries")
    paths = sorted(args.mods_dir.glob("*.jar")) + sorted(args.extra_mods or [])
    if not paths:
        parser.error(f"no mod jars found in {args.mods_dir}")
    mods = [(path, *metadata(path)) for path in paths]
    aggregate = {"rows": [], "mods_found": 0, "mods_listed": 0, "mixin_lines": [], "crash_lines": [], "done": False}
    for start in range(0, len(paths), args.batch_size):
        batch_paths = paths[start:start + args.batch_size]
        code, output = run_batch(binary, game, loader, libs, batch_paths, args.timeout)
        parsed = parse_log(output, [item for item in mods if item[0] in batch_paths])
        aggregate["rows"].extend(parsed["rows"])
        aggregate["mods_found"] += parsed["mods_found"]
        aggregate["mods_listed"] += parsed["mods_listed"]
        aggregate["mixin_lines"].extend(parsed["mixin_lines"])
        aggregate["crash_lines"].extend(parsed["crash_lines"])
        aggregate["done"] = aggregate["done"] or parsed["done"]
        print(f"batch {start // args.batch_size + 1}: {len(batch_paths)} mods, exit={code}, listed={parsed['mods_listed']}", flush=True)
    aggregate["rows"].sort(key=lambda row: (row["id"], row["jar"]))
    text = render(aggregate)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text, encoding="utf-8")
    if args.json:
        print(json.dumps(aggregate, sort_keys=True, indent=2))
    else:
        print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
