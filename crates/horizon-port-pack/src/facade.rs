//! A Java façade skeleton for the `net.minecraft` classes a mod jar names,
//! generated from the real 26.3 jar's shapes and compiled with `javac`
//! (`docs/java-mod-compat.md`, phase 1's first slice).
//!
//! The stubs are shapes, not behaviour: a method throws
//! `UnsupportedOperationException` naming itself, so a mod run against them
//! stops at the first thing the game must actually do. That is the probe: the
//! class loads, the entrypoint runs, and the failure names the next method to
//! implement.
//!
//! Nested classes (`HolderSet$Named`) are emitted nested, because `javac`
//! resolves `A.B` and not the flat `A$B`; references are written the Java way.
use crate::classes::{self, Class, Declared, MemberKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub classes: usize,
    pub methods: usize,
    pub fields: usize,
    /// Named by the mod but not in the game jar (a library, or a version
    /// mismatch), and nested classes deeper than one level.
    pub missing: Vec<String>,
    pub sources: PathBuf,
    pub jar: PathBuf,
}

pub fn generate(
    mod_jar: &Path,
    game_jar: &Path,
    library_jars: &[PathBuf],
    out: &Path,
    javac: &Path,
    jar: &Path,
) -> Result<Report, String> {
    generate_many(
        &[mod_jar.to_path_buf()],
        game_jar,
        library_jars,
        out,
        javac,
        jar,
    )
}

/// Generate one façade from a mod and the loader/API jars it depends on.
/// Dependency jars contribute references and member uses, while their own
/// classes remain real entries on `javac`'s classpath.
pub fn generate_many(
    mod_jars: &[PathBuf],
    game_jar: &Path,
    library_jars: &[PathBuf],
    out: &Path,
    javac: &Path,
    jar: &Path,
) -> Result<Report, String> {
    let game: BTreeMap<String, Class> = classes::read_jar(game_jar)?
        .into_iter()
        .map(|c| (c.name.clone(), c))
        .collect();
    // Libraries are real, not stubbed: they go on `javac`'s and the game's
    // classpath. Only the game's own classes need a façade.
    let mut lib_names: BTreeSet<String> = BTreeSet::new();
    for path in library_jars {
        if let Ok(classes) = classes::read_jar(path) {
            lib_names.extend(classes.into_iter().map(|c| c.name));
        }
    }
    let mut target = Vec::new();
    for mod_jar in mod_jars {
        target.extend(classes::read_jar(mod_jar)?);
    }
    let uses_neoforge = target
        .iter()
        .any(|class| class.refs.iter().any(|r| r.starts_with("net/neoforged/")));
    let own: BTreeSet<&str> = target.iter().map(|c| c.name.as_str()).collect();
    // Shapes from dependency jars are real classes on javac/the probe
    // classpaths. Keep their names available when deciding whether a game
    // stub may extend or implement one (for example Fabric's entity builder
    // contract); they are still excluded from the generated façade by `own`.
    let mut available = lib_names.clone();
    available.extend(own.iter().map(|name| (*name).to_owned()));
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for class in &target {
        for r in &class.refs {
            if (r.starts_with("net/minecraft/") || r.starts_with("com/mojang/"))
                && !own.contains(r.as_str())
                && !lib_names.contains(r)
            {
                wanted.insert(r.clone());
            }
        }
    }
    // A JVM descriptor can name a class through its UTF-8 descriptor alone;
    // javac does not have to emit a CONSTANT_Class entry for every parameter
    // type. Close the initial set over all member descriptors as well, or an
    // API callback such as `MobConversion(..., ConversionParams)` can compile
    // but fail with NoClassDefFoundError at runtime.
    for class in &target {
        for member in &class.members {
            for ty in descriptor_types(&member.descriptor) {
                if (ty.starts_with("net/minecraft/") || ty.starts_with("com/mojang/"))
                    && !own.contains(ty.as_str())
                    && !lib_names.contains(&ty)
                {
                    wanted.insert(ty);
                }
            }
        }
    }
    let mut used: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
    for class in &target {
        for m in &class.members {
            if wanted.contains(&m.owner) {
                used.entry(m.owner.clone())
                    .or_default()
                    .insert((m.name.clone(), m.descriptor.clone()));
            }
        }
    }
    // Accessor and invoker mixins are Java interfaces whose methods are
    // normally materialized by Mixin on the target class. The façade has no
    // bytecode weaver, so make the target implement those interfaces itself.
    // This keeps the common `(TargetAccessor) target` ABI alive while the
    // generated method bodies remain deliberately conservative defaults.
    let mut mixin_impls: BTreeMap<String, Vec<MixinImpl>> = BTreeMap::new();
    for mixin in target.iter().filter(|class| class.mixin) {
        // A package-private accessor cannot appear in a public façade class's
        // implements list. Keep the first generic slice to primitive/void
        // methods; object accessors frequently carry a Signature attribute
        // that the lightweight class reader intentionally does not decode,
        // so emitting a raw return type would fail javac's override checks.
        if mixin.access & 0x0201 != 0x0201 {
            continue;
        }
        let methods = mixin
            .declared
            .iter()
            .filter(|member| {
                member.kind == MemberKind::Method
                    && member.name != "<init>"
                    && safe_mixin_method(member)
                    && mixin
                        .method_annotations
                        .get(&member.name)
                        .is_some_and(|annotations| {
                            annotations.iter().any(|annotation| {
                                matches!(
                                    annotation_simple_name(&annotation.ty),
                                    "Accessor" | "Invoker"
                                )
                            })
                        })
            })
            .cloned()
            .collect::<Vec<_>>();
        if methods.is_empty()
            || methods.len()
                != mixin
                    .declared
                    .iter()
                    .filter(|member| {
                        member.kind == MemberKind::Method
                            && member.name != "<init>"
                            && mixin.method_annotations.get(&member.name).is_some_and(
                                |annotations| {
                                    annotations.iter().any(|annotation| {
                                        matches!(
                                            annotation_simple_name(&annotation.ty),
                                            "Accessor" | "Invoker"
                                        )
                                    })
                                },
                            )
                    })
                    .count()
        {
            continue;
        }
        for target_name in &mixin.mixin_targets {
            mixin_impls
                .entry(target_name.clone())
                .or_default()
                .push(MixinImpl {
                    interface: mixin.name.clone(),
                    methods: methods.clone(),
                });
        }
    }
    // A stub cannot compile without the types it extends and names: close the
    // set over superclasses, interfaces and the descriptors of the members
    // that will be emitted.
    // A nested class needs its outer emitted around it.
    for name in wanted.clone() {
        if let Some((root, _)) = name.split_once('$') {
            wanted.insert(root.to_owned());
        }
    }
    let mut queue: Vec<String> = wanted.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        let Some(class) = game.get(&name) else {
            continue;
        };
        let mut add = |ty: &str| {
            // Anything but the JDK: a missing library becomes an empty type
            // so the stubs still compile; a real one is on the classpath.
            let jdk = ["java/", "javax/", "jdk/", "sun/", "com/sun/"]
                .iter()
                .any(|p| ty.starts_with(p));
            if !jdk && !lib_names.contains(ty) && !own.contains(ty) && wanted.insert(ty.to_owned())
            {
                queue.push(ty.to_owned());
                // The outer class has to be rendered to hold it.
                if let Some((root, _)) = ty.split_once('$')
                    && wanted.insert(root.to_owned())
                {
                    queue.push(root.to_owned());
                }
            }
        };
        add(&class.superclass);
        for interface in &class.interfaces {
            add(interface);
        }
        let is_interface = class.access & 0x0200 != 0;
        let is_enum = class.access & 0x4000 != 0;
        let public = |m: &Declared| m.is_public() || m.is_protected();
        for member in &class.declared {
            // Must match what `render` emits, or a stub names a type it does
            // not have.
            let emitted = match member.kind {
                MemberKind::Field => !is_enum && public(member) && !member.is_enum_constant(),
                MemberKind::Method => {
                    (member.name == "<init>" && public(member))
                        || (public(member)
                            && (is_interface
                                || (class.access & 0x4000 == 0
                                    && class.superclass == "java/lang/Record")
                                || used.get(&name).is_some_and(|u| {
                                    u.contains(&(member.name.clone(), member.descriptor.clone()))
                                })))
                }
                _ => false,
            };
            if !emitted {
                continue;
            }
            for ty in descriptor_types(&member.descriptor) {
                add(&ty);
            }
        }
        // Snapshot-drift methods may be present only in `used`, so their
        // descriptor types are not covered by the game class's declaration.
        // Close over those synthetic signatures as well; otherwise javac can
        // fail on a dependency type before the JVM probe reaches the method.
        if let Some(methods) = used.get(&name) {
            for (_, descriptor) in methods {
                for ty in descriptor_types(descriptor) {
                    add(&ty);
                }
            }
        }
    }
    // One compilation unit per outer class; nested classes are emitted inside
    // it, recursively (`Holder$Reference$Type` is two levels).
    let mut top: BTreeSet<String> = BTreeSet::new();
    let mut nested: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in &wanted {
        match name.rsplit_once('$') {
            None => {
                top.insert(name.clone());
            }
            Some((parent, simple)) => {
                nested
                    .entry(parent.to_owned())
                    .or_default()
                    .insert(simple.to_owned());
            }
        }
    }
    let src = out.join("src");
    let classes_dir = out.join("classes");
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&classes_dir);
    fs::create_dir_all(&src).map_err(|e| e.to_string())?;
    fs::create_dir_all(&classes_dir).map_err(|e| e.to_string())?;
    let mut report = Report {
        sources: src.clone(),
        jar: out.join("facade.jar"),
        ..Report::default()
    };
    let mut files = Vec::new();
    for root in &top {
        let source = tree(
            root,
            true,
            &game,
            &available,
            &used,
            &nested,
            &mixin_impls,
            &mut report,
        );
        let (dir, simple) = root.rsplit_once('/').unwrap_or(("", root.as_str()));
        let file = src.join(dir).join(format!("{simple}.java"));
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&file, source).map_err(|e| format!("{}: {e}", file.display()))?;
        files.push(file);
    }
    // A few Fabric API accessor interfaces are deliberately only implemented
    // by Mixin at runtime. Keep the first bootstrap slice usable by emitting
    // the equivalent constant accessor into the façade jar, which is first on
    // the JVM classpath.
    let shim = src
        .join("net/fabricmc/fabric/mixin/attachment/ClientboundCustomPayloadPacketAccessor.java");
    if let Some(parent) = shim.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(
        &shim,
        "package net.fabricmc.fabric.mixin.attachment;\npublic interface ClientboundCustomPayloadPacketAccessor {\n    static int getMaxPayloadSize() { return 1048576; }\n}\n",
    )
    .map_err(|e| format!("{}: {e}", shim.display()))?;
    files.push(shim);
    // Fabric's VarInt accessor is another static constant used by data
    // attachment bootstrap.  It has no transformed instance target, so the
    // vanilla bound is safe to expose directly from the façade.
    let varint_shim = src.join("net/fabricmc/fabric/mixin/attachment/VarIntAccessor.java");
    if let Some(parent) = varint_shim.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(
        &varint_shim,
        "package net.fabricmc.fabric.mixin.attachment;\npublic interface VarIntAccessor {\n    static int getMaxByteSize() { return 5; }\n}\n",
    )
    .map_err(|e| format!("{}: {e}", varint_shim.display()))?;
    files.push(varint_shim);
    // This accessor is client-only. A server-shaped game jar does not carry
    // ModelLayerLocation, so emitting it there would make an otherwise valid
    // façade fail javac before any mod code is loaded.
    let model_layers_accessor = "net/fabricmc/fabric/mixin/client/rendering/ModelLayersAccessor";
    if available.contains(model_layers_accessor)
        && game.contains_key("net/minecraft/client/model/geom/ModelLayerLocation")
    {
        let model_layers_shim =
            src.join("net/fabricmc/fabric/mixin/client/rendering/ModelLayersAccessor.java");
        if let Some(parent) = model_layers_shim.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(
            &model_layers_shim,
            "package net.fabricmc.fabric.mixin.client.rendering;\npublic interface ModelLayersAccessor {\n    static java.util.Set<net.minecraft.client.model.geom.ModelLayerLocation> getLayers() { return new java.util.HashSet<>(); }\n}\n",
        )
        .map_err(|e| format!("{}: {e}", model_layers_shim.display()))?;
        files.push(model_layers_shim);
    }
    if uses_neoforge {
        let shims = [
            (
                "net/neoforged/fml/common/Mod.java",
                concat!(
                    "package net.neoforged.fml.common;\n",
                    "import java.lang.annotation.ElementType;\n",
                    "import java.lang.annotation.Retention;\n",
                    "import java.lang.annotation.RetentionPolicy;\n",
                    "import java.lang.annotation.Target;\n",
                    "@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.TYPE)\n",
                    "public @interface Mod { String value(); }\n",
                ),
            ),
            (
                "net/neoforged/bus/api/Event.java",
                concat!(
                    "package net.neoforged.bus.api;\n",
                    "public class Event {}\n"
                ),
            ),
            (
                "net/neoforged/bus/api/IEventBus.java",
                concat!(
                    "package net.neoforged.bus.api;\n",
                    "import java.util.function.Consumer;\n",
                    "public interface IEventBus {\n",
                    "    default IEventBus addListener(Consumer<?> listener) { return this; }\n",
                    "    default IEventBus addListener(EventPriority priority, Consumer<?> listener) { return this; }\n",
                    "    default IEventBus addListener(EventPriority priority, boolean receiveCancelled, Consumer<?> listener) { return this; }\n",
                    "    default <T extends Event> IEventBus addListener(Class<T> type, Consumer<T> listener) { return this; }\n",
                    "    default <T extends Event> IEventBus addListener(EventPriority priority, Class<T> type, Consumer<T> listener) { return this; }\n",
                    "    default <T extends Event> IEventBus addListener(EventPriority priority, boolean receiveCancelled, Class<T> type, Consumer<T> listener) { return this; }\n",
                    "    default IEventBus register(Object object) { return this; }\n",
                    "    default boolean post(Event event) { return false; }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/bus/api/EventPriority.java",
                concat!(
                    "package net.neoforged.bus.api;\n",
                    "public enum EventPriority { HIGHEST, HIGH, NORMAL, LOW, LOWEST }\n",
                ),
            ),
            (
                "net/neoforged/neoforge/capabilities/CapabilityToken.java",
                concat!(
                    "package net.neoforged.neoforge.capabilities;\n",
                    "public class CapabilityToken<T> { public CapabilityToken() {} }\n",
                ),
            ),
            (
                "net/neoforged/neoforge/capabilities/Capability.java",
                concat!(
                    "package net.neoforged.neoforge.capabilities;\n",
                    "import net.neoforged.neoforge.common.util.LazyOptional;\n",
                    "public class Capability<T> {\n",
                    "    public boolean isRegistered() { return true; }\n",
                    "    public <R> LazyOptional<R> orEmpty(R value, Object requested) { return requested == this && value != null ? LazyOptional.of(() -> value) : LazyOptional.empty(); }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/capabilities/CapabilityManager.java",
                concat!(
                    "package net.neoforged.neoforge.capabilities;\n",
                    "public final class CapabilityManager {\n",
                    "    private CapabilityManager() {}\n",
                    "    public static <T> Capability<T> get(CapabilityToken<T> token) { return new Capability<>(); }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/common/util/LazyOptional.java",
                concat!(
                    "package net.neoforged.neoforge.common.util;\n",
                    "import java.util.Optional;\n",
                    "import java.util.function.Consumer;\n",
                    "import java.util.function.Function;\n",
                    "import java.util.function.Supplier;\n",
                    "public class LazyOptional<T> {\n",
                    "    private Supplier<? extends T> horizonSupplier;\n",
                    "    private T horizonValue;\n",
                    "    private boolean horizonValid = true;\n",
                    "    private LazyOptional(Supplier<? extends T> supplier) { this.horizonSupplier = supplier; }\n",
                    "    public static <T> LazyOptional<T> empty() { return new LazyOptional<>(() -> null); }\n",
                    "    public static <T> LazyOptional<T> of(Supplier<? extends T> supplier) { return new LazyOptional<>(supplier); }\n",
                    "    public static <T> LazyOptional<T> ofNullable(T value) { return new LazyOptional<>(() -> value); }\n",
                    "    private T horizonGet() { if (horizonValue == null && horizonSupplier != null) horizonValue = horizonSupplier.get(); return horizonValue; }\n",
                    "    public boolean isPresent() { return horizonValid && horizonGet() != null; }\n",
                    "    public T resolve() { return isPresent() ? horizonGet() : null; }\n",
                    "    public T orElse(T fallback) { T value = resolve(); return value == null ? fallback : value; }\n",
                    "    public T orElseGet(Supplier<? extends T> fallback) { T value = resolve(); return value == null ? fallback.get() : value; }\n",
                    "    public void ifPresent(Consumer<? super T> consumer) { T value = resolve(); if (value != null) consumer.accept(value); }\n",
                    "    public <R> LazyOptional<R> map(Function<? super T, ? extends R> mapper) { T value = resolve(); return value == null ? empty() : of(() -> mapper.apply(value)); }\n",
                    "    @SuppressWarnings(\"unchecked\") public <R> LazyOptional<R> cast() { return (LazyOptional<R>) this; }\n",
                    "    public void invalidate() { horizonValid = false; horizonValue = null; horizonSupplier = () -> null; }\n",
                    "    public LazyOptional<T> addListener(Consumer<LazyOptional<T>> listener) { if (!horizonValid) listener.accept(this); return this; }\n",
                    "    public Optional<T> resolveOptional() { return Optional.ofNullable(resolve()); }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/items/IItemHandler.java",
                concat!(
                    "package net.neoforged.neoforge.items;\n",
                    "import net.minecraft.world.item.ItemStack;\n",
                    "import net.minecraft.world.item.Item;\n",
                    "public interface IItemHandler {\n",
                    // Keep a small, per-handler value store so capability
                    // bootstrap code can inspect and round-trip one bounded
                    // slot without pretending to be NeoForge's live block
                    // capability implementation. Reflection keeps this shim
                    // independent of which ItemStack members the census had
                    // to materialize in the generated game façade.
                    "    class HorizonSlot { ItemStack stack; int count; }\n",
                    "    java.util.Map<IItemHandler, HorizonSlot> HORIZON_SLOTS = java.util.Collections.synchronizedMap(new java.util.WeakHashMap<>());\n",
                    "    static HorizonSlot horizonSlot(IItemHandler self, int slot) { return slot == 0 ? HORIZON_SLOTS.computeIfAbsent(self, key -> new HorizonSlot()) : null; }\n",
                    "    static int horizonCount(ItemStack stack) { if (stack == null) return 0; try { return ((Integer) stack.getClass().getMethod(\"getCount\").invoke(stack)).intValue(); } catch (Exception ignored) { return 0; } }\n",
                    "    static Item horizonItem(ItemStack stack) { if (stack == null) return null; try { return (Item) stack.getClass().getMethod(\"getItem\").invoke(stack); } catch (Exception ignored) { return null; } }\n",
                    "    static ItemStack horizonCopy(ItemStack stack, int count) { if (stack == null || count <= 0) return new ItemStack(); try { return new ItemStack(horizonItem(stack), count); } catch (Exception ignored) { return new ItemStack(); } }\n",
                    "    default int getSlots() { return 1; }\n",
                    "    default ItemStack getStackInSlot(int slot) { HorizonSlot value = horizonSlot(this, slot); return value == null || value.count <= 0 ? new ItemStack() : horizonCopy(value.stack, value.count); }\n",
                    "    default ItemStack insertItem(int slot, ItemStack stack, boolean simulate) { HorizonSlot value = horizonSlot(this, slot); int incoming = horizonCount(stack); if (value == null || incoming <= 0) return incoming <= 0 ? new ItemStack() : stack; Item existing = horizonItem(value.stack); Item incomingItem = horizonItem(stack); if (value.count > 0 && existing != incomingItem) return horizonCopy(stack, incoming); int amount = Math.min(incoming, Math.max(0, 64 - value.count)); if (!simulate && amount > 0) { if (value.count == 0) value.stack = stack; value.count += amount; } return amount == incoming ? new ItemStack() : horizonCopy(stack, incoming - amount); }\n",
                    "    default ItemStack extractItem(int slot, int amount, boolean simulate) { HorizonSlot value = horizonSlot(this, slot); if (value == null || amount <= 0 || value.count <= 0) return new ItemStack(); int moved = Math.min(amount, value.count); ItemStack result = horizonCopy(value.stack, moved); if (!simulate) { value.count -= moved; if (value.count == 0) value.stack = null; } return result; }\n",
                    "    default int getSlotLimit(int slot) { return slot == 0 ? 64 : 0; }\n",
                    "    default boolean isItemValid(int slot, ItemStack stack) { return slot == 0 && stack != null; }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/items/IItemHandlerModifiable.java",
                concat!(
                    "package net.neoforged.neoforge.items;\n",
                    "import net.minecraft.world.item.ItemStack;\n",
                    "public interface IItemHandlerModifiable extends IItemHandler {\n",
                    "    default void setStackInSlot(int slot, ItemStack stack) { IItemHandler.HorizonSlot value = IItemHandler.horizonSlot(this, slot); if (value != null) { value.stack = stack; value.count = IItemHandler.horizonCount(stack); } }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext.java",
                concat!(
                    "package net.neoforged.fml.javafmlmod;\n",
                    "import net.neoforged.bus.api.IEventBus;\n",
                    "public class FMLJavaModLoadingContext {\n",
                    "    public static FMLJavaModLoadingContext get() { return new FMLJavaModLoadingContext(); }\n",
                    "    public IEventBus getModEventBus() { return new IEventBus() {}; }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/registries/RegisterEvent.java",
                concat!(
                    "package net.neoforged.neoforge.registries;\n",
                    "import java.util.function.Consumer;\n",
                    "import java.util.function.Supplier;\n",
                    "import net.minecraft.core.Registry;\n",
                    "import net.minecraft.resources.Identifier;\n",
                    "import net.minecraft.resources.ResourceKey;\n",
                    "import net.neoforged.bus.api.Event;\n",
                    "public class RegisterEvent extends Event {\n",
                    "    private final ResourceKey horizonRegistryKey = new ResourceKey();\n",
                    "    private final Registry horizonRegistry = new Registry();\n",
                    "    public void register(ResourceKey key, Identifier id, Supplier value) { if (value != null) horizonRegistry.register(key, id, value.get()); }\n",
                    "    public void register(ResourceKey key, Consumer callback) { if (callback != null) callback.accept(new RegisterHelper() { public void register(Identifier id, Object value) { horizonRegistry.register(key, id, value); } }); }\n",
                    "    public ResourceKey getRegistryKey() { return horizonRegistryKey; }\n",
                    "    public Registry getRegistry() { return horizonRegistry; }\n",
                    "    public Registry getRegistry(ResourceKey key) { return horizonRegistry; }\n",
                    "    public interface RegisterHelper { default void register(ResourceKey key, Object value) {} void register(Identifier id, Object value); }\n",
                    "}\n",
                ),
            ),
            (
                "net/minecraft/core/Registry.java",
                concat!(
                    "package net.minecraft.core;\n",
                    "import java.util.*;\n",
                    "import net.minecraft.resources.*;\n",
                    "public class Registry<T> {\n",
                    "    protected final Map<Identifier,Object> horizonEntries = new LinkedHashMap<>();\n",
                    "    protected final Map<Identifier,ResourceKey> horizonKeys = new LinkedHashMap<>();\n",
                    "    public <V> V register(ResourceKey<V> key, Identifier id, V value) { horizonEntries.put(id, value); horizonKeys.put(id, key); return value; }\n",
                    "    public boolean containsKey(Identifier id) { return horizonEntries.containsKey(id); }\n",
                    "    @SuppressWarnings(\"unchecked\") public T get(Identifier id) { return (T) horizonEntries.get(id); }\n",
                    "    public Optional<T> getOptional(Identifier id) { return Optional.ofNullable(get(id)); }\n",
                    "    public Identifier getKey(Object value) { for (Map.Entry<Identifier,Object> e : horizonEntries.entrySet()) if (Objects.equals(e.getValue(), value)) return e.getKey(); return null; }\n",
                    "    public Optional<ResourceKey> getResourceKey(Object value) { Identifier id = getKey(value); return id == null ? Optional.empty() : Optional.ofNullable(horizonKeys.get(id)); }\n",
                    "}\n",
                ),
            ),
            (
                "net/minecraft/core/DefaultedRegistry.java",
                concat!(
                    "package net.minecraft.core;\n",
                    "import net.minecraft.resources.Identifier;\n",
                    "public class DefaultedRegistry<T> extends Registry<T> {\n",
                    "    public Identifier getKey(Object value) { return super.getKey(value); }\n",
                    "}\n",
                ),
            ),
            (
                "net/minecraft/resources/Identifier.java",
                concat!(
                    "package net.minecraft.resources;\n",
                    "public class Identifier {\n",
                    "    public Identifier() {}\n",
                    "    public Identifier(String namespace, String path) {}\n",
                    "}\n",
                ),
            ),
            (
                "net/minecraft/resources/ResourceKey.java",
                concat!(
                    "package net.minecraft.resources;\n",
                    "public class ResourceKey<T> { public ResourceKey() {} }\n",
                ),
            ),
            (
                "net/neoforged/neoforge/registries/DeferredHolder.java",
                concat!(
                    "package net.neoforged.neoforge.registries;\n",
                    "import java.util.function.Supplier;\n",
                    "import net.minecraft.resources.Identifier;\n",
                    "import net.minecraft.resources.ResourceKey;\n",
                    "public class DeferredHolder<T> {\n",
                    "    private T horizonValue;\n",
                    "    private Supplier<? extends T> horizonSupplier;\n",
                    "    private Identifier horizonId = new Identifier();\n",
                    "    private ResourceKey horizonKey = new ResourceKey();\n",
                    "    public DeferredHolder() {}\n",
                    "    DeferredHolder(Identifier id, ResourceKey key, Supplier<? extends T> supplier) { this.horizonId = id; this.horizonKey = key; this.horizonSupplier = supplier; }\n",
                    "    void horizonBind(Identifier id, ResourceKey key, Supplier<? extends T> supplier) { this.horizonId = id; this.horizonKey = key; this.horizonSupplier = supplier; }\n",
                    "    public T get() { if (horizonValue == null && horizonSupplier != null) horizonValue = horizonSupplier.get(); return horizonValue; }\n",
                    "    public T value() { return get(); }\n",
                    "    public boolean isPresent() { return get() != null; }\n",
                    "    public java.util.Optional<T> getOptional() { return java.util.Optional.ofNullable(get()); }\n",
                    "    public ResourceKey getKey() { return horizonKey; }\n",
                    "    public Identifier getId() { return horizonId; }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/registries/DeferredRegister.java",
                concat!(
                    "package net.neoforged.neoforge.registries;\n",
                    "import java.util.function.Supplier;\n",
                    "import net.minecraft.resources.Identifier;\n",
                    "import net.minecraft.resources.ResourceKey;\n",
                    "import net.neoforged.bus.api.IEventBus;\n",
                    "public class DeferredRegister<T> {\n",
                    "    private String horizonNamespace = \"minecraft\";\n",
                    "    private Object horizonRegistry;\n",
                    "    public static <T> DeferredRegister<T> create(Object registry, String id) { DeferredRegister<T> value = new DeferredRegister<>(); value.horizonRegistry = registry; value.horizonNamespace = id; return value; }\n",
                    "    public <V extends T> DeferredHolder<V> register(String id, Supplier<V> value) { DeferredHolder<V> holder = new DeferredHolder<>(); holder.horizonBind(new Identifier(horizonNamespace, id), new ResourceKey(), value); return holder; }\n",
                    "    public void register(IEventBus bus) {}\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/network/registration/PayloadRegistrar.java",
                concat!(
                    "package net.neoforged.neoforge.network.registration;\n",
                    "public class PayloadRegistrar {\n",
                    "    private int horizonRegistrationCount;\n",
                    "    public int horizonRegistrationCount() { return horizonRegistrationCount; }\n",
                    "    public PayloadRegistrar playToServer(Object type, Object codec, Object handler) { horizonRegistrationCount++; return this; }\n",
                    "    public PayloadRegistrar playToClient(Object type, Object codec, Object handler) { horizonRegistrationCount++; return this; }\n",
                    "    public PayloadRegistrar playBidirectional(Object type, Object codec, Object handler) { horizonRegistrationCount++; return this; }\n",
                    "}\n",
                ),
            ),
            (
                "net/neoforged/neoforge/network/event/RegisterPayloadHandlersEvent.java",
                concat!(
                    "package net.neoforged.neoforge.network.event;\n",
                    "import net.neoforged.bus.api.Event;\n",
                    "import net.neoforged.neoforge.network.registration.PayloadRegistrar;\n",
                    "public class RegisterPayloadHandlersEvent extends Event {\n",
                    "    private final PayloadRegistrar horizonRegistrar = new PayloadRegistrar();\n",
                    "    public PayloadRegistrar registrar(String version) { return horizonRegistrar; }\n",
                    "}\n",
                ),
            ),
        ];
        for (relative, source) in shims {
            let game_value = match relative {
                "net/minecraft/core/Registry.java" => Some("net/minecraft/core/Registry"),
                "net/minecraft/core/DefaultedRegistry.java" => {
                    Some("net/minecraft/core/DefaultedRegistry")
                }
                "net/minecraft/resources/Identifier.java" => {
                    Some("net/minecraft/resources/Identifier")
                }
                "net/minecraft/resources/ResourceKey.java" => {
                    Some("net/minecraft/resources/ResourceKey")
                }
                _ => None,
            };
            if game_value.is_some_and(|name| wanted.contains(name) || lib_names.contains(name)) {
                continue;
            }
            let file = src.join(relative);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::write(&file, source).map_err(|e| format!("{}: {e}", file.display()))?;
            files.push(file);
        }
    }
    if files.is_empty() {
        return Err("nothing to generate".to_owned());
    }
    let list = out.join("sources.txt");
    let mut args = String::new();
    for file in &files {
        let _ = writeln!(args, "\"{}\"", file.display());
    }
    fs::write(&list, args).map_err(|e| e.to_string())?;
    // `classes::read_jar` sees Fabric's nested API jars for shape discovery,
    // but javac cannot read `META-INF/jars/` itself. Extract those dependencies
    // into a bounded scratch classpath for this compilation, including nested
    // jars inside a bundled jar.
    let nested = extract_nested_jars(
        library_jars.iter().chain(mod_jars.iter()),
        &out.join(".nested-classpath"),
    )?;
    let mut compile_jars = library_jars.to_vec();
    compile_jars.extend(nested.jars.iter().cloned());
    for path in mod_jars {
        if !compile_jars.iter().any(|existing| existing == path) {
            compile_jars.push(path.clone());
        }
    }
    let mut command = Command::new(javac);
    command
        .arg("-d")
        .arg(&classes_dir)
        .arg("-proc:none")
        .arg("-nowarn");
    if !compile_jars.is_empty() {
        let path = std::env::join_paths(&compile_jars).map_err(|e| e.to_string())?;
        command.arg("-classpath").arg(path);
    }
    let status = command
        .arg(format!("@{}", list.display()))
        .status()
        .map_err(|e| format!("{}: {e}", javac.display()))?;
    if !status.success() {
        return Err(format!("javac failed: {status}"));
    }
    let status = Command::new(jar)
        .arg("--create")
        .arg("--file")
        .arg(&report.jar)
        .arg("-C")
        .arg(&classes_dir)
        .arg(".")
        .status()
        .map_err(|e| format!("{}: {e}", jar.display()))?;
    if !status.success() {
        return Err(format!("jar failed: {status}"));
    }
    Ok(report)
}

struct NestedClasspath {
    root: PathBuf,
    jars: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct MixinImpl {
    interface: String,
    methods: Vec<Declared>,
}

impl Drop for NestedClasspath {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Extract direct `META-INF/jars/*.jar` entries recursively for javac. The
/// runtime runner has the same bound; keeping it here prevents a malformed
/// mod from turning façade generation into an unbounded archive walk.
fn extract_nested_jars<'a>(
    inputs: impl IntoIterator<Item = &'a PathBuf>,
    root: &Path,
) -> Result<NestedClasspath, String> {
    const MAX_NESTED_JARS: usize = 4096;
    const MAX_NESTED_SIZE: u64 = 32 * 1024 * 1024;
    let _ = fs::remove_dir_all(root);
    fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut queue: Vec<PathBuf> = inputs.into_iter().cloned().collect();
    let mut jars = Vec::new();
    let mut serial = 0usize;
    let mut index = 0usize;
    while index < queue.len() {
        let input = queue[index].clone();
        index += 1;
        let Ok(file) = fs::File::open(&input) else {
            continue;
        };
        let Ok(mut archive) = zip::ZipArchive::new(file) else {
            continue;
        };
        for entry_index in 0..archive.len() {
            let mut entry = archive
                .by_index(entry_index)
                .map_err(|e| format!("{}: reading zip entry: {e}", input.display()))?;
            let Some(name) = nested_jar_name(entry.name()) else {
                continue;
            };
            if entry.size() > MAX_NESTED_SIZE {
                return Err(format!(
                    "{}: nested jar {name} exceeds {MAX_NESTED_SIZE} bytes",
                    input.display()
                ));
            }
            if serial >= MAX_NESTED_JARS {
                return Err(format!(
                    "nested jar limit exceeded ({MAX_NESTED_JARS}); refusing an unbounded classpath"
                ));
            }
            let output = root.join(format!("{serial}-{name}"));
            serial += 1;
            let mut out =
                fs::File::create(&output).map_err(|e| format!("{}: {e}", output.display()))?;
            io::copy(&mut entry, &mut out).map_err(|e| format!("{}: {e}", output.display()))?;
            jars.push(output.clone());
            queue.push(output);
        }
    }
    Ok(NestedClasspath {
        root: root.to_path_buf(),
        jars,
    })
}

fn nested_jar_name(entry: &str) -> Option<&str> {
    let name = entry.strip_prefix("META-INF/jars/")?;
    if name.is_empty() || name.contains('/') || !name.ends_with(".jar") {
        return None;
    }
    Some(name)
}

fn tree(
    name: &str,
    top: bool,
    game: &BTreeMap<String, Class>,
    available: &BTreeSet<String>,
    used: &BTreeMap<String, BTreeSet<(String, String)>>,
    nested: &BTreeMap<String, BTreeSet<String>>,
    mixin_impls: &BTreeMap<String, Vec<MixinImpl>>,
    report: &mut Report,
) -> String {
    let inner: Vec<String> = nested
        .get(name)
        .map(|children| {
            children
                .iter()
                .map(|child| {
                    tree(
                        &format!("{name}${child}"),
                        false,
                        game,
                        available,
                        used,
                        nested,
                        mixin_impls,
                        report,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    match game.get(name) {
        Some(class) => render(
            class,
            used.get(name),
            game,
            available,
            mixin_impls.get(name),
            report,
            !top,
            &inner,
        ),
        None => {
            report.missing.push(name.to_owned());
            report.classes += 1;
            if top {
                minimal(name)
            } else {
                let simple = name.rsplit('$').next().unwrap_or(name);
                format!("public static class {simple} {{ public {simple}() {{}} }}")
            }
        }
    }
}

fn render(
    class: &Class,
    used: Option<&BTreeSet<(String, String)>>,
    game: &BTreeMap<String, Class>,
    available: &BTreeSet<String>,
    mixin_impls: Option<&Vec<MixinImpl>>,
    report: &mut Report,
    nested: bool,
    inner: &[String],
) -> String {
    let (package, simple) = match class.name.rsplit_once('/') {
        Some((package, simple)) => (Some(package), simple),
        None => (None, class.name.as_str()),
    };
    // A nested class's source name is its last `$` segment.
    let simple = if nested {
        simple.rsplit('$').next().unwrap_or(simple)
    } else {
        simple
    };
    let is_interface = class.access & 0x0200 != 0;
    let is_annotation = class.access & 0x2000 != 0;
    let is_enum = class.access & 0x4000 != 0;
    let is_record = class.superclass == "java/lang/Record";
    let modifier = if nested { "public static" } else { "public" };
    let external_super = !is_record
        && !class.superclass.is_empty()
        && !game.contains_key(&class.superclass)
        && (available.contains(&class.superclass) || class.superclass.starts_with("java/"))
        && (simple.ends_with("Exception") || simple.ends_with("Error"));
    let abstract_kw = if external_super { "abstract " } else { "" };
    let mut out = String::new();
    if let Some(package) = package
        && !nested
    {
        let _ = writeln!(out, "package {};", package.replace('/', "."));
    }
    if is_annotation {
        let _ = writeln!(out, "{modifier} @interface {simple} {{}}");
        return out;
    }
    let extends = if !is_record && !class.superclass.is_empty()
        && class.superclass != "java/lang/Object"
        // A record's source form is `record`, not `extends Record`.
        && class.superclass != "java/lang/Record"
        && class.superclass != "java/lang/Enum"
        // A real library superclass would owe its abstract methods.
        && (game.contains_key(&class.superclass)
            || ((available.contains(&class.superclass)
                || class.superclass.starts_with("java/"))
                && (simple.ends_with("Exception") || simple.ends_with("Error"))))
    {
        format!(" extends {}", java_name(&class.superclass))
    } else {
        String::new()
    };
    // Only interfaces keep their parents. A stub class that implemented the
    // game's interfaces would owe their abstract methods and would collide on
    // their default ones; the probe wants the class, not the cast.
    let interfaces: Vec<String> = if is_interface {
        class
            .interfaces
            .iter()
            .filter(|i| i.as_str() != class.name)
            .map(|i| java_name(i))
            .collect()
    } else {
        Vec::new()
    };
    let implements = if interfaces.is_empty() {
        String::new()
    } else {
        format!(" extends {}", interfaces.join(", "))
    };
    let mixin_interfaces = if !is_interface {
        let names = mixin_impls
            .into_iter()
            .flatten()
            .filter(|mixin| available.contains(&mixin.interface))
            .map(|mixin| java_name(&mixin.interface))
            .collect::<BTreeSet<_>>();
        if names.is_empty() {
            String::new()
        } else {
            format!(
                " implements {}",
                names.into_iter().collect::<Vec<_>>().join(", ")
            )
        }
    } else {
        String::new()
    };
    if is_interface {
        let _ = writeln!(out, "{modifier} interface {simple}{implements} {{");
    } else if is_enum {
        let constants: Vec<&str> = class
            .declared
            .iter()
            .filter(|d| d.is_enum_constant())
            .map(|d| d.name.as_str())
            .collect();
        let _ = writeln!(out, "{modifier} enum {simple} {{ {};", constants.join(", "));
    } else if is_record {
        let components = class
            .declared
            .iter()
            .filter(|d| d.kind == MemberKind::Field && !d.is_static() && d.access & 0x1000 == 0)
            .map(|field| format!("{} {}", java_type(&field.descriptor), field.name))
            .collect::<Vec<_>>()
            .join(", ");
        let implements = class
            .interfaces
            .iter()
            .map(|i| java_name(i))
            .collect::<Vec<_>>();
        let implements = if implements.is_empty() {
            String::new()
        } else {
            format!(" implements {}", implements.join(", "))
        };
        let _ = writeln!(
            out,
            "{modifier} {abstract_kw}record {simple}({components}){implements} {{"
        );
    } else {
        let codec_impl =
            if class.name == "net/minecraft/util/StringRepresentable$StringRepresentableCodec" {
                " implements com.mojang.serialization.Codec"
            } else {
                ""
            };
        let trigger = if class.name == "net/minecraft/advancements/triggers/SimpleCriterionTrigger"
        {
            (
                "abstract ",
                " implements net.minecraft.advancements.triggers.CriterionTrigger",
            )
        } else if class.name == "net/minecraft/network/chat/MutableComponent" {
            ("", " implements net.minecraft.network.chat.Component")
        } else {
            ("", "")
        };
        let builder_contract = if class.name == "net/minecraft/world/entity/EntityType$Builder"
            && available
                .contains("net/fabricmc/fabric/impl/object/builder/FabricEntityTypeImpl$Builder")
        {
            " implements net.fabricmc.fabric.impl.object.builder.FabricEntityTypeImpl.Builder"
        } else {
            ""
        };
        let builder_contract = format!("{builder_contract}{mixin_interfaces}");
        let _ = writeln!(
            out,
            "{modifier} {abstract_kw}{trigger_abstract}class {simple}{extends}{codec_impl}{trigger_contract}{builder_contract} {{",
            abstract_kw = abstract_kw,
            trigger_abstract = if abstract_kw.is_empty() {
                trigger.0
            } else {
                ""
            },
            trigger_contract = trigger.1,
            builder_contract = builder_contract
        );
    }
    let identifier = class.name == "net/minecraft/resources/Identifier";
    let resource_key = class.name == "net/minecraft/resources/ResourceKey";
    let tag_key = class.name == "net/minecraft/tags/TagKey";
    if identifier {
        let _ = writeln!(
            out,
            "    private String horizonNamespace = \"minecraft\";\n    private String horizonPath = \"\";"
        );
        let _ = writeln!(
            out,
            "    public Identifier() {{}}\n    public Identifier(String namespace, String path) {{ this.horizonNamespace = namespace; this.horizonPath = path; }}"
        );
    }
    if resource_key {
        let _ = writeln!(
            out,
            "    private net.minecraft.resources.Identifier horizonRegistryName = new net.minecraft.resources.Identifier();\n    private net.minecraft.resources.Identifier horizonIdentifier = new net.minecraft.resources.Identifier();\n    public ResourceKey() {{}}\n    public ResourceKey(net.minecraft.resources.Identifier registryName, net.minecraft.resources.Identifier identifier) {{ this.horizonRegistryName = registryName; this.horizonIdentifier = identifier; }}"
        );
    }
    if tag_key {
        let _ = writeln!(
            out,
            "    private net.minecraft.resources.ResourceKey horizonRegistry = new net.minecraft.resources.ResourceKey();\n    private net.minecraft.resources.Identifier horizonLocation = new net.minecraft.resources.Identifier();\n    public TagKey() {{}}\n    public TagKey(net.minecraft.resources.ResourceKey registry, net.minecraft.resources.Identifier location) {{ this.horizonRegistry = registry; this.horizonLocation = location; }}"
        );
    }
    if matches!(
        class.name.as_str(),
        "net/minecraft/core/Registry" | "net/minecraft/core/DefaultedRegistry"
    ) {
        let _ = writeln!(
            out,
            "    protected final java.util.Map<net.minecraft.resources.Identifier, Object> horizonEntries = new java.util.LinkedHashMap<>();\n    protected final java.util.Map<net.minecraft.resources.Identifier, net.minecraft.resources.ResourceKey> horizonKeys = new java.util.LinkedHashMap<>();"
        );
    }
    let holder_impl = class.name.starts_with("net/minecraft/core/Holder$") && !is_interface;
    if holder_impl {
        let _ = writeln!(
            out,
            "    protected Object horizonValue;\n    protected net.minecraft.resources.ResourceKey horizonKey;\n    protected net.minecraft.resources.Identifier horizonId;\n    public void horizonBind(Object value, net.minecraft.resources.ResourceKey key, net.minecraft.resources.Identifier id) {{ this.horizonValue = value; this.horizonKey = key; this.horizonId = id; }}"
        );
    }
    if class.name == "net/neoforged/neoforge/registries/DeferredHolder" {
        let _ = writeln!(
            out,
            "    protected Object horizonValue;\n    protected java.util.function.Supplier horizonSupplier;\n    protected net.minecraft.resources.Identifier horizonId = new net.minecraft.resources.Identifier();\n    protected net.minecraft.resources.ResourceKey horizonKey = new net.minecraft.resources.ResourceKey();\n    public DeferredHolder() {{}}\n    public DeferredHolder(net.minecraft.resources.Identifier id, net.minecraft.resources.ResourceKey key, java.util.function.Supplier supplier) {{ this.horizonId = id; this.horizonKey = key; this.horizonSupplier = supplier; }}\n    public void horizonBind(net.minecraft.resources.Identifier id, net.minecraft.resources.ResourceKey key, java.util.function.Supplier supplier) {{ this.horizonId = id; this.horizonKey = key; this.horizonSupplier = supplier; }}"
        );
    }
    if matches!(
        class.name.as_str(),
        "net/minecraft/core/Vec3i" | "net/minecraft/core/BlockPos"
    ) {
        let _ = writeln!(
            out,
            "    protected int horizonX;\n    protected int horizonY;\n    protected int horizonZ;"
        );
    }
    if class.name == "net/minecraft/world/phys/Vec3" {
        let _ = writeln!(
            out,
            "    protected double horizonX;\n    protected double horizonY;\n    protected double horizonZ;"
        );
    }
    if class.name == "net/minecraft/network/chat/MutableComponent" {
        let _ = writeln!(
            out,
            "    protected String horizonText = \"\";\n    protected String horizonKey = \"\";"
        );
    }
    if class.name == "net/minecraft/world/phys/AABB" {
        let _ = writeln!(
            out,
            "    protected double horizonMinX;\n    protected double horizonMinY;\n    protected double horizonMinZ;\n    protected double horizonMaxX;\n    protected double horizonMaxY;\n    protected double horizonMaxZ;"
        );
    }
    if class.name == "net/minecraft/world/item/ItemStack" {
        let _ = writeln!(
            out,
            "    protected net.minecraft.world.item.Item horizonItem = new net.minecraft.world.item.Item();\n    protected int horizonCount;\n    protected java.util.Map<net.minecraft.core.component.DataComponentType, Object> horizonComponents = new java.util.HashMap<>();"
        );
    }
    if class.name == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type" {
        let _ = writeln!(
            out,
            "    protected net.minecraft.resources.Identifier horizonId = new net.minecraft.resources.Identifier();"
        );
    }
    if class.name == "net/minecraft/world/level/block/state/BlockState" {
        let _ = writeln!(
            out,
            "    protected java.util.IdentityHashMap<Object, Object> horizonValues = new java.util.IdentityHashMap<>();"
        );
    }
    if class.name == "net/minecraft/nbt/CompoundTag" {
        let _ = writeln!(
            out,
            "    protected java.util.Map<String, Object> horizonValues = new java.util.LinkedHashMap<>();"
        );
    }
    if class.name == "net/minecraft/nbt/ListTag" {
        let _ = writeln!(
            out,
            "    protected java.util.List<net.minecraft.nbt.Tag> horizonValues = new java.util.ArrayList<>();"
        );
    }
    if matches!(
        class.name.as_str(),
        "net/minecraft/world/level/Level" | "net/minecraft/server/level/ServerLevel"
    ) {
        let _ = writeln!(
            out,
            "    protected long horizonGameTime;\n    protected net.minecraft.resources.ResourceKey horizonDimension = new net.minecraft.resources.ResourceKey();\n    protected java.util.Map<String, net.minecraft.world.level.block.state.BlockState> horizonBlocks = new java.util.LinkedHashMap<>();\n    protected java.util.Map<String, net.minecraft.world.level.block.entity.BlockEntity> horizonBlockEntities = new java.util.LinkedHashMap<>();\n    protected String horizonBlockKey(net.minecraft.core.BlockPos pos) {{ return pos.getX() + \",\" + pos.getY() + \",\" + pos.getZ(); }}"
        );
    }
    if class.name == "net/minecraft/world/level/block/entity/BlockEntity" {
        let _ = writeln!(
            out,
            "    protected net.minecraft.core.BlockPos horizonBlockPos = new net.minecraft.core.BlockPos();\n    protected net.minecraft.world.level.Level horizonLevel = new net.minecraft.world.level.Level();\n    protected net.minecraft.world.level.block.state.BlockState horizonBlockState = new net.minecraft.world.level.block.state.BlockState();\n    protected net.minecraft.nbt.CompoundTag horizonData = new net.minecraft.nbt.CompoundTag();\n    protected boolean horizonRemoved;"
        );
    }
    if class.name == "net/minecraft/world/entity/Entity" {
        let _ = writeln!(
            out,
            "    protected double horizonX;\n    protected double horizonY;\n    protected double horizonZ;\n    protected double horizonDeltaX;\n    protected double horizonDeltaY;\n    protected double horizonDeltaZ;\n    protected net.minecraft.world.level.Level horizonLevel = new net.minecraft.world.level.Level();\n    protected java.util.UUID horizonUuid = new java.util.UUID(0L, 0L);\n    protected net.minecraft.world.phys.AABB horizonBoundingBox = new net.minecraft.world.phys.AABB(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);\n    protected boolean horizonOnGround;"
        );
    }
    if class.name == "net/minecraft/world/entity/Mob" {
        let _ = writeln!(
            out,
            "    protected boolean horizonNoAi;\n    protected boolean horizonAggressive;\n    protected net.minecraft.world.entity.LivingEntity horizonTarget;"
        );
    }
    if class.name == "net/minecraft/world/entity/LivingEntity" {
        let _ = writeln!(
            out,
            "    protected float horizonHealth = 20.0f;\n    protected float horizonMaxHealth = 20.0f;\n    protected net.minecraft.world.item.ItemStack horizonMainHand = new net.minecraft.world.item.ItemStack();\n    protected net.minecraft.world.item.ItemStack horizonOffhand = new net.minecraft.world.item.ItemStack();\n    protected int horizonArmorValue;\n    protected java.util.Map<net.minecraft.world.effect.MobEffect, net.minecraft.world.effect.MobEffectInstance> horizonEffects = new java.util.IdentityHashMap<>();"
        );
    }
    if class.name == "net/minecraft/world/effect/MobEffectInstance" {
        let _ = writeln!(
            out,
            "    protected net.minecraft.world.effect.MobEffect horizonEffect = new net.minecraft.world.effect.MobEffect();\n    protected int horizonDuration;\n    protected int horizonAmplifier;\n    protected boolean horizonAmbient;\n    protected boolean horizonVisible = true;\n    protected boolean horizonShowIcon = true;"
        );
    }
    if matches!(
        class.name.as_str(),
        "net/minecraft/world/entity/player/Player" | "net/minecraft/server/level/ServerPlayer"
    ) {
        let _ = writeln!(
            out,
            "    protected boolean horizonCreative;\n    protected boolean horizonSpectator;\n    protected String horizonScoreboardName = \"Player\";\n    protected int horizonExperienceLevel;\n    protected float horizonExperienceProgress;"
        );
    }
    if class.name == "net/minecraft/client/KeyMapping" {
        let _ = writeln!(out, "    private String horizonName = \"\";");
    }
    if class.name == "net/minecraft/world/level/block/Block" {
        let _ = writeln!(
            out,
            "    public void registerDefaultState(net.minecraft.world.level.block.state.BlockState state) {{}}"
        );
    }
    if class.name == "net/minecraft/util/StringRepresentable$StringRepresentableCodec" {
        let _ = writeln!(
            out,
            "    @SuppressWarnings({{\"rawtypes\", \"unchecked\"}}) public com.mojang.serialization.DataResult decode(com.mojang.serialization.DynamicOps ops, Object input) {{ return (com.mojang.serialization.DataResult) com.mojang.serialization.Codec.STRING.decode(ops, input); }}"
        );
        let _ = writeln!(
            out,
            "    @SuppressWarnings({{\"rawtypes\", \"unchecked\"}}) public com.mojang.serialization.DataResult encode(Object value, com.mojang.serialization.DynamicOps ops, Object prefix) {{ return (com.mojang.serialization.DataResult) com.mojang.serialization.Codec.STRING.encode(String.valueOf(value), ops, prefix); }}"
        );
    }
    let mut methods = 0;
    let mut fields = 0;
    for field in class.declared.iter().filter(|d| {
        d.kind == MemberKind::Field
            && !d.is_enum_constant()
            && (!is_record || d.is_static())
            && (!is_enum || field_is_enum_abi(d))
    }) {
        if !(field.is_public() || field.is_protected()) {
            continue;
        }
        let name = &field.name;
        let ty = java_type(&field.descriptor);
        let default = runtime_field_default(
            &class.name,
            &field.descriptor,
            default_value(&field.descriptor),
        );
        if is_interface {
            let _ = writeln!(out, "    public static final {ty} {name} = {default};");
        } else if field.is_static() {
            let initializer = if default != "null" {
                format!(" = {default}")
            } else {
                String::new()
            };
            let _ = writeln!(out, "    public static {ty} {name}{initializer};");
        } else {
            let _ = writeln!(out, "    public {ty} {name};");
        }
        fields += 1;
    }
    // Constructors, so a mod's `super(...)` links.
    let mut no_arg = identifier || resource_key || tag_key;
    for method in class
        .declared
        .iter()
        .filter(|d| d.kind == MemberKind::Method && d.name == "<init>")
    {
        if !(method.is_public() || method.is_protected()) || is_interface || is_record {
            continue;
        }
        let params = parameters(&method.descriptor);
        no_arg |= method.descriptor == "()V";
        let body = if matches!(
            (class.name.as_str(), method.descriptor.as_str()),
            (
                "net/minecraft/core/Vec3i" | "net/minecraft/core/BlockPos",
                "(III)V"
            )
        ) || (class.name == "net/minecraft/world/phys/Vec3"
            && method.descriptor == "(DDD)V")
        {
            "this.horizonX = p0; this.horizonY = p1; this.horizonZ = p2;"
        } else if class.name == "net/minecraft/world/phys/AABB" && method.descriptor == "(DDDDDD)V"
        {
            "this.horizonMinX = p0; this.horizonMinY = p1; this.horizonMinZ = p2; this.horizonMaxX = p3; this.horizonMaxY = p4; this.horizonMaxZ = p5; this.minX = p0; this.minY = p1; this.minZ = p2; this.maxX = p3; this.maxY = p4; this.maxZ = p5;"
        } else if class.name == "net/minecraft/world/phys/AABB"
            && method.descriptor == "(Lnet/minecraft/core/BlockPos;)V"
        {
            "this.horizonMinX = p0.getX(); this.horizonMinY = p0.getY(); this.horizonMinZ = p0.getZ(); this.horizonMaxX = p0.getX() + 1.0; this.horizonMaxY = p0.getY() + 1.0; this.horizonMaxZ = p0.getZ() + 1.0; this.minX = horizonMinX; this.minY = horizonMinY; this.minZ = horizonMinZ; this.maxX = horizonMaxX; this.maxY = horizonMaxY; this.maxZ = horizonMaxZ;"
        } else if class.name == "net/minecraft/world/phys/AABB"
            && method.descriptor
                == "(Lnet/minecraft/world/phys/Vec3;Lnet/minecraft/world/phys/Vec3;)V"
        {
            "this.horizonMinX = Math.min(p0.x(), p1.x()); this.horizonMinY = Math.min(p0.y(), p1.y()); this.horizonMinZ = Math.min(p0.z(), p1.z()); this.horizonMaxX = Math.max(p0.x(), p1.x()); this.horizonMaxY = Math.max(p0.y(), p1.y()); this.horizonMaxZ = Math.max(p0.z(), p1.z()); this.minX = horizonMinX; this.minY = horizonMinY; this.minZ = horizonMinZ; this.maxX = horizonMaxX; this.maxY = horizonMaxY; this.maxZ = horizonMaxZ;"
        } else if class.name == "net/minecraft/world/level/block/Block" {
            "this.stateDefinition = new net.minecraft.world.level.block.state.StateDefinition();"
        } else if class.name == "net/minecraft/client/KeyMapping" {
            "this.horizonName = p0;"
        } else if class.name == "net/minecraft/world/item/ItemStack" {
            match method.descriptor.as_str() {
                "(Lnet/minecraft/world/item/Item;I)V" => {
                    "this.horizonItem = p0; this.horizonCount = p1;"
                }
                "(Lnet/minecraft/world/item/Item;)V" => {
                    "this.horizonItem = p0; this.horizonCount = 1;"
                }
                _ => "",
            }
        } else if class.name == "net/minecraft/world/effect/MobEffectInstance" {
            match method.descriptor.as_str() {
                "(Lnet/minecraft/world/effect/MobEffect;I)V" => {
                    "this.horizonEffect = p0; this.horizonDuration = p1;"
                }
                "(Lnet/minecraft/world/effect/MobEffect;IIZZZ)V" => {
                    "this.horizonEffect = p0; this.horizonDuration = p1; this.horizonAmplifier = p2; this.horizonAmbient = p3; this.horizonVisible = p4; this.horizonShowIcon = p5;"
                }
                _ => "",
            }
        } else if class.name
            == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type"
            && method.descriptor == "(Lnet/minecraft/resources/Identifier;)V"
        {
            "this.horizonId = p0;"
        } else if class.name == "net/minecraft/world/level/block/entity/BlockEntity"
            && method.descriptor
                == "(Lnet/minecraft/world/level/block/entity/BlockEntityType;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;)V"
        {
            "this.horizonBlockPos = p1; this.horizonBlockState = p2;"
        } else if class.name == "net/minecraft/world/level/block/entity/BlockEntity"
            && method.descriptor
                == "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;)V"
        {
            "this.horizonBlockPos = p0; this.horizonBlockState = p1;"
        } else {
            ""
        };
        let _ = writeln!(out, "    public {simple}({params}) {{ {body} }}");
    }
    // Every stub has a no-arg constructor, so a subclass's implicit `super()`
    // links even when the real class only has constructors with arguments.
    if !is_interface && !is_enum && !is_record && !no_arg {
        let body = if matches!(
            class.name.as_str(),
            "net/minecraft/core/Vec3i" | "net/minecraft/core/BlockPos"
        ) {
            "this.horizonX = 0; this.horizonY = 0; this.horizonZ = 0;"
        } else if class.name == "net/minecraft/world/phys/Vec3" {
            "this.horizonX = 0.0; this.horizonY = 0.0; this.horizonZ = 0.0;"
        } else if class.name == "net/minecraft/world/phys/AABB" {
            "this.horizonMinX = 0.0; this.horizonMinY = 0.0; this.horizonMinZ = 0.0; this.horizonMaxX = 0.0; this.horizonMaxY = 0.0; this.horizonMaxZ = 0.0; this.minX = 0.0; this.minY = 0.0; this.minZ = 0.0; this.maxX = 0.0; this.maxY = 0.0; this.maxZ = 0.0;"
        } else if class.name == "net/minecraft/world/level/block/Block" {
            "this.stateDefinition = new net.minecraft.world.level.block.state.StateDefinition();"
        } else if class.name == "net/minecraft/client/KeyMapping" {
            "this.horizonName = \"\";"
        } else if class.name == "net/minecraft/world/item/ItemStack" {
            "this.horizonItem = new net.minecraft.world.item.Item(); this.horizonCount = 0;"
        } else {
            ""
        };
        let _ = writeln!(out, "    public {simple}() {{ {body} }}");
    }
    if class.name == "net/minecraft/world/entity/EntityType$Builder"
        && available
            .contains("net/fabricmc/fabric/impl/object/builder/FabricEntityTypeImpl$Builder")
    {
        let _ = writeln!(
            out,
            "    public void fabric_setLivingEntityBuilder(net.fabricmc.fabric.impl.object.builder.FabricEntityTypeImpl.Builder.Living p0) {{}}"
        );
        let _ = writeln!(
            out,
            "    public void fabric_setMobEntityBuilder(net.fabricmc.fabric.impl.object.builder.FabricEntityTypeImpl.Builder.Mob p0) {{}}"
        );
    }
    if is_record {
        let defaults = class
            .declared
            .iter()
            .filter(|d| d.kind == MemberKind::Field && !d.is_static() && d.access & 0x1000 == 0)
            .map(|field| constructor_default_value(&field.descriptor))
            .collect::<Vec<_>>()
            .join(", ");
        if !defaults.is_empty() {
            let _ = writeln!(out, "    public {simple}() {{ this({defaults}); }}");
        }
    }
    let all = used.cloned().unwrap_or_default();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for method in &class.declared {
        if method.kind != MemberKind::Method || method.name == "<init>" {
            continue;
        }
        if class.name == "net/minecraft/util/StringRepresentable$StringRepresentableCodec"
            && matches!(method.name.as_str(), "encode" | "decode")
        {
            continue;
        }
        if is_record
            && class.declared.iter().any(|field| {
                field.kind == MemberKind::Field
                    && !field.is_static()
                    && field.name == method.name
                    && method.descriptor.starts_with("()")
            })
        {
            // Record components already generate their accessor methods.
            continue;
        }
        if is_record && matches!(method.name.as_str(), "equals" | "hashCode" | "toString") {
            // javac supplies the canonical record object methods.
            continue;
        }
        // `javac` writes these for an enum; declaring them is an error.
        if is_enum && (method.name == "values" || method.name == "valueOf") {
            continue;
        }
        let named = all.contains(&(method.name.clone(), method.descriptor.clone()));
        if !named
            && !(is_interface && (method.is_public() || method.is_protected()))
            && !(is_record && (method.is_public() || method.is_protected()))
        {
            continue;
        }
        if !(method.is_public() || method.is_protected()) {
            continue;
        }
        // An interface cannot redeclare `Object`'s methods as default.
        if is_interface && matches!(method.name.as_str(), "toString" | "equals" | "hashCode") {
            continue;
        }
        let (ret, params) = signature(&method.descriptor);
        // Two generic overloads erase to one signature; emit it once.
        if !seen.insert((method.name.clone(), params.clone())) {
            continue;
        }
        let statik = if method.is_static() { "static " } else { "" };
        let keyword = if is_interface && !method.is_static() {
            "default "
        } else {
            statik
        };
        // Preserve the callable visibility from the game shape. Leaving a
        // public method package-private compiles the façade but fails as soon
        // as a mod invokes it from its own package (`IllegalAccessError`).
        // Object overrides are public in the real classes as well.
        let visibility = if method.is_public() {
            "public "
        } else {
            "protected "
        };
        let body =
            runtime_body_with_descriptor(&class.name, &method.name, &ret, &method.descriptor);
        let _ = writeln!(
            out,
            "    {visibility}{keyword}{ret} {}({params}) {{ {body} }}",
            method.name
        );
        methods += 1;
    }
    // The NeoForge item-handler shim is compiled beside the generated game
    // façade rather than being part of the mod census. Materialize the small
    // ItemStack value ABI it uses even when a mod only mentions ItemStack in
    // an IItemHandler signature; otherwise the shim's reflective access would
    // fail closed to an empty stack for every transfer.
    if class.name == "net/minecraft/world/item/ItemStack" {
        let required = [
            ("getCount", "()I", "int", ""),
            (
                "getItem",
                "()Lnet/minecraft/world/item/Item;",
                "net.minecraft.world.item.Item",
                "",
            ),
            ("setCount", "(I)V", "void", "int p0"),
            (
                "copy",
                "()Lnet/minecraft/world/item/ItemStack;",
                "net.minecraft.world.item.ItemStack",
                "",
            ),
            (
                "copyWithCount",
                "(I)Lnet/minecraft/world/item/ItemStack;",
                "net.minecraft.world.item.ItemStack",
                "int p0",
            ),
            ("grow", "(I)V", "void", "int p0"),
            ("shrink", "(I)V", "void", "int p0"),
            ("isEmpty", "()Z", "boolean", ""),
        ];
        for (name, descriptor, ret, params) in required {
            if !seen.insert((name.to_owned(), params.to_owned())) {
                continue;
            }
            let body = runtime_body_with_descriptor(&class.name, name, ret, descriptor);
            let _ = writeln!(out, "    public {ret} {name}({params}) {{ {body} }}");
            methods += 1;
        }
    }
    if class.name == "net/minecraft/network/chat/MutableComponent" {
        if seen.insert(("getString".to_owned(), "".to_owned())) {
            let _ = writeln!(
                out,
                "    public String getString() {{ return horizonText; }}"
            );
            methods += 1;
        }
        if seen.insert((
            "append".to_owned(),
            "net.minecraft.network.chat.Component p0".to_owned(),
        )) {
            let _ = writeln!(
                out,
                "    public net.minecraft.network.chat.MutableComponent append(net.minecraft.network.chat.Component p0) {{ if (p0 != null) horizonText += p0.getString(); return this; }}"
            );
            methods += 1;
        }
        if seen.insert(("append".to_owned(), "java.lang.String p0".to_owned())) {
            let _ = writeln!(
                out,
                "    public net.minecraft.network.chat.MutableComponent append(java.lang.String p0) {{ if (p0 != null) horizonText += p0; return this; }}"
            );
            methods += 1;
        }
        if seen.insert(("copy".to_owned(), "".to_owned())) {
            let _ = writeln!(
                out,
                "    public net.minecraft.network.chat.MutableComponent copy() {{ net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonText = horizonText; value.horizonKey = horizonKey; return value; }}"
            );
            methods += 1;
        }
    }
    if is_record {
        let mut add_record_method = |name: &str, params: &str, ret: &str, body: &str| {
            if seen.insert((name.to_owned(), params.to_owned())) {
                let _ = writeln!(out, "    public {ret} {name}({params}) {{ {body} }}");
            }
        };
        for interface in &class.interfaces {
            match interface.as_str() {
                "java/lang/AutoCloseable" => add_record_method("close", "", "void", ""),
                "java/lang/Comparable" => {
                    add_record_method("compareTo", "java.lang.Object p0", "int", "return 0;")
                }
                "java/util/function/Predicate" => {
                    add_record_method("test", "java.lang.Object p0", "boolean", "return false;")
                }
                _ => {}
            }
        }
    }
    // A mod can be compiled against a nearby 26.3 snapshot whose member was
    // removed or renamed in the game jar. Preserve that named ABI edge as a
    // synthetic public method so the JVM reports the next semantic gap rather
    // than failing linkage before the entrypoint starts.
    for (name, descriptor) in &all {
        if name == "<init>"
            || matches!(name.as_str(), "getClass" | "name" | "ordinal")
            || (is_interface && matches!(name.as_str(), "equals" | "hashCode" | "toString"))
            || (is_enum
                && matches!(
                    name.as_str(),
                    "values" | "valueOf" | "hashCode" | "equals" | "toString"
                ))
            || class
                .declared
                .iter()
                .any(|member| member.name == *name && member.descriptor == *descriptor)
        {
            continue;
        }
        let (ret, params) = signature(descriptor);
        if !seen.insert((name.clone(), params.clone())) {
            continue;
        }
        let body = runtime_body_with_descriptor(&class.name, name, &ret, descriptor);
        let keyword = if is_interface { "default " } else { "" };
        let _ = writeln!(
            out,
            "    public {keyword}{ret} {name}({params}) {{ {body} }}"
        );
        methods += 1;
    }
    // Materialize the abstract methods from the safe accessor/invoker
    // interfaces on the generated target class. The bodies intentionally
    // return the JVM default for their primitive/void descriptor; this
    // provides linkage and casts while keeping semantic work visible at the
    // next probe boundary.
    if let Some(mixins) = mixin_impls {
        for mixin in mixins {
            if !available.contains(&mixin.interface) {
                continue;
            }
            for method in &mixin.methods {
                let (ret, params) = signature(&method.descriptor);
                if !seen.insert((method.name.clone(), params.clone())) {
                    continue;
                }
                let body = if ret == "void" {
                    String::new()
                } else {
                    format!("return {};", default_value_for_java_type(&ret))
                };
                let _ = writeln!(
                    out,
                    "    public {ret} {}({params}) {{ {body} }}",
                    method.name
                );
                methods += 1;
            }
        }
    }
    for child in inner {
        let _ = writeln!(out, "{child}");
    }
    let _ = writeln!(out, "}}");
    report.classes += 1;
    report.methods += methods;
    report.fields += fields;
    out
}

/// Small value factories that are safe to execute while a mod is booting.
///
/// The façade is intentionally mostly a shape-only jar, but a few classes are
/// reached from static initializers before the game has attached its native
/// bridge. `StreamCodec` is one such class: its factories only need to return
/// an object implementing the codec interface. Returning an empty façade
/// instance keeps registration code alive; encode/decode remain the next
/// boundary to implement when a mod actually sends data.
#[cfg(test)]
fn runtime_body(class: &str, method: &str, ret: &str) -> String {
    runtime_body_with_descriptor(class, method, ret, "")
}

fn runtime_body_with_descriptor(class: &str, method: &str, ret: &str, descriptor: &str) -> String {
    // NeoForge's public registration path is deliberately small: a mod gets
    // an event bus from the loading context, creates a DeferredRegister, and
    // attaches it to that bus. Keep those calls alive in the façade without
    // importing NeoForge's implementation.
    if class == "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext"
        && method == "get"
        && ret == "net.neoforged.fml.javafmlmod.FMLJavaModLoadingContext"
    {
        return "return new FMLJavaModLoadingContext();".to_owned();
    }
    if class == "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext"
        && method == "getModEventBus"
        && ret == "net.neoforged.bus.api.IEventBus"
    {
        return "return new net.neoforged.bus.api.IEventBus() {};".to_owned();
    }
    if class == "net/neoforged/fml/ModContainer"
        && method == "getEventBus"
        && ret == "net.neoforged.bus.api.IEventBus"
    {
        return "return new net.neoforged.bus.api.IEventBus() {};".to_owned();
    }
    if class == "net/neoforged/bus/api/IEventBus" && ret == "net.neoforged.bus.api.IEventBus" {
        return "return this;".to_owned();
    }
    if class == "net/neoforged/bus/api/IEventBus"
        && method == "post"
        && ret == "net.neoforged.bus.api.Event"
    {
        return "return p0;".to_owned();
    }
    if class == "net/neoforged/neoforge/capabilities/CapabilityManager"
        && method == "get"
        && ret == "net.neoforged.neoforge.capabilities.Capability"
    {
        return "return new net.neoforged.neoforge.capabilities.Capability();".to_owned();
    }
    if class == "net/neoforged/neoforge/capabilities/Capability"
        && method == "isRegistered"
        && ret == "boolean"
    {
        return "return true;".to_owned();
    }
    if class == "net/neoforged/neoforge/common/util/LazyOptional" {
        match (method, ret) {
            ("empty", "net.neoforged.neoforge.common.util.LazyOptional")
            | ("of", "net.neoforged.neoforge.common.util.LazyOptional")
            | ("ofNullable", "net.neoforged.neoforge.common.util.LazyOptional") => {
                return "return new net.neoforged.neoforge.common.util.LazyOptional();".to_owned();
            }
            ("isPresent", "boolean") => {
                return "return horizonValid && horizonGet() != null;".to_owned();
            }
            ("resolve", _) => return "return horizonGet();".to_owned(),
            ("orElse", _) => return "return horizonGet() == null ? p0 : horizonGet();".to_owned(),
            ("invalidate", "void") => {
                return "horizonValid = false; horizonValue = null; horizonSupplier = () -> null;"
                    .to_owned();
            }
            _ => {}
        }
    }
    if class == "net/neoforged/neoforge/registries/RegisterEvent" {
        if method == "getRegistryKey" && ret == "net.minecraft.resources.ResourceKey" {
            return "return horizonRegistryKey;".to_owned();
        }
        if method == "getRegistry" && ret == "net.minecraft.core.Registry" {
            return "return horizonRegistry;".to_owned();
        }
    }
    if class == "net/neoforged/neoforge/registries/DeferredRegister" {
        if method == "create" && ret == "net.neoforged.neoforge.registries.DeferredRegister" {
            return "return new DeferredRegister();".to_owned();
        }
        if ret == "net.neoforged.neoforge.registries.DeferredRegister" {
            return "return this;".to_owned();
        }
        if ret == "net.neoforged.neoforge.registries.DeferredHolder" {
            return "return new net.neoforged.neoforge.registries.DeferredHolder();".to_owned();
        }
    }
    if class == "net/neoforged/neoforge/registries/DeferredHolder" {
        match (method, ret) {
            ("get" | "value", _) if descriptor == "()Ljava/lang/Object;" => {
                return "if (horizonValue == null && horizonSupplier != null) horizonValue = horizonSupplier.get(); return horizonValue;".to_owned();
            }
            ("isPresent", "boolean") => {
                return "return get() != null;".to_owned();
            }
            ("getOptional", "java.util.Optional") => {
                return "return java.util.Optional.ofNullable(get());".to_owned();
            }
            ("getKey", "net.minecraft.resources.ResourceKey") => {
                return "return horizonKey;".to_owned();
            }
            ("getId", "net.minecraft.resources.Identifier") => {
                return "return horizonId;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/neoforged/neoforge/network/event/RegisterPayloadHandlersEvent"
        && method == "registrar"
        && ret == "net.neoforged.neoforge.network.registration.PayloadRegistrar"
    {
        return "return new net.neoforged.neoforge.network.registration.PayloadRegistrar();"
            .to_owned();
    }
    if class == "net/neoforged/neoforge/network/registration/PayloadRegistrar"
        && ret == "net.neoforged.neoforge.network.registration.PayloadRegistrar"
    {
        return "return this;".to_owned();
    }
    if class == "net/neoforged/fml/ModContainer"
        && method == "getModId"
        && ret == "java.lang.String"
    {
        return "return \"\";".to_owned();
    }
    if class == "net/minecraft/world/level/block/Block"
        && method == "column"
        && ret == "net.minecraft.world.phys.shapes.VoxelShape"
    {
        return voxel_shape_placeholder();
    }
    if matches!(
        class,
        "net/minecraft/core/Vec3i" | "net/minecraft/core/BlockPos"
    ) {
        match (method, ret) {
            ("getX", "int") => return "return horizonX;".to_owned(),
            ("getY", "int") => return "return horizonY;".to_owned(),
            ("getZ", "int") => return "return horizonZ;".to_owned(),
            ("above", "net.minecraft.core.BlockPos") => {
                return "return new net.minecraft.core.BlockPos(horizonX, horizonY + 1, horizonZ);"
                    .to_owned();
            }
            ("below", "net.minecraft.core.BlockPos") => {
                return "return new net.minecraft.core.BlockPos(horizonX, horizonY - 1, horizonZ);"
                    .to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/phys/Vec3" {
        match (method, ret) {
            ("x", "double") => return "return horizonX;".to_owned(),
            ("y", "double") => return "return horizonY;".to_owned(),
            ("z", "double") => return "return horizonZ;".to_owned(),
            ("add", "net.minecraft.world.phys.Vec3")
                if descriptor
                    == "(Lnet/minecraft/world/phys/Vec3;)Lnet/minecraft/world/phys/Vec3;" =>
            {
                return "return new net.minecraft.world.phys.Vec3(horizonX + p0.x(), horizonY + p0.y(), horizonZ + p0.z());".to_owned();
            }
            ("add", "net.minecraft.world.phys.Vec3")
                if descriptor == "(DDD)Lnet/minecraft/world/phys/Vec3;" =>
            {
                return "return new net.minecraft.world.phys.Vec3(horizonX + p0, horizonY + p1, horizonZ + p2);".to_owned();
            }
            ("subtract", "net.minecraft.world.phys.Vec3")
                if descriptor
                    == "(Lnet/minecraft/world/phys/Vec3;)Lnet/minecraft/world/phys/Vec3;" =>
            {
                return "return new net.minecraft.world.phys.Vec3(horizonX - p0.x(), horizonY - p0.y(), horizonZ - p0.z());".to_owned();
            }
            ("subtract", "net.minecraft.world.phys.Vec3")
                if descriptor == "(DDD)Lnet/minecraft/world/phys/Vec3;" =>
            {
                return "return new net.minecraft.world.phys.Vec3(horizonX - p0, horizonY - p1, horizonZ - p2);".to_owned();
            }
            ("scale", "net.minecraft.world.phys.Vec3")
                if descriptor == "(D)Lnet/minecraft/world/phys/Vec3;" =>
            {
                return "return new net.minecraft.world.phys.Vec3(horizonX * p0, horizonY * p0, horizonZ * p0);".to_owned();
            }
            ("reverse", "net.minecraft.world.phys.Vec3") => {
                return "return new net.minecraft.world.phys.Vec3(-horizonX, -horizonY, -horizonZ);".to_owned();
            }
            ("lengthSqr", "double") => {
                return "return horizonX * horizonX + horizonY * horizonY + horizonZ * horizonZ;"
                    .to_owned();
            }
            ("length", "double") => {
                return "return Math.sqrt(horizonX * horizonX + horizonY * horizonY + horizonZ * horizonZ);".to_owned();
            }
            ("normalize", "net.minecraft.world.phys.Vec3") => {
                return "double length = length(); return length < 1.0E-4 ? new net.minecraft.world.phys.Vec3(0.0, 0.0, 0.0) : new net.minecraft.world.phys.Vec3(horizonX / length, horizonY / length, horizonZ / length);".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/phys/AABB" {
        match (method, ret) {
            ("getXsize", "double") => {
                return "return horizonMaxX - horizonMinX;".to_owned();
            }
            ("getYsize", "double") => {
                return "return horizonMaxY - horizonMinY;".to_owned();
            }
            ("getZsize", "double") => {
                return "return horizonMaxZ - horizonMinZ;".to_owned();
            }
            ("contains", "boolean") if descriptor == "(Lnet/minecraft/world/phys/Vec3;)Z" => {
                return "return p0.x() >= horizonMinX && p0.x() < horizonMaxX && p0.y() >= horizonMinY && p0.y() < horizonMaxY && p0.z() >= horizonMinZ && p0.z() < horizonMaxZ;".to_owned();
            }
            ("getCenter", "net.minecraft.world.phys.Vec3") => {
                return "return new net.minecraft.world.phys.Vec3((horizonMinX + horizonMaxX) * 0.5, (horizonMinY + horizonMaxY) * 0.5, (horizonMinZ + horizonMaxZ) * 0.5);".to_owned();
            }
            ("getBottomCenter", "net.minecraft.world.phys.Vec3") => {
                return "return new net.minecraft.world.phys.Vec3((horizonMinX + horizonMaxX) * 0.5, horizonMinY, (horizonMinZ + horizonMaxZ) * 0.5);".to_owned();
            }
            ("getMinPosition", "net.minecraft.world.phys.Vec3") => {
                return "return new net.minecraft.world.phys.Vec3(horizonMinX, horizonMinY, horizonMinZ);".to_owned();
            }
            ("getMaxPosition", "net.minecraft.world.phys.Vec3") => {
                return "return new net.minecraft.world.phys.Vec3(horizonMaxX, horizonMaxY, horizonMaxZ);".to_owned();
            }
            ("inflate", "net.minecraft.world.phys.AABB")
                if descriptor == "(D)Lnet/minecraft/world/phys/AABB;" =>
            {
                return "return new net.minecraft.world.phys.AABB(horizonMinX - p0, horizonMinY - p0, horizonMinZ - p0, horizonMaxX + p0, horizonMaxY + p0, horizonMaxZ + p0);".to_owned();
            }
            ("expandTowards", "net.minecraft.world.phys.AABB")
                if descriptor
                    == "(Lnet/minecraft/world/phys/Vec3;)Lnet/minecraft/world/phys/AABB;" =>
            {
                return "return new net.minecraft.world.phys.AABB(p0.x() < 0.0 ? horizonMinX + p0.x() : horizonMinX, p0.y() < 0.0 ? horizonMinY + p0.y() : horizonMinY, p0.z() < 0.0 ? horizonMinZ + p0.z() : horizonMinZ, p0.x() > 0.0 ? horizonMaxX + p0.x() : horizonMaxX, p0.y() > 0.0 ? horizonMaxY + p0.y() : horizonMaxY, p0.z() > 0.0 ? horizonMaxZ + p0.z() : horizonMaxZ);".to_owned();
            }
            ("ofSize", "net.minecraft.world.phys.AABB")
                if descriptor
                    == "(Lnet/minecraft/world/phys/Vec3;DDD)Lnet/minecraft/world/phys/AABB;" =>
            {
                return "return new net.minecraft.world.phys.AABB(p0.x() - p1 * 0.5, p0.y() - p2 * 0.5, p0.z() - p3 * 0.5, p0.x() + p1 * 0.5, p0.y() + p2 * 0.5, p0.z() + p3 * 0.5);".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/Direction" {
        match (method, ret, descriptor) {
            ("getName" | "getSerializedName", "java.lang.String", _) => {
                return "return name().toLowerCase(java.util.Locale.ROOT);".to_owned();
            }
            ("get3DDataValue", "int", _) => return "return ordinal();".to_owned(),
            ("getStepX", "int", _) => {
                return "return this == WEST ? -1 : this == EAST ? 1 : 0;".to_owned();
            }
            ("getStepY", "int", _) => {
                return "return this == DOWN ? -1 : this == UP ? 1 : 0;".to_owned();
            }
            ("getStepZ", "int", _) => {
                return "return this == NORTH ? -1 : this == SOUTH ? 1 : 0;".to_owned();
            }
            ("getAxis", "net.minecraft.core.Direction.Axis", _) => {
                return "return (this == UP || this == DOWN) ? Axis.Y : (this == EAST || this == WEST) ? Axis.X : Axis.Z;".to_owned();
            }
            ("getAxisDirection", "net.minecraft.core.Direction.AxisDirection", _) => {
                return "return (this == UP || this == SOUTH || this == EAST) ? AxisDirection.POSITIVE : AxisDirection.NEGATIVE;".to_owned();
            }
            ("getOpposite", "net.minecraft.core.Direction", _) => {
                return "return switch (this) { case DOWN -> UP; case UP -> DOWN; case NORTH -> SOUTH; case SOUTH -> NORTH; case WEST -> EAST; case EAST -> WEST; };".to_owned();
            }
            ("getClockWise", "net.minecraft.core.Direction", _) => {
                return "return switch (this) { case NORTH -> EAST; case EAST -> SOUTH; case SOUTH -> WEST; case WEST -> NORTH; default -> this; };".to_owned();
            }
            ("getCounterClockWise", "net.minecraft.core.Direction", _) => {
                return "return switch (this) { case NORTH -> WEST; case WEST -> SOUTH; case SOUTH -> EAST; case EAST -> NORTH; default -> this; };".to_owned();
            }
            ("getNormal", "net.minecraft.core.Vec3i", _) => {
                return "return new net.minecraft.core.Vec3i(getStepX(), getStepY(), getStepZ());"
                    .to_owned();
            }
            ("from3DDataValue", "net.minecraft.core.Direction", _) => {
                return "return values()[Math.floorMod(p0, values().length)];".to_owned();
            }
            ("byName", "net.minecraft.core.Direction", _) => {
                return "if (p0 == null) return null; for (Direction value : values()) if (value.getName().equals(p0)) return value; return null;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/Direction$Axis" {
        match (method, ret) {
            ("getName" | "getSerializedName", "java.lang.String") => {
                return "return name().toLowerCase(java.util.Locale.ROOT);".to_owned();
            }
            ("isVertical", "boolean") => return "return this == Y;".to_owned(),
            ("isHorizontal", "boolean") => return "return this != Y;".to_owned(),
            ("getPlane", "net.minecraft.core.Direction.Plane") => {
                return "return this == Y ? Direction.Plane.VERTICAL : Direction.Plane.HORIZONTAL;"
                    .to_owned();
            }
            ("choose", "int") => {
                return "return this == X ? p0 : this == Y ? p1 : p2;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/Direction$AxisDirection" {
        match (method, ret) {
            ("getStep", "int") => return "return this == POSITIVE ? 1 : -1;".to_owned(),
            ("isPositive", "boolean") => return "return this == POSITIVE;".to_owned(),
            ("getName" | "getSerializedName", "java.lang.String") => {
                return "return name().toLowerCase(java.util.Locale.ROOT);".to_owned();
            }
            ("opposite", "net.minecraft.core.Direction.AxisDirection") => {
                return "return this == POSITIVE ? NEGATIVE : POSITIVE;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type"
        && method == "id"
        && ret == "net.minecraft.resources.Identifier"
    {
        return "return horizonId;".to_owned();
    }
    if class == "net/minecraft/network/codec/StreamCodec"
        && ret == "net.minecraft.network.codec.StreamCodec"
        && matches!(method, "map" | "mapStream" | "dispatch" | "apply")
    {
        return "return this;".to_owned();
    }
    if ret == "net.minecraft.network.codec.StreamCodec" {
        return "return new net.minecraft.network.codec.StreamCodec() {};".to_owned();
    }
    // Codec values are used while Minecraft registers codecs in static
    // initializers.  The real datafixerupper classes are on the probe's
    // classpath, so a stable primitive codec is enough to let that bootstrap
    // continue without pretending to implement the encoded schema.
    if ret == "com.mojang.serialization.Codec" {
        return "return com.mojang.serialization.Codec.STRING;".to_owned();
    }
    if ret == "com.mojang.serialization.MapCodec" {
        return "return com.mojang.serialization.Codec.STRING.fieldOf(\"\");".to_owned();
    }
    if class == "net/minecraft/network/VarInt" && method == "getByteSize" && ret == "int" {
        return "int v = p0; return (v & -128) == 0 ? 1 : (v & -16384) == 0 ? 2 : (v & -2097152) == 0 ? 3 : (v & -268435456) == 0 ? 4 : 5;".to_owned();
    }
    if class == "net/minecraft/network/VarInt" && method == "hasContinuationBit" && ret == "boolean"
    {
        return "return (p0 & 0x80) != 0;".to_owned();
    }
    if class == "net/minecraft/world/flag/FeatureFlagSet"
        && method == "of"
        && ret == "net.minecraft.world.flag.FeatureFlagSet"
    {
        return "return new FeatureFlagSet();".to_owned();
    }
    if class == "net/minecraft/util/StringRepresentable" {
        if method == "fromEnum" && ret == "net.minecraft.util.StringRepresentable.EnumCodec" {
            return "return new EnumCodec();".to_owned();
        }
        if matches!(method, "fromValues" | "fromEnumWithMapping")
            && ret == "com.mojang.serialization.Codec"
        {
            return "return com.mojang.serialization.Codec.STRING;".to_owned();
        }
    }
    if class == "net/minecraft/core/Vec3i"
        && method == "offsetCodec"
        && ret == "com.mojang.serialization.Codec"
    {
        return "return com.mojang.serialization.Codec.STRING;".to_owned();
    }
    if class == "net/minecraft/resources/FileToIdConverter"
        && matches!(method, "json" | "registry")
        && ret == "net.minecraft.resources.FileToIdConverter"
    {
        return "return new FileToIdConverter();".to_owned();
    }
    if class == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type"
        && method == "id"
        && ret == "net.minecraft.resources.Identifier"
    {
        return "return new net.minecraft.resources.Identifier();".to_owned();
    }
    if class == "net/minecraft/core/component/DataComponentType" && method == "builder" {
        return "return new DataComponentType.Builder();".to_owned();
    }
    if class == "net/minecraft/core/component/DataComponentType$Builder" {
        if ret == "net.minecraft.core.component.DataComponentType.Builder" {
            return "return this;".to_owned();
        }
        if method == "build" && ret == "net.minecraft.core.component.DataComponentType" {
            return "return new net.minecraft.core.component.DataComponentType() {};".to_owned();
        }
    }
    if matches!(
        class,
        "net/minecraft/core/Registry" | "net/minecraft/core/DefaultedRegistry"
    ) {
        if method == "register" {
            if descriptor
                .starts_with("(Lnet/minecraft/core/Registry;Lnet/minecraft/resources/ResourceKey;")
            {
                return "p0.horizonEntries.put(p1.location(), p2); p0.horizonKeys.put(p1.location(), p1); return p2;".to_owned();
            }
            if descriptor.starts_with("(Lnet/minecraft/resources/ResourceKey;") {
                return "horizonEntries.put(p1, p2); horizonKeys.put(p1, p0); return p2;"
                    .to_owned();
            }
            return "return p2;".to_owned();
        }
        if method == "containsKey" && ret == "boolean" {
            return "return horizonEntries.containsKey(p0);".to_owned();
        }
        if method == "get" && descriptor.contains("Lnet/minecraft/resources/Identifier;") {
            return format!("return ({ret}) horizonEntries.get(p0);");
        }
        if method == "getOptional"
            && ret == "java.util.Optional"
            && descriptor.contains("Lnet/minecraft/resources/Identifier;")
        {
            return "return java.util.Optional.ofNullable(horizonEntries.get(p0));".to_owned();
        }
        if method == "getKey" && ret == "net.minecraft.resources.Identifier" {
            return "for (java.util.Map.Entry<net.minecraft.resources.Identifier,Object> e : horizonEntries.entrySet()) if (java.util.Objects.equals(e.getValue(), p0)) return e.getKey(); return null;".to_owned();
        }
        if method == "getResourceKey" && ret == "java.util.Optional" {
            return "net.minecraft.resources.Identifier id = getKey(p0); return id == null ? java.util.Optional.empty() : java.util.Optional.ofNullable(horizonKeys.get(id));".to_owned();
        }
    }
    if class == "net/minecraft/resources/RegistryValidator"
        && matches!(method, "none" | "nonEmpty")
        && ret == "net.minecraft.resources.RegistryValidator"
    {
        return "return new net.minecraft.resources.RegistryValidator() {};".to_owned();
    }
    if class == "net/minecraft/tags/TagKey"
        && method == "create"
        && ret == "net.minecraft.tags.TagKey"
    {
        return "return new net.minecraft.tags.TagKey(p0, p1);".to_owned();
    }
    if class == "net/minecraft/tags/TagKey" {
        match (method, ret) {
            ("location", "net.minecraft.resources.Identifier") => {
                return "return horizonLocation;".to_owned();
            }
            ("registry", "net.minecraft.resources.ResourceKey") => {
                return "return horizonRegistry;".to_owned();
            }
            ("isFor", "boolean") => {
                return "return java.util.Objects.equals(horizonRegistry, p0);".to_owned();
            }
            ("hashCode", "int") => {
                return "return java.util.Objects.hash(horizonRegistry, horizonLocation);"
                    .to_owned();
            }
            ("equals", "boolean") => {
                return "return p0 instanceof TagKey other && java.util.Objects.equals(horizonRegistry, other.horizonRegistry) && java.util.Objects.equals(horizonLocation, other.horizonLocation);".to_owned();
            }
            ("toString", "java.lang.String") => {
                return "return \"TagKey[\" + horizonRegistry + \" / \" + horizonLocation + \"]\";"
                    .to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/HolderSet" {
        match (method, ret) {
            ("size", "int") => return "return 0;".to_owned(),
            ("isEmpty", "boolean") => return "return true;".to_owned(),
            ("contains", "boolean") => return "return false;".to_owned(),
            ("stream", "java.util.stream.Stream") => {
                return "return java.util.stream.Stream.empty();".to_owned();
            }
            ("get", _) => return "return null;".to_owned(),
            ("unwrap", "java.util.Optional") => {
                return "return java.util.Optional.empty();".to_owned();
            }
            ("canSerializeIn", "boolean") => return "return false;".to_owned(),
            ("empty" | "direct" | "list" | "named", "net.minecraft.core.HolderSet") => {
                return "return new net.minecraft.core.HolderSet() {};".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/Registry"
        && method == "registerForHolder"
        && ret == "net.minecraft.core.Holder.Reference"
    {
        if descriptor.starts_with(
            "(Lnet/minecraft/core/Registry;Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;"
        ) {
            return "p0.horizonEntries.put(p2, p3); p0.horizonKeys.put(p2, p1); net.minecraft.core.Holder.Reference holder = new net.minecraft.core.Holder.Reference(); holder.horizonBind(p3, p1, p2); return holder;".to_owned();
        }
        return "horizonEntries.put(p1, p2); horizonKeys.put(p1, p0); net.minecraft.core.Holder.Reference holder = new net.minecraft.core.Holder.Reference(); holder.horizonBind(p2, p0, p1); return holder;".to_owned();
    }
    if class.starts_with("net/minecraft/core/Holder$") {
        match (method, ret) {
            ("value", _) => return "return horizonValue;".to_owned(),
            ("unwrapKey", "java.util.Optional") => {
                return "return java.util.Optional.ofNullable(horizonKey);".to_owned();
            }
            ("isBound", "boolean") => return "return horizonValue != null;".to_owned(),
            ("is", "boolean") if descriptor.contains("ResourceKey") => {
                return "return java.util.Objects.equals(horizonKey, p0);".to_owned();
            }
            ("is", "boolean") if descriptor.contains("Holder") => {
                return "return this == p0;".to_owned();
            }
            ("kind", _) => return "return null;".to_owned(),
            ("tags", _) if ret == "java.util.stream.Stream" => {
                return "return java.util.stream.Stream.empty();".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/core/Holder" {
        match (method, ret) {
            ("value", _) => return "return null;".to_owned(),
            ("unwrapKey", "java.util.Optional") => {
                return "return java.util.Optional.empty();".to_owned();
            }
            ("isBound", "boolean") | ("is", "boolean") => {
                return "return false;".to_owned();
            }
            ("kind", _) => return "return null;".to_owned(),
            ("tags", _) if ret == "java.util.stream.Stream" => {
                return "return java.util.stream.Stream.empty();".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/resources/ResourceKey"
        && matches!(method, "create" | "createRegistryKey")
        && ret == "net.minecraft.resources.ResourceKey"
    {
        if method == "create" {
            return "return new ResourceKey(p0 == null ? new net.minecraft.resources.Identifier() : p0.identifier(), p1);".to_owned();
        }
        return "return new ResourceKey(new net.minecraft.resources.Identifier(), p0);".to_owned();
    }
    if class == "net/minecraft/resources/ResourceKey"
        && method == "identifier"
        && ret == "net.minecraft.resources.Identifier"
    {
        return "return horizonIdentifier;".to_owned();
    }
    if class == "net/minecraft/resources/ResourceKey" {
        if method == "registry" && ret == "net.minecraft.resources.Identifier" {
            return "return horizonRegistryName;".to_owned();
        }
        if method == "location" && ret == "net.minecraft.resources.Identifier" {
            return "return horizonIdentifier;".to_owned();
        }
        if method == "registryKey" && ret == "net.minecraft.resources.ResourceKey" {
            return "return this;".to_owned();
        }
        if method == "isFor" && ret == "boolean" {
            return "return p0 instanceof ResourceKey other && horizonRegistryName.equals(other.horizonRegistryName);".to_owned();
        }
        if method == "toString" && ret == "java.lang.String" {
            return "return horizonRegistryName + \" / \" + horizonIdentifier;".to_owned();
        }
        if method == "hashCode" && ret == "int" {
            return "return java.util.Objects.hash(horizonRegistryName, horizonIdentifier);"
                .to_owned();
        }
        if method == "equals" && ret == "boolean" {
            return "return p0 instanceof ResourceKey other && horizonRegistryName.equals(other.horizonRegistryName) && horizonIdentifier.equals(other.horizonIdentifier);".to_owned();
        }
    }
    if class == "net/minecraft/world/entity/ai/attributes/RangedAttribute"
        && method == "setSyncable"
    {
        return "return this;".to_owned();
    }
    if class == "net/minecraft/world/effect/MobEffect"
        && matches!(method, "addAttributeModifier" | "setBlendDuration")
    {
        return "return this;".to_owned();
    }
    if class == "net/minecraft/world/level/block/state/BlockBehaviour$Properties"
        && ret == "net.minecraft.world.level.block.state.BlockBehaviour.Properties"
    {
        if method == "of" {
            return "return new Properties();".to_owned();
        }
        return "return this;".to_owned();
    }
    if class == "net/minecraft/world/level/block/state/StateDefinition" && method == "any" {
        return "return new net.minecraft.world.level.block.state.BlockState();".to_owned();
    }
    if class == "net/minecraft/world/level/block/Block" {
        if method == "asItem" && ret == "net.minecraft.world.item.Item" {
            return "return new net.minecraft.world.item.Item();".to_owned();
        }
        if method == "getStateDefinition"
            && ret == "net.minecraft.world.level.block.state.StateDefinition"
        {
            return "return stateDefinition;".to_owned();
        }
        if method == "defaultBlockState"
            && ret == "net.minecraft.world.level.block.state.BlockState"
        {
            return "return new net.minecraft.world.level.block.state.BlockState();".to_owned();
        }
    }
    if class == "net/minecraft/world/item/Item$Properties"
        && ret == "net.minecraft.world.item.Item.Properties"
    {
        return "return this;".to_owned();
    }
    if class == "net/minecraft/world/item/ItemStack" {
        match (method, ret) {
            ("getCount", "int") => return "return horizonCount;".to_owned(),
            ("setCount", "void") => {
                return "this.horizonCount = p0;".to_owned();
            }
            ("isEmpty", "boolean") => {
                return "return horizonCount <= 0;".to_owned();
            }
            ("getItem", "net.minecraft.world.item.Item") => {
                return "return horizonItem;".to_owned();
            }
            ("copy", "net.minecraft.world.item.ItemStack")
            | ("copyWithCount", "net.minecraft.world.item.ItemStack") => {
                let count = if method == "copyWithCount" {
                    "p0"
                } else {
                    "horizonCount"
                };
                return format!(
                    "net.minecraft.world.item.ItemStack value = new net.minecraft.world.item.ItemStack(horizonItem, {count}); value.horizonComponents.putAll(horizonComponents); return value;"
                );
            }
            ("grow", "void") => {
                return "this.horizonCount += p0;".to_owned();
            }
            ("shrink", "void") => {
                return "this.horizonCount -= p0;".to_owned();
            }
            ("is", "boolean") if descriptor == "(Lnet/minecraft/world/item/Item;)Z" => {
                return "return horizonItem == p0;".to_owned();
            }
            // Data components are keyed by their identity-bearing type.  The
            // façade keeps the common typed operations after erasure; Java's
            // call site inserts the cast required by the generic API.
            ("get", "java.lang.Object")
                if descriptor
                    == "(Lnet/minecraft/core/component/DataComponentType;)Ljava/lang/Object;" =>
            {
                return "return horizonComponents.get(p0);".to_owned();
            }
            ("getOrDefault", "java.lang.Object")
                if descriptor
                    == "(Lnet/minecraft/core/component/DataComponentType;Ljava/lang/Object;)Ljava/lang/Object;" =>
            {
                return "Object value = horizonComponents.get(p0); return value == null ? p1 : value;"
                    .to_owned();
            }
            ("set", "java.lang.Object")
                if descriptor
                    == "(Lnet/minecraft/core/component/DataComponentType;Ljava/lang/Object;)Ljava/lang/Object;" =>
            {
                return "return horizonComponents.put(p0, p1);".to_owned();
            }
            ("remove", "java.lang.Object")
                if descriptor
                    == "(Lnet/minecraft/core/component/DataComponentType;)Ljava/lang/Object;" =>
            {
                return "return horizonComponents.remove(p0);".to_owned();
            }
            ("has", "boolean") | ("hasNonDefault", "boolean")
                if descriptor == "(Lnet/minecraft/core/component/DataComponentType;)Z" =>
            {
                return "return horizonComponents.containsKey(p0);".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/nbt/CompoundTag" {
        match method {
            "putString" if descriptor == "(Ljava/lang/String;Ljava/lang/String;)V" => {
                return "horizonValues.put(p0, p1);".to_owned();
            }
            "putInt" if descriptor == "(Ljava/lang/String;I)V" => {
                return "horizonValues.put(p0, p1);".to_owned();
            }
            "putBoolean" if descriptor == "(Ljava/lang/String;Z)V" => {
                return "horizonValues.put(p0, p1);".to_owned();
            }
            "putLong" if descriptor == "(Ljava/lang/String;J)V" => {
                return "horizonValues.put(p0, p1);".to_owned();
            }
            _ if matches!(method, "putByte" | "putShort" | "putFloat" | "putDouble")
                && matches!(
                    descriptor,
                    "(Ljava/lang/String;B)V"
                        | "(Ljava/lang/String;S)V"
                        | "(Ljava/lang/String;F)V"
                        | "(Ljava/lang/String;D)V"
                ) =>
            {
                return "horizonValues.put(p0, p1);".to_owned();
            }
            "put"
                if descriptor
                    == "(Ljava/lang/String;Lnet/minecraft/nbt/Tag;)Lnet/minecraft/nbt/Tag;" =>
            {
                return "Object previous = horizonValues.put(p0, p1); return previous instanceof net.minecraft.nbt.Tag ? (net.minecraft.nbt.Tag) previous : null;".to_owned();
            }
            "getString" if descriptor == "(Ljava/lang/String;)Ljava/lang/String;" => {
                return "return horizonValues.get(p0) instanceof String ? (String) horizonValues.get(p0) : \"\";".to_owned();
            }
            "getInt" if descriptor == "(Ljava/lang/String;)I" => {
                return "return horizonValues.get(p0) instanceof Integer ? ((Integer) horizonValues.get(p0)).intValue() : 0;".to_owned();
            }
            "getBoolean" if descriptor == "(Ljava/lang/String;)Z" => {
                return "return horizonValues.get(p0) instanceof Boolean && ((Boolean) horizonValues.get(p0)).booleanValue();".to_owned();
            }
            "getLong" if descriptor == "(Ljava/lang/String;)J" => {
                return "return horizonValues.get(p0) instanceof Long ? ((Long) horizonValues.get(p0)).longValue() : 0L;".to_owned();
            }
            "getByte" if descriptor == "(Ljava/lang/String;)B" => {
                return "return horizonValues.get(p0) instanceof Number ? ((Number) horizonValues.get(p0)).byteValue() : 0;".to_owned();
            }
            "getShort" if descriptor == "(Ljava/lang/String;)S" => {
                return "return horizonValues.get(p0) instanceof Number ? ((Number) horizonValues.get(p0)).shortValue() : 0;".to_owned();
            }
            "getFloat" if descriptor == "(Ljava/lang/String;)F" => {
                return "return horizonValues.get(p0) instanceof Number ? ((Number) horizonValues.get(p0)).floatValue() : 0.0f;".to_owned();
            }
            "getDouble" if descriptor == "(Ljava/lang/String;)D" => {
                return "return horizonValues.get(p0) instanceof Number ? ((Number) horizonValues.get(p0)).doubleValue() : 0.0d;".to_owned();
            }
            "get" if descriptor == "(Ljava/lang/String;)Lnet/minecraft/nbt/Tag;" => {
                return "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.Tag ? (net.minecraft.nbt.Tag) value : null;".to_owned();
            }
            "getCompound"
                if descriptor == "(Ljava/lang/String;)Lnet/minecraft/nbt/CompoundTag;" =>
            {
                return "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.CompoundTag ? (net.minecraft.nbt.CompoundTag) value : new net.minecraft.nbt.CompoundTag();".to_owned();
            }
            "getList" if descriptor == "(Ljava/lang/String;I)Lnet/minecraft/nbt/ListTag;" => {
                return "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.ListTag ? (net.minecraft.nbt.ListTag) value : new net.minecraft.nbt.ListTag();".to_owned();
            }
            "contains" if descriptor == "(Ljava/lang/String;)Z" => {
                return "return horizonValues.containsKey(p0);".to_owned();
            }
            "contains" if descriptor == "(Ljava/lang/String;I)Z" => {
                return "return horizonValues.containsKey(p0);".to_owned();
            }
            "isEmpty" if descriptor == "()Z" => {
                return "return horizonValues.isEmpty();".to_owned();
            }
            "size" if descriptor == "()I" => {
                return "return horizonValues.size();".to_owned();
            }
            "getAllKeys" if descriptor == "()Ljava/util/Set;" => {
                return "return horizonValues.keySet();".to_owned();
            }
            "remove" if descriptor == "(Ljava/lang/String;)Lnet/minecraft/nbt/Tag;" => {
                return "Object previous = horizonValues.remove(p0); return previous instanceof net.minecraft.nbt.Tag ? (net.minecraft.nbt.Tag) previous : null;".to_owned();
            }
            "copy" if descriptor == "()Lnet/minecraft/nbt/CompoundTag;" => {
                return "net.minecraft.nbt.CompoundTag value = new net.minecraft.nbt.CompoundTag(); value.horizonValues.putAll(horizonValues); return value;".to_owned();
            }
            "merge"
                if descriptor
                    == "(Lnet/minecraft/nbt/CompoundTag;)Lnet/minecraft/nbt/CompoundTag;" =>
            {
                return "if (p0 != null) horizonValues.putAll(p0.horizonValues); return this;"
                    .to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/nbt/ListTag" {
        match method {
            "add" if descriptor == "(Lnet/minecraft/nbt/Tag;)Z" => return "return horizonValues.add(p0);".to_owned(),
            "add" if descriptor == "(ILnet/minecraft/nbt/Tag;)V" => return "horizonValues.add(p0, p1);".to_owned(),
            "get" if descriptor == "(I)Lnet/minecraft/nbt/Tag;" => return "return horizonValues.get(p0);".to_owned(),
            "size" if descriptor == "()I" => return "return horizonValues.size();".to_owned(),
            "isEmpty" if descriptor == "()Z" => return "return horizonValues.isEmpty();".to_owned(),
            "getCompound" if descriptor == "(I)Lnet/minecraft/nbt/CompoundTag;" => return "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.CompoundTag ? (net.minecraft.nbt.CompoundTag) value : new net.minecraft.nbt.CompoundTag();".to_owned(),
            "getString" if descriptor == "(I)Ljava/lang/String;" => return "Object value = horizonValues.get(p0); return value instanceof String ? (String) value : value == null ? \"\" : value.toString();".to_owned(),
            _ => {}
        }
    }
    if class == "net/minecraft/world/item/CreativeModeTab$Builder" {
        if ret == "net.minecraft.world.item.CreativeModeTab.Builder" {
            return "return this;".to_owned();
        }
        if method == "build" && ret == "net.minecraft.world.item.CreativeModeTab" {
            return "return new net.minecraft.world.item.CreativeModeTab();".to_owned();
        }
    }
    if class == "net/minecraft/world/item/CreativeModeTab"
        && method == "builder"
        && ret == "net.minecraft.world.item.CreativeModeTab.Builder"
    {
        return "return new CreativeModeTab.Builder();".to_owned();
    }
    if method == "create"
        && matches!(
            class,
            "net/minecraft/world/level/block/state/properties/BooleanProperty"
                | "net/minecraft/world/level/block/state/properties/IntegerProperty"
                | "net/minecraft/world/level/block/state/properties/EnumProperty"
                | "net/minecraft/world/level/block/state/properties/DirectionProperty"
        )
    {
        let simple = class.rsplit('/').next().unwrap_or(class);
        let expected = class.replace('/', ".");
        if ret == expected {
            return format!("return new {simple}();");
        }
    }
    if class == "net/minecraft/sounds/SoundEvent"
        && matches!(method, "createVariableRangeEvent" | "createFixedRangeEvent")
        && ret == "net.minecraft.sounds.SoundEvent"
    {
        return "return new SoundEvent();".to_owned();
    }
    if class == "net/minecraft/network/chat/Component"
        && matches!(
            method,
            "translatable"
                | "literal"
                | "empty"
                | "translatableEscape"
                | "translatableWithFallback"
        )
        && ret == "net.minecraft.network.chat.MutableComponent"
    {
        if method == "literal" {
            return "net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonText = p0; return value;".to_owned();
        }
        if method.starts_with("translatable") {
            return "net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonKey = p0; value.horizonText = p0; return value;".to_owned();
        }
        return "return new net.minecraft.network.chat.MutableComponent();".to_owned();
    }
    if class == "net/minecraft/network/chat/MutableComponent" {
        if method == "getString" && ret == "java.lang.String" {
            return "return horizonText;".to_owned();
        }
        if method == "append"
            && ret == "net.minecraft.network.chat.MutableComponent"
            && descriptor.contains("Lnet/minecraft/network/chat/Component;")
        {
            return "if (p0 != null) horizonText += p0.getString(); return this;".to_owned();
        }
        if method == "append"
            && ret == "net.minecraft.network.chat.MutableComponent"
            && descriptor.contains("Ljava/lang/String;")
        {
            return "if (p0 != null) horizonText += p0; return this;".to_owned();
        }
        if method == "copy"
            && matches!(
                ret,
                "net.minecraft.network.chat.MutableComponent"
                    | "net.minecraft.network.chat.Component"
            )
        {
            return "net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonText = horizonText; value.horizonKey = horizonKey; return value;".to_owned();
        }
    }
    if class == "net/minecraft/world/entity/EntityType$Builder" {
        if method == "of" && ret == "net.minecraft.world.entity.EntityType.Builder" {
            return "return new EntityType.Builder();".to_owned();
        }
        if ret == "net.minecraft.world.entity.EntityType.Builder" {
            return "return this;".to_owned();
        }
        if method == "build" && ret == "net.minecraft.world.entity.EntityType" {
            return "return new net.minecraft.world.entity.EntityType();".to_owned();
        }
    }
    if class == "net/minecraft/world/entity/EntityType"
        && method == "getCategory"
        && ret == "net.minecraft.world.entity.MobCategory"
    {
        return "return net.minecraft.world.entity.MobCategory.MONSTER;".to_owned();
    }
    if class == "net/minecraft/client/KeyMapping$Category"
        && method == "register"
        && ret == "net.minecraft.client.KeyMapping.Category"
    {
        return "return new Category(p0);".to_owned();
    }
    if class == "net/minecraft/client/Minecraft"
        && method == "getInstance"
        && ret == "net.minecraft.client.Minecraft"
    {
        return "return new Minecraft();".to_owned();
    }
    if class == "net/minecraft/client/KeyMapping"
        && method == "getName"
        && ret == "java.lang.String"
    {
        return "return horizonName;".to_owned();
    }
    if class == "net/minecraft/world/item/CreativeModeTab$Builder" {
        if ret == "net.minecraft.world.item.CreativeModeTab.Builder" {
            return "return this;".to_owned();
        }
        if method == "build" && ret == "net.minecraft.world.item.CreativeModeTab" {
            return "return new net.minecraft.world.item.CreativeModeTab();".to_owned();
        }
    }
    if class == "net/minecraft/world/level/block/state/BlockState" {
        match method {
            "setValue" if ret == "net.minecraft.world.level.block.state.BlockState" => {
                return "horizonValues.put(p0, p1); return this;".to_owned();
            }
            "getValue" if ret == "java.lang.Comparable" => {
                return "return (java.lang.Comparable) horizonValues.get(p0);".to_owned();
            }
            "getValue" if ret == "java.lang.Object" => {
                return "return horizonValues.get(p0);".to_owned();
            }
            "hasProperty" if ret == "boolean" => {
                return "return horizonValues.containsKey(p0);".to_owned();
            }
            "getProperties"
                if matches!(
                    ret,
                    "java.util.Collection" | "java.util.Set" | "java.util.Collection<?>"
                ) =>
            {
                return "return horizonValues.keySet();".to_owned();
            }
            _ => {}
        }
    }
    if matches!(
        class,
        "net/minecraft/world/level/Level" | "net/minecraft/server/level/ServerLevel"
    ) {
        match (method, ret, descriptor) {
            ("getGameTime", "long", "()J") => {
                return "return horizonGameTime;".to_owned();
            }
            (
                "dimension",
                "net.minecraft.resources.ResourceKey",
                "()Lnet/minecraft/resources/ResourceKey;",
            ) => {
                return "return horizonDimension;".to_owned();
            }
            (
                "getBlockState",
                "net.minecraft.world.level.block.state.BlockState",
                "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/state/BlockState;",
            ) => {
                return "return horizonBlocks.getOrDefault(horizonBlockKey(p0), new net.minecraft.world.level.block.state.BlockState());".to_owned();
            }
            (
                "getBlockEntity",
                "net.minecraft.world.level.block.entity.BlockEntity",
                "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/entity/BlockEntity;",
            ) => {
                return "return horizonBlockEntities.get(horizonBlockKey(p0));".to_owned();
            }
            ("setBlock", "boolean", descriptor)
                if descriptor.starts_with("(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;") =>
            {
                return "horizonBlocks.put(horizonBlockKey(p0), p1); return true;".to_owned();
            }
            ("setBlockAndUpdate", "boolean", "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;)Z") => {
                return "horizonBlocks.put(horizonBlockKey(p0), p1); return true;".to_owned();
            }
            ("setBlockEntity", "void", "(Lnet/minecraft/world/level/block/entity/BlockEntity;)V") => {
                return "if (p0 != null) horizonBlockEntities.put(horizonBlockKey(p0.getBlockPos()), p0);".to_owned();
            }
            ("removeBlock", "boolean", "(Lnet/minecraft/core/BlockPos;Z)Z") => {
                return "horizonBlocks.remove(horizonBlockKey(p0)); horizonBlockEntities.remove(horizonBlockKey(p0)); return true;".to_owned();
            }
            ("isClientSide", "boolean", "()Z") => {
                return "return false;".to_owned();
            }
            ("getMinBuildHeight", "int", "()I") => {
                return "return -64;".to_owned();
            }
            ("getMaxBuildHeight", "int", "()I") => {
                return "return 320;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/entity/Entity" {
        match (method, ret, descriptor) {
            ("getX", "double", "()D") => return "return horizonX;".to_owned(),
            ("getY", "double", "()D") => return "return horizonY;".to_owned(),
            ("getZ", "double", "()D") => return "return horizonZ;".to_owned(),
            ("setPos", "void", "(DDD)V") => {
                return "horizonX = p0; horizonY = p1; horizonZ = p2;".to_owned();
            }
            ("setPos", "void", "(Lnet/minecraft/world/phys/Vec3;)V") => {
                return "if (p0 != null) { horizonX = p0.x(); horizonY = p0.y(); horizonZ = p0.z(); }"
                    .to_owned();
            }
            ("position", "net.minecraft.world.phys.Vec3", "()Lnet/minecraft/world/phys/Vec3;") => {
                return "return new net.minecraft.world.phys.Vec3(horizonX, horizonY, horizonZ);"
                    .to_owned();
            }
            (
                "getDeltaMovement",
                "net.minecraft.world.phys.Vec3",
                "()Lnet/minecraft/world/phys/Vec3;",
            ) => {
                return "return new net.minecraft.world.phys.Vec3(horizonDeltaX, horizonDeltaY, horizonDeltaZ);"
                    .to_owned();
            }
            ("setDeltaMovement", "void", "(DDD)V") => {
                return "horizonDeltaX = p0; horizonDeltaY = p1; horizonDeltaZ = p2;".to_owned();
            }
            ("setDeltaMovement", "void", "(Lnet/minecraft/world/phys/Vec3;)V") => {
                return "if (p0 != null) { horizonDeltaX = p0.x(); horizonDeltaY = p0.y(); horizonDeltaZ = p0.z(); }"
                    .to_owned();
            }
            ("distanceToSqr", "double", "(Lnet/minecraft/world/entity/Entity;)D") => {
                return "double dx = horizonX - p0.getX(); double dy = horizonY - p0.getY(); double dz = horizonZ - p0.getZ(); return dx * dx + dy * dy + dz * dz;".to_owned();
            }
            ("distanceToSqr", "double", "(DDD)D") => {
                return "double dx = horizonX - p0; double dy = horizonY - p1; double dz = horizonZ - p2; return dx * dx + dy * dy + dz * dz;".to_owned();
            }
            ("distanceTo", "float", "(Lnet/minecraft/world/entity/Entity;)F") => {
                return "return (float) Math.sqrt(distanceToSqr(p0));".to_owned();
            }
            ("blockPosition", "net.minecraft.core.BlockPos", "()Lnet/minecraft/core/BlockPos;") => {
                return "return new net.minecraft.core.BlockPos((int) Math.floor(horizonX), (int) Math.floor(horizonY), (int) Math.floor(horizonZ));".to_owned();
            }
            ("level", "net.minecraft.world.level.Level", "()Lnet/minecraft/world/level/Level;") => {
                return "return horizonLevel;".to_owned();
            }
            ("isAlive", "boolean", "()Z") => return "return true;".to_owned(),
            ("getUUID", "java.util.UUID", "()Ljava/util/UUID;") => {
                return "return horizonUuid;".to_owned();
            }
            (
                "getBoundingBox",
                "net.minecraft.world.phys.AABB",
                "()Lnet/minecraft/world/phys/AABB;",
            ) => {
                return "return horizonBoundingBox;".to_owned();
            }
            ("setBoundingBox", "void", "(Lnet/minecraft/world/phys/AABB;)V") => {
                return "horizonBoundingBox = p0;".to_owned();
            }
            ("isOnGround", "boolean", "()Z") | ("onGround", "boolean", "()Z") => {
                return "return horizonOnGround;".to_owned();
            }
            ("setOnGround", "void", "(Z)V") => return "horizonOnGround = p0;".to_owned(),
            _ => {}
        }
    }
    if class == "net/minecraft/world/entity/Mob" {
        match (method, ret, descriptor) {
            ("isNoAi", "boolean", "()Z") => return "return horizonNoAi;".to_owned(),
            ("setNoAi", "void", "(Z)V") => return "horizonNoAi = p0;".to_owned(),
            ("isAggressive", "boolean", "()Z") => {
                return "return horizonAggressive;".to_owned();
            }
            ("setAggressive", "void", "(Z)V") => {
                return "horizonAggressive = p0;".to_owned();
            }
            (
                "getTarget",
                "net.minecraft.world.entity.LivingEntity",
                "()Lnet/minecraft/world/entity/LivingEntity;",
            ) => return "return horizonTarget;".to_owned(),
            ("setTarget", "void", "(Lnet/minecraft/world/entity/LivingEntity;)V") => {
                return "horizonTarget = p0;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/level/block/entity/BlockEntity" {
        match (method, ret, descriptor) {
            ("getBlockPos", "net.minecraft.core.BlockPos", "()Lnet/minecraft/core/BlockPos;") => {
                return "return horizonBlockPos;".to_owned();
            }
            (
                "getLevel",
                "net.minecraft.world.level.Level",
                "()Lnet/minecraft/world/level/Level;",
            ) => return "return horizonLevel;".to_owned(),
            ("hasLevel", "boolean", "()Z") => return "return horizonLevel != null;".to_owned(),
            (
                "getBlockState",
                "net.minecraft.world.level.block.state.BlockState",
                "()Lnet/minecraft/world/level/block/state/BlockState;",
            ) => return "return horizonBlockState;".to_owned(),
            ("isRemoved", "boolean", "()Z") => return "return horizonRemoved;".to_owned(),
            ("setRemoved", "void", "()V") => return "horizonRemoved = true;".to_owned(),
            ("setChanged", "void", "()V") => return "".to_owned(),
            (
                "getUpdateTag",
                "net.minecraft.nbt.CompoundTag",
                "()Lnet/minecraft/nbt/CompoundTag;",
            ) => return "return horizonData.copy();".to_owned(),
            ("saveAdditional", "void", "(Lnet/minecraft/nbt/CompoundTag;)V")
            | ("saveWithoutMetadata", "void", "(Lnet/minecraft/nbt/CompoundTag;)V") => {
                return "if (p0 != null) p0.merge(horizonData);".to_owned();
            }
            ("load", "void", "(Lnet/minecraft/nbt/CompoundTag;)V")
            | ("handleUpdateTag", "void", "(Lnet/minecraft/nbt/CompoundTag;)V") => {
                return "if (p0 != null) horizonData = p0.copy();".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/entity/LivingEntity" {
        match (method, ret, descriptor) {
            ("getHealth", "float", "()F") => return "return horizonHealth;".to_owned(),
            ("getMaxHealth", "float", "()F") => return "return horizonMaxHealth;".to_owned(),
            ("setHealth", "void", "(F)V") => return "horizonHealth = p0;".to_owned(),
            ("heal", "void", "(F)V") => {
                return "horizonHealth = Math.min(horizonMaxHealth, horizonHealth + p0);"
                    .to_owned();
            }
            ("isDeadOrDying", "boolean", "()Z") => {
                return "return horizonHealth <= 0.0f;".to_owned();
            }
            ("isAlive", "boolean", "()Z") => {
                return "return horizonHealth > 0.0f;".to_owned();
            }
            (
                "getMainHandItem",
                "net.minecraft.world.item.ItemStack",
                "()Lnet/minecraft/world/item/ItemStack;",
            ) => {
                return "return horizonMainHand;".to_owned();
            }
            (
                "getOffhandItem",
                "net.minecraft.world.item.ItemStack",
                "()Lnet/minecraft/world/item/ItemStack;",
            ) => {
                return "return horizonOffhand;".to_owned();
            }
            (
                "getItemInHand",
                "net.minecraft.world.item.ItemStack",
                "(Lnet/minecraft/world/InteractionHand;)Lnet/minecraft/world/item/ItemStack;",
            ) => {
                return "return p0 == net.minecraft.world.InteractionHand.OFF_HAND ? horizonOffhand : horizonMainHand;".to_owned();
            }
            (
                "setItemInHand",
                "void",
                "(Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/item/ItemStack;)V",
            ) => {
                return "if (p0 == net.minecraft.world.InteractionHand.OFF_HAND) horizonOffhand = p1; else horizonMainHand = p1;".to_owned();
            }
            ("getArmorValue", "int", "()I") => return "return horizonArmorValue;".to_owned(),
            ("addEffect", "boolean", "(Lnet/minecraft/world/effect/MobEffectInstance;)Z") => {
                return "if (p0 == null) return false; horizonEffects.put(p0.getEffect(), p0); return true;".to_owned();
            }
            ("hasEffect", "boolean", "(Lnet/minecraft/world/effect/MobEffect;)Z") => {
                return "return horizonEffects.containsKey(p0);".to_owned();
            }
            (
                "getEffect",
                "net.minecraft.world.effect.MobEffectInstance",
                "(Lnet/minecraft/world/effect/MobEffect;)Lnet/minecraft/world/effect/MobEffectInstance;",
            ) => {
                return "return horizonEffects.get(p0);".to_owned();
            }
            ("removeEffect", "boolean", "(Lnet/minecraft/world/effect/MobEffect;)Z") => {
                return "return horizonEffects.remove(p0) != null;".to_owned();
            }
            ("removeAllEffects", "boolean", "()Z") => {
                return "boolean hadEffects = !horizonEffects.isEmpty(); horizonEffects.clear(); return hadEffects;".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/world/effect/MobEffectInstance" {
        match (method, ret, descriptor) {
            (
                "getEffect",
                "net.minecraft.world.effect.MobEffect",
                "()Lnet/minecraft/world/effect/MobEffect;",
            ) => return "return horizonEffect;".to_owned(),
            ("getDuration", "int", "()I") => return "return horizonDuration;".to_owned(),
            ("getAmplifier", "int", "()I") => return "return horizonAmplifier;".to_owned(),
            ("isAmbient", "boolean", "()Z") => return "return horizonAmbient;".to_owned(),
            ("isVisible", "boolean", "()Z") => return "return horizonVisible;".to_owned(),
            ("showIcon", "boolean", "()Z") => return "return horizonShowIcon;".to_owned(),
            ("isInfiniteDuration", "boolean", "()Z") => {
                return "return horizonDuration == -1;".to_owned();
            }
            ("tickDownDuration", "int", "()I") => {
                return "if (horizonDuration > 0) horizonDuration--; return horizonDuration;"
                    .to_owned();
            }
            _ => {}
        }
    }
    if matches!(
        class,
        "net/minecraft/world/entity/player/Player" | "net/minecraft/server/level/ServerPlayer"
    ) {
        match (method, ret, descriptor) {
            ("isCreative", "boolean", "()Z") => {
                return "return horizonCreative;".to_owned();
            }
            ("isSpectator", "boolean", "()Z") => {
                return "return horizonSpectator;".to_owned();
            }
            ("getScoreboardName", "java.lang.String", "()Ljava/lang/String;") => {
                return "return horizonScoreboardName;".to_owned();
            }
            (
                "getName",
                "net.minecraft.network.chat.Component",
                "()Lnet/minecraft/network/chat/Component;",
            )
            | (
                "getDisplayName",
                "net.minecraft.network.chat.Component",
                "()Lnet/minecraft/network/chat/Component;",
            ) => {
                return "return net.minecraft.network.chat.Component.literal(horizonScoreboardName);"
                    .to_owned();
            }
            ("getExperienceLevel", "int", "()I") => {
                return "return horizonExperienceLevel;".to_owned();
            }
            ("getExperienceProgress", "float", "()F") => {
                return "return horizonExperienceProgress;".to_owned();
            }
            (
                "getInventory",
                "net.minecraft.world.entity.player.Inventory",
                "()Lnet/minecraft/world/entity/player/Inventory;",
            ) => {
                return "return new net.minecraft.world.entity.player.Inventory();".to_owned();
            }
            (
                "getAbilities",
                "net.minecraft.world.entity.player.Abilities",
                "()Lnet/minecraft/world/entity/player/Abilities;",
            ) => {
                return "return new net.minecraft.world.entity.player.Abilities();".to_owned();
            }
            _ => {}
        }
    }
    if class == "net/minecraft/resources/Identifier"
        && ret == "net.minecraft.resources.Identifier"
        && matches!(
            method,
            "fromNamespaceAndPath"
                | "withDefaultNamespace"
                | "tryBuild"
                | "bySeparator"
                | "tryBySeparator"
        )
    {
        return match method {
            "fromNamespaceAndPath" | "tryBuild" => "return new Identifier(p0, p1);".to_owned(),
            "withDefaultNamespace" => "return new Identifier(\"minecraft\", p0);".to_owned(),
            _ => "return new Identifier();".to_owned(),
        };
    }
    if class == "net/minecraft/resources/Identifier" {
        return match method {
            "parse" | "tryParse" => "int separator = p0.indexOf(':'); return separator < 0 ? new Identifier(\"minecraft\", p0) : new Identifier(p0.substring(0, separator), p0.substring(separator + 1));".to_owned(),
            "getNamespace" => "return horizonNamespace;".to_owned(),
            "getPath" => "return horizonPath;".to_owned(),
            "withPath" => "return new Identifier(horizonNamespace, (String) p0.apply(horizonPath));".to_owned(),
            "withSuffix" => "return new Identifier(horizonNamespace, horizonPath + p0);".to_owned(),
            "toString" => "return horizonNamespace + \":\" + horizonPath;".to_owned(),
            "toLanguageKey" => "return horizonNamespace + \".\" + horizonPath.replace('/', '.');".to_owned(),
            "toShortString" => "return horizonNamespace.equals(\"minecraft\") ? horizonPath : horizonNamespace + \"/\" + horizonPath;".to_owned(),
            "hashCode" => {
                "return java.util.Objects.hash(horizonNamespace, horizonPath);".to_owned()
            }
            "equals" => {
                "return p0 instanceof Identifier other && horizonNamespace.equals(other.horizonNamespace) && horizonPath.equals(other.horizonPath);".to_owned()
            }
            _ => format!(
                "throw new UnsupportedOperationException(\"horizon stub: {class}#{method}\");"
            ),
        };
    }
    format!("throw new UnsupportedOperationException(\"horizon stub: {class}#{method}\");")
}

fn voxel_shape_placeholder() -> String {
    "return new net.minecraft.world.phys.shapes.VoxelShape(null) {\n".to_owned()
        + "    public it.unimi.dsi.fastutil.doubles.DoubleList getCoords(net.minecraft.core.Direction.Axis axis) {\n"
        + "        return it.unimi.dsi.fastutil.doubles.DoubleLists.EMPTY_LIST;\n"
        + "    }\n"
        + "};"
}

fn runtime_field_default(class: &str, descriptor: &str, fallback: &str) -> String {
    if class == "net/minecraft/world/entity/SpawnPlacementTypes"
        && descriptor == "Lnet/minecraft/world/entity/SpawnPlacementType;"
    {
        return "new net.minecraft.world.entity.SpawnPlacementType() { public boolean isSpawnPositionOk(net.minecraft.world.level.LevelReader level, net.minecraft.core.BlockPos pos, net.minecraft.world.entity.EntityType type) { return true; } }".to_owned();
    }
    if class == "net/minecraft/core/registries/BuiltInRegistries"
        && descriptor == "Lnet/minecraft/core/Registry;"
    {
        return "new net.minecraft.core.Registry() {}".to_owned();
    }
    if class == "net/minecraft/core/registries/BuiltInRegistries"
        && descriptor == "Lnet/minecraft/core/DefaultedRegistry;"
    {
        return "new net.minecraft.core.DefaultedRegistry() {}".to_owned();
    }
    if class == "net/minecraft/core/component/DataComponents"
        && descriptor == "Lnet/minecraft/core/component/DataComponentType;"
    {
        // A distinct object per constant makes it a usable identity key for
        // ItemStack's erased component map (the real type is also identity
        // based), while keeping the generated façade independent of codecs.
        return "new net.minecraft.core.component.DataComponentType() {}".to_owned();
    }
    if class == "net/minecraft/resources/RegistryDataLoader" && descriptor == "Ljava/util/List;" {
        return "new java.util.ArrayList()".to_owned();
    }
    match descriptor {
        "Lnet/minecraft/network/codec/StreamCodec;" => {
            "new net.minecraft.network.codec.StreamCodec() {}".to_owned()
        }
        "Lcom/mojang/serialization/Codec;" | "Lcom/mojang/serialization/codecs/PrimitiveCodec;" => {
            "com.mojang.serialization.Codec.STRING".to_owned()
        }
        "Lcom/mojang/serialization/MapCodec;" => {
            "com.mojang.serialization.Codec.STRING.fieldOf(\"\")".to_owned()
        }
        _ => fallback.to_owned(),
    }
}

fn minimal(name: &str) -> String {
    let (package, simple) = match name.rsplit_once('/') {
        Some((package, simple)) => (Some(package), simple),
        None => (None, name),
    };
    let mut out = String::new();
    if let Some(package) = package {
        let _ = writeln!(out, "package {};", package.replace('/', "."));
    }
    let _ = writeln!(out, "public class {simple} {{ public {simple}() {{}} }}");
    out
}

/// `net/minecraft/world/item/Item$Properties` → `net.minecraft.world.item.Item.Properties`.
fn java_name(name: &str) -> String {
    name.replace(['/', '$'], ".")
}

fn annotation_simple_name(name: &str) -> &str {
    name.trim_start_matches('L')
        .trim_end_matches(';')
        .rsplit('/')
        .next()
        .unwrap_or(name)
}

fn field_is_enum_abi(field: &Declared) -> bool {
    field.is_static() && !field.is_enum_constant()
}

fn default_value_for_java_type(ty: &str) -> String {
    match ty {
        "boolean" => "false".to_owned(),
        "byte" | "char" | "short" | "int" | "long" | "float" | "double" => "0".to_owned(),
        "java.lang.Thread" => "java.lang.Thread.currentThread()".to_owned(),
        "java.lang.String" => "\"\"".to_owned(),
        "java.util.Optional" => "java.util.Optional.empty()".to_owned(),
        "java.util.List" | "java.util.Collection" => "java.util.Collections.emptyList()".to_owned(),
        "java.util.Set" => "java.util.Collections.emptySet()".to_owned(),
        "java.util.Map" => "java.util.Collections.emptyMap()".to_owned(),
        "java.util.Queue" | "java.util.Deque" => "new java.util.ArrayDeque<>()".to_owned(),
        ty if ty.ends_with("[]") && !ty[..ty.len() - 2].ends_with("[]") => {
            format!("new {}[0]", &ty[..ty.len() - 2])
        }
        _ => "null".to_owned(),
    }
}

fn safe_mixin_method(method: &Declared) -> bool {
    let Some(end) = method.descriptor.find(')') else {
        return false;
    };
    let return_descriptor = &method.descriptor[end + 1..];
    matches!(
        return_descriptor.as_bytes().first(),
        Some(b'V' | b'Z' | b'B' | b'C' | b'S' | b'I' | b'J' | b'F' | b'D')
    ) || return_descriptor.starts_with("Ljava/")
}

fn java_type(descriptor: &str) -> String {
    match descriptor {
        "V" => "void".to_owned(),
        "Z" => "boolean".to_owned(),
        "B" => "byte".to_owned(),
        "C" => "char".to_owned(),
        "S" => "short".to_owned(),
        "I" => "int".to_owned(),
        "J" => "long".to_owned(),
        "F" => "float".to_owned(),
        "D" => "double".to_owned(),
        d if d.starts_with('L') => java_name(d[1..d.len() - 1].trim_end_matches(';')),
        d if d.starts_with('[') => format!("{}[]", java_type(&d[1..])),
        d => d.to_owned(),
    }
}

fn default_value(descriptor: &str) -> &'static str {
    match descriptor {
        "Z" => "false",
        "B" | "C" | "S" | "I" | "J" | "F" | "D" => "0",
        _ => "null",
    }
}

fn constructor_default_value(descriptor: &str) -> &'static str {
    match descriptor {
        "B" => "(byte) 0",
        "C" => "'\\0'",
        "S" => "(short) 0",
        "J" => "0L",
        "F" => "0f",
        "D" => "0d",
        _ => default_value(descriptor),
    }
}

/// Every object type a descriptor names: `(Lnet/minecraft/core/BlockPos;)V`.
fn descriptor_types(descriptor: &str) -> Vec<String> {
    let bytes = descriptor.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'L' {
            let start = at + 1;
            while at < bytes.len() && bytes[at] != b';' {
                at += 1;
            }
            out.push(descriptor[start..at].to_owned());
        }
        at += 1;
    }
    out
}

/// `(Ljava/lang/String;I)V` → `String p0, int p1`.
fn parameters(descriptor: &str) -> String {
    let Some(end) = descriptor.find(')') else {
        return String::new();
    };
    let args = &descriptor[1..end];
    let mut out = Vec::new();
    let mut at = 0;
    let bytes = args.as_bytes();
    while at < bytes.len() {
        let start = at;
        while at < bytes.len() && bytes[at] == b'[' {
            at += 1;
        }
        if at >= bytes.len() {
            break;
        }
        if bytes[at] == b'L' {
            while at < bytes.len() && bytes[at] != b';' {
                at += 1;
            }
            at += 1;
        } else {
            at += 1;
        }
        out.push(format!("{} p{}", java_type(&args[start..at]), out.len()));
    }
    out.join(", ")
}

/// `(Ljava/lang/String;I)V` → `(void, String p0, int p1)`.
fn signature(descriptor: &str) -> (String, String) {
    let Some(end) = descriptor.find(')') else {
        return ("void".to_owned(), String::new());
    };
    (java_type(&descriptor[end + 1..]), parameters(descriptor))
}

/// Every jar under a folder (the client's `libraries/`).
pub fn library_jars(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jar") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_become_java() {
        assert_eq!(
            parameters("(Ljava/lang/String;I)V"),
            "java.lang.String p0, int p1"
        );
        assert_eq!(
            parameters("([Lnet/minecraft/core/BlockPos;Z)V"),
            "net.minecraft.core.BlockPos[] p0, boolean p1"
        );
        assert_eq!(
            signature("(I)Lnet/minecraft/world/item/ItemStack;"),
            (
                "net.minecraft.world.item.ItemStack".to_owned(),
                "int p0".to_owned()
            )
        );
        assert_eq!(java_type("[[D"), "double[][]");
        assert_eq!(
            java_name("net/minecraft/world/item/Item$Properties"),
            "net.minecraft.world.item.Item.Properties"
        );
        assert_eq!(
            descriptor_types(
                "(Lnet/minecraft/world/entity/Mob;Lnet/minecraft/world/entity/ConversionParams;)V"
            ),
            vec![
                "net/minecraft/world/entity/Mob".to_owned(),
                "net/minecraft/world/entity/ConversionParams".to_owned()
            ]
        );
    }

    #[test]
    fn neoforge_registration_calls_have_neutral_runtime_values() {
        assert_eq!(
            runtime_body(
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext",
                "get",
                "net.neoforged.fml.javafmlmod.FMLJavaModLoadingContext"
            ),
            "return new FMLJavaModLoadingContext();"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/fml/javafmlmod/FMLJavaModLoadingContext",
                "getModEventBus",
                "net.neoforged.bus.api.IEventBus"
            ),
            "return new net.neoforged.bus.api.IEventBus() {};"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/bus/api/IEventBus",
                "addListener",
                "net.neoforged.bus.api.IEventBus"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/registries/DeferredRegister",
                "create",
                "net.neoforged.neoforge.registries.DeferredRegister"
            ),
            "return new DeferredRegister();"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/registries/DeferredRegister",
                "register",
                "net.neoforged.neoforge.registries.DeferredHolder"
            ),
            "return new net.neoforged.neoforge.registries.DeferredHolder();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/neoforged/neoforge/registries/DeferredHolder",
                "isPresent",
                "boolean",
                "()Z",
            ),
            "return get() != null;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/neoforged/neoforge/registries/DeferredHolder",
                "getOptional",
                "java.util.Optional",
                "()Ljava/util/Optional;",
            ),
            "return java.util.Optional.ofNullable(get());"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/neoforged/neoforge/registries/DeferredHolder",
                "getId",
                "net.minecraft.resources.Identifier",
                "()Lnet/minecraft/resources/Identifier;",
            ),
            "return horizonId;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/registries/RegisterEvent",
                "getRegistryKey",
                "net.minecraft.resources.ResourceKey"
            ),
            "return horizonRegistryKey;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/registries/RegisterEvent",
                "getRegistry",
                "net.minecraft.core.Registry"
            ),
            "return horizonRegistry;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/network/event/RegisterPayloadHandlersEvent",
                "registrar",
                "net.neoforged.neoforge.network.registration.PayloadRegistrar"
            ),
            "return new net.neoforged.neoforge.network.registration.PayloadRegistrar();"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/network/registration/PayloadRegistrar",
                "playToServer",
                "net.neoforged.neoforge.network.registration.PayloadRegistrar"
            ),
            "return this;"
        );
    }

    #[test]
    fn neoforge_payload_registrar_retains_registration_state() {
        let source = include_str!("facade.rs");
        assert!(source.contains("private int horizonRegistrationCount;"));
        assert!(source.contains("horizonRegistrationCount++; return this;"));
        assert!(source.contains("private final PayloadRegistrar horizonRegistrar"));
        assert!(source.contains("return horizonRegistrar;"));
    }

    #[test]
    fn neoforge_capability_values_keep_lazy_optional_shape() {
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/capabilities/CapabilityManager",
                "get",
                "net.neoforged.neoforge.capabilities.Capability"
            ),
            "return new net.neoforged.neoforge.capabilities.Capability();"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/capabilities/Capability",
                "isRegistered",
                "boolean"
            ),
            "return true;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/common/util/LazyOptional",
                "isPresent",
                "boolean"
            ),
            "return horizonValid && horizonGet() != null;"
        );
        assert_eq!(
            runtime_body(
                "net/neoforged/neoforge/common/util/LazyOptional",
                "invalidate",
                "void"
            ),
            "horizonValid = false; horizonValue = null; horizonSupplier = () -> null;"
        );
    }

    #[test]
    fn neoforge_item_handler_contract_is_bounded_and_stateful() {
        // The source shim is intentionally kept beside the façade generator,
        // so assert the ABI policy here: one bounded slot, simulation support,
        // and a mutable counterpart. This guards against silently regressing
        // to shape-only abstract methods when the shim evolves.
        let source = include_str!("facade.rs");
        assert!(source.contains("default int getSlots() { return 1; }"));
        assert!(source.contains("Math.min(incoming, Math.max(0, 64 - value.count))"));
        assert!(source.contains("if (!simulate && amount > 0)"));
        assert!(source.contains("if (!simulate) { value.count -= moved;"));
        assert!(source.contains("default void setStackInSlot(int slot, ItemStack stack)"));
        assert!(source.contains("Collections.synchronizedMap(new java.util.WeakHashMap<>)"));
        assert!(source.contains("if class.name == \"net/minecraft/world/item/ItemStack\""));
        assert!(source.contains("(\"getCount\", \"()I\", \"int\", \"\")"));
    }

    #[test]
    fn stream_codec_factories_are_bootstrap_values() {
        assert_eq!(
            runtime_body(
                "net/minecraft/network/codec/StreamCodec",
                "of",
                "net.minecraft.network.codec.StreamCodec"
            ),
            "return new net.minecraft.network.codec.StreamCodec() {};"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/codec/ByteBufCodecs",
                "idMapper",
                "net.minecraft.network.codec.StreamCodec"
            ),
            "return new net.minecraft.network.codec.StreamCodec() {};"
        );
        assert_eq!(
            runtime_field_default("", "Lnet/minecraft/network/codec/StreamCodec;", "null"),
            "new net.minecraft.network.codec.StreamCodec() {}"
        );
        assert_eq!(
            runtime_field_default("", "Lcom/mojang/serialization/Codec;", "null"),
            "com.mojang.serialization.Codec.STRING"
        );
        assert!(
            runtime_field_default(
                "net/minecraft/world/entity/SpawnPlacementTypes",
                "Lnet/minecraft/world/entity/SpawnPlacementType;",
                "null",
            )
            .contains("isSpawnPositionOk")
        );
        assert_eq!(
            runtime_field_default(
                "net/minecraft/core/registries/BuiltInRegistries",
                "Lnet/minecraft/core/Registry;",
                "null",
            ),
            "new net.minecraft.core.Registry() {}"
        );
        assert_eq!(
            runtime_field_default(
                "net/minecraft/core/component/DataComponents",
                "Lnet/minecraft/core/component/DataComponentType;",
                "null",
            ),
            "new net.minecraft.core.component.DataComponentType() {}"
        );
        assert_eq!(
            runtime_body("net/minecraft/core/Registry", "containsKey", "boolean"),
            "return horizonEntries.containsKey(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/Registry",
                "getOptional",
                "java.util.Optional",
                "(Lnet/minecraft/resources/Identifier;)Ljava/util/Optional;",
            ),
            "return java.util.Optional.ofNullable(horizonEntries.get(p0));"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/DefaultedRegistry",
                "getKey",
                "net.minecraft.resources.Identifier",
                "(Ljava/lang/Object;)Lnet/minecraft/resources/Identifier;",
            ),
            "for (java.util.Map.Entry<net.minecraft.resources.Identifier,Object> e : horizonEntries.entrySet()) if (java.util.Objects.equals(e.getValue(), p0)) return e.getKey(); return null;"
        );
        assert!(
            runtime_body("net/minecraft/world/item/Item", "of", "void")
                .contains("UnsupportedOperationException")
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/resources/Identifier",
                "fromNamespaceAndPath",
                "net.minecraft.resources.Identifier"
            ),
            "return new Identifier(p0, p1);"
        );
        assert!(
            runtime_body(
                "net/minecraft/resources/Identifier",
                "parse",
                "net.minecraft.resources.Identifier"
            )
            .contains("indexOf(':')")
        );
        assert!(
            runtime_body("net/minecraft/network/VarInt", "getByteSize", "int")
                .contains("-268435456")
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/VarInt",
                "hasContinuationBit",
                "boolean"
            ),
            "return (p0 & 0x80) != 0;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/flag/FeatureFlagSet",
                "of",
                "net.minecraft.world.flag.FeatureFlagSet"
            ),
            "return new FeatureFlagSet();"
        );
        assert_eq!(
            runtime_body("net/minecraft/world/item/ItemStack", "getCount", "int"),
            "return horizonCount;"
        );
        assert_eq!(
            runtime_body("net/minecraft/world/item/ItemStack", "isEmpty", "boolean"),
            "return horizonCount <= 0;"
        );
        assert!(
            runtime_body(
                "net/minecraft/world/item/ItemStack",
                "copy",
                "net.minecraft.world.item.ItemStack"
            )
            .contains("horizonCount")
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/util/StringRepresentable",
                "fromEnum",
                "net.minecraft.util.StringRepresentable.EnumCodec"
            ),
            "return new EnumCodec();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/component/DataComponentType",
                "builder",
                "net.minecraft.core.component.DataComponentType.Builder"
            ),
            "return new DataComponentType.Builder();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/component/DataComponentType$Builder",
                "cacheEncoding",
                "net.minecraft.core.component.DataComponentType.Builder"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/Registry",
                "register",
                "java.lang.Object"
            ),
            "return p2;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/entity/ai/attributes/RangedAttribute",
                "setSyncable",
                "net.minecraft.world.entity.ai.attributes.Attribute"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/Registry",
                "registerForHolder",
                "net.minecraft.core.Holder.Reference"
            ),
            "horizonEntries.put(p1, p2); horizonKeys.put(p1, p0); net.minecraft.core.Holder.Reference holder = new net.minecraft.core.Holder.Reference(); holder.horizonBind(p2, p0, p1); return holder;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/effect/MobEffect",
                "addAttributeModifier",
                "net.minecraft.world.effect.MobEffect"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/block/state/BlockBehaviour$Properties",
                "of",
                "net.minecraft.world.level.block.state.BlockBehaviour.Properties"
            ),
            "return new Properties();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/block/state/StateDefinition",
                "any",
                "net.minecraft.world.level.block.state.StateHolder"
            ),
            "return new net.minecraft.world.level.block.state.BlockState();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/resources/ResourceKey",
                "create",
                "net.minecraft.resources.ResourceKey"
            ),
            "return new ResourceKey(p0 == null ? new net.minecraft.resources.Identifier() : p0.identifier(), p1);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/resources/ResourceKey",
                "identifier",
                "net.minecraft.resources.Identifier"
            ),
            "return horizonIdentifier;"
        );
        assert_eq!(
            runtime_body("net/minecraft/resources/ResourceKey", "hashCode", "int"),
            "return java.util.Objects.hash(horizonRegistryName, horizonIdentifier);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/DefaultedRegistry",
                "getKey",
                "net.minecraft.resources.Identifier"
            ),
            "for (java.util.Map.Entry<net.minecraft.resources.Identifier,Object> e : horizonEntries.entrySet()) if (java.util.Objects.equals(e.getValue(), p0)) return e.getKey(); return null;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/DefaultedRegistry",
                "getResourceKey",
                "java.util.Optional"
            ),
            "net.minecraft.resources.Identifier id = getKey(p0); return id == null ? java.util.Optional.empty() : java.util.Optional.ofNullable(horizonKeys.get(id));"
        );
        assert_eq!(
            runtime_field_default(
                "net/minecraft/resources/RegistryDataLoader",
                "Ljava/util/List;",
                "null"
            ),
            "new java.util.ArrayList()"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/resources/RegistryValidator",
                "none",
                "net.minecraft.resources.RegistryValidator"
            ),
            "return new net.minecraft.resources.RegistryValidator() {};"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/tags/TagKey",
                "create",
                "net.minecraft.tags.TagKey"
            ),
            "return new net.minecraft.tags.TagKey(p0, p1);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/tags/TagKey",
                "location",
                "net.minecraft.resources.Identifier"
            ),
            "return horizonLocation;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/tags/TagKey",
                "registry",
                "net.minecraft.resources.ResourceKey"
            ),
            "return horizonRegistry;"
        );
        assert_eq!(
            runtime_body("net/minecraft/tags/TagKey", "isFor", "boolean"),
            "return java.util.Objects.equals(horizonRegistry, p0);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/entity/EntityType",
                "getCategory",
                "net.minecraft.world.entity.MobCategory"
            ),
            "return net.minecraft.world.entity.MobCategory.MONSTER;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/client/KeyMapping$Category",
                "register",
                "net.minecraft.client.KeyMapping.Category"
            ),
            "return new Category(p0);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/client/Minecraft",
                "getInstance",
                "net.minecraft.client.Minecraft"
            ),
            "return new Minecraft();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/client/KeyMapping",
                "getName",
                "java.lang.String"
            ),
            "return horizonName;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/item/Item$Properties",
                "useBlockDescriptionPrefix",
                "net.minecraft.world.item.Item.Properties"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/block/state/properties/BooleanProperty",
                "create",
                "net.minecraft.world.level.block.state.properties.BooleanProperty"
            ),
            "return new BooleanProperty();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/chat/Component",
                "translatable",
                "net.minecraft.network.chat.MutableComponent"
            ),
            "net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonKey = p0; value.horizonText = p0; return value;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/chat/Component",
                "literal",
                "net.minecraft.network.chat.MutableComponent"
            ),
            "net.minecraft.network.chat.MutableComponent value = new net.minecraft.network.chat.MutableComponent(); value.horizonText = p0; return value;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/chat/MutableComponent",
                "getString",
                "java.lang.String"
            ),
            "return horizonText;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/Vec3i",
                "offsetCodec",
                "com.mojang.serialization.Codec"
            ),
            "return com.mojang.serialization.Codec.STRING;"
        );
        assert_eq!(
            runtime_body("net/minecraft/core/Vec3i", "getX", "int"),
            "return horizonX;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/Direction",
                "getOpposite",
                "net.minecraft.core.Direction"
            ),
            "return switch (this) { case DOWN -> UP; case UP -> DOWN; case NORTH -> SOUTH; case SOUTH -> NORTH; case WEST -> EAST; case EAST -> WEST; };"
        );
        assert_eq!(
            runtime_body("net/minecraft/core/Direction", "getStepX", "int"),
            "return this == WEST ? -1 : this == EAST ? 1 : 0;"
        );
        assert_eq!(
            runtime_body("net/minecraft/core/Direction$Axis", "isVertical", "boolean"),
            "return this == Y;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/core/BlockPos",
                "above",
                "net.minecraft.core.BlockPos"
            ),
            "return new net.minecraft.core.BlockPos(horizonX, horizonY + 1, horizonZ);"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/levelgen/structure/placement/StructurePlacement",
                "codec",
                "com.mojang.serialization.MapCodec"
            ),
            "return com.mojang.serialization.Codec.STRING.fieldOf(\"\");"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/levelgen/structure/placement/StructurePlacement",
                "codec",
                "com.mojang.serialization.Codec"
            ),
            "return com.mojang.serialization.Codec.STRING;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/resources/FileToIdConverter",
                "json",
                "net.minecraft.resources.FileToIdConverter"
            ),
            "return new FileToIdConverter();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type",
                "id",
                "net.minecraft.resources.Identifier"
            ),
            "return horizonId;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/network/codec/StreamCodec",
                "map",
                "net.minecraft.network.codec.StreamCodec"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/item/CreativeModeTab$Builder",
                "title",
                "net.minecraft.world.item.CreativeModeTab.Builder"
            ),
            "return this;"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/block/Block",
                "asItem",
                "net.minecraft.world.item.Item"
            ),
            "return new net.minecraft.world.item.Item();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/entity/EntityType$Builder",
                "of",
                "net.minecraft.world.entity.EntityType.Builder"
            ),
            "return new EntityType.Builder();"
        );
        assert_eq!(
            runtime_body(
                "net/minecraft/world/level/block/Block",
                "column",
                "net.minecraft.world.phys.shapes.VoxelShape"
            ),
            voxel_shape_placeholder()
        );
        assert!(voxel_shape_placeholder().contains("DoubleLists.EMPTY_LIST"));
    }

    #[test]
    fn emitted_methods_keep_game_visibility() {
        let class = Class {
            name: "net/minecraft/Test".to_owned(),
            declared: vec![Declared {
                name: "value".to_owned(),
                descriptor: "()V".to_owned(),
                access: 0x0001,
                kind: MemberKind::Method,
            }],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([("value".to_owned(), "()V".to_owned())])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("public void value()"));
    }

    #[test]
    fn item_stack_components_keep_common_typed_operations() {
        let component = "Lnet/minecraft/core/component/DataComponentType;";
        let object = "Ljava/lang/Object;";
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "get",
                "java.lang.Object",
                &format!("({component}){object}"),
            ),
            "return horizonComponents.get(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "getOrDefault",
                "java.lang.Object",
                &format!("({component}{object}){object}"),
            ),
            "Object value = horizonComponents.get(p0); return value == null ? p1 : value;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "set",
                "java.lang.Object",
                &format!("({component}{object}){object}"),
            ),
            "return horizonComponents.put(p0, p1);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "remove",
                "java.lang.Object",
                &format!("({component}){object}"),
            ),
            "return horizonComponents.remove(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "has",
                "boolean",
                &format!("({component})Z"),
            ),
            "return horizonComponents.containsKey(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/item/ItemStack",
                "hasNonDefault",
                "boolean",
                &format!("({component})Z"),
            ),
            "return horizonComponents.containsKey(p0);"
        );
    }

    #[test]
    fn item_stack_facade_emits_component_storage_and_copy() {
        let class = Class {
            name: "net/minecraft/world/item/ItemStack".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "get".to_owned(),
                    descriptor:
                        "(Lnet/minecraft/core/component/DataComponentType;)Ljava/lang/Object;"
                            .to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "copy".to_owned(),
                    descriptor: "()Lnet/minecraft/world/item/ItemStack;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "get".to_owned(),
                    "(Lnet/minecraft/core/component/DataComponentType;)Ljava/lang/Object;"
                        .to_owned(),
                ),
                (
                    "copy".to_owned(),
                    "()Lnet/minecraft/world/item/ItemStack;".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonComponents = new java.util.HashMap<>()"));
        assert!(source.contains("return horizonComponents.get(p0);"));
        assert!(source.contains("horizonComponents.putAll(horizonComponents)"));
    }

    #[test]
    fn vec3i_facade_preserves_coordinates_for_value_queries() {
        let class = Class {
            name: "net/minecraft/core/Vec3i".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "<init>".to_owned(),
                    descriptor: "(III)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getX".to_owned(),
                    descriptor: "()I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([("getX".to_owned(), "()I".to_owned())])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected int horizonX;"));
        assert!(source.contains("this.horizonX = p0;"));
        assert!(source.contains("public int getX() { return horizonX; }"));
    }

    #[test]
    fn vec3_facade_preserves_coordinates_and_vector_arithmetic() {
        let class = Class {
            name: "net/minecraft/world/phys/Vec3".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "<init>".to_owned(),
                    descriptor: "(DDD)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "x".to_owned(),
                    descriptor: "()D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "add".to_owned(),
                    descriptor: "(Lnet/minecraft/world/phys/Vec3;)Lnet/minecraft/world/phys/Vec3;"
                        .to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("x".to_owned(), "()D".to_owned()),
                (
                    "add".to_owned(),
                    "(Lnet/minecraft/world/phys/Vec3;)Lnet/minecraft/world/phys/Vec3;".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected double horizonX;"));
        assert!(source.contains("this.horizonX = p0;"));
        assert!(source.contains("public double x() { return horizonX; }"));
        assert!(source.contains("horizonX + p0.x()"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/phys/Vec3",
                "scale",
                "net.minecraft.world.phys.Vec3",
                "(D)Lnet/minecraft/world/phys/Vec3;",
            ),
            "return new net.minecraft.world.phys.Vec3(horizonX * p0, horizonY * p0, horizonZ * p0);"
        );
    }

    #[test]
    fn block_state_facade_retains_property_values() {
        let class = Class {
            name: "net/minecraft/world/level/block/state/BlockState".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "setValue".to_owned(),
                    descriptor: "(Lnet/minecraft/world/level/block/state/properties/Property;Ljava/lang/Comparable;)Lnet/minecraft/world/level/block/state/BlockState;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getValue".to_owned(),
                    descriptor: "(Lnet/minecraft/world/level/block/state/properties/Property;)Ljava/lang/Comparable;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "hasProperty".to_owned(),
                    descriptor: "(Lnet/minecraft/world/level/block/state/properties/Property;)Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "setValue".to_owned(),
                    "(Lnet/minecraft/world/level/block/state/properties/Property;Ljava/lang/Comparable;)Lnet/minecraft/world/level/block/state/BlockState;".to_owned(),
                ),
                (
                    "getValue".to_owned(),
                    "(Lnet/minecraft/world/level/block/state/properties/Property;)Ljava/lang/Comparable;".to_owned(),
                ),
                (
                    "hasProperty".to_owned(),
                    "(Lnet/minecraft/world/level/block/state/properties/Property;)Z".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("IdentityHashMap<Object, Object> horizonValues"));
        assert!(source.contains("horizonValues.put(p0, p1); return this;"));
        assert!(source.contains("return (java.lang.Comparable) horizonValues.get(p0);"));
        assert!(source.contains("return horizonValues.containsKey(p0);"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/state/BlockState",
                "getProperties",
                "java.util.Collection",
                "()Ljava/util/Collection;",
            ),
            "return horizonValues.keySet();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/state/BlockState",
                "setValue",
                "net.minecraft.world.level.block.state.BlockState",
                "(Lnet/minecraft/world/level/block/state/properties/Property;Ljava/lang/Comparable;)Lnet/minecraft/world/level/block/state/BlockState;",
            ),
            "horizonValues.put(p0, p1); return this;"
        );
    }

    #[test]
    fn mutable_component_facade_preserves_literal_text() {
        let class = Class {
            name: "net/minecraft/network/chat/MutableComponent".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![Declared {
                name: "getString".to_owned(),
                descriptor: "()Ljava/lang/String;".to_owned(),
                access: 0x0001,
                kind: MemberKind::Method,
            }],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([(
                "getString".to_owned(),
                "()Ljava/lang/String;".to_owned(),
            )])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected String horizonText = \"\";"));
        assert!(source.contains("public java.lang.String getString() { return horizonText; }"));
    }

    #[test]
    fn tag_key_facade_preserves_registry_and_location() {
        let class = Class {
            name: "net/minecraft/tags/TagKey".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "location".to_owned(),
                    descriptor: "()Lnet/minecraft/resources/Identifier;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "registry".to_owned(),
                    descriptor: "()Lnet/minecraft/resources/ResourceKey;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "location".to_owned(),
                    "()Lnet/minecraft/resources/Identifier;".to_owned(),
                ),
                (
                    "registry".to_owned(),
                    "()Lnet/minecraft/resources/ResourceKey;".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonRegistry"));
        assert!(source.contains("horizonLocation"));
        assert!(source.contains("public TagKey(net.minecraft.resources.ResourceKey registry"));
        assert!(source.contains(
            "public net.minecraft.resources.Identifier location() { return horizonLocation; }"
        ));
    }

    #[test]
    fn resource_key_facade_exposes_location_and_registry_identity() {
        let class = Class {
            name: "net/minecraft/resources/ResourceKey".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "location".to_owned(),
                    descriptor: "()Lnet/minecraft/resources/Identifier;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isFor".to_owned(),
                    descriptor: "(Lnet/minecraft/resources/ResourceKey;)Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "location".to_owned(),
                    "()Lnet/minecraft/resources/Identifier;".to_owned(),
                ),
                (
                    "isFor".to_owned(),
                    "(Lnet/minecraft/resources/ResourceKey;)Z".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonRegistryName"));
        assert!(source.contains("horizonIdentifier"));
        assert!(source.contains(
            "public net.minecraft.resources.Identifier location() { return horizonIdentifier; }"
        ));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/resources/ResourceKey",
                "isFor",
                "boolean",
                "(Lnet/minecraft/resources/ResourceKey;)Z",
            ),
            "return p0 instanceof ResourceKey other && horizonRegistryName.equals(other.horizonRegistryName);"
        );
    }

    #[test]
    fn compound_tag_facade_preserves_scalar_values() {
        let class = Class {
            name: "net/minecraft/nbt/CompoundTag".to_owned(),
            superclass: "net/minecraft/nbt/Tag".to_owned(),
            declared: vec![
                Declared {
                    name: "putString".to_owned(),
                    descriptor: "(Ljava/lang/String;Ljava/lang/String;)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getString".to_owned(),
                    descriptor: "(Ljava/lang/String;)Ljava/lang/String;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getInt".to_owned(),
                    descriptor: "(Ljava/lang/String;)I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "contains".to_owned(),
                    descriptor: "(Ljava/lang/String;)Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getAllKeys".to_owned(),
                    descriptor: "()Ljava/util/Set;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "remove".to_owned(),
                    descriptor: "(Ljava/lang/String;)Lnet/minecraft/nbt/Tag;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "copy".to_owned(),
                    descriptor: "()Lnet/minecraft/nbt/CompoundTag;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "putString".to_owned(),
                    "(Ljava/lang/String;Ljava/lang/String;)V".to_owned(),
                ),
                (
                    "getString".to_owned(),
                    "(Ljava/lang/String;)Ljava/lang/String;".to_owned(),
                ),
                ("getInt".to_owned(), "(Ljava/lang/String;)I".to_owned()),
                ("contains".to_owned(), "(Ljava/lang/String;)Z".to_owned()),
                ("getAllKeys".to_owned(), "()Ljava/util/Set;".to_owned()),
                (
                    "remove".to_owned(),
                    "(Ljava/lang/String;)Lnet/minecraft/nbt/Tag;".to_owned(),
                ),
                (
                    "copy".to_owned(),
                    "()Lnet/minecraft/nbt/CompoundTag;".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected java.util.Map<String, Object> horizonValues"));
        assert!(source.contains("horizonValues.put(p0, p1);"));
        assert!(source.contains("horizonValues.containsKey(p0)"));
        assert!(source.contains("horizonValues.remove(p0);"));
        assert!(source.contains("value.horizonValues.putAll(horizonValues)"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "put",
                "net.minecraft.nbt.Tag",
                "(Ljava/lang/String;Lnet/minecraft/nbt/Tag;)Lnet/minecraft/nbt/Tag;",
            ),
            "Object previous = horizonValues.put(p0, p1); return previous instanceof net.minecraft.nbt.Tag ? (net.minecraft.nbt.Tag) previous : null;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getCompound",
                "net.minecraft.nbt.CompoundTag",
                "(Ljava/lang/String;)Lnet/minecraft/nbt/CompoundTag;",
            ),
            "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.CompoundTag ? (net.minecraft.nbt.CompoundTag) value : new net.minecraft.nbt.CompoundTag();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "isEmpty",
                "boolean",
                "()Z",
            ),
            "return horizonValues.isEmpty();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getString",
                "java.lang.String",
                "(Ljava/lang/String;)Ljava/lang/String;",
            ),
            "return horizonValues.get(p0) instanceof String ? (String) horizonValues.get(p0) : \"\";"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getInt",
                "int",
                "(Ljava/lang/String;)I",
            ),
            "return horizonValues.get(p0) instanceof Integer ? ((Integer) horizonValues.get(p0)).intValue() : 0;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getAllKeys",
                "java.util.Set",
                "()Ljava/util/Set;",
            ),
            "return horizonValues.keySet();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "remove",
                "net.minecraft.nbt.Tag",
                "(Ljava/lang/String;)Lnet/minecraft/nbt/Tag;",
            ),
            "Object previous = horizonValues.remove(p0); return previous instanceof net.minecraft.nbt.Tag ? (net.minecraft.nbt.Tag) previous : null;"
        );
    }

    #[test]
    fn compound_and_list_tags_preserve_numeric_and_list_contracts() {
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "putDouble",
                "void",
                "(Ljava/lang/String;D)V",
            ),
            "horizonValues.put(p0, p1);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getFloat",
                "float",
                "(Ljava/lang/String;)F",
            ),
            "return horizonValues.get(p0) instanceof Number ? ((Number) horizonValues.get(p0)).floatValue() : 0.0f;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/CompoundTag",
                "getList",
                "net.minecraft.nbt.ListTag",
                "(Ljava/lang/String;I)Lnet/minecraft/nbt/ListTag;",
            ),
            "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.ListTag ? (net.minecraft.nbt.ListTag) value : new net.minecraft.nbt.ListTag();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/ListTag",
                "add",
                "boolean",
                "(Lnet/minecraft/nbt/Tag;)Z",
            ),
            "return horizonValues.add(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/nbt/ListTag",
                "getCompound",
                "net.minecraft.nbt.CompoundTag",
                "(I)Lnet/minecraft/nbt/CompoundTag;",
            ),
            "Object value = horizonValues.get(p0); return value instanceof net.minecraft.nbt.CompoundTag ? (net.minecraft.nbt.CompoundTag) value : new net.minecraft.nbt.CompoundTag();"
        );
        let class = Class {
            name: "net/minecraft/nbt/ListTag".to_owned(),
            superclass: "net/minecraft/nbt/Tag".to_owned(),
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::new()),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected java.util.List<net.minecraft.nbt.Tag> horizonValues"));
    }

    #[test]
    fn holder_reference_facade_keeps_bound_value_and_key_shape() {
        let class = Class {
            name: "net/minecraft/core/Holder$Reference".to_owned(),
            superclass: "net/minecraft/core/Holder".to_owned(),
            declared: vec![
                Declared {
                    name: "value".to_owned(),
                    descriptor: "()Ljava/lang/Object;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "unwrapKey".to_owned(),
                    descriptor: "()Ljava/util/Optional;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("value".to_owned(), "()Ljava/lang/Object;".to_owned()),
                ("unwrapKey".to_owned(), "()Ljava/util/Optional;".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected Object horizonValue;"));
        assert!(source.contains("void horizonBind(Object value"));
        assert!(source.contains("return horizonValue;"));
        assert!(source.contains("return java.util.Optional.ofNullable(horizonKey);"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/Registry",
                "registerForHolder",
                "net.minecraft.core.Holder.Reference",
                "(Lnet/minecraft/core/Registry;Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;Ljava/lang/Object;)Lnet/minecraft/core/Holder$Reference;",
            ),
            "p0.horizonEntries.put(p2, p3); p0.horizonKeys.put(p2, p1); net.minecraft.core.Holder.Reference holder = new net.minecraft.core.Holder.Reference(); holder.horizonBind(p3, p1, p2); return holder;"
        );
    }

    #[test]
    fn holder_set_facade_keeps_bounded_membership_shape() {
        assert_eq!(
            runtime_body_with_descriptor("net/minecraft/core/HolderSet", "size", "int", "()I",),
            "return 0;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/HolderSet",
                "isEmpty",
                "boolean",
                "()Z",
            ),
            "return true;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/HolderSet",
                "contains",
                "boolean",
                "(Lnet/minecraft/core/Holder;)Z",
            ),
            "return false;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/HolderSet",
                "stream",
                "java.util.stream.Stream",
                "()Ljava/util/stream/Stream;",
            ),
            "return java.util.stream.Stream.empty();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/HolderSet",
                "direct",
                "net.minecraft.core.HolderSet",
                "(Lnet/minecraft/core/Holder;)Lnet/minecraft/core/HolderSet;",
            ),
            "return new net.minecraft.core.HolderSet() {};"
        );
    }

    #[test]
    fn registry_resource_key_lookup_keeps_registration_identity() {
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/Registry",
                "register",
                "java.lang.Object",
                "(Lnet/minecraft/core/Registry;Lnet/minecraft/resources/ResourceKey;Ljava/lang/Object;)Ljava/lang/Object;",
            ),
            "p0.horizonEntries.put(p1.location(), p2); p0.horizonKeys.put(p1.location(), p1); return p2;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/Registry",
                "register",
                "java.lang.Object",
                "(Lnet/minecraft/resources/ResourceKey;Lnet/minecraft/resources/Identifier;Ljava/lang/Object;)Ljava/lang/Object;",
            ),
            "horizonEntries.put(p1, p2); horizonKeys.put(p1, p0); return p2;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/core/Registry",
                "getResourceKey",
                "java.util.Optional",
                "(Ljava/lang/Object;)Ljava/util/Optional;",
            ),
            "net.minecraft.resources.Identifier id = getKey(p0); return id == null ? java.util.Optional.empty() : java.util.Optional.ofNullable(horizonKeys.get(id));"
        );
    }

    #[test]
    fn level_facade_preserves_common_world_read_contract() {
        let class = Class {
            name: "net/minecraft/world/level/Level".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "getGameTime".to_owned(),
                    descriptor: "()J".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "dimension".to_owned(),
                    descriptor: "()Lnet/minecraft/resources/ResourceKey;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getBlockState".to_owned(),
                    descriptor:
                        "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/state/BlockState;"
                            .to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getBlockEntity".to_owned(),
                    descriptor:
                        "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/entity/BlockEntity;"
                            .to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setBlock".to_owned(),
                    descriptor:
                        "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;I)Z"
                            .to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setBlockEntity".to_owned(),
                    descriptor:
                        "(Lnet/minecraft/world/level/block/entity/BlockEntity;)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isClientSide".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("getGameTime".to_owned(), "()J".to_owned()),
                (
                    "dimension".to_owned(),
                    "()Lnet/minecraft/resources/ResourceKey;".to_owned(),
                ),
                (
                    "getBlockState".to_owned(),
                    "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/state/BlockState;"
                        .to_owned(),
                ),
                (
                    "getBlockEntity".to_owned(),
                    "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/entity/BlockEntity;"
                        .to_owned(),
                ),
                (
                    "setBlock".to_owned(),
                    "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;I)Z"
                        .to_owned(),
                ),
                (
                    "setBlockEntity".to_owned(),
                    "(Lnet/minecraft/world/level/block/entity/BlockEntity;)V".to_owned(),
                ),
                ("isClientSide".to_owned(), "()Z".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected long horizonGameTime;"));
        assert!(source.contains("horizonDimension"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/Level",
                "getGameTime",
                "long",
                "()J"
            ),
            "return horizonGameTime;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/Level",
                "getBlockState",
                "net.minecraft.world.level.block.state.BlockState",
                "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/state/BlockState;",
            ),
            "return horizonBlocks.getOrDefault(horizonBlockKey(p0), new net.minecraft.world.level.block.state.BlockState());"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/Level",
                "setBlock",
                "boolean",
                "(Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;I)Z",
            ),
            "horizonBlocks.put(horizonBlockKey(p0), p1); return true;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/Level",
                "setBlockEntity",
                "void",
                "(Lnet/minecraft/world/level/block/entity/BlockEntity;)V",
            ),
            "if (p0 != null) horizonBlockEntities.put(horizonBlockKey(p0.getBlockPos()), p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/Level",
                "getBlockEntity",
                "net.minecraft.world.level.block.entity.BlockEntity",
                "(Lnet/minecraft/core/BlockPos;)Lnet/minecraft/world/level/block/entity/BlockEntity;",
            ),
            "return horizonBlockEntities.get(horizonBlockKey(p0));"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/server/level/ServerLevel",
                "isClientSide",
                "boolean",
                "()Z",
            ),
            "return false;"
        );
    }

    #[test]
    fn block_entity_facade_preserves_position_state_and_lifecycle() {
        let class = Class {
            name: "net/minecraft/world/level/block/entity/BlockEntity".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "getBlockPos".to_owned(),
                    descriptor: "()Lnet/minecraft/core/BlockPos;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getLevel".to_owned(),
                    descriptor: "()Lnet/minecraft/world/level/Level;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "hasLevel".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getBlockState".to_owned(),
                    descriptor: "()Lnet/minecraft/world/level/block/state/BlockState;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isRemoved".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setRemoved".to_owned(),
                    descriptor: "()V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getUpdateTag".to_owned(),
                    descriptor: "()Lnet/minecraft/nbt/CompoundTag;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "saveAdditional".to_owned(),
                    descriptor: "(Lnet/minecraft/nbt/CompoundTag;)V".to_owned(),
                    access: 0x0004,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "load".to_owned(),
                    descriptor: "(Lnet/minecraft/nbt/CompoundTag;)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                (
                    "getBlockPos".to_owned(),
                    "()Lnet/minecraft/core/BlockPos;".to_owned(),
                ),
                (
                    "getLevel".to_owned(),
                    "()Lnet/minecraft/world/level/Level;".to_owned(),
                ),
                ("hasLevel".to_owned(), "()Z".to_owned()),
                (
                    "getBlockState".to_owned(),
                    "()Lnet/minecraft/world/level/block/state/BlockState;".to_owned(),
                ),
                ("isRemoved".to_owned(), "()Z".to_owned()),
                ("setRemoved".to_owned(), "()V".to_owned()),
                (
                    "getUpdateTag".to_owned(),
                    "()Lnet/minecraft/nbt/CompoundTag;".to_owned(),
                ),
                (
                    "saveAdditional".to_owned(),
                    "(Lnet/minecraft/nbt/CompoundTag;)V".to_owned(),
                ),
                (
                    "load".to_owned(),
                    "(Lnet/minecraft/nbt/CompoundTag;)V".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonBlockPos"));
        assert!(source.contains("horizonBlockState"));
        assert!(source.contains("horizonData"));
        assert!(source.contains("horizonRemoved"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "getBlockPos",
                "net.minecraft.core.BlockPos",
                "()Lnet/minecraft/core/BlockPos;",
            ),
            "return horizonBlockPos;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "getBlockState",
                "net.minecraft.world.level.block.state.BlockState",
                "()Lnet/minecraft/world/level/block/state/BlockState;",
            ),
            "return horizonBlockState;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "getUpdateTag",
                "net.minecraft.nbt.CompoundTag",
                "()Lnet/minecraft/nbt/CompoundTag;",
            ),
            "return horizonData.copy();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "saveAdditional",
                "void",
                "(Lnet/minecraft/nbt/CompoundTag;)V",
            ),
            "if (p0 != null) p0.merge(horizonData);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "load",
                "void",
                "(Lnet/minecraft/nbt/CompoundTag;)V",
            ),
            "if (p0 != null) horizonData = p0.copy();"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/level/block/entity/BlockEntity",
                "setRemoved",
                "void",
                "()V",
            ),
            "horizonRemoved = true;"
        );
    }

    #[test]
    fn entity_facade_preserves_common_position_and_identity_contract() {
        let class = Class {
            name: "net/minecraft/world/entity/Entity".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "getX".to_owned(),
                    descriptor: "()D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setPos".to_owned(),
                    descriptor: "(DDD)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getDeltaMovement".to_owned(),
                    descriptor: "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setDeltaMovement".to_owned(),
                    descriptor: "(DDD)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "position".to_owned(),
                    descriptor: "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "blockPosition".to_owned(),
                    descriptor: "()Lnet/minecraft/core/BlockPos;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "level".to_owned(),
                    descriptor: "()Lnet/minecraft/world/level/Level;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getUUID".to_owned(),
                    descriptor: "()Ljava/util/UUID;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getBoundingBox".to_owned(),
                    descriptor: "()Lnet/minecraft/world/phys/AABB;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setBoundingBox".to_owned(),
                    descriptor: "(Lnet/minecraft/world/phys/AABB;)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isOnGround".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("getX".to_owned(), "()D".to_owned()),
                ("setPos".to_owned(), "(DDD)V".to_owned()),
                (
                    "getDeltaMovement".to_owned(),
                    "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
                ),
                ("setDeltaMovement".to_owned(), "(DDD)V".to_owned()),
                (
                    "position".to_owned(),
                    "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
                ),
                (
                    "blockPosition".to_owned(),
                    "()Lnet/minecraft/core/BlockPos;".to_owned(),
                ),
                (
                    "level".to_owned(),
                    "()Lnet/minecraft/world/level/Level;".to_owned(),
                ),
                ("getUUID".to_owned(), "()Ljava/util/UUID;".to_owned()),
                (
                    "getBoundingBox".to_owned(),
                    "()Lnet/minecraft/world/phys/AABB;".to_owned(),
                ),
                (
                    "setBoundingBox".to_owned(),
                    "(Lnet/minecraft/world/phys/AABB;)V".to_owned(),
                ),
                ("isOnGround".to_owned(), "()Z".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected double horizonX;"));
        assert!(source.contains("protected double horizonDeltaX;"));
        assert!(source.contains("protected net.minecraft.world.level.Level horizonLevel"));
        assert!(source.contains("horizonBoundingBox"));
        assert!(source.contains("horizonOnGround"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "getX",
                "double",
                "()D"
            ),
            "return horizonX;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "setPos",
                "void",
                "(DDD)V",
            ),
            "horizonX = p0; horizonY = p1; horizonZ = p2;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "setPos",
                "void",
                "(Lnet/minecraft/world/phys/Vec3;)V",
            ),
            "if (p0 != null) { horizonX = p0.x(); horizonY = p0.y(); horizonZ = p0.z(); }"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "getDeltaMovement",
                "net.minecraft.world.phys.Vec3",
                "()Lnet/minecraft/world/phys/Vec3;",
            ),
            "return new net.minecraft.world.phys.Vec3(horizonDeltaX, horizonDeltaY, horizonDeltaZ);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "setDeltaMovement",
                "void",
                "(DDD)V",
            ),
            "horizonDeltaX = p0; horizonDeltaY = p1; horizonDeltaZ = p2;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "distanceToSqr",
                "double",
                "(Lnet/minecraft/world/entity/Entity;)D",
            ),
            "double dx = horizonX - p0.getX(); double dy = horizonY - p0.getY(); double dz = horizonZ - p0.getZ(); return dx * dx + dy * dy + dz * dz;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "distanceTo",
                "float",
                "(Lnet/minecraft/world/entity/Entity;)F",
            ),
            "return (float) Math.sqrt(distanceToSqr(p0));"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "position",
                "net.minecraft.world.phys.Vec3",
                "()Lnet/minecraft/world/phys/Vec3;",
            ),
            "return new net.minecraft.world.phys.Vec3(horizonX, horizonY, horizonZ);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "blockPosition",
                "net.minecraft.core.BlockPos",
                "()Lnet/minecraft/core/BlockPos;",
            ),
            "return new net.minecraft.core.BlockPos((int) Math.floor(horizonX), (int) Math.floor(horizonY), (int) Math.floor(horizonZ));"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "getUUID",
                "java.util.UUID",
                "()Ljava/util/UUID;",
            ),
            "return horizonUuid;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "getBoundingBox",
                "net.minecraft.world.phys.AABB",
                "()Lnet/minecraft/world/phys/AABB;",
            ),
            "return horizonBoundingBox;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "setBoundingBox",
                "void",
                "(Lnet/minecraft/world/phys/AABB;)V",
            ),
            "horizonBoundingBox = p0;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Entity",
                "isOnGround",
                "boolean",
                "()Z",
            ),
            "return horizonOnGround;"
        );
    }

    #[test]
    fn mob_facade_preserves_ai_flags_and_target() {
        let class = Class {
            name: "net/minecraft/world/entity/Mob".to_owned(),
            superclass: "net/minecraft/world/entity/LivingEntity".to_owned(),
            declared: vec![
                Declared {
                    name: "isNoAi".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "setNoAi".to_owned(),
                    descriptor: "(Z)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isAggressive".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getTarget".to_owned(),
                    descriptor: "()Lnet/minecraft/world/entity/LivingEntity;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("isNoAi".to_owned(), "()Z".to_owned()),
                ("setNoAi".to_owned(), "(Z)V".to_owned()),
                ("isAggressive".to_owned(), "()Z".to_owned()),
                (
                    "getTarget".to_owned(),
                    "()Lnet/minecraft/world/entity/LivingEntity;".to_owned(),
                ),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonNoAi"));
        assert!(source.contains("horizonAggressive"));
        assert!(source.contains("horizonTarget"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Mob",
                "isNoAi",
                "boolean",
                "()Z"
            ),
            "return horizonNoAi;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Mob",
                "setNoAi",
                "void",
                "(Z)V"
            ),
            "horizonNoAi = p0;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/Mob",
                "getTarget",
                "net.minecraft.world.entity.LivingEntity",
                "()Lnet/minecraft/world/entity/LivingEntity;",
            ),
            "return horizonTarget;"
        );
    }

    #[test]
    fn living_entity_facade_preserves_health_and_hand_values() {
        let class = Class {
            name: "net/minecraft/world/entity/LivingEntity".to_owned(),
            superclass: "net/minecraft/world/entity/Entity".to_owned(),
            declared: vec![
                Declared {
                    name: "getHealth".to_owned(),
                    descriptor: "()F".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getMaxHealth".to_owned(),
                    descriptor: "()F".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getMainHandItem".to_owned(),
                    descriptor: "()Lnet/minecraft/world/item/ItemStack;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isDeadOrDying".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "hasEffect".to_owned(),
                    descriptor: "(Lnet/minecraft/world/effect/MobEffect;)Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getEffect".to_owned(),
                    descriptor: "(Lnet/minecraft/world/effect/MobEffect;)Lnet/minecraft/world/effect/MobEffectInstance;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "removeEffect".to_owned(),
                    descriptor: "(Lnet/minecraft/world/effect/MobEffect;)Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("getHealth".to_owned(), "()F".to_owned()),
                ("getMaxHealth".to_owned(), "()F".to_owned()),
                (
                    "getMainHandItem".to_owned(),
                    "()Lnet/minecraft/world/item/ItemStack;".to_owned(),
                ),
                ("isDeadOrDying".to_owned(), "()Z".to_owned()),
                ("hasEffect".to_owned(), "(Lnet/minecraft/world/effect/MobEffect;)Z".to_owned()),
                ("getEffect".to_owned(), "(Lnet/minecraft/world/effect/MobEffect;)Lnet/minecraft/world/effect/MobEffectInstance;".to_owned()),
                ("removeEffect".to_owned(), "(Lnet/minecraft/world/effect/MobEffect;)Z".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected float horizonHealth = 20.0f;"));
        assert!(source.contains("horizonMainHand"));
        assert!(source.contains("horizonEffects"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "getHealth",
                "float",
                "()F",
            ),
            "return horizonHealth;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "setHealth",
                "void",
                "(F)V",
            ),
            "horizonHealth = p0;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "heal",
                "void",
                "(F)V",
            ),
            "horizonHealth = Math.min(horizonMaxHealth, horizonHealth + p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "getMainHandItem",
                "net.minecraft.world.item.ItemStack",
                "()Lnet/minecraft/world/item/ItemStack;",
            ),
            "return horizonMainHand;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "getItemInHand",
                "net.minecraft.world.item.ItemStack",
                "(Lnet/minecraft/world/InteractionHand;)Lnet/minecraft/world/item/ItemStack;",
            ),
            "return p0 == net.minecraft.world.InteractionHand.OFF_HAND ? horizonOffhand : horizonMainHand;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "addEffect",
                "boolean",
                "(Lnet/minecraft/world/effect/MobEffectInstance;)Z",
            ),
            "if (p0 == null) return false; horizonEffects.put(p0.getEffect(), p0); return true;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "isDeadOrDying",
                "boolean",
                "()Z",
            ),
            "return horizonHealth <= 0.0f;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "hasEffect",
                "boolean",
                "(Lnet/minecraft/world/effect/MobEffect;)Z",
            ),
            "return horizonEffects.containsKey(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "getEffect",
                "net.minecraft.world.effect.MobEffectInstance",
                "(Lnet/minecraft/world/effect/MobEffect;)Lnet/minecraft/world/effect/MobEffectInstance;",
            ),
            "return horizonEffects.get(p0);"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "removeEffect",
                "boolean",
                "(Lnet/minecraft/world/effect/MobEffect;)Z",
            ),
            "return horizonEffects.remove(p0) != null;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/LivingEntity",
                "getArmorValue",
                "int",
                "()I",
            ),
            "return horizonArmorValue;"
        );
    }

    #[test]
    fn mob_effect_instance_facade_preserves_duration_and_flags() {
        let class = Class {
            name: "net/minecraft/world/effect/MobEffectInstance".to_owned(),
            declared: vec![
                Declared {
                    name: "getDuration".to_owned(),
                    descriptor: "()I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getAmplifier".to_owned(),
                    descriptor: "()I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isAmbient".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isInfiniteDuration".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("getDuration".to_owned(), "()I".to_owned()),
                ("getAmplifier".to_owned(), "()I".to_owned()),
                ("isAmbient".to_owned(), "()Z".to_owned()),
                ("isInfiniteDuration".to_owned(), "()Z".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("horizonDuration"));
        assert!(source.contains("horizonAmplifier"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/effect/MobEffectInstance",
                "getDuration",
                "int",
                "()I",
            ),
            "return horizonDuration;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/effect/MobEffectInstance",
                "isInfiniteDuration",
                "boolean",
                "()Z",
            ),
            "return horizonDuration == -1;"
        );
    }

    #[test]
    fn player_facade_preserves_identity_and_access_state() {
        let class = Class {
            name: "net/minecraft/world/entity/player/Player".to_owned(),
            superclass: "net/minecraft/world/entity/LivingEntity".to_owned(),
            declared: vec![
                Declared {
                    name: "isCreative".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "isSpectator".to_owned(),
                    descriptor: "()Z".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getScoreboardName".to_owned(),
                    descriptor: "()Ljava/lang/String;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getExperienceLevel".to_owned(),
                    descriptor: "()I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getExperienceProgress".to_owned(),
                    descriptor: "()F".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([
                ("isCreative".to_owned(), "()Z".to_owned()),
                ("isSpectator".to_owned(), "()Z".to_owned()),
                (
                    "getScoreboardName".to_owned(),
                    "()Ljava/lang/String;".to_owned(),
                ),
                ("getExperienceLevel".to_owned(), "()I".to_owned()),
                ("getExperienceProgress".to_owned(), "()F".to_owned()),
            ])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected boolean horizonCreative;"));
        assert!(source.contains("horizonScoreboardName = \"Player\";"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/player/Player",
                "isCreative",
                "boolean",
                "()Z",
            ),
            "return horizonCreative;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/player/Player",
                "getScoreboardName",
                "java.lang.String",
                "()Ljava/lang/String;",
            ),
            "return horizonScoreboardName;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/entity/player/Player",
                "getExperienceProgress",
                "float",
                "()F",
            ),
            "return horizonExperienceProgress;"
        );
    }

    #[test]
    fn aabb_facade_preserves_bounds_and_geometry_queries() {
        let class = Class {
            name: "net/minecraft/world/phys/AABB".to_owned(),
            superclass: "java/lang/Object".to_owned(),
            declared: vec![
                Declared {
                    name: "minX".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "minY".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "minZ".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "maxX".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "maxY".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "maxZ".to_owned(),
                    descriptor: "D".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Field,
                },
                Declared {
                    name: "<init>".to_owned(),
                    descriptor: "(DDDDDD)V".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
                Declared {
                    name: "getCenter".to_owned(),
                    descriptor: "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                },
            ],
            ..Class::default()
        };
        let mut report = Report::default();
        let source = render(
            &class,
            Some(&BTreeSet::from([(
                "getCenter".to_owned(),
                "()Lnet/minecraft/world/phys/Vec3;".to_owned(),
            )])),
            &BTreeMap::new(),
            &BTreeSet::new(),
            None,
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("protected double horizonMinX;"));
        assert!(source.contains("this.horizonMinX = p0;"));
        assert!(source.contains("public net.minecraft.world.phys.Vec3 getCenter()"));
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/phys/AABB",
                "getXsize",
                "double",
                "()D",
            ),
            "return horizonMaxX - horizonMinX;"
        );
        assert_eq!(
            runtime_body_with_descriptor(
                "net/minecraft/world/phys/AABB",
                "inflate",
                "net.minecraft.world.phys.AABB",
                "(D)Lnet/minecraft/world/phys/AABB;",
            ),
            "return new net.minecraft.world.phys.AABB(horizonMinX - p0, horizonMinY - p0, horizonMinZ - p0, horizonMaxX + p0, horizonMaxY + p0, horizonMaxZ + p0);"
        );
    }

    #[test]
    fn accessor_mixin_methods_are_materialized_on_target_stub() {
        let class = Class {
            name: "net/minecraft/Test".to_owned(),
            ..Class::default()
        };
        let mut mixins = BTreeMap::new();
        mixins.insert(
            class.name.clone(),
            vec![MixinImpl {
                interface: "example/TestAccessor".to_owned(),
                methods: vec![Declared {
                    name: "getValue".to_owned(),
                    descriptor: "()I".to_owned(),
                    access: 0x0001,
                    kind: MemberKind::Method,
                }],
            }],
        );
        let mut report = Report::default();
        let source = render(
            &class,
            None,
            &BTreeMap::new(),
            &BTreeSet::from(["example/TestAccessor".to_owned()]),
            mixins.get(&class.name),
            &mut report,
            false,
            &[],
        );
        assert!(source.contains("implements example.TestAccessor"));
        assert!(source.contains("public int getValue() { return 0; }"));
    }

    #[test]
    fn accessor_object_defaults_are_non_null_for_collections() {
        assert_eq!(
            default_value_for_java_type("java.util.List"),
            "java.util.Collections.emptyList()"
        );
        assert_eq!(
            default_value_for_java_type("java.util.Optional"),
            "java.util.Optional.empty()"
        );
        assert_eq!(default_value_for_java_type("int[]"), "new int[0]");
        assert_eq!(default_value_for_java_type("int[][]"), "null");
        assert_eq!(
            default_value_for_java_type("java.lang.Thread"),
            "java.lang.Thread.currentThread()"
        );
        assert!(safe_mixin_method(&Declared {
            name: "getThread".to_owned(),
            descriptor: "()Ljava/lang/Thread;".to_owned(),
            access: 0x0001,
            kind: MemberKind::Method,
        }));
    }

    #[test]
    fn extracts_nested_jars_for_javac_and_cleans_them_up() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("horizon-facade-nested-{stamp}"));
        let outer = root.with_extension("jar");
        let nested_bytes = {
            let mut bytes = std::io::Cursor::new(Vec::new());
            {
                let mut jar = zip::ZipWriter::new(&mut bytes);
                jar.start_file("marker.txt", zip::write::SimpleFileOptions::default())
                    .unwrap();
                std::io::Write::write_all(&mut jar, b"nested").unwrap();
                jar.finish().unwrap();
            }
            bytes.into_inner()
        };
        {
            let file = fs::File::create(&outer).unwrap();
            let mut jar = zip::ZipWriter::new(file);
            jar.start_file(
                "META-INF/jars/inner.jar",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            std::io::Write::write_all(&mut jar, &nested_bytes).unwrap();
            jar.finish().unwrap();
        }
        let extracted = extract_nested_jars(std::slice::from_ref(&outer), &root).unwrap();
        assert_eq!(extracted.jars.len(), 1);
        assert!(extracted.jars[0].is_file());
        drop(extracted);
        assert!(!root.exists());
        let _ = fs::remove_file(outer);
    }
}
