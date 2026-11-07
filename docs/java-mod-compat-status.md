# Java mod compatibility: status

Status: **ported onto the main tree** (branch `port/native-java-on-main`). Measured 6 November 2026 with
binaries built from this branch at 19:02. The design, tier model and phase plan are
[java-mod-compat.md](java-mod-compat.md); this page is the maintained progress view: what runs today, what the
corpus and the clock measured, and what is left.

A jar runs only when it is explicitly named with `--java-mod PATH` (repeatable) on `horizon-client`. Horizon never
scans a folder for Java jars, and a run without the flag starts no Java runtime. A window runs its jars in the client
role, its server thread in the integrated-server role, and `--dedicated` runs them before the content root is built.
A jar the runtime refuses is logged and skipped, and `HORIZON_JAVA_STRICT=1` makes the dedicated server stop instead
of skipping it. A jar that panics in its entrypoint is isolated; the remaining jars still run.

## The goal: two phases to native-only

The project has one destination and one checkpoint on the way. Both phases are gated on the measured corpus, not on a
date:

| Phase | Goal | Exit criteria |
|---|---|---|
| **1. Every corpus jar works on a tier** | no measured jar is left without a path: it runs natively, or through the loader tier, and its label says why | **0 `native-gap`** rows in both committed ledgers; **every `needs-loader` jar proven on the loader tier** — a real Fabric-loader run reaches `Done (`, lists the jar's mod id and names no exception against it; and a hybrid pack session completes both halves inside its work bound |
| **2. Native-only** | the same corpus runs on `horizon-native-java` alone; the loader tier is retired and no game path starts a JVM | 0 `needs-loader` and 0 `native-gap`; every exercised mixin plan is applied by the native weaver or bound to a named host hook; `backends.jvm` stays zero because nothing has a JVM to fall back to |

Phase 1 is a checkpoint, not the product: a `needs-loader` jar is proven, not ported. Skipped client-only jars and
libraries stay `native-ok` throughout — the server role has nothing of theirs to run, so they are not failures.
Phase 2 starts from phase 1's zero-refusal baseline and adds what the loader was carrying: game-class mixins, event
callbacks and the client surface. The design and tier model are
[java-mod-compat.md](java-mod-compat.md#native-first-strategy-and-tiers).

### Coverage of the two gates (6 November 2026)

| Corpus | Jars | `native-ok` | `needs-loader` | of those proven on the loader (7 Nov) | `native-gap` | Phase 1 met? |
|---|---:|---:|---:|---:|---:|---|
| wide (`corpus-xl`) | 223 | 151 (68%) | 45 (20%) | 36/36, and 9 are `environment: client` | 27 (12%) | no |
| mega (500 unique) | 500 | 346 (69%) | 86 (17%) | 57/58, and 28 are `environment: client` | 68 (14%) | no |

The label columns are the server ledgers'. "Proven" comes from the two loader proof ledgers
(`LAST_LOADER_PROOF_223.txt`, `LAST_LOADER_PROOF_500.txt`), one real Fabric launch per jar's dependency closure
over both whole corpora: the launch lists the jar's mod id, reaches `Done (`, and no failure line of that run
names the mod. A `needs-loader` jar that declares `environment: client` needs no server proof, because Fabric
never loads it on a dedicated server. The one unproven mega row is combat-hitboxes, whose `main` reaches client
classes. The same proof also covers native refusals: 23 of the 27 wide and 57 of the 68 mega `native-gap` rows
are listed by the loader (21 and 55 proven), which is how the tier coverage below reaches 0 `native-gap`. The
full per-row result, including the 5 listed-but-unproven rows and why, is the generated
[tier coverage](#tier-coverage-over-the-whole-corpus).

The same runs show the distance to phase 2: 0 mixins were woven in either corpus, with 2,944 (wide) and 7,742 (mega)
plans skipped, overwhelmingly `target-outside-jar`. Of the 121 wide and 265 mega jars that declare a server
entrypoint, 94 and 197 reached `Done`; the refusals are the `native-gap` rows above.

### Blockers

**Phase 1:**

1. **Native refusals (27 wide, 68 mega).** 9 wide / 29 mega fail decoding class metadata; 18 / 34 fail with an
   uncaught exception in the entrypoint; mega adds one verifier refusal, two nested-jar-frame cases, one duplicate
   declaration and one unreadable metadata file. The gate is 0, so every row needs either a fix or a reclassification
   backed by a loader proof. Re-measured 7 November on the 38 metadata rows of both ledgers (one fresh dedicated
   server per jar): 15 → 22 run, 6 are client-only and now skipped, and 7 metadata refusals remain, each a jar
   that needs something outside itself (below, under *Close metadata refusals*).
2. **Five loader rows are listed but not proven.** 169 of the 174 `needs-loader` rows of the tier coverage reach
   `Done (` with no failure naming them. Four ship client code the server role loads (CreativeCore 2.14.16 twice,
   sway, combat-hitboxes; a plain Fabric server fails on sway and CreativeCore too), so they need a client-role
   loader proof, which needs a client launch the server corpus does not have. moderner_beta boots but logs an
   empty-registry ERROR against itself.
3. **The hybrid session completes on one pack, the server role only.** The 112-jar Helix pack now runs both
   halves inside the 180 s bound with 0 static conflicts (71 native jars, 43 loader jars, the loader half at
   `Done (` in 15 s; [Hybrid sessions](#hybrid-sessions-a-native-half-and-a-loader-half)). Sway stays out of
   it (client code), seven Helix jars have no ledger row, and no client-role hybrid has been measured.
4. **NeoForge is outside the measured corpus.** Both corpora are Fabric 26.3. If a NeoForge corpus joins phase 1,
   the dedicated role still lacks the FML preflight and there is no automatic intermediary-to-official remapping.

**Phase 2:**

1. **Game-class mixins mostly do not apply.** The load-time weaver only rewrites same-jar targets. The opt-in hook
   tier (`HORIZON_JAVA_GAME_HOOKS=1`) now fires its 19 sites from the game, but they cover a small share of the
   game-class plans a `needs-loader` jar carries, and a callback runs only as far as the hook tier's object model
   reaches. Closing this is most of the distance for the 45 / 86 `needs-loader` jars.
2. **Callbacks and the client surface.** Only entrypoints and received-payload handlers execute; event listeners are
   recorded but never dispatched, and there is no compiled HUD, key-binding, tooltip, screen, renderer or model
   façade, so client jars register content but do not draw.
3. **Content consumption.** Recipes and block entity types reach the content root only when everything they name is
   declared (824 of 3,192 mod-namespace recipe files in the 25 mega jars that ship them); 12 of 40 data-component
   registrations are still opaque and every `BlockEntityType` builder is; ingredients have no consumer; a guest
   without the jars is refused by the content identity check before the manifest helps it; there is no mod hot
   reload.
4. **No fallback by definition.** Phase 2 keeps phase 1's zero-refusal baseline and moves the 45 / 86
   `needs-loader` jars onto the native engine; the opt-in FML preflight and `horizon-jvm` remain development tools,
   never a quiet fallback, and the NeoForge lifecycle needs native coverage if the corpus widens.

### Measuring the gates

```sh
# the label gates, from the committed ledgers (no jars, no servers)
python3 tools/java_mod_server_ledger.py --from tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt | tail -1
python3 tools/java_mod_server_ledger.py --from tools/real_mod_fixtures/LAST_SERVER_LEDGER_500.txt | tail -1

# a fresh corpus through the real dedicated server (one fresh server per jar, 60 s bound)
scratch=$(mktemp -d)
TMPDIR=$scratch HORIZON_NO_AUDIO=1 python3 tools/java_mod_server_ledger.py ~/horizon-corpus/corpus-xl \
  --binary target/fast/horizon-client --native-binary target/fast/horizon-native-java \
  --resources .local/horizon-client/26.3 --timeout 60 --json --output ledger.txt > ledger.json

# the same corpus's native-gap rows again, each jar with its declared dependency closure
TMPDIR=$scratch HORIZON_NO_AUDIO=1 python3 tools/java_mod_server_ledger.py ~/horizon-corpus/corpus-xl --with-deps \
  --rerun tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt --binary target/fast/horizon-client \
  --native-binary target/fast/horizon-native-java --resources .local/horizon-client/26.3 --output deps-ledger.txt

# loader-tier proof (needs the pinned game jar, loader and libraries)
python3 tools/java_mod_loader_ledger.py --mods-dir ~/horizon-corpus/corpus-xl \
  --binary target/fast/horizon-jvm --output tools/real_mod_fixtures/LAST_LOADER_LEDGER.txt

# the hybrid session, both halves, on the measured pack
python3 tools/hybrid_pack.py ~/horizon-corpus/corpus-helix \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_500.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_223.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_500.txt --match-by-hash --measure
```

## What is ported

### `horizon-native-java` (the runtime)

- **Metadata.** Fabric `fabric.mod.json`, NeoForge `neoforge.mods.toml` and legacy Forge `mods.toml`, with identity,
  versions, dependencies, client-only markers and declared entrypoint kinds. Nested JiJ/JarJar jars are reported
  (the corpus probe extracts and probes them) but the game does not descend into them.
- **Entrypoints.** Fabric `main`, `client` and `server` (string, object and array forms) and NeoForge `@Mod` classes.
  The loader runs the first kind it finds for its role (`client`/`main`/`neoforge` on a client,
  `server`/`main`/`neoforge` on a server). The Kotlin adapter is resolved to the default invocation contract (its
  classes compile to ordinary JVM bytecode), and an adapter it does not model is refused. `preLaunch` entrypoints
  are not run.
- **Execution.** A bounded classfile interpreter with exact `long`/`float`/`double` arithmetic, exceptions, records,
  bounded reflection, collections and strings, behind an explicit `Host` boundary. A call the host does not model
  returns a neutral value by default and is counted; the probe fixtures can make it a hard error
  (`with_strict_unknown_host_calls`). The whole run is isolated per jar (`catch_unwind`).
- **Declarations.** Typed `Registry.register`/`DeferredRegister` rows become blocks, items, mob effects, entities,
  menus, block entities, recipes, ingredients, data components and attachments, with item/block property inference
  from ordinary Java builders, data-component flags from `DataComponentType.builder()` chains, and Fabric
  `AttachmentRegistry` rows (`declarations_content.rs`). A jar's `data/<ns>/recipe/*.json` files are recipe rows
  beside its metadata. `META-INF/horizon-native.json` remains the explicit override.
- **Mixin.** Annotation-based plan discovery (config lists, side, priority, selectors) and a classfile writer/weaver
  that rewrites classes whose target is in the same jar. All eleven fixture families weave: `HEAD`, cancellable
  `HEAD`, `RETURN`, `TAIL`, `INVOKE`, `FIELD`, `NEW`, `JUMP`, `@Overwrite`, `@ModifyArg` and `@ModifyVariable`,
  with `CallbackInfo`/`CallbackInfoReturnable`, instance callbacks through a generated bridge, condy and stack-map
  frames. A plan the weaver cannot carry stays skipped with a fixed label (`target-outside-jar`, `non-head-anchor`,
  `unsupported-family`, `cancellable`, `selector-modifier`, `not-in-config`, `other-side`, `config-plugin`,
  `constructor-target`, `unresolved`, `target-missing`, `ambiguous-target`, `listed-without-plan`, `weaver-rejected`).
  The game uses the load-time weave only: a game-class target still has no class to rewrite, so a real jar reads
  `native-qualified`. With `HORIZON_JAVA_GAME_HOOKS=1` the loader plans with `plan_load_time_weave_with_game_hooks`
  instead, binds an `@Inject` for a game class to one of the 19 `GAME_HOOK_SITES` rows, and the game fires the
  callback at that host hook point ([the hook tier](java-mod-compat.md#game-class-hook-tier-experiment)).

  The fixture family matrix is **11 applied / 0 missing**: every family above weaves and runs with its own argument
  at its own position (`crates/horizon-native-java/tests/fixture_mixin_family_matrix.rs`). This is the exact
  load-time path the game calls, not a claim that game-class mixins work: those plans still need the loader or the
  opt-in hook tier.
- **Payloads.** Declared payload channels, codecs (`minecraft:raw`/`byte_array`, `minecraft:string`,
  `minecraft:empty`, opaque fallback), sender/receiver hosts and dispatch of a received frame to the jar's handler.
- **Game events** (`src/events.rs`). Each listener a jar registers gets a stable identity: the Fabric `Event` field
  (22 server-side fields: `ServerLifecycleEvents` start/started/stopping/stopped, `ServerTickEvents` server and
  level start/end, `ServerLevelEvents` load/unload, `ServerPlayConnectionEvents` and `ServerPlayerEvents`
  join/leave, `ServerEntityEvents.ENTITY_LOAD`, `ServerLivingEntityEvents` allow/after death, `UseBlock`/`UseItem`/
  `AttackEntityCallback`, `PlayerBlockBreakEvents` before/after) or the NeoForge/Forge event class (`addListener`
  lambdas and method references, `register(Object|Class)` and `@EventBusSubscriber` `@SubscribeEvent` methods,
  Forge EventBus 7's per-event `BUS`). `NativeJar::run_game_listener` runs one with the jar's own statics, under a
  time budget, against bounded façade markers (server, player list, level, player, join handler, NeoForge event
  object); a façade call outside that surface refuses the listener by name. Fabric `boolean`/`InteractionResult`
  callbacks stop at the first deciding answer, NeoForge `setCanceled` skips later listeners without
  `receiveCanceled`. Priorities are not modelled (registration order). Over the 723-jar corpus, 83 of the 499 jars
  that bootstrap natively register 257 listeners the game can fire (`server_started` 38 jars, `server_starting`
  35, `player_join` 34, `server_stopped` 22, `player_leave` 21, `server_stopping` 19, `server_tick_end` 17); the
  largest unfired families are client ticks and `CommandRegistrationCallback` (44 jars).
- **Probe.** `horizon-native-java [--json] [--mixin-plans] [--weave <out.jar>] <jar> [kind] [class]` reports
  entrypoints, host calls, registrations, plans and skips, and can write a woven jar.

### `horizon-client` (the game)

- Java rows are converted into the current content model and layered into the content root: the window's client role
  before the root is built, the integrated server's rows published from the server thread and merged on the next
  rebuild, and the dedicated role before resources. A sorted typed registry snapshot is published beside the root
  and coalesces equal rows; two rows that disagree are refused, not first-wins. Resolvable recipes become generated
  `data/<ns>/recipe/` files the crafting loader and the recipe book read, block entity types are numbered after the
  per-block ones, and one namespace declared by two jars joins rather than clashes. Each jar logs what it adds and
  the first reason for what it leaves out.
- Declared Java content travels to LAN guests: a bounded, versioned `horizon:java_content` manifest (v3 header with
  epoch and content hash, eight registry id sections, block-entity/menu/entity template rows) is sent at login and
  again when the declaration set changes. A mismatched v3 hash is refused rather than replacing local content.
- Java payload frames travel both ways over LAN on `horizon:payload`, registration on `horizon:payload_channels`;
  frames above the custom-payload envelope are chunked, direction and codec are validated, and `horizon-ctl`'s
  `payload send` injects a declared frame into a hosted world. Each role (client, integrated server, dedicated)
  has its own handler host and queues.
- Java event listeners run on servers (`game/java_events.rs`): the batch keeps every jar that registered a listener
  the game fires, with the host its entrypoints ran on, per role (such a jar is no longer replayed from the content
  cache). The integrated and dedicated servers fire server starting/started/stopping/stopped, server tick
  start/end, level tick start/end, player join/leave and a living entity's death (`ALLOW_DEATH`/`LivingDeathEvent`
  can cancel it, then `AFTER_DEATH`). With no listener, each point costs one atomic load. A listener that fails is
  disabled and logged once with the cause; a server stop logs kept/disabled/run/refused counts. Use, attack, block
  break, entity load and level load/unload are identified but not fired yet. Measured on `--dedicated` (fast
  profile, flat world, no players): Cupboard alone ran its `SERVER_STARTED` and `END_SERVER_TICK` listeners 557 times
  in 28 s with no refusal; the 48 corpus jars with server listeners kept 138 listeners from 42 jars, of which 105
  were still enabled at stop after 13,865 runs in 45 s. Join, leave and death were covered by tests only, not by a
  run with a connected player. `java-runtime.json` does not carry listener counts yet (only the stop log line).
- An opt-in NeoForge loader preflight: when `HORIZON_NEOFORGE_SERVER` (an installed server) and `HORIZON_JVM_BIN` are
  set, the integrated server runs `horizon-jvm --real-loader --entry-kind neoforge` once before its native batch in a
  bounded child process and reports what FancyModLoader saw (completion, mod count, sorted ids, first error).
  A missing binary or server is a report line, not a failure; only `HORIZON_NEOFORGE_STRICT=1` stops startup.
- `server-status` carries bounded `java` and `fml` sections: per-jar loader, engine, backend, weave label, executed
  entrypoints, registrations, opaque rows and isolation failures, `payload_channels`, and the preflight's
  status/role/jars/mods/ids/error. Refused jars are rows too (listed first, up to 8, `engine = "refused"`, the
  runner's whole cause in `refusal`), and `jars`/`started`/`refused` and the engine counts include them. The dedicated
  log prints one line per started jar and one bounded `skipping` line. `java-runtime.json` (schema v1, 256 jars,
  256 KiB cap) is written under `HORIZON_HOME` for launcher and support tooling: one row per jar given, refused ones
  with `refusal`, started ones with the first 64 skipped `plans` (mixin, target, method, injector, reason; the
  loader-relevant ones first), `skipped_by_reason` counts and the jar's cache `sha256`. Plan lists shrink before rows
  are dropped when the file would pass its cap. The launcher's runtime panel shows all of it per mod, with a "Clear
  runtime cache" action that removes that jar's `java-content-cache/<sha256>-<role>.json` entries.

### `horizon-jvm` (the reference runner, not the native path)

A standalone JVM runner for the same jars: real Fabric Loader discovery/freeze/entrypoints, an opt-in Knot class
loader (classpath unlock, transformer chain, `--launch-game` against a real 26.3 server), a real
FancyModLoader/NeoForge lifecycle (`--real-loader`, `--list-fml-mods`, `--fml-report`), datagen entrypoints, bounded
mixin-config registration and `--measure`. It is development/reference tooling: no game path runs a world or a jar's
gameplay through it (the only in-game use is the opt-in FML preflight child above), so `backends.jvm` in
`server-status` is always zero and a refused jar is skipped rather than handed to it. A Knot-backed run hands Fabric
a copy of any supplied jar that shades the platform logging classes (Axiom's slf4j 1.x), without the classes the
game's logging jars already provide, so the game's copy wins as in Fabric's own class path order.

### Tools and tests

`tools/real_mod_corpus.py` (outer + nested probe with a verdict per jar), `tools/java_mod_server_ledger.py` (one real
dedicated server per jar, verdict from its log, `java-runtime.json` and plan report as
`native-ok`/`needs-loader`/`native-gap`; `--with-deps` runs a jar with its declared closure and labels an
unresolvable one `missing-dependency`; `--docs` regenerates the
table below), `tools/java_mod_loader_ledger.py` (real Fabric loader),
`tools/real_mod_fixtures/build.py` (source-only Fabric/NeoForge/legacy-Forge fixtures), the
`real-mod-corpus` workflow (fixtures and harness self-test only, never third-party jars), the fixture suites under
`crates/horizon-native-java/tests/` and `crates/horizon-jvm/tests/` (the latter skipped unless a Minecraft jar,
loader and libraries are supplied), `tools/native_java_smoke.py`, and the efficiency harness (`tools/efficiency/`,
[efficiency.md](efficiency.md)) behind the numbers below.

## Measured corpus

Two Fabric 26.3 corpora, each jar run once through the real dedicated server
(`horizon-client --java-mod <jar> --dedicated`, fresh scratch home, flat world, `online-mode=false`, 60 s bound, one
process group killed after `Done (`). The [nightly Java corpus workflow](../.github/workflows/nightly-java-corpus.yml)
runs this same probe and ledger over jars already held by its runner. Raw reports:
`~/horizon-corpus/{ledger-wide,ledger-mega}(-classified).{txt,json}`, analysed in `ledger-wide-report.md` and
`mega-report.md`; the committed ledgers are `tools/real_mod_fixtures/LAST_SERVER_LEDGER_{223,500}.txt`.

| Corpus | Jars | Ran | Refused (native gap) | Skipped | Wall to `Done` per jar |
|---|---|---:|---:|---:|---|
| wide (`corpus-xl`: 6 packs, SHA-256 deduped) | 223 | 94 (42%) | 27 (12%) | 102 (46%) | mean 2.42 s, max 5.66 s |
| mega (500 unique jars from 50 Fabric 26.3 packs) | 500 | 197 (39%) | 68 (14%) | 235 (47%) | mean 2.26 s, max 8.75 s |

Skipped jars are out of scope for a server, not failures: 69 are client-only and 33 are libraries in the wide
corpus, 180 and 55 in mega. Of the jars that ran, 13 (wide) and 37 (mega) ran with engine `native` (no skipped plan);
the rest ran `native-qualified`. **No mixin was woven in either corpus**: 2,944 plans were skipped in the wide corpus
and 7,742 in mega. Every `ran` row therefore has `woven=0`: the load-time weaver only rewrites targets found in the
same jar, while real mods' plans target `net/minecraft` classes and Horizon has no Java class for those targets. The
dominant skip reason is `target-outside-jar`; 45 (wide) and 86 (mega) of the running jars have at least half of
their skipped plans aimed at the game and are labelled `needs-loader`.

Refusals split by cause (the ledger's own categories): 9 wide / 29 mega fail decoding class metadata (bounded
collection markers, `ServiceLoader` and stream markers, `String.split`/`Map#keySet` null receivers, `Gson` needing an
`Appendable`, malformed `fabric.mod.json`), and 18 / 34 fail with an uncaught exception in the entrypoint (a null
config or listener phase, reflection on a null class, a mixed-loader guard, a mixin plugin that did not initialise).
Mega adds one verifier refusal (`StackUnderflow` in spell_engine's datagen), two jars that exceed the interpreter's
128 nested-jar-frame bound, one duplicate native item declaration, and one jar whose metadata cannot be read at all.
The Kotlin entrypoint adapter now runs (`fzzy_config` starts), so no refusal is attributed to it. The ledger marks
27 / 68 rows as `native-gap`, 45 / 86 as `needs-loader`, and the remainder, 151 / 346, as `native-ok` (client-only,
libraries, or already running).

The role-agnostic probe over the same 500 jars (`real_mod_corpus.py`) reports **167 ok (33.4%), 303 fallback (60.6%),
30 unsupported (6.0%)**, with the top non-ok groups being "no main entrypoint but needs the loader/embedded JVM",
client-only entrypoints, and malformed metadata.

## What runs native, what needs the loader, why

Every ledger row ends in one of three labels (four with `--with-deps`), so a verdict says what to do instead of
counting a refusal:

- `native-ok`: the dedicated server runs the jar on its own. Either it executes its entrypoint and no skipped plan
  aims at the game, or it is a client-only jar or a library the server role never loads.
- `needs-loader`: it runs, but at least half of its skipped mixin plans target game classes (`target-outside-jar`), so
  only a real loader applies them.
- `native-gap`: native execution refuses it yet; [Known gaps](#known-gaps) and the row's reason say where.
- `missing-dependency` (`--with-deps` only): the jar's declared closure needs a mod id no corpus jar provides, so no
  loader would start it either; the ledger does not run it.

The default ledger starts one server per jar, so a jar that declares a dependency (archers on `spell_engine`, zoomify
on YACL) runs without it, which a real loader never does. `--with-deps` runs each jar with its declared closure
(`fabric.mod.json` `depends`, `mods.toml`/`neoforge.mods.toml` required dependencies, resolved through `provides`
aliases and jar-in-jar modules against the corpora under `~/horizon-corpus`); the row stays the jar's own, and
`deps=`, `dep_refused=` (a dependency whose entrypoint was refused; libraries with nothing to run are not counted)
and `blocked_in=` (the dependency whose class the refusal stopped in) say what the closure did. When a jar is refused
both alone and with the pack class path and the two causes differ, the game reports
`with the pack class path: <cause>; alone: <cause>`: the linked attempt runs the same code until the first class
alone lacked, so its cause is the blocker the pack hits, and the ledger categorises the row by it.

Measured on 7 November 2026 (one build, the 95 `native-gap` rows of the two committed ledgers): one jar per server,
15 stay `native-gap` and 80 leave it from native fixes landed since those ledgers (68 run: 31 `native-ok`, 37
`needs-loader`; 12 are now read as client-only, `native-ok`); with the closure, 14 stay `native-gap`, 1 more runs
(archers, `needs-loader`) and none is `missing-dependency`. The closure moves the blocker more than the label:
wizards and paladins now stop in `spell_power`'s `SpellSchools.createMagic`, zoomify in YACL's
`JsonFileCodecConfig.saveToFile`. The native host already answers a required mod's presence, so most dependents
ran alone too.

The block below is generated from the wide corpus's committed ledger, and the tool rewrites it in place
(`--docs`), so it cannot drift from the ledger format:

<!-- java-mod-ledger:start -->
_Generated by `tools/java_mod_server_ledger.py --docs docs/java-mod-compat-status.md` from `tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt`; edit the tool, not this block._

**223 jars: 151 `native-ok`, 45 `needs-loader`, 27 `native-gap`.**

| Label | Jars | What it means | Why |
|---|---:|---|---|
| `native-ok` | 151 | the dedicated server runs it on its own | 69 client-only (the server role has no entrypoint to run); 36 ran, every skipped plan stayed inside the jar; 33 libraries (nothing for the server to execute); 13 ran with no plan skipped |
| `needs-loader` | 45 | it runs, but its plans target the game, so only a real loader applies them | 45 ran, but their skipped plans target the game |
| `native-gap` | 27 | native execution cannot carry it yet | 18 refused by an uncaught exception in its entrypoint; 9 refused while decoding metadata |

**Jars that are not `native-ok` (72).**

| Jar | Label | Why |
|---|---|---|
| `1283899-8898481.jar` | `needs-loader` | ran; 25 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `248787-8907912.jar` | `needs-loader` | ran; 11 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `326652-8890593.jar` | `needs-loader` | ran; 9 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `348831-8926733.jar` | `needs-loader` | ran; 18 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `367706-8917645.jar` | `needs-loader` | ran; 277 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `368825-8901843.jar` | `needs-loader` | ran; 9 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `410295-8911219.jar` | `needs-loader` | ran; 8 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `419699-8915075.jar` | `needs-loader` | ran; 124 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `456640-8026052.jar` | `needs-loader` | ran; 1 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `511733-8929530.jar` | `needs-loader` | ran; 10 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `526901-8925000.jar` | `needs-loader` | ran; 1 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `608235-8905060.jar` | `needs-loader` | ran; 15 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `639842-8906536.jar` | `needs-loader` | ran; 38 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `734339-8931641.jar` | `needs-loader` | ran; 4 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `832882-8896904.jar` | `needs-loader` | ran; 31 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `882495-8910991.jar` | `needs-loader` | ran; 2 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `888790-8002356.jar` | `needs-loader` | ran; 5 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `892086-8902974.jar` | `needs-loader` | ran; 8 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `BedrockEntityModels-26.3-1.1.2-fabric.jar` | `needs-loader` | ran; 15 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `BedrockSkins-26.3-2.1.1.jar` | `needs-loader` | ran; 88 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `CustomPlayerModels-Fabric-26.3-0.6.27e.jar` | `needs-loader` | ran; 60 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `InventoryInteractions-2.2.0+26.3+fabric.jar` | `needs-loader` | ran; 6 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `InventoryParticles-3.2.0+26.3+fabric.jar` | `needs-loader` | ran; 20 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `Jade-mc26.3-Fabric-26.3.5.jar` | `needs-loader` | ran; 22 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `PatPat-1.3.2+26.3+fabric.jar` | `needs-loader` | ran; 31 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `advancementscreenshot-26.3.0-5.3.jar` | `needs-loader` | ran; 6 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `architectury-fabric-22.0.4.jar` | `needs-loader` | ran; 124 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `armor-stand-poses-fabric-26.3-1.4.0.jar` | `needs-loader` | ran; 6 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `baguettelib-26.3-Fabric-2.0.7.jar` | `needs-loader` | ran; 19 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `bannerpoint-fabric-1.1.2+26.3.jar` | `needs-loader` | ran; 17 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `cropsloverain-3.2.1.jar` | `needs-loader` | ran; 16 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `custom-glow-enchantment-outline-fabric-26.3-1.2.3.jar` | `needs-loader` | ran; 23 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `deleteworldstotrash-v26.3.0-mc26.3.x+fabric.jar` | `needs-loader` | ran; 4 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `fix-keyboard-on-linux-1.0.2.jar` | `needs-loader` | ran; 2 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `immersive-hotbar-1.2.2-26.3.jar` | `needs-loader` | ran; 20 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `jei-26.3-fabric-31.16.0.83.jar` | `needs-loader` | ran; 24 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `light-overlay-2.13.0+26.3.jar` | `needs-loader` | ran; 3 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `mcwifipnp-2.1.4-26.3-fabric.jar` | `needs-loader` | ran; 4 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `modernregsyncfix-2.0.0.jar` | `needs-loader` | ran; 5 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `naturally_trimmed-3.6.1+26.3-fabric.jar` | `needs-loader` | ran; 5 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `peek-fabric-1.6.0+26.3.jar` | `needs-loader` | ran; 40 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `simplemenu-26.3.0-2.3.jar` | `needs-loader` | ran; 39 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `smooth_f5-fabric-1.3.2.jar` | `needs-loader` | ran; 4 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `takeitout-1.1.28-26.3.jar` | `needs-loader` | ran; 20 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `titlechanger-1.4.8+mc26.3.jar` | `needs-loader` | ran; 1 plan(s) skipped, at least half aimed at the game (`target-outside-jar`) |
| `257814-8894326.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/AssertionError (at team/creative/creativecore/mixi…` |
| `342584-8921261.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/NullPointerException: Tried to register a listener…` |
| `531761-8889669.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/IllegalArgumentException: Invalid version range '[…` |
| `623764-8914234.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/IllegalStateException: Could not determine mod loa…` |
| `Legacy4J-26.3-1.9.1-fabric.jar` | `native-gap` | refused while decoding metadata: `Metadata("malformed java/lang/Boolean marker (at wily/legacy/init/LegacyRegistr…` |
| `betterstats-5.6.0-beta.2+fn-26.3.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/IllegalStateException: Unexpected subclass com/t...` |
| `betterstats-5.7.0+fn-26.3.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/IllegalStateException: Unexpected subclass com/the…` |
| `clientsort-fabric-3.104.1+26.3.jar` | `native-gap` | refused while decoding metadata: `Metadata("expected a native collection marker, got null (call java/util/Map#key…` |
| `collective-26.3.0-8.42.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/NullPointerException: Tried to register a listener…` |
| `collective-26.3.0-8.46.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/NullPointerException: Tried to register a listener…` |
| `kreno_fpatcher-26.2.0+26.1.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/SecurityException: Unsupported mod detected: bukki…` |
| `main-menu-credits-1.4.0+26.2-universal.jar` | `native-gap` | refused while decoding metadata: `Metadata("java/util/ServiceLoader has no explicitly seeded providers fo...` |
| `moderner-beta-fabric-5.0.0-alpha.4+26.3.jar` | `native-gap` | refused while decoding metadata: `Metadata("array index 0 out of bounds (at mod/bluestaggo/modernerbeta/...` |
| `overlaylib-0.12.1+26.3.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/NullPointerException: reflection on a null Class (…` |
| `pickupnotifications-fabric-3.0.1+26.3.jar` | `native-gap` | refused by an uncaught exception: `uncaught exception java/lang/NullPointerException: codec (at me/roundaro...` |

_12 more row(s) are in the ledger, not this table._
<!-- java-mod-ledger:end -->

After a ledger run, regenerate the block without touching a jar set again:

```sh
python3 tools/java_mod_server_ledger.py --from tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt \
  --docs docs/java-mod-compat-status.md
```

`--table` prints the same block without writing it, and `--table-limit N` sets how many per-jar rows it keeps.

### Tier coverage over the whole corpus

Every jar in both corpora gets exactly one tier. `native-ok` means the dedicated server runs it, or the
server role has nothing to run (client-only and library jars). `needs-loader` means native execution cannot
carry it but a real Fabric loader launch of its pack lists it; the row also says whether that launch reached
`Done (`. `native-gap` means neither tier carries it, with the cause. The block below is generated from the
two committed server ledgers and the two committed loader proof ledgers:

<!-- java-mod-coverage:start -->
_Generated by `tools/hybrid_pack.py --coverage --docs docs/java-mod-compat-status.md` from `LAST_SERVER_LEDGER_223.txt`, `LAST_SERVER_LEDGER_500.txt`, `LAST_LOADER_PROOF_223.txt`, `LAST_LOADER_PROOF_500.txt`; edit the tool, not this block._

**599 unique jars (723 rows across 2 corpora): 446 `native-ok`, 150 `needs-loader`, 0 `native-gap`, 3 out of scope.**

| Corpus | Jars | `native-ok` | `needs-loader` | `native-gap` | out of scope |
|---|---:|---:|---:|---:|---:|
| `corpus-xl` | 223 | 164 | 59 | 0 | 0 |
| `mega` | 500 | 382 | 115 | 0 | 3 |
| union (SHA-256) | 599 | 446 | 150 | 0 | 3 |

**Loader proof.** 169 of 174 `needs-loader` rows are proven: a real Fabric loader launch lists the jar, reaches `Done (` and no failure line of that run names its mod id. 5 are listed without that proof (below), and 0 have no loader evidence.

**Native refusals absorbed by the loader tier (80 rows).** They are `native-gap` in the server ledger; the real loader lists them, so the tier coverage counts them as `needs-loader`.

| Cause | Rows |
|---|---:|
| runtime | 46 |
| metadata | 30 |
| other | 2 |
| declaration | 1 |
| verifier | 1 |

**Out of scope (3).** Built for another game version, so no 26.3 tier can carry them; they belong out of the corpus.

| Jar | Corpus | Why |
|---|---|---|
| `1bb71c0df6-coroutil-fabric-1.21.1-1.3.8.jar` | `mega` | built for another game version (requires minecraft 1.21.x); it cannot run on 26.3 |
| `949fa8a29a-show-me-your-skin-2.0.3+26.2.jar` | `mega` | built for another game version (requires minecraft >=26.2 <26.3); it cannot run on 26.3 |
| `c90f03eedd-noisium-fabric-2.8.5+mc26.2-pre-2.jar` | `mega` | built for another game version (requires minecraft ~26.2-); it cannot run on 26.3 |

**Listed but not proven (5).** The loader discovered these jars, but the launch did not reach `Done (` or a failure line names them.

| Jar | Corpus | Loader id | Why |
|---|---|---|---|
| `257814-8894326.jar` | `corpus-xl` | `creativecore` | native refused (runtime); the real Fabric loader lists it but the run did not boot (client-only code on a server) |
| `3c1a43daba-sway-2.4.5-fabric+26.3.jar` | `mega` | `sway` | native refused (metadata); the real Fabric loader lists it but the run did not boot (client-only code on a server) |
| `4ed60c0212-combat-hitboxes-1.0.11.jar` | `mega` | `combat-hitboxes` | the real Fabric loader lists it but the run did not boot (client-only code on a server) |
| `80a0fc2ac1-CreativeCore_FABRIC_v2.14.16_mc26.3.jar` | `mega` | `creativecore` | native refused (runtime); the real Fabric loader lists it but the run did not boot (client-only code on a server) |
| `moderner-beta-fabric-5.0.0-alpha.4+26.3.jar` | `corpus-xl` | `moderner_beta` | native refused (metadata); the real Fabric loader lists it and reached Done, but a failure names it: [11:12:13] [Thread-0/ERROR]: Registry 'moderner_beta:blocksource' was empty after loading |
<!-- java-mod-coverage:end -->

A row whose reason says `requires minecraft …` is out of scope: the jar was built for another game version
(1.21, 26.2), so no 26.3 tier can carry it and it belongs out of the corpus rather than in a compatibility
fix. Of the five `listed but not proven` rows, four ship client code the server role loads: CreativeCore 2.14.16
(both copies) and sway put client mixins (`GameRenderer`, `LevelRenderer`) in a common config, and
combat-hitboxes' `main` entrypoint reaches client classes. A plain `KnotServer` launch of sway or CreativeCore
with Fabric API, outside Horizon, fails the same way, so this is the jar's side, not the runner. They stay
`needs-loader` because a client pack uses the loader half; proving them needs a client launch, which the
server corpus does not carry. The fifth, moderner_beta, reaches `Done (` but logs
`Registry 'moderner_beta:blocksource' was empty after loading` at ERROR, which the proof does not wave through.

Re-proved 7 November 2026 with every row launched again (`--reprove`): the previous ledgers had counted
corpus-xl's CreativeCore as booted from a run that applied no mixins, and both Axiom versions as not booted.
Axiom shades slf4j 1.x, which muted the launched server's log (fixed in `horizon-jvm`, see
[Hybrid sessions](#hybrid-sessions-a-native-half-and-a-loader-half)); a launch now records the first failure
line naming each mod (`exception=`), and a row with one is not proven.

Regenerate it without starting a game:

```sh
python3 tools/hybrid_pack.py --coverage \
  --corpus corpus-xl=~/horizon-corpus/corpus-xl --corpus mega=~/horizon-corpus/mega \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_500.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_223.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_500.txt \
  --docs docs/java-mod-compat-status.md
```

`--measure-proof` fills the proof ledgers: for every row without a proof it launches the real loader with
the jar's declared dependency closure (`provides` aliases included), splits a batch that cannot boot, gives an
unproven target one run of its own, and retries with each `--proof-loader`. `--reprove` launches every row
again instead of trusting the committed listings, and `--proof-ledger NAME=PATH` writes a corpus's proof
ledger in the committed format. The sweep that wrote the committed ledgers (about 190 launches, one JVM at a
time, the runner's default `-Xmx1g`):

```sh
python3 tools/hybrid_pack.py --coverage \
  --corpus corpus-xl=~/horizon-corpus/corpus-xl --corpus mega=~/horizon-corpus/mega \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_223.txt \
  --ledger tools/real_mod_fixtures/LAST_SERVER_LEDGER_500.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_223.txt \
  --loader-ledger tools/real_mod_fixtures/LAST_LOADER_PROOF_500.txt \
  --measure-proof --reprove --proof-universe ~/horizon-corpus/mega-deps \
  --proof-loader ~/horizon-corpus/fabric-libs-98/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar \
  --proof-loader ~/horizon-corpus/fabric-loader-0.19.5.jar \
  --proof-ledger corpus-xl=tools/real_mod_fixtures/LAST_LOADER_PROOF_223.txt \
  --proof-ledger mega=tools/real_mod_fixtures/LAST_LOADER_PROOF_500.txt
```

`--strict` exits non-zero while a `native-gap` or unproven row remains.

### Phase 2 score: native-only over the loader tier

The phase-1 table says which jars the loader carries. Phase 2 asks the other question of the same rows: with
the loader gone, does a fresh native-only dedicated run carry the jar on its own?
`tools/hybrid_pack.py --native-score` runs each unique `needs-loader` jar exactly like the committed server
ledger — one `horizon-client --java-mod <jar> --dedicated` process, flat world, 60 s bound — reads its
`java-runtime.json` row and its plan report, and calls it `loader-free` only when the run reaches `Done (`,
executes a server entrypoint and has no server-side plan whose target lives outside the jar. A jar
that still has such a plan is `loader`; a refusal or a run that never reaches `Done (` is `refused`. Client-only
mixins do not count against the server role. The same bytes in both corpora run once and answer both rows. The
block below is generated, and the trend keeps one point per measurement so a fix that moves a jar is visible:

<!-- java-mod-native-score:start -->
_Generated by `tools/hybrid_pack.py --native-score --docs docs/java-mod-compat-status.md` from `LAST_SERVER_LEDGER_223.txt`, `LAST_SERVER_LEDGER_500.txt`, `LAST_LOADER_PROOF_223.txt`, `LAST_LOADER_PROOF_500.txt`; edit the tool, not this block._

**Phase 2 score: 124 of 174 `needs-loader` rows (150 unique jars) run natively on a fresh native-only run; 30 are loader-free, 94 still need the loader, 50 refused, 0 skipped, 0 missing.**

| Status | Rows | What it means |
|---|---:|---|
| `loader-free` | 30 | runs, and no server-side plan targets a class outside the jar |
| `loader` | 94 | runs, but still skips at least one server-side game-class plan |
| `refused` | 50 | native execution refuses it or the run did not reach `Done (` |

**Trend.** One point per measurement: a rerun with the same commit, binary and counts replaces the last row instead of adding a duplicate.

| Date (UTC) | Commit | Binary | Hooks | Rows | Unique | `loader-free` | still need loader | refused | plans skipped | game plans |
|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|
| 2026-11-06T12:08:39Z | `efca454f4` | 20261106-2255 | off | 174 | 150 | 30 | 94 | 50 | 5028 | 1285 |

**Loader-free (30).** The run reached `Done (` and no server-side plan targets a class outside the jar.

| Jar | Corpus | Engine | Skipped | Server plans |
|---|---|---|---:|---:|
| `1283899-8898481.jar` | `corpus-xl` | native-qualified | 25 | 0 |
| `368825-8901843.jar` | `corpus-xl` | native-qualified | 13 | 0 |
| `410295-8911219.jar` | `corpus-xl` | native-qualified | 8 | 0 |
| `526901-8925000.jar` | `corpus-xl` | native-qualified | 1 | 0 |
| `734339-8931641.jar` | `corpus-xl` | native-qualified | 4 | 0 |
| `882495-8910991.jar` | `corpus-xl` | native-qualified | 2 | 0 |
| `BedrockSkins-26.3-2.1.1.jar` | `corpus-xl` | native-qualified | 90 | 0 |
| `PatPat-1.3.2+26.3+fabric.jar` | `corpus-xl` | native-qualified | 31 | 0 |
| `advancementscreenshot-26.3.0-5.3.jar` | `corpus-xl` | native-qualified | 6 | 0 |
| `betterstats-5.6.0-beta.2+fn-26.3.jar` | `corpus-xl` | native-qualified | 1 | 0 |
| `betterstats-5.7.0+fn-26.3.jar` | `corpus-xl` | native-qualified | 1 | 0 |
| `clientsort-fabric-3.104.1+26.3.jar` | `corpus-xl` | native-qualified | 31 | 0 |
| `fix-keyboard-on-linux-1.0.2.jar` | `corpus-xl` | native-qualified | 2 | 0 |
| `simplemenu-26.3.0-2.3.jar` | `corpus-xl` | native-qualified | 39 | 0 |
| `smooth_f5-fabric-1.3.2.jar` | `corpus-xl` | native-qualified | 8 | 0 |
| `0da0ce012a-betterstats-5.7.0+fn-26.3.jar` | `mega` | native-qualified | 1 | 0 |
| `26baec9303-NoChatRestrictions-Fabric-MC26.3-v1.3.0.jar` | `mega` | native-qualified | 5 | 0 |
| `2c0e2ba8a9-midnightlib-fabric-1.9.3+26.3-rc-2.jar` | `mega` | native-qualified | 3 | 0 |
| `4ed60c0212-combat-hitboxes-1.0.11.jar` | `mega` | native-qualified | 3 | 0 |
| `53d859f95b-betterstats-5.6.0-beta.2+fn-26.3.jar` | `mega` | native-qualified | 1 | 0 |
| `5a2f06f60a-betterstats-5.8.0+fn-26.3.jar` | `mega` | native-qualified | 1 | 0 |
| `61412064ba-konkrete_fabric_1.11.1_MC_26.3.jar` | `mega` | native-qualified | 8 | 0 |
| `7751ee3925-locator-heads-2.5.0.jar` | `mega` | native-qualified | 7 | 0 |
| `96cbf8e292-clientsort-fabric-3.104.1+26.3.jar` | `mega` | native-qualified | 31 | 0 |
| `9c77a694f8-bkupsV3-fabric-mod-26.3.jar` | `mega` | native-qualified | 5 | 0 |
| `9ce277e691-particular-26.3-Fabric-1.5.7.jar` | `mega` | native-qualified | 36 | 0 |
| `cc53b1cd2b-fullbrightnesstoggle-26.3.0-4.9.jar` | `mega` | native-qualified | 6 | 0 |
| `dea53a10e5-CrashAssistant-fabric-26.2-26.3-1.11.14.jar` | `mega` | native-qualified | 7 | 0 |
| `e878fad023-PlayerAnimationLibMerged-1.2.8+mc.26.3.jar` | `mega` | native-qualified | 25 | 0 |
| `f0b01c55aa-midnightlib-fabric-1.10.0+26.3.jar` | `mega` | native-qualified | 3 | 0 |

**Still needs the loader (94).**

| Jar | Corpus | Server game plans | Server plans |
|---|---|---:|---:|
| `248787-8907912.jar` | `corpus-xl` | 2 | 2 |
| `326652-8890593.jar` | `corpus-xl` | 4 | 4 |
| `342584-8921261.jar` | `corpus-xl` | 79 | 79 |
| `348831-8926733.jar` | `corpus-xl` | 15 | 15 |
| `367706-8917645.jar` | `corpus-xl` | 3 | 3 |
| `419699-8915075.jar` | `corpus-xl` | 45 | 45 |
| `456640-8026052.jar` | `corpus-xl` | 1 | 1 |
| `511733-8929530.jar` | `corpus-xl` | 2 | 2 |
| `531761-8889669.jar` | `corpus-xl` | 26 | 26 |
| `608235-8905060.jar` | `corpus-xl` | 4 | 4 |
| `623764-8914234.jar` | `corpus-xl` | 6 | 6 |
| `639842-8906536.jar` | `corpus-xl` | 6 | 6 |
| `832882-8896904.jar` | `corpus-xl` | 16 | 16 |
| `888790-8002356.jar` | `corpus-xl` | 1 | 1 |
| `CustomPlayerModels-Fabric-26.3-0.6.27e.jar` | `corpus-xl` | 3 | 3 |
| `Jade-mc26.3-Fabric-26.3.5.jar` | `corpus-xl` | 2 | 2 |
| `architectury-fabric-22.0.4.jar` | `corpus-xl` | 45 | 45 |
| `armor-stand-poses-fabric-26.3-1.4.0.jar` | `corpus-xl` | 5 | 5 |
| `baguettelib-26.3-Fabric-2.0.7.jar` | `corpus-xl` | 12 | 12 |
| `bannerpoint-fabric-1.1.2+26.3.jar` | `corpus-xl` | 10 | 10 |
| `collective-26.3.0-8.42.jar` | `corpus-xl` | 81 | 81 |
| `collective-26.3.0-8.46.jar` | `corpus-xl` | 81 | 81 |
| `cropsloverain-3.2.1.jar` | `corpus-xl` | 8 | 8 |
| `kreno_fpatcher-26.2.0+26.1.jar` | `corpus-xl` | 2 | 2 |
| `mcwifipnp-2.1.4-26.3-fabric.jar` | `corpus-xl` | 1 | 1 |
| `modernregsyncfix-2.0.0.jar` | `corpus-xl` | 1 | 1 |
| `puzzleslib-v26.3.12-mc26.3.x+fabric.jar` | `corpus-xl` | 49 | 49 |
| `puzzleslib-v26.3.13-mc26.3.x+fabric.jar` | `corpus-xl` | 49 | 49 |
| `puzzleslib-v26.3.14-mc26.3.x+fabric.jar` | `corpus-xl` | 49 | 49 |
| `swinginglanterns-fabric-26.3-2.0.3.jar` | `corpus-xl` | 6 | 6 |
| `takeitout-1.1.28-26.3.jar` | `corpus-xl` | 4 | 4 |
| `tcdcommons-5.6.0-beta.2+fn-26.3.jar` | `corpus-xl` | 2 | 2 |
| `tcdcommons-5.7.0+fn-26.3.jar` | `corpus-xl` | 2 | 2 |
| `02d17d2f1c-Axiom-6.1.5-for-MC26.3.jar` | `mega` | 24 | 24 |
| `03280769d5-baguettelib-26.3-Fabric-2.0.7.jar` | `mega` | 12 | 12 |
| `0610f18c4f-fancymenu_fabric_3.9.14_MC_26.3.jar` | `mega` | 3 | 3 |
| `0c6898e27d-armor-stand-poses-fabric-26.3-1.4.0.jar` | `mega` | 5 | 5 |
| `1079f41d14-otterlib-0.4.0.2+26.3-fabric.jar` | `mega` | 1 | 1 |
| `1316efc419-alternate-current-mc26.3-1.9.0.jar` | `mega` | 8 | 8 |
| `1b44d97048-balm-fabric-26.3-26.3.0.3.jar` | `mega` | 26 | 26 |

_54 more row(s) are in the score ledger._

**Not runnable natively (50).**

| Jar | Corpus | Status | Why |
|---|---|---|---|
| `257814-8894326.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/AssertionError (at team/creative/creativecore/mixin/ArgumentTypeIn... |
| `Legacy4J-26.3-1.9.1-fabric.jar` | `corpus-xl` | `refused` | Metadata("Stream.collect needs a collector (at wily/legacy/Legacy4J#<clinit> +20)") |
| `jei-26.3-fabric-31.16.0.83.jar` | `corpus-xl` | `refused` | Metadata("java/util/ServiceLoader has no explicitly seeded providers for net/mezzde... |
| `moderner-beta-fabric-5.0.0-alpha.4+26.3.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/ArrayIndexOutOfBoundsException: Index 0 o... |
| `naturally_trimmed-3.6.1+26.3-fabric.jar` | `corpus-xl` | `refused` | Metadata("unsupported String.format conversion %0 (at net/majo24/naturally... |
| `peek-fabric-1.6.0+26.3.jar` | `corpus-xl` | `refused` | Metadata("map entry marker must contain key and value (at de/maxhenkel/peek/configbuild... |
| `pickupnotifications-fabric-3.0.1+26.3.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/NullPointerException: codec (at me/roundaro... |
| `servercore-fabric-1.5.20+26.3.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/NullPointerException: charset (at space/arim/dazzle... |
| `whiteout-0.21.0.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/IllegalStateException: The mixin plugin did not initialize the co... |
| `xaeroworldmap-fabric-26.3-1.47.0.jar` | `corpus-xl` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/common/confi... |
| `zoomify-2.17.0+mc26.3-universal.jar` | `corpus-xl` | `refused` | Metadata("malformed java/lang/Boolean marker (at dev/isxander/zoomify/config/Z... |
| `0efe50ab35-icys-better-horses-2.1.4-fab-26.3.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalArgumentException: breed thoroughb... |
| `11bd97b82a-xaeros-world-map-fabric-26.3-1.46.4.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/com... |
| `1282302b6c-xaeroworldmap-fabric-26.3-1.47.2.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/common... |
| `17aff6842e-almanac-fabric-26.3-1.26.10.1.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException: reflection on a null Cl... |
| `1af0d1f40d-collectorsalbum-fabric-2.9.0+26.3.jar` | `mega` | `refused` | Metadata("expected a native stream marker (at team/tnt/collectorsalbum... |
| `1b3681bc98-CreativeCore_FABRIC_v2.14.19_mc26.3.jar` | `mega` | `refused` | uncaught exception java/lang/AssertionError (at team/creative/creati... |
| `2344273592-voicechat-fabric-2.6.27+26.3.jar` | `mega` | `refused` | Metadata("map entry marker must contain key and value (at de/maxhenkel/voic... |
| `286a800d23-emotecraft-for-MC26.3-3.5.0-b.build.179.jar` | `mega` | `refused` | Metadata("expected a native stream marker (at io/github/kosmx/em... |
| `287f670348-xaerominimap-fabric-26.3-26.6.0.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/common/... |
| `28b6739189-xaeros-minimap-fabric-26.3-26.5.3.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/commo... |
| `2ac4bb844e-simple-voice-chat-fabric-2.6.24+26.3.jar` | `mega` | `refused` | Metadata("map entry marker must contain key and value (at de/maxhen... |
| `36e8de6c68-carryon-fabric-26.3-2.12.0.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalArgumentException: Config supplied does n... |
| `3b90f638d6-jei-26.3-fabric-31.9.0.61.jar` | `mega` | `refused` | Metadata("java/util/ServiceLoader has no explicitly seeded providers for net/m... |
| `4881abae4b-archers-fabric-3.1.4+26.3.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException (at net/archers/entity/Arche... |
| `554319fbea-syncmatica-fabric-26.3-0.3.20.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException: codec (at net/fabricmc/... |
| `564c46b80e-carpet-26.3.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException (at carpet/script/Module#<init> +5) |
| `65ac95d621-FarmersDelight-26.3-3.6.28+refabricated.jar` | `mega` | `refused` | native registration id "minecraft:" is invalid |
| `691967a54e-FarmersDelight-26.3-3.6.30+refabricated.jar` | `mega` | `refused` | native registration id "minecraft:" is invalid |
| `6f9aebaa6c-TerraBlender-fabric-26.3-26.3.0.0.9.jar` | `mega` | `refused` | Metadata("malformed java/lang/Integer marker (at terrablender/config... |
| `7b10411d20-worldedit-7.4.6-beta-02.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException: Optional.orElseThrow supplier... |
| `7b1bf05fde-runes-fabric-1.3.2+26.3.jar` | `mega` | `refused` | duplicate native item runes:null_stone |
| `7f18090fea-zoomify-2.17.0+mc26.3-universal.jar` | `mega` | `refused` | Metadata("malformed java/lang/Boolean marker (at dev/isxander/zoomify/config/Z... |
| `80a0fc2ac1-CreativeCore_FABRIC_v2.14.16_mc26.3.jar` | `mega` | `refused` | uncaught exception java/lang/AssertionError (at team/creative/creativecore/mixin/ArgumentTypeIn... |
| `8f52bbe46b-armor-hider-fabric-0.14.4+26.3.jar` | `mega` | `refused` | uncaught exception java/lang/RuntimeException: Failed to register factori... |
| `8f7962199e-worldedit-mod-7.4.6.jar` | `mega` | `refused` | uncaught exception java/lang/NullPointerException: Optional.orElseThrow supplier ret... |
| `92815a8e4c-emotecraft-for-MC26.3-3.5.0-a.build.171.jar` | `mega` | `refused` | Metadata("expected a native stream marker (at io/github/kosmx/em... |
| `9ace0b0646-xaeroworldmap-fabric-26.3-1.47.0.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/common/confi... |
| `9b44e5718f-xaerominimap-fabric-26.3-26.6.2.jar` | `mega` | `refused` | uncaught exception java/lang/IllegalStateException (at xaero/lib/common/... |
| `9cc655e54a-cristellib-fabric-26.3-3.1.13.jar` | `mega` | `refused` | uncaught exception java/lang/RuntimeException: [cristellib] Tried to acces... |

_10 more row(s) are in the score ledger._
<!-- java-mod-native-score:end -->

Regenerate it with about 150 dedicated runs:

```sh
python3 tools/hybrid_pack.py --native-score --docs docs/java-mod-compat-status.md
```

`--score-limit N` runs a prefix as a smoke test, `--strict` exits 1 while a row still needs the loader,
`--score-game-hooks` measures the opt-in `HORIZON_JAVA_GAME_HOOKS` tier instead of the default weave, and
`--plan-probe` points at the `horizon-native-java` used for the server-side plan count.

### Hybrid sessions: a native half and a loader half

The label decides where a jar runs: `native-ok` in one `horizon-client --java-mod` process, `needs-loader` in
`horizon-jvm --real-loader --launch-game`, and `native-gap` in neither unless the loader proof lists it
(`--loader-ledger`, a listing that reached `Done (`) or `--gap-to-loader` routes every refusal there.
`tools/hybrid_pack.py` partitions a pack by these ledgers (falling back to SHA-256 when the pack's filenames differ
from the corpus), pulls every dependency of a loader jar into the loader half, moves a native jar whose dependency
only the loader half runs into the loader half too (`follows-dependency`, repeated until no native jar is
stranded), checks the pack's own declarations, and `--measure` runs both halves. The design and the two launch
commands are in [java-mod-compat.md](java-mod-compat.md#hybrid-sessions-a-native-half-and-a-loader-half).

Measured 7 November 2026 on the 112-jar Helix pack, matched to the two committed server ledgers by content hash
and to the two proof ledgers (horizon-client and horizon-jvm built from this branch that day; server role, flat
world, each half stops at `Done (`, 180 s bound):

| Half | Jars | Executed | Mixin plans applied | Wall |
|---|---:|---:|---:|---:|
| native-only (native half) | 71 | 11 entrypoints | 0 (891 skipped) | 3.6 s |
| hybrid — native half | 71 | 11 entrypoints | 0 (891 skipped) | 4.3 s |
| hybrid — loader half | 43 | 74 mods loaded, `Done (` | 477 (53 configs, 254 targets, 0 failed) | 15.3 s |

Both halves complete. The loader half carries 21 `needs-loader` jars, 8 `native-gap` jars the loader proof lists
(nochatreports, clientsort, collective, modernfix, puzzleslib, swinginglanterns, worldedit, xaeroworldmap), 10
`native-ok` libraries shared with the native half, and 4 `native-ok` jars that follow their dependency
(litematica-printer, pickupnotifier, stackrefill, wakes). The static check reports 0 conflicts (was 5
`cross-partition-dependency`) and 42 warnings: 34 `declared-break` version ranges it does not evaluate, 7 jars with
no ledger row, and the excluded sway. The loader half's log has one ERROR line, vanilla's
`No key layers in MapLike[{}]` from the flat world, and no exception. The native half is the same process as before
and still applies no game mixins; its entrypoint count moved from 18 to 11 because the four add-ons left it and the
binary changed under concurrent work, so the two native rows are this build's, not a trend.

The 6 November run that "timed out at 180 s while transforming `NaturalSpawner`" was not a hang. Axiom shades
slf4j 1.x's `org/slf4j/LoggerFactory`; the launch kept the game's logging jars parent-visible only, so Knot's
child-first lookup resolved Minecraft's logger from Axiom, SLF4J bound its NOP logger and every vanilla line,
`Done (` included, vanished while the server ran (a thread dump showed it waiting for its next tick, and it wrote
region files). Fabric's own launch adds the game's log jars to Knot before any mod, so the game's copy wins;
`horizon-jvm` now hands Fabric a copy of such a jar without the classes a logging jar on the class path already
provides. Axiom is the only corpus jar that shades them.

What is not shown: a client-role hybrid (the native half as the window, the loader half as a client), a pack with
players connected, and any pack but Helix. Sway stays out because it cannot boot a server under any loader.

## Measured efficiency

Host: 16 CPUs, load average 9.6–13 during the runs (shared box). Medians; client figures are medians of three
interleaved runs in rotated order, server figures median of six. The rerun recipe is [efficiency.md](efficiency.md)
(`tools/efficiency/`); this run's raw reports are in `~/horizon-corpus/` (`EFFICIENCY.md`, `efficiency-*.json`, and
the `efficiency-scripts/` the suite was written from).

### Dedicated server, vanilla 26.3 vs Horizon (no java mods), same `server.properties`

| Metric (median) | Vanilla 26.3 | Horizon | Difference |
|---|---:|---:|---:|
| Wall to listening port | 7.09 s | 3.22 s | **2.2× faster** |
| Wall to `Done (` | 7.86 s | 3.22 s | **2.4× faster** |
| Server's own `Done (x.xxxs)` timer | 0.61 s | 3.20 s | 5.2× slower, different span* |
| RSS at ready | 1312 MiB | 248 MiB | **5.3× less** |
| Threads at ready | 102 | 21 | **4.9× fewer** |

\* Vanilla's timer starts after ~7.3 s of JVM startup, registries, datafixers and its 68 MiB library bundle; Horizon's
3.20 s is effectively the whole process (resources 1.9 s, worldgen data 0.39 s, level prep). Wall clock is the honest
comparison, where Horizon wins by 4.6 s. Horizon opens the listening socket at `Done` (0.04 ms gap), so a client
never connects to a world that is not ready.

### Client scaling with native java mods (offscreen, render distance 8, `maxFps` 260)

0/3/20/74 jars are filename-sorted slices of the natively-runnable set, not a curated pack.

| Jars | Effective client mods | Startup | RSS | avg fps | p50 | p95 | low 1% |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 4080 ms | 1179 MiB | 1556 | 0.62 ms | 0.89 ms | 494 |
| 3 | 3 | 4150 ms | 1229 MiB | 1714 | 0.58 ms | 0.64 ms | 543 |
| 20 | 19 | 5150 ms | 1698 MiB | 1529 | 0.62 ms | 0.82 ms | 452 |
| 74 | 65 | 8410 ms | 2089 MiB | 1629 | 0.60 ms | 0.67 ms | 564 |

Frame time is mod-insensitive in this fixture: tick and update stay ~0.01 ms and render ~0.2 ms per frame; the mod
tax is startup (the Java entrypoint plus its share of resource work, ~60 ms per effective mod) and memory
(~14 MiB per mod). Threads stay at 61 regardless of jar count.

### Horizon dedicated startup with native jars

| Jars passed | Effective server mods | Time to listening port/`Done` | RSS | Threads |
|---:|---:|---:|---:|---:|
| 0 | 0 | 2.39 s | 265 MiB | 21 |
| 25 | 25 | 3.26 s | 544 MiB | 21 |
| 50 | 50 | 3.82 s | 700 MiB | 21 |
| 74 | 51 | 4.39 s | 695 MiB | 21 |
| 89 (all native-ok) | 52 | 5.22 s | 689 MiB | 21 |

Only 52 of the 89 natively-runnable jars declare a dedicated-server entrypoint, so the 74-jar point is 51 executed
mods and time/RSS flatten past 50. There were zero fallbacks to the JVM and zero startup failures in any run.

### 500-jar native profile

Measured on 2026-11-06 (16 CPUs) over the same 500 jars, one flat-world dedicated run per point, `Done (N.NNNs)!`
and peak RSS; a 0-jar run takes 2.2-2.4 s, so "jar work" is wall time over that same binary's baseline. Both rows
use **one binary**, and only `HORIZON_WORKER_THREADS` differs (`1` against the machine's 16): the difference is the
bootstrap pool, not another change landing between runs.

| 500 jars | Wall to `Done` | Jar work | Peak RSS | Started |
|---|---:|---:|---:|---:|
| serial (`HORIZON_WORKER_THREADS=1`) | 15.90 s | 13.5 s | 828 MiB | 191 |
| parallel (default pool) | **10.46 s** | 8.2 s | 859 MiB | 191 |
| bootstrap batch, serial | 10.22 s | — | — | 191 |
| bootstrap batch, parallel | **5.14 s** | — | — | 191 |

The batch is most of the run and the pool takes 5.1 s off it (10.22 -> 5.14); the rest of startup is unchanged, so
the whole run drops 5.4 s. Peak RSS grows 31 MiB, because up to eight jars are parsed and bootstrapped at once.
**191 of the 500 started in that profile run**; the 19:02 binary's ledger starts 197 and skips 7,742 plans, so the
profile remains a parser/loader measurement, not a claim that 500 effective server mods run.

Caveats: the host was loaded (absolute fps is depressed and noisy); vanilla ran `-Xmx2G` and RSS is measured at
`Done`; the client ran offscreen with `maxFps=260` because the default 120 fps cap hides all frame differences.

### Bootstrap content cache (AOT)

What a jar declares depends on its bytes, this build's engine and its role, so a jar that started once is stored and
replayed instead of parsed and executed again (`game/java_cache.rs`). Measured 6 November 2026 (16 CPUs, load
average about 10) on the first 100 of the 197 jars that start natively from the 500-jar corpus: one flat-world
dedicated run per point, one game folder, `HORIZON_HOME` and world reused, the cache cleared before each cold run,
eight cold and eight warm starts alternating.

| Start | Wall to `Done` (8 runs) | Median | Peak RSS (median) | Reused |
|---|---|---:|---:|---:|
| cold (empty cache) | 2.88–4.34 s | 3.29 s | 821 MiB | 0 of 100 |
| warm (filled cache) | 2.82–3.73 s | 3.45 s | 722 MiB | 86 of 100 |

On this loaded host the wall difference is inside the run-to-run noise: the eight pairs average 0.02 s in the
warm start's favour and range from 0.58 s against it to 0.85 s for it (an earlier, quieter run of the same suite
measured 2.48 s cold against 2.41 s warm). What is stable is memory: the warm start's median peak RSS is ~100 MiB
lower because 86 jars are replayed rather than parsed at once, and its `java-runtime.json` is byte-identical to the
cold run's. The fourteen jars not reused declare payload channels, whose handlers need the bootstrapped host rather
than rows; the cache held 86 entries in 436 KiB.

## Known gaps

1. **Game-class mixins do not apply.** The load weave only rewrites classes inside the jar; a plan whose target is a
   Minecraft class is skipped. That is every plan that matters (0 woven; 2,944 and 7,742 skipped), so `ran` means
   "entrypoint executed, mixins skipped, vanilla server reached `Done`", not "the mod works". The opt-in hook tier
   (`HORIZON_JAVA_GAME_HOOKS=1`) fires its 19 public sites and 7 internal Java-tier ones (value-returning
   `getDestroySpeed` and `growCrop`, `PlayerList#remove`, `stopServer`'s tail, the player-tick heads and `Player#tick`)
   from the game's own points, and asks a config's `IMixinConfigPlugin` natively, but across both corpora it binds
   only 87 server-side plans (was 40; 51 of the loader tier's 1,285, was 18, statically counted), and a bound
   callback stops at the first member the hook tier does not model. On a
   dedicated server with two bot players and eight of those jars, 4 jars' callbacks ran (appleskin and syncmatica
   `player_logged_in` 2 each, shogi `player_tick` 1,457, servux `server_stopping` 1) and the rest refused and were
   disabled (`ServerPlayNetworking.canSend`, `Connection#getRemoteAddress`, a `@Shadow` field, Guava's `Ordering`).
   Objects an entrypoint built keep only their static fields after it returns, so a callback that reads a
   listener list the entrypoint filled (servux's) sees it empty.
2. **Refusals.** The committed ledgers (an older binary) count 27 of 121 wide and 68 of 265 mega jars that declare a
   server entrypoint as failing (22%/26%). A fresh dedicated-server re-measure on 7 November 2026 (all workstreams'
   fixes, one server per jar, 60 s) counts **5 wide and 15 mega `native-gap` rows**. The entrypoint-exception bucket
   (54 rows across both ledgers: null-config/listener NPEs, `Unexpected subclass`, loader-detection guards,
   `AssertionError`s, `No value present`, invalid ids, the nested-frame bound, `StackUnderflow`, the duplicate item)
   fell from 22 still failing to 4: static Mixin accessors run their generated contract, reflection answers the
   class file (superclass, assignability, enums, constructors, nested simple names, annotations with defaults,
   `Modifier`), JDK method references keep their arguments, concrete virtual calls reach overrides, pattern-matching
   `switch` sites answer, `String.matches` has a bounded regex, `StackWalker` sees jar frames, and game codecs,
   builders, registry keys, `BlockItemId`/`ColorCollection` ids and mod registries are never null. The four left:
   archers and wizards read `spell_engine`'s config classes, which a lone jar does not have (both reach `Done` with
   `spell_engine` and `structure_pool_api` beside them; a real loader would refuse them alone for the missing
   dependency, so they need a dependency-aware ledger row, not an interpreter fix); both WorldEdit builds need a
   Guava collector in `Stream.collect` (stream modelling). Paladins fails the same way as archers. Jars whose
   `fabric.mod.json` says `"environment": "client"` are now client-only skips on a server, as on a real Fabric
   server, including a multi-loader jar's NeoForge `@Mod` class: 16 wide and 54 mega rows that used to "run" there
   moved to `skipped`. The raw log line is bounded at 160 characters, so a human sees a clipped cause; the ledger
   recovers the skip class from the jar's own metadata. The JDK model is bounded, not a JDK: a `subList` is a copy,
   a `Map.Entry` keeps its old value after `setValue` (the map is written), loggers drop everything,
   `String.format` refuses argument indexes and the `+ , # (` flags, and streams are eager with at most 4,096
   elements from a spliterator, range or file walk.
3. **Nested and non-server entrypoints.** Nested JiJ/JarJar jars are probed but never bootstrapped in-game (`preLaunch`
   is not run either); the two mega jars that hit the 128-nested-jar-frame bound (ShulkerBoxTooltip) now run, the
   bound itself is unchanged. The Kotlin entrypoint
   adapter runs, but an adapter the runtime still does not model is refused.
4. **Callbacks.** Entrypoints, received-payload handlers and server-side event listeners execute. Servers fire
   lifecycle, server/level tick, player join/leave and living-death listeners (Fabric, NeoForge, Forge EventBus 7),
   but the façade a listener sees is small: on a dedicated run of 48 corpus jars with server listeners, 42 jars
   kept 138 listeners and 33 were refused at their first unmodelled call, mostly `MinecraftServer#getWorldPath`
   (6), `registryAccess` (6 with the level's), `getResourceManager` (2), and interpreter collection/stream markers.
   Use-block/item, attack, block-break, entity-load and level load/unload listeners are identified but not fired;
   command registration (`CommandRegistrationCallback`, 44 jars) and every client event (client tick 57 jars,
   screens, rendering) have no dispatch. Priorities are registration order. There is also no compiled HUD,
   key-binding, tooltip, screen, renderer or model façade in this branch. The former façade and census sources were
   deleted; they are recoverable from commit `be8567b9a`, so client jars register content but do not draw.
5. **Content consumption.** Blocks, items, effects, entities, menus, block entity types and recipes reach the content
   model; data components and attachments are typed declaration rows that travel in the snapshot and the manifest.
   What is left:
   - *Recipes* load from a jar's `data/<ns>/recipe/*.json` and from typed declarations, but only when the type is a
     vanilla one and every item they name is vanilla or declared by the same jar. On the 25 mega jars that ship mod
     recipes, one dedicated server applies **824** and logs **1,212** left out with a reason (most name an item the
     jar registers opaquely, or use the mod's own serializer); before this change 0 applied. A recipe naming another
     jar's item is left out, since each jar is checked alone.
   - *Block entity types* come only from `META-INF/horizon-native.json`: the 15 corpus `block_entity_type`
     registrations go through `FabricBlockEntityTypeBuilder`/`BlockEntityType` builders with a mod class and no vanilla
     template, so they stay opaque. A declared type keeps its template id; the game gives the block its own
     template's behaviour, not the declared type's.
   - *Data components*: 28 of 40 corpus registrations are typed (17/29 mega, 11/11 wide; 0 before), with
     `persistent`/`network`/`cache_encoding` and a codec label that is only the codec's declared type. *Attachments*:
     21 Fabric `AttachmentRegistry` rows (16 mega, 5 wide; none recorded before). NeoForge's
     `DeferredRegister.DataComponents` and `AttachmentType` register through suppliers the deferred register never
     runs, so they stay opaque; no component value is encoded from a row.
   - *Ingredients* are kept as declarations: no vanilla data folder reads them.
   - *Guests*: an applied manifest's item, block, effect, entity, data-component, menu and block-entity ids join the
     guest's command registries (parsing and suggestions). A guest without the jars is still refused by the content
     identity check, and its creative tab and item tables stay its own root's.
   - There is no mod hot reload (content rows arriving late rebuild the root once).
6. **No JVM fallback in the game.** `horizon-jvm` is a standalone reference runner; a jar the native runtime refuses
   is skipped, so the `jvm`/`sidecar` counts in `server-status` are structurally zero.
7. **Diagnostics.** `server-status`, `java-runtime.json`, the launcher's runtime panel and the server ledger now carry
   refused jars with the runner's whole cause (bounded only at 2,048 characters; the log line still stops at 160) and
   each started jar's skipped plans and counts by reason. What is left: a refused row names no loader (the failure
   carries only the path and cause), and the launcher matches report rows to installed mods by file name, so a jar
   run outside the browser's `horizon-mods` folder has no panel row.
8. **NeoForge.** Native metadata and `@Mod` bootstrap work. The real FML lifecycle is a bounded, opt-in child
   process (`horizon-jvm --real-loader`) used as a preflight on the integrated server, not the path a mod's code
   takes; the dedicated role does not preflight yet, there is no in-game loader selection, and no automatic
   intermediary-to-official remapping.
9. **The numbers are narrow.** Both corpora are filename-sorted slices of 26.3 Fabric jars, the client run is
   offscreen with no mod drawing, and the efficiency suite has no steady-state or playable-session figure.

## Planned work

1. **Make mixins count.** Widen the game-hook tier's object model from what the bound callbacks reach first
   (players, entities, levels, the server; Fabric networking, `Connection`, `@Shadow` fields next), keep entrypoint
   objects' instance fields alive for later callbacks, and map the remaining game-class plans onto hook points (or
   provide the target classes through the loader path). Gate: `woven` or `run_game_hook` covered by a real-mod ledger, and every
   `native` label backed by zero skipped plans.
2. **Close metadata refusals.** Gate: metadata refusals 38 → 0 across both corpora. The JDK surface config
   bootstraps reach is modelled in `jdk_model.rs`/`jdk_stream.rs` (charsets, loggers, file walks, `Comparator`,
   the missing stream stages, `toMap`, `String.format` widths, JDK `instanceof`, interface-default and
   receiver dispatch; [java-mod-compat.md](java-mod-compat.md)). On the 38 rows, measured 7 November with a fresh
   dedicated server per jar: `malformed Boolean marker` 7 → 2, `malformed Integer marker` 3 → 0, `expected a native
   stream marker` 4 → 2, `ServiceLoader has no seeded providers` 2 → 0, `native collection marker, got null`
   2 → 2, `list index out of bounds` 1 → 1, and icy-better-horses' uncaught exception is gone. What is left needs
   more than the JDK: zoomify (×2) needs `yet_another_config_lib_v3`, whose config writer encodes through DFU's
   `JsonOps`; emotecraft (×2) needs `player_animation_library` and the service providers it ships; servercore (×2)
   builds its config through DazzleConf's reflection (`Method.getReturnType`, `getAnnotatedReturnType`, `Proxy`);
   voxy walks its own jar through `ModContainer.getRootPaths()`. A ledger run is one jar per server, so the first
   two cannot pass on their own, and the dedicated log reports the jar's alone-run refusal even when the pack class
   path got further.
3. **Close runtime refusals.** The entrypoint-exception bucket is down to two missing-dependency rows (archers,
   wizards, and paladins in the same shape) that need the ledger to run a jar with its declared dependencies, and
   WorldEdit's Guava collector. Gate: refused share of in-scope server jars below 5% (committed ledgers 22%/26%;
   fresh re-measure 5 wide / 15 mega rows).
4. **Finish content.** Type `BlockEntityType` builders and the jar's opaque items so more recipes resolve (check
   recipes across the pack, not per jar); NeoForge data components and attachments through their deferred suppliers;
   let a guest without the jars build a root from the manifest; then mod hot reload.
5. **Event surface.** Grow the listener façade by the refusals the dedicated pack run logs (world path, registry
   access, resource manager, game rules, `execute`), fire the identified use/attack/break/entity/level-load events
   from their game points, and dispatch `CommandRegistrationCallback`. Gate: no façade refusal among the ten most
   common listener causes on the corpus pack run.
6. **Client surface.** Compile the façade client families (HUD, keys, tooltips, screens, renderers, models) into the
   runtime and drive them from the game; measure with rendering on and a curated pack, not offscreen.
7. **NeoForge path.** Finish the FML path: give the dedicated role the integrated server's opt-in preflight, then
   in-process loader selection, with remap readiness checked against a real NeoForge 26.3 server.
8. **Diagnostics.** Done: weave counts, first skip reason, the per-plan list, refused rows with their whole cause in
   `server-status` and `java-runtime.json`, and the launcher's per-mod row with a runtime-cache clear. Next: name a
   refused jar's loader from its metadata, and show runtime rows for jars the launcher did not install.
9. **Keep the corpus honest.** Refresh `tools/real_mod_fixtures/LAST_SERVER_LEDGER_{223,500}.txt` per release, fail
   a run when `ran` regresses or a refused row loses its cause, and extend the fixture workflow (it deliberately never
   downloads third-party jars).
10. **Performance.** Rerun the efficiency suite with a curated pack and steady-state heap, and a playable client
   session with mods that actually draw.

## How to reproduce

```sh
cargo build --profile fast -p horizon-client -p horizon-native-java -p horizon-jvm
cargo test -p horizon-native-java
python3 tools/real_mod_corpus.py --self-test
python3 -m unittest tests.test_real_mod_corpus tests.test_real_mod_fixtures

# a corpus through the real dedicated server (one fresh server per jar; 60 s bound)
scratch=$(mktemp -d)
TMPDIR=$scratch HORIZON_NO_AUDIO=1 python3 tools/java_mod_server_ledger.py ~/horizon-corpus/corpus-xl \
  --binary target/fast/horizon-client --native-binary target/fast/horizon-native-java \
  --resources .local/horizon-client/26.3 --timeout 60 --json --output ledger.txt > ledger.json

# the same jars through the real Fabric loader (needs the game jar, loader and libraries)
python3 tools/java_mod_loader_ledger.py --mods-dir ~/horizon-corpus/corpus-xl \
  --binary target/fast/horizon-jvm --json
```

In the game: `horizon-client --java-mod mods/example.jar --resources .local/horizon-client/26.3 --world W`, add
`--java-mod` once per jar, and put `--java-mod` before `--dedicated` on a server. `HORIZON_JAVA_STRICT=1` turns a
refusal into a startup failure. The efficiency harness is in `tools/efficiency/` ([efficiency.md](efficiency.md)):
`python3 tools/efficiency/run_server.py --runs 3`,
`python3 tools/efficiency/run_client.py --counts 0,3,20,74 --max-fps 260 --warmup 5` and
`python3 tools/efficiency/run_startup.py --counts 0,25,50,74`; the run's own reports and its jar-selection probe stay
in `~/horizon-corpus/`.
