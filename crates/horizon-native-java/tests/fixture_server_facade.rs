//! A source-built Fabric jar reaches the server façade's second round the
//! way real server mods do: saved data through `getDataStorage()`, a task
//! handed to `server.execute`, the server's running thread, its layered
//! registries, its command dispatcher and a player's permissions
//! (`facade_server.rs`, `facade_saved_data.rs`).
//!
//! The game classes are compile-only stubs: every call on them crosses the
//! host boundary, as for a real jar. The expected values are written out
//! here from vanilla's contracts, not read back from the model.

use horizon_native_java::{
    ExecutionBudget, Host, NativeJar, ServerFacts, StdHost, Value,
    events::{EventContext, EventFacade, EventFiring, FacadePlayer, GameEvent, GameListener},
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

const ALEX: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

fn scratch() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/test-scratch")
        .join(format!("server-facade-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

fn build_jar(root: &Path, sources: &[(&str, &str)]) -> Option<PathBuf> {
    let src = root.join("src");
    let classes = root.join("classes");
    let mut paths = Vec::new();
    for (path, text) in sources {
        let file = src.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        paths.push(file);
    }
    fs::create_dir_all(&classes).unwrap();
    let javac = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin/javac"))
        .unwrap_or_else(|| "javac".into());
    let status = Command::new(javac)
        .args(["--release", "17", "-d"])
        .arg(&classes)
        .args(&paths)
        .status()
        .ok()?;
    assert!(status.success(), "javac failed");
    let jar_path = root.join("mod.jar");
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(
        br#"{"id":"facade_demo","version":"1.0","entrypoints":{"main":["demo.FacadeMod"]}}"#,
    )
    .unwrap();
    for entry in fs::read_dir(classes.join("demo")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.strip_prefix(&classes).unwrap().to_string_lossy();
        zip.start_file(name.replace('\\', "/"), SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&fs::read(&path).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    Some(jar_path)
}

const STUBS: &[(&str, &str)] = &[
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         public abstract class Event<T> { public void register(T listener) {} }",
    ),
    (
        "net/fabricmc/fabric/api/event/lifecycle/v1/ServerLifecycleEvents.java",
        "package net.fabricmc.fabric.api.event.lifecycle.v1;\n\
         import net.fabricmc.fabric.api.event.Event;\n\
         import net.minecraft.server.MinecraftServer;\n\
         public final class ServerLifecycleEvents {\n\
           public static final Event<ServerStarted> SERVER_STARTED = null;\n\
           public interface ServerStarted { void onServerStarted(MinecraftServer server); } }",
    ),
    (
        "net/fabricmc/fabric/api/event/lifecycle/v1/ServerTickEvents.java",
        "package net.fabricmc.fabric.api.event.lifecycle.v1;\n\
         import net.fabricmc.fabric.api.event.Event;\n\
         import net.minecraft.server.MinecraftServer;\n\
         public final class ServerTickEvents {\n\
           public static final Event<EndTick> END_SERVER_TICK = null;\n\
           public interface EndTick { void onEndTick(MinecraftServer server); } }",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\n\
         public class MinecraftServer {\n\
           public net.minecraft.world.level.storage.SavedDataStorage getDataStorage() { return null; }\n\
           public Thread getRunningThread() { return null; }\n\
           public void execute(Runnable task) {}\n\
           public net.minecraft.core.LayeredRegistryAccess registries() { return null; }\n\
           public net.minecraft.commands.Commands getCommands() { return null; }\n\
           public net.minecraft.server.players.PlayerList getPlayerList() { return null; } }",
    ),
    (
        "net/minecraft/server/players/PlayerList.java",
        "package net.minecraft.server.players;\n\
         public class PlayerList { public java.util.List<net.minecraft.server.level.ServerPlayer> getPlayers() { return null; } }",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\n\
         public class ServerPlayer { public net.minecraft.server.network.ServerGamePacketListenerImpl connection;\n\
           public net.minecraft.server.permissions.PermissionSet permissions() { return null; } }",
    ),
    (
        "net/minecraft/server/network/ServerGamePacketListenerImpl.java",
        "package net.minecraft.server.network;\n\
         public class ServerGamePacketListenerImpl { public net.minecraft.server.level.ServerPlayer player; }",
    ),
    (
        "net/minecraft/server/permissions/Permission.java",
        "package net.minecraft.server.permissions;\npublic interface Permission {}",
    ),
    (
        "net/minecraft/server/permissions/Permissions.java",
        "package net.minecraft.server.permissions;\n\
         public final class Permissions { public static final Permission COMMANDS_GAMEMASTER = null; public static final Permission COMMANDS_OWNER = null; }",
    ),
    (
        "net/minecraft/server/permissions/PermissionSet.java",
        "package net.minecraft.server.permissions;\n\
         public interface PermissionSet { boolean hasPermission(Permission p); }",
    ),
    (
        "net/minecraft/core/LayeredRegistryAccess.java",
        "package net.minecraft.core;\n\
         public class LayeredRegistryAccess { public RegistryAccess.Frozen compositeAccess() { return null; } }",
    ),
    (
        "net/minecraft/core/RegistryAccess.java",
        "package net.minecraft.core;\n\
         public interface RegistryAccess { interface Frozen extends RegistryAccess {} }",
    ),
    (
        "net/minecraft/commands/Commands.java",
        "package net.minecraft.commands;\n\
         public class Commands { public com.mojang.brigadier.CommandDispatcher<Object> getDispatcher() { return null; } }",
    ),
    (
        "com/mojang/brigadier/CommandDispatcher.java",
        "package com.mojang.brigadier;\npublic class CommandDispatcher<S> {}",
    ),
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\n\
         public final class Identifier { public static Identifier fromNamespaceAndPath(String n, String p) { return null; } }",
    ),
    (
        "com/mojang/serialization/Codec.java",
        "package com.mojang.serialization;\n\
         public interface Codec<T> { static <T> Codec<T> unit(T value) { return null; } }",
    ),
    (
        "net/minecraft/util/datafix/DataFixTypes.java",
        "package net.minecraft.util.datafix;\npublic enum DataFixTypes { SAVED_DATA_COMMAND_STORAGE }",
    ),
    (
        "net/minecraft/world/level/saveddata/SavedData.java",
        "package net.minecraft.world.level.saveddata;\n\
         public abstract class SavedData { public void setDirty() {} public boolean isDirty() { return false; } }",
    ),
    (
        "net/minecraft/world/level/saveddata/SavedDataType.java",
        "package net.minecraft.world.level.saveddata;\n\
         public final class SavedDataType<T extends SavedData> {\n\
           public SavedDataType(net.minecraft.resources.Identifier id, java.util.function.Supplier<T> constructor,\n\
             com.mojang.serialization.Codec<T> codec, net.minecraft.util.datafix.DataFixTypes fix) {} }",
    ),
    (
        "net/minecraft/world/level/storage/SavedDataStorage.java",
        "package net.minecraft.world.level.storage;\n\
         import net.minecraft.world.level.saveddata.*;\n\
         public class SavedDataStorage {\n\
           public <T extends SavedData> T computeIfAbsent(SavedDataType<T> type) { return null; }\n\
           public <T extends SavedData> T get(SavedDataType<T> type) { return null; } }",
    ),
];

const MOD: &str = r#"
package demo;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.minecraft.resources.Identifier;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.permissions.Permissions;
import net.minecraft.world.level.saveddata.SavedData;
import net.minecraft.world.level.saveddata.SavedDataType;
public final class FacadeMod {
  public static final class Visits extends SavedData {
    public int count;
  }
  // A new type object on every call, as graves builds its own: the
  // storage keys by the type's id, so both reach one object.
  static SavedDataType<Visits> type() {
    return new SavedDataType<>(Identifier.fromNamespaceAndPath("facade_demo", "visits"),
        Visits::new, com.mojang.serialization.Codec.unit(null), null);
  }
  public static int visits;
  public static int dirty;
  public static int absentBefore;
  public static int executed;
  public static int sameThread;
  public static int gamemasters;
  public static int owners;
  public static int composite;
  public static int dispatcher;
  public static int connectionBack;
  public void onInitialize() {
    ServerLifecycleEvents.SERVER_STARTED.register(server -> {
      absentBefore = server.getDataStorage().get(type()) == null ? 1 : 0;
      Visits v = server.getDataStorage().computeIfAbsent(type());
      v.count++;
      v.setDirty();
      dirty = v.isDirty() ? 1 : 0;
      composite = server.registries().compositeAccess() != null ? 1 : 0;
      dispatcher = server.getCommands().getDispatcher() != null ? 1 : 0;
    });
    ServerTickEvents.END_SERVER_TICK.register(server -> {
      Visits v = server.getDataStorage().computeIfAbsent(type());
      v.count++;
      visits = v.count;
      server.execute(() -> executed++);
      sameThread = server.getRunningThread() == Thread.currentThread() ? 1 : 0;
      for (ServerPlayer player : server.getPlayerList().getPlayers()) {
        if (player.permissions().hasPermission(Permissions.COMMANDS_GAMEMASTER)) gamemasters++;
        if (player.permissions().hasPermission(Permissions.COMMANDS_OWNER)) owners++;
        // A field read with its receiver: the player's connection, and back.
        if (player.connection.player == player) connectionBack++;
      }
    });
  }
}
"#;

fn facade() -> Arc<EventFacade> {
    Arc::new(EventFacade {
        dedicated: true,
        max_players: 20,
        players: vec![FacadePlayer {
            uuid: ALEX,
            name: "Alex".into(),
            level: "minecraft:overworld".into(),
            entity_id: 7,
        }],
        server: Arc::new(ServerFacts {
            registries: BTreeMap::from([(
                "minecraft:block".into(),
                Arc::new(BTreeSet::from(["minecraft:stone".to_owned()])),
            )]),
            // Alex is a level-2 operator: a gamemaster, not an owner.
            permission_levels: BTreeMap::from([(ALEX, 2)]),
            ..ServerFacts::default()
        }),
        ..EventFacade::default()
    })
}

fn fire(jar: &NativeJar, host: &mut StdHost, listeners: &[GameListener], event: GameEvent) {
    let mut firing = EventFiring::new(event, facade(), EventContext::default());
    for listener in listeners.iter().filter(|l| l.event == event) {
        jar.run_game_listener(listener, &mut firing, host, &ExecutionBudget::UNBOUNDED)
            .unwrap_or_else(|error| panic!("{event:?} refused: {error}"));
    }
}

fn int(host: &StdHost, field: &str) -> Option<Value> {
    host.persistent_class_state()
        .0
        .remove(&format!("demo/FacadeMod#{field}I"))
}

#[test]
fn listeners_keep_saved_data_run_tasks_and_read_permissions_like_the_server() {
    let root = scratch();
    let mut sources = STUBS.to_vec();
    sources.push(("demo/FacadeMod.java", MOD));
    let Some(jar_path) = build_jar(&root, &sources) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    let listeners = host.game_listeners(&jar, true).listeners;

    fire(&jar, &mut host, &listeners, GameEvent::ServerStarted);
    // A new world has no data until `computeIfAbsent` builds it.
    assert_eq!(int(&host, "absentBefore"), Some(Value::Int(1)));
    assert_eq!(int(&host, "dirty"), Some(Value::Int(1)));
    assert_eq!(int(&host, "composite"), Some(Value::Int(1)));
    assert_eq!(int(&host, "dispatcher"), Some(Value::Int(1)));

    for _ in 0..3 {
        fire(&jar, &mut host, &listeners, GameEvent::ServerTickEnd);
    }
    // One object for the life of the server: started (1) + three ticks.
    assert_eq!(int(&host, "visits"), Some(Value::Int(4)));
    // `execute` on the server thread runs each task at once.
    assert_eq!(int(&host, "executed"), Some(Value::Int(3)));
    assert_eq!(int(&host, "sameThread"), Some(Value::Int(1)));
    // A level-2 operator passes the gamemaster check every tick, never owner.
    assert_eq!(int(&host, "gamemasters"), Some(Value::Int(3)));
    assert_eq!(int(&host, "connectionBack"), Some(Value::Int(3)));
    // (A static the jar never wrote reads its default, 0.)
    assert!(matches!(int(&host, "owners"), None | Some(Value::Int(0))));
    let _ = fs::remove_dir_all(&root);
}
