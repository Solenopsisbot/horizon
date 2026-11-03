#!/usr/bin/env python3
"""Compare horizon-native-java with horizon-jvm on one pure-Java fixture.

Builds the fixture with javac, runs both runners, and checks success,
entrypoint, exit status and the deterministic printed value.  Prints
``differential: ok`` or a precise diff; exits non-zero only on disagreement.
Missing javac/jar/cargo or an unusable JVM is a clean skip (exit 0).

    python3 tools/native_java_differential.py
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ENTRYPOINT = "demo.Differential"
VALUE = "differential=42"
TIMEOUT = 1800
SOURCE = """\
package demo;

public final class Differential {
    public void onInitialize() {
        // Integer.parseInt is a modelled host call, so native host_calls
        // proves the body ran even though stdout is not captured there.
        int value = add(19, Integer.parseInt("23"));
        System.out.println("differential=" + value);
    }

    private static int add(int left, int right) {
        return left + right;
    }
}
"""
METADATA = {"schemaVersion": 1, "id": "native-differential", "version": "1.0.0",
            "depends": {"minecraft": "26.3", "fabricloader": "*"},
            "entrypoints": {"main": [ENTRYPOINT]}}
JVM_SKIP = ("could not start a JVM", "set JAVA_HOME to a JDK", "libjvm")
JAVAC_SKIP = ("invalid flag: --release", "release version 17 not supported",
              "invalid source release", "release 17 is not supported")


class Skip(Exception): pass  # environment cannot run the comparison


class Failure(Exception): pass  # fixture or runner failed; exit non-zero


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compare horizon-native-java with horizon-jvm on a pure-Java fixture.",
        epilog="Exit 0 on agreement or skip, 1 on disagreement or fixture failure.",
    )
    return parser.parse_args(argv)


def run(command: list[str], timeout: int) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(command, cwd=ROOT, text=True, capture_output=True,
                              timeout=timeout)
    except FileNotFoundError as error:
        raise Skip(f"cannot run {command[0]}: {error.strerror}") from None
    except subprocess.TimeoutExpired:
        raise Failure(f"{command[0]} timed out after {timeout}s") from None


def head(result: subprocess.CompletedProcess[str]) -> str:
    for line in (result.stdout + result.stderr).splitlines():
        if line.strip():
            return line.strip()
    return "no output"


def tail(result: subprocess.CompletedProcess[str], lines: int = 12) -> list[str]:
    rows = [line for line in (result.stdout + result.stderr).splitlines() if line.strip()]
    return rows[-lines:]


def build(root: Path, timeout: int) -> Path:
    source = root / "demo" / "Differential.java"
    source.parent.mkdir(parents=True)
    source.write_text(SOURCE, encoding="utf-8")
    classes = root / "classes"
    classes.mkdir()
    javac = run(["javac", "--release", "17", "-d", str(classes), str(source)], timeout)
    if javac.returncode:
        kind = Skip if any(marker in javac.stdout + javac.stderr for marker in JAVAC_SKIP) else Failure
        raise kind(f"javac failed ({javac.returncode}): {head(javac)}")
    metadata = root / "fabric.mod.json"
    metadata.write_text(json.dumps(METADATA), encoding="utf-8")
    archive = root / "native-differential.jar"
    packaged = run(["jar", "--create", "--file", str(archive),
                    "-C", str(classes), "demo/Differential.class",
                    "-C", str(root), "fabric.mod.json"], timeout)
    if packaged.returncode:
        raise Failure(f"jar failed ({packaged.returncode}): {head(packaged)}")
    return archive


def last_json(stdout: str) -> dict | None:
    for line in reversed(stdout.splitlines()):
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


def compare(native, report, listed, entries, jvm) -> list[str]:
    report = report or {}
    got = report.get("entrypoints")
    host = report.get("host_calls")
    checks = [
        (native.returncode == 0,
         f"native exit {native.returncode} (expected 0): {head(native)}"),
        (report.get("status") == "ok",
         f"native status {report.get('status')!r} (expected 'ok'): "
         f"{report.get('stage', '?')}: {report.get('error', 'no error')}"),
        (report.get("kind") == "main", f"native kind {report.get('kind')!r} (expected 'main')"),
        (got == [ENTRYPOINT], f"native entrypoints {got!r} (expected {[ENTRYPOINT]!r})"),
        (isinstance(host, int) and host >= 1,
         f"native host_calls {host!r} (expected at least 1)"),
        (listed.returncode == 0,
         f"jvm entrypoint listing exit {listed.returncode}: {head(listed)}"),
        (entries == [ENTRYPOINT], f"jvm entrypoints {entries!r} (expected {[ENTRYPOINT]!r})"),
        (jvm.returncode == 0, f"jvm exit {jvm.returncode} (expected 0): {head(jvm)}"),
        ("onInitialize returned" in jvm.stdout, "jvm did not report 'onInitialize returned'"),
        (VALUE in jvm.stdout, f"jvm did not print {VALUE!r} (deterministic value)"),
        ("no main entrypoint" not in jvm.stdout + jvm.stderr, "jvm found no main entrypoint"),
    ]
    return [message for ok, message in checks if not ok]


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    missing = [tool for tool in ("javac", "jar", "cargo") if shutil.which(tool) is None]
    if missing:
        print(f"differential: skipped: missing {', '.join(missing)}")
        return 0
    root = Path(tempfile.mkdtemp(prefix="horizon-native-differential-"))
    try:
        try:
            archive = build(root, TIMEOUT)
            native = run(["cargo", "run", "-q", "-p", "horizon-native-java", "--",
                          "--json", str(archive), "main"], TIMEOUT)
            native_report = last_json(native.stdout)
            listed = run(["cargo", "run", "-q", "-p", "horizon-jvm", "--",
                          "--list-entrypoints", str(archive)], TIMEOUT)
            entries = [line.split("\t", 1)[1].strip() for line in listed.stdout.splitlines()
                       if line.startswith("main\t")]
            jvm = run(["cargo", "run", "-q", "-p", "horizon-jvm", "--", str(archive)],
                      TIMEOUT)
            if jvm.returncode != 0 and any(
                    marker in jvm.stdout + jvm.stderr for marker in JVM_SKIP):
                raise Skip(f"horizon-jvm cannot start a JVM here: {head(jvm)}")
        except Skip as error:
            print(f"differential: skipped: {error}")
            return 0
        except Failure as error:
            print(f"differential: error: {error}", file=sys.stderr)
            return 1
        mismatches = compare(native, native_report, listed, entries, jvm)
        view = native_report or {}
        native_view = {
            "exit": native.returncode, "status": view.get("status"),
            "kind": view.get("kind"), "entrypoints": view.get("entrypoints"),
            "host_calls": view.get("host_calls"),
        }
        if mismatches:
            print(f"differential: mismatch\n"
                  f"  native: {json.dumps(native_view, sort_keys=True)}\n"
                  f"  jvm: exit={jvm.returncode} entrypoints={entries!r} "
                  f"value={VALUE if VALUE in jvm.stdout else None!r}")
            for message in mismatches:
                print(f"  - {message}")
            for label, result in (("native", native), ("jvm", jvm)):
                rows = tail(result)
                if rows:
                    print(f"  {label} output:\n" + "\n".join(f"    {row}" for row in rows))
        else:
            print(f"differential: native ok (entrypoint {ENTRYPOINT}, "
                  f"{native_view['host_calls']} host calls, exit 0)\n"
                  f"differential: jvm ok (entrypoint {ENTRYPOINT}, printed {VALUE}, exit 0)\n"
                  "differential: ok")
        return 0 if not mismatches else 1
    finally:
        shutil.rmtree(root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
