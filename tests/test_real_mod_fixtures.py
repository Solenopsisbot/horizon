"""Build and exercise the committed source-only real-mod corpus fixtures."""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from tools import real_mod_corpus


ROOT = Path(__file__).resolve().parents[1]
BUILDER = ROOT / "tools" / "real_mod_fixtures" / "build.py"


class RealModFixtureTest(unittest.TestCase):
    def setUp(self) -> None:
        if shutil.which("javac") is None:
            self.skipTest("javac is unavailable")
        self.temp = tempfile.TemporaryDirectory(prefix="horizon-real-fixtures-test-")
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name) / "jars"
        result = subprocess.run(
            ["python3", str(BUILDER), "--output", str(self.output)],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=True,
        )
        self.assertTrue((self.output / "manifest.json").is_file(), result.stdout + result.stderr)
        self.manifest = json.loads((self.output / "manifest.json").read_text())
        self.fabric_root = Path(self.temp.name) / "fabric"
        self.neo_root = Path(self.temp.name) / "neo"
        self.fabric_root.mkdir()
        self.neo_root.mkdir()
        shutil.copy2(self.output / "fabric-fixture.jar", self.fabric_root / "fabric-fixture.jar")
        for name in ("neoforge-fixture.jar", "forge-fixture.jar"):
            shutil.copy2(self.output / name, self.neo_root / name)

    def corpus(self, *roots: str, server: Path | None = None) -> dict:
        argv = ["--bin-dir", str(self.bin_dir), "--timeout", "30", "--json"]
        for flag in roots:
            argv.extend([flag, str(self.fabric_root if flag == "--fabric-jars" else self.neo_root)])
        if server is not None:
            argv.extend(["--server", str(server)])
        args = real_mod_corpus.parse_args(argv)
        args.fabric_jars = [self.fabric_root] if "--fabric-jars" in roots else []
        args.neoforge_jars = [self.neo_root] if "--neoforge-jars" in roots else []
        args.bin_dir = self.bin_dir
        args.server = server
        return real_mod_corpus.build_report(args)

    @property
    def bin_dir(self) -> Path:
        for name in ("fast", "debug"):
            path = ROOT / "target" / name
            if (path / "horizon-native-java").is_file():
                return path
        return ROOT / "target" / "debug"

    def test_manifest_is_deterministic_and_fabric_nested_probe_is_folded(self) -> None:
        self.assertEqual(set(self.manifest), {"fabric-fixture.jar", "forge-fixture.jar", "neoforge-fixture.jar"})
        report = self.corpus("--fabric-jars")
        self.assertEqual(report["counts"]["total"], 1)
        row = next(item for item in report["jars"] if item["file"] == "fabric-fixture.jar")
        self.assertEqual(row["loader"], "fabric")
        self.assertEqual(row["verdict"], "ok")
        self.assertEqual(len(row["nested"]), 1)
        self.assertEqual(row["nested"][0]["name"], "META-INF/jars/nested.jar")
        self.assertGreaterEqual(row["entrypoints"], 1)

    def test_neoforge_and_legacy_forge_metadata_only_without_server(self) -> None:
        report = self.corpus("--neoforge-jars")
        self.assertEqual(report["counts"]["metadata-only"], 2)
        for row in report["jars"]:
            self.assertEqual(row["loader"], "neoforge")
            self.assertEqual(row["verdict"], "metadata-only")
            self.assertIn("NeoForge server not supplied", row["reason"])

    def test_neoforge_path_runs_when_local_server_is_available(self) -> None:
        server = Path("/tmp/opencode/nf263")
        if not (server / "libraries").is_dir():
            self.skipTest("no local NeoForge server install")
        report = self.corpus("--neoforge-jars", server=server)
        rows = {row["file"]: row for row in report["jars"]}
        self.assertIn(rows["neoforge-fixture.jar"]["verdict"], {"ok", "fallback", "unsupported"})
        self.assertTrue(rows["neoforge-fixture.jar"]["probes"])


if __name__ == "__main__":
    unittest.main()
