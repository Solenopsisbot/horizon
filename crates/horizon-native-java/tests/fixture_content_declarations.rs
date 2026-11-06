//! A source-built Fabric jar declares content the content model now types:
//! data component types built with `DataComponentType.builder()`, attachment
//! types through Fabric's `AttachmentRegistry` (a direct call, a builder chain
//! and a configuring lambda), and recipes as `data/<ns>/recipe/*.json` files.
//!
//! The game and Fabric API stubs are compiled onto a separate classpath and
//! not packaged, so every builder call crosses the interpreter's boundary as in
//! a real install.

use horizon_native_java::{ATTACHMENT_REGISTRY, NativeJar, RegistryDeclaration, StdHost};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};
use zip::write::SimpleFileOptions;

/// A Java source tree for [`build_jar`]: relative path => source text.
macro_rules! sources {
    ($($path:literal => $body:expr),* $(,)?) => {{
        let mut map: BTreeMap<&str, &str> = BTreeMap::new();
        $(map.insert($path, $body);)*
        map
    }};
}

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

fn scratch_root(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-content-decl-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

fn write_sources(root: &Path, tree: &BTreeMap<&str, &str>) -> Vec<PathBuf> {
    tree.iter()
        .map(|(relative, body)| {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            path
        })
        .collect()
}

fn compile(out: &Path, classpath: Option<&Path>, sources: &[PathBuf], label: &str) {
    fs::create_dir_all(out).unwrap();
    let mut command = Command::new(javac());
    command.arg("--release").arg("17").arg("-d").arg(out);
    if let Some(classpath) = classpath {
        command.arg("-cp").arg(classpath);
    }
    let status = command.args(sources).status().unwrap();
    assert!(status.success(), "javac failed for {label}");
}

/// Compile API stubs onto a classpath, compile the mod against them, and
/// package only the mod's own class files plus its loader metadata.
fn build_jar(
    name: &str,
    stubs: &BTreeMap<&str, &str>,
    sources: &BTreeMap<&str, &str>,
    metadata: &[u8],
    data: &[(&str, &[u8])],
) -> (PathBuf, PathBuf) {
    let root = scratch_root(name);
    let stub_classes = root.join("stub-classes");
    let classes = root.join("classes");
    let stub_paths = write_sources(&root.join("stub-src"), stubs);
    compile(&stub_classes, None, &stub_paths, name);
    let paths = write_sources(&root.join("src"), sources);
    compile(&classes, Some(&stub_classes), &paths, name);

    let jar_path = root.join(format!("{name}.jar"));
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(metadata).unwrap();
    for (name, bytes) in data {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    for class in files_under(&classes) {
        let entry = class
            .strip_prefix(&classes)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(entry, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(class).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    (root, jar_path)
}

/// The game and Fabric API surface the mod compiles against.
fn stubs() -> BTreeMap<&'static str, &'static str> {
    sources! {
        "net/fabricmc/api/ModInitializer.java" =>
            "package net.fabricmc.api; public interface ModInitializer { void onInitialize(); }",
        "net/minecraft/resources/Identifier.java" => r#"package net.minecraft.resources;
public final class Identifier {
    private Identifier() { }
    public static Identifier fromNamespaceAndPath(String namespace, String path) { return null; }
}"#,
        "com/mojang/serialization/Codec.java" => r#"package com.mojang.serialization;
    public interface Codec<A> { Codec<Integer> INT = null; }"#,
        "net/minecraft/core/component/DataComponentType.java" => r#"package net.minecraft.core.component;
public interface DataComponentType<T> {
    static <T> Builder<T> builder() { return new Builder<>(); }
    final class Builder<T> {
        public Builder<T> persistent(com.mojang.serialization.Codec<T> codec) { return this; }
        public Builder<T> networkSynchronized(Object codec) { return this; }
        public Builder<T> cacheEncoding() { return this; }
        public DataComponentType<T> build() { return null; }
    }
}"#,
        "net/fabricmc/fabric/api/attachment/v1/AttachmentType.java" =>
            "package net.fabricmc.fabric.api.attachment.v1; public interface AttachmentType<A> { }",
        "net/fabricmc/fabric/api/attachment/v1/AttachmentRegistry.java" => r#"package net.fabricmc.fabric.api.attachment.v1;
import net.minecraft.resources.Identifier;
import java.util.function.Consumer;
import java.util.function.Supplier;
public final class AttachmentRegistry {
    public static <A> AttachmentType<A> create(Identifier id) { return null; }
    public static <A> AttachmentType<A> create(Identifier id, Consumer<Builder<A>> consumer) { return null; }
    public static <A> AttachmentType<A> createPersistent(Identifier id, com.mojang.serialization.Codec<A> codec) { return null; }
    public static <A> AttachmentType<A> createDefaulted(Identifier id, Supplier<A> initializer) { return null; }
    public static <A> Builder<A> builder() { return null; }
    public interface Builder<A> {
        Builder<A> persistent(com.mojang.serialization.Codec<A> codec);
        Builder<A> copyOnDeath();
        Builder<A> initializer(Supplier<A> initializer);
        AttachmentType<A> buildAndRegister(Identifier id);
    }
}"#,
        "net/minecraft/core/Registry.java" => r#"package net.minecraft.core;
import net.minecraft.resources.Identifier;
public interface Registry<T> {
    static <V, T extends V> T register(Registry<V> registry, Identifier id, T value) {
        return value;
    }
}"#,
        "net/minecraft/core/registries/BuiltInRegistries.java" => r#"package net.minecraft.core.registries;
import net.minecraft.core.Registry;
public final class BuiltInRegistries {
    public static final Registry<Object> DATA_COMPONENT_TYPE = null;
}"#,
    }
}

fn content_mod() -> BTreeMap<&'static str, &'static str> {
    sources! {
        "demo/ContentMod.java" => r#"
package demo;
import com.mojang.serialization.Codec;
import net.fabricmc.api.ModInitializer;
import net.fabricmc.fabric.api.attachment.v1.AttachmentRegistry;
import net.fabricmc.fabric.api.attachment.v1.AttachmentType;
import net.minecraft.core.Registry;
import net.minecraft.core.component.DataComponentType;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.resources.Identifier;

public final class ContentMod implements ModInitializer {
    static Identifier id(String path) { return Identifier.fromNamespaceAndPath("fixture", path); }

    @Override
    public void onInitialize() {
        Registry.register(BuiltInRegistries.DATA_COMPONENT_TYPE, id("charge"),
                DataComponentType.<Integer>builder().persistent(Codec.INT).networkSynchronized(null).build());
        Registry.register(BuiltInRegistries.DATA_COMPONENT_TYPE, id("cached"),
                DataComponentType.<Integer>builder().cacheEncoding().build());
        AttachmentType<Integer> plain = AttachmentRegistry.create(id("plain"));
        AttachmentType<Integer> saved = AttachmentRegistry.createPersistent(id("saved"), Codec.INT);
        AttachmentType<Integer> chain = AttachmentRegistry.<Integer>builder()
                .copyOnDeath().initializer(() -> 0).buildAndRegister(id("chain"));
        AttachmentType<Integer> lambda = AttachmentRegistry.<Integer>create(id("lambda"),
                builder -> builder.persistent(Codec.INT).copyOnDeath());
    }
}
"#,
    }
}

fn flags(declaration: &Option<RegistryDeclaration>, kind: &str) -> Vec<String> {
    match declaration {
        Some(RegistryDeclaration::Custom {
            kind: found,
            properties,
        }) if found == kind => properties
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect(),
        other => panic!("not a {kind} declaration: {other:?}"),
    }
}

#[test]
fn builder_chains_type_data_components_and_attachments() {
    let (root, jar_path) = build_jar(
        "builders",
        &stubs(),
        &content_mod(),
        br#"{"id":"fixture","version":"1.0","entrypoints":{"main":"demo.ContentMod"}}"#,
        &[],
    );
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = jar.seed_host(StdHost::new());
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    let rows: BTreeMap<String, &Option<RegistryDeclaration>> = host
        .registrations()
        .iter()
        .map(|entry| (entry.key(), &entry.declaration))
        .collect();
    let key = |registry: &str, id: &str| format!("{registry}/fixture:{id}");
    let component = |id: &str| {
        flags(
            rows[&key("minecraft:data_component_type", id)],
            "data_component",
        )
    };
    let attachment = |id: &str| flags(rows[&key(ATTACHMENT_REGISTRY, id)], "attachment");
    // `Codec.INT` is an opaque codec marker: the label is its declared type.
    assert_eq!(
        component("charge"),
        [
            "codec=com/mojang/serialization/Codec",
            "network=true",
            "persistent=true"
        ]
    );
    assert_eq!(component("cached"), ["cache_encoding=true"]);
    assert!(attachment("plain").is_empty());
    assert_eq!(
        attachment("saved"),
        ["codec=com/mojang/serialization/Codec", "persistent=true"]
    );
    assert_eq!(
        attachment("chain"),
        ["copy_on_death=true", "initializer=true"]
    );
    // The configuring lambda ran on a builder: its setters typed the row.
    assert_eq!(
        attachment("lambda"),
        [
            "codec=com/mojang/serialization/Codec",
            "copy_on_death=true",
            "persistent=true"
        ]
    );
    assert_eq!(rows.len(), 6, "{rows:?}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_jars_data_recipes_become_recipe_declarations() {
    let shaped = br##"{"type":"minecraft:crafting_shaped","pattern":["#"],"key":{"#":"minecraft:stick"},"result":{"id":"fixture:wand"}}"##;
    let (root, jar_path) = build_jar(
        "recipes",
        &stubs(),
        &content_mod(),
        br#"{"id":"fixture","version":"1.0","entrypoints":{"main":"demo.ContentMod"}}"#,
        &[
            ("data/fixture/recipe/wand.json", shaped),
            (
                "data/fixture/recipe/tools/rod.json",
                br#"{"type":"minecraft:smelting"}"#,
            ),
            // Vanilla's recipes stay vanilla's; broken and non-object files
            // are skipped as the recipe manager skips them.
            (
                "data/minecraft/recipe/stick.json",
                br#"{"type":"minecraft:smelting"}"#,
            ),
            ("data/fixture/recipe/broken.json", b"{not json"),
            ("data/fixture/recipe/list.json", b"[1, 2]"),
            ("data/fixture/advancement/recipe/wand.json", b"{}"),
        ],
    );
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let ids: Vec<String> = jar.native_declarations.iter().map(|e| e.key()).collect();
    assert_eq!(
        ids,
        [
            "minecraft:recipe/fixture:tools/rod",
            "minecraft:recipe/fixture:wand"
        ]
    );
    let Some(RegistryDeclaration::Recipe { json }) = &jar.native_declarations[1].declaration else {
        panic!("a recipe row is typed");
    };
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(value["result"]["id"], "fixture:wand");
    // A metadata-only read sees the same rows: they are the jar's metadata.
    let metadata = NativeJar::from_path_metadata(&jar_path).unwrap();
    assert_eq!(metadata.native_declarations, jar.native_declarations);
    let _ = fs::remove_dir_all(root);
}
