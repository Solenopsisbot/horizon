#!/usr/bin/env python3
"""Run one Horizon dedicated-server process per Java mod and record outcomes.

Every row ends in one of three labels: `native-ok` (the server runs the jar, and the
plans it skips stay inside the jar), `needs-loader` (it runs, but its mixin plans
target game classes, which only a real loader applies) or `native-gap` (native
execution refuses it yet). `--table` prints the label table, and `--docs PATH`
rewrites the generated block of `docs/java-mod-compat-status.md` between its
`java-mod-ledger` markers. `--from PATH` reads an earlier text ledger instead of
starting servers, so a table can be regenerated without a jar set.

The game's dedicated log names a started jar's engine but no identity or plan counts,
so a run also reads the `java-runtime.json` that role writes under the scratch home,
and asks `horizon-native-java --mixin-plans` how many of the jar's plans name a class
the jar does not hold (the `target-outside-jar` count the label needs). A jar the log
names after `skipping` did not start, whatever else the process reached.

`--with-deps` runs each jar together with its declared dependency closure, the way a
real loader would refuse to load it without one: the required ids of its
`fabric.mod.json` (`depends`) or `mods.toml`/`neoforge.mods.toml` (mandatory/required
`[[dependencies]]`), resolved through `provides` aliases and jar-in-jar modules
against the jars of `--deps-dir` (by default the mod directory and the corpora under
`~/horizon-corpus`). The row stays the jar under test's own outcome; the closure's
ids and the ones the server refused are extra fields. A jar whose closure needs an
id no corpus jar provides is not started and gets a fourth label,
`missing-dependency`, instead of a `native-gap` that would blame native execution
for what any loader refuses too. `--rerun LEDGER` picks the jars an earlier ledger
labelled `--rerun-label` (default `native-gap`).
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import zipfile
from dataclasses import asdict, dataclass, replace
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hybrid_pack  # noqa: E402  (jar declarations and the provider index)
import lab  # noqa: E402

MAX_OUTPUT = 512 * 1024
MAX_REASON = 240
# A refused jar's `java-runtime.json` row carries the runner's whole cause up to the
# game's own bound (`MAX_CAUSE_CHARS` in game/java_runtime_file.rs); the ledger keeps
# all of it, where the log line it used to read stops at 160 characters.
MAX_CAUSE = 2048
MAX_TABLE_REASON = 80

# The three answers a row can give. `native-ok` runs natively; `needs-loader` runs
# but its skipped plans target game classes the weaver cannot rewrite, so only a
# real loader applies them; `native-gap` is refused before it runs.
NATIVE_OK = "native-ok"
NEEDS_LOADER = "needs-loader"
NATIVE_GAP = "native-gap"
# Only `--with-deps` writes this one: the jar's declared closure needs an id no corpus
# jar provides, so no loader (native or real) would start it and it was not run.
MISSING_DEPENDENCY = "missing-dependency"
LABELS = (NATIVE_OK, NEEDS_LOADER, NATIVE_GAP, MISSING_DEPENDENCY)
# Labels earlier runs wrote; `read_ledger` normalises them.
LEGACY_LABELS = {"native": NATIVE_OK, "loader": NEEDS_LOADER}

# The generated table in docs/java-mod-compat-status.md lives between these two lines.
DOC_START = "<!-- java-mod-ledger:start -->"
DOC_END = "<!-- java-mod-ledger:end -->"

# The entrypoint kinds the dedicated server role asks a jar for, in the order
# `game/dedicated.rs` tries them. Every other Fabric entrypoint key (client,
# modmenu, preLaunch, sodium:config_api_user, controlify, ...) is a client-side
# hook the server role never queries.
SERVER_ENTRYPOINT_KINDS = frozenset({"server", "main", "neoforge"})
NEOFORGE_MOD_ANNOTATIONS = (
    b"Lnet/neoforged/fml/common/Mod;",
    b"Lnet/minecraftforge/fml/common/Mod;",
)


@dataclass(frozen=True)
class JarEntrypoints:
    """Loader entrypoints read from a jar without running it."""

    kinds: tuple[str, ...] = ()
    neoforge: bool = False
    client_side_only: bool = False

    @property
    def server(self) -> bool:
        """Whether the dedicated server role has an entrypoint to execute."""
        return self.neoforge or any(kind in SERVER_ENTRYPOINT_KINDS for kind in self.kinds)

    @property
    def available(self) -> str:
        kinds = set(self.kinds)
        if self.neoforge:
            kinds.add("neoforge")
        return ", ".join(sorted(kinds)) if kinds else "none"


@dataclass(frozen=True)
class Row:
    file: str
    id: str = "?"
    version: str = "?"
    verdict: str = "refused"
    subtype: str = ""
    engine: str = "?"
    woven: int = 0
    skipped: int = 0
    reason: str = ""
    seconds: float = 0.0
    skip_reasons: tuple[str, ...] = ()
    # Plans whose target class is outside the jar, from the plan report. The
    # `target-outside-jar` share, used by `recommendation`; not serialized.
    game_plans: int = 0
    # One of LABELS; empty until `parse_log` classifies the row.
    recommend: str = ""
    # `--with-deps` only: the mod ids of the dependency closure the jar ran with,
    # and the ones among them the server refused (a loader would refuse the jar too;
    # the row's own verdict still says what native execution did with the jar).
    deps: tuple[str, ...] = ()
    dep_refused: tuple[str, ...] = ()
    # `--with-deps` only: the closure mod whose class the refusal's `(at Class#method)`
    # names, when the jar under test got that far into its dependency's code. The
    # label stays the jar's (native execution still cannot carry the pack); this says
    # whose code to fix.
    blocked_in: str = ""


def bounded(value: str, limit: int = MAX_REASON) -> str:
    value = " ".join(value.replace("\x00", "").split())
    return value if len(value) <= limit else value[: limit - 1] + "…"


def metadata(path: Path) -> tuple[str, str]:
    try:
        with zipfile.ZipFile(path) as jar:
            try:
                raw = jar.read("fabric.mod.json").decode("utf-8", errors="replace")
                obj = json.loads(lenient_json_text(raw))
                if not isinstance(obj, dict):
                    return "?", "?"
                return str(obj.get("id", "?")), str(obj.get("version", "?"))
            except KeyError:
                for name in ("META-INF/neoforge.mods.toml", "META-INF/mods.toml"):
                    try:
                        text = jar.read(name).decode("utf-8", errors="replace")
                    except KeyError:
                        continue
                    ident = re.search(r"\bmodId\s*=\s*\"([^\"]+)\"", text)
                    version = re.search(r"\bversion\s*=\s*\"([^\"]+)\"", text)
                    if ident:
                        return ident.group(1), version.group(1) if version else "?"
                return "?", "?"
    except (OSError, KeyError, ValueError, zipfile.BadZipFile):
        return "?", "?"


def lenient_json_text(text: str) -> str:
    """Escape raw control characters inside JSON strings, as the native reader does.

    A leading byte-order mark is dropped: the loader (and the native reader)
    accept a fabric.mod.json that starts with one.
    """
    text = text.removeprefix("\ufeff")
    out: list[str] = []
    in_string = escaped = False
    escapes = {"\n": "\\n", "\r": "\\r", "\t": "\\t", "\b": "\\b", "\f": "\\f"}
    for char in text:
        if not in_string:
            if char == '"':
                in_string = True
            out.append(char)
            continue
        if escaped:
            escaped = False
            out.append(char)
            continue
        if char == "\\":
            escaped = True
            out.append(char)
        elif char == '"':
            in_string = False
            out.append(char)
        elif ord(char) < 0x20:
            out.append(escapes.get(char, f"\\u{ord(char):04x}"))
        else:
            out.append(char)
    return "".join(out)


def fabric_entrypoint_kinds(obj: dict) -> tuple[str, ...]:
    """The Fabric entrypoint keys that carry at least one usable entry."""
    kinds = set()
    groups = obj.get("entrypoints")
    if isinstance(groups, dict):
        for kind, group in groups.items():
            values = group if isinstance(group, list) else [group]
            if any(
                isinstance(value, str)
                or (isinstance(value, dict) and isinstance(value.get("value"), str))
                for value in values
            ):
                kinds.add(str(kind))
    return tuple(sorted(kinds))


def read_jar_entrypoints(path: Path | None) -> JarEntrypoints | None:
    """Read a jar's loader metadata; None when the jar cannot be read.

    This mirrors the native reader's rule: Fabric entrypoint keys, NeoForge
    `@Mod` classes, and client-only from the NeoForge/Forge TOML's
    `clientSideOnly` or Fabric's `"environment": "client"`.
    """
    if path is None:
        return None
    try:
        with zipfile.ZipFile(path) as jar:
            names = jar.namelist()
            kinds: tuple[str, ...] = ()
            if "fabric.mod.json" in names:
                raw = jar.read("fabric.mod.json").decode("utf-8", errors="replace")
                obj = json.loads(lenient_json_text(raw))
                if not isinstance(obj, dict):
                    return None
                kinds = fabric_entrypoint_kinds(obj)
                # `"environment": "client"` keeps a Fabric mod off a server
                # (a multi-loader jar's NeoForge `@Mod` class included).
                fabric_client_only = obj.get("environment") == "client"
            else:
                fabric_client_only = False
            client_side_only = fabric_client_only
            for name in ("META-INF/neoforge.mods.toml", "META-INF/mods.toml"):
                if name in names:
                    text = jar.read(name).decode("utf-8", errors="replace")
                    client_side_only = fabric_client_only or bool(
                        re.search(r"(?im)^\s*clientSideOnly\s*=\s*[\"']?true[\"']?\s*$", text)
                    )
                    break
            if client_side_only or any(kind in SERVER_ENTRYPOINT_KINDS for kind in kinds):
                # The server has something to execute (or the jar says it is
                # client-only); no class scan can change that verdict.
                return JarEntrypoints(kinds, False, client_side_only)
            neoforge = False
            for name in names:
                if (
                    not name.endswith(".class")
                    or name.endswith("module-info.class")
                    or name.endswith("package-info.class")
                    or name.startswith("META-INF/versions/")
                ):
                    continue
                data = jar.read(name)
                if any(marker in data for marker in NEOFORGE_MOD_ANNOTATIONS):
                    neoforge = True
                    break
            return JarEntrypoints(kinds, neoforge, client_side_only)
    except (OSError, ValueError, KeyError, EOFError, RuntimeError, zipfile.BadZipFile):
        return None


def skip_subtype(entrypoints: JarEntrypoints | None) -> str | None:
    """Classify a jar the server cannot execute: client-only, library, or neither."""
    if entrypoints is None:
        return None
    if entrypoints.client_side_only:
        return "client-only"
    if entrypoints.server:
        return None
    if entrypoints.kinds:
        return "client-only"
    return "library"


def runtime_row(report: dict, jar: Path) -> dict | None:
    """The `java-runtime.json` row a started jar published, if it published one."""
    rows = report.get("jars") if isinstance(report, dict) else None
    if not isinstance(rows, list):
        return None
    for row in rows:
        if isinstance(row, dict) and row.get("file") == jar.name:
            return row
    return None


# The corpora `--with-deps` looks for providers in when no `--deps-dir` is given,
# besides the mod directory itself: the two ledger corpora, the dependency pool the
# mega corpus was fetched with, and the other corpora a provider may only live in.
# (`fabric-libs-*` are the game's and loader's Maven libraries, not mods.)
DEFAULT_DEP_DIRS = tuple(Path.home() / "horizon-corpus" / name for name in (
    "corpus-xl", "mega", "mega-deps", "corpus-xplus", "corpus-helix"))


@dataclass(frozen=True)
class Closure:
    """The jars one jar needs beside it, and the ids no corpus jar provides.

    `missing` maps each unresolved id to the mod id that required it (the jar under
    test itself, or a dependency the closure pulled in).
    """

    jars: tuple[Path, ...] = ()
    ids: tuple[str, ...] = ()
    missing: tuple[tuple[str, str], ...] = ()


def corpus_jars(directories: list[Path]) -> list[hybrid_pack.PackJar]:
    """Every `.jar` of `directories` with its declaration, each real file once."""
    seen: set[Path] = set()
    jars: list[hybrid_pack.PackJar] = []
    for directory in directories:
        if not directory.is_dir():
            continue
        for path in sorted(directory.iterdir(), key=lambda item: item.name):
            if path.suffix.lower() != ".jar" or not path.is_file():
                continue
            real = path.resolve()
            if real in seen:
                continue
            seen.add(real)
            jars.append(hybrid_pack.PackJar(path, None, hybrid_pack.read_declaration(path)))
    return jars


def provider_index(jars: list[hybrid_pack.PackJar]) -> dict[str, list[hybrid_pack.PackJar]]:
    """Each id (own, `provides` alias or nested module) and its providers, best first.

    The hybrid tool's order: a jar whose own id is the dependency first, then one
    that carries it as a nested module, then the newest version.
    """
    return hybrid_pack._provider_index(jars)


def canonical_provider(providers: list[hybrid_pack.PackJar]) -> hybrid_pack.PackJar:
    """The provider a dependency should get, from the index's best-first list.

    A mod that bundles a library as jar-in-jar provides every id the library
    does: Distant Horizons nests Fabric API's root and its 44 modules, so the
    index's "most nested ids" order would hand `fabric-api-base` to Distant
    Horizons and run a whole second mod beside the jar under test. The library
    itself is the provider: a candidate whose own id another candidate nests
    wins, and the index's order breaks the remaining ties.
    """
    bundled = {ident.lower() for provider in providers
               for ident in provider.declaration.nested_ids}
    for provider in providers:
        if provider.mod_id.lower() in bundled:
            return provider
    return providers[0]


def dependency_closure(jar: Path, index: dict[str, list[hybrid_pack.PackJar]],
                       declaration: hybrid_pack.Declaration | None = None) -> Closure:
    """The declared dependency closure of `jar`, resolved against `index`.

    Every required id is satisfied by the platform (`minecraft`, `fabricloader`,
    `neoforge`, ...), by an id the closure already holds (the jar's own id, its
    `provides` aliases and jar-in-jar modules, and the same for each provider it
    pulled in), or by the best provider in the index; anything else is missing.
    Version ranges are not checked: the newest provider stands in for the range,
    which is what the corpus can offer.
    """
    root = declaration or hybrid_pack.read_declaration(jar)
    target = hybrid_pack.PackJar(jar, None, root)
    held = {target.mod_id.lower()} | {ident.lower() for ident in root.nested_ids}
    chosen: dict[Path, hybrid_pack.PackJar] = {}
    missing: dict[str, str] = {}
    queue = [target]
    while queue:
        current = queue.pop(0)
        for dependency in current.declaration.depends:
            key = dependency.lower()
            if key in hybrid_pack.PLATFORM_IDS or key in held:
                continue
            providers = [provider for provider in index.get(key, [])
                         if provider.path.resolve() != jar.resolve()
                         and provider.path not in chosen
                         and provider.mod_id.lower() not in held]
            if not providers:
                missing.setdefault(key, current.mod_id)
                continue
            provider = canonical_provider(providers)
            chosen[provider.path] = provider
            held.add(provider.mod_id.lower())
            held.update(ident.lower() for ident in provider.declaration.nested_ids)
            queue.append(provider)
    # An id first missed and then provided by a later provider's nested module
    # is not missing after all.
    missing = {key: by for key, by in missing.items() if key not in held}
    deps = sorted(chosen.values(), key=lambda item: item.name)
    return Closure(tuple(item.path for item in deps), tuple(item.mod_id for item in deps),
                   tuple(sorted(missing.items())))


def missing_dependency_row(jar: Path, closure: Closure) -> Row:
    """The row of a jar the ledger did not start: its closure cannot be completed."""
    ident, version = metadata(jar)
    named = ", ".join(key if by.lower() == ident.lower() else f"{key} (needed by {by})"
                      for key, by in closure.missing)
    return Row(jar.name, ident, version, "unresolved", "", "?", reason=bounded(
        f"missing-dependency: {named}", MAX_CAUSE), recommend=MISSING_DEPENDENCY,
        deps=closure.ids)


def read_runtime_report(home: Path) -> dict:
    """The role's runtime report, or an empty report when it wrote none."""
    try:
        report = json.loads((home / "java-runtime.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return report if isinstance(report, dict) else {}


def server_side_mixins(report: dict) -> dict[str, str]:
    """Each mixin class's physical side, from the jar's own configs.

    A class in a common `mixins` list applies on both sides; a class only in a
    `client` or `server` list applies there. The first config that names a class
    decides, matching the load-time gate's "first config that lists it" rule.
    """
    sides: dict[str, str] = {}
    configs = report.get("configs") if isinstance(report, dict) else None
    for config in configs if isinstance(configs, list) else ():
        if not isinstance(config, dict):
            continue
        common = {str(item) for item in config.get("mixins") or ()}
        server = common | {str(item) for item in config.get("server") or ()}
        client = common | {str(item) for item in config.get("client") or ()}
        for name in server:
            sides[name] = "both" if sides.get(name) == "client" else "server"
        for name in client:
            sides[name] = "both" if sides.get(name) == "server" else "client"
    return sides


def outside_plans(probe: Path | None, jar: Path, report: dict) -> int:
    """Plans on the server side whose target class the jar does not hold.

    That is the `target-outside-jar` share of the weave's skips. The game's log
    no longer lists each skip, so the plan report is the only place the count
    survives; a missing probe, an unreadable jar or unparseable JSON answers 0,
    and the label then falls back to whatever the runtime report did say.
    """
    if probe is None or not probe.is_file():
        return 0
    try:
        result = subprocess.run(
            [str(probe), "--json", "--mixin-plans", str(jar)],
            capture_output=True, text=True, timeout=120, check=False,
        )
        plans = json.loads(result.stdout).get("plans")
        names = set(zipfile.ZipFile(jar).namelist())
    except (OSError, ValueError, subprocess.SubprocessError, zipfile.BadZipFile):
        return 0
    if not isinstance(plans, list):
        return 0
    sides = server_side_mixins(report)
    count = 0
    for plan in plans:
        if not isinstance(plan, dict):
            continue
        callback = str(plan.get("callback", "")).split("#", 1)[0].replace("/", ".")
        target = str(plan.get("target", ""))
        if not target or sides.get(callback, "both") == "client":
            continue
        if f"{target}.class" not in names:
            count += 1
    return count


def parse_log(text: str, filename: str, *, seconds: float = 0.0, timed_out: bool = False,
              exit_code: int | None = None, jar: Path | None = None,
              runtime: dict | None = None, game_plans: int = 0) -> Row:
    """Extract the stable fields the dedicated role's log and runtime report carry.

    `runtime` is the jar's `java-runtime.json` row: a started jar's carries the
    engine, the weave's woven/skipped counts (which the log no longer prints) and
    its skips counted by reason; a refused jar's carries `engine = "refused"` and
    the runner's whole cause in `refusal`, which wins over the clipped log line.
    `game_plans` is the plan report's `target-outside-jar` count.
    """
    ident, version = "?", "?"
    engine = "?"
    match = re.search(r"engine=([^\s]+).*?\bid=([^\s]+)\s+version=([^\s]+)", text)
    if match:
        engine, ident, version = match.groups()
    else:
        match = re.search(r"\bid=([^\s]+)\s+version=([^\s]+).*?engine=([^\s]+)", text)
        if match:
            ident, version, engine = match.groups()
    if runtime is not None:
        engine = str(runtime.get("engine") or engine)

    woven = skipped = 0
    if runtime is not None:
        try:
            woven = int(runtime.get("woven") or 0)
            skipped = int(runtime.get("skipped") or 0)
        except (TypeError, ValueError):
            woven = skipped = 0
    else:
        counts = re.search(r"mixin weave:\s*(\d+)\s+woven,\s*(\d+)\s+skipped", text)
        if counts:
            woven, skipped = map(int, counts.groups())
        else:
            counts = re.search(r"(?:^|\s)mixin:\s*(\d+)\s+woven,\s*(\d+)\s+skipped", text, re.M)
            if counts:
                woven, skipped = map(int, counts.groups())

    skip_reasons = tuple(re.findall(
        r"\b(target-outside-jar|target-missing|game-class)\b", text, re.I
    ))
    by_reason = runtime.get("skipped_by_reason") if runtime is not None else None
    if isinstance(by_reason, dict) and not skip_reasons:
        # Every skipped plan, counted by its label: one entry per plan.
        skip_reasons = tuple(label for label, count in sorted(by_reason.items())
                             if isinstance(count, int) and count > 0
                             for _ in range(count))
    if runtime is not None and not skip_reasons:
        # An older report keeps only the first game-class skip, not the list.
        first = str(runtime.get("first_skip_reason") or "")
        label = first.split(" ", 1)[0] if first else ""
        if label in {"target-outside-jar", "target-missing", "game-class",
                     "unsupported-family", "game-hook-mismatch"}:
            skip_reasons = (label,)
    if jar is not None and ident == "?" and version == "?":
        # The started line names the engine only; the jar's metadata has the id.
        ident, version = metadata(jar)

    done = bool(re.search(r"Done \(", text))
    # A jar the batch did not start is named after `skipping`, and the vanilla
    # server can still reach Done: that line, not Done, is the row's outcome.
    refused_line = next((line for line in text.splitlines()
                         if "skipping " in line and filename in line), None)
    if runtime is not None and runtime.get("engine") not in (None, "refused"):
        # The runtime report says the jar started. A `skipping` line that names
        # it can only be another jar's refusal sharing the batch's one line
        # (`--with-deps` runs several jars), never this jar's.
        refused_line = None
    reason = ""
    refusal = runtime.get("refusal") if runtime is not None else None
    whole = isinstance(refusal, str) and bool(refusal.strip())
    if whole:
        # The runner's whole cause, not the log's clipped copy of it.
        reason = bounded(refusal, MAX_CAUSE)
        refused_line = refused_line or reason
    if refused_line and not reason:
        reason = bounded(refused_line.rsplit(f"{filename}:", 1)[-1].strip())
    if not reason:
        for pattern in (r"skipping .*?\.jar:\s*([^\n]+)", r"startup failed:\s*([^\n]+)",
                        r"refused:\s*([^\n]+)", r"Metadata\([^\n]+", r"expected a [^\n]+"):
            failure = re.search(pattern, text, re.I)
            if failure:
                reason = bounded(failure.group(1) if failure.lastindex else failure.group(0))
                break
    if not whole and not re.search(r"no native Fabric or NeoForge entrypoint|no native entrypoint",
                                   reason, re.I):
        entrypoint_line = next((line for line in text.splitlines()
                                if re.search(r"no native Fabric or NeoForge entrypoint"
                                             r"|no native entrypoint", line, re.I)), None)
        if entrypoint_line:
            reason = bounded(entrypoint_line.split("ERROR", 1)[-1].lstrip(" :"))
    if not reason:
        errors = [line for line in text.splitlines() if "ERROR" in line and ("Horizon:" in line or filename in line)]
        if errors:
            reason = bounded(errors[-1].split("ERROR", 1)[-1].lstrip(" :"))
    subtype = ""
    if done and not refused_line and engine != "refused":
        verdict = "ran"
        reason = ""
    else:
        if timed_out:
            reason = reason or "hard timeout"
        else:
            reason = reason or (f"exit code {exit_code}" if exit_code not in (None, 0)
                                else ("engine refused" if engine == "refused"
                                      else "server stopped before Done"))
        entrypoint_phrase = re.search(
            r"no native Fabric or NeoForge entrypoint|no native entrypoint"
            r"|clientSideOnly mod cannot run",
            reason + "\n" + text, re.I
        )
        entrypoint = re.search(
            r"no native Fabric or NeoForge entrypoint for server execution\s*"
            r"\(available:\s*([^)]*)\)", reason + "\n" + text, re.I
        )
        if entrypoint_phrase:
            # The logger bounds its line at 160 characters, which clips the
            # `(available: ...)` tail on long paths. The jar's own metadata is
            # authoritative; the log is the fallback for unreadable jars.
            entrypoints = read_jar_entrypoints(jar)
            subtype = skip_subtype(entrypoints)
            if not subtype and entrypoint:
                available = {item.strip() for item in entrypoint.group(1).split(",")}
                subtype = "client-only" if available & {"client", "modmenu"} else "library"
            if subtype:
                verdict = "skipped"
                if entrypoints is not None and "(available:" not in reason.lower():
                    reason = bounded(f"{reason} (available: {entrypoints.available})")
            else:
                verdict = "refused"
        else:
            verdict = "refused"
        subtype = subtype or ""
        if verdict == "refused" and engine == "?":
            engine = "refused"
        if verdict == "skipped" and engine == "?":
            # The engine never saw a server-role entrypoint, so say so rather
            # than leaving the row's engine unknown.
            engine = "refused"
    row = Row(filename, ident, version, verdict, subtype, engine, woven, skipped,
              reason, round(seconds, 3), skip_reasons, game_plans)
    return Row(filename, ident, version, verdict, subtype, engine, woven, skipped,
               reason, round(seconds, 3), skip_reasons, game_plans, recommendation(row))


LOADER_SKIP_REASONS = {"target-outside-jar", "target-missing", "game-class"}


def recommendation(row: Row) -> str:
    """Choose the next compatibility path from the stable server outcome."""
    if row.verdict == "unresolved":
        return MISSING_DEPENDENCY
    if row.engine.lower() in {"loader", "jvm", "knot"}:
        return NEEDS_LOADER
    if row.verdict == "skipped" and row.subtype in {"client-only", "library"}:
        return NATIVE_OK
    if row.verdict == "ran":
        loader_skips = row.game_plans or sum(
            reason.lower() in LOADER_SKIP_REASONS for reason in row.skip_reasons)
        if row.skipped and loader_skips * 2 >= row.skipped and loader_skips:
            return NEEDS_LOADER
        return NATIVE_OK
    return NATIVE_GAP


def label(row: Row) -> str:
    """The row's stored label, normalised; `recommendation` when it has none."""
    stored = LEGACY_LABELS.get(row.recommend, row.recommend)
    return stored if stored in LABELS else recommendation(row)


def own_reason(reason: str) -> str:
    """The cause the game names first.

    A jar refused both alone and with the pack class path carries both causes,
    `with the pack class path: <linked>; alone: <alone>`; the linked one is the
    blocker the batch hit, so the category reads that one only.
    """
    text = reason
    marker = "with the pack class path: "
    at = text.find(marker)
    if at >= 0:
        text = text[at + len(marker):]
        text = text.split("; alone: ", 1)[0]
    return text


def reason_category(row: Row) -> str:
    if row.verdict == "unresolved":
        return "missing-dependency"
    if row.verdict == "ran":
        return "none"
    if row.verdict == "skipped":
        # The server role had no entrypoint to execute.
        return "no-entrypoint"
    reason = own_reason(row.reason).lower()
    if "no native fabric or neoforge entrypoint" in reason or "no native entrypoint" in reason:
        return "no-entrypoint"
    if "timeout" in reason:
        return "timeout"
    if "metad" in reason:
        return "metadata"
    if "uncaught exception" in reason:
        return "uncaught"
    if "invalidclass" in reason or "unsupportedopcode" in reason or "stackunderflow" in reason:
        return "invalid-class"
    if "missingmethod" in reason or "nosuchmethod" in reason or "missing member" in reason:
        return "missing-member"
    if "startup failed" in reason:
        return "startup-failed"
    if "exit code" in reason:
        return "exit-code"
    return "other"


def summary(rows: list[Row]) -> dict[str, object]:
    categories = sorted({reason_category(row) for row in rows})
    recommendations = sorted({label(row) for row in rows}, key=LABELS.index)
    return {
        "total": len(rows),
        "ran": sum(row.verdict == "ran" for row in rows),
        "refused": sum(row.verdict == "refused" for row in rows),
        "skipped": sum(row.verdict == "skipped" for row in rows),
        "reason_categories": {category: sum(reason_category(row) == category for row in rows)
                              for category in categories},
        "recommendations": {value: sum(label(row) == value for row in rows)
                             for value in recommendations},
    }


# What each label asks a player or maintainer to do.
MEANING = {
    NATIVE_OK: "the dedicated server runs it on its own",
    NEEDS_LOADER: "it runs, but its plans target the game, so only a real loader applies them",
    NATIVE_GAP: "native execution cannot carry it yet",
    MISSING_DEPENDENCY: "it requires a mod no corpus jar provides, so no loader starts it; not run",
}

# The clause a row adds to its label's `why`, in the generated summary table.
CAUSE_TEXT = {
    (NATIVE_OK, "ran-clean"): "ran with no plan skipped",
    (NATIVE_OK, "ran"): "ran, every skipped plan stayed inside the jar",
    (NATIVE_OK, "client-only"): "client-only (the server role has no entrypoint to run)",
    (NATIVE_OK, "library"): "libraries (nothing for the server to execute)",
    (NEEDS_LOADER, "game-plans"): "ran, but their skipped plans target the game",
    (NEEDS_LOADER, "loader-engine"): "ran through the reference loader",
    (NATIVE_GAP, "metadata"): "refused while decoding metadata",
    (NATIVE_GAP, "no-entrypoint"): "refused before a server entrypoint matched",
    (NATIVE_GAP, "uncaught"): "refused by an uncaught exception in its entrypoint",
    (NATIVE_GAP, "invalid-class"): "refused by the verifier (opcode or stack)",
    (NATIVE_GAP, "missing-member"): "refused on a member it could not resolve",
    (NATIVE_GAP, "startup-failed"): "refused in the entrypoint",
    (NATIVE_GAP, "timeout"): "did not reach `Done (` in the time bound",
    (NATIVE_GAP, "exit-code"): "exited before `Done (`",
    (NATIVE_GAP, "other"): "refused for another cause",
    (NATIVE_GAP, "none"): "refused for another cause",
    (MISSING_DEPENDENCY, "missing-dependency"): "need an id outside the corpus",
}

# The per-jar table's short cause, before the ledger's own reason is appended.
GAP_TEXT = {
    "metadata": "refused while decoding metadata",
    "no-entrypoint": "refused before a server entrypoint matched",
    "uncaught": "refused by an uncaught exception",
    "invalid-class": "refused by the verifier",
    "missing-member": "refused on a missing member",
    "startup-failed": "refused in the entrypoint",
    "timeout": "no `Done (` before the time bound",
    "exit-code": "exited before `Done (`",
    "other": "refused",
    "none": "refused",
    "missing-dependency": "not run: its closure is incomplete",
}


def cause_key(row: Row) -> str:
    """The bucket a row contributes to in its label's `why`."""
    kind = label(row)
    if kind == NATIVE_OK:
        if row.verdict == "skipped" and row.subtype:
            return row.subtype
        return "ran" if row.skipped else "ran-clean"
    if kind == NEEDS_LOADER:
        return "game-plans" if row.verdict == "ran" and row.skipped else "loader-engine"
    return reason_category(row)


def why(row: Row) -> str:
    """One jar's cause, for the generated per-jar table."""
    kind = label(row)
    if kind == NEEDS_LOADER:
        return (f"ran; {row.skipped} plan(s) skipped, at least half aimed at the game"
                f" (`target-outside-jar`)"
                if row.verdict == "ran" and row.skipped
                else "ran through the reference loader")
    if kind == NATIVE_OK:
        if row.subtype == "client-only":
            return "client-only: no server-role entrypoint"
        if row.subtype == "library":
            return "library: nothing for the server to execute"
        return (f"ran; {row.skipped} plan(s) skipped, none aimed at the game"
                if row.skipped else "ran; no plan skipped")
    text = GAP_TEXT[reason_category(row)]
    if row.blocked_in:
        text += f" in `{row.blocked_in}`'s code"
    return text


def doc_reason(reason: str) -> str:
    """A ledger reason fit for a docs table: no local paths, no clipping noise."""
    text = re.sub(r"^[\s\]:,.…\-]+", "", reason)
    # The maintainer's corpus path is not the docs' business; the jar column names the jar.
    text = re.sub(r"(?:/[\w.+@-]+)+/[\w.+@-]*\.jar:?\s*", "", text)
    # `(entrypoint, class): cause` keeps the cause, not the entry the ledger already ran.
    text = re.sub(r"^\([^)]*\):?\s*", "", text)
    text = re.sub(r"\s{2,}", " ", text).strip()
    return text


def compat_table(rows: list[Row], *, source: str = "", per_jar_limit: int = 60) -> str:
    """The generated 'what runs native, what needs the loader, why' markdown block."""
    total = len(rows)
    causes: dict[str, dict[str, int]] = {kind: {} for kind in LABELS}
    for row in rows:
        bucket = causes[label(row)]
        bucket[cause_key(row)] = bucket.get(cause_key(row), 0) + 1
    counts = {kind: sum(causes[kind].values()) for kind in LABELS}
    head = ", ".join(f"{counts[kind]} `{kind}`" for kind in LABELS if counts[kind])
    where = f" from `{source}`" if source else ""
    lines = [
        f"_Generated by `tools/java_mod_server_ledger.py --docs docs/java-mod-compat-status.md`{where};"
        " edit the tool, not this block._",
        "",
        f"**{total} jar{'s' if total != 1 else ''}: {head}.**",
        "",
        "| Label | Jars | What it means | Why |",
        "|---|---:|---|---|",
    ]
    for kind in LABELS:
        if not counts[kind]:
            continue
        parts = [f"{count} {CAUSE_TEXT.get((kind, cause), cause)}"
                 for cause, count in sorted(causes[kind].items(), key=lambda item: (-item[1], item[0]))]
        lines.append(f"| `{kind}` | {counts[kind]} | {MEANING[kind]} | {'; '.join(parts)} |")
    rest = sorted((row for row in rows if label(row) != NATIVE_OK),
                  key=lambda row: (LABELS.index(label(row)), row.file))
    lines.extend(["", f"**Jars that are not `native-ok` ({len(rest)}).**", ""])
    if rest:
        lines.extend(["| Jar | Label | Why |", "|---|---|---|"])
        for row in rest[:per_jar_limit]:
            reason = bounded(doc_reason(row.reason), MAX_TABLE_REASON)
            if len(reason) < 12 or reason[0] in "([":
                # A path with no cause behind it: say what the ledger does know.
                reason = ""
            text = why(row)
            if reason:
                text += f": `{reason}`"
            elif label(row) == NATIVE_GAP:
                text += " (the ledger's line was clipped before the cause)"
            text = text.replace("|", "\\|")
            jar = row.file.replace("|", "\\|")
            lines.append(f"| `{jar}` | `{label(row)}` | {text} |")
        if len(rest) > per_jar_limit:
            lines.append("")
            lines.append(f"_{len(rest) - per_jar_limit} more row(s) are in the ledger, not this table._")
    else:
        lines.append("_None: every jar in this run is `native-ok`._")
    return "\n".join(lines) + "\n"


def write_docs(path: Path, block: str) -> bool:
    """Replace the marked block in `path`; True when the file changed."""
    text = path.read_text(encoding="utf-8")
    start = text.find(DOC_START)
    end = text.find(DOC_END)
    if start < 0 or end < start:
        raise ValueError(f"{path}: no {DOC_START} … {DOC_END} markers")
    updated = text[: start + len(DOC_START)] + "\n" + block.strip("\n") + "\n" + text[end:]
    if updated == text:
        return False
    path.write_text(updated, encoding="utf-8")
    return True


def read_ledger(path: Path) -> list[Row]:
    """Rows from an earlier `format_row` text ledger, labels normalised."""
    rows = []
    for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), 1):
        if not line.strip() or line.startswith("summary\t"):
            continue
        fields = line.split("\t")
        if len(fields) < 2:
            continue
        values = dict(field.partition("=")[::2] for field in fields[1:])
        stored = values.get("recommend", "")
        try:
            rows.append(Row(
                fields[0],
                values.get("id", "?"),
                values.get("version", "?"),
                values.get("verdict", "refused"),
                values.get("subtype", ""),
                values.get("engine", "?"),
                int(values.get("woven", "0") or 0),
                int(values.get("skipped", "0") or 0),
                values.get("reason", ""),
                float(values.get("seconds", "0") or 0),
                (),
                0,
                LEGACY_LABELS.get(stored, stored),
                id_list(values.get("deps", "")),
                id_list(values.get("dep_refused", "")),
                values.get("blocked_in", ""),
            ))
        except (ValueError, IndexError) as error:
            raise ValueError(f"{path}:{number}: not a ledger row ({error})") from error
    return rows


def id_list(value: str) -> tuple[str, ...]:
    """A `deps=`/`dep_refused=` field: comma-separated mod ids, empty for none."""
    return tuple(item for item in value.split(",") if item)


def _kill_process(proc: subprocess.Popen[bytes]) -> None:
    if proc.poll() is not None:
        return
    try:
        os.killpg(proc.pid, signal.SIGTERM)
        proc.wait(timeout=1.5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def closure_text(text: str, jar: Path, deps: tuple[Path, ...]) -> str:
    """The log lines that can speak for `jar`: drop those naming only its dependencies.

    One batch logs every jar it ran; a dependency's own refusal or entrypoint line
    must not become the jar under test's reason. A line naming the jar too (the
    batch's single `skipping a, b` line) stays.
    """
    if not deps:
        return text
    others = [dep.name for dep in deps if dep.name != jar.name]
    return "\n".join(line for line in text.splitlines()
                     if jar.name in line or not any(name in line for name in others))


# A refusal that only says the server role has nothing to execute: Fabric API, a
# config library with client entrypoints. A loader loads such a jar for its classes,
# so a dependency refused this way is not a dependency that failed.
NOTHING_TO_RUN = re.compile(r"no native (?:Fabric or NeoForge )?entrypoint|clientSideOnly mod cannot run",
                            re.I)


def dependency_refusals(report: dict, deps: tuple[Path, ...], ids: tuple[str, ...]) -> tuple[str, ...]:
    """The closure's mod ids whose entrypoint the server refused.

    A dependency with no server-role entrypoint is not counted: it is a library to
    the server, and its classes still reach the pack class path.
    """
    refused = []
    for dep, ident in zip(deps, ids):
        row = runtime_row(report, dep)
        if row is None or row.get("engine") != "refused":
            continue
        if NOTHING_TO_RUN.search(str(row.get("refusal") or "")):
            continue
        refused.append(ident)
    return tuple(refused)


def blocking_dependency(reason: str, jar: Path, deps: tuple[Path, ...],
                        ids: tuple[str, ...]) -> str:
    """The closure mod that holds the class a refusal stopped in, or "".

    The runner names where it stopped as `(at owner/Class#method +pc)`. A class the
    jar under test holds is its own refusal; a class only a dependency's jar holds
    means the jar got into that dependency's code and the blocker is there.
    """
    match = re.search(r"\(at ([\w/$]+)#", own_reason(reason))
    if not match:
        return ""
    entry = f"{match.group(1)}.class"

    def holds(path: Path) -> bool:
        try:
            with zipfile.ZipFile(path) as archive:
                return entry in archive.namelist()
        except (OSError, zipfile.BadZipFile):
            return False

    if holds(jar):
        return ""
    return next((ident for dep, ident in zip(deps, ids) if holds(dep)), "")


def run_one(binary: Path, resources: Path, jar: Path, *, timeout: float, port: int,
            probe: Path | None = None, closure: Closure | None = None) -> Row:
    """Start one dedicated server for `jar` (and its closure's jars) and read its row."""
    deps = closure.jars if closure is not None else ()
    with tempfile.TemporaryDirectory(prefix="horizon-ledger-") as scratch:
        root = Path(scratch)
        game_dir = root / "game"
        home = root / "home"
        game_dir.mkdir()
        home.mkdir()
        (game_dir / "eula.txt").write_text("eula=true\n", encoding="utf-8")
        (game_dir / "server.properties").write_text(
            f"level-type=minecraft:flat\nonline-mode=false\nserver-port={port}\n", encoding="utf-8"
        )
        # Each jar runs in its own scratch homes so nothing a real player keeps (logs, options, saves)
        # is read or written; lab.game_env also sets the audio and launcher switches every game run needs.
        env = lab.game_env(
            HORIZON_HOME=home,
            XDG_DATA_HOME=root / "xdg-data",
            XDG_CONFIG_HOME=root / "xdg-config",
        )
        # The jar under test comes first: the pack class path keeps the first
        # jar that declares a class, as a JVM class path does.
        mods = [arg for path in (jar, *deps) for arg in ("--java-mod", str(path))]
        command = [str(binary), "--resources", str(resources), "--game-dir", str(game_dir),
                   "--home", str(home), *mods, "--dedicated", "--nogui",
                   "--port", str(port), "--world", "ledger-world"]
        start = time.monotonic()
        log_path = root / "server.log"
        with log_path.open("wb") as log:
            proc = subprocess.Popen(command, cwd=game_dir, env=env, stdout=log,
                                    stderr=subprocess.STDOUT, start_new_session=True)
            timed_out = False
            deadline = start + timeout
            while proc.poll() is None:
                if time.monotonic() >= deadline:
                    timed_out = True
                    _kill_process(proc)
                    break
                try:
                    if b"Done (" in log_path.read_bytes()[-MAX_OUTPUT:]:
                        _kill_process(proc)
                        break
                except OSError:
                    pass
                time.sleep(0.05)
        try:
            with log_path.open("rb") as log:
                log.seek(0, os.SEEK_END)
                log.seek(max(0, log.tell() - MAX_OUTPUT))
                output = log.read(MAX_OUTPUT)
        except OSError:
            output = b""
        text = output.decode("utf-8", errors="replace")
        report = read_runtime_report(home)
        runtime = runtime_row(report, jar)
        # A refused jar ran no weave, so its plans say nothing about the label.
        started = runtime is not None and runtime.get("engine") != "refused"
        plans = outside_plans(probe, jar, report) if started else 0
        row = parse_log(closure_text(text, jar, deps), jar.name,
                        seconds=time.monotonic() - start, timed_out=timed_out,
                        exit_code=proc.returncode, jar=jar, runtime=runtime, game_plans=plans)
        if closure is None:
            return row
        blocked = (blocking_dependency(row.reason, jar, closure.jars, closure.ids)
                   if row.verdict == "refused" else "")
        return replace(row, deps=closure.ids,
                       dep_refused=dependency_refusals(report, closure.jars, closure.ids),
                       blocked_in=blocked)


def format_row(row: Row) -> str:
    values = [row.file, f"id={row.id}", f"version={row.version}", f"verdict={row.verdict}",
              f"subtype={row.subtype}",
              f"engine={row.engine}", f"woven={row.woven}", f"skipped={row.skipped}",
              f"reason={bounded(row.reason, MAX_CAUSE)}", f"recommend={label(row)}",
              f"seconds={row.seconds:.3f}"]
    # Only a `--with-deps` row names a closure, so a one-jar ledger keeps its format.
    if row.deps:
        values.append(f"deps={','.join(row.deps)}")
    if row.dep_refused:
        values.append(f"dep_refused={','.join(row.dep_refused)}")
    if row.blocked_in:
        values.append(f"blocked_in={row.blocked_in}")
    return "\t".join(value.replace("\t", " ") for value in values)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mods", nargs="?", type=Path, help="directory containing mod jars")
    parser.add_argument("--binary", type=Path, default=Path("target/fast/horizon-client"))
    parser.add_argument("--native-binary", type=Path,
                        default=Path("target/fast/horizon-native-java"),
                        help="plan probe used for the target-outside-jar count")
    parser.add_argument("--resources", type=Path, default=Path(".local/horizon-client/26.3"))
    parser.add_argument("--limit", type=int, default=0, help="maximum jars (0 means all)")
    parser.add_argument("--only", help="only filenames containing this substring")
    parser.add_argument("--timeout", type=float, default=45.0, help="seconds per jar")
    parser.add_argument("--port-base", type=int, default=28000)
    parser.add_argument("--json", action="store_true", dest="json_output")
    parser.add_argument("--output", type=Path, help="also write the text ledger here")
    parser.add_argument("--from", type=Path, dest="reuse",
                        help="read an earlier text ledger instead of starting servers")
    parser.add_argument("--table", action="store_true", help="print the label table as markdown")
    parser.add_argument("--docs", type=Path,
                        help="rewrite the marked table block of this markdown file")
    parser.add_argument("--table-limit", type=int, default=60,
                        help="per-jar rows in the table (0 means all)")
    parser.add_argument("--with-deps", action="store_true",
                        help="run each jar with its declared dependency closure")
    parser.add_argument("--deps-dir", type=Path, action="append", default=None,
                        help="where --with-deps looks for providers besides the mod directory "
                             "(repeatable; default: the ~/horizon-corpus corpora that exist)")
    parser.add_argument("--rerun", type=Path,
                        help="only the jars this earlier text ledger labelled --rerun-label")
    parser.add_argument("--rerun-label", default=NATIVE_GAP, choices=LABELS)
    args = parser.parse_args(argv)

    if args.reuse is not None:
        if not args.reuse.is_file():
            parser.error(f"ledger does not exist: {args.reuse}")
        try:
            rows = read_ledger(args.reuse)
        except ValueError as error:
            parser.error(str(error))
        if not rows:
            parser.error(f"no rows in {args.reuse}")
        source = str(args.reuse)
    else:
        if args.mods is None:
            parser.error("a mod directory, or --from PATH")
        args.binary = args.binary.resolve()
        args.resources = args.resources.resolve()
        args.native_binary = args.native_binary.resolve()
        if not args.mods.is_dir():
            parser.error(f"mod directory does not exist: {args.mods}")
        jars = sorted((p for p in args.mods.iterdir() if p.suffix.lower() == ".jar"),
                      key=lambda p: p.name)
        if args.only:
            jars = [p for p in jars if args.only in p.name or args.only in metadata(p)[0]]
        if args.rerun is not None:
            if not args.rerun.is_file():
                parser.error(f"ledger does not exist: {args.rerun}")
            try:
                wanted = {row.file for row in read_ledger(args.rerun)
                          if label(row) == args.rerun_label}
            except ValueError as error:
                parser.error(str(error))
            jars = [p for p in jars if p.name in wanted]
        if args.limit:
            jars = jars[: max(0, args.limit)]
        if not jars:
            parser.error("no matching .jar files")
        if not args.binary.is_file():
            parser.error(f"binary does not exist: {args.binary}")
        if not args.resources.is_dir():
            parser.error(f"resources directory does not exist: {args.resources}")
        if not args.native_binary.is_file():
            print(f"warning: plan probe missing ({args.native_binary}); "
                  "target-outside-jar counts fall back to the runtime report",
                  file=sys.stderr, flush=True)

        index_by_id: dict[str, list[hybrid_pack.PackJar]] = {}
        if args.with_deps:
            # The mod directory always provides: a jar's dependency is often its neighbour.
            pools = list(dict.fromkeys(
                path.resolve() for path in [args.mods, *(args.deps_dir or DEFAULT_DEP_DIRS)]))
            pool_jars = corpus_jars(pools)
            index_by_id = provider_index(pool_jars)
            print(f"dependency providers: {len(pool_jars)} jar(s) from "
                  f"{', '.join(str(pool) for pool in pools if pool.is_dir())}",
                  file=sys.stderr, flush=True)

        rows = []
        for index, jar in enumerate(jars):
            closure = dependency_closure(jar, index_by_id) if args.with_deps else None
            note = ""
            if closure is not None:
                note = (f" (missing: {', '.join(key for key, _ in closure.missing)})"
                        if closure.missing else
                        f" (+ {', '.join(closure.ids)})" if closure.ids else "")
            print(f"[{index + 1}/{len(jars)}] {jar.name}{note}", file=sys.stderr, flush=True)
            if closure is not None and closure.missing:
                # A loader refuses a jar whose requirement nothing provides, so
                # there is nothing native execution could be blamed for.
                rows.append(missing_dependency_row(jar, closure))
                continue
            rows.append(run_one(args.binary, args.resources, jar, timeout=args.timeout,
                                port=args.port_base + index,
                                probe=args.native_binary if args.native_binary.is_file() else None,
                                closure=closure))
        source = str(args.mods) + (" --with-deps" if args.with_deps else "")

    rows.sort(key=lambda row: row.file)
    counts = summary(rows)
    summary_fields = [f"{key}={value}" for key, value in counts.items()
                      if key not in {"reason_categories", "recommendations"}]
    summary_fields.extend(f"recommend_{key}={value}"
                          for key, value in counts["recommendations"].items())
    summary_fields.extend(f"reason_{key}={value}" for key, value in counts["reason_categories"].items())
    text = "\n".join([format_row(row) for row in rows] + ["summary\t" + "\t".join(summary_fields)]) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text, encoding="utf-8")
    markdown = compat_table(rows, source=source,
                            per_jar_limit=args.table_limit if args.table_limit else len(rows) + 1)
    if args.table:
        print(markdown, end="")
    if args.docs:
        try:
            changed = write_docs(args.docs, markdown)
        except (OSError, ValueError) as error:
            parser.error(str(error))
        print(f"{'updated' if changed else 'unchanged'}: {args.docs}", file=sys.stderr)
    if args.json_output:
        print(json.dumps({"rows": [asdict(row) for row in rows], "summary": counts},
                         sort_keys=True))
    elif not (args.table or args.docs):
        print(text, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
