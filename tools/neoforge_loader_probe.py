#!/usr/bin/env python3
"""Run a real NeoForge mod through FancyModLoader (FML).

This drives FML's own loader — mod discovery, `@Mod` construction with its
event bus, and Minecraft bootstrap — instead of Horizon's bounded scan. It
needs an installed NeoForge server directory (the output of
`neoforge-*-installer.jar --installServer`):

    NEOFORGE_SERVER_DIR=/path/to/server python3 tools/neoforge_loader_probe.py

The probe skips cleanly (exit 0) when that directory, a JDK, or `cargo` is
missing. It never writes into the installed server: the fixture gets its own
temporary game directory.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

PROBE_SOURCE = r'''
package probe;

import java.nio.file.*;
import java.util.*;
import net.neoforged.api.distmarker.Dist;
import net.neoforged.fml.ModLoader;
import net.neoforged.fml.loading.FMLLoader;
import net.neoforged.fml.loading.LoadingModList;
import net.neoforged.fml.startup.StartupArgs;

public final class FmlProbe {
  public static void main(String[] args) throws Exception {
    java.util.List<String> fml = new java.util.ArrayList<>();
    for (String name : new String[]{"mcVersion", "neoForgeVersion", "neoFormVersion"}) {
      String value = System.getProperty("horizon.fml." + name);
      if (value != null) { fml.add("--fml." + name); fml.add(value); }
    }
    StartupArgs startup = new StartupArgs(Paths.get(System.getProperty("user.dir")), true,
        Dist.DEDICATED_SERVER, true, fml.toArray(new String[0]), Set.of(), List.of(), ClassLoader.getSystemClassLoader());
    FMLLoader loader = FMLLoader.create(startup);
    LoadingModList mods = loader.getLoadingModList();
    System.out.println("mods=" + mods.getMods().size());
    for (var mod : mods.getMods()) System.out.println("mod=" + mod.getModId());
    ClassLoader game = loader.getGameLayer().findLoader("minecraft");
    Class<?> constants = Class.forName("net.minecraft.SharedConstants", true, game);
    constants.getMethod("tryDetectVersion").invoke(null);
    Class<?> bootstrap = Class.forName("net.minecraft.server.Bootstrap", true, game);
    bootstrap.getMethod("bootStrap").invoke(null);
    ModLoader.gatherAndInitializeMods(Runnable::run, Runnable::run, () -> {});
    System.out.println("initialized");
  }
}
'''

MOD_SOURCE = r'''
package demo;
import net.neoforged.bus.api.IEventBus;
@net.neoforged.fml.common.Mod("fmlfixture")
public final class RealMod {
  public RealMod(IEventBus bus) { System.out.println("fml-fixture-ok:" + getClass().getName()); }
}
'''

MOD_TOML = '''modLoader = "javafml"
loaderVersion = "[1,)"
license = "MIT"

[[mods]]
modId = "fmlfixture"
version = "1.0"
displayName = "FML Fixture"
authors = "Horizon"
description = "A real NeoForge fixture."
'''


def run(command: list[str], *, cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)


def java_tool(name: str) -> Path:
    return (
        Path(os.environ["JAVA_HOME"]) / "bin" / name
        if os.environ.get("JAVA_HOME")
        else Path(name)
    )


def classpath(server: Path) -> str | None:
    args = sorted(server.glob("libraries/net/neoforged/neoforge/*/unix_args.txt"))
    if not args:
        return None
    lines = args[-1].read_text().splitlines()
    try:
        entries = lines[lines.index("-classpath") + 1].split(os.pathsep)
    except (ValueError, IndexError):
        return None
    # The file is written relative to the server directory.
    return os.pathsep.join(
        str(server / entry) if not os.path.isabs(entry) else entry for entry in entries
    )


def fml_properties(server: Path) -> list[str]:
    args = sorted(server.glob("libraries/net/neoforged/neoforge/*/unix_args.txt"))
    if not args:
        return []
    lines = args[-1].read_text().splitlines()
    out = []
    for line in lines:
        key, _, value = line.partition(" ")
        if key.startswith("--fml.") and value:
            out.append("-Dhorizon." + key[2:] + "=" + value)
    return out


def main() -> int:
    server_dir = os.environ.get("NEOFORGE_SERVER_DIR")
    if not server_dir:
        print(
            "neoforge loader probe skipped: set NEOFORGE_SERVER_DIR to an installed NeoForge server",
            file=sys.stderr,
        )
        return 0
    server = Path(server_dir)
    cp = classpath(server)
    if cp is None:
        print(f"neoforge loader probe skipped: {server} is not an installed NeoForge server", file=sys.stderr)
        return 0
    missing = [tool for tool in ("javac", "jar") if shutil.which(tool) is None]
    if missing:
        print(f"neoforge loader probe skipped: missing {', '.join(missing)}", file=sys.stderr)
        return 0
    with tempfile.TemporaryDirectory(prefix="horizon-neoforge-loader-") as scratch:
        root = Path(scratch)
        probe_src = root / "probe" / "FmlProbe.java"
        mod_src = root / "mod" / "demo" / "RealMod.java"
        probe_src.parent.mkdir(parents=True)
        mod_src.parent.mkdir(parents=True)
        probe_src.write_text(PROBE_SOURCE)
        mod_src.write_text(MOD_SOURCE)
        classes = root / "classes"
        mod_classes = root / "modclasses"
        classes.mkdir()
        mod_classes.mkdir()
        compiled = run([str(java_tool("javac")), "-cp", cp, "-d", str(classes), str(probe_src)])
        if compiled.returncode != 0:
            print(f"probe javac failed:\n{compiled.stderr}{compiled.stdout}", file=sys.stderr)
            return 1
        compiled = run(
            [
                str(java_tool("javac")),
                "--release",
                "17",
                "-cp",
                cp,
                "-d",
                str(mod_classes),
                str(mod_src),
            ]
        )
        if compiled.returncode != 0:
            print(f"mod javac failed:\n{compiled.stderr}{compiled.stdout}", file=sys.stderr)
            return 1

        game = root / "game"
        mods = game / "mods"
        mods.mkdir(parents=True)
        jar = mods / "fml-fixture.jar"
        packaged = run(
            [
                str(java_tool("jar")),
                "cf",
                str(jar),
                "-C",
                str(mod_classes),
                ".",
            ]
        )
        if packaged.returncode != 0:
            print(f"jar failed:\n{packaged.stderr}{packaged.stdout}", file=sys.stderr)
            return 1
        with jar.open("ab") as archive:
            # Adding the toml after the classes keeps `jar cf` simple.
            pass
        # `jar` cannot add a file from a string, so write it and update.
        toml = root / "neoforge.mods.toml"
        toml.write_text(MOD_TOML)
        manifest_dir = root / "meta" / "META-INF"
        manifest_dir.mkdir(parents=True)
        shutil.copy(toml, manifest_dir / "neoforge.mods.toml")
        updated = run([str(java_tool("jar")), "uf", str(jar), "-C", str(root / "meta"), "META-INF"])
        if updated.returncode != 0:
            print(f"jar update failed:\n{updated.stderr}{updated.stdout}", file=sys.stderr)
            return 1

        command = [
            str(java_tool("java")),
            "--add-opens",
            "java.base/java.lang.invoke=ALL-UNNAMED",
            "-DlibraryDirectory=" + str(server / "libraries"),
            "-cp",
            f"{cp}:{classes}",
            *fml_properties(server),
            "probe.FmlProbe",
        ]
        result = run(command, cwd=game)
        output = f"{result.stderr}{result.stdout}"
        if result.returncode != 0:
            print(f"neoforge loader probe failed ({result.returncode}):\n{output}", file=sys.stderr)
            return 1
        needed = ["mod=fmlfixture", "fml-fixture-ok:demo.RealMod", "initialized"]
        missing = [text for text in needed if text not in output]
        if missing:
            print(f"neoforge loader probe missing {missing!r}:\n{output}", file=sys.stderr)
            return 1
        print("neoforge loader: ok")
        for line in output.splitlines():
            if line.startswith("mods=") or line.startswith("mod=") or "fml-fixture-ok" in line:
                print(f"  {line}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
