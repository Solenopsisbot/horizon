//! The objects a jar's entrypoint builds outlive it: a later event listener
//! walks the listener objects the entrypoint stored in a static, reads the
//! entrypoint instance's own fields through its lambda's captured `this`,
//! mutates instance fields, and the next listener sees the mutation. What
//! nothing reaches any more is collected, so a listener that allocates every
//! tick does not grow the jar's heap for the life of the server.
//!
//! The Fabric and game classes are compiled as stubs so `javac` accepts the
//! mod; only the mod's own classes go into the jar, as for a real one.

use horizon_native_java::{
    ExecutionBudget, Host, NativeJar, StdHost, Value,
    events::{EventContext, EventFacade, EventFiring, GameEvent, GameListener},
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
        "horizon-native-java-resident-heap-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

const STUBS: &[(&str, &str)] = &[
    (
        "net/fabricmc/fabric/api/event/Event.java",
        "package net.fabricmc.fabric.api.event;\n\
         public abstract class Event<T> { public void register(T listener) {} }",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\n\
         public class MinecraftServer { public int getTickCount() { return 0; } }",
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
           public static final Event<ServerStopping> SERVER_STOPPING = null;\n\
           public interface ServerStopping { void onServerStopping(MinecraftServer server); } }",
    ),
];

/// The shape servux has: listener objects in a static list, a config object
/// in an entrypoint field, a tick listener that changes them and a stop
/// listener that reads them back. The tick listener also allocates garbage
/// every tick (a counter nobody keeps, an array, a list).
const MOD: &str = r#"
package demo;
import java.util.ArrayList;
import java.util.List;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerLifecycleEvents;
import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;
public final class HeapMod {
  static final List<Counter> LISTENERS = new ArrayList<>();
  public static int seen = -1;
  public static String names = "";
  private final Config config = new Config();
  static final class Counter {
    final String name;
    int count;
    Counter(String name) { this.name = name; }
  }
  static final class Config { int step = 2; }
  public void onInitialize() {
    LISTENERS.add(new Counter("a"));
    LISTENERS.add(new Counter("b"));
    ServerTickEvents.END_SERVER_TICK.register(server -> {
      for (Counter counter : LISTENERS) {
        counter.count += config.step;
      }
      Counter garbage = new Counter("tick");
      garbage.count = new int[] { 1, 2, 3 }.length;
      List<Counter> scratch = new ArrayList<>();
      scratch.add(garbage);
    });
    ServerLifecycleEvents.SERVER_STOPPING.register(server -> {
      int sum = 0;
      StringBuilder seenNames = new StringBuilder();
      for (Counter counter : LISTENERS) {
        sum += counter.count;
        seenNames.append(counter.name);
      }
      seen = sum + 1000 * LISTENERS.size();
      names = seenNames.toString();
    });
  }
}
"#;

fn build_jar(root: &Path) -> PathBuf {
    let src = root.join("src");
    let classes = root.join("classes");
    let mut paths = Vec::new();
    for (path, text) in STUBS.iter().chain(&[("demo/HeapMod.java", MOD)]) {
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
    zip.start_file("fabric.mod.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(br#"{"id":"heap_mod","version":"1.0","entrypoints":{"main":["demo.HeapMod"]}}"#)
        .unwrap();
    for entry in fs::read_dir(classes.join("demo")).unwrap() {
        let path = entry.unwrap().path();
        let name = format!("demo/{}", path.file_name().unwrap().to_string_lossy());
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(&fs::read(&path).unwrap()).unwrap();
    }
    zip.finish().unwrap();
    jar_path
}

fn fire(jar: &NativeJar, host: &mut StdHost, listeners: &[GameListener], event: GameEvent) {
    let mut firing = EventFiring::new(
        event,
        Arc::new(EventFacade::default()),
        EventContext::default(),
    );
    let mut ran = 0;
    for listener in listeners.iter().filter(|l| l.event == event) {
        let run = jar
            .run_game_listener(listener, &mut firing, host, &ExecutionBudget::UNBOUNDED)
            .unwrap();
        ran += usize::from(run.ran);
    }
    assert_eq!(ran, 1, "{event:?}");
}

fn static_value(host: &StdHost, key: &str) -> Option<Value> {
    host.persistent_class_state()
        .0
        .remove(&format!("demo/HeapMod#{key}"))
}

#[test]
fn a_later_listener_sees_the_objects_the_entrypoint_built_and_what_earlier_ones_changed() {
    let root = scratch("listeners");
    let jar = NativeJar::from_path(&build_jar(&root)).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    let listeners = host.game_listeners(&jar, true).listeners;

    for _ in 0..3 {
        fire(&jar, &mut host, &listeners, GameEvent::ServerTickEnd);
    }
    fire(&jar, &mut host, &listeners, GameEvent::ServerStopping);

    // Three ticks of `config.step` (2, read through the lambda's captured
    // entrypoint) on each of the two counters: 6 + 6, plus 1000 per counter.
    // Without the resident heap the counters and the config lost their
    // fields after the entrypoint returned and this read 2000.
    assert_eq!(static_value(&host, "seenI"), Some(Value::Int(2012)));
    assert_eq!(
        static_value(&host, "namesLjava/lang/String;"),
        Some(Value::Ref(Some("ab".into())))
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn objects_a_listener_drops_are_collected_and_the_kept_ones_survive_it() {
    let root = scratch("collect");
    let jar = NativeJar::from_path(&build_jar(&root)).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    let listeners = host.game_listeners(&jar, true).listeners;
    let after_bootstrap = host.resident_heap().instance_fields();

    // Each tick leaves a counter (two fields), an array and a list nobody
    // reaches: 3,000 ticks would keep 6,000 dead fields without collection.
    for _ in 0..3000 {
        fire(&jar, &mut host, &listeners, GameEvent::ServerTickEnd);
    }
    let heap = host.resident_heap();
    assert!(
        heap.instance_fields() < after_bootstrap + 2 * 1024 + 64,
        "{} fields kept after 3,000 ticks ({after_bootstrap} after bootstrap)",
        heap.instance_fields()
    );
    assert!(heap.arrays() < 1024 + 64, "{} arrays kept", heap.arrays());

    // The collection never touched the reachable counters.
    fire(&jar, &mut host, &listeners, GameEvent::ServerStopping);
    assert_eq!(
        static_value(&host, "seenI"),
        Some(Value::Int(2000 + 2 * 6000))
    );
    let _ = fs::remove_dir_all(root);
}
