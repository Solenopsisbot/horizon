#!/usr/bin/env python3
"""Compile and execute tiny Fabric and NeoForge native probe jars.

This is intentionally independent of ``.local`` and a Minecraft installation.  It
checks the complete native probe path with real classfiles: javac emits the
bytecode, jar packages it with Fabric metadata, and horizon-native-java discovers
and executes both lifecycle entries, including an Optional.filter predicate callback,
plus a constructor-discovered NeoForge jar that registers a configuration payload.
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

import java.util.HashMap;
import java.util.Optional;
import java.util.OptionalDouble;
import java.util.OptionalLong;

public final class First {
    public void onInitialize() {
        int value = plus(19, 23);
        while (value < 42) {
            value++;
        }
        Optional<String> marker = Optional.ofNullable("native");
        if (!marker.filter(text -> text.length() == 6).isPresent()) {
            throw new IllegalStateException("Optional.filter host path failed");
        }
        OptionalLong longMarker = OptionalLong.of(42L);
        if (!longMarker.isPresent() || !longMarker.toString().equals("OptionalLong[42]")) {
            throw new IllegalStateException("OptionalLong host path failed");
        }
        OptionalDouble doubleMarker = OptionalDouble.of(3.5D);
        if (!doubleMarker.isPresent() || !doubleMarker.toString().equals("OptionalDouble[3.5]")) {
            throw new IllegalStateException("OptionalDouble host path failed");
        }
        HashMap<String, Integer> values = new HashMap<>();
        values.put("left", 19);
        values.put("right", 23);
        values.forEach((key, item) -> {
            if (item < 19 || item > 23) {
                throw new IllegalStateException("Map.forEach host path failed for " + key);
            }
        });
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
        new net.neoforged.neoforge.network.event.RegisterPayloadHandlersEvent()
            .registrar("1")
            .configurationToClient("config-type", "config-codec", "config-handler");
        int remaining = 2;
        while (remaining > 0) {
            remaining--;
        }
    }
}
""",
    "net/neoforged/neoforge/network/event/RegisterPayloadHandlersEvent.java": """
package net.neoforged.neoforge.network.event;

public final class RegisterPayloadHandlersEvent {
    public net.neoforged.neoforge.network.registration.PayloadRegistrar registrar(String version) {
        return new net.neoforged.neoforge.network.registration.PayloadRegistrar();
    }
}
""",
    "net/neoforged/neoforge/network/registration/PayloadRegistrar.java": """
package net.neoforged.neoforge.network.registration;

public final class PayloadRegistrar {
    public PayloadRegistrar configurationToClient(Object type, Object codec, Object handler) {
        return this;
    }
}
""",
}

# A small legacy Forge jar keeps the compatibility probe honest for packs that
# still carry ``META-INF/mods.toml``. Forge and NeoForge share the constructor
# and registration ABI, but the package names and metadata marker differ; the
# native loader intentionally treats this as a bounded legacy alias and reports
# the same ``neoforge`` execution kind.
FORGE_SOURCES = {
    "net/minecraftforge/fml/common/Mod.java": """
package net.minecraftforge.fml.common;

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
    "net/minecraftforge/registries/RegistryObject.java": """
package net.minecraftforge.registries;

public final class RegistryObject<T> {}
""",
    "net/minecraftforge/registries/DeferredRegister.java": """
package net.minecraftforge.registries;

import java.util.function.Supplier;

public final class DeferredRegister<T> {
    public static <T> DeferredRegister<T> create(Object registry, String id) {
        return new DeferredRegister<>();
    }

    public <V extends T> RegistryObject<V> register(String id, Supplier<V> value) {
        return new RegistryObject<>();
    }
}
""",
    "net/minecraftforge/network/simple/SimpleChannel.java": """
package net.minecraftforge.network.simple;

public final class SimpleChannel {
    public SimpleChannel registerMessage(
            int id, Class<?> type, Object encoder, Object decoder, Object handler) {
        return this;
    }
}
""",
    "demo/Forge.java": """
package demo;

@net.minecraftforge.fml.common.Mod("native-smoke-forge")
public final class Forge {
    public Forge() {
        net.minecraftforge.registries.DeferredRegister
            .create("item", "native-smoke-forge")
            .register("crystal", null);
        new net.minecraftforge.network.simple.SimpleChannel()
            .registerMessage(0, Object.class, null, null, null);
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
        for relative, source in {**SOURCES, **NEO_SOURCES, **FORGE_SOURCES}.items():
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
                *(
                    str(source_root / relative)
                    for relative in {**SOURCES, **NEO_SOURCES, **FORGE_SOURCES}
                ),
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
        forge_metadata = metadata / "mods.toml"
        forge_metadata.write_text(
            'modLoader = "javafml"\n'
            'loaderVersion = "[1,)"\n'
            'license = "MIT"\n'
            '[[mods]]\n'
            'modId = "native-smoke-forge"\n'
            'version = "1.0.0"\n'
            '[[dependencies.native-smoke-forge]]\n'
            'modId = "minecraft"\n'
            'mandatory = true\n',
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

        forge_jar = root / "native-smoke-forge.jar"
        package_result = run(
            [
                "jar",
                "--create",
                "--file",
                str(forge_jar),
                "-C",
                str(classes),
                "demo/Forge.class",
                "-C",
                str(classes),
                "net/minecraftforge/fml/common/Mod.class",
                "-C",
                str(classes),
                "net/minecraftforge/registries/DeferredRegister.class",
                "-C",
                str(classes),
                "net/minecraftforge/registries/RegistryObject.class",
                "-C",
                str(classes),
                "net/minecraftforge/network/simple/SimpleChannel.class",
                "-C",
                str(metadata),
                "mods.toml",
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
                "demo/Neo.class",
                "-C",
                str(classes),
                "net/neoforged/fml/common/Mod.class",
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
        if report["host_calls"] < 10:
            print(
                "native probe did not exercise the Optional/primitive-optional and Map.forEach host paths: "
                f"expected at least 10 host calls, got {report['host_calls']}",
                file=sys.stderr,
            )
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
            or neo_report.get("payload_directions") != ["configurationToClient"]
        ):
            print(f"native NeoForge probe failed: {json.dumps(neo_report, sort_keys=True)}", file=sys.stderr)
            return 1
        forge_probe = run(
            [
                "cargo",
                "run",
                "--quiet",
                "-p",
                "horizon-native-java",
                "--",
                "--json",
                str(forge_jar),
                "neoforge",
            ]
        )
        if forge_probe.returncode:
            print(forge_probe.stderr, file=sys.stderr, end="")
            print(forge_probe.stdout, file=sys.stderr, end="")
            return forge_probe.returncode
        try:
            forge_report = json.loads(forge_probe.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError) as error:
            print(f"native Forge probe returned invalid JSON: {error}\n{forge_probe.stdout}", file=sys.stderr)
            return 1
        if (
            forge_report.get("status") != "ok"
            or forge_report.get("kind") != "neoforge"
            or forge_report.get("entrypoints") != ["demo.Forge"]
            or not isinstance(forge_report.get("host_calls"), int)
            or forge_report["host_calls"] < 2
        ):
            print(f"native Forge probe failed: {json.dumps(forge_report, sort_keys=True)}", file=sys.stderr)
            return 1
        print(
            json.dumps(
                {"fabric": report, "forge": forge_report, "neoforge": neo_report},
                sort_keys=True,
            )
        )
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
