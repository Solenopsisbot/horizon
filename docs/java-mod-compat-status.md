# Java mod compatibility checklist

Status: active work for Minecraft 26.3. The implementation is tracked in [issue #6679](https://github.com/horizon-rust/horizon/issues/6679). This page is the short progress view; [java-mod-compat.md](java-mod-compat.md) remains the design and phase plan.

Last checked: 3 November 2026, commit `f8ec79d7c`.

## Supported now

- [x] Read Fabric `fabric.mod.json`, NeoForge `neoforge.mods.toml`, and legacy Forge `mods.toml` for bounded identity, dependencies, and entrypoint discovery.
- [x] Discover and run multiple Fabric entrypoints in deterministic order.
- [x] Discover NeoForge and legacy Forge `@Mod` classes through the native probe path.
- [x] Run unsupported entrypoints through the embedded JVM runner as a fallback.
- [x] Expose bounded Fabric loader values: loaded-mod checks, metadata, versions, environment type, development marker, root paths, and seeded containers.
- [x] Expose bounded NeoForge and legacy Forge loader values: mod lists, containers, event-bus access, and mod ids.
- [x] Register and inspect bounded NeoForge and Forge deferred registrations, holders, registry objects, keys, ids, and entry collections.
- [x] Record Fabric events, NeoForge events, and legacy Forge event-bus listeners with ordering, cancellation markers, and unregister support.
- [x] Record custom payload registrations for the supported Fabric/NeoForge directions and the bounded legacy Forge `SimpleChannel.registerMessage` alias.
- [x] Provide bounded NeoForge and legacy Forge capability/`LazyOptional` markers, NBT values, identifiers, resource keys, arrays, reflection metadata, collections (including `removeIf`), maps (including `computeIfPresent` and `merge`), streams (including runtime-owned `toArray` results and `Collectors.toList/toSet`), and optionals.
- [x] Provide common Java wrapper/string/collection operations used by loader bootstrap code, including integer/long/float/double/byte/short/character conversion, `Number` accessors, checked math, seeded `System.currentTimeMillis`/`identityHashCode`, seeded `System.getProperty(key, default)` and `System.getenv` queries, deterministic `Thread.currentThread`/`getName`/`isDaemon`/`getContextClassLoader` markers, bounded callback combinators, `Arrays.asList`, `Objects.hashCode`/`requireNonNullElse`, and a restricted `String.format` subset.
- [x] Provide bounded `ClassLoader.getResource`/`getResourceAsStream` markers from explicitly seeded `StdHost::with_resource` entries, plus explicitly seeded `ServiceLoader` providers and iterators; resource and provider queries never scan host files or the process classpath.
- [x] Maintain a deterministic Fabric jar fixture covering entrypoint execution, seeded resources, multiple typed registry declarations, and unknown-kind diagnostics.
- [x] Maintain a runtime-built legacy Forge fixture covering `mods.toml`, `@Mod` discovery, `DeferredRegister`, and the bounded `SimpleChannel.registerMessage` alias; this remains probe coverage rather than live Forge runtime support.
- [x] Preserve bounded Throwable/Exception/Error messages and cause markers, including `getMessage`, `getCause`, `initCause`, and `toString` diagnostics.
- [x] Mirror the common `Locale.US` integer grouping path used by the native statistics screen without emulating arbitrary locale patterns.
- [x] Generate and test the Java façade, including bounded public Mixin `@Accessor`/`@Invoker` stubs, array-returning signatures, and common `Identifier` prefix/language-key helpers.
- [x] Maintain a class/member/Mixin census with native hook matching, config target discovery, `@At` metadata preservation, access-widener/access-transformer discovery, Mojang support-API façade classification, and explicit report-only diagnostics.

## Partial or report-only

- [ ] Full Mixin weaving. The census reads config targets, injection annotations, `@At`, plugins, refmaps, and access metadata; it does not execute plugins, apply access changes, or rewrite bytecode.
- [ ] Unmatched Mixin injection points. They are listed under `hook_points` with an empty `native_hooks` array and counted in `points_report_only`.
- [ ] Java registration markers. Explicit `META-INF/horizon-native.json` declarations now cross the typed sink; opaque registrations observed during Java execution still need a Horizon adapter before they can create live game content.
- [ ] JVM fallback coverage. Unsupported native calls, dynamic classpath failures, and missing façade members now have explicit diagnostics; the JVM path still needs broader loader façades and real-mod fixtures.
- [ ] Java classpath discovery. The census reports `ServiceLoader` and arbitrary `ClassLoader` class loading as dynamic diagnostics. Native `ServiceLoader.load`/`loadInstalled` now support only explicitly seeded providers and bounded iteration; arbitrary provider discovery, `ClassLoader.loadClass`, and unseeded resources remain report-only.

## Remaining work

- [ ] Build an ABI corpus from representative Fabric, Forge, and NeoForge jars and rank missing calls by execution frequency.
- [ ] Define the Mixin translation pipeline: resolve config targets, apply priority/order and cancellation, map supported injectors to Horizon hooks, and keep a JVM fallback for unsupported transforms.
- [ ] Add real-mod integration fixtures for registries, events, networking, capabilities, entities, menus, resource loading, and client/server separation.
- [ ] Connect translated callbacks to the generated typed native API and Horizon's server-thread and dedicated-server boundaries.
- [ ] Complete live content registration and registry synchronization for code-declared blocks, items, entities, recipes, data maps, attachments, and custom ingredients.
- [ ] Expand client compatibility for HUDs, key mappings, tooltips, models, renderers, particles, screens, and shader-facing code.
- [ ] Add dedicated-server loading, performance measurements, crash isolation, permissions, and a clear unsupported-call report in the launcher.
- [ ] Define the distribution policy for Java jars, native translations, signatures, and per-mod compatibility levels.

## Validation snapshot

- `horizon-native-java`: 93 unit tests passing, plus runtime-built Fabric, legacy Forge, and NeoForge fixture tests, including bounded `Number` accessors, callback combinators, restricted string formatting, deterministic Thread/class-loader markers, Throwable metadata, and explicit ServiceLoader provider boundaries.
- `horizon-port-pack`: 156 unit tests passing, including Mixin `@At` metadata, multi-method hook matching, and descriptor normalization coverage.
- `horizon-jvm`: 8 unit tests passing, including explicit fallback labels.
- Workspace check, targeted clippy, formatting, and `tools/native_java_smoke.py` passing.

Every compatibility commit should update this checklist when a surface moves between these sections and should include the focused test or probe that justifies the change.
