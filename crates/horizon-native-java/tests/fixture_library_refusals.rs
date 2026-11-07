//! Library-shaped refusals: the shapes real config and serialization
//! libraries reach before a mod's entrypoint registers anything.
//!
//! Each probe is ordinary Java compiled at test time; the expected strings are
//! what `java` prints for the same code, not values read back from the
//! interpreter.

use horizon_native_java::{
    ClassFile, ContentBuilderEvent, ExecutionBudget, Host, LinkedClassTable, MethodRef,
    NativeError, NativeJar, StdHost, Value,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use zip::write::SimpleFileOptions;

/// Generic reflection a config library builds its model from (DazzleConf,
/// servercore), and the `TypeToken` subclass idiom (WorldEdit's piston).
const REFLECTION: &str = r##"package demo;

import java.lang.invoke.MethodHandles;
import java.lang.reflect.*;
import java.util.*;

public class Probe {
    public interface Settings {
        Map<String, Integer> limits();
        List<? extends Number> values();
        boolean enabled();
        String[] names();
        default int size() { return 3; }
    }
    public static abstract class Token<T> {
        protected Token() {}
        public Type captured() {
            return ((ParameterizedType) getClass().getGenericSuperclass()).getActualTypeArguments()[0];
        }
    }
    static Class<?> raw(Type type) {
        if (type instanceof Class) return (Class<?>) type;
        if (type instanceof ParameterizedType) return (Class<?>) ((ParameterizedType) type).getRawType();
        if (type instanceof WildcardType) return raw(((WildcardType) type).getUpperBounds()[0]);
        return null;
    }
    static String annotated(AnnotatedType type) {
        StringBuilder out = new StringBuilder(raw(type.getType()).getName());
        if (type instanceof AnnotatedParameterizedType) {
            out.append('<');
            for (AnnotatedType argument : ((AnnotatedParameterizedType) type).getAnnotatedActualTypeArguments()) {
                out.append(annotated(argument)).append(',');
            }
            out.append('>');
        }
        out.append(type.getAnnotations().length);
        return out.toString();
    }
    public static String reflection() throws Exception {
        Method limits = Settings.class.getMethod("limits");
        Method values = Settings.class.getMethod("values");
        Method enabled = Settings.class.getMethod("enabled");
        Method names = Settings.class.getMethod("names");
        Method size = Settings.class.getMethod("size");
        return limits.getReturnType().getName() + "|" + raw(limits.getGenericReturnType()).getName()
            + "|" + annotated(limits.getAnnotatedReturnType())
            + "|" + annotated(values.getAnnotatedReturnType())
            + "|" + enabled.getReturnType().getName()
            + "|" + Modifier.isPublic(enabled.getReturnType().getModifiers())
            + "|" + Modifier.isAbstract(Settings.class.getModifiers())
            + "|" + Modifier.isPublic(String.class.getModifiers())
            + "|" + names.getReturnType().getName()
            + "|" + size.isDefault() + enabled.isDefault()
            + "|" + new Token<List<String>>() {}.captured().getTypeName()
            + "|" + (enabled.getGenericReturnType() == boolean.class);
    }
    public static String collectionCopies() {
        List<String> list = new ArrayList<>(2);
        list.add("a");
        list.add("b");
        Object[] array = ((Collection<String>) list).toArray();
        Object[] copy = array.clone();
        List<Object> out = Collections.unmodifiableList(Arrays.asList(copy));
        return array.length + "|" + out.size() + "|" + out.get(1) + "|" + out.isEmpty();
    }
    // --- library dispatch -------------------------------------------------
    public interface Named { String extension(); }
    public interface Writer extends Named { default boolean canWrite(String name) { return name.endsWith("." + extension()); } }
    public static final class JsonWriter implements Writer { public String extension() { return "json"; } }

    public static String inheritedAbstract() {
        Writer writer = new JsonWriter();
        return writer.extension() + "|" + writer.canWrite("a.json") + "|" + writer.canWrite("a.txt");
    }

    static String caller() { return new Throwable().getStackTrace()[1].getMethodName(); }
    static String outer() { return caller(); }
    public static String stackTrace() {
        StackTraceElement[] frames = new Throwable().getStackTrace();
        return frames[0].getClassName() + "|" + frames[0].getMethodName() + "|" + outer()
            + "|" + Thread.currentThread().getStackTrace()[1].getMethodName();
    }

    public static String paths() {
        return java.nio.file.Paths.get("config", "emotes", "x.json").toString() + "|"
            + java.nio.file.Path.of("", "config").toString() + "|" + java.nio.file.Paths.get("a/", "b").toString();
    }

    public static String classQueries() throws Exception {
        Class<?> module = Class.forName("java.lang.Module");
        boolean missing;
        try { Class.forName("demo.DoesNotExist"); missing = false; } catch (ClassNotFoundException e) { missing = true; }
        return module.getName() + "|" + missing
            + "|" + Modifier.isPublic(int.class.getModifiers()) + Modifier.isFinal(int.class.getModifiers())
            + "|" + Modifier.isInterface(Named.class.getModifiers()) + Modifier.isAbstract(Named.class.getModifiers())
            + "|" + Modifier.isFinal(JsonWriter.class.getModifiers())
            + "|" + int.class.getPackageName() + "|" + String[].class.getPackageName()
            + "|" + Map.class.isInstance(new HashMap<>()) + List.class.isInstance(new HashMap<>())
            + Named.class.isInstance(new JsonWriter()) + int.class.isInstance(1)
            + "|" + Probe.class.getModule().isNamed() + Probe.class.getModule().isExported("demo")
            + String.class.getModule().isExported("java.lang", Probe.class.getModule());
    }

    public static String boxes() {
        Object value = Double.valueOf(0.5);
        Object whole = Long.valueOf(7L);
        Object count = Integer.valueOf(3);
        return (value instanceof Number) + "|" + (value instanceof Double) + "|" + value
            + "|" + (whole instanceof Number) + (whole instanceof Integer) + "|" + whole
            + "|" + (count instanceof Integer) + count;
    }

    public interface Hidden {
        int visible();
        private int helper() { return 2; }
        static int build() { return 1; }
    }
    static String joined(List<String> parts) {
        StringBuilder out = new StringBuilder();
        for (String part : parts) out.append(out.length() == 0 ? "" : ",").append(part);
        return out.toString();
    }
    public static String publicMethods() {
        List<String> names = new ArrayList<>();
        for (Method method : Hidden.class.getMethods()) names.add(method.getName());
        Collections.sort(names);
        List<String> declared = new ArrayList<>();
        for (Method method : Hidden.class.getDeclaredMethods()) declared.add(method.getName());
        Collections.sort(declared);
        return joined(names) + "|" + joined(declared);
    }

    public static String entries() {
        Map.Entry<String, Integer> fixed = new AbstractMap.SimpleImmutableEntry<>("a", 1);
        Map.Entry<String, Integer> open = new AbstractMap.SimpleEntry<>(fixed);
        Integer old = open.setValue(5);
        String immutable;
        try { fixed.setValue(2); immutable = "no"; } catch (UnsupportedOperationException e) { immutable = "uoe"; }
        return fixed.getKey() + fixed.getValue() + "|" + open.getKey() + open.getValue() + "|" + old + "|" + immutable;
    }

    // --- streams ------------------------------------------------------------
    public static final class LittleEndian extends java.io.FilterInputStream {
        public LittleEndian(java.io.InputStream in) { super(in); }
        public int readShortLE() throws java.io.IOException {
            int low = read();
            int high = read();
            if ((low | high) < 0) throw new java.io.EOFException();
            return (high << 8) | low;
        }
        public void readFully(byte[] into) throws java.io.IOException {
            int done = 0;
            while (done < into.length) {
                int n = read(into, done, into.length - done);
                if (n < 0) throw new java.io.EOFException();
                done += n;
            }
        }
    }
    public static String streams() throws Exception {
        byte[] data = {1, 2, (byte) 0xff, 0x7f, 9, 8, 7};
        LittleEndian in = new LittleEndian(new java.io.BufferedInputStream(new java.io.ByteArrayInputStream(data)));
        int first = in.readShortLE();
        int second = in.readShortLE();
        byte[] rest = new byte[2];
        in.readFully(rest);
        int last = in.read();
        int end = in.read();
        String eof;
        try { in.readFully(new byte[1]); eof = "no"; } catch (java.io.EOFException e) { eof = "eof"; }
        byte[] all = new java.io.ByteArrayInputStream(data, 2, 3).readAllBytes();
        return first + "|" + second + "|" + rest[0] + rest[1] + "|" + last + "|" + end + "|" + eof + "|" + all.length + all[0];
    }

    // --- proxies --------------------------------------------------------------
    public interface Config {
        int limit();
        String name();
        boolean enabled();
        default String describe() { return name() + "=" + limit(); }
    }
    public static String proxies() throws Throwable {
        Map<String, Object> values = new HashMap<>();
        values.put("limit", 12);
        values.put("name", "spawn");
        values.put("enabled", Boolean.TRUE);
        List<String> seen = new ArrayList<>();
        InvocationHandler handler = new InvocationHandler() {
            public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
                seen.add(method.getName() + (args == null ? "" : ":" + args.length));
                if (method.getDeclaringClass() == Object.class) {
                    return method.getName().equals("hashCode") ? 42 : "proxy";
                }
                if (method.isDefault()) {
                    return MethodHandles.privateLookupIn(Config.class, MethodHandles.lookup())
                        .unreflectSpecial(method, Config.class).bindTo(proxy).invokeWithArguments(args == null ? new Object[0] : args);
                }
                return values.get(method.getName());
            }
        };
        Config config = (Config) Proxy.newProxyInstance(Probe.class.getClassLoader(), new Class<?>[] {Config.class}, handler);
        return config.limit() + "|" + config.name() + "|" + config.enabled() + "|" + config.describe()
            + "|" + (config instanceof Config) + (config instanceof Proxy) + "|" + config.hashCode()
            + "|" + (Proxy.getInvocationHandler(config) == handler) + "|" + joined(seen);
    }

    public static String missingConfig() {
        try (java.io.BufferedReader reader = java.nio.file.Files.newBufferedReader(java.nio.file.Path.of("config", "absent.json"))) {
            return "opened";
        } catch (java.nio.file.NoSuchFileException e) {
            return "missing:" + e.getMessage();
        } catch (java.io.IOException e) {
            return "io";
        }
    }

    public static void main(String[] args) throws Throwable {
        System.out.println(reflection());
        System.out.println(collectionCopies());
        System.out.println(inheritedAbstract());
        System.out.println(stackTrace());
        System.out.println(paths());
        System.out.println(classQueries());
        System.out.println(boxes());
        System.out.println(publicMethods());
        System.out.println(entries());
        System.out.println(streams());
        System.out.println(proxies());
        System.out.println(missingConfig());
    }
}
"##;

/// Probes over classes the platform provides: Fabric Loader's
/// `ModContainer.getRootPaths()` (voxy lists its own packages through it) and
/// the fastutil maps the game ships (servercore's config defaults). The stubs
/// exist for `javac` only; the jar must reach the host's models. Expected
/// values follow the libraries' documented behaviour: a jar root lists the
/// jar's entries, fastutil maps are `java.util.Map`s with a `0` default.
const PLATFORM: &str = r##"package demo;

import it.unimi.dsi.fastutil.objects.Object2IntOpenHashMap;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.stream.Collectors;
import net.fabricmc.loader.api.FabricLoader;

public class Platform {
    public static final class ConfigA {}
    public static final class ConfigB {}

    public static String rootPaths() throws Exception {
        Path root = FabricLoader.getInstance().getModContainer("libref-fixture").get().getRootPaths().get(0);
        Path own = root.resolve("demo");
        String listed = Files.list(own)
            .map(path -> path.getFileName().toString())
            .filter(name -> name.startsWith("Platform"))
            .sorted()
            .collect(Collectors.joining(","));
        return Files.exists(own) + "|" + Files.isDirectory(own) + "|"
            + Files.isRegularFile(own.resolve("Platform.class")) + "|" + Files.exists(root.resolve("missing"))
            + "|" + listed + "|" + Files.walk(own).count();
    }

    public static String ownRegistry() {
        net.minecraft.resources.ResourceKey<net.minecraft.core.Registry<String>> key =
            net.minecraft.resources.ResourceKey.createRegistryKey(
                net.minecraft.resources.Identifier.fromNamespaceAndPath("demo", "things"));
        net.minecraft.core.Registry<String> built = net.fabricmc.fabric.api.event.registry.FabricRegistryBuilder
            .from(new net.minecraft.core.MappedRegistry<>(key, com.mojang.serialization.Lifecycle.stable(), false))
            .buildAndRegister();
        net.minecraft.resources.Identifier id = built.key().identifier();
        return id.getNamespace() + ":" + id.getPath();
    }

    public static String fastutil() {
        Object2IntOpenHashMap<String> counts = new Object2IntOpenHashMap<>();
        counts.put("a", 3);
        counts.addTo("a", 2);
        counts.addTo("b", 1);
        Map<String, Integer> boxed = counts;
        return counts.getInt("a") + "|" + counts.getInt("missing") + "|" + boxed.get("b")
            + "|" + boxed.size() + "|" + (counts instanceof Map) + "|" + Map.class.isInstance(counts);
    }
}
"##;

const FABRIC_LOADER: &str = r#"package net.fabricmc.loader.api;
public interface FabricLoader {
    static FabricLoader getInstance() { return null; }
    java.util.Optional<ModContainer> getModContainer(String id);
}
"#;

const MOD_CONTAINER: &str = r#"package net.fabricmc.loader.api;
public interface ModContainer {
    java.util.List<java.nio.file.Path> getRootPaths();
}
"#;

/// Just enough of the game's registry types and Fabric's builder for
/// `javac`; the host answers all of them.
const REGISTRY_STUBS: &[(&str, &str)] = &[
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\npublic final class Identifier {\n    public static Identifier fromNamespaceAndPath(String namespace, String path) { return null; }\n    public String getNamespace() { return null; }\n    public String getPath() { return null; }\n}\n",
    ),
    (
        "net/minecraft/resources/ResourceKey.java",
        "package net.minecraft.resources;\npublic final class ResourceKey<T> {\n    public static <T> ResourceKey<net.minecraft.core.Registry<T>> createRegistryKey(Identifier id) { return null; }\n    public Identifier identifier() { return null; }\n}\n",
    ),
    (
        "net/minecraft/core/Registry.java",
        "package net.minecraft.core;\npublic interface Registry<T> { net.minecraft.resources.ResourceKey<? extends Registry<T>> key(); }\n",
    ),
    (
        "net/minecraft/core/WritableRegistry.java",
        "package net.minecraft.core;\npublic interface WritableRegistry<T> extends Registry<T> {}\n",
    ),
    (
        "net/minecraft/core/MappedRegistry.java",
        "package net.minecraft.core;\npublic class MappedRegistry<T> implements WritableRegistry<T> {\n    public MappedRegistry(net.minecraft.resources.ResourceKey<? extends Registry<T>> key, com.mojang.serialization.Lifecycle lifecycle, boolean intrusive) {}\n    public net.minecraft.resources.ResourceKey<? extends Registry<T>> key() { return null; }\n}\n",
    ),
    (
        "com/mojang/serialization/Lifecycle.java",
        "package com.mojang.serialization;\npublic class Lifecycle { public static Lifecycle stable() { return null; } }\n",
    ),
    (
        "net/fabricmc/fabric/api/event/registry/FabricRegistryBuilder.java",
        "package net.fabricmc.fabric.api.event.registry;\npublic final class FabricRegistryBuilder<T, R extends net.minecraft.core.WritableRegistry<T>> {\n    public static <T, R extends net.minecraft.core.WritableRegistry<T>> FabricRegistryBuilder<T, R> from(R registry) { return null; }\n    public R buildAndRegister() { return null; }\n}\n",
    ),
];

const OBJECT2INT: &str = r#"package it.unimi.dsi.fastutil.objects;
public class Object2IntOpenHashMap<K> extends java.util.HashMap<K, Integer> {
    public int put(K key, int value) { return 0; }
    public int getInt(Object key) { return 0; }
    public int addTo(K key, int increment) { return 0; }
}
"#;

/// The Guava surface the game ships: `Preconditions`, `TypeToken`, a
/// `LoadingCache`, a `HashMultimap` and `Suppliers.memoize` (WorldEdit's
/// piston injector, spell_power's memoized defaults). The expected string is
/// what `java` prints with Guava 33.3.1 on the class path; the jar itself
/// carries no Guava, so the stubs below exist for `javac` only.
const GUAVA_PROBE: &str = r##"package demo;

import com.google.common.base.Preconditions;
import com.google.common.base.Suppliers;
import com.google.common.cache.CacheBuilder;
import com.google.common.cache.CacheLoader;
import com.google.common.cache.LoadingCache;
import com.google.common.collect.HashMultimap;
import com.google.common.reflect.TypeToken;
import java.util.List;
import java.util.function.Supplier;

public class Libs {
    static int loads;
    static int supplied;

    public static String guava() throws Exception {
        String checked = Preconditions.checkNotNull("kept");
        String npe;
        try { Preconditions.checkNotNull(null, "missing"); npe = "no"; } catch (NullPointerException e) { npe = e.getMessage(); }
        String iae;
        try { Preconditions.checkArgument(false, "bad"); iae = "no"; } catch (IllegalArgumentException e) { iae = e.getMessage(); }

        TypeToken<String> plain = TypeToken.of(String.class);
        TypeToken<List<String>> list = new TypeToken<List<String>>() {};
        TypeToken<Integer> boxed = TypeToken.of(int.class).wrap();

        LoadingCache<String, Integer> cache = CacheBuilder.newBuilder().weakKeys().maximumSize(10)
            .build(CacheLoader.from(key -> { loads++; return key.length(); }));
        int first = cache.getUnchecked("abc");
        int again = cache.getUnchecked("abc");
        int other = cache.getUnchecked("hello");

        HashMultimap<String, Integer> multi = HashMultimap.create();
        multi.put("a", 1);
        multi.put("a", 2);
        multi.put("a", 1);
        multi.get("b").add(5);

        Supplier<Integer> memo = Suppliers.memoize(() -> ++supplied)::get;
        int m1 = memo.get();
        int m2 = memo.get();

        return checked + "|" + npe + "|" + iae
            + "|" + plain.getRawType().getName() + "|" + list.getRawType().getName() + "|" + list
            + "|" + plain.equals(TypeToken.of(String.class)) + list.equals(plain) + "|" + boxed.getRawType().getName()
            + "|" + first + again + other + "|" + loads
            + "|" + multi.get("a").size() + "|" + multi.size() + "|" + multi.containsKey("b") + multi.containsKey("c")
            + "|" + m1 + m2 + "|" + supplied;
    }

}
"##;

const GUAVA_STUBS: &[(&str, &str)] = &[
    (
        "com/google/common/base/Preconditions.java",
        "package com.google.common.base;\npublic final class Preconditions {\n    public static <T> T checkNotNull(T value) { return value; }\n    public static <T> T checkNotNull(T value, Object message) { return value; }\n    public static void checkArgument(boolean ok, Object message) {}\n}\n",
    ),
    (
        "com/google/common/base/Supplier.java",
        "package com.google.common.base;\npublic interface Supplier<T> extends java.util.function.Supplier<T> { T get(); }\n",
    ),
    (
        "com/google/common/base/Function.java",
        "package com.google.common.base;\npublic interface Function<F, T> extends java.util.function.Function<F, T> { T apply(F input); }\n",
    ),
    (
        "com/google/common/base/Suppliers.java",
        "package com.google.common.base;\npublic final class Suppliers {\n    public static <T> Supplier<T> memoize(Supplier<T> delegate) { return delegate; }\n}\n",
    ),
    (
        "com/google/common/cache/CacheLoader.java",
        "package com.google.common.cache;\npublic abstract class CacheLoader<K, V> {\n    public abstract V load(K key) throws Exception;\n    public static <K, V> CacheLoader<K, V> from(com.google.common.base.Function<K, V> function) { return null; }\n}\n",
    ),
    (
        "com/google/common/cache/LoadingCache.java",
        "package com.google.common.cache;\npublic interface LoadingCache<K, V> { V getUnchecked(K key); }\n",
    ),
    (
        "com/google/common/cache/CacheBuilder.java",
        "package com.google.common.cache;\npublic final class CacheBuilder<K, V> {\n    public static CacheBuilder<Object, Object> newBuilder() { return null; }\n    public CacheBuilder<K, V> weakKeys() { return this; }\n    public CacheBuilder<K, V> maximumSize(long size) { return this; }\n    public <K1 extends K, V1 extends V> LoadingCache<K1, V1> build(CacheLoader<? super K1, V1> loader) { return null; }\n}\n",
    ),
    (
        "com/google/common/collect/HashMultimap.java",
        "package com.google.common.collect;\npublic final class HashMultimap<K, V> {\n    public static <K, V> HashMultimap<K, V> create() { return null; }\n    public boolean put(K key, V value) { return true; }\n    public java.util.Set<V> get(K key) { return null; }\n    public int size() { return 0; }\n    public boolean containsKey(Object key) { return false; }\n}\n",
    ),
    (
        "com/google/common/reflect/TypeToken.java",
        "package com.google.common.reflect;\npublic abstract class TypeToken<T> {\n    protected TypeToken() {}\n    public static <T> TypeToken<T> of(Class<T> type) { return null; }\n    public final Class<? super T> getRawType() { return null; }\n    public final TypeToken<T> wrap() { return this; }\n}\n",
    ),
];

fn javac() -> PathBuf {
    std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| "javac".into())
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

/// One compiled probe jar; its scratch folder goes with it.
struct Fixture {
    root: PathBuf,
    jar: NativeJar,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    /// Compile `sources` and pack the `demo/` classes; anything else is a
    /// stub that stands for a class the platform provides.
    fn build(test: &str, sources: &[(&str, &str)]) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "horizon-native-java-libref-{test}-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let mut fixture = Self {
            root,
            jar: NativeJar::default(),
        };
        let classes = fixture.root.join("classes");
        let mut javac = Command::new(javac());
        javac.args(["--release", "21", "-d"]).arg(&classes);
        for (path, source) in sources {
            let file = fixture.root.join("src").join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, source).unwrap();
            javac.arg(file);
        }
        assert!(javac.status().unwrap().success(), "javac failed for {test}");
        let jar_path = fixture.root.join("probe.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
        zip.start_file("fabric.mod.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(
            br#"{"id":"libref-fixture","version":"1.0","entrypoints":{"main":"demo.Probe"}}"#,
        )
        .unwrap();
        for class in files_under(&classes.join("demo")) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(entry, SimpleFileOptions::default()).unwrap();
            zip.write_all(&fs::read(class).unwrap()).unwrap();
        }
        zip.finish().unwrap();
        fixture.jar = NativeJar::from_path(&jar_path).unwrap();
        fixture
    }

    /// Run `demo.Probe.<method>()` and answer the string it returns.
    fn text(&self, method: &str) -> String {
        self.text_of("demo/Probe", method)
    }

    /// Run `<class>.<method>()` and answer the string it returns.
    fn text_of(&self, class: &str, method: &str) -> String {
        let mut host = self.jar.seed_host(StdHost::new());
        match self
            .jar
            .invoke_method_with_args(class, method, "()Ljava/lang/String;", None, &[], &mut host)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
        {
            Value::Ref(Some(text)) => text,
            other => panic!("{method} did not return a string: {other:?}"),
        }
    }
}

/// One compiled probe per test process would be cheaper, but each test keeps
/// its own scratch folder so a failure names one shape.
fn probe(test: &str, method: &str) -> String {
    Fixture::build(test, &[("demo/Probe.java", REFLECTION)]).text(method)
}

#[test]
fn an_abstract_method_inherited_from_a_superinterface_dispatches_on_the_receiver() {
    // emotecraft's `IWriter.getExtension()`, declared by `ISerializer`.
    assert_eq!(probe("inherited", "inheritedAbstract"), "json|true|false");
}

#[test]
fn stack_traces_list_the_jar_frames_innermost_first() {
    // voxy's logger reads `new Throwable().getStackTrace()[2]`.
    assert_eq!(
        probe("trace", "stackTrace"),
        "demo.Probe|stackTrace|outer|stackTrace"
    );
}

#[test]
fn varargs_paths_join_their_parts() {
    assert_eq!(probe("paths", "paths"), "config/emotes/x.json|config|a/b");
}

#[test]
fn class_queries_answer_modules_modifiers_packages_and_instances() {
    assert_eq!(
        probe("classes", "classQueries"),
        "java.lang.Module|true|truetrue|truetrue|true|java.lang|java.lang\
         |truefalsetruefalse|falsetruetrue"
    );
}

#[test]
fn unboxed_boxes_are_numbers_and_print_their_value() {
    assert_eq!(probe("boxes", "boxes"), "true|true|0.5|truefalse|7|true3");
}

#[test]
fn get_methods_lists_public_members_only() {
    assert_eq!(
        probe("methods", "publicMethods"),
        "build,visible|build,helper,visible"
    );
}

#[test]
fn simple_entries_keep_their_key_and_value() {
    assert_eq!(probe("entries", "entries"), "a1|a5|1|uoe");
}

#[test]
fn filter_streams_read_through_the_stream_they_wrap() {
    // emotecraft's `LittleEndianInputStream` over a `BufferedInputStream`.
    assert_eq!(probe("streams", "streams"), "513|32767|98|7|-1|eof|3-1");
}

#[test]
fn proxies_route_calls_to_their_handler_and_run_default_methods() {
    // DazzleConf's config sections and its default-method handler.
    assert_eq!(
        probe("proxies", "proxies"),
        "12|spawn|true|spawn=12|truetrue|42|true\
         |limit,name,enabled,describe,name,limit,hashCode"
    );
}

fn platform(test: &str, method: &str) -> String {
    let mut sources = vec![
        ("demo/Probe.java", REFLECTION),
        ("demo/Platform.java", PLATFORM),
        ("net/fabricmc/loader/api/FabricLoader.java", FABRIC_LOADER),
        ("net/fabricmc/loader/api/ModContainer.java", MOD_CONTAINER),
        (
            "it/unimi/dsi/fastutil/objects/Object2IntOpenHashMap.java",
            OBJECT2INT,
        ),
    ];
    sources.extend_from_slice(REGISTRY_STUBS);
    Fixture::build(test, &sources).text_of("demo/Platform", method)
}

#[test]
fn a_registry_the_mod_constructs_keeps_its_key_through_the_fabric_builder() {
    // moderner_beta: `FabricRegistryBuilder.from(new MappedRegistry<>(key, ...))`.
    assert_eq!(platform("registry", "ownRegistry"), "demo:things");
}

#[test]
fn a_mod_root_lists_the_jars_own_classes() {
    // `Platform`, `Platform$ConfigA`, `Platform$ConfigB` and the lambda-free
    // rest of `demo/`; `walk` counts the folder itself too.
    let classes = [
        "Platform.class",
        "Platform$ConfigA.class",
        "Platform$ConfigB.class",
    ];
    let mut sorted = classes.to_vec();
    sorted.sort();
    let answer = platform("roots", "rootPaths");
    let (head, walked) = answer.rsplit_once('|').unwrap();
    assert_eq!(head, format!("true|true|true|false|{}", sorted.join(",")));
    // Every `demo/` class plus the folder itself.
    let fixture_classes: usize = walked.parse().unwrap();
    assert!(
        fixture_classes > classes.len(),
        "walk saw {fixture_classes}"
    );
}

#[test]
fn fastutil_primitive_maps_are_maps_with_a_zero_default() {
    assert_eq!(platform("fastutil", "fastutil"), "5|0|1|2|true|true");
}

#[test]
fn guava_preconditions_tokens_caches_multimaps_and_memoized_suppliers() {
    let mut sources = vec![("demo/Libs.java", GUAVA_PROBE)];
    sources.extend_from_slice(GUAVA_STUBS);
    let fixture = Fixture::build("guava", &sources);
    assert_eq!(
        fixture.text_of("demo/Libs", "guava"),
        "kept|missing|bad|java.lang.String|java.util.List|java.util.List<java.lang.String>\
         |truefalse|java.lang.Integer|335|2|2|3|truefalse|11|1"
    );
}

#[test]
fn reading_a_config_file_that_was_never_written_is_no_such_file() {
    // moderner_beta catches the `NoSuchFileException` and writes defaults.
    assert_eq!(
        probe("missing", "missingConfig"),
        "missing:config/absent.json"
    );
}

#[test]
fn a_collection_copied_through_to_array_keeps_its_elements() {
    // DazzleConf's `ImmutableCollections.listOf(Collection)`.
    let fixture = Fixture::build("copies", &[("demo/Probe.java", REFLECTION)]);
    assert_eq!(fixture.text("collectionCopies"), "2|2|b|false");
}

#[test]
fn method_return_types_and_annotated_types_come_from_the_signature() {
    let fixture = Fixture::build("reflection", &[("demo/Probe.java", REFLECTION)]);
    assert_eq!(
        fixture.text("reflection"),
        "java.util.Map|java.util.Map|java.util.Map<java.lang.String0,java.lang.Integer0,>0\
         |java.util.List<java.lang.Number0,>0|boolean|true|true|true|[Ljava.lang.String;\
         |truefalse|java.util.List<java.lang.String>|true"
    );
}

/// Every class one jar holds, skipping any this build cannot parse.
fn classes_of(path: &Path) -> Vec<ClassFile> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut classes = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_owned();
        if !name.ends_with(".class")
            || name.ends_with("module-info.class")
            || name.starts_with("META-INF/versions/")
        {
            continue;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        if let Ok(class) = ClassFile::parse(&bytes) {
            classes.push(class);
        }
    }
    classes
}

/// A host that forwards everything to [`StdHost`] and, when
/// `HORIZON_LIBREF_TRACE` is set, prints each host call with its arguments.
struct Tracing {
    inner: StdHost,
    trace: bool,
}

impl Tracing {
    fn show(&self, r: &MethodRef, receiver: Option<&Value>, args: &[Value], out: &Value) {
        if self.trace {
            eprintln!(
                "host {}#{}{} recv={receiver:?} args={args:?} -> {out:?}",
                r.owner, r.name, r.descriptor
            );
        }
    }
}

impl Host for Tracing {
    fn get_static(&mut self, r: &MethodRef) -> Result<Value, NativeError> {
        let out = Host::get_static(&mut self.inner, r)?;
        self.show(r, None, &[], &out);
        Ok(out)
    }
    fn invoke(&mut self, r: &MethodRef, args: &[Value]) -> Result<Value, NativeError> {
        let out = Host::invoke(&mut self.inner, r, args);
        self.show(r, None, args, out.as_ref().unwrap_or(&Value::Void));
        out
    }
    fn invoke_instance(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        let out = Host::invoke_instance(&mut self.inner, r, receiver, args);
        self.show(
            r,
            Some(receiver),
            args,
            out.as_ref().unwrap_or(&Value::Void),
        );
        out
    }
    fn mutable_collection_values(&self, value: &Value) -> Option<Vec<String>> {
        Host::mutable_collection_values(&self.inner, value)
    }
    fn service_providers(&self, service: &str) -> Vec<String> {
        let out = Host::service_providers(&self.inner, service);
        if self.trace {
            eprintln!("services {service} -> {out:?}");
        }
        out
    }
    fn resource_stream_text(&mut self, marker: &str) -> Option<String> {
        Host::resource_stream_text(&mut self.inner, marker)
    }
    fn resource_stream_bytes(&mut self, marker: &str, limit: Option<usize>) -> Option<Vec<u8>> {
        Host::resource_stream_bytes(&mut self.inner, marker, limit)
    }
    fn note_content_builder(&mut self, event: ContentBuilderEvent) {
        Host::note_content_builder(&mut self.inner, event)
    }
    fn linked_class_table(&self) -> Option<&Arc<LinkedClassTable>> {
        Host::linked_class_table(&self.inner)
    }
    fn models_class(&self, name: &str) -> bool {
        Host::models_class(&self.inner, name)
    }
    fn platform_class_present(&self, name: &str) -> bool {
        Host::platform_class_present(&self.inner, name)
    }
    fn throwable_message(&self, marker: &str) -> Option<String> {
        Host::throwable_message(&self.inner, marker)
    }
    fn record_throwable(&mut self, marker: &str, message: Option<String>) {
        if self.trace {
            eprintln!("throwable {marker} {message:?}");
        }
        Host::record_throwable(&mut self.inner, marker, message)
    }
    fn persistent_class_state(&self) -> (BTreeMap<String, Value>, BTreeSet<String>) {
        Host::persistent_class_state(&self.inner)
    }
    fn store_persistent_class_state(
        &mut self,
        fields: BTreeMap<String, Value>,
        initialized: BTreeSet<String>,
    ) -> Result<(), NativeError> {
        Host::store_persistent_class_state(&mut self.inner, fields, initialized)
    }
    fn host_instance_of(&self, marker: &str, wanted: &str) -> bool {
        Host::host_instance_of(&self.inner, marker, wanted)
    }
    fn empty_mutable_collection(&mut self, kind: &str) -> Option<Value> {
        Host::empty_mutable_collection(&mut self.inner, kind)
    }
}

/// Run one real jar the way the dedicated server does, with optional library
/// jars linked beside it, and print the outcome and the host calls nothing
/// modelled. A diagnosis aid for a refused corpus row; the jars live outside
/// the repository, so the test is ignored by default.
///
/// ```sh
/// HORIZON_LIBREF_JARS=mod.jar:dep.jar \
///   cargo test --profile fast -p horizon-native-java --test fixture_library_refusals \
///   -- --ignored --nocapture probe_a_real_jar
/// ```
#[test]
#[ignore = "reads real mod jars outside the repository"]
fn probe_a_real_jar() {
    let Ok(list) = std::env::var("HORIZON_LIBREF_JARS") else {
        eprintln!("skipping: set HORIZON_LIBREF_JARS=mod.jar[:dep.jar...]");
        return;
    };
    let paths: Vec<PathBuf> = list.split(':').map(PathBuf::from).collect();
    let jar = NativeJar::from_path(&paths[0]).expect("read the mod jar");
    let mut table = LinkedClassTable::default();
    for path in &paths[1..] {
        // A library jar links whole; a bundled one that does not read as a
        // mod jar links class by class, the way the game links nested jars.
        match NativeJar::from_path(path) {
            Ok(dep) => {
                table
                    .link_jar(path.display().to_string(), &dep)
                    .expect("link a library jar");
            }
            Err(_) => {
                table
                    .link_classes(path.display().to_string(), classes_of(path))
                    .expect("link a bundled jar");
            }
        }
    }
    // The dedicated server's order: the first kind the jar declares runs.
    let (kind, entry) = ["server", "main", "neoforge"]
        .into_iter()
        .find_map(|kind| {
            jar.discover_entrypoints(kind)
                .into_iter()
                .next()
                .map(|entry| (kind, entry))
        })
        .expect("a server-side entrypoint");
    let mut host = Tracing {
        inner: jar.seed_host(
            StdHost::new()
                .with_environment_type("SERVER")
                .with_linked_class_table(Arc::new(table)),
        ),
        trace: std::env::var("HORIZON_LIBREF_TRACE").is_ok_and(|value| !value.is_empty()),
    };
    let budget = ExecutionBudget::within(std::time::Duration::from_secs(60));
    let outcome = jar.execute_with_budget(
        &entry.class,
        entry.method.as_deref(),
        kind,
        &mut host,
        &budget,
    );
    match &outcome {
        Ok(report) => println!("ok: {} host calls", report.calls),
        Err(error) => println!("refused: {error}"),
    }
    let mut unknown = host.inner.unknown_host_calls();
    unknown.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (call, count) in unknown.iter().take(60) {
        println!("unknown {count:>5} {call}");
    }
}
