# Java mods on Horizon: the compatibility runtime (design)

Status: Phase 1 probe, 1 November 2026. Phase 0 is measured ([mod-abi-census.md](mod-abi-census.md));
the façade generator and embedded JVM runner are now built and exercised against Fabric API and Hollow
Dawn. Read it with [mods.md](mods.md) (what runs today), [modpacks.md](modpacks.md) (the
port pipeline) and [native-hooks-neoforge.md](native-hooks-neoforge.md) (hooks named after NeoForge
events).

**Scope: 26.3 only.** A jar is in scope when its `fabric.mod.json` or `neoforge.mods.toml` says 26.3.
Forge and NeoForge split at 1.20.2, so the 26.3 Forge-family loader is NeoForge; a jar declaring the old
`mods.toml` is out of scope, and its data is still the carry pipeline's job ([modpacks.md](modpacks.md)).
Anything built for an older Minecraft version is a port, not a bridge.

That narrowing is worth its weight. Since 26.1 the game ships unobfuscated
(`crates/horizon-port-pack/src/classes.rs`), so a 26.3 Fabric jar and a 26.3 NeoForge jar name classes
the same way — one naming scheme for the façade, one name table for the census, and no intermediary/SRG
remapper in the runtime at all.

`mods.md` opens with: "Fabric and NeoForge jars do not run as they are: their code calls and patches
Minecraft's Java classes, and Horizon has none." This page is the plan for the exception: a runtime that
gives them those classes, so a jar runs — unmodified — beside Horizon's own simulation.

The instinct behind it is right, and this page is that instinct made precise: **map exactly what the
loaders and the two mod APIs attach to, map what each of those is here, then run the jar against that
map.** The one correction is the last word. You do not translate the jar to native Rust; you translate
*the attachment points*, and the jar keeps running as Java. That inversion is what makes the project
finite instead of a JVM rewrite.

There is now a deliberately smaller native experiment beside the bridge. `horizon-native-java` parses
real classfiles and Fabric metadata, discovers `Class::method` entrypoints, and interprets a bounded
no-argument bytecode subset. Calls it cannot model cross an explicit Rust `Host` boundary; unsupported
opcodes report the class, method, and program counter so the launcher can fall back to the embedded JVM.
This gives simple utility mods a no-JVM path without claiming that the subset is a general Java runtime.

## The short version

- **What it buys.** A pack's libraries, integrations and client UI — 156 of ATM10's 495, the one pack we
  measured — are classes of mod that today have *no* path at all (`modpacks.md`). They run for free. The
  content mods stop needing an agent each and start needing one only when someone wants them *fast*.
  Coverage stops scaling with our labour and starts scaling with the mapping table. (ATM10 is 1.21.1, so
  the 26.3 census will be smaller; the *ratio* is what carries over.)
- **What it costs.** A JVM in the process (memory, startup, GC), a second place where gameplay semantics
  live, and about a year of one team's work before client-side mods are 80% covered. Server-side mods
  ship sooner (phase 3).
- **What it does not do.** It does not make mods faster than vanilla. Horizon's "better than the pack"
  number stays a property of native ports. A bridged mod is a *vanilla-speed* mod with Horizon's engine
  under it.
- **The rule it breaks.** "Everything players run is Rust" (`AGENTS.md`). A compatibility runtime is by
  definition a JVM. The carve-out is explicit and narrow: the JVM starts only when a jar mod is turned on,
  and the Rust game is byte-for-byte the same when none is.

## Why a JVM, and not "translate any jar to Rust"

A jar is bytecode for the JVM. Two ways to make it run here:

| | Provide the classes, run the JVM | Compile the bytecode to Rust/native |
|---|---|---|
| What you must reproduce | the semantics of the Minecraft classes the mod touches | the semantics of the JVM: `invokedynamic`, lambdas, records, reflection, `MethodHandle`, `Object.wait`, `synchronized`, exceptions with stack traces, `Class.forName`, `Unsafe`, `Enum` identity, string interning, class initialization order, ... |
| Size of the problem | finite, and measurable: the classes mods name | unbounded, and every edge case is a mod that mysteriously breaks |
| Per-call cost | ~20–30 ns across JNI | ~0 |
| Startup | a JVM's | a compiler's |
| Bytecode features added by future Java | free | a new compiler pass each |

The second column is a JVM. Nobody should write one to run Create.

So: keep HotSpot, and shrink the problem to the only thing that has to be right — **the class library**.
That surface is long-tailed but finite, and it is measurable today, from jars we already parse
(`horizon-port-pack`'s `classes::parse`). Measure it before writing a line of the runtime; if the tail
turns out to be an order of magnitude worse than assumed, this plan stops at phase 0 and says so.

### What "fully translate to pure Rust" would actually require

It is the right *end state* to aim at, and the wrong *first pipeline*, for reasons that are not about the
compiler. "Translate the jar" is shorthand for "be the JVM". The scoreboard, in person-months, ±50%:

| Piece | What it must do | Est. |
|---|---|---:|
| Bytecode → IR | parse, verify, SSA, stack maps | 3 |
| Object model | exact layouts, headers, arrays, strings, boxing | 4 |
| Dispatch | vtables, interfaces, default methods, `invokespecial` | 2 |
| Exceptions | exception tables, `finally`, throw/catch, stack traces | 4 |
| GC | a tracing collector — Java graphs are cyclic, so refcounting leaks | 6 |
| Threads and monitors | `synchronized`, `wait`/`notify`, `volatile` | 4 |
| `invokedynamic` | lambdas, method refs, string concat | 3 |
| Reflection | `Class`, `Method`, `Field`, proxies, annotations, `setAccessible` | 9 |
| Dynamic loading | nested jars, `Class.forName`, optional dependencies | 5 |
| Interpreter fallback | for everything the closed world misses | 7 |
| Codegen | Rust source or LLVM IR, inlining, name mangling | 10 |
| Debug info | line tables, Java stack traces that name the mod | 3 |
| Build system | per-pack AOT, caching, incremental | 5 |
| **Execution engine** | | **~65** |
| The façade and its mapping | shared with the bridge, so not extra | ~12 |

For scale: HotSpot plus the parts of the JDK class library mods touch is on the order of a million lines,
and every one of them exists because someone hit the case you are about to hit. The reference points are
not encouraging: GraalVM native-image is about a decade of a large team and still asks mods for reflection
configuration; RoboVM took years and never had dynamic class loading; TeaVM has spent a decade on a
closed-world subset. None of them would run Create unmodified.

### Three things that are not compiler problems

**1. The build pipeline contains a JVM anyway.** Mixin is a bytecode-to-bytecode transform (ASM), and it
must run *before* translation: a mixin into `Block` changes the class you are about to compile. So the
pipeline is "emit the façade as bytecode → run Mixin → translate". The façade is generated from Horizon's
tables, so emitting it is fine — but then Mixin has to run on it, which means either reimplementing
Mixin's selector semantics (`@At` by value, ordinal, slice, `@Redirect` matching) on your own IR — a
multi-month subsystem with a long tail — or running the real Mixin, which needs a JVM at build time.
"Pure Rust" ends up with a JVM in its build, or a second Mixin.

**2. A closed world leaks.** Static translation wants every class known at build time. The census
([mod-abi-census.md](mod-abi-census.md)) found leaks in 5 of 53 code jars: `Class.forName` behind `isModLoaded`
checks, nested jars loaded at runtime, reflection, Kotlin runtime reflection, and scripts (KubeJS, Rhino)
that are *data*, not bytecode. That is rarer than this page first assumed, and it makes a static-subset
translator more plausible, not less — but every leak still needs a fallback, the fallback is an
interpreter, and an interpreter is a JVM with the JIT removed.

**3. Mixin makes the build per-pack, not per-mod.** Mixin changes field and method layouts at class-load
time, and mods `@Shadow` each other's injections. In a JVM the loader applies them and the layout adapts.
AOT picks the layout by whole-program analysis, so **every mod set is a different binary**, and adding one
mod recompiles everything. The bridge's artifact is per-mod — a jar its author already built. The
translator's is per-pack.

### Where AOT would win

- No 150 MB JVM base, no GC pauses, no JIT warmup.
- No boundary cost — the 25 ns becomes 0, which was never the bottleneck.
- A real sandbox: no `Class.forName` from disk, no `Runtime.exec`, no network.

The last one is the only argument with teeth, and it is cheaper to get by running the JVM in a child
process (decision 1 below) than by writing a JVM.

### And the cost lands on the player

AOT at install means compiling the pack: generated Rust or IR for 495 mods is tens of millions of lines,
`rustc` plus LLVM does maybe 10–50k lines a second on generated code, and the first build is tens of
minutes to an hour at 16–32 GB — then every mod update rebuilds it. `native-mods.md` already measures
9 minutes to build the game plus a few small mods; that is the floor, not the ceiling. Shipping prebuilt
binaries instead means running a build farm for every pack version.

### The verdict

The bridge and the translator share the mapping — 80% of the work and all of the insight. The translator's
extra is the entire execution engine. The census moves the reflection line (9 of the 65 person-months)
out of most jars' path; object model, exceptions, GC, monitors, codegen and a build per mod set are
untouched. **You do not escape the JVM; you write a worse one, then maintain it against every Java
release.** "Pure Rust" is reachable exactly the way it is reached today: per mod, by the port pipeline,
when profiling says a mod is hot (`Tier 5` below). The bridge is what makes that optional instead of
mandatory.

## What a jar actually attaches to

Three surfaces, wildly different in size. All three need an answer; only the third is the project.

| Surface | Who defines it | Rough size | Difficulty |
|---|---|---|---|
| **Loader** | Fabric Loader; Forge's ModLauncher / FML | tens of classes, stable | low for Fabric (it is designed to embed); high for NeoForge (it *is* a patch set, not a loader) |
| **API** | Fabric API's modules; NeoForge's events, registries, capabilities, components | a few hundred classes | medium — this is where the semantic translation lives |
| **Minecraft** | `net.minecraft.*`, the classes mods call and Mixin weaves into | ~1,000–1,500 classes referenced by a 500-mod pack; ~100 carry most references | the project |

The third surface splits again, and this is the split the whole design turns on:

1. **Called by the mod** (`ItemStack.getCount()`, `Level.getBlockState`) — a normal method call; the cost
   is the boundary.
2. **Extended by the mod** (`class MyBlock extends Block`) — the façade class must be a real superclass
   with real vtables, constructors and fields.
3. **Weaved into by Mixin** (`@Inject` into `Player.tick`) — the *call site in Horizon's control flow*
   must exist and be reachable, and the Java method containing it must be real.

(3) is why this works at all: an injection point is a call site, and `native-hooks.json` already names 45
of them. The census measured how far that goes — **7 of Fabric API's 689 injection points (1%) land on a
method a hook names** ([mod-abi-census.md](mod-abi-census.md)). The catalog is a starting point, not
coverage. The rest splits in two: where the façade implements a method *in Java* its mixins need no Rust
hook at all (the hook-host class below); where the decision stays in Rust, a named hook is added, and
`native-hooks-neoforge.md` stays the rule for adding one.

## Step one: measure the surface (phase 0)

The tool is `horizon-port-pack census` (it starts there, and splits into its own crate when phase 1
needs it). It reads the way `check` does — constant pool, annotations, `fabric.mod.json` /
`mods.toml`, `*.mixins.json`; no decompiling, no running:

- every class the jar's code references, by count;
- every **member** reference — `(owner, name, descriptor)` — which `classes::parse` does not record yet;
- every Mixin target, injection point, `at` value, ordinal, and whether it is `@Inject`, `@Redirect`,
  `@WrapOperation`, `@ModifyVariable`, `@Overwrite`, `@Accessor`;
- entrypoint interfaces and `@SubscribeEvent` listener types;
- access wideners and access transformers, by target.

Join that against `native-hooks.json` (which points already have a hook) and against the port ledger
(which methods Horizon has). Output is ranked three ways:

1. **By class**, with a cumulative percentage — the façade's build order.
2. **By attachment point**, with `hook exists / no hook` — the hook backlog.
3. **By mod**, with a projected verdict — the `check` verdict's second axis.

**First run, 31 October 2026** ([mod-abi-census.md](mod-abi-census.md)): Fabric API 26.3's 47 jars name
**883 distinct `net/minecraft` classes**; a real 26.3 mod (Hollow Dawn, 8 jars) names 319. Mod code
reaches 80% of its references by class 100 and 93% by 200; library code reaches 71% by 200, which is what
a library is. Of the 53 Fabric API and Hollow Dawn jars that contain code, **48 reference no reflection
at all**.

**Decision rule, corrected by that run.** A *mod* corpus must reach 80% of its references by class 200 —
the first run passes at 100. A *library* corpus instead defines the façade's class list (883 for Fabric
API alone), and its tail is the API's job, not a failure. Both clear the plan: each later phase is now
scoped by a number.

The census is also a product: it is the "what runs" table the launcher shows for a jar before install,
and the generated reference under `docs/api/`.

## Phase 1, first slice (measured 1 November 2026)

The attach path is real: a mod's entrypoint runs on an embedded JVM against a generated façade.

The census now records Fabric and Quilt entrypoint declarations (the `kind` and class from
`fabric.mod.json`, including the object form with an adapter field) beside each jar. NeoForge has no equivalent entrypoint list in
`neoforge.mods.toml`, so the census also finds classes annotated with `@Mod("id")` and records them as
`neoforge` entrypoints. This makes the next runner step mechanical: discover the `main`, `client`, or
`server` classes from Fabric metadata, or the NeoForge mod classes from annotations, then invoke only
the entrypoint kinds supported by the current Horizon mode. `horizon-jvm` now performs that discovery
when `--entry` is omitted, while retaining the explicit override for mixed jars and diagnostics.

- **`horizon-port-pack facade <jar> <out> --game <26.3.jar> --jdk <dir> [--libs <dir>] [--extra <jar-or-dir>]`** generates the
  façade for exactly the game classes the jar names, in the real 26.3 shapes (from the game jar and the
  client's libraries), and compiles it. Hollow Dawn's core jar: **1,287 classes, 2,149 methods, 7,687
  fields, 0 named but missing**, `javac` clean. Methods throw
  `UnsupportedOperationException("horizon stub: class#method")`; nested classes are emitted nested
  (`Holder.Reference.Type`), because `javac` resolves the dotted form, not the flat `A$B`.
  `--extra` adds loader/API jars to the reference closure and `javac` classpath; core plus processed
  Fabric API jars currently produces **1,967 classes, 4,096 methods and 10,098 fields**; the combined
  Hollow Dawn module probe closes to **2,088 classes, 5,337 methods and 10,407 fields**; including
  the outer Fabric API bundle and its nested jars, the client façade compiles to **2,239 classes,
  5,678 methods and 10,596 fields**.
- **`crates/horizon-jvm`** embeds a JVM (`jni` with `invocation`) and runs a mod's entrypoint with that
  façade first on the classpath. Fabric jars-in-jars under `META-INF/jars/*.jar` are extracted into a
  temporary classpath directory (including nested bundles) and removed when the runner exits.
- **`crates/horizon-native-java`** is the optional no-JVM path. It parses Fabric metadata and classfiles,
  discovers `Class::method` entrypoints, and executes a bounded no-argument bytecode subset with an
  explicit Rust `Host` boundary for static fields, calls, and values. Unsupported opcodes stop with a
  class/method/program-counter diagnostic so the launcher can fall back to `horizon-jvm`; it is a native
  interpreter prototype, not a replacement for the full JVM yet.

Hollow Dawn `CoreModule`, 122 jars on the classpath (the first probe, before the `StreamCodec` slice):

```
jvm up: 122 jars on the classpath, entry dev.solexsis.hollowdawn.core.CoreModule
Exception in thread "Thread-0" java.lang.ExceptionInInitializerError
    at dev.solexsis.hollowdawn.core.CoreModule.onInitialize(CoreModule.java:59)
Caused by: java.lang.UnsupportedOperationException: horizon stub: net/minecraft/network/codec/StreamCodec#of
    at net.minecraft.network.codec.StreamCodec.of(StreamCodec.java:3)
    at dev.solexsis.hollowdawn.core.registry.CoreAttachments.<clinit>(CoreAttachments.java:51)
```

That is the loop working: the class loads, the entrypoint runs, and the first unimplemented game method
is named with the mod's own file and line under it. The first runtime slice now treats
`StreamCodec` factories as bootstrap values, returning an empty codec object so static registration can
rather than the codec factory itself. Codec composition methods (`map`, `mapStream`, `dispatch`, `apply`, `optional`, `nullable`, `cast`, and `tuple`) preserve a non-null receiver, and
`CustomPacketPayload.Type` retains its `Identifier` through construction, `id()`, `toString()`, equality,
and hashing, so payload registration can compare and log type identities before crossing the native network boundary. The next failure
is therefore the next concrete value or registry operation the mod reaches,
rather than the codec factory itself.

The live core-plus-Fabric probe has since advanced through façade visibility, stateful `Identifier`
values, `VarInt` sizing, a Fabric accessor shim, StringRepresentable codecs, data-component builders,
registry holders, synthetic snapshot-drift methods, fluent block properties, block-state construction,
property factories, SoundEvent creation, criterion-trigger contracts, component factories, creative tab
builders, `Vec3i.offsetCodec`, `FileToIdConverter` factories, and `FeatureFlagSet.of` bootstrap values.
The generated `Vec3i`/`BlockPos` stubs now retain constructor coordinates and answer the common `getX`,
`getY`, `getZ`, `above`, `below`, `offset`, `relative`, `subtract`, and distance queries, so
value-oriented bootstrap code does not immediately hit a neutral default. `Vec3` retains double coordinates with common vector arithmetic, and `AABB` now retains its
six bounds with size, containment, center, inflation, and directional expansion queries.
Component factories now preserve literal and translatable text in `MutableComponent` values; `getString`,
`append`, and `copy` keep the common display-name bootstrap path alive.
`CompoundTag` now has a bounded value path for scalar `putString`, `putInt`, `putBoolean`, `putLong`,
`putByte`, `putShort`, `putFloat`, `putDouble`, and bounded byte-array round trips, with matching reads, `contains`, `isEmpty`, `size`,
`remove`, `copy`, and `getAllKeys`, backed by a Java-owned map. Numeric reads accept any boxed
`Number`, so common NBT bootstrap code can read a value written with a narrower type. `put`/`get`
preserve nested `Tag` references, `getCompound` returns the nested value or an empty façade tag, and
`getList` plus the generated `ListTag` retain bounded add/get/remove/size/string/compound operations. These
collections remain callback-local until the Rust NBT bridge is wired through the native boundary.
`Level` and `ServerLevel` now retain a game-time and dimension marker and answer the common
`getGameTime`/`getDayTime`, weather and thunder levels, `isRaining`/`isThundering`, `dimension`,
block-state/entity, loaded-chunk, sky-visibility, side, and build-height queries. Their
bounded coordinate-keyed maps also preserve `setBlock`, `setBlockAndUpdate`, `setBlockEntity`, and
`removeBlock` writes within the façade instance, so a registration callback can observe its own world
mutation sequence. `getSeaLevel`, `getMaxLocalRawBrightness`, and `isOutsideBuildHeight` provide common
overworld terrain bounds for terrain and light bootstrap code. The maps remain callback-local until the native level handle is
wired through the game boundary; they do not replace Horizon's authoritative world state.
`Entity` now retains position, delta movement, rotation, removal state, a level handle, a stable UUID
marker, and a bounded bounding-box/on-ground state; `getX`/`getY`/`getZ`, both `setPos` overloads,
`position`, `getDeltaMovement`, both `setDeltaMovement` overloads, `getXRot`/`setXRot`,
`getYRot`/`setYRot`, `distanceTo`/`distanceToSqr`, `blockPosition`, `getBlockX`/`getBlockY`/`getBlockZ`, `isPassenger`/`getVehicle`, `level`, `isAlive`, `isRemoved`,
`getEyePosition`/`getEyeY`, `getEyeHeight`, `getBbHeight`/`getBbWidth`, `discard`, `getUUID`, `getBoundingBox`, `setBoundingBox`, and `isOnGround` preserve common entity
inspection, movement, and collision setup paths. The box, motion, rotation, and removal state are
façade-local until the native entity boundary wires live physics updates through them; player mutation
and synchronization remain native-boundary work.
`BlockEntity` now retains its block position, level, block state, and removed flag. `getBlockPos`,
`getLevel`, `hasLevel`, `getBlockState`, `isRemoved`, `setRemoved`, `setChanged`, and `getUpdateTag`
keep common block-entity registration and synchronization code linked. `saveAdditional` and `load` now
round-trip a bounded callback-local `CompoundTag` payload, while persistence outside the callback and live
level writes remain native-boundary work.
`LivingEntity` now retains bounded health, main/off-hand stack values, and an identity-keyed effect map;
health mutation/healing, death-state, armor-value, effect-presence, effect application/lookup/removal, and
hand-stack reads/writes keep common mob and player inspection code linked while equipment synchronization
outside the façade remains native-boundary work.
`Mob` now retains bounded `isNoAi`/`isAggressive` flags and a target reference, so common AI gates and
target-selection callbacks can round-trip their state inside one façade callback. `MobEffectInstance`
retains its effect, duration, amplifier, ambient/visibility/icon flags, infinite-duration query, and
bounded duration ticking; live AI and effect scheduling remain native-boundary work.
`Player` and `ServerPlayer` façade stubs now retain bounded creative/spectator flags, scoreboard identity,
and experience level/progress. `isCreative`, `isSpectator`, `getScoreboardName`, `getName`,
`getDisplayName`, and the experience queries preserve common permission, HUD, and display-name paths.
`getInventory` now returns a 36-slot callback-local inventory with selected-item, set/remove, and clear
operations; `getAbilities` exposes bounded flying/walking speed accessors. Authority, profile identity,
and live inventory synchronization remain native-boundary work.
The Hollow Dawn core entrypoint now
initialises successfully with the combined façade and processed Fabric API jars, keeping each runtime
slice concrete and replayable.

The same combined façade now runs all seven Hollow Dawn main module entrypoints through
`onInitialize`: core, arsenal, bestiary, gloam, lantern, tallowmire, and vesper. The façade closure
also follows descriptor-only callback types, so Fabric's entity-event bootstrap receives
`ConversionParams` instead of failing with `NoClassDefFoundError`. The runner skips the signed
Minecraft runtime jar when pointed at a full server directory, preventing package signer conflicts
with the generated façade.

The matching seven client entrypoints also return from `onInitializeClient`; this exercises model-layer
registration, key mapping registration, and the nested Fabric API client modules in the same probe.
Additional installed-mod probes now complete for Chunky, Spark (including its static `Class::method`
client entrypoint and `MinecraftAccessor` cast), FabricProxy-Lite's server initializer, and ServerReplay's
Kotlin configuration bootstrap.

The façade also materializes a bounded Mixin ABI slice: when a target mod contains a public interface
annotated with `@Mixin` and method-level `@Accessor` or `@Invoker`, the generated target stub implements
that interface when its methods have primitive, `void`, or common erased JDK/collection return types and
supplies neutral linkage bodies. Object-returning accessors use empty values for common collections,
optionals, and one-dimensional arrays; the real field or call semantics remain a later hook. Spark's client
entrypoint now exercises this path through its `MinecraftAccessor` cast. Static Fabric attachment constants
with no transformed target use explicit shims.

Reproduce:

```sh
cargo run -q -p horizon-port-pack -- facade <mod.jar> /tmp/facade \
  --game ~/.minecraft/versions/26.3/26.3.jar --jdk ~/jdks/jdk-25 --libs ~/.minecraft/libraries \
  --extra <fabric-api.jar-or-processed-jars>
cargo run -q -p horizon-jvm -- [--entry <entrypoint class>] [--entry-kind main|client|server] \
  --stubs /tmp/facade/facade.jar --loader fabric-loader.jar --libs ~/.minecraft/libraries mods/*.jar

# Machine-readable native verdict for a launcher or compatibility screen:
cargo run -q -p horizon-native-java -- --json mods/example.jar main
```

When `--entry` is omitted, the runner discovers Fabric's `fabric.mod.json` entrypoints (including the
string, object, and array forms), selects `main` by default, and executes every declaration in metadata
order; `--entry-kind client` or `server` selects the other environment. NeoForge jars are scanned for
top-level classes carrying `@Mod` and each discovered class is executed in sorted order unless an explicit
`--entry` override selects one. Full Mixin weaving,
the object contract, and semantics for real game objects remain later phases.

The selected Fabric kind now dispatches to its matching lifecycle method (`onInitialize`,
`onInitializeClient`, or `onInitializeServer`), and the embedded runner seeds fresh loader game/config
directories. It also seeds lightweight `ModContainer`/`ModMetadata` proxies from the supplied
`fabric.mod.json` files, so entrypoints that query `FabricLoader.getModContainer` see the jars being
probed without requiring Fabric's full game provider. Hollow Dawn's `CoreClient` now reaches and returns
from `onInitializeClient`; Lithium's `onInitialize` now completes through the same path. Entrypoint
classes exposing the Mixin config-plugin `onLoad(String)` callback receive their package before the
lifecycle call, which lets FabricProxy-Lite initialize its config. NeoForge
classes without a Fabric lifecycle method are accepted after construction so their constructor-driven
registration can be probed.

The native CLI's `--json` form returns
`{status,kind,entrypoints,host_calls,payload_directions}` on success and a structured
`{status,error,stage}` object on load, discovery, or execution failure. A launcher can use that verdict to
select the native path or fall back to `horizon-jvm` without parsing human diagnostics. `cargo horizon-mod
check` runs the same bounded probe as an optional `native` field in its JSON report, trying Fabric `main`,
`client`, and `server` entrypoints before a NeoForge `@Mod` entrypoint. The launcher prints the selected kind,
executes every declaration for that selected kind (retaining the first as the compatibility `entrypoint`),
and reports the attempted `entrypoints`, seeded mod ids, call count, recorded registrations/listeners/payloads, and all bounded-interpreter errors alongside the
content-port result, so a client-only or server-only jar is diagnosed without an extra command.
When the CLI kind argument is omitted, it makes the same ordered discovery attempt; passing `main`, `client`,
`server`, or `neoforge` keeps selection strict for reproducible diagnostics.

The compiled smoke regression exercises this path without a Minecraft installation or `.local` data:

```sh
python3 tools/native_java_smoke.py
```

The script uses `javac` to compile two ordinary Java classes and a tiny NeoForge `@Mod` annotation,
packages real Fabric and NeoForge metadata, and checks that the native runner discovers and executes both
Fabric `main` entrypoints plus the NeoForge constructor. The first Fabric entrypoint also runs an
`Optional.filter` predicate and a mutable `Map.forEach` `BiConsumer` through the bounded host, so the smoke
test proves real callback boundaries in addition to classfile and metadata discovery. It therefore catches classfile, metadata, annotation,
and multi-entrypoint regressions that an in-memory Rust fixture cannot; the full mod-port
harness remains the check that needs prepared vanilla resources.

The game has an explicit `--java-mod <jar>` startup option for this bounded path. It runs the selected
Fabric or NeoForge entrypoint before resource workers start, records host calls and registration intent,
seeds the deterministic loader with the jar's own id/version and required loader dependencies, and fails
with the jar and entrypoint when the bounded interpreter cannot continue. Typed registrations emitted by
the host are merged with the jar's explicit `META-INF/horizon-native.json` declarations before content
planning. Java jars are never
scanned from `mods/` implicitly. Registrations remain diagnostic until a typed declaration adapter supplies
the content rows described below.
`NativeJar` reads the first `[[mods]]` record in `META-INF/neoforge.mods.toml` as the NeoForge id and version,
and carries required `[[dependencies.<id>]]` records into the same startup seed used for Fabric metadata.
Fabric entrypoint objects retain their optional `adapter` name; the native launcher reports that adapter
explicitly and stops rather than invoking the target with the wrong lifecycle contract; the selected
entrypoint set is validated before any entrypoint executes, so a mixed declaration cannot leave partial
registrations behind.
Loader-provided ids (`minecraft`, `neoforge`, `forge`, `java`, and `fabricloader`) and optional dependencies
are excluded, so `ModList` and `FabricLoader` presence checks see only the mod's real required libraries.
If a jar has no entrypoint usable for the current client or server role, the startup error names the role
and lists the loader kinds actually declared (`main`, `client`, `server`, or `neoforge`), so selecting the
wrong side of a split Fabric jar is immediately actionable.

An explicitly supported jar may include `META-INF/horizon-native.json` to supply that typed adapter data:
`{"id":"mod_id","registrations":[{"registry":"minecraft:block","id":"mod_id:block","kind":"block","template":"minecraft:stone","properties":{},"item":true}]}`.
The loader validates block, item, and mob-effect declarations, turns them into declaration-only `Content`,
and layers them through the same `content_root` planner as `.hmod` content. Unknown custom kinds fail the
startup rather than silently entering the native registries.

The façade now also has the first NeoForge registration boundary: `FMLJavaModLoadingContext.get()` and
`getModEventBus()` yield neutral context and bus values, `IEventBus` fluent registration returns its receiver,
and `DeferredRegister.create`/`register` yield façade register and holder objects. `DeferredHolder` now
also exposes the common `value`, `getOptional`, `getKey`, and `getId` accessors with typed neutral return
values, so constructors can inspect a registration without linking against NeoForge's implementation.
`RegisterEvent` now
also has its public registration overloads, a retained registry key and registry view, and a helper callback that
writes registrations into that view. The networking
registration boundary includes `RegisterPayloadHandlersEvent` and fluent `PayloadRegistrar` methods. The façade
also emits the common `IEventBus.addListener` overloads with `EventPriority`, event classes, and cancellation
flags, so listener-heavy NeoForge constructors keep linking. This keeps the usual constructor registration
sequence linked; the native host now records the direct `RegisterEvent.register` supplier overload as a
`RegistryEntry`, alongside bounded listener dispatch and the `RegistrySink` export. Supplier values remain
opaque callback markers until a Horizon-owned adapter executes them; live registry wiring remains follow-up work.
The host now recognizes NeoForge listeners that opt into cancelled events and excludes default listeners from
cancelled dispatches, while Fabric listeners remain eligible for the array-backed event callback even when the
event marker is cancelled; both loader families retain registration order.
`PayloadRegistrar` now retains the registrar returned by one `RegisterPayloadHandlersEvent` and counts its
play and configuration registrations in each direction, so payload bootstrap can inspect a stable
registration object before the native channel boundary is attached.
When a target references NeoForge, the generator emits these public ABI shims directly, so the path does not
depend on shipping NeoForge's jar. A synthetic `@Mod` that creates a `DeferredRegister`, attaches it to the
event bus, and registers an object now completes through both `horizon-jvm` and the native probe.

There is now an independent native probe, `crates/horizon-native-java`, for environments that cannot ship a
JVM. It reads a mod jar and its `fabric.mod.json` or NeoForge `@Mod` entrypoints in Rust, parses classfile
constant pools and `Code` attributes, and executes a deliberately bounded bytecode subset. Constructors
(including NeoForge constructors with loader arguments), method arguments and locals, integer constants,
checked arithmetic, integer bitwise/shift operations, primitive narrowing and comparison operations, distinct
jar-owned object allocation, stack duplication and swap forms, conditional branches,
`goto`, `tableswitch`/`lookupswitch`, bounded arrays (allocation, length, loads, and stores),
bounded in-jar instance fields, class initialization (`<clinit>`) and jar-owned static fields, returns,
`invokeinterface`, and calls through the explicit `Host` trait are covered; wide constants, `checkcast`,
`invokedynamic` sites now parse `BootstrapMethods`/`MethodHandle` metadata, preserve captured values, and
resolve supported in-jar callback targets; unresolved targets remain host-visible callback markers
(bootstrap execution remains bounded), and exception paths have neutral bounded fallbacks. An unsupported opcode
still returns a diagnostic naming the class, method, program counter, and opcode. The probe now reaches
Lithium (8 host calls), Spark (9), FabricProxy-Lite's server entrypoint (6), Chunky (5), ServerReplay (53),
Logfare (120), and the workspace's NeoForge 1.21.11 ABI probe constructor (165) now complete without a JVM.
Instance calls preserve their receiver through `Host::invoke_instance`, so a game-owned host can implement
stateful object contracts without changing the interpreter's classfile boundary. Jar-owned `new` allocations
also receive distinct bounded identity markers, so instance fields from separate Java objects do not alias
inside one native invocation.
This is a real native execution boundary, not a claim of full JVM compatibility: general reflection,
Kotlin runtime behavior, and loader APIs remain on the embedded-JVM path until their native host contracts are
implemented. Class literals support bounded metadata queries (`getName`, `getSimpleName`, `getTypeName`,
primitive/array checks, and nominal `isAssignableFrom`) without loading classes from disk; `Class.forName` resolves
only already-loaded jar classes, while reflective member enumeration is limited to methods declared by those loaded classes. `getMethods` and
`getDeclaredMethods` return bounded arrays of method markers; `getMethod`/`getDeclaredMethod` can produce a bounded marker,
including an exact descriptor when the parameter `Class[]` is available, and `Method.invoke` dispatches only to a
matching overload already loaded from the jar. Bounded `Method.getName`, `getParameterCount`, and `toString`
metadata accessors expose that marker without loading classes. The bounded runtime also
handles `java.lang.reflect.Array` `newInstance`, `getLength`, `get`, and `set` against arrays it owns during the
current invocation. The crate
also exposes a deterministic `StdHost` adapter for a small set of side-effect-free
queries (`System.getProperty`, primitive parsing, line separators, string receivers, and math helpers); every
external call is recorded with its receiver and arguments for later dispatch. The CLI keeps the neutral `NullHost` default so probe results remain independent of
machine properties, while a game-facing host can opt into Horizon-owned mappings without coupling the
interpreter crate to `horizon-client`. Unknown calls keep the neutral return contract.
The standard-library surface also includes deterministic `Integer.compare`/`toHexString`, integer
`Math.floorDiv`/`floorMod`/`addExact`, and
bounded `Long.compare`/`toHexString`,
`StringBuilder`/`StringBuffer` append and
`toString` values, primitive `String.valueOf` overloads, plus `Objects.equals`, `requireNonNull`, and
`toString`, which covers common loader bootstrap formatting and null checks without a JVM.
String `replace` (character and `CharSequence` forms), `indexOf`, `lastIndexOf`, `trim`, `strip`, and `isBlank` are also
deterministic, covering namespace/path normalization used by loader metadata and registration code.
`System.getProperties()` exposes the seeded properties as a bounded `Properties` map, so bootstrap code
can use the same key/value queries as `getProperty` without crossing the host boundary.
`StdHost` also exposes deterministic Fabric loader values for mod-presence checks, metadata ids, names,
versions, empty `getProvides` and `getDependencies` collections (including
`Version.getRaw`/`getFriendlyString`), environment type (`CLIENT` by default, or
`SERVER` via `with_environment_type`, including `EnvType.isClient`, `name`, and `ordinal`), and the
game/config/mods paths, plus a deterministic `getAllMods` list backed by the seeded containers; callers seed the loaded-mod map with
`with_mod`. NeoForge's `ModList.get().isLoaded`, `getMods`, and `getModContainerById` use the same seeded map and
return deterministic container markers, covering the common loader-side dependency checks. Common
`Optional.empty`/`of`/`ofNullable` constructors and
`isPresent`, `isEmpty`, `map`, `filter`, `equals`, `hashCode`, and `toString` preserve bounded optional
value semantics for loader bootstrap code, while unresolved callbacks remain explicit markers. Common
`java.nio.file.Paths.get` and `Path.resolve`, `toString`, `normalize`, and
`toAbsolutePath` calls use deterministic path markers. Common `List.of`, `Set.of`, `Map.of`, empty/singleton
`Collections` factories, and bounded `size`, `isEmpty`, `contains`, `containsAll`, `get`, `Map.get`, `Map.getOrDefault`, and
key/value queries use escaped value markers; `Map.keySet`, `Map.values`, and `Collection.iterator` project those markers into bounded
set/list views, while `Collection.forEach` invokes bounded `Consumer` callbacks, so bootstrap code can inspect
immutable configuration collections without starting a JVM. Boxed
`Integer` and `Boolean` values preserve deterministic markers through `valueOf`, unboxing,
equality, hashing, and string conversion. Bounded `Double` and `Float` parsing, boxing, unboxing,
`isNaN`/`isInfinite`, and Java-style string conversion use deterministic floating-point markers;
malformed values remain explicit metadata errors. `java.util.UUID.fromString`, deterministic `randomUUID`,
`toString`, `equals`, and `hashCode` preserve canonical UUID identity markers without process randomness;
malformed UUID strings remain explicit metadata errors. `Enum.valueOf`, `name`, `ordinal`, `equals`,
and `hashCode` preserve deterministic enum markers for loader and configuration branches. Collection `stream()` supports bounded `map`, `filter`, `distinct`,
`limit`, `skip`, `count`, `toList`, `isEmpty`, `findFirst`, `findAny`, `anyMatch`, `allMatch`, `noneMatch`, and `forEach` chains using the same in-jar callback bridge. `Optional.stream()` also produces a bounded zero-or-one-element stream, so optional registry lookups can enter those chains without a JVM.
Native `ArrayList`, `HashMap`, `LinkedHashMap`, `HashSet`, and `LinkedHashSet` markers also retain bounded
constructor, add/put, get, size, clear, indexed insertion/replacement, set duplicate suppression and removal,
list removal and index queries, `List.addAll`, `Collection.containsAll`, `removeAll`, and `retainAll`,
and `Map.putAll`, `putIfAbsent`, `merge`, conditional `replace`, and conditional `remove` updates for bootstrap
code that builds mutable registration tables. The bulk operations consume the same escaped collection
markers produced by `List.of`/`Map.of`, and interface-owner calls (`List`/`Map`) dispatch to the mutable
receiver where the marker is one of the supported concrete collections. The state is invocation-scoped
and bounded; distinct Java object identity and concurrent collection semantics remain on the JVM fallback
path.
The common `Optional.map`/`filter`/`ifPresent`/`orElseGet`/`orElseThrow` callback flow executes supported in-jar
`Function`, `Predicate`, `Consumer`, and `Supplier` targets and keeps neutral markers for unresolved callbacks. Its NeoForge mappings keep
the context, event-bus listener, deferred-register, registry-event, and payload-registration chains alive;
listener lambdas arrive as captured `@indy:...` markers in the recorded invocation stream. `StdHost` exposes
those listeners through `dispatch_event` and records deterministic `RegistryEntry` values and holder lookups,
which is the first dispatchable host contract. `NativeJar::execute_dispatch` now resolves supported callback
targets, restores captured receivers, appends the event, and invokes the callback through the bounded native
interpreter. The host also carries the common `Identifier`/`ResourceLocation`/`ResourceKey` value operations (parse, namespace,
path, short/language keys, suffixes, equality, key identifiers, and the common identity path-function
callback) as deterministic markers. The native host also carries the text-bearing
`Component`/`MutableComponent` subset (`literal`, translatable factories, `getString`, `append`, and
`copy`) as deterministic markers, so loader diagnostics and registration messages do not immediately
fall back to the JVM path. `RegistrySink`/
`apply_registrations` is the matching game-independent export seam: Horizon can
commit those entries into its native tables without coupling the interpreter crate to `horizon-client`.
Fabric's array-backed `EventFactory.createArrayBacked` now returns a deterministic event marker;
`Event.register` listeners are retained alongside NeoForge bus listeners, `Event.invoker` returns a
stable invoker marker, and `dispatch_event` presents both loader families through the same callback
work-item surface. This keeps Fabric callback registration observable without pretending to execute
an arbitrary event combiner in the bounded host. NeoForge `IEventBus.post` also records posted event
values and returns a deterministic cancellation result for the explicit `event/cancelled/...`,
`cancelled/...`, and `@cancelled:...` markers; full event hierarchy and mutable cancellation state
remain a native hook boundary. Event callbacks can query inherited `isCanceled()` against those markers;
changing cancellation still belongs to the native hook boundary. `PayloadRegistrar` calls now retain a bounded direction/type/codec/handler
record through `StdHost::payload_registrations`, leaving byte encoding and channel ownership to Horizon's
network boundary.
The generated NeoForge `DeferredHolder` façade now retains its id, registry key, and lazy supplier: `get`,
`value`, `isPresent`, `getOptional`, `getId`, and `getKey` agree on one holder state. The matching
`DeferredRegister` shim binds the namespace and supplier when `register` is called, so common registry
bootstrap code can inspect holders before the native registry sink commits typed declarations.
Unsupported callback bodies still return the interpreter's explicit bounded diagnostics. The sink is
deliberately an export boundary today: Horizon's prepared block and item registries are immutable once
`content_root` has loaded, and an opaque Java object marker cannot safely become a native block or item.
The typed declaration path runs before `Assets::load`; jars without declarations still report registrations
without mutating the live game tables.

The typed half of that boundary now lives in `horizon-mods::native::NativeDeclarations`. It implements
`TypedRegistrySink`, validates names and block/item/effect properties, and collects Horizon's existing
`Block`, `Item`, and `MobEffect` declarations for the pre-`Assets::load` phase. `Content::from_native`
turns that collection into declaration-only content that the existing resource planner consumes. The game's
explicit `--java-mod` path runs this adapter before `Assets::load` and passes the resulting content through
`content_root`; opaque Java registrations remain diagnostic until a typed declaration is supplied.
`NativeDeclarations::from_registry_entries` and `register_entries` provide the all-or-nothing batch seam
for combining typed entries parsed from `META-INF/horizon-native.json` with typed registrations recorded
during entrypoint execution. An opaque or invalid entry aborts the batch and rolls back the staged set, so no partial declaration
can reach the immutable resource tables. Native content derives its owned namespace fence from every typed
block, item, and effect identifier, so extra namespaces participate in the same clash checks as packaged
content before the resource root is built.

The discovery scanner has a regression fixture for a top-level NeoForge `@Mod` annotation marker;
the constructor path is therefore testable without requiring a full NeoForge game launch. A synthetic
NeoForge-marked jar now runs end to end through discovery and constructor completion in `horizon-jvm`.

## The runtime

Four pieces.

| Piece | Language | What it is |
|---|---|---|
| `horizon-compat` | Java, built with a pinned JDK | The class library: `net.minecraft.*`, `net.fabricmc.*`, `net.neoforged.*`, Mixin's bootstrap. Mostly generated from a spec; a few thousand lines of real logic. |
| `crates/horizon-jvm` | Rust | Embeds the JRE, owns the classloader and the lifecycle, exports the ABI, dispatches events. |
| `crates/horizon-native-java` | Rust | Optional bounded interpreter for simple entrypoints; explicit host callbacks and diagnostics, with JVM fallback. |
| `crates/horizon-mod-abi` | Rust | Census, mapping catalog, remapper/shim generator, verdicts. Offline tool; the game never needs it. |

Fabric Loader and Mixin are **used as they are** (Apache-2.0, MIT): discovery, dependency resolution,
entrypoints, `Mixin` weaving, access wideners, JiJ. We are a game provider to Fabric, not a reimplementation
of it. NeoForge's loader is not usable that way, so `horizon-compat` supplies an FML-shaped surface
(`@Mod`, `IEventBus`, `DeferredRegister`, `ModContainer`, `PayloadRegistrar`) implemented from NeoForge's
public documentation — the same rule `native-hooks-neoforge.md` already sets: names and contracts from
docs, nothing read or copied from their source.

### The object contract

The single most important design decision, because it decides whether the boundary is 30 ns or 30 µs.
Three classes of object, defined by who owns the truth:

**1. Values.** `ResourceLocation`, `Direction`, `BlockPos`, `Vec3`, `AABB`, `Component`, `CompoundTag`, `ItemStack`,
`BlockState`. Java owns the value; Rust reads it back at the boundary. `BlockState` is a singleton per
registry id (no allocation per read, as vanilla caches them); `ItemStack` is Java-owned with a dirty flag
and is pushed to Rust at the points where the simulation can see it (use, menu click, container transfer,
block-entity save, entity tick).

**2. Registry objects.** `Block`, `Item`, `BlockEntityType`, `MenuType`, `EntityType`, `MobEffect`,
`RecipeType`, `DataComponentType`. One Java instance per registered id, built at load from Horizon's
registry, immutable after that. Identity is the id. No boundary cost at all.

**3. Live objects.** `Level`, `ServerLevel`, `Entity`, `Player`, `ServerPlayer`, `BlockEntity`, chunk.
A Java wrapper around a stable handle `(kind, id, generation)`. Reads are cached with an epoch valid for
the duration of one callback; writes go to Rust immediately; a removed object's handle throws what
vanilla would.

The rule that keeps this sane: **Horizon is authoritative, Java is a view that may be written through.**
No field on a live object is ever "the truth" for longer than one callback.

### The boundary

- **Rust → Java:** one dispatch entry per *event family*, not per hook — `horizon_dispatch(event_id,
  payload)`. Adding a hook adds no JNI symbol. Payloads are a flat buffer, not JNI objects.
- **Java → Rust:** a generated `Native` class of `static native` methods, one per ABI function, with a
  version constant checked at load (`horizon_mod_abi_version`), so a mismatched compat jar refuses with a
  sentence instead of a `NoSuchMethodError` in a mod's tick.
- **Per-hook bitmap:** a point with no listener costs one predictable branch in Rust. This is the "free
  when unused" rule (`modpacks.md`), applied per hook rather than per mod.

### Threads

Three long-lived threads attach to the JVM once at startup: the window's, the server thread's, the
network thread's. Java-side caches for client objects are per-thread.

The client is where this design gets a large gift: **`horizon-compat` has no graphics API to fake.**
A mod's `GuiGraphics.blit(...)` records a draw command into a list Horizon's `gfx` renders; `PoseStack`
is a matrix stack of our own; there is no OpenGL, no LWJGL, no `RenderSystem` state to emulate beyond
bookkeeping. Shaders (`ShaderInstance`) and anything that draws with a program of its own do not map and
are reported, not emulated.

### Hook-host classes

Some façade classes exist only to be injected into: `LevelRenderer.renderLevel` is an empty Java method
that Rust calls right after it draws the world, so a mod's `@Inject(at = TAIL)` runs at the right moment.
They are named in the catalog and are the cheapest possible coverage for a `@Redirect` on vanilla code
Horizon does not have an object for.

## The mapping

The part the request calls "then what that maps to here". Columns: the Java thing, what mods actually do
with it, what Horizon has, and the bridge's answer.

| Java side | What mods do with it | Horizon today | Bridge |
|---|---|---|---|
| `Registry`, `DeferredRegister`, `RegisterEvent` | register blocks, items, entities, menus | `mod_content` (`data/<ns>/horizon/*.json`), native registries | the native host records registration intent now; typed declarations must be exported before the immutable tables load so bridged and carried content share one namespace |
| `Holder`, `Holder.Reference`, `HolderSet` | retain a registered value, compare its key, inspect optional bindings | native registry handles and typed declarations | generated holder references retain a bounded value/key/id triple and expose `key()`; `registerForHolder` binds it, while `HolderSet` exposes empty-safe size, membership, stream, and factory defaults until live cross-mod membership is wired |
| `Block`, `BlockState`, `BlockBehaviour.Properties` | behaviour, states, shapes | `horizon-items/blocks.rs`, `horizon-world/level/` | façade superclass; calls route to Horizon's trait dispatch; unknown behaviours are reported per class |
| `ItemStack`, `DataComponents` | item data, NBT-free components | `items.rs`, `item_data.rs` | mirrored value; the component map is 1:1 with Horizon's, which is the luckiest alignment in this design |
| `BlockEntity`, `BlockEntityType`, `saveAdditional` | machines, NBT | `native_block_entity` family (API 15–18) | Java object owns the NBT mirror; Rust calls `tick`, `load`, `save` at the same points |
| `AbstractContainerMenu`, `MenuType`, `Screen` | GUIs | `native_menu*`, `native_screen*` (API 10, 17, 19, 20), `menu.rs` | a `ModContainer` per Java menu; slots mirrored; draw calls recorded |
| `Entity`, `LivingEntity`, `Mob`, `PathNavigation`, `Brain` | mobs, AI | `entities.rs`, `mobs/` | handle + callbacks; Horizon's AI stays authoritative for vanilla mobs, mod mobs own theirs |
| `Level`, `ServerLevel` | blocks, time, weather, entities | `GameLevel`, `horizon-world::level` | façade handle; common time/dimension/block reads are value-preserving, with `setBlock` and entity mutation still routed through the native level boundary |
| Capabilities (`ResourceHandler`, `EnergyHandler`) | machine interop | bounded `Capability`, `CapabilityToken`, `LazyOptional`, and `IItemHandler` façade shapes | live transfer still needs the `block_capability(pos, kind, side, out)` ABI proposed in `native-hooks-neoforge.md` |
| `PayloadRegistrar`, `CustomPacketPayload` | mod networking | `net.rs` channels | channel per payload type; the vanilla protocol is untouched |
| `@SubscribeEvent` listeners | lifecycle, interaction | the hook catalog | 1:1 with named hooks (`native-hooks-neoforge.md` rule 1) |
| Models, textures, blockstates, lang | everything visual | `horizon-assets`, `model_library` | the jar's `assets/` is the content pipeline's job already — no Java needed |
| Data generation (`DataProvider`) | build-time only | — | ignored at run time |
| Access wideners / transformers | reach into `net.minecraft` | — | applied to the façade, which is generated to permit it |
| Config screens (`ClothConfig`, `ModMenu`) | settings UI | native mods' own screens | map onto `native_screen*`, or report |

The ABI census classifies mod classes extending `Level`, `ServerLevel`, and sibling `world/level/*`
types as `world-access` behavior. This keeps level implementations from being misreported as content-only
while the façade handle and Rust mutation boundary are completed.

The census also routes descriptor-bearing member references into four ABI queues:
`facade` for `net/minecraft`, `loader` for Fabric and NeoForge APIs, `jdk` for
standard-library owners, and `external` for carried dependencies or explicit
shims. The complete `owner#name descriptor` key remains available, so overloads
do not collapse while the aggregate drives implementation order.

Two rows deserve their own paragraph, because they are where a naive bridge dies:

**Registries.** A bridged mod's block must be *the same kind of thing* as a carried content mod's block,
or a pack that uses both gets two universes. The existing content root is the eventual integration point:
the bridge will contribute typed rows to the same tables (`facts.json`, `items.json`, state ids after
vanilla's), and the "vanilla state ids never move" rule (`modpacks.md`) will hold. The Java `Block` object
can then be a façade over the row the pipeline already knows how to load. The current native boundary stops
before this conversion: it records `RegistryEntry` values and leaves the live tables unchanged until the
typed declaration format and pre-`Assets::load` phase exist.

**Item data.** 1.20.5 moved `ItemStack` from NBT to data components, and Horizon made the same move
independently. A Java `ItemStack` is therefore a component map on both sides of the boundary, and the
copy is a handful of small objects rather than an NBT tree walk. This one coincidence removes what
would otherwise have been the worst hotspot in the design.
The generated façade now preserves the common item/count value path (`getCount`, `setCount`, `isEmpty`,
`getItem`, `copy`, `copyWithCount`, `grow`, and `shrink`) plus bounded stack inspection (`getMaxStackSize`,
`hasFoil`, `isDamageable`, `isSameItem`/`isSameItemSameTags`, bounded damage/enchantment/use-duration inspection, and `getHoverName`) so bootstrap code
can inspect stacks before the full component bridge is attached. `Item.getDefaultInstance` now creates a one-item
stack retaining the item identity, and stack copies preserve damage alongside their component map. It also has a bounded erased component map: `get`,
`getOrDefault`, `set`, `remove`, `has`, and `hasNonDefault` use the `DataComponentType` object as the
key; generated `DataComponents` constants are distinct identity keys, absent values read as `null` (or
the supplied default), and copies retain the map. This covers
the common registration and tooltip paths while the Rust mirror grows typed accessors for the modelled
`StackData` fields in `horizon-items/item_data.rs`.
BlockState bootstrap paths retain a bounded block identity and property map: `getBlock`, `is`, `isAir`,
`canOcclude`, `isSolidRender`, and `getLightEmission` provide stable inspection defaults, while
`setValue` feeds `getValue`, `hasProperty`, and `getProperties` on the same façade instance. This covers
common state construction and inspection while the registry-backed state definition and vanilla default
values remain follow-up work. `hasBlockEntity` and `isRandomlyTicking` expose explicit empty-safe defaults
for bootstrap checks.
The `ItemStack.is(TagKey)` and `BlockState.is(TagKey)` overloads are also linked with an explicit empty
membership result until registry-backed tag tables are attached.
`TagKey` now retains its registry key and identifier as a value pair: `create`, `location`, `registry`,
`isFor`, equality, hashing, and string conversion operate on those fields. This keeps tag-backed block,
item, and entity registration code from collapsing distinct tags into one neutral object; holder-set
queries now have empty-safe bounded defaults, while `HolderSet.direct` preserves a singleton holder's
size, identity membership, stream, and indexed access. Live cross-mod membership remains outside the façade.
`ResourceKey` exposes the corresponding `location`, `registry`, `registryKey`, and `isFor` value operations,
so code that compares a registry family or resolves an entry's identifier keeps its namespace/path identity
through the generated façade. Generated `Registry`/`DefaultedRegistry` values now retain bounded
identifier-to-object entries plus each entry's `ResourceKey` for `register`, `containsKey`, `get` (by identifier or key),
`getOptional`, reverse `getKey`, and `getResourceKey` queries, so bootstrap code can observe the
registrations it just made without losing registry-family identity. `registerForHolder` updates the same
bounded table and binds the returned holder to that key/value pair. The live native registry sink still
owns cross-mod membership and final table construction.
NeoForge capability bootstrap now has the same bounded value layer: `CapabilityManager.get` returns a
registered `Capability`, `CapabilityToken` preserves the generic lookup shape, `LazyOptional` supports
lazy/nullable construction, `Capability.orEmpty` identity filtering, presence, mapping, fallback, listeners, and invalidation, and
`IItemHandler`/`IItemHandlerModifiable` expose the inventory ABI methods. These shims keep capability
registration and optional lookups linkable without copying NeoForge implementation code. The native host
now records `Level.getCapability` and `BlockEntity.getCapability` requests with capability, position, and
side markers; a game adapter can seed a handler value for an exact request, while unseeded lookups remain
empty. Actual block, side, and item transfer still belongs to the live native ABI rather than a fabricated
Java result.
The handler façade now keeps a bounded one-slot state per handler: reads return copies, inserts cap the
stack at 64 and honor simulation, extraction decrements state, and the modifiable variant can replace the
slot. The native `StdHost` also preserves capability markers and callback-local `LazyOptional` values
through presence, fallback, mapping, and invalidation queries. These value layers are only for bootstrap code
that needs a stable inventory shape; they are not connected to the live block/side capability table, which
remains the explicit native boundary.

## Mixin

Mixin weaves into *our* classes, so we choose their shape. That is the lever: the façade is generated to
satisfy the ecosystem's injections (real `@Shadow` fields, real target methods), not to mirror vanilla's
arbitrary internals.

| Construct | Works | How |
|---|---|---|
| `@Inject` at HEAD / RETURN / TAIL | yes | the hook at that named point; the body runs in Java with the mirrored arguments |
| `@Inject` at INVOKE / FIELD | only on demand | a per-callsite hook, generated when the census shows more than a handful of mods want it |
| `@ModifyVariable`, `@ModifyArg`, `@ModifyConstant` | yes | the hook exposes the variable by name |
| `@Redirect`, `@WrapOperation` | yes | a per-callsite hook hands the callee's arguments to the mod and takes its result |
| `@Overwrite` | only where Horizon already routes the method to a hook | otherwise refused, with the method named |
| `@Accessor`, `@Invoker` | bounded | target stubs materialize public interfaces for linkage; common JDK/collection returns are empty or neutral values, while game generic signatures and field/call semantics still need a hook |
| `@Shadow` | yes | fields and methods exist because the façade was generated with them |
| Mixin into a class with no façade | no | the census lists it; adding a façade class is the fix, not a workaround |

The rule that keeps this honest: **a method that receives many mixins becomes mod-owned at that point.**
Rust stops deciding and calls Java there — the same move as a named hook, but chosen by data rather than
by taste. The census is the data: 670 points over 264 classes, with `LivingEntity` (34), `Minecraft` (25)
and `Hud` (23) at the top ([mod-abi-census.md](mod-abi-census.md)).

## "Translate any jar": five outcomes

This replaces today's three-way `check` verdict (`Works` / `Partly` / `Doesn't run`) with five, and the
launcher shows the bridge outcome next to the port outcome.

| Tier | What happens | The jar |
|---|---|---|
| **0 — runs** | links against the façade as-is | untouched |
| **1 — remapped** | names translated where a 26.3.x minor moved something, or the jar was built against a 26.3 snapshot; data files upgraded by the existing carry | rewritten in memory |
| **2 — shimmed** | a generated jar supplies classes the mod expects that Horizon models differently (nested libraries, shadowed APIs) | wrapped |
| **3 — ported** | the existing agent pipeline writes a native mod (`modpacks.md`) | replaced |
| **4 — refused** | native libraries, direct GL, reflection into internals, `@Overwrite` of a core method — named, with the reason | rejected |

Tier 1 is small *because* of the 26.3-only scope. With the game unobfuscated since 26.1, a 26.3 Fabric
jar and a 26.3 NeoForge jar name classes the same way, so there is no intermediary/SRG table and no
remapper in the runtime. What is left is drift inside the 26.3 line and jars built against snapshots — a
table of tens of entries, not thousands. That is the dividend the narrowing buys.

**Tier 5 (later, if profiling asks):** take a bridged mod's hot methods and compile *those* to native
while the rest stays on the JVM. A compiler reading bytecode is neither a person reading mod source nor
an agent porting it, so `modpacks.md`'s licence policy is intact.

## Performance budget

Measured, or it is a wish. Targets, to be replaced by numbers in phase 1:

| What | Target | Why |
|---|---|---|
| JVM start | ≤ 400 ms with 0 mods, ≤ 2 s with 100 | one-time |
| Memory | ≤ 150 MB base + ~1 MB/mod | comparable to vanilla's own modded footprint |
| Java → Rust call | ≤ 30 ns | 10k block reads/tick ≈ 0.3 ms |
| Unhooked hook point | one branch | the "free when unused" rule |
| A tick hook with listeners | ≤ 50 µs for 20 mods | 1 ms is 5% of a 20 TPS tick |
| Full GC | not on the render thread | the one pause users feel |

The honest risk: GC. A JVM pause inside a frame is a hitch, and mods allocate like it is 2013. Mitigations
are ZGC/GenZGC settings, keeping mod code off the render thread except for draw-list building, and the
per-mod allocation profile in the census. This is the number to watch; if it cannot be held, the bridge
stays server-side and client mods stay ports.

## Trust

A bridged jar is third-party code the registry never reviewed, running with the player's rights. That is
worse than a native mod, which at least passed a human.

- Same stance as `native-mods.md`: no sandbox promise, a warning before the first one, a per-instance
  opt-in, and the launcher naming the mod that was running when the JVM died.
- A `Thread.UncaughtExceptionHandler` at every dispatch catches `Throwable`, blames the mod, and offers
  "open without it" — never a crash to the desktop for a mod's `NullPointerException`.
- Out-of-process (child JVM, restricted filesystem view, shared-memory object arena) is the real
  containment and costs 2–5 µs per crossing instead of 30 ns. Recommendation: **in-process for the
  bridge, out-of-process only if abuse becomes real.** It is a product decision, listed below.

## Multiplayer

- Single player and LAN host: the JVM lives in the host's process, exactly like native mods.
- A bridged mod's registry entries travel the same way content mods' do; a client without them is
  refused, as today.
- Dedicated server: the same binary with `--dedicated`. Phase 10, but phase 3's server-side scope is
  deliberately most of the way there.
- Mods that change packets outside their own channel: reported, not emulated.

## How it is tested

The repo already has the right machine: a Java oracle, run against the real jars, compared with the Rust
side (`tools/parity.py`, `docs/parity-workflow.md`). The bridge adds a sibling:

`tools/java_mod_parity.py` — start a real NeoForge/Fabric instance with a mod and a fixture world, run a
scripted scenario, record the trace (block changes, entity positions, inventories, chat, sounds); then the
same scenario on Horizon with the same mod through the bridge, same seed, same script; diff the traces.
Per-mod gate: identical, or a documented difference with the reason.

Early gates that do not need choosing winners:

- **Phase 1:** Fabric API's own test mods register a block, an item and an entity, and one of its own
  mixins applies.
- **Phase 3:** that whole test-mod set runs; failures are counted, not argued about.
- **Phase 4:** the top 100 Fabric mods by downloads: how many reach tier 0–2.
- **Phase 5:** the biggest NeoForge pack on 26.3: the `check` verdict distribution, before and after.

## Phases

Estimates for one small team; the gates are the real content, the weeks are reassurance.

| # | What | Weeks | Gate |
|---|---|---:|---|
| 0 | **Done 31 Oct 2026**: census tool, 56 jars measured ([mod-abi-census.md](mod-abi-census.md)) | 3 | mod corpus 93% at 200 classes, 883 MC classes: go |
| 1 | **Started 1 Nov 2026**: JVM embedded, façade generated, a real mod's entrypoint runs; loader discovery, registries and one mixin remain | 6 | Fabric API test mod's block, item and entity load and appear |
| 2 | Server-side surface: blocks, items, block entities, entities, the object contract | 10 | 20 real data/logic-tier Fabric mods run from jars |
| 3 | NeoForge loader, event bus, `DeferredRegister` | 8 | **ship: "Compatibility" instance, server-side, Fabric + NeoForge** |
| 4 | Mixin surface: the 264 classes and 670 points the census ranks, split between Java-owned methods and new hooks | 8 | every point either has a hook or a test that its Java-owned method runs the mixin |
| 5 | Menus and screens | 8 | a machine mod with a GUI |
| 6 | Capabilities, networking, attachments | 6 | two machine mods talk to each other through a capability |
| 7 | Client: HUD, keys, tooltips, item and block models | 8 | a HUD mod and an item-model mod |
| 8 | Client rendering: entity/block-entity renderers, particles, animated models | 12 | **ship: client-side bridge** |
| 9 | Dedicated server, registry sync, performance hardening | 6 | a bridged server with guests |

About 72 of the 75 serial weeks remain; phases 1–2 and 4 are parallelizable, and 3 is the first thing a
player can feel. There is no "old version" phase: the 26.3-only scope removed it.

## What this costs, and what it does not do

**The cost nobody sees in the table:** every gameplay feature now has a second expression, the façade,
and it can drift from the engine's. The mitigation is that the façade is *generated from the same tables
the engine loads* (registries, components, block facts), so drift is a build failure, not a bug report.

**It does not do:**

- make bridged mods fast — that is what ports are for;
- run mods that ship native code (LWJGL users, JNI), or replace the renderer;
- emulate shaders, or `RenderSystem` calls with no draw-list equivalent;
- survive a mod that reflects into internals we shaped differently — the census predicts this, it cannot
  prevent it;
- remove the need for the port pipeline. Deep tier stays ports; the bridge just stops them being the
  *only* path, and stops a pack's libraries and integrations being dead weight.

**The economy, plainly:** today, support scales with agents × mods × versions. With the bridge it scales
with the mapping table, and the marginal cost of the next mod is zero. That is the whole argument.

## Decisions needed

1. **In-process JVM, or child process with a shared-memory arena?** Recommend in-process now, revisit with
   data.
2. **Ship a JRE?** Recommend yes (a pinned Temurin, ~45 MB compressed, GPLv2+CE); requiring a system Java
   loses most players.
3. **Does a bridged world get its own instance kind?** Recommend yes: "Compatibility" beside native, so a
   world never mixes a bridged mod's registrations with a port's.
4. ~~Legacy Forge?~~ Settled by the 26.3-only scope: NeoForge, and no old-version path. If a 1.21.1 pack
   ever matters, that is a second name table, not a second loader.
5. **Does `AGENTS.md`'s "everything players run is Rust" gain the carve-out?** Without it this page is
   fiction.
6. **Do we accept that a bridged mod is a vanilla-speed mod?** The performance story stays with ports;
   saying otherwise to players would be a lie.

## First week

1. Extend `classes.rs` to record member references and Mixin annotations; run it over 200 jars; print the
   top 100 classes and the no-hook attachment points.
2. `jni` + `JNI_CreateJavaVM` in a throwaway binary; call a Java method that returns a `long`; measure
   the round trip on this machine.
3. Boot Fabric Loader with an empty `fabric.mod.json` and no game; see where it insists on a game.
4. Write the census result into this page as measured numbers, then decide whether phase 1 starts.
