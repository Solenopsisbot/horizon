import contextlib
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).parents[1]
_SPEC = importlib.util.spec_from_file_location(
    "java_mod_server_ledger_tool", ROOT / "tools" / "java_mod_server_ledger.py"
)
LEDGER = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = LEDGER
# The tool imports `lab` by putting its own folder on sys.path; restore the
# path afterwards so unittest discovery keeps resolving tests from `tests/`.
_before = list(sys.path)
_SPEC.loader.exec_module(LEDGER)
sys.path[:] = _before


class JavaModServerLedgerTests(unittest.TestCase):
    @staticmethod
    def _jar(directory, name, *, fabric=None, neoforge=None, mod_class=False,
             fabric_raw=None, corrupt=False):
        path = Path(directory) / name
        if corrupt:
            path.write_bytes(b"not a zip")
            return path
        with zipfile.ZipFile(path, "w") as jar:
            if fabric_raw is not None:
                jar.writestr("fabric.mod.json", fabric_raw)
            elif fabric is not None:
                jar.writestr("fabric.mod.json", json.dumps(fabric))
            if neoforge is not None:
                jar.writestr("META-INF/neoforge.mods.toml", neoforge)
            if mod_class:
                jar.writestr(
                    "example/ExampleMod.class",
                    b"\xca\xfe\xba\xbeLnet/neoforged/fml/common/Mod;",
                )
        return path

    @staticmethod
    def _clipped_log(name):
        # The dedicated logger truncates at 160 characters, so the
        # `(available: ...)` tail never reaches the ledger.
        return (
            f"engine=refused id={name} version=1\n"
            f"Horizon: ERROR skipping /mods/{name}.jar: /mods/{name}.jar: "
            "no native Fabric or NeoForge entrypoint for server executio...\n"
        )

    def test_parse_success_and_counts(self):
        log = (
            "Horizon: dedicated Java compatibility: Horizon: Java compatibility /mods/apple.jar: "
            "role=dedicated-server engine=native-qualified loader=fabric id=appleskin version=3.0\n"
            "Horizon: Java compatibility: mixin weave: 0 woven, 11 skipped across 1 jar(s)\n"
            "Done (1.234s)! For help, type 'help'"
        )
        row = LEDGER.parse_log(log, "apple.jar", seconds=2.5)
        self.assertEqual((row.verdict, row.engine, row.id, row.version), ("ran", "native-qualified", "appleskin", "3.0"))
        self.assertEqual((row.woven, row.skipped), (0, 11))

    def test_parse_refusal_bounds_reason(self):
        row = LEDGER.parse_log(
            "Horizon: dedicated Java compatibility: ... engine=refused loader=fabric id=cupboard version=1.2\n"
            "dedicated Java mod startup failed: " + "x" * 1000,
            "cupboard.jar", seconds=1.0, exit_code=1)
        self.assertEqual(row.verdict, "refused")
        self.assertEqual(row.engine, "refused")
        self.assertLessEqual(len(row.reason), LEDGER.MAX_REASON)

    def test_timeout_and_summary(self):
        row = LEDGER.parse_log("starting", "slow.jar", timed_out=True, seconds=45.0)
        self.assertEqual(row.verdict, "refused")
        self.assertEqual(LEDGER.summary([row, LEDGER.Row("ok.jar", verdict="ran")]), {
            "total": 2, "ran": 1, "refused": 1, "skipped": 0,
            "reason_categories": {"none": 1, "timeout": 1},
            "recommendations": {"native-ok": 1, "native-gap": 1},
        })

    def test_parse_entrypoint_refusals_are_skipped_by_subtype(self):
        client = LEDGER.parse_log(
            "engine=refused id=visuality version=1\n"
            "refused: /mods/visuality.jar: no native Fabric or NeoForge entrypoint "
            "for server execution (available: client, modmenu)",
            "visuality.jar",
        )
        library = LEDGER.parse_log(
            "engine=refused id=ferritecore version=1\n"
            "refused: /mods/ferritecore.jar: no native Fabric or NeoForge entrypoint "
            "for server execution (available: none)",
            "ferritecore.jar",
        )
        self.assertEqual((client.verdict, client.subtype), ("skipped", "client-only"))
        self.assertEqual((library.verdict, library.subtype), ("skipped", "library"))
        self.assertIn("available: none", library.reason)

    def test_refused_engine_is_not_success_when_server_reaches_done(self):
        row = LEDGER.parse_log(
            "engine=refused id=ferritecore version=1\n"
            "ERROR: skipping mod: no native Fabric or NeoForge entrypoint for server execution (available: none)\n"
            "Done (1s)",
            "ferritecore.jar",
        )
        self.assertEqual((row.verdict, row.subtype), ("skipped", "library"))

    def test_summary_reason_categories_are_deterministic(self):
        rows = [
            LEDGER.parse_log("Done (1s)", "a.jar"),
            LEDGER.parse_log("engine=refused id=b version=1\nrefused: InvalidClass(x)", "b.jar"),
            LEDGER.parse_log("engine=refused id=c version=1\nrefused: no native Fabric or NeoForge entrypoint for server execution (available: preLaunch)", "c.jar"),
        ]
        self.assertEqual(LEDGER.summary(rows)["reason_categories"], {
            "invalid-class": 1, "no-entrypoint": 1, "none": 1,
        })

    def test_recommendation_classifies_native_loader_and_gap(self):
        ran = LEDGER.parse_log(
            "engine=native-qualified id=gamefix version=1\n"
            "mixin weave: 1 woven, 3 skipped; skipped Mixin work does not run: "
            "[target-outside-jar HEAD net/minecraft/server/level/ServerPlayer#tick, "
            "target-outside-jar HEAD net/minecraft/world/level/Level#tick, "
            "unsupported-family INVOKE demo/Target#run]\nDone (1s)",
            "gamefix.jar",
        )
        client = LEDGER.Row("client.jar", verdict="skipped", subtype="client-only", engine="refused")
        refused = LEDGER.Row("broken.jar", verdict="refused", engine="refused", reason="Metadata(bad)")
        self.assertEqual(ran.recommend, "needs-loader")
        self.assertEqual(LEDGER.recommendation(client), "native-ok")
        self.assertEqual(LEDGER.recommendation(refused), "native-gap")

    def test_runtime_refusal_is_the_whole_cause_and_survives_the_text_ledger(self):
        cause = ("uncaught exception java/lang/NullPointerException: Tried to register a listener "
                 "for a null phase (at " + "com/example/collective/events/" * 6 + "Listener#init)")
        self.assertGreater(len(cause), 160)
        # The log line stops at 160 characters; the runtime row does not.
        log = (f"Horizon: dedicated Java compatibility: skipping /mods/collective.jar: "
               f"{cause[:120]}...\nDone (1s)")
        runtime = {"file": "collective.jar", "engine": "refused", "loader": "unknown",
                   "woven": 0, "skipped": 0, "registrations": 0, "opaque": 0, "refusal": cause}
        row = LEDGER.parse_log(log, "collective.jar", runtime=runtime)
        self.assertEqual((row.verdict, row.engine, row.recommend), ("refused", "refused", "native-gap"))
        self.assertEqual(row.reason, cause)
        with tempfile.TemporaryDirectory() as scratch:
            ledger = Path(scratch) / "ledger.txt"
            ledger.write_text(LEDGER.format_row(row) + "\n", encoding="utf-8")
            [back] = LEDGER.read_ledger(ledger)
        self.assertEqual(back.reason, cause)
        self.assertEqual(back.recommend, "native-gap")

    def test_runtime_refusal_wins_over_a_bare_server_done(self):
        # The vanilla server reaches Done without the jar: the row is the refusal.
        runtime = {"file": "balm.jar", "engine": "refused", "refusal": "Metadata(\"bad range\")"}
        row = LEDGER.parse_log("Done (1s)", "balm.jar", runtime=runtime)
        self.assertEqual((row.verdict, row.reason), ("refused", 'Metadata("bad range")'))

    def test_runtime_counts_by_reason_label_a_jar_needs_loader(self):
        # No skip list in the log and no first reason: the counts alone decide.
        runtime = {"file": "gamefix.jar", "engine": "native-qualified", "woven": 1, "skipped": 4,
                   "skipped_by_reason": {"target-outside-jar": 3, "cancellable": 1},
                   "plans": [{"mixin": "demo/M", "target": "net/minecraft/A", "injector": "HEAD",
                              "reason": "target-outside-jar"}]}
        row = LEDGER.parse_log("Done (1s)", "gamefix.jar", runtime=runtime)
        self.assertEqual((row.verdict, row.skipped), ("ran", 4))
        self.assertEqual(row.recommend, "needs-loader")
        own = dict(runtime, skipped_by_reason={"cancellable": 4})
        self.assertEqual(LEDGER.parse_log("Done (1s)", "own.jar", runtime=own).recommend, "native-ok")

    def test_recommendation_prefers_loader_engine(self):
        row = LEDGER.Row("knot.jar", verdict="refused", engine="knot")
        self.assertEqual(LEDGER.recommendation(row), "needs-loader")

    def test_clipped_client_only_jar_is_skipped_from_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "visuality.jar",
                fabric={
                    "id": "visuality",
                    "version": "1",
                    "entrypoints": {"client": ["a.Client"], "modmenu": ["b.Menu"]},
                },
            )
            row = LEDGER.parse_log(self._clipped_log("visuality"), "visuality.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))
        self.assertIn("available: client, modmenu", row.reason)

    def test_clipped_client_hook_kinds_are_client_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "reeses.jar",
                fabric={
                    "id": "reeses-sodium-options",
                    "version": "2",
                    "entrypoints": {
                        "controlify": ["a.Controlify"],
                        "sodium:config_api_user": ["a.Sodium"],
                    },
                },
            )
            row = LEDGER.parse_log(self._clipped_log("reeses"), "reeses.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))

    def test_clipped_library_jar_is_skipped_from_metadata(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "ferritecore.jar",
                fabric={"id": "ferritecore", "version": "1"},
            )
            row = LEDGER.parse_log(self._clipped_log("ferritecore"), "ferritecore.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "library"))
        self.assertIn("available: none", row.reason)

    def test_server_entrypoint_that_failed_stays_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "broken.jar",
                fabric={"id": "broken", "version": "1", "entrypoints": {"main": ["a.Main"]}},
            )
            row = LEDGER.parse_log(
                "engine=refused id=broken version=1\n"
                "Horizon: ERROR skipping /mods/broken.jar: /mods/broken.jar: "
                "uncaught exception java/lang/AssertionError (at a/Main.java)",
                "broken.jar", jar=jar, exit_code=1,
            )
            # Even a clipped no-entrypoint line cannot override the jar's own
            # server-side entrypoint.
            contradicted = LEDGER.parse_log(
                self._clipped_log("broken"), "broken.jar", jar=jar)
        self.assertEqual(row.verdict, "refused")
        self.assertEqual(contradicted.verdict, "refused")

    def test_neoforge_metadata_classification(self):
        with tempfile.TemporaryDirectory() as tmp:
            client = self._jar(
                tmp, "client.jar",
                neoforge='modLoader = "javafml"\n[[mods]]\nmodId = "client"\nclientSideOnly = true\n',
            )
            server = self._jar(
                tmp, "server.jar",
                neoforge='modLoader = "javafml"\n[[mods]]\nmodId = "server"\n',
                mod_class=True,
            )
            client_row = LEDGER.parse_log(self._clipped_log("client"), "client.jar", jar=client)
            server_row = LEDGER.parse_log(self._clipped_log("server"), "server.jar", jar=server)
        self.assertEqual((client_row.verdict, client_row.subtype), ("skipped", "client-only"))
        self.assertEqual(server_row.verdict, "refused")

    def test_fabric_client_environment_skips_a_multi_loader_jar(self):
        # A Fabric `"environment": "client"` jar that also carries a NeoForge
        # `@Mod` class is client-only on a server: the native reader refuses it
        # with the clientSideOnly line, and the row is a client-only skip.
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "multi.jar",
                fabric={"schemaVersion": 1, "id": "multi", "environment": "client",
                        "entrypoints": {"client": ["demo.Client"]}},
                neoforge='modLoader = "javafml"\n[[mods]]\nmodId = "multi"\n',
                mod_class=True,
            )
            row = LEDGER.parse_log(
                "[Server thread/WARN]: Horizon: dedicated Java compatibility: skipping "
                "/mods/multi.jar: /mods/multi.jar: clientSideOnly mod cannot run for server "
                "execution\nDone (1.0s)!",
                "multi.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))

    def test_a_universal_jar_with_only_client_fabric_entrypoints_is_client_only(self):
        # zoomify's shape: Fabric declares only `client`, the jar also ships a
        # NeoForge `@Mod` class. Fabric Loader never reads `@Mod`, so a server
        # has nothing to run and the native path says so.
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "universal.jar",
                fabric={"schemaVersion": 1, "id": "universal",
                        "entrypoints": {"client": ["demo.Client"]}},
                neoforge='modLoader = "javafml"\n[[mods]]\nmodId = "universal"\n',
                mod_class=True,
            )
            row = LEDGER.parse_log(
                "[Server thread/WARN]: Horizon: dedicated Java compatibility: skipping "
                "/mods/universal.jar: /mods/universal.jar: no native entrypoint\nDone (1.0s)!",
                "universal.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))

    def test_fabric_metadata_with_a_byte_order_mark_is_read(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._jar(
                tmp, "bom.jar",
                fabric_raw="\ufeff" + json.dumps(
                    {"schemaVersion": 1, "id": "bom", "environment": "client",
                     "entrypoints": {"main": [], "client": []}}),
            )
            row = LEDGER.parse_log(
                "skipping /mods/bom.jar: /mods/bom.jar: clientSideOnly mod cannot run for server "
                "execution\nDone (1.0s)!",
                "bom.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))

    def test_unreadable_jar_keeps_log_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            corrupt = self._jar(tmp, "corrupt.jar", corrupt=True)
            clipped = LEDGER.parse_log(
                self._clipped_log("corrupt"), "corrupt.jar", jar=corrupt)
            log_available = LEDGER.parse_log(
                "refused: /mods/corrupt.jar: no native Fabric or NeoForge entrypoint "
                "for server execution (available: client, modmenu)",
                "corrupt.jar", jar=corrupt,
            )
        # No `(available: ...)` in the log and no readable metadata: the old
        # refusal stands rather than guessing a skip.
        self.assertEqual(clipped.verdict, "refused")
        self.assertEqual((log_available.verdict, log_available.subtype),
                         ("skipped", "client-only"))

    def test_metadata_reader_tolerates_raw_control_characters(self):
        with tempfile.TemporaryDirectory() as tmp:
            raw = (
                '{\n  "id": "etf",\n  "version": "7",\n'
                '  "description": "line one\nline two",\n'
                '  "entrypoints": {"client": ["a.Client"], "modmenu": ["b.Menu"]}\n}'
            )
            jar = self._jar(tmp, "etf.jar", fabric_raw=raw)
            row = LEDGER.parse_log(self._clipped_log("etf"), "etf.jar", jar=jar)
        self.assertEqual((row.verdict, row.subtype), ("skipped", "client-only"))

    def test_integration_requires_prepared_resources(self):
        resources = ROOT / ".local/horizon-client/26.3"
        if os.environ.get("HORIZON_LEDGER_INTEGRATION") != "1" or not resources.is_dir():
            self.skipTest("real-server integration is opt-in and needs prepared resources")
        self.assertTrue(resources.is_dir())


class DependencyClosureTests(unittest.TestCase):
    """`--with-deps`: which jars run beside a jar, and whose outcome the row is."""

    @staticmethod
    def _fabric(directory, name, ident, *, depends=(), provides=(), nested=(),
                version="1.0.0"):
        """A Fabric jar; `nested` are (id, provides) pairs carried as jar-in-jar."""
        path = Path(directory) / name
        meta = {"schemaVersion": 1, "id": ident, "version": version,
                "depends": {dep: "*" for dep in depends}}
        if provides:
            meta["provides"] = list(provides)
        with zipfile.ZipFile(path, "w") as jar:
            jar.writestr("fabric.mod.json", json.dumps(meta))
            for index, (inner, inner_provides) in enumerate(nested):
                buffer = io.BytesIO()
                with zipfile.ZipFile(buffer, "w") as nested_jar:
                    nested_jar.writestr("fabric.mod.json", json.dumps(
                        {"schemaVersion": 1, "id": inner, "version": "1",
                         "provides": list(inner_provides)}))
                jar.writestr(f"META-INF/jars/inner{index}.jar", buffer.getvalue())
        return path

    def _closure(self, directory, jar):
        index = LEDGER.provider_index(LEDGER.corpus_jars([Path(directory)]))
        return LEDGER.dependency_closure(jar, index)

    def test_closure_resolves_aliases_and_nested_modules_transitively(self):
        with tempfile.TemporaryDirectory() as tmp:
            # spells needs a config library under its alias, and an API module
            # only the API jar carries nested; the config library needs the API.
            jar = self._fabric(tmp, "spells.jar", "spells",
                               depends=("minecraft", "fabricloader", "cloth-config2",
                                        "fabric-networking-api-v1"))
            self._fabric(tmp, "cloth.jar", "cloth-config", provides=("cloth-config2",),
                         depends=("fabric-api",))
            self._fabric(tmp, "fabric-api.jar", "fabric-api",
                         nested=(("fabric-networking-api-v1", ()),))
            self._fabric(tmp, "unrelated.jar", "unrelated")
            closure = self._closure(tmp, jar)
        self.assertEqual(sorted(closure.ids), ["cloth-config", "fabric-api"])
        self.assertEqual(sorted(path.name for path in closure.jars),
                         ["cloth.jar", "fabric-api.jar"])
        self.assertEqual(closure.missing, ())

    def test_a_jars_own_nested_module_satisfies_its_dependency(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._fabric(tmp, "bundler.jar", "bundler", depends=("tinylib",),
                               nested=(("tinylib", ()),))
            self._fabric(tmp, "tinylib.jar", "tinylib")
            closure = self._closure(tmp, jar)
        self.assertEqual((closure.jars, closure.missing), ((), ()))

    def test_the_library_beats_a_mod_that_bundles_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._fabric(tmp, "app.jar", "app", depends=("fabric-api-base",))
            # The bundler nests the whole API (and so more ids than the API jar).
            self._fabric(tmp, "bundler.jar", "lod", nested=(
                ("fabric-api", ()), ("fabric-api-base", ()), ("fabric-extra", ())))
            self._fabric(tmp, "fabric-api.jar", "fabric-api",
                         nested=(("fabric-api-base", ()),))
            closure = self._closure(tmp, jar)
        self.assertEqual(closure.ids, ("fabric-api",))

    def test_missing_ids_name_who_needed_them_and_do_not_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = self._fabric(tmp, "archers.jar", "archers",
                               depends=("spell_engine", "absent_lib"))
            self._fabric(tmp, "spell_engine.jar", "spell_engine", depends=("structure_pool_api",))
            closure = self._closure(tmp, jar)
            row = LEDGER.missing_dependency_row(jar, closure)
            # No server starts: a fake binary and an empty resources folder are enough.
            binary = Path(tmp) / "not-a-game"
            binary.write_text("")
            resources = Path(tmp) / "resources"
            resources.mkdir()
            output = Path(tmp) / "ledger.txt"
            with contextlib.redirect_stdout(io.StringIO()), \
                    contextlib.redirect_stderr(io.StringIO()):
                LEDGER.main([tmp, "--only", "archers", "--with-deps", "--deps-dir", tmp,
                             "--binary", str(binary), "--resources", str(resources),
                             "--native-binary", str(binary), "--output", str(output)])
            stored = LEDGER.read_ledger(output)
        self.assertEqual(closure.ids, ("spell_engine",))
        self.assertEqual(closure.missing, (("absent_lib", "archers"),
                                           ("structure_pool_api", "spell_engine")))
        self.assertEqual(LEDGER.label(row), LEDGER.MISSING_DEPENDENCY)
        self.assertEqual(LEDGER.reason_category(row), "missing-dependency")
        self.assertEqual(
            row.reason,
            "missing-dependency: absent_lib, structure_pool_api (needed by spell_engine)")
        self.assertEqual(len(stored), 1)
        self.assertEqual((stored[0].file, LEDGER.label(stored[0]), stored[0].reason),
                         ("archers.jar", LEDGER.MISSING_DEPENDENCY, row.reason))
        self.assertEqual(stored[0].deps, ("spell_engine",))

    def test_neoforge_mandatory_dependencies_join_the_closure(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = Path(tmp) / "neo.jar"
            with zipfile.ZipFile(jar, "w") as archive:
                archive.writestr("META-INF/neoforge.mods.toml", (
                    '[[mods]]\nmodId="neo"\nversion="1"\n'
                    '[[dependencies.neo]]\nmodId="neoforge"\ntype="required"\n'
                    '[[dependencies.neo]]\nmodId="geckolib"\ntype="required"\n'
                    '[[dependencies.neo]]\nmodId="jei"\ntype="optional"\n'))
            self._fabric(tmp, "gecko.jar", "geckolib")
            self._fabric(tmp, "jei.jar", "jei")
            closure = self._closure(tmp, jar)
        self.assertEqual((closure.ids, closure.missing), (("geckolib",), ()))

    def test_the_row_is_the_jar_under_test_not_its_dependency(self):
        deps = (Path("/mods/lib-2.jar"),)
        log = (
            "Horizon: dedicated Java compatibility: /mods/app-2.jar: engine=native "
            "loader=fabric id=app version=1\n"
            "Horizon: ERROR /mods/lib-2.jar: no native Fabric or NeoForge entrypoint "
            "for server execution (available: client)\n"
            "[Server thread/WARN]: Horizon: dedicated Java compatibility: skipping "
            "/mods/lib-2.jar: clientSideOnly mod cannot run for server execution\n"
            "Done (1.0s)!\n"
        )
        text = LEDGER.closure_text(log, Path("/mods/app-2.jar"), deps)
        self.assertNotIn("lib-2.jar", text)
        row = LEDGER.parse_log(text, "app-2.jar", runtime={"file": "app-2.jar", "engine": "native"})
        self.assertEqual((row.verdict, LEDGER.label(row)), ("ran", LEDGER.NATIVE_OK))
        # The batch's one `skipping` line can name the jar's file as a substring of
        # a dependency's; a jar the runtime report says started is not refused.
        joined = "skipping /mods/xapp-2.jar: no native entrypoint\nDone (1.0s)!\n"
        row = LEDGER.parse_log(joined, "app-2.jar",
                               runtime={"file": "app-2.jar", "engine": "native"})
        self.assertEqual(row.verdict, "ran")
        report = {"jars": [{"file": "app-2.jar", "engine": "native"},
                           {"file": "lib-2.jar", "engine": "refused", "refusal": "x"}]}
        self.assertEqual(LEDGER.dependency_refusals(report, deps, ("lib",)), ("lib",))
        # A library dependency (Fabric API) has nothing for the server to run: the
        # runner refuses its entrypoint by design, which is not a failed dependency.
        report["jars"][1]["refusal"] = "no native entrypoint"
        self.assertEqual(LEDGER.dependency_refusals(report, deps, ("lib",)), ())

    def test_a_refusal_inside_a_dependency_names_that_dependency(self):
        with tempfile.TemporaryDirectory() as tmp:
            jar = Path(tmp) / "wizards.jar"
            dep = Path(tmp) / "spell_power.jar"
            for path, name in ((jar, "net/wizards/Entities.class"),
                               (dep, "net/spell_power/api/SpellSchools.class")):
                with zipfile.ZipFile(path, "w") as archive:
                    archive.writestr(name, b"\xca\xfe\xba\xbe")
            linked = ('with the pack class path: Metadata("malformed floating-point marker '
                      '(at net/spell_power/api/SpellSchools#createMagic +21)"); alone: '
                      'uncaught exception java/lang/NullPointerException (at net/wizards/Entities#a +7)')
            own = "uncaught exception java/lang/NullPointerException (at net/wizards/Entities#a +7)"
            self.assertEqual(
                LEDGER.blocking_dependency(linked, jar, (dep,), ("spell_power",)), "spell_power")
            self.assertEqual(LEDGER.blocking_dependency(own, jar, (dep,), ("spell_power",)), "")
            self.assertEqual(LEDGER.blocking_dependency("hard timeout", jar, (dep,), ("x",)), "")
        row = LEDGER.Row("wizards.jar", reason=linked, blocked_in="spell_power")
        self.assertTrue(LEDGER.why(row).endswith("in `spell_power`'s code"))

    def test_a_both_ways_refusal_is_categorised_by_its_linked_cause(self):
        reason = ("with the pack class path: uncaught exception java/lang/NullPointerException"
                  " (at a/B); alone: Metadata(\"missing member lib/Config.name\")")
        row = LEDGER.parse_log("Done (1s)!\n", "app.jar",
                               runtime={"file": "app.jar", "engine": "refused", "refusal": reason})
        self.assertEqual(LEDGER.reason_category(row), "uncaught")

    def test_closure_fields_survive_the_text_ledger(self):
        row = LEDGER.Row("app.jar", "app", "1", "refused", engine="refused",
                         deps=("fabric-api", "lib"), dep_refused=("lib",), blocked_in="lib")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "ledger.txt"
            path.write_text(LEDGER.format_row(row) + "\n", encoding="utf-8")
            stored = LEDGER.read_ledger(path)[0]
        self.assertEqual((stored.deps, stored.dep_refused, stored.blocked_in),
                         (("fabric-api", "lib"), ("lib",), "lib"))
        # A one-jar row keeps the old format: no closure fields at all.
        self.assertNotIn("deps=", LEDGER.format_row(LEDGER.Row("solo.jar")))


    @staticmethod
    def _ledger_row(file, verdict, recommend, reason=""):
        return LEDGER.Row(file, "id", "1", verdict, engine="native" if verdict == "ran" else "refused",
                          reason=reason, recommend=recommend)

    def test_a_started_jar_turned_native_gap_is_a_regression(self):
        before = [self._ledger_row("ok.jar", "ran", "native-ok"),
                  self._ledger_row("loader.jar", "ran", "needs-loader"),
                  self._ledger_row("moved.jar", "ran", "native-ok"),
                  self._ledger_row("fixed.jar", "refused", "native-gap"),
                  self._ledger_row("absent.jar", "refused", "missing-dependency")]
        after = [self._ledger_row("ok.jar", "refused", "native-gap", "uncaught exception NPE"),
                 self._ledger_row("loader.jar", "refused", "native-gap", "Metadata(x)"),
                 # Between the two started labels only the plan count moved.
                 self._ledger_row("moved.jar", "ran", "needs-loader"),
                 self._ledger_row("fixed.jar", "ran", "native-ok"),
                 self._ledger_row("absent.jar", "refused", "native-gap"),
                 self._ledger_row("new.jar", "refused", "native-gap")]
        regressions = LEDGER.label_regressions(before, after)
        self.assertEqual([(r.file, r.before, r.after) for r in regressions],
                         [("loader.jar", "ran/needs-loader", "refused/native-gap"),
                          ("ok.jar", "ran/native-ok", "refused/native-gap")])
        self.assertIn("uncaught exception NPE", regressions[1].line())

    def test_a_jar_that_ran_and_is_refused_regresses_whatever_its_label(self):
        # A row of an older ledger with a legacy label still counts by verdict.
        before = [self._ledger_row("legacy.jar", "ran", "native")]
        after = [self._ledger_row("legacy.jar", "refused", "missing-dependency")]
        self.assertEqual([r.file for r in LEDGER.label_regressions(before, after)], ["legacy.jar"])
        # Losing only a dependency is not a native regression when the jar never ran.
        self.assertEqual(LEDGER.label_regressions(
            [self._ledger_row("lib.jar", "skipped", "native-ok")],
            [self._ledger_row("lib.jar", "unresolved", "missing-dependency")]), [])

    def test_compare_to_fails_the_run_and_lists_the_rows(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = Path(tmp) / "old.txt"
            new = Path(tmp) / "new.txt"
            old.write_text("\n".join(LEDGER.format_row(row) for row in [
                self._ledger_row("a.jar", "ran", "native-ok"),
                self._ledger_row("b.jar", "ran", "needs-loader")]) + "\n", encoding="utf-8")
            new.write_text("\n".join(LEDGER.format_row(row) for row in [
                self._ledger_row("a.jar", "refused", "native-gap", "uncaught exception boom"),
                self._ledger_row("b.jar", "ran", "needs-loader")]) + "\n", encoding="utf-8")
            err = io.StringIO()
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
                worse = LEDGER.main(["--from", str(new), "--compare-to", str(old)])
            self.assertEqual(worse, 1)
            self.assertIn("compared 2 jar(s)", err.getvalue())
            self.assertIn("a.jar: ran/native-ok -> refused/native-gap: uncaught exception boom",
                          err.getvalue())
            self.assertNotIn("b.jar:", err.getvalue())
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                same = LEDGER.main(["--from", str(old), "--compare-to", str(old)])
            self.assertEqual(same, 0)


if __name__ == "__main__":
    unittest.main()
