//! Runtime-built fixture for the server-wide façade of the game-hook tier.
//!
//! The members real jars reach first after an object's own facts are about
//! the server as a whole: the world folder, the game rules, the registries,
//! whether a client takes a payload channel, and the login's connection and
//! profile (`facade_server.rs`, `game_hooks.rs`). This fixture compiles a
//! `PlayerList#placeNewPlayer` Mixin that asks each of them, fires the
//! `player_logged_in` point with a [`ServerFacts`] snapshot, and checks the
//! answers the callback saw (through the compile-only `demo/HookSink`, whose
//! calls the host records) against the facts, written out here by hand.
//!
//! The scratch jar lives under `.local/` (never the system temp directory).

use horizon_native_java::{
    ExecutionBudget, GameHookContext, GameHookState, GameObject, GameRuleValue, MixinSide,
    NativeJar, ServerFacts, StdHost, Value,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use zip::write::SimpleFileOptions;

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

/// Mixin's annotations and `CallbackInfo`, and the game, loader and authlib
/// classes the callback names: all compile-only, so every call on them
/// crosses the host boundary.
const COMPILE_ONLY: &[(&str, &str)] = &[
    (
        "org/spongepowered/asm/mixin/Mixin.java",
        "package org.spongepowered.asm.mixin;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.TYPE)\npublic @interface Mixin { Class<?>[] value() default {}; String[] targets() default {}; int priority() default 1000; }\n",
    ),
    (
        "org/spongepowered/asm/mixin/injection/At.java",
        "package org.spongepowered.asm.mixin.injection;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target({})\npublic @interface At { String value() default \"\"; }\n",
    ),
    (
        "org/spongepowered/asm/mixin/injection/Inject.java",
        "package org.spongepowered.asm.mixin.injection;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.METHOD)\npublic @interface Inject { String[] method(); At[] at(); boolean cancellable() default false; }\n",
    ),
    (
        "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
        "package org.spongepowered.asm.mixin.injection.callback;\npublic class CallbackInfo { public void cancel() {} }\n",
    ),
    (
        "demo/HookSink.java",
        "package demo;\npublic final class HookSink { public static void mark(String what) {} }\n",
    ),
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\npublic final class Identifier { public static Identifier fromNamespaceAndPath(String n, String p) { return null; } public static Identifier withDefaultNamespace(String p) { return null; } }\n",
    ),
    (
        "net/minecraft/resources/ResourceKey.java",
        "package net.minecraft.resources;\npublic final class ResourceKey<T> {}\n",
    ),
    (
        "net/minecraft/core/Registry.java",
        "package net.minecraft.core;\npublic interface Registry<T> { boolean containsKey(net.minecraft.resources.Identifier id); int size(); }\n",
    ),
    (
        "net/minecraft/core/RegistryAccess.java",
        "package net.minecraft.core;\npublic interface RegistryAccess { <E> Registry<E> lookupOrThrow(net.minecraft.resources.ResourceKey<? extends Registry<? extends E>> key); interface Frozen extends RegistryAccess {} }\n",
    ),
    (
        "net/minecraft/core/registries/Registries.java",
        "package net.minecraft.core.registries;\npublic final class Registries { public static final net.minecraft.resources.ResourceKey<net.minecraft.core.Registry<Object>> BLOCK = null; public static final net.minecraft.resources.ResourceKey<net.minecraft.core.Registry<Object>> BIOME = null; }\n",
    ),
    (
        "net/minecraft/core/registries/BuiltInRegistries.java",
        "package net.minecraft.core.registries;\npublic final class BuiltInRegistries { public static final net.minecraft.core.DefaultedRegistry<net.minecraft.world.item.Item> ITEM = null; }\n",
    ),
    (
        "net/minecraft/core/DefaultedRegistry.java",
        "package net.minecraft.core;\npublic interface DefaultedRegistry<T> extends Registry<T> { net.minecraft.resources.Identifier getKey(T value); }\n",
    ),
    (
        "net/minecraft/world/item/Item.java",
        "package net.minecraft.world.item;\npublic class Item {}\n",
    ),
    (
        "net/minecraft/world/item/ItemStack.java",
        "package net.minecraft.world.item;\npublic final class ItemStack { public Item getItem() { return null; } public int getCount() { return 0; } public boolean isEmpty() { return true; } }\n",
    ),
    (
        "net/minecraft/world/level/storage/LevelResource.java",
        "package net.minecraft.world.level.storage;\npublic final class LevelResource { public static final LevelResource ROOT = null; public LevelResource(String id) {} }\n",
    ),
    (
        "net/minecraft/world/level/gamerules/GameRule.java",
        "package net.minecraft.world.level.gamerules;\npublic final class GameRule<T> {}\n",
    ),
    (
        "net/minecraft/world/level/gamerules/GameRules.java",
        "package net.minecraft.world.level.gamerules;\npublic class GameRules { public static final GameRule<Boolean> KEEP_INVENTORY = null; public static final GameRule<Integer> RANDOM_TICK_SPEED = null; public <T> T get(GameRule<T> rule) { return null; } }\n",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\npublic class MinecraftServer { public java.nio.file.Path getWorldPath(net.minecraft.world.level.storage.LevelResource r) { return null; } public net.minecraft.core.RegistryAccess.Frozen registryAccess() { return null; } }\n",
    ),
    (
        "net/minecraft/world/level/Level.java",
        "package net.minecraft.world.level;\npublic class Level { public long getGameTime() { return 0; } }\n",
    ),
    (
        "net/minecraft/server/level/ServerLevel.java",
        "package net.minecraft.server.level;\npublic class ServerLevel extends net.minecraft.world.level.Level { public net.minecraft.world.level.gamerules.GameRules getGameRules() { return null; } }\n",
    ),
    (
        "net/minecraft/world/entity/player/Player.java",
        "package net.minecraft.world.entity.player;\npublic class Player { public net.minecraft.world.item.ItemStack getMainHandItem() { return null; } public net.minecraft.world.item.ItemStack getOffhandItem() { return null; } }\n",
    ),
    (
        "com/mojang/authlib/GameProfile.java",
        "package com.mojang.authlib;\npublic record GameProfile(java.util.UUID id, String name) {}\n",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\npublic class ServerPlayer extends net.minecraft.world.entity.player.Player { public net.minecraft.server.MinecraftServer getServer() { return null; } public ServerLevel level() { return null; } public com.mojang.authlib.GameProfile getGameProfile() { return null; } }\n",
    ),
    (
        "net/minecraft/network/Connection.java",
        "package net.minecraft.network;\npublic class Connection { public java.net.SocketAddress getRemoteAddress() { return null; } }\n",
    ),
    (
        "net/minecraft/server/network/CommonListenerCookie.java",
        "package net.minecraft.server.network;\npublic record CommonListenerCookie(com.mojang.authlib.GameProfile gameProfile) {}\n",
    ),
    (
        "net/minecraft/server/players/PlayerList.java",
        "package net.minecraft.server.players;\npublic class PlayerList { public void placeNewPlayer(net.minecraft.network.Connection c, net.minecraft.server.level.ServerPlayer p, net.minecraft.server.network.CommonListenerCookie k) {} }\n",
    ),
    (
        "net/minecraft/network/protocol/common/custom/CustomPacketPayload.java",
        "package net.minecraft.network.protocol.common.custom;\npublic interface CustomPacketPayload { final class Type<T extends CustomPacketPayload> { public Type(net.minecraft.resources.Identifier id) {} } }\n",
    ),
    (
        "net/fabricmc/fabric/api/networking/v1/ServerPlayNetworking.java",
        "package net.fabricmc.fabric.api.networking.v1;\npublic final class ServerPlayNetworking { public static boolean canSend(net.minecraft.server.level.ServerPlayer p, net.minecraft.network.protocol.common.custom.CustomPacketPayload.Type<?> t) { return false; } }\n",
    ),
];

/// Asks the server-wide façade everything the measured jars reached first,
/// marking each answer. The last line reaches a registry the facts do not
/// list, which must refuse.
const LOGIN_MIXIN: &str = r#"package demo;
import net.fabricmc.fabric.api.networking.v1.ServerPlayNetworking;
import net.minecraft.core.registries.BuiltInRegistries;
import net.minecraft.core.registries.Registries;
import net.minecraft.network.Connection;
import net.minecraft.network.protocol.common.custom.CustomPacketPayload;
import net.minecraft.resources.Identifier;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.network.CommonListenerCookie;
import net.minecraft.server.players.PlayerList;
import net.minecraft.world.level.gamerules.GameRules;
import net.minecraft.world.level.storage.LevelResource;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(PlayerList.class)
public class LoginMixin {
    static final CustomPacketPayload.Type<?> SYNC =
        new CustomPacketPayload.Type<>(Identifier.fromNamespaceAndPath("demo", "sync"));
    static final CustomPacketPayload.Type<?> OTHER =
        new CustomPacketPayload.Type<>(Identifier.fromNamespaceAndPath("demo", "other"));

    @Inject(method = "placeNewPlayer", at = @At("TAIL"))
    private void onJoin(Connection connection, ServerPlayer player, CommonListenerCookie cookie, CallbackInfo ci) {
        HookSink.mark("address/" + connection.getRemoteAddress().toString());
        HookSink.mark("profile/" + cookie.gameProfile().name() + "/" + player.getGameProfile().id().toString());
        HookSink.mark("config/" + player.getServer().getWorldPath(LevelResource.ROOT).resolve("serverconfig").toString());
        HookSink.mark("own/" + player.getServer().getWorldPath(new LevelResource("data/demo.json")).toString());
        GameRules rules = player.level().getGameRules();
        boolean keep = rules.get(GameRules.KEEP_INVENTORY);
        int speed = rules.get(GameRules.RANDOM_TICK_SPEED);
        HookSink.mark("rules/" + keep + "/" + speed);
        HookSink.mark("time/" + player.level().getGameTime());
        HookSink.mark("hand/" + BuiltInRegistries.ITEM.getKey(player.getMainHandItem().getItem()).toString()
            + "/" + player.getMainHandItem().getCount() + "/" + player.getOffhandItem().isEmpty());
        HookSink.mark("send/" + ServerPlayNetworking.canSend(player, SYNC) + "/" + ServerPlayNetworking.canSend(player, OTHER));
        HookSink.mark("stone/" + player.getServer().registryAccess().lookupOrThrow(Registries.BLOCK)
            .containsKey(Identifier.withDefaultNamespace("stone")));
        HookSink.mark("biomes/" + player.getServer().registryAccess().lookupOrThrow(Registries.BIOME).size());
    }
}
"#;

struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    fn build() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/native-java-game-hook")
            .join(format!("facade-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let classes = root.join("classes");
        fs::create_dir_all(&classes).unwrap();
        let mut paths = Vec::new();
        for (relative, body) in COMPILE_ONLY
            .iter()
            .copied()
            .chain([("demo/LoginMixin.java", LOGIN_MIXIN)])
        {
            let path = src.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            paths.push(path);
        }
        let status = Command::new(javac())
            .args(["--release", "17", "-d"])
            .arg(&classes)
            .args(paths)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed");
        let metadata = r#"{"id":"game-facade","version":"1.0","mixins":["demo.mixins.json"]}"#;
        let config = serde_json::json!({ "package": "demo", "mixins": ["LoginMixin"] }).to_string();
        let jar = root.join("facade.jar");
        let options = SimpleFileOptions::default();
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar).unwrap());
        for (path, body) in [("fabric.mod.json", metadata), ("demo.mixins.json", &config)] {
            zip.start_file(path, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        for class in files_under(&classes) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // Only the Mixin ships: everything it names is the host's.
            if entry.starts_with("demo/LoginMixin") {
                zip.start_file(entry, options).unwrap();
                zip.write_all(&fs::read(class).unwrap()).unwrap();
            }
        }
        zip.finish().unwrap();
        Self { root, jar }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn marks(host: &StdHost) -> Vec<String> {
    host.invocations()
        .iter()
        .filter(|call| call.method.owner == "demo/HookSink" && call.method.name == "mark")
        .filter_map(|call| match call.args.first() {
            Some(Value::Ref(Some(text))) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_login_callback_reads_the_server_from_its_facts_and_refuses_the_rest() {
    let fixture = Fixture::build();
    let jar = NativeJar::from_path(&fixture.jar).unwrap();
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    let registrations = jar.register_game_hooks(&weave).unwrap();
    assert_eq!(
        registrations.hooks().collect::<Vec<_>>(),
        ["player_logged_in"],
        "{:#?}",
        weave.skipped
    );

    let alex = 0x0123_4567_89ab_cdef_0011_2233_4455_6677_u128;
    let facts = Arc::new(ServerFacts {
        world_dir: Some("/horizon/game/world".into()),
        game_rules: BTreeMap::from([
            ("keep_inventory".into(), GameRuleValue::Bool(true)),
            ("random_tick_speed".into(), GameRuleValue::Int(3)),
        ]),
        registries: BTreeMap::from([(
            "minecraft:block".into(),
            Arc::new(BTreeSet::from(["minecraft:stone".to_owned()])),
        )]),
        client_channels: BTreeMap::from([(alex, BTreeSet::from(["demo:sync".to_owned()]))]),
        game_time: Some(1200),
        day_time: Some(6000),
    });
    // Lenient, so the compile-only `HookSink` calls are recorded; every
    // answer and refusal below is the hook tier's own.
    let mut host = StdHost::new();
    let mut state = GameHookState::default();
    let mut context = GameHookContext::new();
    context.set_server_facts(facts);
    context.insert(GameObject::server(true));
    let player = GameObject::player(alex, "Alex")
        .with_dimension("minecraft:overworld")
        .with_hands(("minecraft:diamond_sword", 1), ("minecraft:air", 0));
    let receiver = context.insert(player.clone());
    let connection = context.insert(
        GameObject::opaque("net/minecraft/network/Connection", "c0")
            .with_address("/127.0.0.1:52144"),
    );
    let joined = context.insert(player);
    let cookie = context.insert(
        GameObject::opaque("net/minecraft/server/network/CommonListenerCookie", "k0")
            .with_uuid(alex)
            .with_name("Alex"),
    );
    let error = jar
        .run_game_hook_in(
            &registrations,
            &mut state,
            &mut context,
            "player_logged_in",
            Some(receiver),
            &[connection, joined, cookie],
            &mut host,
            &ExecutionBudget::within(std::time::Duration::from_secs(5)),
        )
        .unwrap_err()
        .to_string();
    assert_eq!(
        marks(&host),
        [
            "address//127.0.0.1:52144",
            "profile/Alex/01234567-89ab-cdef-0011-223344556677",
            // Vanilla resolves `ROOT`'s id (".") under the world folder.
            "config//horizon/game/world/./serverconfig",
            // A resource the jar builds itself keeps the id it was given.
            "own//horizon/game/world/data/demo.json",
            "rules/true/3",
            "time/1200",
            "hand/minecraft:diamond_sword/1/true",
            // Alex's client takes `demo:sync` only.
            "send/true/false",
            "stone/true",
        ]
    );
    // Biomes are not in the facts: the lookup refuses by name instead of
    // answering an empty registry.
    assert!(error.contains("lookupOrThrow"), "{error}");
}
