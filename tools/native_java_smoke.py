#!/usr/bin/env python3
"""Compile and execute tiny Fabric and NeoForge native probe jars.

This is intentionally independent of ``.local`` and a Minecraft installation.  It
checks the complete native probe path with real classfiles: javac emits the
bytecode, jar packages it with Fabric metadata, and horizon-native-java discovers
and executes both lifecycle entries, including an Optional.filter predicate callback,
plus a constructor-discovered NeoForge jar that registers a configuration payload.

On top of that the Fabric entrypoint resolves ``Registries.ITEM`` and an
``Identifier`` marker through the host and calls the plain ``Registry.register``
path (the probe returns the registered value only after it recorded the row), a
second NeoForge jar registers payloads and has its ``payload_directions`` checked
exactly, and a Fabric jar whose entrypoint lacks ``onInitialize`` must be rejected
with the class and method named in its error report.  A compiled Mixin jar covers
the plan report (``--mixin-plans`` and the normal JSON run) end to end: two
``HEAD``/``RETURN`` callbacks become plans while an ``INVOKE`` callback is left
out.
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
import net.minecraft.core.Registry;
import net.minecraft.core.registries.Registries;
import net.minecraft.resources.Identifier;

public final class First {
    public void onInitialize() {
        int value = plus(19, 23);
        while (value < 42) {
            value++;
        }
        Optional<String> marker = Optional.ofNullable("native");
        require(
            marker.filter(text -> text.length() == 6).isPresent(),
            "Optional.filter host path failed");
        OptionalLong longMarker = OptionalLong.of(42L);
        require(
            longMarker.isPresent() && longMarker.toString().equals("OptionalLong[42]"),
            "OptionalLong host path failed");
        OptionalDouble doubleMarker = OptionalDouble.of(3.5D);
        // Double constants stay neutral in the bounded constant pool, so only
        // presence and the toString shape can be asserted here.
        require(
            doubleMarker.isPresent() && doubleMarker.toString().startsWith("OptionalDouble["),
            "OptionalDouble host path failed");
        HashMap<String, Integer> values = new HashMap<>();
        values.put("left", 19);
        values.put("right", 23);
        values.forEach((key, item) -> {
            require(
                item >= 19 && item <= 23,
                "Map.forEach host path failed for " + key);
        });
        // Both Identifier factories resolve to the same marker, so toString and
        // equals only pass when the static and instance host paths answered.
        Identifier created = Identifier.fromNamespaceAndPath("demo", "ruby");
        require(
            created.toString().equals("demo:ruby"),
            "Identifier.fromNamespaceAndPath host path failed");
        Identifier parsed = Identifier.tryParse("demo:ruby");
        require(parsed.equals(created), "Identifier.tryParse host path failed");
        // Registries.ITEM becomes a registry marker and the parsed Identifier an
        // identifier marker. The host returns the registered value only together
        // with the recorded registration row, so null means the plain
        // Registry.register path did not run.
        Object registered = Registry.register(Registries.ITEM, parsed, new Object());
        require(registered != null, "Registry.register host path failed");
        // An unmodelled host call whose descriptor returns void must not leave a
        // phantom value behind for the rest of the method.
        java.lang.Thread.yield();
        require(plus(19, 23) == 42, "void host call disturbed the operand stack");
    }

    private static int plus(int left, int right) {
        return left + right;
    }

    private static void require(boolean condition, String message) {
        if (!condition) {
            // The native interpreter answers athrow with a neutral return, so a
            // thrown exception would not fail the probe. A host error does, and
            // Double.parseDouble echoes the message back in that error.
            java.lang.Double.parseDouble(message);
        }
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
    # Fabric metadata lists this class as a main entrypoint, but it has no
    # onInitialize: the probe must reject it rather than skip it silently.
    "demo/Negative.java": """
package demo;

public final class Negative {
    public void onLoad() {
    }
}
""",
    # Compile-only stubs: javac needs these signatures, but the class files are
    # left out of the jar so every call crosses the host, which is the path
    # under test.
    "net/minecraft/resources/Identifier.java": """
package net.minecraft.resources;

public final class Identifier {
    public static Identifier fromNamespaceAndPath(String namespace, String path) {
        throw new UnsupportedOperationException("compile-only stub");
    }

    public static Identifier tryParse(String value) {
        throw new UnsupportedOperationException("compile-only stub");
    }

    // Declared so javac emits net/minecraft/resources/Identifier as the call
    // owner; java/lang/Object would bypass the host's Identifier handling.
    @Override
    public String toString() {
        throw new UnsupportedOperationException("compile-only stub");
    }

    @Override
    public boolean equals(Object other) {
        throw new UnsupportedOperationException("compile-only stub");
    }
}
""",
    "net/minecraft/core/Registry.java": """
package net.minecraft.core;

import net.minecraft.resources.Identifier;

public final class Registry<T> {
    public static <T> T register(Registry<T> registry, Identifier id, T value) {
        throw new UnsupportedOperationException("compile-only stub");
    }
}
""",
    "net/minecraft/core/registries/Registries.java": """
package net.minecraft.core.registries;

public final class Registries {
    public static final net.minecraft.core.Registry<Object> ITEM = null;
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

    public PayloadRegistrar playToClient(Object type, Object codec, Object handler) {
        return this;
    }

    public PayloadRegistrar playToServer(Object type, Object codec, Object handler) {
        return this;
    }
}
""",
    "demo/NeoPayload.java": """
package demo;

@net.neoforged.fml.common.Mod("native-smoke-neo-payload")
public final class NeoPayload {
    public NeoPayload() {
        new net.neoforged.neoforge.network.event.RegisterPayloadHandlersEvent()
            .registrar("1")
            .playToClient("client-type", "client-codec", "client-handler")
            .playToServer("server-type", "server-codec", "server-handler");
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


# A compiled Mixin jar exercises the whole plan path: javac writes real
# ``RuntimeVisibleAnnotations``, ``--mixin-plans`` reports the two supported
# anchors, and the same jar still runs through a normal Fabric entrypoint.
# ``demo/Probe`` is compile-only, so the target's call crosses the host and the
# normal run proves the packaged target class executed.
MIXIN_SOURCES = {
    "org/spongepowered/asm/mixin/Mixin.java": """
package org.spongepowered.asm.mixin;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.TYPE)
public @interface Mixin {
    String[] value() default {};
    String[] targets() default {};
}
""",
    "org/spongepowered/asm/mixin/injection/At.java": """
package org.spongepowered.asm.mixin.injection;

import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
@Target({})
public @interface At {
    String value();
}
""",
    "org/spongepowered/asm/mixin/injection/Inject.java": """
package org.spongepowered.asm.mixin.injection;

import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;

@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.METHOD)
public @interface Inject {
    String[] method();
    At[] at();
    boolean cancellable() default false;
}
""",
    "demo/Probe.java": """
package demo;

public final class Probe {
    public static void mark(String marker, int value) {
    }
}
""",
    "demo/Target.java": """
package demo;

public final class Target {
    public static void run(int value) {
        Probe.mark("target", value);
    }
}
""",
    "demo/TargetMixin.java": """
package demo;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;

@Mixin(targets = "Ldemo/Target;")
public abstract class TargetMixin {
    @Inject(method = "run(I)V", at = @At("HEAD"))
    private static void before(int value) {
        Probe.mark("head", value);
    }

    @Inject(method = "run(I)V", at = @At("RETURN"))
    private static void after(int value) {
        Probe.mark("return", value);
    }
}
""",
    # ``INVOKE`` is not a native execution anchor: discovery must drop this
    # callback instead of approximating it, so the jar still reports two plans.
    "demo/InvokeMixin.java": """
package demo;

import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;

@Mixin(targets = "Ldemo/Target;")
public abstract class InvokeMixin {
    @Inject(method = "run(I)V", at = @At("INVOKE"))
    private static void invokeOnly(int value) {
        Probe.mark("invoke", value);
    }
}
""",
    "demo/MixinEntry.java": """
package demo;

public final class MixinEntry {
    public void onInitialize() {
        Target.run(21);
    }
}
""",
}


def run(command: list[str], *, cwd: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)


def package_jar(archive: Path, *entries: tuple[Path, str]) -> int:
    """Package ``(directory, entry)`` pairs; returns the ``jar`` exit code."""
    command = ["jar", "--create", "--file", str(archive)]
    for directory, entry in entries:
        command.extend(["-C", str(directory), entry])
    result = run(command)
    if result.returncode:
        print(result.stderr, file=sys.stderr, end="")
    return result.returncode


def run_probe(archive: Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    """Run horizon-native-java on ``archive`` in JSON mode."""
    command = ["cargo", "run", "--quiet", "-p", "horizon-native-java", "--", "--json"]
    return run([*command, str(archive), *arguments])


def probe_json(archive: Path, *arguments: str) -> tuple[int, dict | None, str]:
    """Probe one jar; returns ``(exit code, report or None, combined output)``."""
    result = run_probe(archive, *arguments)
    output = f"{result.stderr}{result.stdout}"
    if result.returncode:
        return result.returncode, None, output
    try:
        return 0, json.loads(result.stdout.strip().splitlines()[-1]), output
    except (IndexError, json.JSONDecodeError):
        return 1, None, output


def expect_probe(
    name: str,
    archive: Path,
    kind: str,
    entrypoints: list[str],
    payload_directions: list[str],
    minimum_calls: int,
    covered: str = "",
    registrations: list[tuple[str, str]] | None = None,
) -> dict | None:
    """Probe one jar and check its whole JSON report; ``None`` means failure."""
    status, report, output = probe_json(archive, kind)
    if report is None:
        print(f"native {name} probe failed ({status}):\n{output}", file=sys.stderr)
        return None
    problems = []
    if report.get("status") != "ok":
        problems.append(f"status {report.get('status')!r}")
    if report.get("kind") != kind:
        problems.append(f"kind {report.get('kind')!r}")
    if report.get("entrypoints") != entrypoints:
        problems.append(f"entrypoints {report.get('entrypoints')!r}, expected {entrypoints!r}")
    if not isinstance(report.get("host_calls"), int):
        problems.append("no host call count")
    elif report["host_calls"] < minimum_calls:
        detail = f" ({covered})" if covered else ""
        problems.append(
            f"{report['host_calls']} host calls{detail}, expected at least {minimum_calls}"
        )
    if report.get("payload_directions") != payload_directions:
        problems.append(
            f"payload directions {report.get('payload_directions')!r}, "
            f"expected {payload_directions!r}"
        )
    if registrations is not None:
        recorded = [
            (entry.get("registry"), entry.get("id"))
            for entry in report.get("registrations", [])
            if isinstance(entry, dict)
        ]
        if recorded != registrations:
            problems.append(
                f"registrations {recorded!r}, expected {registrations!r}"
            )
    if report.get("mixin_plans") != 0 or report.get("mixin_targets") != []:
        problems.append(
            f"unexpected Mixin plans {report.get('mixin_plans')!r} "
            f"targets {report.get('mixin_targets')!r} for a plain jar"
        )
    if problems:
        print(
            f"native {name} probe failed: {', '.join(problems)}: "
            f"{json.dumps(report, sort_keys=True)}",
            file=sys.stderr,
        )
        return None
    return report


def expect_mixin_probe(archive: Path) -> dict | None:
    """Check plan discovery plus a normal run of the same compiled Mixin jar."""
    expected_plans = [
        {
            "target": "demo/Target",
            "method": "run",
            "descriptor": "(I)V",
            "callback": "demo/TargetMixin#before",
            "callback_descriptor": "(I)V",
            "at": "HEAD",
            "cancellable": False,
        },
        {
            "target": "demo/Target",
            "method": "run",
            "descriptor": "(I)V",
            "callback": "demo/TargetMixin#after",
            "callback_descriptor": "(I)V",
            "at": "RETURN",
            "cancellable": False,
        },
    ]
    expected_targets = ["demo/Target#run(I)V"]
    status, plans_report, output = probe_json(archive, "--mixin-plans")
    if plans_report is None:
        print(f"native mixin plan probe failed ({status}):\n{output}", file=sys.stderr)
        return None
    problems = []
    if plans_report.get("status") != "ok":
        problems.append(f"plan status {plans_report.get('status')!r}")
    if plans_report.get("targets") != expected_targets:
        problems.append(
            f"plan targets {plans_report.get('targets')!r}, expected {expected_targets!r}"
        )
    plans = plans_report.get("plans")
    if not isinstance(plans, list):
        problems.append(f"plans {plans!r}, expected a list")
    else:
        if plans != expected_plans:
            problems.append(f"plans {plans!r}, expected {expected_plans!r}")
        # The separate INVOKE mixin is dropped at discovery; no plan may name it.
        if any(
            str(plan.get("callback", "")).startswith("demo/InvokeMixin#")
            for plan in plans
            if isinstance(plan, dict)
        ):
            problems.append("INVOKE callback leaked into the plans")
    status, run_report, output = probe_json(archive, "main")
    if run_report is None:
        print(f"native mixin run failed ({status}):\n{output}", file=sys.stderr)
        return None
    if run_report.get("status") != "ok":
        problems.append(f"run status {run_report.get('status')!r}")
    if run_report.get("entrypoints") != ["demo.MixinEntry"]:
        problems.append(f"run entrypoints {run_report.get('entrypoints')!r}")
    if run_report.get("mixin_plans") != len(expected_plans):
        problems.append(
            f"run mixin plans {run_report.get('mixin_plans')!r}, "
            f"expected {len(expected_plans)}"
        )
    if run_report.get("mixin_targets") != expected_targets:
        problems.append(
            f"run mixin targets {run_report.get('mixin_targets')!r}, "
            f"expected {expected_targets!r}"
        )
    # The entrypoint calls the packaged target, whose Probe.mark crosses the
    # host, so at least one call proves the normal run executed the class.
    if not isinstance(run_report.get("host_calls"), int) or run_report["host_calls"] < 1:
        problems.append(f"run host calls {run_report.get('host_calls')!r}, expected >= 1")
    if problems:
        print(
            f"native mixin probe failed: {', '.join(problems)}: "
            f"{json.dumps({'plans': plans_report, 'run': run_report}, sort_keys=True)}",
            file=sys.stderr,
        )
        return None
    return {"plans": plans_report, "run": run_report}


def expect_rejected(archive: Path, class_name: str, method: str) -> bool:
    """A jar whose entrypoint lacks ``method`` must fail and name both."""
    result = run_probe(archive)
    try:
        report = json.loads(result.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError):
        report = {}
    error = str(report.get("error", "")) + result.stderr
    if (
        result.returncode != 0
        and report.get("status", "error") in ("error", None)
        and (report.get("stage") in ("execute", None))
        and class_name in error
        and method in error
    ):
        return True
    print(
        f"native probe did not reject {class_name} without {method}:\n"
        f"{result.stderr}{result.stdout}",
        file=sys.stderr,
    )
    return False


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
        negative_metadata = root / "negative-metadata"
        mixin_metadata = root / "mixin-metadata"
        source_root.mkdir()
        classes.mkdir()
        metadata.mkdir()
        negative_metadata.mkdir()
        mixin_metadata.mkdir()
        sources = {**SOURCES, **NEO_SOURCES, **FORGE_SOURCES, **MIXIN_SOURCES}
        for relative, source in sources.items():
            path = source_root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source.lstrip(), encoding="utf-8")

        compile_result = run(
            [
                "javac",
                "--release",
                "17",
                "-d",
                str(classes),
                *(str(source_root / relative) for relative in sources),
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
        # Same Fabric shape as ``fabric_metadata``, but the only entrypoint has
        # no onInitialize, so the probe has to fail instead of skipping it.
        (negative_metadata / "fabric.mod.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": "native-smoke-negative",
                    "version": "1.0.0",
                    "depends": {"minecraft": "26.3", "fabricloader": "*"},
                    "entrypoints": {"main": ["demo.Negative"]},
                }
            ),
            encoding="utf-8",
        )
        # The Mixin jar declares an ordinary entrypoint, so the same classes
        # serve the plan report and a normal execution run.
        (mixin_metadata / "fabric.mod.json").write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "id": "native-smoke-mixin",
                    "version": "1.0.0",
                    "depends": {"minecraft": "26.3", "fabricloader": "*"},
                    "entrypoints": {"main": ["demo.MixinEntry"]},
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

        # Only the entrypoint classes are packaged: every stub stays out, so
        # net.minecraft/net.neoforged calls cross the host, the path under test.
        fabric_jar = root / "native-smoke-fabric.jar"
        package_result = package_jar(
            fabric_jar,
            (classes, "demo/First.class"),
            (classes, "demo/Second.class"),
            (metadata, "fabric.mod.json"),
        )
        if package_result:
            return package_result

        forge_jar = root / "native-smoke-forge.jar"
        package_result = package_jar(
            forge_jar,
            (classes, "demo/Forge.class"),
            (classes, "net/minecraftforge/fml/common/Mod.class"),
            (metadata, "mods.toml"),
        )
        if package_result:
            return package_result

        neo_jar = root / "native-smoke-neoforge.jar"
        package_result = package_jar(
            neo_jar,
            (classes, "demo/Neo.class"),
            (classes, "net/neoforged/fml/common/Mod.class"),
            (metadata, "neoforge.mods.toml"),
        )
        if package_result:
            return package_result

        # A separate NeoForge jar proves payload recording is per-run: its
        # directions must be exactly its own, never the other jar's.
        payload_jar = root / "native-smoke-neoforge-payload.jar"
        package_result = package_jar(
            payload_jar,
            (classes, "demo/NeoPayload.class"),
            (classes, "net/neoforged/fml/common/Mod.class"),
            (metadata, "neoforge.mods.toml"),
        )
        if package_result:
            return package_result

        negative_jar = root / "native-smoke-negative.jar"
        package_result = package_jar(
            negative_jar,
            (classes, "demo/Negative.class"),
            (negative_metadata, "fabric.mod.json"),
        )
        if package_result:
            return package_result

        # Only the mod classes and metadata are packaged: the annotation stubs
        # and demo/Probe stay out, so discovery reads real annotations and the
        # target's Probe.mark crosses the host.
        mixin_jar = root / "native-smoke-mixin.jar"
        package_result = package_jar(
            mixin_jar,
            (classes, "demo/Target.class"),
            (classes, "demo/TargetMixin.class"),
            (classes, "demo/InvokeMixin.class"),
            (classes, "demo/MixinEntry.class"),
            (mixin_metadata, "fabric.mod.json"),
        )
        if package_result:
            return package_result

        report = expect_probe(
            "fabric",
            fabric_jar,
            "main",
            ["demo.First", "demo.Second"],
            [],
            20,
            "Optional/OptionalLong/OptionalDouble, Map.forEach, Identifier, "
            "Registry.register and void host paths",
            [("minecraft:item", "demo:ruby")],
        )
        if report is None:
            return 1
        # The plain (non-JSON) invocation has to accept a bare jar path: the
        # old parser consumed it as an optional switch and printed usage.
        plain = run(
            [
                "cargo",
                "run",
                "--quiet",
                "-p",
                "horizon-native-java",
                "--",
                str(fabric_jar),
            ]
        )
        if plain.returncode != 0 or "entrypoint(s) ran" not in plain.stdout:
            print(
                f"native plain CLI failed ({plain.returncode}):\n"
                f"{plain.stderr}{plain.stdout}",
                file=sys.stderr,
            )
            return 1
        mixin_report = expect_mixin_probe(mixin_jar)
        if mixin_report is None:
            return 1
        neo_report = expect_probe(
            "neoforge", neo_jar, "neoforge", ["demo.Neo"], ["configurationToClient"], 4, ""
        )
        if neo_report is None:
            return 1
        # A separate payload jar proves recording is per run: its directions are
        # exactly its own, never the other jar's.
        payload_report = expect_probe(
            "neoforge payload",
            payload_jar,
            "neoforge",
            ["demo.NeoPayload"],
            ["playToClient", "playToServer"],
            3,
            "",
        )
        if payload_report is None:
            return 1
        forge_report = expect_probe(
            "forge", forge_jar, "neoforge", ["demo.Forge"], ["playBidirectional"], 2, ""
        )
        if forge_report is None:
            return 1
        if not expect_rejected(negative_jar, "demo.Negative", "onInitialize"):
            return 1
        print("negative: ok")
        print(
            json.dumps(
                {
                    "fabric": report,
                    "forge": forge_report,
                    "mixin": mixin_report,
                    "neoforge": neo_report,
                    "neoforge_payload": payload_report,
                },
                sort_keys=True,
            )
        )
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
