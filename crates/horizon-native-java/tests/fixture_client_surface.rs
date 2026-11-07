//! Source-built Fabric and NeoForge client jars register key mappings, client
//! tick listeners and HUD elements the way real client mods do; the client
//! surface names each one, answers `consumeClick` from the key state it is
//! given, and records what a HUD element draws.
//!
//! The loader and game classes are compiled as stubs so `javac` accepts the
//! mods; only the mods' own classes go into the jars, so the native host
//! answers the API classes, exactly as it does for a real jar.

use horizon_native_java::{
    ExecutionBudget, Host, NativeJar, StdHost, Value,
    client_surface::{
        ClientKeyMapping, ClientMessage, ClientPlayer, ClientSnapshot, ClientText, DrawOp,
        KeyPress, TextMetrics,
    },
    events::{EventContext, EventFacade, EventFiring, EventLoader, GameEvent, GameListener},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
use zip::write::SimpleFileOptions;

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-client-surface-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// Compile `sources` and pack the classes under `demo/` into a jar with
/// `metadata` as its loader file. `None` without a Java toolchain.
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

/// A frame in a world with a player at `x`, or at the title screen.
fn frame(x: Option<f64>) -> Arc<ClientSnapshot> {
    Arc::new(ClientSnapshot {
        player: x.map(|x| ClientPlayer {
            name: "Alex".into(),
            pos: [x, 64.0, -3.0],
            health: 20.0,
            max_health: 20.0,
            ..ClientPlayer::default()
        }),
        gui_size: [320, 240],
        metrics: Arc::new(TextMetrics {
            advances: vec![6.0; 128],
            fallback: 6.0,
            translations: BTreeMap::new(),
        }),
        ..ClientSnapshot::default()
    })
}

/// Fire `event` at every listener of it, inside one client frame.
fn fire(
    jar: &NativeJar,
    host: &mut StdHost,
    listeners: &[GameListener],
    event: GameEvent,
    snapshot: Arc<ClientSnapshot>,
    keys: &[KeyPress],
) -> horizon_native_java::client_surface::ClientFrameOutput {
    host.begin_client_frame(snapshot, keys);
    let mut firing = EventFiring::new(
        event,
        Arc::new(EventFacade::default()),
        EventContext::default(),
    );
    for listener in listeners.iter().filter(|l| l.event == event) {
        jar.run_game_listener(listener, &mut firing, host, &ExecutionBudget::UNBOUNDED)
            .unwrap();
    }
    host.end_client_frame()
}

const GAME_STUBS: &[(&str, &str)] = &[
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\n\
         public final class Identifier { public static Identifier fromNamespaceAndPath(String n, String p) { return null; } }",
    ),
    (
        "net/minecraft/network/chat/Component.java",
        "package net.minecraft.network.chat;\n\
         public interface Component { static MutableComponent literal(String s) { return null; }\n\
           static MutableComponent translatable(String s) { return null; } }",
    ),
    (
        "net/minecraft/network/chat/MutableComponent.java",
        "package net.minecraft.network.chat;\npublic interface MutableComponent extends Component {}",
    ),
    (
        "com/mojang/blaze3d/platform/InputConstants.java",
        "package com.mojang.blaze3d.platform;\n\
         public final class InputConstants { public static final int KEY_C = 6;\n\
           public enum Type { KEYBOARD, MOUSE } }",
    ),
    (
        "net/minecraft/client/KeyMapping.java",
        "package net.minecraft.client;\n\
         public class KeyMapping {\n\
           public KeyMapping(String n, int k, Category c) {}\n\
           public KeyMapping(String n, com.mojang.blaze3d.platform.InputConstants.Type t, int k, Category c) {}\n\
           public boolean consumeClick() { return false; } public boolean isDown() { return false; }\n\
           public record Category(net.minecraft.resources.Identifier id) {\n\
             public static final Category MISC = null;\n\
             public static Category register(net.minecraft.resources.Identifier id) { return null; } } }",
    ),
    (
        "net/minecraft/client/gui/Font.java",
        "package net.minecraft.client.gui;\n\
         public class Font { public final int lineHeight = 9; public int width(String s) { return 0; } }",
    ),
    (
        "net/minecraft/client/gui/GuiGraphicsExtractor.java",
        "package net.minecraft.client.gui;\n\
         public class GuiGraphicsExtractor {\n\
           public void text(Font f, String s, int x, int y, int c) {}\n\
           public void text(Font f, net.minecraft.network.chat.Component s, int x, int y, int c, boolean shadow) {}\n\
           public void fill(int a, int b, int c, int d, int e) {}\n\
           public int guiWidth() { return 0; } }",
    ),
    (
        "net/minecraft/client/DeltaTracker.java",
        "package net.minecraft.client;\npublic interface DeltaTracker {}",
    ),
    (
        "net/minecraft/client/player/LocalPlayer.java",
        "package net.minecraft.client.player;\n\
         public class LocalPlayer { public double getX() { return 0; }\n\
           public void displayClientMessage(net.minecraft.network.chat.Component c, boolean overlay) {} }",
    ),
    (
        "net/minecraft/client/Minecraft.java",
        "package net.minecraft.client;\n\
         public class Minecraft { public net.minecraft.client.player.LocalPlayer player;\n\
           public net.minecraft.client.gui.Font font;\n\
           public static Minecraft getInstance() { return null; } }",
    ),
];

const FABRIC_STUBS: &[(&str, &str)] = &[
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         public abstract class Event<T> { public void register(T listener) {} }",
    ),
    (
        "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents.java",
        "package net.fabricmc.fabric.api.client.event.lifecycle.v1;\n\
         public final class ClientTickEvents {\n\
           public static final net.fabricmc.fabric.api.event.Event<EndTick> END_CLIENT_TICK = null;\n\
           public interface EndTick { void onEndTick(net.minecraft.client.Minecraft client); } }",
    ),
    (
        "net/fabricmc/fabric/api/client/keymapping/v1/KeyMappingHelper.java",
        "package net.fabricmc.fabric.api.client.keymapping.v1;\n\
         public final class KeyMappingHelper {\n\
           public static net.minecraft.client.KeyMapping registerKeyMapping(net.minecraft.client.KeyMapping k) { return k; } }",
    ),
    (
        "net/fabricmc/fabric/api/client/rendering/v1/hud/HudElement.java",
        "package net.fabricmc.fabric.api.client.rendering.v1.hud;\n\
         public interface HudElement { void extractRenderState(net.minecraft.client.gui.GuiGraphicsExtractor g, net.minecraft.client.DeltaTracker d); }",
    ),
    (
        "net/fabricmc/fabric/api/client/rendering/v1/hud/HudElementRegistry.java",
        "package net.fabricmc.fabric.api.client.rendering.v1.hud;\n\
         public interface HudElementRegistry {\n\
           static void addLast(net.minecraft.resources.Identifier id, HudElement e) {} }",
    ),
];

const FABRIC_MOD: &str = r#"
package demo;
import com.mojang.blaze3d.platform.InputConstants;
import net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents;
import net.fabricmc.fabric.api.client.keymapping.v1.KeyMappingHelper;
import net.fabricmc.fabric.api.client.rendering.v1.hud.HudElementRegistry;
import net.minecraft.client.KeyMapping;
import net.minecraft.client.Minecraft;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
public final class ClientMod {
  public static KeyMapping ZOOM;
  public static int presses;
  public static int ticks;
  public void onInitializeClient() {
    KeyMapping.Category category = KeyMapping.Category.register(Identifier.fromNamespaceAndPath("demo", "main"));
    ZOOM = KeyMappingHelper.registerKeyMapping(
        new KeyMapping("key.demo.zoom", InputConstants.Type.KEYBOARD, InputConstants.KEY_C, category));
    ClientTickEvents.END_CLIENT_TICK.register(client -> {
      ticks++;
      while (ZOOM.consumeClick()) {
        presses++;
        client.player.displayClientMessage(Component.literal("zoom " + presses), false);
      }
    });
    HudElementRegistry.addLast(Identifier.fromNamespaceAndPath("demo", "coords"), (graphics, delta) -> {
      Minecraft mc = Minecraft.getInstance();
      if (mc.player == null) return;
      graphics.text(mc.font, "X " + (int) mc.player.getX(), 2, 2, 0xFFFFFFFF);
      graphics.fill(0, 0, graphics.guiWidth(), 1, 0x80000000);
    });
  }
}
"#;

#[test]
fn a_fabric_client_mod_gets_its_key_tick_listener_and_hud_element() {
    let root = scratch("fabric");
    let mut sources = GAME_STUBS.to_vec();
    sources.extend_from_slice(FABRIC_STUBS);
    sources.push(("demo/ClientMod.java", FABRIC_MOD));
    let Some(jar_path) = build_jar(
        &root,
        &sources,
        (
            "fabric.mod.json",
            r#"{"id":"demo","version":"1","environment":"client","entrypoints":{"client":["demo.ClientMod"]}}"#,
        ),
    ) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("client").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "client", &mut host)
        .unwrap();

    // The tick listener is a client event: a client keeps it, a server
    // counts it as an event it never fires.
    let client = host.game_listeners(&jar, false);
    let named: Vec<(GameEvent, EventLoader, &str)> = client
        .listeners
        .iter()
        .map(|l| (l.event, l.loader, l.source.as_str()))
        .collect();
    assert_eq!(
        named,
        [(
            GameEvent::ClientTickEnd,
            EventLoader::Fabric,
            "ClientTickEvents.END_CLIENT_TICK"
        )]
    );
    let server = host.game_listeners(&jar, true);
    assert!(server.listeners.is_empty());
    assert_eq!(
        server.unsupported.get("ClientTickEvents.END_CLIENT_TICK"),
        Some(&1)
    );

    let zoom = static_value(
        &host,
        "demo/ClientMod",
        "ZOOMLnet/minecraft/client/KeyMapping;",
    )
    .and_then(|value| match value {
        Value::Ref(Some(marker)) => Some(marker),
        _ => None,
    })
    .expect("registerKeyMapping hands the mapping back");
    assert_eq!(
        host.client_key_mappings(),
        &[ClientKeyMapping {
            marker: zoom,
            name: "key.demo.zoom".into(),
            default_key: "key.keyboard.c".into(),
            category: "demo:main".into(),
            source: "KeyMappingHelper.registerKeyMapping",
        }]
    );
    let (layers, refused) = host.client_hud_layers(&jar);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].id, "demo:coords");

    // Two clicks waiting: the listener consumes both and says so in chat.
    let out = fire(
        &jar,
        &mut host,
        &client.listeners,
        GameEvent::ClientTickEnd,
        frame(Some(12.7)),
        &[KeyPress {
            down: true,
            clicks: 2,
        }],
    );
    assert_eq!(
        out.messages,
        [
            ClientMessage::Chat(ClientText::Literal("zoom 1".into())),
            ClientMessage::Chat(ClientText::Literal("zoom 2".into())),
        ]
    );
    assert_eq!(
        out.keys,
        [KeyPress {
            down: true,
            clicks: 0
        }]
    );
    // No click: the tick still runs, nothing is consumed.
    let out = fire(
        &jar,
        &mut host,
        &client.listeners,
        GameEvent::ClientTickEnd,
        frame(Some(12.7)),
        &[KeyPress::default()],
    );
    assert!(out.messages.is_empty());
    assert_eq!(
        static_value(&host, "demo/ClientMod", "ticksI"),
        Some(Value::Int(2))
    );
    assert_eq!(
        static_value(&host, "demo/ClientMod", "pressesI"),
        Some(Value::Int(2))
    );

    // The HUD element draws the player's x and a bar the GUI's width.
    host.begin_client_frame(frame(Some(12.7)), &[]);
    jar.run_hud_layer(&layers[0], &mut host, &ExecutionBudget::UNBOUNDED)
        .unwrap();
    let out = host.end_client_frame();
    assert_eq!(
        out.draws,
        [
            DrawOp::Text {
                text: ClientText::Literal("X 12".into()),
                at: [2.0, 2.0],
                argb: 0xFFFF_FFFF,
                shadow: true,
                scale: [1.0, 1.0],
            },
            DrawOp::Fill {
                rect: [0.0, 0.0, 320.0, 1.0],
                argb: 0x8000_0000,
            },
        ]
    );
    // Without a world there is no player, and the element draws nothing.
    host.begin_client_frame(frame(None), &[]);
    jar.run_hud_layer(&layers[0], &mut host, &ExecutionBudget::UNBOUNDED)
        .unwrap();
    assert!(host.end_client_frame().draws.is_empty());
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
        "net/neoforged/api/distmarker/Dist.java",
        "package net.neoforged.api.distmarker;\npublic enum Dist { CLIENT, DEDICATED_SERVER }",
    ),
    (
        "net/neoforged/fml/common/EventBusSubscriber.java",
        "package net.neoforged.fml.common;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface EventBusSubscriber {\n\
           String modid() default \"\"; net.neoforged.api.distmarker.Dist[] value() default {}; }",
    ),
    (
        "net/neoforged/bus/api/SubscribeEvent.java",
        "package net.neoforged.bus.api;\n\
         import java.lang.annotation.*;\n\
         @Retention(RetentionPolicy.RUNTIME) public @interface SubscribeEvent {}",
    ),
    (
        "net/neoforged/bus/api/Event.java",
        "package net.neoforged.bus.api;\npublic abstract class Event {}",
    ),
    (
        "net/neoforged/neoforge/client/event/RegisterKeyMappingsEvent.java",
        "package net.neoforged.neoforge.client.event;\n\
         public class RegisterKeyMappingsEvent extends net.neoforged.bus.api.Event {\n\
           public void register(net.minecraft.client.KeyMapping k) {} }",
    ),
    (
        "net/neoforged/neoforge/client/gui/GuiLayer.java",
        "package net.neoforged.neoforge.client.gui;\n\
         public interface GuiLayer { void render(net.minecraft.client.gui.GuiGraphicsExtractor g, net.minecraft.client.DeltaTracker d); }",
    ),
    (
        "net/neoforged/neoforge/client/event/RegisterGuiLayersEvent.java",
        "package net.neoforged.neoforge.client.event;\n\
         public class RegisterGuiLayersEvent extends net.neoforged.bus.api.Event {\n\
           public void registerAboveAll(net.minecraft.resources.Identifier id, net.neoforged.neoforge.client.gui.GuiLayer l) {} }",
    ),
    (
        "net/neoforged/neoforge/client/event/ClientTickEvent.java",
        "package net.neoforged.neoforge.client.event;\n\
         public abstract class ClientTickEvent extends net.neoforged.bus.api.Event {\n\
           public static class Post extends ClientTickEvent {} }",
    ),
];

const NEOFORGE_MOD: &str = r#"
package demo;
import net.minecraft.client.KeyMapping;
import net.minecraft.client.Minecraft;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
import net.neoforged.api.distmarker.Dist;
import net.neoforged.bus.api.SubscribeEvent;
import net.neoforged.fml.common.EventBusSubscriber;
import net.neoforged.fml.common.Mod;
import net.neoforged.neoforge.client.event.ClientTickEvent;
import net.neoforged.neoforge.client.event.RegisterGuiLayersEvent;
import net.neoforged.neoforge.client.event.RegisterKeyMappingsEvent;
@Mod("demonf")
@EventBusSubscriber(modid = "demonf", value = Dist.CLIENT)
public final class NeoClient {
  public static final KeyMapping OPEN = new KeyMapping("key.demonf.open", 7, KeyMapping.Category.MISC);
  public static int held;
  @SubscribeEvent public static void keys(RegisterKeyMappingsEvent event) { event.register(OPEN); }
  @SubscribeEvent public static void layers(RegisterGuiLayersEvent event) {
    event.registerAboveAll(Identifier.fromNamespaceAndPath("demonf", "title"),
        (graphics, delta) -> graphics.text(Minecraft.getInstance().font,
            Component.translatable("hud.demonf.title"), 4, 6, -1, false));
  }
  @SubscribeEvent public static void tick(ClientTickEvent.Post event) { if (OPEN.isDown()) held++; }
}
"#;

#[test]
fn a_neoforge_client_mod_registers_through_its_mod_bus_events() {
    let root = scratch("neoforge");
    let mut sources = GAME_STUBS.to_vec();
    sources.extend_from_slice(NEOFORGE_STUBS);
    sources.push(("demo/NeoClient.java", NEOFORGE_MOD));
    let Some(jar_path) = build_jar(
        &root,
        &sources,
        (
            "META-INF/neoforge.mods.toml",
            "modLoader=\"javafml\"\nloaderVersion=\"[1,)\"\nlicense=\"MIT\"\n[[mods]]\nmodId=\"demonf\"\nversion=\"1.0\"\n",
        ),
    ) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let mut host = StdHost::new();
    jar.execute("demo.NeoClient", None, "neoforge", &mut host)
        .unwrap();
    let found = host.game_listeners(&jar, false);
    let mut named: Vec<GameEvent> = found.listeners.iter().map(|l| l.event).collect();
    named.sort();
    assert_eq!(
        named,
        [
            GameEvent::ClientTickEnd,
            GameEvent::ClientRegisterKeyMappings,
            GameEvent::ClientRegisterGuiLayers,
        ]
    );
    // A server drops a `Dist.CLIENT` subscriber's methods.
    assert!(host.game_listeners(&jar, true).listeners.is_empty());

    // Nothing is registered until the client fires its registration events.
    assert!(host.client_key_mappings().is_empty());
    for event in [
        GameEvent::ClientRegisterKeyMappings,
        GameEvent::ClientRegisterGuiLayers,
    ] {
        fire(&jar, &mut host, &found.listeners, event, frame(None), &[]);
    }
    let keys = host.client_key_mappings();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        (
            keys[0].name.as_str(),
            keys[0].default_key.as_str(),
            keys[0].category.as_str(),
            keys[0].source
        ),
        (
            "key.demonf.open",
            "key.keyboard.d",
            "minecraft:misc",
            "RegisterKeyMappingsEvent.register"
        )
    );
    let (layers, _) = host.client_hud_layers(&jar);
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].id, "demonf:title");

    // `isDown` follows the key state: held for one tick, released the next.
    for down in [true, false] {
        fire(
            &jar,
            &mut host,
            &found.listeners,
            GameEvent::ClientTickEnd,
            frame(Some(0.0)),
            &[KeyPress { down, clicks: 0 }],
        );
    }
    assert_eq!(
        static_value(&host, "demo/NeoClient", "heldI"),
        Some(Value::Int(1))
    );
    host.begin_client_frame(frame(Some(0.0)), &[]);
    jar.run_hud_layer(&layers[0], &mut host, &ExecutionBudget::UNBOUNDED)
        .unwrap();
    assert_eq!(
        host.end_client_frame().draws,
        [DrawOp::Text {
            text: ClientText::Translatable("hud.demonf.title".into()),
            at: [4.0, 6.0],
            argb: 0xFFFF_FFFF,
            shadow: false,
            scale: [1.0, 1.0],
        }]
    );
    let _ = fs::remove_dir_all(root);
}
