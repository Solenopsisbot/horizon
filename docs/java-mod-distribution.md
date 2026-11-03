# Java mods: what ships, and why it can be trusted

Status: policy for Minecraft 26.3, written 3 November 2026 (`4e8a6cb12`). Design,
[java-mod-compat.md](java-mod-compat.md); progress, [java-mod-compat-status.md](java-mod-compat-status.md);
mods generally, [mods.md](mods.md) and [native-mods.md](native-mods.md). **Exists** = in the tree today;
**planned** = design only, and nothing may describe it as working.

## 1. What ships, and what never does

| Artifact | Ships? | Where it comes from | Why |
|---|---|---|---|
| Mojang game jar, assets, libraries, decompiled source | never | — | `tools/check_public_tree.py` rejects `.jar`, `.class`, `.java`, `.local/` |
| Prepared resources, `.local/horizon-client/26.3` | no | `horizon-prepare`, from **the player's** install | derived from Mojang's files; stays on the machine |
| A Java mod's jar | no | the player's own file (Modrinth, CurseForge, a folder) | third-party code; never redistributed, never in an instance we publish |
| Generated façade (`facade.jar`) | planned | `horizon-port-pack facade`, from the game jar + JDK + `--libs` | carries Mojang's class and member names: local-only, like `.local/` |
| Translations and rewritten classes | planned | Mixin/remap, per jar, in memory | same, plus the mod's own bytes |
| Embedded JRE | undecided (design decision 2) | a pinned Temurin | today `horizon-jvm` needs `JAVA_HOME` with `libjvm`, and says so |
| `horizon-jvm`, `horizon-native-java`, `horizon-port-pack` | yes | this repo, MIT | Rust probe binaries: Horizon's own code |

A release holds Rust, the exporters' own Java ([launcher-preparation.md](launcher-preparation.md)), and nothing of
Mojang's and nothing of a mod's. A façade or translation is never committed, published or handed to another
player: it means nothing without the exact game jar and mod jar it came from. `--java-mod PATH` (repeatable,
`game.rs`) is the only way a jar runs, and `mods/` is never scanned implicitly, so no Java code runs by accident.

## 2. Compatibility tiers

A tier belongs to a **(jar SHA-256, Horizon build)** pair, not to a mod forever: it measures **how much had to
change to make the jar run**.

| Tier | Precise definition | Evidence that assigns it |
|---|---|---|
| **0 — runs** | Every `net/minecraft`, loader and API member the jar references resolves in the generated façade; every required Mixin plan is accepted; no native library, no direct GL, no reflection into internals. The jar's bytes are on the classpath unmodified. | `horizon-jvm` returns from the entrypoint and the scenario runs with zero `horizon stub:` lines |
| **1 — remapped** | A 26.3 jar whose names drifted inside the 26.3 line (or one built against a snapshot): names translated in memory, data files upgraded by the carry. On-disk jar unchanged. | the census's missing members are covered by a name table; the JVM run is clean after translation |
| **2 — shimmed** | A generated jar supplies classes the mod expects but Horizon models differently: a nested library, or an API class the real Fabric API does not ship. Classpath wrapped; jar unchanged. | the façade emits the class only because the real one is absent; clean JVM run with `--extra` |
| **3 — ported** | The port pipeline writes a `.hmod`/native mod from observed behaviour; the jar is replaced, not run ([modpacks.md](modpacks.md#licences)). | the pipeline's carrier/port report, not a runtime probe |
| **4 — refused** | JNI or a bundled native library; direct GL/`RenderSystem` with no draw-list equivalent; reflection into internals Horizon shaped differently; a required Mixin plan with no hook and no fallback; `@Overwrite` of a core method. Named, with the reason. | the first failing member or plan, quoted |
| **5 — partly compiled** | A tier 0–2 jar whose hot methods are AOT-compiled to native while the rest stays on the JVM. The jar is still the artifact; the tier stays 0–2 plus a "compiled" mark. | profiling says the method is hot, then the compiler's report |

Tier 5 has no compiler. No code assigns a tier yet: `check` still emits the three-way `Verdict`
(`horizon-port-pack/src/check.rs`). First step: a `tier` beside `verdict`, and a bump past `mod-browser/checks-1`.

Today the launcher maps that `Verdict` to `Support::{Works, Partial, No}` (`launcher/src/browser/java.rs`) and
degrades `Works` to `Partly` when the native probe stopped. A tier row replaces it and adds the jar's SHA-256 and
the Horizon build measured on — a tier without both is a rumour.

| Tier | Badge | Line under the name | Install |
|---|---|---|---|
| 0 | Runs | "Runs unmodified: N entrypoints, 0 unsupported calls, scenario passed" | offered |
| 1 | Remapped | "Runs; N names translated for 26.3" | offered |
| 2 | Shimmed | "Runs with N generated classes" and the list | offered |
| 3 | Ported | "Doesn't run as a jar; ports as a content/native mod: what ports, what doesn't" | becomes a port |
| 4 | Doesn't run | the named reason | disabled |
| 5 | Runs · compiled | "Runs; N hot methods compiled" | offered |

## 3. Trust

**Bytes and provenance.** Downloads are checked against the source's checksum before they reach the game folder:
SHA-512 (Modrinth), SHA-1 (CurseForge, the strongest it publishes), and a file whose source lists none is refused
([mod-browser.md](mod-browser.md)). Horizon's registry pins SHA-256, the launcher refuses other bytes, and an
Ed25519 `.minisig` is checked against the key the index pins ([mods.md](mods.md)).

- A Java jar the player placed themselves is unsigned and never came from the registry, so its provenance is the
  hash; a bridged jar **cannot** be signed by Horizon, since we do not build it, and the UI says "source checksum +
  the player chose it", never implying review.
- The compatibility record is keyed by the file's checksum (`mod-browser/checks-1/<checksum>`), so a verdict
  belongs to exact bytes. A native mod installed from GitHub keeps `horizon-native-mods/<id>.origin.json`
  (owner/repo, tag, commit, asset, SHA-256); the Java equivalent is planned. A browser install records source,
  project, version, checksum and verdict in `mod-browser.json`; a hand-placed jar has none, and the game names it
  by path.

**Consent.** Off until the player turns it on, per instance, like native mods: a **Java compatibility
(experimental)** switch plus `--trust-java-mods`. Planned: today only `--java-mod` exists, a developer flag the
launcher does not offer. The notice names the mods (id, version, SHA-256), says the registry did not review them,
and says they run with the player's rights — the same sentence as native mods, because the risk is the same or
worse. Turning a mod on is what starts the JVM; with none on, no JVM exists in the process and the Rust game is
byte-for-byte the same, which is the carve-out's whole condition (§4).

**Quarantine on failure** (planned). A jar whose tier is 4, or whose startup throws, is turned off for that
instance and the instance still opens; the jar is kept, the row names the failure, and "open without it" is one
click. The mechanism is a `Thread.UncaughtExceptionHandler` at every dispatch catching `Throwable` and blaming the
mod ([java-mod-compat.md](java-mod-compat.md#trust)). What exists to build on: typed declarations commit
all-or-nothing (`NativeDeclarations::from_registry_entries` aborts the batch; `attach_registration_declaration`
rejects a missing or enriched entry), so a failing mod cannot leave half a registry; two mods cannot share a
namespace; `Verdict::No` drops the carry; `revoked.toml` disables a file. A record is per instance (mod, failure,
jar hash), not global: the same jar may work elsewhere.

**What the JVM may touch.** Two boundaries, and "sandbox" is reserved for the first:

- **The native probe is bounded by construction (exists).** `horizon-native-java` opens no file, reads no process
  environment (`System.getenv` answers only seeded values), reads no clock (`currentTimeMillis` is seeded), and
  never scans the classpath: `Class.forName` resolves only classes already loaded from the current jar,
  `ServiceLoader` only seeded providers, resources only bytes seeded from the jar (4 MiB per file, 32 MiB per
  jar), `ClassLoader.loadClass` only seeded markers.
- **The embedded JVM is not bounded (exists, by decision).** It runs in-process with the player's rights and can
  open sockets and files: the native-mod stance applied to Java — no sandbox promise, consent instead.
  Out-of-process with a restricted view and a shared-memory arena is design decision 1, unbuilt.
- No Java path may be called sandboxed until it denies filesystem, network and process spawning by default; a UI
  that mentions isolation says "the bounded probe", not "the mod".

## 4. Licensing

| Component | Licence | How it is used |
|---|---|---|
| Fabric Loader | Apache-2.0 | used as it is; Horizon is a game provider to Fabric, not a reimplementation |
| Mixin | MIT | used as it is, as Fabric does |
| Fabric API jars | Apache-2.0 | the mod's own dependency, supplied by the player; the façade emits an API class only when the real one is absent, so we neither shadow nor ship it |
| NeoForge's FML surface | Horizon's own, from public documentation | names and contracts from docs, nothing read or copied from NeoForge's source (`native-hooks-neoforge.md` rule) |
| NeoForge and Quilt logos | CC BY 4.0, CC0 | already shipped as images; Fabric's and Forge's are fetched at run time ([THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)) |
| A mod's jar and code | the mod's own licence | never redistributed; it is the player's file |

**The carve-out from "everything players run is Rust".** A compatibility runtime is by definition a JVM, so the
carve-out is explicit and narrow:

1. Horizon's shipped binaries — game, launcher, dedicated server, probe tools — are Rust. No Java is compiled
   into them.
2. A JVM starts only when a jar the player turned on is loaded, and the façade it runs against is generated at
   runtime from Horizon's tables and the player's own game jar; nothing Java ships in a release.
3. With no Java mod on, the process runs no JVM and the Rust game is byte-for-byte the same.
4. What the jar does is the mod author's behaviour, reported as a tier — never "Horizon implements this".

This is the text to cite if the line is added to AGENTS.md's rules; until it is, the carve-out is this page's
claim, not the repo's.

**Per-mod artifacts and attribution.** A carried or ported artifact records its origin in its own manifest:
`ported-from = { id, version, license, url }` (the carry writes this today) with the source jar's SHA-256 beside
it. A façade is generated per jar and thrown away — it embeds Mojang's names (facts read from the game jar) and
the mod's referenced members — so it lives and dies beside `.local/`. A tier in a UI ("Runs unmodified") is a
measurement about a jar, not an endorsement; name, author and licence come from the jar's own metadata, read by
`horizon-port-pack`'s `jar::read`. Nothing here relaxes [modpacks.md](modpacks.md#licences) rules 1–5.

## 5. Update and rollback

- **Keys.** The instance's mod set is a SHA-256 over sorted `(id, file SHA-256)` pairs (native builds already key
  a build this way; Java is planned). A façade's key is the game jar's SHA-256, the generator version, the
  `--extra` API jars' hashes, the JDK major and `horizon_mod_abi_version` — today `facade` just writes to the
  output path it is given. A translation and tier key adds the mod set key, the jar's Mixin config set and the
  check-schema version. Copy `horizon-prepare`'s `.local/exporters/<sha256>` (staged, then published) and the
  check cache's folder bump, so a smarter check re-checks instead of reusing an old answer.
- **Invalidation.** A Minecraft version move (26.3 → the next) invalidates every façade, translation and tier;
  since 26.1 the game is unobfuscated, so that is one name table and one rebuild, not a remapper. A loader version
  move invalidates the shim layer and what is keyed on it. A Horizon build move re-tiers only if
  `horizon_mod_abi_version` or the hook API changed — a UI-only release must not reprocess every jar. A jar's own
  hash change invalidates only its entry.
- **Rollback.** Keep the last N builds per instance as native builds do (`native-<commit>-<key>` folders, pruned
  by `native_mods::prune`). An update that fails (tier falling to 4, a startup throw, a crash blamed on a mod)
  reverts to the last keyed set and says so, instead of leaving the instance unopenable. Removing a mod removes
  its façade, translation and tier record with it.

## 6. Support and telemetry

- **Logs.** `logs/latest.log` in the game home (`$HORIZON_HOME`, else the platform data folder; a dedicated
  server in its own folder), rotated as `logs/<date>-<n>.log.gz`; crashes in
  `crash-reports/crash-<date>_<time>-client.txt`; a native build's log is `native/build.log`; `horizon-ctl logs
  [--lines N] [--crash]` prints both. A Java mod's compatibility report is logged at startup
  (`log_java_reports`) and, on a dedicated server, exposed as the status `java` section.
- **Unsupported calls are named, never swallowed.** The façade throws
  `UnsupportedOperationException("horizon stub: class#method")` with the mod's own file and line under it; the
  native probe answers `{status, error, stage}` naming class, method and program counter; a Mixin plan with no
  hook is a diagnostic that must take the JVM path.
- **Crash isolation** (planned): one dispatch handler per event family catches `Throwable`, records (mod, jar
  SHA-256, member, stack head) and offers "open without it", so a mod's exception never takes the process down.
  Today a JVM-path failure fails startup naming the jar and entrypoint; a native-path failure returns a
  diagnostic.
- **Opt-in diagnostics** (planned): an unsupported-call report is a new telemetry kind, so it needs a
  `CONSENT_VERSION` bump (`crates/horizon-telemetry`), an off-by-default switch, `outbox::push` gating and the
  same scrubbing. It carries mod id, version, jar SHA-256, loader, MC version, Horizon build, tier, first
  unsupported member and counts — never the jar, the mod's source, the log, world or account names — and
  published aggregates keep the 3-install floor benchmarks use ([telemetry.md](telemetry.md)).

## 7. Checklist: before an agent claims a mod is supported

1. `/claim` the issue; `gh pr list --state open --search <area>` first ([AGENTS.md](../AGENTS.md)).
2. The jar must declare **26.3**; anything older is a port, not a bridge.
3. `horizon-port-pack check <jar> <out>`: record `Verdict`, `summary`, ports, `native`; nothing enters the repo.
4. `horizon-port-pack facade <jar> <out> --game <26.3.jar> --jdk <dir> --libs <dir> [--extra …]`: `javac` clean,
   "0 named but missing", counts recorded.
5. `horizon-jvm …`: the selected entrypoints return, and every remaining failure is a named stub with the mod's
   file and line — not a silence.
6. `horizon-native-java --json <jar> <kind>`: paste `status`, `host_calls`, `entrypoints`; a green probe is not
   tier 0 by itself.
7. Registrations must be **typed** (`META-INF/horizon-native.json`, inferred builders, explicit adapter); an
   opaque `RegistryEntry` is a diagnostic, not support.
8. Role matrix `client`/`integrated-server`/`dedicated-server`: a client-only jar is skipped before constructor
   loading on both server roles, with the role and kinds declared in the error.
9. Mixin: every required plan runs natively or is a named JVM fallback; an unresolved plan is never support.
10. Failure paths: jar named, boundary catches, instance opens, log line exists — quote it in the PR.
11. Update [java-mod-compat-status.md](java-mod-compat-status.md) (its owner does); record tier and Horizon build
    here.
12. `tools/check_public_tree.py` and `tools/pr_ready.py --quick` pass; name the parity harnesses affected.

## Open decisions

1. In-process JVM or a child process with a shared-memory arena (design decision 1). This page assumes in-process.
2. Whether a JRE ships (design decision 2). Until it does, a Java mod stays a developer feature.
3. Whether a bridged world is its own instance kind ("Compatibility", design decision 3). The tier model and the
   per-instance mod-set key assume yes: a bridged mod's registrations must not mix with a port's.
4. Adding the carve-out in §4 to AGENTS.md's rules, or leaving it as a claim here.
5. When tier 5 may start (profiling threshold, who signs off a compiled artifact).
6. Whether the unsupported-call report is a telemetry kind, an issue template, or both — either way it needs a
   consent version bump.
