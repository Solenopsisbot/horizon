#!/usr/bin/env python3
"""Compile and execute tiny Fabric and NeoForge native probe jars.

This is intentionally independent of ``.local`` and a Minecraft installation.  It
checks the complete native probe path with a real classfile: javac emits the
bytecode, jar packages it with Fabric metadata, and horizon-native-java discovers
and executes both lifecycle entries, plus a constructor-discovered NeoForge jar.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


SOURCES = {
    "demo/First.java": """
package demo;

public final class First {
    public void onInitialize() {
        int value = plus(19, 23);
        while (value < 42) {
            value++;
        }
    }

    private static int plus(int left, int right) {
        return left + right;
    }
}
""",
    "demo/Second.java": """
package demo;

public final class Second {
    public void onInitialize() {
        int remaining = 3;
        while (remaining > 0) {
            remaining--;
        }
    }
}
""",
}

NEO_SOURCES = {
    "net/neoforged/fml/common/Mod.java": """
package net.neoforged.fml.common;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.TYPE)
public @interface Mod {
    String value();
}
""",
    "demo/Neo.java": """
package demo;

@net.neoforged.fml.common.Mod("native-smoke-neo")
public final class Neo {
    public Neo() {
        int remaining = 2;
        while (remaining > 0) {
            remaining--;
        }
    }
}
""",
}


def run(command: list[str], *, cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)


def main() -> int:
    missing = [tool for tool in ("javac", "jar", "cargo") if shutil.which(tool) is None]
    if missing:
        print(f"native Java smoke skipped: missing {', '.join(missing)}", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="horizon-native-java-smoke-") as scratch:
        root = Path(scratch)
        source_root = root / "src"
        classes = root / "classes"
        metadata = root / "metadata"
        source_root.mkdir()
        classes.mkdir()
        metadata.mkdir()
        for relative, source in {**SOURCES, **NEO_SOURCES}.items():
            path = source_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source.lstrip(), encoding="utf-8")
        (metadata / "fabric.mod.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": "native-smoke",
                    "version": "1.0.0",
                    "depends": {"minecraft": "26.3", "fabricloader": "*"},
                    "entrypoints": {"main": ["demo.First", "demo.Second"]},
                }
            ),
            encoding="utf-8",
        )

        compile_result = run(
            [
                "javac",
                "--release",
                "17",
                "-d",
                str(classes),
                *(str(source_root / relative) for relative in {**SOURCES, **NEO_SOURCES}),
            ]
        )
        if compile_result.returncode:
            print(compile_result.stderr, file=sys.stderr, end="")
            return compile_result.returncode

        fabric_metadata = metadata / "fabric.mod.json"
        fabric_metadata.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": "native-smoke",
                    "version": "1.0.0",
                    "depends": {"minecraft": "26.3", "fabricloader": "*"},
                    "entrypoints": {"main": ["demo.First", "demo.Second"]},
                }
            ),
            encoding="utf-8",
        )
        # The NeoForge marker is compiled as an ordinary annotation class, so
        # this second jar also exercises annotation discovery and constructor
        # entrypoint execution without shipping a loader dependency.
        neo_metadata = metadata / "neoforge.mods.toml"
        neo_metadata.write_text(
            '[[mods]]\nmodId = "native-smoke-neo"\nversion = "1.0.0"\n',
            encoding="utf-8",
        )

        fabric_jar = root / "native-smoke-fabric.jar"
        package_result = run(
            [
                "jar",
                "--create",
                "--file",
                str(fabric_jar),
                "-C",
                str(classes),
                ".",
                "-C",
                str(metadata),
                "fabric.mod.json",
            ]
        )
        if package_result.returncode:
            print(package_result.stderr, file=sys.stderr, end="")
            return package_result.returncode

        neo_jar = root / "native-smoke-neoforge.jar"
        package_result = run(
            [
                "jar",
                "--create",
                "--file",
                str(neo_jar),
                "-C",
                str(classes),
                ".",
                "-C",
                str(metadata),
                "neoforge.mods.toml",
            ]
        )
        if package_result.returncode:
            print(package_result.stderr, file=sys.stderr, end="")
            return package_result.returncode

        probe = run(
            [
                "cargo",
                "run",
                "--quiet",
                "-p",
                "horizon-native-java",
                "--",
                "--json",
                str(fabric_jar),
            ]
        )
        if probe.returncode:
            print(probe.stderr, file=sys.stderr, end="")
            print(probe.stdout, file=sys.stderr, end="")
            return probe.returncode
        try:
            report = json.loads(probe.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError) as error:
            print(f"native probe returned invalid JSON: {error}\n{probe.stdout}", file=sys.stderr)
            return 1
        expected = ["demo.First", "demo.Second"]
        if report.get("status") != "ok" or report.get("kind") != "main":
            print(f"native probe failed: {json.dumps(report, sort_keys=True)}", file=sys.stderr)
            return 1
        if report.get("entrypoints") != expected:
            print(
                "native probe discovered the wrong entrypoints: "
                f"expected {expected!r}, got {report.get('entrypoints')!r}",
                file=sys.stderr,
            )
            return 1
        if not isinstance(report.get("host_calls"), int):
            print(f"native probe omitted host call count: {report!r}", file=sys.stderr)
            return 1
        neo_probe = run(
            [
                "cargo",
                "run",
                "--quiet",
                "-p",
                "horizon-native-java",
                "--",
                "--json",
                str(neo_jar),
                "neoforge",
            ]
        )
        if neo_probe.returncode:
            print(neo_probe.stderr, file=sys.stderr, end="")
            print(neo_probe.stdout, file=sys.stderr, end="")
            return neo_probe.returncode
        try:
            neo_report = json.loads(neo_probe.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError) as error:
            print(f"native NeoForge probe returned invalid JSON: {error}\n{neo_probe.stdout}", file=sys.stderr)
            return 1
        if (
            neo_report.get("status") != "ok"
            or neo_report.get("kind") != "neoforge"
            or neo_report.get("entrypoints") != ["demo.Neo"]
            or not isinstance(neo_report.get("host_calls"), int)
        ):
            print(f"native NeoForge probe failed: {json.dumps(neo_report, sort_keys=True)}", file=sys.stderr)
            return 1
        print(json.dumps({"fabric": report, "neoforge": neo_report}, sort_keys=True))
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
