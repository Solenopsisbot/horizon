//! Source-built Fabric and NeoForge jars register game-event listeners the way
//! real server mods do, and the listeners run when the event is fired.
//!
//! The loader and game classes are compiled as stubs so `javac` accepts the
//! mods, but only the mods' own classes go into the jars: the native host
//! answers the API classes, exactly as it does for a real jar.

use horizon_native_java::{
    ExecutionBudget, Host, NativeError, NativeJar, StdHost, Value,
    events::{
        EventContext, EventFacade, EventFiring, EventLoader, FacadeEntity, FacadePlayer, GameEvent,
        GameListener,
    },
};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use zip::write::SimpleFileOptions;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-game-events-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Compile `sources` (path → Java text) and pack the classes under `mod_root`
/// into a jar with `metadata` as its loader file.
fn build_jar(
    root: &Path,
    sources: &[(&str, &str)],
    mod_root: &str,
    metadata: (&str, &str),
) -> PathBuf {
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
        .unwrap();
    assert!(status.success(), "javac failed");
    let jar_path = root.join("mod.jar");
    let mut zip = zip::ZipWriter::new(fs::File::create(&jar_path).unwrap());
    zip.start_file(metadata.0, SimpleFileOptions::default())
        .unwrap();
    zip.write_all(metadata.1.as_bytes()).unwrap();
    let mut stack = vec![classes.join(mod_root)];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = path.strip_prefix(&classes).unwrap().to_string_lossy();
                zip.start_file(name.replace('\\', "/"), SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(&fs::read(&path).unwrap()).unwrap();
            }
        }
    }
    zip.finish().unwrap();
    jar_path
}

/// The value of a jar class's static field after the listeners ran. `key` is
/// `name` + descriptor (`ticksI`), the interpreter's own field key.
fn static_value(host: &StdHost, owner: &str, key: &str) -> Option<Value> {
    host.persistent_class_state()
        .0
        .remove(&format!("{owner}#{key}"))
}

fn facade(tick_count: i32) -> Arc<EventFacade> {
    Arc::new(EventFacade {
        tick_count,
        dedicated: true,
        max_players: 20,
        players: vec![FacadePlayer {
            uuid: 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210,
            name: "Alex".into(),
            level: "minecraft:overworld".into(),
            entity_id: 7,
        }],
        levels: vec![],
    })
}

/// Run every listener of `event` in registration order, as the game does.
fn fire(
    jar: &NativeJar,
    host: &mut StdHost,
    listeners: &[GameListener],
    firing: &mut EventFiring,
) -> Result<usize, NativeError> {
    let mut ran = 0;
    let event = firing.event;
    for listener in listeners.iter().filter(|l| l.event == event) {
        let run = jar.run_game_listener(listener, firing, host, &ExecutionBudget::UNBOUNDED)?;
        ran += usize::from(run.ran);
        if run.stop {
            break;
        }
    }
    Ok(ran)
}

const FABRIC_STUBS: &[(&str, &str)] = &[
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         public abstract class Event<T> { public void register(T listener) {} }",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\n\
         public class MinecraftServer { public int getTickCount() { return 0; }\n\
         public Object getCommands() { return null; }\n\
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
         public class ServerPlayer extends net.minecraft.world.entity.LivingEntity { public String getScoreboardName() { return null; } }",
    ),
    (
        "net/minecraft/world/entity/LivingEntity.java",
        "package net.minecraft.world.entity;\npublic class LivingEntity {}",
    ),
    (
        "net/minecraft/world/damagesource/DamageSource.java",
        "package net.minecraft.world.damagesource;\npublic class DamageSource {}",
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
        "net/fabricmc/fabric/api/event/lifecycle/v1/ServerLifecycleEvents.java",
        "package net.fabricmc.fabric.api.event.lifecycle.v1;\n\
         import net.fabricmc.fabric.api.event.Event;\n\
         import net.minecraft.server.MinecraftServer;\n\
         public final class ServerLifecycleEvents {\n\
           public static final Event<ServerStarted> SERVER_STARTED = null;\n\
           public static final Event<ServerStarting> SERVER_STARTING = null;\n\
           public interface ServerStarted { void onServerStarted(MinecraftServer server); }\n\
           public interface ServerStarting { void onServerStarting(MinecraftServer server); } }",
    ),
    (
        "net/fabricmc/fabric/api/entity/event/v1/ServerLivingEntityEvents.java",
        "package net.fabricmc.fabric.api.entity.event.v1;\n\
         import net.fabricmc.fabric.api.event.Event;\n\
         import net.minecraft.world.entity.LivingEntity;\n\
         import net.minecraft.world.damagesource.DamageSource;\n\
         public final class ServerLivingEntityEvents {\n\
           public static final Event<AllowDeath> ALLOW_DEATH = null;\n\
           public interface AllowDeath { boolean allowDeath(LivingEntity e, DamageSource s, float amount); } }",
    ),
    (
        "net/fabricmc/fabric/api/command/v2/CommandRegistrationCallback.java",
        "package net.fabricmc.fabric.api.command.v2;\n\
         import net.fabricmc.fabric.api.event.Event;\n\
         public interface CommandRegistrationCallback {\n\
           Event<CommandRegistrationCallback> EVENT = null;\n\
           void register(Object dispatcher, Object context, Object selection); }",
    ),
];

const FABRIC_MOD: &str = r#"
package demo;
import net.fabricmc.fabric.api.command.v2.CommandRegistrationCallback;
import net.fabricmc.fabric.api.entity.event.v1.ServerLivingEntityEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
public final class TickMod {
  public static int ticks;
  public static int lastTick;
  public static int started;
  public static String firstPlayer;
  public static int refused;
  static final class Started implements ServerLifecycleEvents.ServerStarted {
    public void onServerStarted(MinecraftServer server) {
      started++;
      for (ServerPlayer player : server.getPlayerList().getPlayers()) {
        firstPlayer = player.getScoreboardName();
      }
    }
  }
  public void onInitialize() {
    ServerTickEvents.END_SERVER_TICK.register(server -> {
      ticks++;
      lastTick = server.getTickCount();
    });
    ServerLifecycleEvents.SERVER_STARTED.register(new Started());
    ServerLifecycleEvents.SERVER_STARTING.register(TickMod::starting);
    ServerLivingEntityEvents.ALLOW_DEATH.register((entity, source, amount) -> amount < 10.0f);
    CommandRegistrationCallback.EVENT.register((dispatcher, context, selection) -> { });
  }
  static void starting(MinecraftServer server) {
    refused++;
    server.getCommands();
  }
}
"#;

#[test]
fn fabric_listeners_keep_their_event_and_run_once_per_firing() {
    let root = scratch("fabric");
    let mut sources = FABRIC_STUBS.to_vec();
    sources.push(("demo/TickMod.java", FABRIC_MOD));
    let jar_path = build_jar(
        &root,
        &sources,
        "demo",
        (
            "fabric.mod.json",
            r#"{"id":"tick_mod","version":"1.0","entrypoints":{"main":["demo.TickMod"]}}"#,
        ),
    );
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();

    let found = host.game_listeners(&jar, true);
    let named: Vec<(GameEvent, &str)> = found
        .listeners
        .iter()
        .map(|l| (l.event, l.source.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            (GameEvent::ServerTickEnd, "ServerTickEvents.END_SERVER_TICK"),
            (
                GameEvent::ServerStarted,
                "ServerLifecycleEvents.SERVER_STARTED"
            ),
            (
                GameEvent::ServerStarting,
                "ServerLifecycleEvents.SERVER_STARTING"
            ),
            (
                GameEvent::LivingAllowDeath,
                "ServerLivingEntityEvents.ALLOW_DEATH"
            ),
        ]
    );
    assert!(
        found
            .listeners
            .iter()
            .all(|l| l.loader == EventLoader::Fabric)
    );
    // The command callback is registered on an event the game never fires.
    assert_eq!(
        found
            .unsupported
            .get("net/fabricmc/fabric/api/command/v2/CommandRegistrationCallback.EVENT"),
        Some(&1)
    );

    // Five ticks, numbered 100..=104: the listener runs five times and its
    // last read of `getTickCount` is the last tick's.
    let calls_before = host.invocations().len();
    for tick in 100..105 {
        let mut firing = EventFiring::new(
            GameEvent::ServerTickEnd,
            facade(tick),
            EventContext::default(),
        );
        assert_eq!(
            fire(&jar, &mut host, &found.listeners, &mut firing).unwrap(),
            1
        );
        assert!(!firing.cancelled);
    }
    assert_eq!(
        static_value(&host, "demo/TickMod", "ticksI"),
        Some(Value::Int(5))
    );
    assert_eq!(
        static_value(&host, "demo/TickMod", "lastTickI"),
        Some(Value::Int(104))
    );
    // Firing does not grow the host's call log.
    assert_eq!(host.invocations().len(), calls_before);

    // An object listener walks the façade's player list.
    let mut firing = EventFiring::new(GameEvent::ServerStarted, facade(0), EventContext::default());
    assert_eq!(
        fire(&jar, &mut host, &found.listeners, &mut firing).unwrap(),
        1
    );
    assert_eq!(
        static_value(&host, "demo/TickMod", "startedI"),
        Some(Value::Int(1))
    );
    assert_eq!(
        static_value(&host, "demo/TickMod", "firstPlayerLjava/lang/String;"),
        Some(Value::Ref(Some("Alex".into())))
    );

    // `ALLOW_DEATH` cancels when a listener answers false.
    let death = |amount: f32| EventContext {
        level: Some("minecraft:overworld".into()),
        entity: Some(FacadeEntity {
            id: 3,
            kind: "minecraft:zombie".into(),
            level: "minecraft:overworld".into(),
        }),
        damage: Some("minecraft:player_attack".into()),
        amount,
        ..EventContext::default()
    };
    let mut firing = EventFiring::new(GameEvent::LivingAllowDeath, facade(0), death(4.0));
    fire(&jar, &mut host, &found.listeners, &mut firing).unwrap();
    assert!(!firing.cancelled);
    let mut firing = EventFiring::new(GameEvent::LivingAllowDeath, facade(0), death(12.0));
    fire(&jar, &mut host, &found.listeners, &mut firing).unwrap();
    assert!(firing.cancelled);

    // A call the façade does not model refuses the listener by name; what the
    // listener wrote before it stays written.
    let mut firing = EventFiring::new(
        GameEvent::ServerStarting,
        facade(0),
        EventContext::default(),
    );
    match fire(&jar, &mut host, &found.listeners, &mut firing) {
        Err(NativeError::UnknownHostCall(method)) => assert_eq!(method.name, "getCommands"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        static_value(&host, "demo/TickMod", "refusedI"),
        Some(Value::Int(1))
    );
    let _ = fs::remove_dir_all(root);
}

const NEOFORGE_STUBS: &[(&str, &str)] = &[
    (
        "net/neoforged/fml/common/Mod.java",
        "package net.neoforged.fml.common;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface Mod { String value(); }",
    ),
    (
        "net/neoforged/fml/common/EventBusSubscriber.java",
        "package net.neoforged.fml.common;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface EventBusSubscriber { String modid() default \"\"; }",
    ),
    (
        "net/neoforged/bus/api/SubscribeEvent.java",
        "package net.neoforged.bus.api;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface SubscribeEvent { boolean receiveCanceled() default false; }",
    ),
    (
        "net/neoforged/bus/api/Event.java",
        "package net.neoforged.bus.api;\npublic abstract class Event {}",
    ),
    (
        "net/neoforged/bus/api/IEventBus.java",
        "package net.neoforged.bus.api;\n\
         public interface IEventBus { <T extends Event> void addListener(java.util.function.Consumer<T> listener); }",
    ),
    (
        "net/neoforged/neoforge/common/NeoForge.java",
        "package net.neoforged.neoforge.common;\n\
         public final class NeoForge { public static final net.neoforged.bus.api.IEventBus EVENT_BUS = null; }",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\npublic class MinecraftServer { public int getTickCount() { return 0; } }",
    ),
    (
        "net/minecraft/world/entity/player/Player.java",
        "package net.minecraft.world.entity.player;\npublic class Player { public String getScoreboardName() { return null; } }",
    ),
    (
        "net/neoforged/neoforge/event/tick/ServerTickEvent.java",
        "package net.neoforged.neoforge.event.tick;\n\
         public abstract class ServerTickEvent extends net.neoforged.bus.api.Event {\n\
           public net.minecraft.server.MinecraftServer getServer() { return null; }\n\
           public static class Post extends ServerTickEvent {} }",
    ),
    (
        "net/neoforged/neoforge/event/entity/living/LivingDeathEvent.java",
        "package net.neoforged.neoforge.event.entity.living;\n\
         public class LivingDeathEvent extends net.neoforged.bus.api.Event {\n\
           public void setCanceled(boolean c) {} public boolean isCanceled() { return false; } }",
    ),
    (
        "net/neoforged/neoforge/event/entity/player/PlayerEvent.java",
        "package net.neoforged.neoforge.event.entity.player;\n\
         public class PlayerEvent extends net.neoforged.bus.api.Event {\n\
           public net.minecraft.world.entity.player.Player getEntity() { return null; }\n\
           public static class PlayerLoggedInEvent extends PlayerEvent {} }",
    ),
];

const NEOFORGE_MOD: &str = r#"
package demo;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.entity.living.LivingDeathEvent;
import net.neoforged.neoforge.event.tick.ServerTickEvent;
@Mod("neo_events") public final class NeoMod {
  public static int ticks;
  public static int lastTick;
  public static int sawCancelled;
  public NeoMod() {
    NeoForge.EVENT_BUS.addListener(this::onTick);
    NeoForge.EVENT_BUS.addListener((LivingDeathEvent event) -> event.setCanceled(true));
    NeoForge.EVENT_BUS.addListener((LivingDeathEvent event) -> sawCancelled++);
  }
  private void onTick(ServerTickEvent.Post event) {
    ticks++;
    lastTick = event.getServer().getTickCount();
  }
}
"#;

const NEOFORGE_SUBSCRIBER: &str = r#"
package demo;
import net.neoforged.bus.api.SubscribeEvent;
import net.neoforged.fml.common.EventBusSubscriber;
import net.neoforged.neoforge.event.entity.player.PlayerEvent;
@EventBusSubscriber(modid = "neo_events") public final class Logins {
  public static String last;
  @SubscribeEvent public static void onLogin(PlayerEvent.PlayerLoggedInEvent event) {
    last = event.getEntity().getScoreboardName();
  }
}
"#;

#[test]
fn neoforge_listeners_run_by_event_class_and_honour_set_canceled() {
    let root = scratch("neoforge");
    let mut sources = NEOFORGE_STUBS.to_vec();
    sources.push(("demo/NeoMod.java", NEOFORGE_MOD));
    sources.push(("demo/Logins.java", NEOFORGE_SUBSCRIBER));
    let jar_path = build_jar(
        &root,
        &sources,
        "demo",
        (
            "META-INF/neoforge.mods.toml",
            "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\nlicense=\"MIT\"\n[[mods]]\nmodId=\"neo_events\"\nversion=\"1.0\"\n",
        ),
    );
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let mut host = StdHost::new();
    jar.execute("demo.NeoMod", None, "neoforge", &mut host)
        .unwrap();
    let found = host.game_listeners(&jar, true);
    let named: Vec<(GameEvent, EventLoader)> = found
        .listeners
        .iter()
        .map(|l| (l.event, l.loader))
        .collect();
    assert_eq!(
        named,
        [
            (GameEvent::ServerTickEnd, EventLoader::NeoForge),
            (GameEvent::LivingAllowDeath, EventLoader::NeoForge),
            (GameEvent::LivingAllowDeath, EventLoader::NeoForge),
            (GameEvent::PlayerJoin, EventLoader::NeoForge),
        ]
    );

    for tick in 1..=3 {
        let mut firing = EventFiring::new(
            GameEvent::ServerTickEnd,
            facade(tick * 10),
            EventContext::default(),
        );
        assert_eq!(
            fire(&jar, &mut host, &found.listeners, &mut firing).unwrap(),
            1
        );
    }
    assert_eq!(
        static_value(&host, "demo/NeoMod", "ticksI"),
        Some(Value::Int(3))
    );
    assert_eq!(
        static_value(&host, "demo/NeoMod", "lastTickI"),
        Some(Value::Int(30))
    );

    // The first death listener cancels; the second did not ask for cancelled
    // events, so it is skipped.
    let mut firing = EventFiring::new(
        GameEvent::LivingAllowDeath,
        facade(0),
        EventContext {
            player: Some(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210),
            ..EventContext::default()
        },
    );
    assert_eq!(
        fire(&jar, &mut host, &found.listeners, &mut firing).unwrap(),
        1
    );
    assert!(firing.cancelled);
    assert_eq!(static_value(&host, "demo/NeoMod", "sawCancelledI"), None);

    // The `@EventBusSubscriber` static method runs without any `register`.
    let mut firing = EventFiring::new(
        GameEvent::PlayerJoin,
        facade(0),
        EventContext {
            player: Some(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210),
            ..EventContext::default()
        },
    );
    assert_eq!(
        fire(&jar, &mut host, &found.listeners, &mut firing).unwrap(),
        1
    );
    assert_eq!(
        static_value(&host, "demo/Logins", "lastLjava/lang/String;"),
        Some(Value::Ref(Some("Alex".into())))
    );
    let _ = fs::remove_dir_all(root);
}
