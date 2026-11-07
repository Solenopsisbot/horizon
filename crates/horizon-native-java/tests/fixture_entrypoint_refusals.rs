//! Shapes real entrypoints hit before they reach their first registration.
//!
//! Each test compiles a small synthetic jar whose `main` asks one question the
//! JVM answers in a fixed way (a bound method reference to a JDK method, a
//! reflective superclass walk, ...) and reports the answer through
//! `demo.Probe.check`. `Probe` is left out of the jar, so every call reaches
//! the host and is recorded with its arguments; a check that the interpreter
//! answered wrongly records `false` and an unanswered shape fails the run.

use horizon_native_java::{NativeJar, StdHost, Value};
use std::{collections::BTreeMap, fs, io::Write, path::Path, process::Command};
use zip::write::SimpleFileOptions;

fn javac() -> std::path::PathBuf {
    std::env::var_os("JAVA_HOME")
        .map(|home| std::path::PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| "javac".into())
}

fn files_under(root: &Path) -> Vec<std::path::PathBuf> {
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

const PROBE: &str = r#"package demo;
public final class Probe {
    public static void check(String label, boolean value) {}
}
"#;

/// Compile `sources` (plus `demo.Probe`) and pack every class except the
/// `excluded` ones, which stand for classes the game or a loader provides.
fn build_jar(
    name: &str,
    mut sources: BTreeMap<&str, &str>,
    resources: &[(&str, &str)],
    excluded: &[&str],
) -> std::path::PathBuf {
    sources.insert("demo/Probe.java", PROBE);
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-entrypoint-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let src = root.join("src");
    let classes = root.join("classes");
    fs::create_dir_all(&classes).unwrap();
    let mut paths = Vec::new();
    for (relative, body) in sources {
        let path = src.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        paths.push(path);
    }
    let status = Command::new(javac())
        // 21: pattern-matching `switch` (`SwitchBootstraps.typeSwitch`) is a
        // Java 21 feature, and real 26.3 mods target 21 or later.
        .arg("--release")
        .arg("21")
        .arg("-d")
        .arg(&classes)
        .args(paths)
        .status()
        .unwrap();
    assert!(status.success(), "javac failed for {name}");
    let jar_path = root.join(format!("{name}.jar"));
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
    for (path, body) in resources {
        zip.start_file(*path, SimpleFileOptions::default()).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    for class in files_under(&classes) {
        let entry = class
            .strip_prefix(&classes)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if entry == "demo/Probe.class" || excluded.contains(&entry.as_str()) {
            continue;
        }
        zip.start_file(entry, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(class).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    jar_path
}

fn checks(host: &StdHost) -> BTreeMap<String, bool> {
    host.invocations()
        .iter()
        .filter(|invocation| {
            invocation.method.owner == "demo/Probe" && invocation.method.name == "check"
        })
        .filter_map(|invocation| match invocation.args.as_slice() {
            [Value::Ref(Some(label)), Value::Int(flag)] => Some((label.clone(), *flag != 0)),
            _ => None,
        })
        .collect()
}

/// Run `demo.Main.main` and answer the recorded checks.
fn run(name: &str, sources: BTreeMap<&str, &str>, excluded: &[&str]) -> BTreeMap<String, bool> {
    let jar = NativeJar::from_path(&build_jar(name, sources, &[], excluded)).unwrap();
    let mut host = jar.seed_host(StdHost::new());
    jar.execute("demo.Main", Some("main"), "main", &mut host)
        .unwrap();
    checks(&host)
}

fn all_true(labels: &[&str]) -> BTreeMap<String, bool> {
    labels
        .iter()
        .map(|label| ((*label).to_owned(), true))
        .collect()
}

/// Method references to JDK methods: the bound receiver is a capture, the
/// call's argument follows it, and a static reference takes every value as an
/// argument. CreativeCore registers `Enum.class::isAssignableFrom` as a config
/// type predicate; answering it without the tested class was a refusal.
#[test]
fn jdk_method_references_receive_their_captures_and_arguments() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.function.*;
public final class Main {
    enum Color { RED }
    public static void main() {
        Predicate<Class<?>> isEnum = Enum.class::isAssignableFrom;
        Probe.check("bound-true", isEnum.test(Color.class));
        Probe.check("bound-false", !isEnum.test(String.class));
        Function<String, Integer> parse = Integer::parseInt;
        Probe.check("static", parse.apply("42") == 42);
        Function<String, Integer> length = String::length;
        Probe.check("unbound", length.apply("abcd") == 4);
    }
}
"#,
    );
    assert_eq!(
        run("jdk-refs", sources, &[]),
        all_true(&["bound-false", "bound-true", "static", "unbound"])
    );
}

/// A config scanner walks `getSuperclass()` until it reaches `Object.class`.
/// The jar's own chain must be followed exactly, and the walk must end.
#[test]
fn get_superclass_walks_the_jar_hierarchy_to_object() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
public final class Main {
    static class Base { int a; }
    static class Middle extends Base { int b; }
    static class Leaf extends Middle { int c; }
    static int depth(Class<?> type) {
        if (type.getSuperclass() == Object.class) return 1;
        return 1 + depth(type.getSuperclass());
    }
    public static void main() {
        Probe.check("parent", Leaf.class.getSuperclass() == Middle.class);
        Probe.check("depth", depth(Leaf.class) == 3);
        Probe.check("object-root", Object.class.getSuperclass() == null);
        Probe.check("jdk-ends", String.class.getSuperclass() == Object.class);
    }
}
"#,
    );
    assert_eq!(
        run("superclass", sources, &[]),
        all_true(&["depth", "jdk-ends", "object-root", "parent"])
    );
}

/// Annotation-driven config holders (Carry On's `ConfigLoader`) ask
/// `isAnnotationPresent` on the holder class, then walk its fields and read
/// each field's `@Property`. Runtime-visible annotations answer from the class
/// file; a `CLASS`-retention one stays invisible, as on the JVM.
#[test]
fn annotated_element_queries_answer_classes_fields_and_methods() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Config.java",
        r#"package demo;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
public @interface Config { String value(); }
"#,
    );
    sources.insert(
        "demo/Property.java",
        r#"package demo;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
public @interface Property { String description() default ""; }
"#,
    );
    sources.insert(
        "demo/Hidden.java",
        r#"package demo;
import java.lang.annotation.*;
@Retention(RetentionPolicy.CLASS)
public @interface Hidden {}
"#,
    );
    sources.insert(
        "demo/Holder.java",
        r#"package demo;
@Config("holder") @Hidden
public class Holder {
    @Property(description = "speed") public int speed = 3;
    public int plain;
    @Property public void tick() {}
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.lang.reflect.Field;
public final class Main {
    public static void main() throws Exception {
        Class<?> type = new Holder().getClass();
        Probe.check("class-present", type.isAnnotationPresent(Config.class));
        Probe.check("class-absent", !type.isAnnotationPresent(Property.class));
        Probe.check("class-retention", !type.isAnnotationPresent(Hidden.class));
        Field speed = type.getDeclaredField("speed");
        Probe.check("field-present", speed.isAnnotationPresent(Property.class));
        Probe.check("field-value", "speed".equals(speed.getAnnotation(Property.class).description()));
        Probe.check("field-absent", !type.getDeclaredField("plain").isAnnotationPresent(Property.class));
        Probe.check("field-null", type.getDeclaredField("plain").getAnnotation(Property.class) == null);
        Probe.check("method-present", type.getMethod("tick").isAnnotationPresent(Property.class));
        int annotated = 0;
        for (Field field : type.getDeclaredFields()) {
            if (field.isAnnotationPresent(Property.class)) annotated++;
        }
        Probe.check("walk", annotated == 1);
    }
}
"#,
    );
    assert_eq!(
        run("annotations", sources, &[]),
        all_true(&[
            "class-absent",
            "class-present",
            "class-retention",
            "field-absent",
            "field-null",
            "field-present",
            "field-value",
            "method-present",
            "walk",
        ])
    );
}

/// Multi-loader libraries pick their platform by trying one implementation
/// per loader and keeping the first that answers (Cubes Without Borders'
/// `ModLoader.create(Supplier...)`). The supplied object is a jar subclass, so
/// a virtual call on it must reach the subclass's override, not the abstract
/// base's `throw new UnsupportedOperationException()`.
#[test]
fn supplied_platform_object_dispatches_to_its_override() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Platform.java",
        r#"package demo;
import java.util.function.Supplier;
public abstract class Platform {
    private static final Platform INSTANCE = create(Platform.First::new, Platform.Second::new);
    @SafeVarargs
    private static Platform create(Supplier<Platform>... candidates) {
        for (Supplier<Platform> candidate : candidates) {
            try {
                Platform platform = candidate.get();
                if (platform.name() != null) return platform;
            } catch (Throwable ignored) {}
        }
        return new Platform() {};
    }
    public static Platform get() { return INSTANCE; }
    public String name() { throw new UnsupportedOperationException(); }
    private static final class First extends Platform {
        private First() {}
        @Override public String name() { return null; }
    }
    private static final class Second extends Platform {
        private Second() {}
        @Override public String name() { return "second"; }
    }
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
public final class Main {
    public static void main() {
        Probe.check("override", "second".equals(Platform.get().name()));
    }
}
"#,
    );
    assert_eq!(run("platform", sources, &[]), all_true(&["override"]));
}

/// Fabric's `"environment": "client"` keeps a mod off a dedicated server, so a
/// multi-loader jar (Scribble, Cubes Without Borders) must not have its
/// NeoForge `@Mod` class run in the server role instead. The game reads
/// `client_side_only` to keep the jar off every server role.
#[test]
fn fabric_client_environment_marks_the_jar_client_only() {
    let jar_for = |name: &str, environment: &str| {
        let mut sources = BTreeMap::new();
        sources.insert(
            "demo/Main.java",
            "package demo;\npublic final class Main { public static void main() {} }\n",
        );
        let metadata = format!(
            r#"{{"schemaVersion":1,"id":"demo","version":"1","environment":"{environment}",
               "entrypoints":{{"client":["demo.Main"]}}}}"#
        );
        let path = build_jar(
            name,
            sources,
            &[("fabric.mod.json", metadata.as_str())],
            &[],
        );
        NativeJar::from_path(&path).unwrap()
    };
    assert!(jar_for("env-client", "client").client_side_only);
    assert!(!jar_for("env-any", "*").client_side_only);
    assert!(!jar_for("env-server", "server").client_side_only);
}

/// WorldEdit's `LogManagerCompat` names its logger after the class two frames
/// up (`StackWalker.walk(s -> s.skip(2).findFirst())`) and throws when the
/// walk is empty. The walk sees the interpreter's jar frames, innermost first.
#[test]
fn stack_walker_sees_the_jar_frames_innermost_first() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Logs.java",
        r#"package demo;
public final class Logs {
    static String callerCaller() {
        return StackWalker.getInstance()
            .walk(frames -> frames.skip(2).findFirst())
            .orElseThrow(() -> new AssertionError("No caller found"))
            .getClassName();
    }
    static String named() { return callerCaller(); }
    static String method() {
        return StackWalker.getInstance().walk(frames -> frames.findFirst()).get().getMethodName();
    }
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
public final class Main {
    public static void main() {
        Probe.check("caller", "demo.Main".equals(Logs.named()));
        Probe.check("method", "method".equals(Logs.method()));
    }
}
"#,
    );
    assert_eq!(
        run("stack-walker", sources, &[]),
        all_true(&["caller", "method"])
    );
}

/// Id validators: `String.matches` and `Pattern.matches` answer the bounded
/// regex subset exactly (supermartijn642's core library checks its own mod id
/// against `[a-z0-9_.-]*` before registering anything).
#[test]
fn string_matches_validates_ids_like_java() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.regex.Pattern;
public final class Main {
    public static void main() {
        Probe.check("valid", "supermartijn642corelib".matches("[a-z0-9_.-]*"));
        Probe.check("invalid", !"Bad Id".matches("[a-z0-9_.-]*"));
        Probe.check("version", Pattern.matches("\\d+(\\.\\d+)*", "26.3.1"));
        Probe.check("version-bad", !Pattern.matches("\\d+(\\.\\d+)*", "26.3."));
    }
}
"#,
    );
    assert_eq!(
        run("matches", sources, &[]),
        all_true(&["invalid", "valid", "version", "version-bad"])
    );
}

/// Payload records `requireNonNull` the codec a game factory built
/// (`StreamCodec.composite(ByteBufCodecs.INT, ...)`). The game never answers
/// null for a codec, so neither does the host; a codec-typed constant is not
/// null either.
#[test]
fn game_codec_factories_never_answer_null() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "net/minecraft/network/codec/StreamCodec.java",
        r#"package net.minecraft.network.codec;
public interface StreamCodec<B, V> {
    static <B, V> StreamCodec<B, V> composite(StreamCodec<B, ?> field, java.util.function.Function<?, ?> a,
            java.util.function.Function<?, ?> b) { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/network/codec/ByteBufCodecs.java",
        r#"package net.minecraft.network.codec;
public interface ByteBufCodecs {
    StreamCodec<Object, Integer> INT = null;
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.Objects;
import net.minecraft.network.codec.ByteBufCodecs;
import net.minecraft.network.codec.StreamCodec;
public final class Main {
    public static void main() {
        StreamCodec<Object, Object> codec = StreamCodec.composite(ByteBufCodecs.INT, x -> x, x -> x);
        Objects.requireNonNull(codec, "codec");
        Probe.check("constant", ByteBufCodecs.INT != null);
        Probe.check("composite", codec != null);
    }
}
"#,
    );
    assert_eq!(
        run(
            "codecs",
            sources,
            &[
                "net/minecraft/network/codec/StreamCodec.class",
                "net/minecraft/network/codec/ByteBufCodecs.class",
            ],
        ),
        all_true(&["composite", "constant"])
    );
}

/// Registry wrappers are indexed by `registry.key().identifier()`
/// (supermartijn642's `Registries`). Each built-in registry must answer its
/// own id; a shared null id made the second wrapper a duplicate.
#[test]
fn registry_key_identifier_names_the_registry() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\npublic final class Identifier { @Override public String toString() { return null; } }\n",
    );
    sources.insert(
        "net/minecraft/resources/ResourceKey.java",
        "package net.minecraft.resources;\npublic final class ResourceKey<T> { public Identifier identifier() { return null; } }\n",
    );
    sources.insert(
        "net/minecraft/core/Registry.java",
        "package net.minecraft.core;\npublic interface Registry<T> { net.minecraft.resources.ResourceKey<? extends Registry<T>> key(); }\n",
    );
    sources.insert(
        "net/minecraft/core/DefaultedRegistry.java",
        "package net.minecraft.core;\npublic interface DefaultedRegistry<T> extends Registry<T> {}\n",
    );
    sources.insert(
        "net/minecraft/core/registries/BuiltInRegistries.java",
        r#"package net.minecraft.core.registries;
import net.minecraft.core.*;
public final class BuiltInRegistries {
    public static DefaultedRegistry<Object> BLOCK;
    public static DefaultedRegistry<Object> ITEM;
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.Identifier;
public final class Main {
    public static void main() {
        Identifier block = BuiltInRegistries.BLOCK.key().identifier();
        Identifier item = BuiltInRegistries.ITEM.key().identifier();
        Probe.check("block", "minecraft:block".equals(block.toString()));
        Probe.check("distinct", !block.equals(item));
    }
}
"#,
    );
    assert_eq!(
        run(
            "registry-key",
            sources,
            &[
                "net/minecraft/resources/Identifier.class",
                "net/minecraft/resources/ResourceKey.class",
                "net/minecraft/core/Registry.class",
                "net/minecraft/core/DefaultedRegistry.class",
                "net/minecraft/core/registries/BuiltInRegistries.class",
            ],
        ),
        all_true(&["block", "distinct"])
    );
}

/// Per-mod registration helpers are cached with
/// `map.computeIfAbsent(modid, Handler::new)`: the constructor reference runs
/// with the key as its argument, so the cached helper knows its mod id.
#[test]
fn compute_if_absent_constructor_reference_receives_the_key() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.HashMap;
import java.util.Map;
public final class Main {
    static final Map<String, Main.Handler> HANDLERS = new HashMap<>();
    static final class Handler {
        final String modid;
        private Handler(String modid) { this.modid = modid; }
    }
    public static void main() {
        Handler first = HANDLERS.computeIfAbsent("demo", Handler::new);
        Probe.check("modid", "demo".equals(first.modid));
        Probe.check("cached", HANDLERS.computeIfAbsent("demo", Handler::new) == first);
    }
}
"#,
    );
    assert_eq!(
        run("compute-if-absent", sources, &[]),
        all_true(&["cached", "modid"])
    );
}

/// Carpet's settings manager: a rule annotation read for members the use site
/// left at their defaults (an empty `Class[]`, an enum, a string), an enum
/// field type recognised with `isEnum` and listed with `getEnumConstants`, and
/// a converter table built with `Map.ofEntries(Map.entry(...))`.
#[test]
fn settings_annotations_enums_and_entry_maps_answer_like_the_jvm() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Rule.java",
        r#"package demo;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
public @interface Rule {
    String desc() default "none";
    Class<?>[] condition() default {};
    Class<?> type() default String.class;
    Mode mode() default Mode.FAST;
    String[] options() default {"a", "b"};
    int weight() default 3;
}
"#,
    );
    sources.insert(
        "demo/Mode.java",
        "package demo;\npublic enum Mode { FAST, SLOW { @Override public String toString() { return \"slow\"; } } }\n",
    );
    sources.insert(
        "demo/Settings.java",
        r#"package demo;
public final class Settings {
    @Rule(desc = "speed") public static int speed = 1;
    @Rule(mode = Mode.SLOW, options = {}) public static Mode mode = Mode.FAST;
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.Map;
public final class Main {
    public static void main() throws Exception {
        Rule speed = Settings.class.getField("speed").getAnnotation(Rule.class);
        Probe.check("explicit", "speed".equals(speed.desc()));
        Probe.check("default-array", speed.condition().length == 0);
        Probe.check("default-class", speed.type() == String.class);
        Probe.check("default-enum", speed.mode() == Mode.FAST);
        Probe.check("default-strings", speed.options().length == 2 && "b".equals(speed.options()[1]));
        Probe.check("default-int", speed.weight() == 3);
        Rule mode = Settings.class.getField("mode").getAnnotation(Rule.class);
        Probe.check("explicit-enum", mode.mode() == Mode.SLOW);
        Probe.check("explicit-empty", mode.options().length == 0);

        Probe.check("is-enum", Mode.class.isEnum() && !Settings.class.isEnum());
        Object[] constants = Mode.class.getEnumConstants();
        Probe.check("constants", constants.length == 2 && constants[1] == Mode.SLOW);
        Probe.check("not-enum-constants", Settings.class.getEnumConstants() == null);

        Map<Class<?>, String> converters = Map.ofEntries(
            Map.entry(String.class, "string"), Map.entry(Integer.class, "int"));
        Probe.check("entries", "int".equals(converters.get(Integer.class)) && converters.size() == 2);
    }
}
"#,
    );
    assert_eq!(
        run("settings", sources, &[]),
        all_true(&[
            "constants",
            "default-array",
            "default-class",
            "default-enum",
            "default-int",
            "default-strings",
            "entries",
            "explicit",
            "explicit-empty",
            "explicit-enum",
            "is-enum",
            "not-enum-constants",
        ])
    );
}

/// `BiConsumer.andThen` runs both consumers with both arguments, and
/// `BiFunction.andThen` feeds the result on (supermartijn642's registry
/// override handlers chain their field patchers this way).
#[test]
fn two_argument_functional_composition_runs_both_sides() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.function.*;
public final class Main {
    static int sum;
    public static void main() {
        BiConsumer<Integer, Integer> first = (a, b) -> sum += a * b;
        BiConsumer<Integer, Integer> both = first.andThen((a, b) -> sum += a + b);
        both.accept(3, 4);
        Probe.check("consumers", sum == 19);
        BiFunction<Integer, Integer, Integer> times = (a, b) -> a * b;
        Probe.check("function", times.andThen(x -> x + 1).apply(5, 6) == 31);
    }
}
"#,
    );
    assert_eq!(
        run("bi-compose", sources, &[]),
        all_true(&["consumers", "function"])
    );
}

/// Libraries the game ships (commons-lang3, commons-io) and the `Locale`
/// case overloads, as Carpet's script loader uses them to name and read its
/// bundled modules.
#[test]
fn bundled_library_helpers_answer() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "org/apache/commons/lang3/ClassUtils.java",
        "package org.apache.commons.lang3;\npublic final class ClassUtils { public static Class<?> primitiveToWrapper(Class<?> c) { return null; } }\n",
    );
    sources.insert(
        "org/apache/commons/io/IOUtils.java",
        "package org.apache.commons.io;\npublic final class IOUtils { public static String toString(java.io.InputStream s, java.nio.charset.Charset c) { return null; } }\n",
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.nio.charset.StandardCharsets;
import java.util.Locale;
import org.apache.commons.io.IOUtils;
import org.apache.commons.lang3.ClassUtils;
public final class Main {
    public static void main() throws Exception {
        Probe.check("wrapper", ClassUtils.primitiveToWrapper(int.class) == Integer.class);
        Probe.check("passthrough", ClassUtils.primitiveToWrapper(String.class) == String.class);
        Probe.check("lower", "Camera.SC".toLowerCase(Locale.ROOT).equals("camera.sc"));
        String text = IOUtils.toString(
            Main.class.getClassLoader().getResourceAsStream("assets/demo/hello.sc"),
            StandardCharsets.UTF_8);
        Probe.check("resource", "print('hi')".equals(text));
    }
}
"#,
    );
    let jar = NativeJar::from_path(&build_jar(
        "library-helpers",
        sources,
        &[("assets/demo/hello.sc", "print('hi')")],
        &[
            "org/apache/commons/lang3/ClassUtils.class",
            "org/apache/commons/io/IOUtils.class",
        ],
    ))
    .unwrap();
    let mut host = jar.seed_host(StdHost::new());
    jar.execute("demo.Main", Some("main"), "main", &mut host)
        .unwrap();
    assert_eq!(
        checks(&host),
        all_true(&["lower", "passthrough", "resource", "wrapper"])
    );
}

/// Deferred registration: a `Supplier` captures a `Function` and an id, and the
/// function itself wraps another one (Collector's Album's
/// `PlatformItemRegistry.registerItem`). Every layer must hand back the object
/// the innermost constructor built.
#[test]
fn nested_captured_functions_return_the_constructed_object() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.Objects;
import java.util.function.*;
public final class Main {
    static final class Props { int stack = 64; Props stacksTo(int n) { stack = n; return this; } }
    static final class Album { final int stack; Album(Props props) { stack = props.stack; } }
    static <R> Supplier<R> register(String name, Function<String, R> factory) {
        return () -> factory.apply(name);
    }
    static <T> Supplier<T> registerItem(String name, Function<Props, T> factory) {
        return register(name, id -> factory.apply(new Props()));
    }
    public static void main() {
        Supplier<Album> album = registerItem("album", props -> new Album(props.stacksTo(1)));
        Album built = Objects.requireNonNull(album.get());
        Probe.check("built", built.stack == 1);
    }
}
"#,
    );
    assert_eq!(run("nested-functions", sources, &[]), all_true(&["built"]));
}

/// A game builder chain (`CreativeModeTab.builder(...).title(...).build()`)
/// never answers null in the game, and mods `requireNonNull` what they
/// register from it.
#[test]
fn game_builder_chains_never_answer_null() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "net/minecraft/world/item/CreativeModeTab.java",
        r#"package net.minecraft.world.item;
public class CreativeModeTab {
    public static Builder builder(int row, int column) { return null; }
    public static class Builder {
        public Builder title(String title) { return null; }
        public CreativeModeTab build() { return null; }
    }
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.util.Objects;
import net.minecraft.world.item.CreativeModeTab;
public final class Main {
    public static void main() {
        CreativeModeTab tab = CreativeModeTab.builder(0, 1).title("demo").build();
        Objects.requireNonNull(tab);
        Probe.check("built", tab != null);
    }
}
"#,
    );
    assert_eq!(
        run(
            "builder",
            sources,
            &[
                "net/minecraft/world/item/CreativeModeTab.class",
                "net/minecraft/world/item/CreativeModeTab$Builder.class",
            ],
        ),
        all_true(&["built"])
    );
}

/// Reflection a config registry and a service binder use: listing a type's
/// constructors by parameter count (Armor Hider), and a nested interface's
/// simple name naming the field it binds to (REI's `Internals`).
#[test]
fn constructors_and_nested_simple_names_follow_the_class_file() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.lang.reflect.Constructor;
public final class Main {
    interface EntryProvider {}
    static final class Item {
        final String value;
        Item() { this("default"); }
        Item(String value) { this.value = value; }
    }
    public static void main() throws Exception {
        Constructor<?> one = null, none = null;
        for (Constructor<?> constructor : Item.class.getDeclaredConstructors()) {
            if (constructor.getParameterCount() == 1) one = constructor;
            else if (constructor.getParameterCount() == 0) none = constructor;
        }
        Probe.check("one", one != null && "x".equals(((Item) one.newInstance("x")).value));
        Probe.check("none", none != null);
        Probe.check("nested", "EntryProvider".equals(EntryProvider.class.getSimpleName()));
        Probe.check("top", "Main".equals(Main.class.getSimpleName()));
        Object anonymous = new Object() {};
        Probe.check("anonymous", anonymous.getClass().getSimpleName().isEmpty());
    }
}
"#,
    );
    assert_eq!(
        run("constructors", sources, &[]),
        all_true(&["anonymous", "nested", "none", "one", "top"])
    );
}

/// Game-side values a mod's own registry needs: a registry key built from an
/// id, Fabric's registry builder answering a registry of that id, DFU's
/// `Pair` handing back what it holds, an `EnumMap` keeping its entries, a game
/// type's `create*` factory and `NumberFormat.format(long)`.
#[test]
fn mod_registry_and_shipped_library_values_are_kept() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "net/minecraft/resources/Identifier.java",
        r#"package net.minecraft.resources;
public final class Identifier {
    public static Identifier fromNamespaceAndPath(String namespace, String path) { return null; }
    @Override public String toString() { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/resources/ResourceKey.java",
        r#"package net.minecraft.resources;
public final class ResourceKey<T> {
    public static <T> ResourceKey<T> createRegistryKey(Identifier id) { return null; }
    public Identifier identifier() { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/core/Registry.java",
        "package net.minecraft.core;\npublic interface Registry<T> { net.minecraft.resources.ResourceKey<? extends Registry<T>> key(); }\n",
    );
    sources.insert(
        "net/minecraft/core/WritableRegistry.java",
        "package net.minecraft.core;\npublic interface WritableRegistry<T> extends Registry<T> {}\n",
    );
    sources.insert(
        "net/fabricmc/fabric/api/event/registry/FabricRegistryBuilder.java",
        r#"package net.fabricmc.fabric.api.event.registry;
import net.minecraft.resources.ResourceKey;
public final class FabricRegistryBuilder<T> {
    public static <T> FabricRegistryBuilder<T> create(ResourceKey<?> key) { return null; }
    public FabricRegistryBuilder<T> attribute(Object attribute) { return null; }
    public net.minecraft.core.WritableRegistry<T> buildAndRegister() { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/sounds/SoundEvent.java",
        "package net.minecraft.sounds;\npublic final class SoundEvent { public static SoundEvent createVariableRangeEvent(net.minecraft.resources.Identifier id) { return null; } }\n",
    );
    sources.insert(
        "com/mojang/datafixers/util/Pair.java",
        r#"package com.mojang.datafixers.util;
public final class Pair<F, S> {
    public static <F, S> Pair<F, S> of(F first, S second) { return null; }
    public F getFirst() { return null; }
    public S getSecond() { return null; }
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import com.mojang.datafixers.util.Pair;
import java.text.NumberFormat;
import java.util.*;
import net.fabricmc.fabric.api.event.registry.FabricRegistryBuilder;
import net.minecraft.core.Registry;
import net.minecraft.resources.*;
import net.minecraft.sounds.SoundEvent;
public final class Main {
    enum Side { CLIENT, SERVER }
    public static void main() {
        ResourceKey<Registry<Object>> key =
            ResourceKey.createRegistryKey(Identifier.fromNamespaceAndPath("demo", "cards"));
        Registry<Object> registry = FabricRegistryBuilder.create(key).attribute(null).buildAndRegister();
        Probe.check("registry", registry != null
            && "demo:cards".equals(registry.key().identifier().toString()));

        Pair<Integer, String> pair = Pair.of(3, "field");
        Probe.check("pair", pair.getFirst() == 3 && "field".equals(pair.getSecond()));

        EnumMap<Side, List<String>> sides = new EnumMap<>(Side.class);
        sides.put(Side.SERVER, new ArrayList<>());
        Probe.check("enum-map", sides.get(Side.SERVER) != null && sides.get(Side.SERVER).isEmpty());

        Probe.check("factory", SoundEvent.createVariableRangeEvent(
            Identifier.fromNamespaceAndPath("demo", "flip")) != null);
        Probe.check("format", "1,234,567".equals(NumberFormat.getIntegerInstance(Locale.US).format(1234567L)));
    }
}
"#,
    );
    assert_eq!(
        run(
            "mod-registry",
            sources,
            &[
                "net/minecraft/resources/Identifier.class",
                "net/minecraft/resources/ResourceKey.class",
                "net/minecraft/core/Registry.class",
                "net/minecraft/core/WritableRegistry.class",
                "net/fabricmc/fabric/api/event/registry/FabricRegistryBuilder.class",
                "net/minecraft/sounds/SoundEvent.class",
                "com/mojang/datafixers/util/Pair.class",
            ],
        ),
        all_true(&["enum-map", "factory", "format", "pair", "registry"])
    );
}

/// Vanilla 26.3's id helpers as Farmer's Delight registers its blocks: a
/// `BlockItemId`'s block key names the id it was created from, and a
/// `ColorCollection` derives sixteen coloured ids through a jar function.
#[test]
fn block_item_ids_and_color_collections_name_their_ids() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "net/minecraft/resources/Identifier.java",
        r#"package net.minecraft.resources;
public final class Identifier {
    public static Identifier fromNamespaceAndPath(String namespace, String path) { return null; }
    @Override public String toString() { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/resources/ResourceKey.java",
        "package net.minecraft.resources;\npublic final class ResourceKey<T> { public Identifier identifier() { return null; } }\n",
    );
    sources.insert(
        "net/minecraft/references/BlockItemId.java",
        r#"package net.minecraft.references;
import net.minecraft.resources.*;
public final class BlockItemId {
    public static BlockItemId create(Identifier block, Identifier item) { return null; }
    public ResourceKey<Object> block() { return null; }
    public ResourceKey<Object> item() { return null; }
}
"#,
    );
    sources.insert(
        "net/minecraft/world/level/block/ColorCollection.java",
        r#"package net.minecraft.world.level.block;
public final class ColorCollection<T> {
    public static <T> ColorCollection<T> create(T value) { return null; }
    public static ColorCollection<String> prefixWithColor(ColorCollection<String> names) { return null; }
    public <U> ColorCollection<U> map(java.util.function.Function<T, U> function) { return null; }
    public T white() { return null; }
    public T lightBlue() { return null; }
    public T black() { return null; }
}
"#,
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import net.minecraft.references.BlockItemId;
import net.minecraft.resources.Identifier;
import net.minecraft.world.level.block.ColorCollection;
public final class Main {
    static Identifier id(String path) { return Identifier.fromNamespaceAndPath("demo", path); }
    public static void main() {
        BlockItemId stove = BlockItemId.create(id("stove"), id("stove_item"));
        Probe.check("block", "demo:stove".equals(stove.block().identifier().toString()));
        Probe.check("item", "demo:stove_item".equals(stove.item().identifier().toString()));
        ColorCollection<BlockItemId> signs = ColorCollection
            .prefixWithColor(ColorCollection.create("canvas_sign"))
            .map(name -> BlockItemId.create(id(name), id(name)));
        Probe.check("white", "demo:white_canvas_sign".equals(signs.white().block().identifier().toString()));
        Probe.check("light-blue", "demo:light_blue_canvas_sign".equals(signs.lightBlue().block().identifier().toString()));
        Probe.check("black", "demo:black_canvas_sign".equals(signs.black().item().identifier().toString()));
    }
}
"#,
    );
    assert_eq!(
        run(
            "block-item-ids",
            sources,
            &[
                "net/minecraft/resources/Identifier.class",
                "net/minecraft/resources/ResourceKey.class",
                "net/minecraft/references/BlockItemId.class",
                "net/minecraft/world/level/block/ColorCollection.class",
            ],
        ),
        all_true(&["black", "block", "item", "light-blue", "white"])
    );
}

/// A pattern-matching `switch` over a config default's type (ShulkerBoxTooltip's
/// `makeNbtReader`): the `typeSwitch` site answers the first matching label,
/// `-1` for null and the label count (the `default` arm) for no match.
#[test]
fn pattern_switch_selects_the_first_matching_label() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "demo/Main.java",
        r#"package demo;
public final class Main {
    enum Mode { ON }
    static String kind(Object value) {
        return switch (value) {
            case null -> "null";
            case Enum<?> e -> "enum";
            case Boolean b -> "boolean";
            case String s -> "string";
            case Integer i when i > 10 -> "big";
            case Integer i -> "int";
            default -> "other";
        };
    }
    public static void main() {
        Probe.check("enum", "enum".equals(kind(Mode.ON)));
        Probe.check("boolean", "boolean".equals(kind(Boolean.TRUE)));
        Probe.check("string", "string".equals(kind("text")));
        Probe.check("guard", "big".equals(kind(Integer.valueOf(42))));
        Probe.check("int", "int".equals(kind(Integer.valueOf(3))));
        Probe.check("null", "null".equals(kind(null)));
        Probe.check("default", "other".equals(kind(new Object())));
    }
}
"#,
    );
    assert_eq!(
        run("type-switch", sources, &[]),
        all_true(&[
            "boolean", "default", "enum", "guard", "int", "null", "string"
        ])
    );
}

/// A config manager checks each annotated field with
/// `Modifier.isPublic(field.getModifiers())` and names its declaring class in
/// the error (naturally_trimmed); a logging shim's `$SwitchMap` over SLF4J's
/// `Level` sizes itself from `values()` (moderner-beta).
#[test]
fn modifier_predicates_and_library_enum_switch_maps_answer() {
    let mut sources = BTreeMap::new();
    sources.insert(
        "org/slf4j/event/Level.java",
        "package org.slf4j.event;\npublic enum Level { ERROR, WARN, INFO, DEBUG, TRACE }\n",
    );
    sources.insert(
        "demo/Main.java",
        r#"package demo;
import java.lang.reflect.Field;
import java.lang.reflect.Modifier;
import org.slf4j.event.Level;
public final class Main {
    public static int open = 1;
    private static final int closed = 2;
    static int weight(Level level) {
        switch (level) {
            case TRACE: return 5;
            case ERROR: return 1;
            default: return 3;
        }
    }
    public static void main() throws Exception {
        Field open = Main.class.getDeclaredField("open");
        Field closed = Main.class.getDeclaredField("closed");
        Probe.check("public", Modifier.isPublic(open.getModifiers()) && Modifier.isStatic(open.getModifiers()));
        Probe.check("private", !Modifier.isPublic(closed.getModifiers())
            && Modifier.isPrivate(closed.getModifiers()) && Modifier.isFinal(closed.getModifiers()));
        Probe.check("declaring", open.getDeclaringClass() == Main.class);
        Probe.check("switch", weight(Level.TRACE) == 5);
        Probe.check("ordinal", Level.TRACE.ordinal() == 4 && "WARN".equals(Level.WARN.name()));
    }
}
"#,
    );
    assert_eq!(
        run("modifiers", sources, &["org/slf4j/event/Level.class"]),
        all_true(&["declaring", "ordinal", "private", "public", "switch"])
    );
}
