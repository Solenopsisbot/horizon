//! Fabric's payload type registry (`PayloadTypeRegistry.clientboundPlay()` and
//! its siblings, `register`/`registerLarge`) records each channel's direction,
//! type and codec into the same `payload_registrations` rows a receiver fills,
//! so a channel is known with its codec even before (or without) a receiver.
//! Signatures follow fabric-networking-api-v1 6.4.1 (Apache-2.0) for 26.3.

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

fn build_jar(
    name: &str,
    sources: BTreeMap<&str, &str>,
    resources: &[(&str, &str)],
    excluded: &[&str],
) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-fabric-payload-types-{name}-{}",
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
        .arg("--release")
        .arg("17")
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
        if excluded.contains(&entry.as_str()) {
            continue;
        }
        zip.start_file(entry, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(class).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    jar_path
}

const STUBS: [&str; 7] = [
    "net/minecraft/resources/Identifier.class",
    "net/minecraft/network/codec/StreamCodec.class",
    "net/minecraft/network/protocol/common/custom/CustomPacketPayload.class",
    "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type.class",
    "net/minecraft/network/protocol/common/custom/CustomPacketPayload$TypeAndCodec.class",
    "net/fabricmc/fabric/api/networking/v1/PayloadTypeRegistry.class",
    "net/fabricmc/fabric/api/client/networking/v1/ClientPlayNetworking.class",
];

fn sources() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "net/minecraft/resources/Identifier.java",
            r#"package net.minecraft.resources;
public final class Identifier {
    public static Identifier fromNamespaceAndPath(String namespace, String path) { return null; }
}
"#,
        ),
        (
            "net/minecraft/network/codec/StreamCodec.java",
            "package net.minecraft.network.codec; public interface StreamCodec<B, V> {}",
        ),
        (
            "net/minecraft/network/protocol/common/custom/CustomPacketPayload.java",
            r#"package net.minecraft.network.protocol.common.custom;
import net.minecraft.resources.Identifier;
public interface CustomPacketPayload {
    Type<? extends CustomPacketPayload> type();
    record Type<T extends CustomPacketPayload>(Identifier id) {}
    record TypeAndCodec<B, T extends CustomPacketPayload>(Type<T> type, net.minecraft.network.codec.StreamCodec<B, T> codec) {}
}
"#,
        ),
        (
            "net/fabricmc/fabric/api/networking/v1/PayloadTypeRegistry.java",
            r#"package net.fabricmc.fabric.api.networking.v1;
import net.minecraft.network.codec.StreamCodec;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
public interface PayloadTypeRegistry<B> {
    // Raw types: the erased descriptors are Fabric's own.
    @SuppressWarnings("rawtypes")
    CustomPacketPayload.TypeAndCodec register(CustomPacketPayload.Type type, StreamCodec codec);
    @SuppressWarnings("rawtypes")
    CustomPacketPayload.TypeAndCodec registerLarge(CustomPacketPayload.Type type, StreamCodec codec, int maxPacketSize);
    static PayloadTypeRegistry<Object> clientboundPlay() { return null; }
    static PayloadTypeRegistry<Object> serverboundPlay() { return null; }
    static PayloadTypeRegistry<Object> clientboundConfiguration() { return null; }
    static PayloadTypeRegistry<Object> serverboundConfiguration() { return null; }
}
"#,
        ),
        (
            "net/fabricmc/fabric/api/client/networking/v1/ClientPlayNetworking.java",
            r#"package net.fabricmc.fabric.api.client.networking.v1;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
public final class ClientPlayNetworking {
    public interface PlayPayloadHandler<T> { void receive(T payload, Object context); }
    public static <T extends CustomPacketPayload> boolean registerGlobalReceiver(CustomPacketPayload.Type<T> type, PlayPayloadHandler<T> handler) { return true; }
}
"#,
        ),
        (
            "demo/FrameCodec.java",
            r#"package demo;
public final class FrameCodec implements net.minecraft.network.codec.StreamCodec<Object, Object> {}
"#,
        ),
        (
            "demo/Main.java",
            r#"package demo;
import net.fabricmc.fabric.api.client.networking.v1.ClientPlayNetworking;
import net.fabricmc.fabric.api.networking.v1.PayloadTypeRegistry;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;
public final class Main {
    static final FrameCodec FRAME = new FrameCodec();
    @SuppressWarnings({"unchecked", "rawtypes"})
    public static void main() {
        CustomPacketPayload.Type sync = new CustomPacketPayload.Type(Identifier.fromNamespaceAndPath("demo", "sync"));
        CustomPacketPayload.Type hello = new CustomPacketPayload.Type(Identifier.fromNamespaceAndPath("demo", "hello"));
        Object typeAndCodec = PayloadTypeRegistry.clientboundPlay().register(sync, FRAME);
        if (typeAndCodec == null) throw new IllegalStateException("register answered null");
        PayloadTypeRegistry.serverboundConfiguration().registerLarge(hello, FRAME, 1 << 20);
        ClientPlayNetworking.registerGlobalReceiver(sync, (payload, context) -> {});
    }
}
"#,
        ),
    ])
}

#[test]
fn payload_type_registry_records_direction_type_and_codec() {
    let jar_path = build_jar("payload-types", sources(), &[], &STUBS);
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let mut host = jar.seed_host(StdHost::new());
    jar.execute("demo.Main", Some("main"), "main", &mut host)
        .unwrap();
    let rows = host.payload_registrations().to_vec();
    // One row per (direction, type): the receiver joins the type's row.
    assert_eq!(rows.len(), 2, "{rows:?}");
    let sync = rows
        .iter()
        .find(|row| row.direction == "playToClient")
        .expect("clientbound play row");
    assert!(matches!(sync.codec, Value::Ref(Some(_))), "{sync:?}");
    assert!(matches!(sync.handler, Value::Ref(Some(_))), "{sync:?}");
    let hello = rows
        .iter()
        .find(|row| row.direction == "configurationToServer")
        .expect("serverbound configuration row");
    assert_eq!(hello.codec, sync.codec, "the same codec object");
    assert_eq!(hello.handler, Value::Ref(None), "no receiver for it");
    assert_ne!(hello.payload_type, sync.payload_type);
    let unknown = host
        .unknown_host_calls()
        .into_iter()
        .filter(|(method, _)| method.contains("PayloadTypeRegistry"))
        .collect::<Vec<_>>();
    assert!(unknown.is_empty(), "{unknown:?}");
}
