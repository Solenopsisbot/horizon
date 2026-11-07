//! A source-built HUD mod shaped like the corpus' durability and day-counter
//! HUDs: its element gates on a cloth-config `AutoConfig` object, reads the
//! held stack's durability, draws the stack with its decorations and a
//! styled label, and queues a task with `Minecraft.execute`. Only the mod's
//! classes go into the jar; the config library and the game are stubs for
//! `javac`, so the host answers them, as for a real jar run alone.

use horizon_native_java::{
    ExecutionBudget, NativeJar, StdHost, Value,
    client_surface::{
        ClientPlayer, ClientSnapshot, ClientStack, ClientText, DrawOp, StackSlot, TextMetrics,
    },
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

fn build_jar(root: &Path, sources: &[(&str, &str)], metadata: &str) -> Option<PathBuf> {
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
    zip.write_all(metadata.as_bytes()).unwrap();
    for entry in fs::read_dir(classes.join("demo")).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("demo/{}", path.file_name().unwrap().to_string_lossy());
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(&path).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    Some(jar_path)
}

const STUBS: &[(&str, &str)] = &[
    (
        "net/minecraft/resources/Identifier.java",
        "package net.minecraft.resources;\n\
         public final class Identifier { public static Identifier fromNamespaceAndPath(String n, String p) { return null; } }",
    ),
    (
        "net/minecraft/ChatFormatting.java",
        "package net.minecraft;\npublic enum ChatFormatting { GOLD, BOLD }",
    ),
    (
        "net/minecraft/network/chat/Component.java",
        "package net.minecraft.network.chat;\n\
         public interface Component { static MutableComponent literal(String s) { return null; } }",
    ),
    (
        "net/minecraft/network/chat/MutableComponent.java",
        "package net.minecraft.network.chat;\n\
         public interface MutableComponent extends Component { MutableComponent withStyle(net.minecraft.ChatFormatting f); }",
    ),
    (
        "net/minecraft/world/item/ItemStack.java",
        "package net.minecraft.world.item;\n\
         public class ItemStack { public boolean isEmpty() { return true; } public int getMaxDamage() { return 0; }\n\
           public int getDamageValue() { return 0; } }",
    ),
    (
        "net/minecraft/client/gui/Font.java",
        "package net.minecraft.client.gui;\npublic class Font {}",
    ),
    (
        "net/minecraft/client/gui/GuiGraphicsExtractor.java",
        "package net.minecraft.client.gui;\n\
         public class GuiGraphicsExtractor {\n\
           public void text(Font f, net.minecraft.network.chat.Component s, int x, int y, int c, boolean shadow) {}\n\
           public void item(net.minecraft.world.item.ItemStack s, int x, int y) {}\n\
           public void itemDecorations(Font f, net.minecraft.world.item.ItemStack s, int x, int y) {} }",
    ),
    (
        "net/minecraft/client/DeltaTracker.java",
        "package net.minecraft.client;\npublic interface DeltaTracker {}",
    ),
    (
        "net/minecraft/client/player/LocalPlayer.java",
        "package net.minecraft.client.player;\n\
         public class LocalPlayer { public net.minecraft.world.item.ItemStack getMainHandItem() { return null; } }",
    ),
    (
        "net/minecraft/client/Minecraft.java",
        "package net.minecraft.client;\n\
         public class Minecraft { public net.minecraft.client.player.LocalPlayer player;\n\
           public net.minecraft.client.gui.Font font;\n\
           public void execute(Runnable r) {}\n\
           public static Minecraft getInstance() { return null; } }",
    ),
    (
        "me/shedaniel/autoconfig/ConfigData.java",
        "package me.shedaniel.autoconfig;\npublic interface ConfigData {}",
    ),
    (
        "me/shedaniel/autoconfig/ConfigHolder.java",
        "package me.shedaniel.autoconfig;\npublic interface ConfigHolder<T extends ConfigData> { T getConfig(); }",
    ),
    (
        "me/shedaniel/autoconfig/AutoConfig.java",
        "package me.shedaniel.autoconfig;\n\
         public final class AutoConfig {\n\
           public static <T extends ConfigData> ConfigHolder<T> register(Class<T> c, Object factory) { return null; }\n\
           public static <T extends ConfigData> ConfigHolder<T> getConfigHolder(Class<T> c) { return null; } }",
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

const CONFIG: &str = r#"
package demo;
public class Config implements me.shedaniel.autoconfig.ConfigData {
  public static Config INSTANCE;
  public boolean enabled = true;
  public int x = 4;
}
"#;

const MOD: &str = r#"
package demo;
import me.shedaniel.autoconfig.AutoConfig;
import net.fabricmc.fabric.api.client.rendering.v1.hud.HudElementRegistry;
import net.minecraft.ChatFormatting;
import net.minecraft.client.Minecraft;
import net.minecraft.network.chat.Component;
import net.minecraft.resources.Identifier;
import net.minecraft.world.item.ItemStack;
public final class HudMod {
  public static int tasks;
  public void onInitializeClient() {
    AutoConfig.register(Config.class, null);
    Config.INSTANCE = AutoConfig.getConfigHolder(Config.class).getConfig();
    HudElementRegistry.addLast(Identifier.fromNamespaceAndPath("demo", "durability"), (graphics, delta) -> {
      if (!Config.INSTANCE.enabled) return;
      Minecraft mc = Minecraft.getInstance();
      ItemStack held = mc.player.getMainHandItem();
      if (held.isEmpty()) return;
      int left = held.getMaxDamage() - held.getDamageValue();
      graphics.item(held, Config.INSTANCE.x, 20);
      graphics.itemDecorations(mc.font, held, Config.INSTANCE.x, 20);
      graphics.text(mc.font, Component.literal(left + " left").withStyle(ChatFormatting.GOLD), 24, 24, 0xFFFFFFFF, true);
      mc.execute(() -> tasks++);
    });
  }
}
"#;

#[test]
fn a_config_gated_hud_element_draws_the_held_stack_and_its_durability() {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-client-hud-items-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let mut sources = STUBS.to_vec();
    sources.push(("demo/Config.java", CONFIG));
    sources.push(("demo/HudMod.java", MOD));
    let Some(jar_path) = build_jar(
        &root,
        &sources,
        r#"{"id":"demo","version":"1","environment":"client","entrypoints":{"client":["demo.HudMod"]}}"#,
    ) else {
        eprintln!("skipped: no javac");
        return;
    };
    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("client").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "client", &mut host)
        .unwrap();
    let (layers, refused) = host.client_hud_layers(&jar);
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(layers.len(), 1);

    let mut inventory = vec![ClientStack::empty(); 36];
    inventory[0] = ClientStack {
        item: "minecraft:iron_pickaxe".into(),
        count: 1,
        damage: 50,
        max_damage: 250,
        max_stack: 1,
        name: "Iron Pickaxe".into(),
        ..ClientStack::empty()
    };
    let frame = Arc::new(ClientSnapshot {
        player: Some(ClientPlayer {
            name: "Alex".into(),
            inventory,
            ..ClientPlayer::default()
        }),
        gui_size: [320, 240],
        metrics: Arc::new(TextMetrics {
            advances: vec![6.0; 128],
            fallback: 6.0,
            translations: BTreeMap::new(),
        }),
        ..ClientSnapshot::default()
    });
    host.begin_client_frame(Arc::clone(&frame), &[]);
    jar.run_hud_layer(&layers[0], &mut host, &ExecutionBudget::UNBOUNDED)
        .unwrap();
    let out = host.end_client_frame();
    // The config's defaults (`enabled`, `x = 4`) gate and place the draw;
    // 250 - 50 = 200 uses left, in gold.
    let [item, decorations, label] = &out.draws[..] else {
        panic!("three draws: {:?}", out.draws);
    };
    assert!(matches!(
        item,
        DrawOp::Item { slot: Some(StackSlot::MainHand), at, model: true, decorations: None, .. }
            if *at == [4.0, 20.0]
    ));
    assert!(matches!(
        decorations,
        DrawOp::Item {
            model: false,
            decorations: Some(None),
            ..
        }
    ));
    let DrawOp::Text {
        text: ClientText::Json(json),
        at,
        ..
    } = label
    else {
        panic!("a styled label: {label:?}");
    };
    assert_eq!(*at, [24.0, 24.0]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(json).unwrap(),
        serde_json::json!({"text": "200 left", "color": "gold"})
    );
    // The queued task runs when the game runs it, not inside the frame.
    assert_eq!(out.tasks.len(), 1);
    host.begin_client_frame(frame, &[]);
    jar.run_client_task(&out.tasks[0], &mut host, &ExecutionBudget::UNBOUNDED)
        .unwrap();
    host.end_client_frame();
    let tasks = horizon_native_java::Host::persistent_class_state(&host)
        .0
        .remove("demo/HudMod#tasksI");
    assert_eq!(tasks, Some(Value::Int(1)));
    let _ = fs::remove_dir_all(&root);
}
