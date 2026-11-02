# Java mod attachment surface: first census

Status: measured, 31 October 2026. Phase 0 of [java-mod-compat.md](java-mod-compat.md). The tool is
`horizon-port-pack census`; the numbers below are its first run.

This is the "map exactly what Fabric and NeoForge attach to" step, done by counting instead of guessing:
every class, field, method and Mixin point the jars' code names, from their constant pools and
annotations. No jar is decompiled and none is run.

## What was measured

| Corpus | Jars | What it is |
|---|---:|---|
| Fabric API `0.161.0+26.3` | 46 modules + the bundler | the modules, as `hd-bestiary-scratch/fabjars` has them; a library, so it over-represents the long tail. The bundler is read alone to check nested-jar support |
| Hollow Dawn `0.1.0` | 8 | a real 26.3 Fabric mod (content, commands, worldgen, client), built for `minecraft ~26.3` on Java 25 |
| Better Realism `0.1.0` | 74 | the whole pack: 26.3 Fabric, client-heavy (Sodium, Iris, Entity Texture Features, FancyMenu, Kotlin) |
| Mezzo Clef | 1 | a pre-26.1 Fabric mod, kept as a control |

Reproduce:

```sh
cargo run -q -p horizon-port-pack -- census <jar or folder> --hooks native-hooks.json --out census.json --top 20
```

The JSON holds the ranked surface per jar and merged; the command prints the merged top.

For native hook translation, `census.json` also includes `hook_points`: a map keyed by
`target#method`. Its rows retain the injection `kind`, the exact `at` (`@At`) value, count,
and any matching native hook names in the explicit `native_hooks` array. An empty array is the
unsupported-mixin diagnostic: the point needs a façade-owned Java method or a new native hook.
Accessor, invoker, and shadow rows are omitted because
they require member shape rather than a call-site hook. This keeps consumers from having to
filter and regroup the ranked `points` array.

The report labels the bounded diagnostic explicitly. Mixin targets found by reading a
`*.mixins.json` file are **report-only declarations**; the census does not execute a config or
weave bytecode. Injection points whose target method has a native hook are **hook-covered**.
The remaining call-site points are **report-only** and are counted in `points_report_only`, so an
unsupported transform is visible without implying that the tool implements weaving.
`mixin_config_targets_report_only` counts the target declarations collected from config JSON.
Each jar also reports the bounded `mixin_config_fields` map: for each config that declares one
of the common edge fields (`plugin`, `refmap`, `required`, `minVersion`, or
`compatibilityLevel`), it records the field names that are present. Values are not interpreted,
plugin callbacks are not run, and this metadata never enables weaving.

The census also discovers loader access metadata without applying it. A Fabric jar's
`accessWidener` path in `fabric.mod.json` is read and its access, extendable, mutable (including
transitive forms) targets are listed. NeoForge/Forge `accesstransformer.cfg` resources are listed by
target after their access modifier. Each jar contributes at most 256 targets of each kind;
the merged JSON and text summary include counts by target. This is an attachment-surface
report only: bytecode access changes remain outside the census. The CLI summary also prints a
single `access metadata` line with the total target declarations and an explicit report-only
marker, so a run's output makes clear that no access changes were applied.

## Fabric API 26.3

2,099 classes making **18,627** class references, **21,779** member references, **570** mixins and
**1,100** injection points.

**883 distinct `net/minecraft` classes.** That is the façade's real workload, and it lands inside the
1,000–1,500 the design assumed.

Coverage is the important number, because the plan's go/no-go gate is about it:

| Top | Share of `net/minecraft` references |
|---:|---:|
| 10 | 17.4% |
| 20 | 26.7% |
| 50 | 42.3% |
| 100 | 56.2% |
| 200 | 71.4% |

**This corpus fails the gate** — and it should. Fabric API is a wrapper around the whole game; its job is
to touch everything. The tail here is not noise, it is the API's definition.

The mixin surface is much friendlier than the raw class count suggests:

| Injection | Count | Share |
|---|---:|---:|
| `@Inject` | 399 | 36% |
| `@Shadow` | 300 | 27% |
| `@WrapOperation` | 110 | 10% |
| `@Accessor` | 90 | 8% |
| `@Redirect` | 56 | 5% |
| `@ModifyExpressionValue` | 44 | 4% |
| `@ModifyArg` | 32 | 3% |
| `@Invoker` | 28 | 3% |
| others | 40 | 4% |
| **`@Overwrite`** | **1** | **0.1%** |

`@At`: **HEAD 136, RETURN 112, TAIL 57** — 45% of all injection points are the three easy anchors. The
rest are `INVOKE`/`FIELD`/`NEW` targets, which are the per-callsite hooks the design budgeted for.

The classes mods weave into, most first: `Entity` (9), `LivingEntity` (9), `Minecraft` (8),
`MinecraftServer` (8), `ClientPacketListener` (7), `ServerPlayer` (5). That is the façade's build order,
ranked by the ecosystem itself.

## The hooks at those points

`native-hooks.json` now carries a `vanilla` field: the class and method each hook fires in, taken from the
hook's own doc comment and verified against the 26.3 jar with `javap` (19 of 45 hooks; the rest are
Horizon-only, like `zoom` or `minimap`). Joined against Fabric API 26.3:

**7 of 689 injection points (1.0%) land on a method a hook already names**, at six methods:
`ServerPlayerGameMode#destroyBlock` (`destroy_block`), `#handleBlockBreakAction` (`left_click_block`),
`#useItemOn` (`right_click_block`), `Player#attack` (`attack_entity`), `ItemStack#useOn` (`use_item_on`)
and `FlowingFluid#spreadTo` (`block_spread`). Hollow Dawn does worse: 0 of its 3 points (on
`MinecraftServer` and `ChunkGenerator`).

The census now reports `@Accessor`, `@Invoker`, and `@Shadow` separately as member-only points. They
still require the façade to expose the right field or method, but they do not inflate the Rust call-site
hook backlog.

That is the honest state of the catalog: it was designed around NeoForge's *events*, not around the
methods Fabric API actually mixes into. The other 670 points sit on **264 classes**; the biggest:

| Class | Points |
|---|---:|
| `net/minecraft/world/entity/LivingEntity` | 34 |
| `net/minecraft/client/Minecraft` | 25 |
| `net/minecraft/client/gui/Hud` | 23 |
| `net/minecraft/server/MinecraftServer` | 21 |
| `net/minecraft/client/multiplayer/ClientPacketListener` | 16 |
| `net/minecraft/client/renderer/LevelRenderer` | 15 |
| `net/minecraft/world/entity/Entity` | 15 |
| `net/minecraft/server/level/ServerPlayer` | 12 |

**Read that number correctly.** A point does not always need a new Rust hook. If the façade class
implements the method *in Java* — `LivingEntity.tick` as Java that calls Rust for world access — every
mixin into `tick` works with no hook at all; that is the hook-host design in
[java-mod-compat.md](java-mod-compat.md). A Rust hook is needed only where the method's decision lives in
Rust. So 670 is the upper bound, and the real work is choosing, per class, between a Java-owned method and
a named hook. The census ranks that choice; it does not make it.

## Hollow Dawn 26.3 (a real mod)

364 classes naming 319 distinct `net/minecraft` classes, 5,022 class refs and 8,272 member refs.

| Top | Share of `net/minecraft` references |
|---:|---:|
| 10 | 24.9% |
| 20 | 37.0% |
| 50 | 61.4% |
| 100 | **80.8%** |
| 200 | 93.4% |

**A real mod passes the gate at 100 classes, not 200.** Mod code concentrates; library code doesn't. The
gate was written for the wrong corpus, and the corrected rule is below.

Its surface is what a content mod looks like: `Level` (61), `ServerPlayer` (53), `BlockPos` (47),
`BlockBehaviour`/`Properties` (47 each), `ServerLevel` (46), `BlockState` (44), `Block` (41),
`Identifier` (31), `Player` (31), `Vec3` (30). Two mixin targets, six injections: `shadow` 3, `inject` 2,
`redirect` 1.

Note `net/minecraft/resources/Identifier`: 26.3's name for what older versions called `ResourceLocation`.
The census is how that kind of drift gets found before it is coded around.

## Better Realism 26.3 (the whole pack)

74 jars, **27,539 classes**, 287,409 class references, 470,651 member references, **1,893 mixins** and
**3,794 injection points** — a real client pack, and the first corpus big enough to trust.

**1,873 distinct `net/minecraft` classes.** The design guessed 1,000–1,500; a client pack is above that.

| Top | Share of `net/minecraft` references |
|---:|---:|
| 100 | 62.0% |
| 200 | 73.6% |
| 300 | **79.7%** |
| 500 | 87.0% |
| 1,000 | 95.2% |

The 80% mark is at **300 classes**, not 200 — and this is the corpus that mixes libraries with mods, so
it is the honest number for "how big is the façade".

Hooks: **25 of 2,404 injection points (1.0%)**, over **1,890 points on 594 classes**. The same 1% as the
Fabric API corpus, now measured on 74 real mods. The biggest unhooked classes are `Minecraft` (99 points),
`LivingEntity` (67), `client/gui/Hud` (58), `LevelRenderer` (58), `ClientPacketListener` (53),
`MinecraftServer` (42).

The mixin mix holds: `@Inject` 1,622, `@Shadow` 957, `@Accessor` 363, `@WrapOperation` 273, `@Redirect`
143, **`@Overwrite` 26** (0.7%). HEAD/RETURN/TAIL are 55% of anchors.

**And the static share collapses.** Only **29 of the 67 jars with code** have no reflection — 43%, not the
90% the two small corpora showed. The biggest jars are exactly the dynamic ones: `fabric-language-kotlin`
(5,245 classes), `fancymenu` (`Class.forName`), `fabric-api`, `iris`, `spark`, `sodium`. The earlier
corpora were unrepresentative: a library of small modules and one content mod do not look like a client
pack. See "What this changes" below.

## The finding that matters for the Rust question

**41 of 45 Fabric API module jars and 7 of 8 Hollow Dawn jars have no reference to reflection or dynamic
class loading at all** — 48 jars. (The Fabric API bundler inherits its nested jars' answer, and
`fabric-transitive-access-wideners-v1` holds no classes. Mezzo Clef is pre-26.1 and outside the scope.)

| Jar | What makes it dynamic |
|---|---|
| `fabric-api-base` | `java.lang.reflect.Array` |
| `fabric-gametest-api-v1` | `Method`, `InvocationTargetException` |
| `fabric-item-api-v1` | `Field` |
| `fabric-model-loading-api-v1` | `Type` |
| `hollowdawn` (core) | `Field` |

The heuristic counts references to `java.lang.reflect.*`, `java.lang.invoke.*` (minus the compiler's
lambda/record/string-concat sugar), `javax.script.*`, `sun.misc.*` and `Class.forName`-shaped calls. On the
two small corpora it said something the design did not assume: a static translation would not need a
reflection subsystem for nine jars in ten.

**The full pack corrects that.** 29 of 67 jars with code (43%), and the dynamic ones are the big ones:
`fabric-language-kotlin`, `fancymenu`, `fabric-api`, `iris`, `spark`, `sodium`. The interpreter fallback
that [java-mod-compat.md](java-mod-compat.md) priced at seven person-months is back on the critical path —
not for every jar, but for the ones players install first.

It does not repeal the other costs — object model, exceptions, GC, monitors, and a build per mod set.

## What this changes

The census now keeps the full descriptor on every member key and adds a
`member_disposition` aggregate. References route to `facade` (`net/minecraft`),
`loader` (`net/fabricmc`, `net/neoforged`, `net/minecraftforge`, or `org/quiltmc`), `jdk` (standard-library owners), or
`external`. This is a routing aid for the generated ABI: overloaded methods
remain distinct in `members`, while the aggregate tells the next implementation
queue whether to extend the façade, native loader host, JDK runtime, or a
dependency shim.

1. **The gate is met, on the right corpus.** Mod code: 80.8% at 100 classes, 93.4% at 200. The whole
   pack: 79.7% at 300. Revised rule: a *mod* reaches 80% by class 200; a *pack* by class 300; *library*
   jars define the façade's class list instead, and the test there is the distinct count, not
   concentration.
2. **The façade is ~1,900 classes for a pack, not 1,500.** Fabric API's 883 is the core; a client pack
   adds the rendering, config and library surface (`GuiGraphicsExtractor` 467 references, `Screen`, the
   Kotlin runtime).
3. **`@Overwrite` is not a problem.** 1 in 1,100 injections. `@Redirect` is 5%. The hard mixin forms are
   rare; the design's per-callsite hook budget should be small.
4. **The top five mixin targets are the first façade classes:** `Entity`, `LivingEntity`, `Minecraft`,
   `MinecraftServer`, `ClientPacketListener`.
5. **The hook catalog covers 1% of the mixin surface.** 45 hooks, 7 points. Phase 4's job is a ranked
   list of 264 classes, not "extend the catalog a bit" — and the Java-owned-method rule decides how many
   of the 670 points need a hook at all.

## What it does not say

- **56 jars is not the ecosystem.** Fabric API is a library and Hollow Dawn is one mod; a pack of 300
  would add a different mix.
- **There are no 26.3 mods on Modrinth yet.** The public API's newest game version is 1.21.x, so "every
  26.3 jar" cannot be fetched. These are the 26.3 jars that exist on this machine. Rerun when the
  ecosystem has them.
- **Nested jars are read one level.** `fabric-api-0.161.0+26.3.jar` is a bundler: it reads as 44 nested
  jars and 1,994 classes against the 46 module files' 2,099 — close, which is the check that it works.
  JiJ inside JiJ is not followed.
- **"Static" is about references, not proof.** No reflection is necessary for a static translation, not
  sufficient: exceptions, monitors, GC and `synchronized` still have to be built.
- **Pre-26.1 jars read as intermediary.** Mezzo Clef names `net/minecraft/class_310` and `method_3192`,
  and is out of scope by the 26.3-only rule — but a player will bring one, so the launcher's verdict has
  to say "made for an older version", not "doesn't run".

## Next

- ~~Read nested jars~~ done: `META-INF/jars/*.jar` is read one level deep.
- ~~Join the hooks~~ done: `census --hooks native-hooks.json`, after `vanilla` was added to the catalog.
- Decide per class, from the 264, between a Java-owned method and a named hook; that list is phase 4.
- Run the same census over every 26.3 jar as the ecosystem reaches the version.
