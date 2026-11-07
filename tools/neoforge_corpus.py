#!/usr/bin/env python3
"""Assemble a NeoForge 26.3 mod corpus from Modrinth, reproducibly.

Both measured corpora (`corpus-xl`, `mega`) are Fabric 26.3. This builds the
NeoForge counterpart the same way `mega` was built (Modrinth's public API,
every download checked against the hash Modrinth publishes), but from a
committed lock file, so another checkout fetches byte-identical jars:

    python3 tools/neoforge_corpus.py lock --limit 80     # query Modrinth, write the lock
    python3 tools/neoforge_corpus.py fetch               # download the locked jars

`lock` takes the most-downloaded Modrinth mods that publish a NeoForge build
for 26.3 (newest release, else newest pre-release), plus every required
dependency's NeoForge 26.3 build, so `java_mod_server_ledger.py --with-deps`
can close each jar's dependencies inside the corpus. The lock lists project,
version, file name, URL, size and SHA-512; it never holds a jar.

`fetch` writes the jars to `~/horizon-corpus/neoforge/` (never the checkout:
third-party jars are not committed) and skips a file already there with the
locked hash. A download whose size or SHA-512 differs is discarded.

Measure the result with the server ledger and the real FML loader:

    python3 tools/java_mod_server_ledger.py ~/horizon-corpus/neoforge --with-deps \\
        --output tools/real_mod_fixtures/LAST_SERVER_LEDGER_NEOFORGE.txt
    python3 tools/real_mod_corpus.py --neoforge-jars ~/horizon-corpus/neoforge \\
        --server <installed NeoForge 26.3 server>

Exit codes: 0 done, 1 some downloads failed, 2 bad arguments or no network
answer.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Iterable

API = "https://api.modrinth.com/v2"
USER_AGENT = "horizon-neoforge-corpus/1.0 (https://github.com/horizon-rust/horizon)"
GAME_VERSION = "26.3"
LOADER = "neoforge"
DEFAULT_LOCK = Path(__file__).resolve().parent / "real_mod_fixtures" / "neoforge_corpus.lock.json"
DEFAULT_OUT = Path.home() / "horizon-corpus" / "neoforge"
# A jar larger than this is left out: it says nothing a smaller one does not,
# and the corpus stays a modest download.
MAX_JAR_BYTES = 64 * 1024 * 1024
# Search pages Modrinth serves at most.
PAGE = 100
# Modrinth allows 300 requests a minute; this keeps well under it.
REQUEST_GAP = 0.25

Getter = Callable[[str], bytes]


@dataclass(frozen=True)
class LockedJar:
    """One jar of the corpus, as Modrinth published it."""

    slug: str
    project_id: str
    version_id: str
    version_number: str
    file: str
    url: str
    size: int
    sha512: str
    # `top` for a selected project, `dependency` for one a selected jar requires.
    reason: str


def http_get(url: str, *, timeout: float = 60.0, retries: int = 4) -> bytes:
    last: Exception | None = None
    for attempt in range(retries):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(request, timeout=timeout) as response:
                data = response.read()
            time.sleep(REQUEST_GAP)
            return data
        except Exception as error:  # noqa: BLE001 - network errors of every kind retry
            last = error
            time.sleep(1.5 * (attempt + 1))
    assert last is not None
    raise last


def get_json(get: Getter, path: str, query: dict[str, str] | None = None) -> object:
    url = f"{API}{path}" + (f"?{urllib.parse.urlencode(query)}" if query else "")
    return json.loads(get(url).decode("utf-8"))


def search_projects(get: Getter, limit: int) -> list[dict]:
    """The most-downloaded mods Modrinth tags with NeoForge and 26.3.

    The search facets are per project (any version), so a hit can still lack
    a NeoForge 26.3 build; `pick_version` decides that per project.
    """
    facets = json.dumps([["project_type:mod"], [f"versions:{GAME_VERSION}"],
                         [f"categories:{LOADER}"]])
    hits: list[dict] = []
    offset = 0
    while len(hits) < limit:
        page = get_json(get, "/search", {"facets": facets, "index": "downloads",
                                         "limit": str(min(PAGE, limit - len(hits))),
                                         "offset": str(offset)})
        found = page.get("hits", []) if isinstance(page, dict) else []
        if not found:
            break
        hits.extend(found)
        offset += len(found)
    return hits[:limit]


def pick_version(versions: Iterable[dict]) -> dict | None:
    """The build a player would get: the newest release, else the newest of any type.

    Only versions that name both 26.3 and NeoForge count; Modrinth already
    filters by the query, but a cached or hand-written answer may not.
    """
    usable = [version for version in versions
              if GAME_VERSION in version.get("game_versions", ())
              and LOADER in version.get("loaders", ())
              and primary_jar(version) is not None]
    if not usable:
        return None
    usable.sort(key=lambda version: (version.get("date_published", ""), version.get("id", "")),
                reverse=True)
    releases = [version for version in usable if version.get("version_type") == "release"]
    return (releases or usable)[0]


def primary_jar(version: dict) -> dict | None:
    jars = [item for item in version.get("files", ()) if str(item.get("filename", "")).endswith(".jar")]
    primary = [item for item in jars if item.get("primary")]
    return (primary or jars or [None])[0]


def project_version(get: Getter, project: str) -> dict | None:
    versions = get_json(get, f"/project/{project}/version", {
        "game_versions": json.dumps([GAME_VERSION]), "loaders": json.dumps([LOADER])})
    return pick_version(versions if isinstance(versions, list) else [])


def locked(slug: str, project_id: str, version: dict, reason: str) -> LockedJar | None:
    item = primary_jar(version)
    if item is None:
        return None
    sha512 = item.get("hashes", {}).get("sha512", "")
    size = int(item.get("size", 0) or 0)
    if not sha512 or size <= 0 or size > MAX_JAR_BYTES:
        return None
    return LockedJar(slug, project_id, version["id"], version.get("version_number", "?"),
                     item["filename"], item["url"], size, sha512, reason)


def required_projects(version: dict) -> list[str]:
    return sorted({dep["project_id"] for dep in version.get("dependencies", ())
                   if dep.get("dependency_type") == "required" and dep.get("project_id")})


def build_lock(get: Getter, limit: int, *, log: Callable[[str], None] = print) -> list[LockedJar]:
    """Select `limit` projects, then close their required dependencies (bounded by the closure)."""
    jars: dict[str, LockedJar] = {}
    versions: dict[str, dict] = {}
    for hit in search_projects(get, limit):
        version = project_version(get, hit["project_id"])
        row = locked(hit["slug"], hit["project_id"], version, "top") if version else None
        if row is None:
            log(f"skip {hit['slug']}: no NeoForge {GAME_VERSION} jar within the bounds")
            continue
        jars[row.project_id] = row
        versions[row.project_id] = version
    # Every project is looked up once, whether or not it had a usable build.
    seen = set(jars)
    queue = [dep for version in versions.values() for dep in required_projects(version)]
    while queue:
        project = queue.pop()
        if project in seen:
            continue
        seen.add(project)
        version = project_version(get, project)
        if version is None:
            log(f"dependency {project}: no NeoForge {GAME_VERSION} build (the ledger reports it missing)")
            continue
        info = get_json(get, f"/project/{project}")
        slug = info.get("slug", project) if isinstance(info, dict) else project
        row = locked(slug, project, version, "dependency")
        if row is not None:
            jars[project] = row
            queue.extend(required_projects(version))
    return sorted(jars.values(), key=lambda row: row.file.lower())


def write_lock(path: Path, rows: list[LockedJar]) -> None:
    payload = {"game_version": GAME_VERSION, "loader": LOADER, "source": "modrinth",
               "jars": [asdict(row) for row in rows]}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=1, sort_keys=True) + "\n", encoding="utf-8")


def read_lock(path: Path) -> list[LockedJar]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    return [LockedJar(**row) for row in payload["jars"]]


def sha512_of(path: Path) -> str:
    digest = hashlib.sha512()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch_one(row: LockedJar, out: Path, get: Getter) -> str | None:
    """Download one locked jar; `None` on success, else why it was discarded."""
    dest = out / row.file
    if dest.is_file() and dest.stat().st_size == row.size and sha512_of(dest) == row.sha512:
        return None
    try:
        data = get(row.url)
    except Exception as error:  # noqa: BLE001
        return f"download failed: {error}"
    if len(data) != row.size:
        return f"size {len(data)} != locked {row.size}"
    if hashlib.sha512(data).hexdigest() != row.sha512:
        return "sha512 differs from the lock"
    part = dest.with_name(dest.name + ".part")
    part.write_bytes(data)
    part.replace(dest)
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    lock = sub.add_parser("lock", help="query Modrinth and write the lock file")
    lock.add_argument("--limit", type=int, default=80, help="projects to select before dependencies")
    lock.add_argument("--lock", type=Path, default=DEFAULT_LOCK)
    fetch = sub.add_parser("fetch", help="download the locked jars")
    fetch.add_argument("--lock", type=Path, default=DEFAULT_LOCK)
    fetch.add_argument("--out", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args(argv)

    if args.command == "lock":
        if not 1 <= args.limit <= 1000:
            print("neoforge_corpus: --limit must be 1..1000", file=sys.stderr)
            return 2
        try:
            rows = build_lock(http_get, args.limit)
        except Exception as error:  # noqa: BLE001
            print(f"neoforge_corpus: Modrinth query failed: {error}", file=sys.stderr)
            return 2
        write_lock(args.lock, rows)
        top = sum(row.reason == "top" for row in rows)
        print(f"locked {len(rows)} jar(s) ({top} selected, {len(rows) - top} dependencies), "
              f"{sum(row.size for row in rows) / 1e6:.1f} MB -> {args.lock}")
        return 0

    try:
        rows = read_lock(args.lock)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"neoforge_corpus: cannot read {args.lock}: {error}", file=sys.stderr)
        return 2
    args.out.mkdir(parents=True, exist_ok=True)
    failed = 0
    for number, row in enumerate(rows, 1):
        why = fetch_one(row, args.out, http_get)
        if why:
            failed += 1
            print(f"{row.file}: {why}", file=sys.stderr)
        if number % 20 == 0:
            print(f" {number}/{len(rows)}", flush=True)
    print(f"fetched {len(rows) - failed} of {len(rows)} jar(s) into {args.out}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
