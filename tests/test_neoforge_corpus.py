import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).parents[1]
_SPEC = importlib.util.spec_from_file_location("neoforge_corpus_tool", ROOT / "tools" / "neoforge_corpus.py")
CORPUS = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = CORPUS
_SPEC.loader.exec_module(CORPUS)


def _version(ident, *, date, kind="release", loaders=("neoforge",), games=("26.3",), deps=(),
             data=b"jar", name=None):
    return {
        "id": ident, "version_number": ident, "version_type": kind, "date_published": date,
        "loaders": list(loaders), "game_versions": list(games),
        "dependencies": [{"project_id": dep, "dependency_type": kind_}
                         for dep, kind_ in deps],
        "files": [{"filename": name or f"{ident}.jar", "url": f"https://cdn/{ident}.jar",
                   "primary": True, "size": len(data),
                   "hashes": {"sha512": hashlib.sha512(data).hexdigest()}}],
    }


class FakeModrinth:
    """Answers the three API paths the tool asks, and file downloads."""

    def __init__(self, hits, versions, files):
        self.hits, self.versions, self.files = hits, versions, files

    def __call__(self, url):
        parsed = urllib.parse.urlparse(url)
        path = parsed.path.removeprefix("/v2")
        if url.startswith("https://cdn/"):
            return self.files[url]
        if path == "/search":
            query = urllib.parse.parse_qs(parsed.query)
            offset, limit = int(query["offset"][0]), int(query["limit"][0])
            return json.dumps({"hits": self.hits[offset:offset + limit]}).encode()
        if path.endswith("/version"):
            return json.dumps(self.versions.get(path.split("/")[2], [])).encode()
        return json.dumps({"slug": path.split("/")[2] + "-slug"}).encode()


class NeoforgeCorpusTests(unittest.TestCase):
    def test_the_lock_takes_the_newest_release_and_closes_required_dependencies(self):
        modrinth = FakeModrinth(
            hits=[{"project_id": "A", "slug": "alpha"}, {"project_id": "B", "slug": "beta"},
                  {"project_id": "C", "slug": "gamma"}],
            versions={
                # A newer beta does not beat an older release; a fabric-only and
                # a 26.2 build do not count at all.
                "A": [_version("a-1", date="2026-01", deps=[("L", "required"), ("O", "optional")]),
                      _version("a-2b", date="2026-03", kind="beta"),
                      _version("a-3", date="2026-04", loaders=("fabric",)),
                      _version("a-4", date="2026-05", games=("26.2",))],
                # Only a pre-release exists: it is taken.
                "B": [_version("b-1a", date="2026-02", kind="alpha", deps=[("M", "required")])],
                "C": [],
                "L": [_version("l-1", date="2026-01", deps=[("A", "required")])],
                "O": [_version("o-1", date="2026-01")],
            },
            files={})
        notes = []
        rows = CORPUS.build_lock(modrinth, 3, log=notes.append)
        picked = {row.version_id: row.reason for row in rows}
        self.assertEqual(picked, {"a-1": "top", "b-1a": "top", "l-1": "dependency"})
        self.assertEqual(next(row.slug for row in rows if row.version_id == "l-1"), "L-slug")
        # The unusable project and the dependency with no build are said, not hidden.
        self.assertTrue(any("gamma" in note for note in notes))
        self.assertTrue(any(note.startswith("dependency M") for note in notes))

    def test_fetch_keeps_only_a_jar_matching_the_lock(self):
        good, bad = b"good jar bytes", b"tampered bytes"
        row = CORPUS.LockedJar("s", "P", "v", "1", "mod.jar", "https://cdn/mod.jar", len(good),
                               hashlib.sha512(good).hexdigest(), "top")
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            lock = out / "lock.json"
            CORPUS.write_lock(lock, [row])
            self.assertEqual(CORPUS.read_lock(lock), [row])
            why = CORPUS.fetch_one(row, out, FakeModrinth([], {}, {row.url: bad[:len(good)]}))
            self.assertIn("sha512", why)
            self.assertFalse((out / "mod.jar").exists())
            self.assertIsNone(CORPUS.fetch_one(row, out, FakeModrinth([], {}, {row.url: good})))
            self.assertEqual((out / "mod.jar").read_bytes(), good)
            # Already there with the locked hash: no download is asked for.
            self.assertIsNone(CORPUS.fetch_one(row, out, FakeModrinth([], {}, {})))


if __name__ == "__main__":
    unittest.main()
