#!/usr/bin/env python3
"""Run a real Fabric mod through Fabric Loader and `horizon-jvm --real-loader`.

The probe compiles a tiny `ModInitializer` jar at test time, then asks
`horizon-jvm` to start Fabric Loader, let it discover the jar through
`fabric.addMods`, and invoke the loader's own entrypoint containers. It is
independent of `.local` and a Minecraft install: the loader jar and its
libraries are supplied by the caller.

Skip cleanly (exit 0) when no Fabric Loader jar is available.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# A loader jar the caller can point at, plus common caches on this machine.
CANDIDATES = [
    *(
        [Path(os.environ["FABRIC_LOADER_JAR"])]
        if os.environ.get("FABRIC_LOADER_JAR")
        else []
    ),
    Path.home()
    / "solexsis-mc/data/logfare/server/libraries/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar",
    Path.home()
    / ".gradle/caches/modules-2/files-2.1/net.fabricmc/fabric-loader/0.20.1/7c7f9831e9f27b303d5d08decb5f4258a9f03296/fabric-loader-0.20.1.jar",
]

MOD_SOURCE = """
package demo;
import net.fabricmc.api.ModInitializer;
public final class Real implements ModInitializer {
  public void onInitialize() { System.out.println("real-loader-ok:" + getClass().getName()); }
}
"""

METADATA = """{
  "schemaVersion": 1,
  "id": "real-loader-fixture",
  "version": "1.0",
  "entrypoints": { "main": ["demo.Real"] }
}
"""


def run(command: list[str], *, cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)


def find_loader() -> Path | None:
    for candidate in CANDIDATES:
        if candidate.is_file():
            return candidate
    return None


def find_libraries(loader: Path) -> Path | None:
    if os.environ.get("FABRIC_LIBS_DIR"):
        directory = Path(os.environ["FABRIC_LIBS_DIR"])
        return directory if directory.is_dir() else None
    # `<libraries>/net/fabricmc/fabric-loader/<version>/loader.jar`
    for parent in loader.resolve().parents:
        if parent.name == "libraries" and parent.is_dir():
            return parent
    return None


def build_mod(root: Path, loader: Path) -> Path:
    src = root / "src" / "demo"
    classes = root / "classes"
    src.mkdir(parents=True)
    classes.mkdir()
    (src / "Real.java").write_text(MOD_SOURCE)
    javac = (
        Path(os.environ["JAVA_HOME"]) / "bin" / "javac"
        if os.environ.get("JAVA_HOME")
        else Path("javac")
    )
    compiled = run(
        [
            str(javac),
            "--release",
            "17",
            "-cp",
            str(loader),
            "-d",
            str(classes),
            str(src / "Real.java"),
        ]
    )
    if compiled.returncode != 0:
        raise SystemExit(f"javac failed:\n{compiled.stderr}{compiled.stdout}")
    metadata = root / "fabric.mod.json"
    metadata.write_text(METADATA)
    archive = root / "real-loader.jar"
    packaged = run(
        [
            "jar",
            "cf",
            str(archive),
            "-C",
            str(classes),
            ".",
            "-C",
            str(root),
            "fabric.mod.json",
        ]
    )
    if packaged.returncode != 0:
        raise SystemExit(f"jar failed:\n{packaged.stderr}{packaged.stdout}")
    return archive


def main() -> int:
    missing = [tool for tool in ("cargo", "javac", "jar") if shutil.which(tool) is None]
    if missing:
        print(f"fabric loader probe skipped: missing {', '.join(missing)}", file=sys.stderr)
        return 0
    loader = find_loader()
    if loader is None:
        print(
            "fabric loader probe skipped: set FABRIC_LOADER_JAR to a Fabric Loader jar",
            file=sys.stderr,
        )
        return 0
    libraries = find_libraries(loader)
    if libraries is None:
        print(
            "fabric loader probe skipped: set FABRIC_LIBS_DIR to the loader's libraries",
            file=sys.stderr,
        )
        return 0
    with tempfile.TemporaryDirectory(prefix="horizon-fabric-loader-") as scratch:
        root = Path(scratch)
        mod = build_mod(root, loader)
        command = [
            "cargo",
            "run",
            "--quiet",
            "-p",
            "horizon-jvm",
            "--",
            "--real-loader",
            "--entry-kind",
            "main",
            "--loader",
            str(loader),
            "--libs",
            str(libraries),
            str(mod),
        ]
        result = run(command)
        output = f"{result.stderr}{result.stdout}"
        if result.returncode != 0:
            print(f"fabric loader probe failed ({result.returncode}):\n{output}", file=sys.stderr)
            return 1
        needed = ["real-loader-ok:demo.Real", "real loader:", "1 main entrypoint(s)"]
        missing = [text for text in needed if text not in output]
        if missing:
            print(f"fabric loader probe missing {missing!r}:\n{output}", file=sys.stderr)
            return 1
        print("fabric loader: ok")
        for line in output.splitlines():
            if "real-loader-ok" in line or "real loader:" in line:
                print(f"  {line}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
