//! Source-built jars register player-interaction listeners on Fabric phases
//! and NeoForge priorities; the listeners run in the loaders' order, get the
//! block and hand the game fired them with, and a deciding result stops the
//! rest the way Fabric's invoker and NeoForge's bus do.
//!
//! The loader and game classes are stubs so `javac` accepts the mods; only
//! the mods' own classes go into the jars, and the native host answers the
//! API classes exactly as it does for a real jar.

use horizon_native_java::{
    ExecutionBudget, Host, NativeJar, StdHost, Value,
    events::{
        EventContext, EventFacade, EventFiring, FacadePlayer, GameEvent, GameListener,
        ListenerOrder, dispatch_order,
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

const ALEX: u128 = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-event-order-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Compile `sources` and pack the `demo/` classes with `metadata`; `None`
/// without a Java toolchain.
fn build_jar(root: &Path, sources: &[(&str, &str)], metadata: (&str, &str)) -> Option<PathBuf> {
    let src = root.join("src");
    let classes = root.join("classes");
    let mut paths = Vec::new();
    for (path, text) in sources {
        let file = src.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, text).unwrap();
        paths.push(file);
    }
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
    zip.start_file(metadata.0, SimpleFileOptions::default())
        .unwrap();
    zip.write_all(metadata.1.as_bytes()).unwrap();
    for entry in fs::read_dir(classes.join("demo")).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("demo/{}", path.file_name().unwrap().to_string_lossy());
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(&path).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    Some(jar_path)
}

fn static_value(host: &StdHost, owner: &str, key: &str) -> Option<Value> {
    host.persistent_class_state()
        .0
        .remove(&format!("{owner}#{key}"))
}

fn facade() -> Arc<EventFacade> {
    Arc::new(EventFacade {
        players: vec![FacadePlayer {
            uuid: ALEX,
            name: "Alex".into(),
            level: "minecraft:overworld".into(),
            entity_id: 7,
        }],
        ..EventFacade::default()
    })
}

/// Fire `firing` the way the game does: every listener of its event in
/// [`dispatch_order`], stopping at a deciding Fabric answer. Returns how many
/// ran.
fn fire(
    jar: &NativeJar,
    host: &mut StdHost,
    listeners: &[GameListener],
    orderings: &[horizon_native_java::events::PhaseOrdering],
    firing: &mut EventFiring,
) -> usize {
    let mine: Vec<(usize, usize, &GameListener)> = listeners
        .iter()
        .enumerate()
        .filter(|(_, l)| l.event == firing.event)
        .map(|(at, l)| (0, at, l))
        .collect();
    let orderings: Vec<_> = orderings.iter().collect();
    let mut ran = 0;
    for (_, at) in dispatch_order(&mine, &orderings) {
        let run = jar
            .run_game_listener(&listeners[at], firing, host, &ExecutionBudget::UNBOUNDED)
            .unwrap();
        ran += usize::from(run.ran);
        if run.stop {
            break;
        }
    }
    ran
}

const FABRIC_STUBS: &[(&str, &str)] = &[
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\n\
         public final class Identifier {\n\
           public static Identifier fromNamespaceAndPath(String n, String p) { return null; } }",
    ),
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         import net.minecraft.resources.Identifier;\n\
         public abstract class Event<T> {\n\
           public static final Identifier DEFAULT_PHASE = null;\n\
           public void register(T listener) {}\n\
           public void register(Identifier phase, T listener) {}\n\
           public void addPhaseOrdering(Identifier first, Identifier second) {} }",
    ),
    (
        "net/minecraft/world/InteractionResult.java",
        "package net.minecraft.world;\n\
         public interface InteractionResult {\n\
           InteractionResult PASS = null; InteractionResult FAIL = null; InteractionResult SUCCESS = null; }",
    ),
    (
        "net/minecraft/world/InteractionHand.java",
        "package net.minecraft.world;\npublic enum InteractionHand { MAIN_HAND, OFF_HAND }",
    ),
    (
        "net/minecraft/world/entity/player/Player.java",
        "package net.minecraft.world.entity.player;\npublic class Player {}",
    ),
    (
        "net/minecraft/world/level/Level.java",
        "package net.minecraft.world.level;\npublic class Level {}",
    ),
    (
        "net/minecraft/world/phys/BlockHitResult.java",
        "package net.minecraft.world.phys;\npublic class BlockHitResult {}",
    ),
    (
        "net/minecraft/core/BlockPos.java",
        "package net.minecraft.core;\npublic class BlockPos { public int getY() { return 0; } }",
    ),
    (
        "net/minecraft/core/Direction.java",
        "package net.minecraft.core;\npublic enum Direction { DOWN, UP, NORTH, SOUTH, WEST, EAST }",
    ),
    (
        "net/fabricmc/fabric/api/event/player/UseBlockCallback.java",
        "package net.fabricmc.fabric.api.event.player;\n\
         import net.minecraft.world.*;\n\
         public interface UseBlockCallback {\n\
           net.fabricmc.fabric.api.event.Event<UseBlockCallback> EVENT = null;\n\
           InteractionResult interact(net.minecraft.world.entity.player.Player p, net.minecraft.world.level.Level l,\n\
             InteractionHand h, net.minecraft.world.phys.BlockHitResult hit); }",
    ),
    (
        "net/fabricmc/fabric/api/event/player/AttackBlockCallback.java",
        "package net.fabricmc.fabric.api.event.player;\n\
         import net.minecraft.world.*;\n\
         public interface AttackBlockCallback {\n\
           net.fabricmc.fabric.api.event.Event<AttackBlockCallback> EVENT = null;\n\
           InteractionResult interact(net.minecraft.world.entity.player.Player p, net.minecraft.world.level.Level l,\n\
             InteractionHand h, net.minecraft.core.BlockPos pos, net.minecraft.core.Direction d); }",
    ),
];

/// `order` collects the listeners' digits in the order they ran.
const FABRIC_MOD: &str = r#"
package demo;
import net.fabricmc.fabric.api.event.Event;
import net.fabricmc.fabric.api.event.player.AttackBlockCallback;
import net.fabricmc.fabric.api.event.player.UseBlockCallback;
import net.minecraft.core.Direction;
import net.minecraft.resources.Identifier;
import net.minecraft.world.InteractionHand;
import net.minecraft.world.InteractionResult;
public final class PhaseMod {
  public static int order;
  public static int lastY;
  public static int offHand;
  public static int up;
  public void onInitialize() {
    Identifier early = Identifier.fromNamespaceAndPath("demo", "zz_early");
    // Registered first, in the default phase: runs after `early`.
    UseBlockCallback.EVENT.register((p, l, h, hit) -> { order = order * 10 + 2; return InteractionResult.PASS; });
    UseBlockCallback.EVENT.register(early, (p, l, h, hit) -> { order = order * 10 + 1; return InteractionResult.PASS; });
    // Without this ordering `demo:zz_early` would sort after `fabric:default`.
    UseBlockCallback.EVENT.addPhaseOrdering(early, Event.DEFAULT_PHASE);
    UseBlockCallback.EVENT.register((p, l, h, hit) -> {
      order = order * 10 + 3;
      if (h == InteractionHand.OFF_HAND) offHand++;
      return InteractionResult.FAIL;
    });
    // Never reached: the FAIL before it decides the use.
    UseBlockCallback.EVENT.register((p, l, h, hit) -> { order = order * 10 + 4; return InteractionResult.SUCCESS; });
    AttackBlockCallback.EVENT.register((p, l, h, pos, dir) -> {
      lastY = pos.getY();
      if (dir == Direction.UP) up++;
      return pos.getY() > 64 ? InteractionResult.FAIL : InteractionResult.PASS;
    });
  }
}
"#;

#[test]
fn fabric_phases_order_listeners_and_a_deciding_result_stops_the_rest() {
    let root = scratch("fabric");
    let mut sources = FABRIC_STUBS.to_vec();
    sources.push(("demo/PhaseMod.java", FABRIC_MOD));
    let Some(jar_path) = build_jar(
        &root,
        &sources,
        (
            "fabric.mod.json",
            r#"{"id":"phase_mod","version":"1.0","entrypoints":{"main":["demo.PhaseMod"]}}"#,
        ),
    ) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    let found = host.game_listeners(&jar, true);
    let orders: Vec<&ListenerOrder> = found
        .listeners
        .iter()
        .filter(|l| l.event == GameEvent::UseBlock)
        .map(|l| &l.order)
        .collect();
    assert_eq!(
        orders,
        [
            &ListenerOrder::Phase("fabric:default".into()),
            &ListenerOrder::Phase("demo:zz_early".into()),
            &ListenerOrder::Phase("fabric:default".into()),
            &ListenerOrder::Phase("fabric:default".into()),
        ]
    );
    assert_eq!(found.phase_orderings.len(), 1);

    // The early phase, then the default phase in registration order, up to
    // the FAIL: the fourth listener never runs, and the use is decided.
    let mut firing = EventFiring::new(
        GameEvent::UseBlock,
        facade(),
        EventContext {
            level: Some("minecraft:overworld".into()),
            player: Some(ALEX),
            pos: Some([1, 70, 2]),
            face: Some([0, 1, 0]),
            off_hand: true,
            ..EventContext::default()
        },
    );
    let ran = fire(
        &jar,
        &mut host,
        &found.listeners,
        &found.phase_orderings,
        &mut firing,
    );
    assert_eq!(ran, 3);
    assert_eq!(
        static_value(&host, "demo/PhaseMod", "orderI"),
        Some(Value::Int(123))
    );
    assert_eq!(
        static_value(&host, "demo/PhaseMod", "offHandI"),
        Some(Value::Int(1))
    );
    assert!(firing.cancelled);
    assert_eq!(firing.interaction.as_deref(), Some("FAIL"));

    // The attack sees the block position and face it was fired with; a block
    // above y 64 is refused, one below passes.
    let attack = |pos: [i32; 3]| {
        EventFiring::new(
            GameEvent::AttackBlock,
            facade(),
            EventContext {
                level: Some("minecraft:overworld".into()),
                player: Some(ALEX),
                pos: Some(pos),
                face: Some([0, 1, 0]),
                ..EventContext::default()
            },
        )
    };
    let mut high = attack([0, 80, 0]);
    fire(&jar, &mut host, &found.listeners, &[], &mut high);
    assert_eq!(high.interaction.as_deref(), Some("FAIL"));
    let mut low = attack([0, 12, 0]);
    fire(&jar, &mut host, &found.listeners, &[], &mut low);
    assert!(!low.cancelled);
    assert_eq!(low.interaction, None);
    assert_eq!(
        static_value(&host, "demo/PhaseMod", "lastYI"),
        Some(Value::Int(12))
    );
    assert_eq!(
        static_value(&host, "demo/PhaseMod", "upI"),
        Some(Value::Int(2))
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
        "net/neoforged/bus/api/EventPriority.java",
        "package net.neoforged.bus.api;\npublic enum EventPriority { HIGHEST, HIGH, NORMAL, LOW, LOWEST }",
    ),
    (
        "net/neoforged/bus/api/SubscribeEvent.java",
        "package net.neoforged.bus.api;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface SubscribeEvent {\n\
           EventPriority priority() default EventPriority.NORMAL; boolean receiveCanceled() default false; }",
    ),
    (
        "net/neoforged/bus/api/Event.java",
        "package net.neoforged.bus.api;\npublic abstract class Event {}",
    ),
    (
        "net/neoforged/bus/api/IEventBus.java",
        "package net.neoforged.bus.api;\n\
         public interface IEventBus {\n\
           <T extends Event> void addListener(java.util.function.Consumer<T> listener);\n\
           <T extends Event> void addListener(EventPriority p, java.util.function.Consumer<T> listener);\n\
           <T extends Event> void addListener(EventPriority p, boolean receiveCanceled, java.util.function.Consumer<T> listener); }",
    ),
    (
        "net/neoforged/neoforge/common/NeoForge.java",
        "package net.neoforged.neoforge.common;\n\
         public final class NeoForge { public static final net.neoforged.bus.api.IEventBus EVENT_BUS = null; }",
    ),
    (
        "net/neoforged/neoforge/event/entity/player/PlayerInteractEvent.java",
        "package net.neoforged.neoforge.event.entity.player;\n\
         public class PlayerInteractEvent extends net.neoforged.bus.api.Event {\n\
           public void setCanceled(boolean c) {} public boolean isCanceled() { return false; }\n\
           public static class LeftClickBlock extends PlayerInteractEvent {} }",
    ),
];

const NEOFORGE_MOD: &str = r#"
package demo;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.common.NeoForge;
import net.neoforged.neoforge.event.entity.player.PlayerInteractEvent;
@Mod("neo_order") public final class NeoOrder {
  public static int order;
  public NeoOrder() {
    NeoForge.EVENT_BUS.addListener((PlayerInteractEvent.LeftClickBlock e) -> order = order * 10 + 3);
    NeoForge.EVENT_BUS.addListener(EventPriority.LOWEST, true,
        (PlayerInteractEvent.LeftClickBlock e) -> order = order * 10 + 5);
    NeoForge.EVENT_BUS.addListener(EventPriority.LOW, (PlayerInteractEvent.LeftClickBlock e) -> order = order * 10 + 4);
    NeoForge.EVENT_BUS.addListener(EventPriority.HIGHEST, (PlayerInteractEvent.LeftClickBlock e) -> {
      order = order * 10 + 1;
      e.setCanceled(true);
    });
  }
}
"#;

const NEOFORGE_SUBSCRIBER: &str = r#"
package demo;
import net.neoforged.bus.api.EventPriority;
import net.neoforged.bus.api.SubscribeEvent;
import net.neoforged.fml.common.EventBusSubscriber;
import net.neoforged.neoforge.event.entity.player.PlayerInteractEvent;
@EventBusSubscriber(modid = "neo_order") public final class HighSubscriber {
  @SubscribeEvent(priority = EventPriority.HIGH, receiveCanceled = true)
  public static void onClick(PlayerInteractEvent.LeftClickBlock event) {
    NeoOrder.order = NeoOrder.order * 10 + 2;
  }
}
"#;

#[test]
fn neoforge_priorities_order_listeners_and_cancellation_skips_the_unasked() {
    let root = scratch("neoforge");
    let mut sources = NEOFORGE_STUBS.to_vec();
    sources.push(("demo/NeoOrder.java", NEOFORGE_MOD));
    sources.push(("demo/HighSubscriber.java", NEOFORGE_SUBSCRIBER));
    let Some(jar_path) = build_jar(
        &root,
        &sources,
        (
            "META-INF/neoforge.mods.toml",
            "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\nlicense=\"MIT\"\n[[mods]]\nmodId=\"neo_order\"\nversion=\"1.0\"\n",
        ),
    ) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let mut host = StdHost::new();
    jar.execute("demo.NeoOrder", None, "neoforge", &mut host)
        .unwrap();
    let found = host.game_listeners(&jar, true);
    assert_eq!(found.listeners.len(), 5);
    assert!(
        found
            .listeners
            .iter()
            .all(|l| l.event == GameEvent::AttackBlock)
    );
    let mut firing = EventFiring::new(
        GameEvent::AttackBlock,
        facade(),
        EventContext {
            level: Some("minecraft:overworld".into()),
            player: Some(ALEX),
            pos: Some([0, 64, 0]),
            ..EventContext::default()
        },
    );
    let ran = fire(&jar, &mut host, &found.listeners, &[], &mut firing);
    // HIGHEST cancels; HIGH and LOWEST asked for cancelled events, NORMAL and
    // LOW did not.
    assert_eq!(
        static_value(&host, "demo/NeoOrder", "orderI"),
        Some(Value::Int(125))
    );
    assert_eq!(ran, 3);
    assert!(firing.cancelled);
    let _ = fs::remove_dir_all(root);
}
