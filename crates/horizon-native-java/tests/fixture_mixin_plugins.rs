//! Runtime-built fixture for Mixin config plugins (`mixin_plugins.rs`).
//!
//! The jar is compiled with `javac` against compile-only stubs of Mixin's
//! plugin interface, ASM's `ClassNode`, Fabric's loader API and the game
//! classes the Mixins target, then planned the way the game plans it
//! (`plan_load_time_weave_with_game_hooks`) and run the way it runs an
//! entrypoint (`execute`). What is checked:
//!
//! - one plugin instance lives through the whole session: a field the
//!   constructor sets and `onLoad` extends is what `shouldApplyMixin` reads;
//! - the session runs in Mixin's order: `acceptTargets` sees the targets the
//!   questions kept, and `getMixins` (after it) adds a companion Mixin that
//!   binds without being listed;
//! - the loader-platform probes answer as on a Fabric server: Fabric's classes
//!   load, NeoForge's throw `ClassNotFoundException`, `isModLoaded` knows the
//!   jar itself only, the environment and Mixin's side are `SERVER`;
//! - a `postApply` that touches the target's `ClassNode` refuses its Mixin,
//!   naming the member;
//! - on the entrypoint path a plugin's `onLoad` has run before the entrypoint
//!   (whiteout's shape), and a plugin that fails has its writes rolled back
//!   and never refuses the jar.

use horizon_native_java::{MixinSide, MixinSkipReason, NativeJar, StdHost};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
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

/// Compile-only classes: none of them is packed into the jar.
const STUBS: &[(&str, &str)] = &[
    (
        "org/spongepowered/asm/mixin/Mixin.java",
        "package org.spongepowered.asm.mixin;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.TYPE)\npublic @interface Mixin { Class<?>[] value() default {}; String[] targets() default {}; }\n",
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
        "org/spongepowered/asm/mixin/extensibility/IMixinInfo.java",
        "package org.spongepowered.asm.mixin.extensibility;\npublic interface IMixinInfo {}\n",
    ),
    (
        "org/spongepowered/asm/mixin/extensibility/IMixinConfigPlugin.java",
        "package org.spongepowered.asm.mixin.extensibility;\nimport java.util.*;\nimport org.objectweb.asm.tree.ClassNode;\npublic interface IMixinConfigPlugin {\n void onLoad(String mixinPackage);\n String getRefMapperConfig();\n boolean shouldApplyMixin(String targetClassName, String mixinClassName);\n void acceptTargets(Set<String> myTargets, Set<String> otherTargets);\n List<String> getMixins();\n void preApply(String targetClassName, ClassNode targetClass, String mixinClassName, IMixinInfo mixinInfo);\n void postApply(String targetClassName, ClassNode targetClass, String mixinClassName, IMixinInfo mixinInfo);\n}\n",
    ),
    (
        "org/spongepowered/asm/mixin/MixinEnvironment.java",
        "package org.spongepowered.asm.mixin;\npublic final class MixinEnvironment {\n public static MixinEnvironment getCurrentEnvironment() { return null; }\n public Side getSide() { return null; }\n public enum Side { UNKNOWN, CLIENT, SERVER }\n}\n",
    ),
    (
        "org/objectweb/asm/tree/ClassNode.java",
        "package org.objectweb.asm.tree;\npublic class ClassNode { public void visitEnd() {} }\n",
    ),
    (
        "net/fabricmc/api/EnvType.java",
        "package net.fabricmc.api;\npublic enum EnvType { CLIENT, SERVER }\n",
    ),
    (
        "net/fabricmc/loader/api/FabricLoader.java",
        "package net.fabricmc.loader.api;\npublic interface FabricLoader {\n static FabricLoader getInstance() { return null; }\n boolean isModLoaded(String id);\n net.fabricmc.api.EnvType getEnvironmentType();\n}\n",
    ),
    (
        "net/neoforged/fml/ModList.java",
        "package net.neoforged.fml;\npublic class ModList {}\n",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\npublic class MinecraftServer {}\n",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\npublic class ServerPlayer {}\n",
    ),
    (
        "net/minecraft/server/players/PlayerList.java",
        "package net.minecraft.server.players;\npublic class PlayerList {}\n",
    ),
    (
        "demo/Unmodelled.java",
        "package demo;\npublic final class Unmodelled { public static void call() {} }\n",
    ),
];

const MIXIN_HEADER: &str = "package demo.mixin;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.players.PlayerList;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
";

/// The plugin under test. `asked` starts at 100 in the constructor, so a
/// `true` from the generic branch proves the constructor's object is the one
/// the questions see; `pkg` proves `onLoad` ran on it first.
const PLUGIN: &str = "package demo;
import java.util.*;
import net.fabricmc.api.EnvType;
import net.fabricmc.loader.api.FabricLoader;
import org.objectweb.asm.tree.ClassNode;
import org.spongepowered.asm.mixin.MixinEnvironment;
import org.spongepowered.asm.mixin.extensibility.*;
public class Plugin implements IMixinConfigPlugin {
    private String pkg;
    private int asked;
    public static String probe = \"\";
    public static boolean keptPlayerList;
    public Plugin() { asked = 100; }
    public void onLoad(String mixinPackage) {
        pkg = mixinPackage;
        try { Class.forName(\"net.fabricmc.api.EnvType\"); probe += \"fabric;\"; }
        catch (ClassNotFoundException e) { probe += \"no-fabric;\"; }
        try { Class.forName(\"net.neoforged.fml.ModList\"); probe += \"neoforge;\"; }
        catch (ClassNotFoundException e) { probe += \"no-neoforge;\"; }
        if (FabricLoader.getInstance().isModLoaded(\"plugin-demo\")) probe += \"self;\";
        if (FabricLoader.getInstance().isModLoaded(\"other-mod\")) probe += \"other;\";
        if (FabricLoader.getInstance().getEnvironmentType() == EnvType.SERVER) probe += \"server;\";
        if (MixinEnvironment.getCurrentEnvironment().getSide() == MixinEnvironment.Side.SERVER) probe += \"mixin-server;\";
    }
    public String getRefMapperConfig() { return null; }
    public boolean shouldApplyMixin(String targetClassName, String mixinClassName) {
        asked++;
        if (!mixinClassName.startsWith(pkg + \".\")) return false;
        if (mixinClassName.endsWith(\"Declined\")) return false;
        if (mixinClassName.endsWith(\"Probed\")) return probe.equals(\"fabric;no-neoforge;self;server;mixin-server;\");
        return asked > 100;
    }
    public void acceptTargets(Set<String> myTargets, Set<String> otherTargets) {
        keptPlayerList = myTargets.contains(\"net.minecraft.server.players.PlayerList\");
    }
    public List<String> getMixins() { return keptPlayerList ? List.of(\"CompanionMixin\") : null; }
    public void preApply(String target, ClassNode node, String mixin, IMixinInfo info) {}
    public void postApply(String target, ClassNode node, String mixin, IMixinInfo info) {
        if (mixin.endsWith(\"Edits\")) node.visitEnd();
    }
}
";

/// The Mixins of `plugin.mixins.json` (package `demo.mixin`), and the
/// companion its plugin adds.
const MIXINS: &[(&str, &str)] = &[
    (
        "LeaveMixin",
        "@Mixin(PlayerList.class) public class LeaveMixin {
    @Inject(method = \"remove\", at = @At(\"HEAD\"))
    private static void leaving(ServerPlayer player, CallbackInfo ci) {}
}",
    ),
    (
        "StoppedDeclined",
        "@Mixin(MinecraftServer.class) public class StoppedDeclined {
    @Inject(method = \"stopServer\", at = @At(\"TAIL\"))
    private static void declined(CallbackInfo ci) {}
}",
    ),
    (
        "StoppingProbed",
        "@Mixin(MinecraftServer.class) public class StoppingProbed {
    @Inject(method = \"stopServer\", at = @At(\"HEAD\"))
    private static void probed(CallbackInfo ci) {}
}",
    ),
    (
        "StoppedEdits",
        "@Mixin(MinecraftServer.class) public class StoppedEdits {
    @Inject(method = \"stopServer\", at = @At(\"TAIL\"))
    private static void edits(CallbackInfo ci) {}
}",
    ),
    (
        "CompanionMixin",
        "@Mixin(MinecraftServer.class) public class CompanionMixin {
    @Inject(method = \"stopServer\", at = @At(\"TAIL\"))
    private static void companion(CallbackInfo ci) {}
}",
    ),
];

struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    /// Compile `classes` (`demo/...` paths) and the Mixins, and pack them
    /// with one config per `(file, plugin, mixins)` row.
    fn build(
        name: &str,
        classes: &[(&str, &str)],
        configs: &[(&str, &str, &[&str])],
        main: &str,
    ) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/native-java-mixin-plugins")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let out = root.join("classes");
        fs::create_dir_all(&out).unwrap();
        let mut sources: BTreeMap<String, String> = STUBS
            .iter()
            .map(|(path, body)| ((*path).to_owned(), (*body).to_owned()))
            .collect();
        for (class, body) in MIXINS {
            sources.insert(
                format!("demo/mixin/{class}.java"),
                format!("{MIXIN_HEADER}{body}\n"),
            );
        }
        sources.insert("demo/Main.java".into(), main.to_owned());
        for (path, body) in classes {
            sources.insert((*path).to_owned(), (*body).to_owned());
        }
        let mut paths = Vec::new();
        for (relative, body) in &sources {
            let path = src.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            paths.push(path);
        }
        let status = Command::new(javac())
            .args(["--release", "17", "-d"])
            .arg(&out)
            .args(paths)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed for {name}");
        // A stub's nested classes (`MixinEnvironment$Side`) stay out too.
        let stems: Vec<String> = STUBS
            .iter()
            .map(|(path, _)| path.trim_end_matches(".java").to_owned())
            .collect();
        let compile_only = |entry: &str| {
            stems.iter().any(|stem| {
                entry
                    .strip_prefix(stem.as_str())
                    .is_some_and(|rest| rest == ".class" || rest.starts_with('$'))
            })
        };
        let files: Vec<&str> = configs.iter().map(|(file, _, _)| *file).collect();
        let metadata = serde_json::json!({
            "id": "plugin-demo",
            "version": "1.0",
            "entrypoints": { "main": ["demo.Main"] },
            "mixins": files,
        })
        .to_string();
        let jar = root.join(format!("{name}.jar"));
        let options = SimpleFileOptions::default();
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar).unwrap());
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(metadata.as_bytes()).unwrap();
        for (file, plugin, names) in configs {
            let config = serde_json::json!({
                "package": "demo.mixin",
                "plugin": plugin,
                "mixins": names,
            });
            zip.start_file(*file, options).unwrap();
            zip.write_all(config.to_string().as_bytes()).unwrap();
        }
        for class in files_under(&out) {
            let entry = class
                .strip_prefix(&out)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if compile_only(&entry) {
                continue;
            }
            zip.start_file(entry, options).unwrap();
            zip.write_all(&fs::read(class).unwrap()).unwrap();
        }
        zip.finish().unwrap();
        Self { root, jar }
    }

    fn load(&self) -> NativeJar {
        NativeJar::from_path(&self.jar).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const PLAIN_MAIN: &str =
    "package demo;\npublic final class Main { public void onInitialize() {} }\n";

#[test]
fn a_plugin_session_keeps_its_instance_and_decides_like_mixin() {
    let fixture = Fixture::build(
        "session",
        &[("demo/Plugin.java", PLUGIN)],
        &[(
            "plugin.mixins.json",
            "demo.Plugin",
            &[
                "LeaveMixin",
                "StoppedDeclined",
                "StoppingProbed",
                "StoppedEdits",
            ],
        )],
        PLAIN_MAIN,
    );
    let jar = fixture.load();
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    let bound: BTreeMap<&str, &str> = weave
        .game_hooks
        .iter()
        .map(|plan| (plan.plan.callback.owner.as_str(), plan.hook))
        .collect();
    assert_eq!(
        bound,
        BTreeMap::from([
            // Listed and answered `true` by the constructor's own object.
            ("demo/mixin/LeaveMixin", "java_player_list_remove"),
            // Answered `true` only when every platform probe answered as on
            // a Fabric server.
            ("demo/mixin/StoppingProbed", "server_stopping"),
            // Listed by no config: `getMixins` added it after
            // `acceptTargets` saw `PlayerList` among the kept targets.
            ("demo/mixin/CompanionMixin", "java_server_stopped"),
        ]),
        "plugins: {:#?}\nskipped: {:#?}",
        weave.config_plugins,
        weave.skipped
    );
    for owner in ["demo/mixin/StoppedDeclined", "demo/mixin/StoppedEdits"] {
        let reason = &weave
            .skipped
            .iter()
            .find(|plan| plan.callback.owner == owner)
            .unwrap_or_else(|| panic!("{owner} should be skipped"))
            .reason;
        assert_eq!(*reason, MixinSkipReason::ConfigPlugin, "{owner}");
    }
    let [report] = weave.config_plugins.as_slice() else {
        panic!("one session: {:#?}", weave.config_plugins);
    };
    assert_eq!(report.refused, None);
    assert_eq!(report.companions, ["demo/mixin/CompanionMixin"]);
    // Three `true`s, one of them withdrawn by its `postApply`; one `false`.
    assert_eq!((report.applied, report.declined), (2, 1));
    let [(mixin, reason)] = report.failed.as_slice() else {
        panic!("one failed Mixin: {:#?}", report.failed);
    };
    assert_eq!(mixin, "demo/mixin/StoppedEdits");
    assert!(
        reason.starts_with("postApply") && reason.contains("ClassNode#visitEnd"),
        "{reason}"
    );
    // The default weave never runs a plugin, so it binds nothing.
    let plain = jar.plan_load_time_weave(MixinSide::Server);
    assert!(plain.game_hooks.is_empty() && plain.config_plugins.is_empty());
}

/// whiteout's shape: the plugin's `onLoad` fills the static the entrypoint
/// checks. A second config's plugin writes a static and then stops at an
/// unmodelled call; its write is rolled back, and the jar still runs.
#[test]
fn plugins_run_before_the_entrypoint_and_a_failing_one_is_rolled_back() {
    let init = "package demo;
public class InitPlugin extends Base {
    public void onLoad(String mixinPackage) { Main.CONFIG = \"loaded \" + mixinPackage; }
}
";
    let broken = "package demo;
public class BrokenPlugin extends Base {
    public void onLoad(String mixinPackage) { Main.LEAKED = \"half\"; Unmodelled.call(); }
}
";
    let base = "package demo;
import java.util.*;
import org.objectweb.asm.tree.ClassNode;
import org.spongepowered.asm.mixin.extensibility.*;
public abstract class Base implements IMixinConfigPlugin {
    public String getRefMapperConfig() { return null; }
    public boolean shouldApplyMixin(String target, String mixin) { return true; }
    public void acceptTargets(Set<String> mine, Set<String> others) {}
    public List<String> getMixins() { return null; }
    public void preApply(String t, ClassNode n, String m, IMixinInfo i) {}
    public void postApply(String t, ClassNode n, String m, IMixinInfo i) {}
}
";
    let main = "package demo;
public final class Main {
    public static String CONFIG;
    public static String LEAKED;
    public void onInitialize() {
        if (CONFIG == null) throw new IllegalStateException(\"The mixin plugin did not initialize the config!\");
        if (LEAKED != null) throw new IllegalStateException(\"a failed plugin leaked \" + LEAKED);
        if (!CONFIG.equals(\"loaded demo.mixin\")) throw new IllegalStateException(CONFIG);
    }
}
";
    let fixture = Fixture::build(
        "entrypoint",
        &[
            ("demo/Base.java", base),
            ("demo/InitPlugin.java", init),
            ("demo/BrokenPlugin.java", broken),
        ],
        &[
            ("init.mixins.json", "demo.InitPlugin", &["LeaveMixin"]),
            (
                "broken.mixins.json",
                "demo.BrokenPlugin",
                &["StoppedDeclined"],
            ),
        ],
        main,
    );
    let jar = fixture.load();
    let mut host = jar.seed_host(
        StdHost::new()
            .with_environment_type("SERVER")
            .with_strict_unknown_host_calls(),
    );
    jar.execute("demo.Main", None, "main", &mut host)
        .expect("the entrypoint sees the config its plugin loaded");
    // A second entrypoint on the same host does not run the session again
    // (it would fail the same way, and `InitPlugin` already ran).
    jar.execute("demo.Main", None, "main", &mut host)
        .expect("the second entrypoint sees the same state");

    // The gate's own session agrees: `InitPlugin` answers, `BrokenPlugin`
    // is refused with the member it stopped at.
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    let refused: Vec<_> = weave
        .config_plugins
        .iter()
        .map(|report| (report.plugin.as_str(), report.refused.as_deref()))
        .collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(refused[1].0, "demo.InitPlugin");
    assert_eq!(refused[1].1, None);
    assert_eq!(refused[0].0, "demo.BrokenPlugin");
    assert!(
        refused[0].1.is_some_and(
            |reason| reason.starts_with("onLoad") && reason.contains("Unmodelled#call")
        ),
        "{refused:?}"
    );
}
