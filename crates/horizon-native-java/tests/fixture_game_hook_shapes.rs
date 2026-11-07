//! Runtime-built fixture for the game-hook tier's value-returning shapes and
//! its internal Java-tier sites (`hook_shapes.rs`).
//!
//! The jar is compiled with `javac` against compile-only stubs of the game
//! classes and of Mixin's `CallbackInfoReturnable`, then driven the way the
//! game drives it: plan with `plan_load_time_weave_with_game_hooks`, bind with
//! `register_game_hooks`, fire with `run_game_hook_returning`. What a callback
//! saw crosses the host boundary through the compile-only `demo/HookSink`, so
//! the test reads it from `StdHost`'s invocation log, not from a Rust stub.
//!
//! The shapes covered:
//!
//! - `RETURN`, cancellable: a `getDestroySpeed` callback doubles the result
//!   with `setReturnValue`, and the host gets the override back.
//! - `TAIL`, listen-only: an earlier callback reads the method's own result
//!   with `getReturnValue` before the override.
//! - `HEAD`, cancellable, on a `boolean` method: `setReturnValue` short-circuits
//!   with that value, a bare `cancel()` with `false`, and no call leaves the
//!   method alone.
//! - Mixin's `CancellationException` for a callback that is not cancellable.
//! - The shapes a site refuses: a position it does not fire, a cancellable
//!   callback at a listen-only point, and a `CallbackInfo` where the target
//!   returns a value.

use horizon_native_java::{
    ExecutionBudget, GameHookContext, GameHookState, MixinSide, MixinSkipReason, NativeJar,
    StdHost, Value,
};
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

/// Mixin's annotations and callback-info classes, compile-only. The runtime
/// owns the callback-info object, so the stubs' bodies never run.
const MIXIN_STUBS: &[(&str, &str)] = &[
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
        "package org.spongepowered.asm.mixin.injection.callback;\npublic class CallbackInfo { public void cancel() {} public boolean isCancelled() { return false; } }\n",
    ),
    (
        "org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable.java",
        "package org.spongepowered.asm.mixin.injection.callback;\npublic class CallbackInfoReturnable<R> extends CallbackInfo { public R getReturnValue() { return null; } public void setReturnValue(R value) {} }\n",
    ),
];

/// Compile-only game classes the Mixins name, and the sink the callbacks
/// report through.
const GAME_STUBS: &[(&str, &str)] = &[
    (
        "net/minecraft/world/entity/player/Player.java",
        "package net.minecraft.world.entity.player;\npublic class Player {}\n",
    ),
    (
        "net/minecraft/world/level/block/state/BlockState.java",
        "package net.minecraft.world.level.block.state;\npublic class BlockState {}\n",
    ),
    (
        "net/minecraft/world/item/BoneMealItem.java",
        "package net.minecraft.world.item;\npublic class BoneMealItem {}\n",
    ),
    (
        "net/minecraft/world/item/ItemStack.java",
        "package net.minecraft.world.item;\npublic class ItemStack {}\n",
    ),
    (
        "net/minecraft/world/level/Level.java",
        "package net.minecraft.world.level;\npublic class Level {}\n",
    ),
    (
        "net/minecraft/core/BlockPos.java",
        "package net.minecraft.core;\npublic class BlockPos {}\n",
    ),
    (
        "net/minecraft/server/MinecraftServer.java",
        "package net.minecraft.server;\npublic class MinecraftServer {}\n",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\npublic class ServerPlayer extends net.minecraft.world.entity.player.Player {}\n",
    ),
    (
        "net/minecraft/server/players/PlayerList.java",
        "package net.minecraft.server.players;\npublic class PlayerList {}\n",
    ),
    (
        "demo/HookSink.java",
        "package demo;\npublic final class HookSink { public static void mark(String point) {} public static void speed(String point, float value) {} }\n",
    ),
];

const HEADER: &str = "package demo;
import net.minecraft.core.BlockPos;
import net.minecraft.server.MinecraftServer;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.server.players.PlayerList;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.item.BoneMealItem;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.state.BlockState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;
";

/// The Mixin classes of the jar that binds, in config (apply) order.
const BOUND: &[(&str, &str)] = &[
    (
        "ObserveSpeedMixin",
        "@Mixin(Player.class) public class ObserveSpeedMixin {
    @Inject(method = \"getDestroySpeed\", at = @At(\"TAIL\"))
    private static void observe(BlockState state, CallbackInfoReturnable<Float> cir) {
        HookSink.speed(\"observed\", cir.getReturnValue());
    }
}",
    ),
    (
        "DoubleSpeedMixin",
        "@Mixin(Player.class) public class DoubleSpeedMixin {
    @Inject(method = \"getDestroySpeed\", at = @At(\"RETURN\"), cancellable = true)
    private static void twice(BlockState state, CallbackInfoReturnable<Float> cir) {
        cir.setReturnValue(cir.getReturnValue() * 2.0f);
    }
}",
    ),
    (
        "GrowMixin",
        "@Mixin(BoneMealItem.class) public class GrowMixin {
    @Inject(method = \"growCrop\", at = @At(\"HEAD\"), cancellable = true)
    private static void decide(ItemStack stack, Level level, BlockPos pos, CallbackInfoReturnable<Boolean> cir) {
        Counter.grows++;
        if (Counter.grows == 1) {
            cir.setReturnValue(true);
        } else if (Counter.grows == 2) {
            cir.cancel();
        } else {
            HookSink.mark(\"grow-untouched\");
        }
    }
}",
    ),
    (
        "LeaveMixin",
        "@Mixin(PlayerList.class) public class LeaveMixin {
    @Inject(method = \"remove\", at = @At(\"HEAD\"))
    private static void leaving(ServerPlayer player, CallbackInfo ci) { HookSink.mark(\"leaving\"); }
}",
    ),
    (
        "StoppedMixin",
        "@Mixin(MinecraftServer.class) public class StoppedMixin {
    @Inject(method = \"stopServer\", at = @At(\"TAIL\"))
    private static void stopped(CallbackInfo ci) { HookSink.mark(\"stopped\"); }
}",
    ),
    (
        "StoppingMixin",
        "@Mixin(MinecraftServer.class) public class StoppingMixin {
    @Inject(method = \"stopServer\", at = @At(\"HEAD\"))
    private static void stopping(CallbackInfo ci) { HookSink.mark(\"stopping\"); }
}",
    ),
];

/// Shapes the sites refuse, and one that binds but breaks Mixin's rule when
/// it runs.
const REFUSED: &[(&str, &str)] = &[
    (
        "HeadSpeedMixin",
        "@Mixin(Player.class) public class HeadSpeedMixin {
    @Inject(method = \"getDestroySpeed\", at = @At(\"HEAD\"), cancellable = true)
    private static void early(BlockState state, CallbackInfoReturnable<Float> cir) {}
}",
    ),
    (
        "PlainInfoSpeedMixin",
        "@Mixin(Player.class) public class PlainInfoSpeedMixin {
    @Inject(method = \"getDestroySpeed\", at = @At(\"RETURN\"))
    private static void plain(BlockState state, CallbackInfo ci) {}
}",
    ),
    (
        "CancelStoppedMixin",
        "@Mixin(MinecraftServer.class) public class CancelStoppedMixin {
    @Inject(method = \"stopServer\", at = @At(\"TAIL\"), cancellable = true)
    private static void cancelStopped(CallbackInfo ci) {}
}",
    ),
    (
        "IllegalGrowMixin",
        "@Mixin(BoneMealItem.class) public class IllegalGrowMixin {
    @Inject(method = \"growCrop\", at = @At(\"HEAD\"))
    private static void sneak(ItemStack stack, Level level, BlockPos pos, CallbackInfoReturnable<Boolean> cir) {
        cir.setReturnValue(false);
    }
}",
    ),
];

struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    /// One config listing every Mixin class, with no plugin.
    fn build(name: &str, mixins: &[(&str, &str)]) -> Self {
        let names: Vec<&str> = mixins.iter().map(|(class, _)| *class).collect();
        Self::build_with(name, mixins, &[], &[("demo.mixins.json", None, &names)])
    }

    /// Compile `mixins` and the extra `classes` (shipped in the jar), and
    /// pack them with one Mixin config per `(file, plugin, mixins)` row.
    fn build_with(
        name: &str,
        mixins: &[(&str, &str)],
        classes_in_jar: &[(&str, &str)],
        configs: &[(&str, Option<&str>, &[&str])],
    ) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/native-java-game-hook")
            .join(format!("shapes-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let classes = root.join("classes");
        fs::create_dir_all(&classes).unwrap();
        let mut sources: BTreeMap<String, String> = MIXIN_STUBS
            .iter()
            .chain(GAME_STUBS)
            .map(|(path, body)| ((*path).to_owned(), (*body).to_owned()))
            .collect();
        sources.insert(
            "demo/Counter.java".into(),
            "package demo;\npublic final class Counter { public static int grows; }\n".into(),
        );
        sources.insert(
            "demo/Main.java".into(),
            "package demo;\npublic final class Main { public static void onInitialize() {} }\n"
                .into(),
        );
        for (class, body) in mixins {
            sources.insert(format!("demo/{class}.java"), format!("{HEADER}{body}\n"));
        }
        for (class, body) in classes_in_jar {
            sources.insert(format!("demo/{class}.java"), (*body).to_owned());
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
            .arg(&classes)
            .args(paths)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed for {name}");

        let compile_only: Vec<String> = MIXIN_STUBS
            .iter()
            .chain(GAME_STUBS)
            .map(|(path, _)| path.replace(".java", ".class"))
            .collect();
        let files: Vec<&str> = configs.iter().map(|(file, _, _)| *file).collect();
        let metadata = serde_json::json!({
            "id": "game-hook-shapes",
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
            let mut config = serde_json::json!({ "package": "demo", "mixins": names });
            if let Some(plugin) = plugin {
                config["plugin"] = serde_json::json!(plugin);
            }
            zip.start_file(*file, options).unwrap();
            zip.write_all(config.to_string().as_bytes()).unwrap();
        }
        for class in files_under(&classes) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if compile_only.contains(&entry) {
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

/// What the callbacks told `demo/HookSink`, in call order.
fn sink(host: &StdHost) -> Vec<(String, Vec<Value>)> {
    host.invocations()
        .iter()
        .filter(|call| call.method.owner == "demo/HookSink")
        .map(|call| (call.method.name.clone(), call.args.clone()))
        .collect()
}

/// Fire one point the way the game does, with a fresh state and context.
fn fire(
    jar: &NativeJar,
    registrations: &horizon_native_java::GameHookRegistrations,
    host: &mut StdHost,
    hook: &str,
    args: &[Value],
    actual: Option<Value>,
) -> Result<horizon_native_java::GameHookRun, horizon_native_java::NativeError> {
    jar.run_game_hook_returning(
        registrations,
        &mut GameHookState::default(),
        &mut GameHookContext::new(),
        hook,
        None,
        args,
        actual,
        host,
        &ExecutionBudget::UNBOUNDED,
    )
}

#[test]
fn value_shapes_bind_and_return_their_override_to_the_host() {
    let fixture = Fixture::build("bound", BOUND);
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
            ("demo/DoubleSpeedMixin", "java_destroy_speed"),
            ("demo/GrowMixin", "java_grow_crop"),
            ("demo/LeaveMixin", "java_player_list_remove"),
            ("demo/ObserveSpeedMixin", "java_destroy_speed"),
            // One method, two rows: the anchor picks the public HEAD hook or
            // the internal TAIL one.
            ("demo/StoppedMixin", "java_server_stopped"),
            ("demo/StoppingMixin", "server_stopping"),
        ]),
        "skipped: {:#?}",
        weave.skipped
    );
    let registrations = jar.register_game_hooks(&weave).unwrap();
    let mut host = StdHost::new();

    // RETURN: the listen-only callback reads the method's own result, the
    // cancellable one replaces it, and the host gets the replacement in the
    // method's float shape.
    let run = fire(
        &jar,
        &registrations,
        &mut host,
        "java_destroy_speed",
        &[Value::Ref(Some("game/opaque/block_state".into()))],
        Some(Value::Float(1.5)),
    )
    .unwrap();
    assert_eq!(run.ran, 2);
    assert_eq!(run.returned, Some(Value::Float(3.0)));
    assert_eq!(
        sink(&host),
        [(
            "speed".to_owned(),
            vec![Value::Ref(Some("observed".into())), Value::Float(1.5)]
        )]
    );

    // A RETURN point needs the result; a HEAD one must not get one.
    assert!(
        fire(
            &jar,
            &registrations,
            &mut host,
            "java_destroy_speed",
            &[Value::Ref(None)],
            None
        )
        .is_err()
    );
    let stack = Value::Ref(Some("game/item_stack/meal".into()));
    let grow_args = [
        stack.clone(),
        Value::Ref(Some("game/level/minecraft:overworld".into())),
        Value::Ref(Some("game/opaque/pos".into())),
    ];
    assert!(
        fire(
            &jar,
            &registrations,
            &mut host,
            "java_grow_crop",
            &grow_args,
            Some(Value::Int(1))
        )
        .is_err()
    );

    // HEAD on a boolean method: `setReturnValue(true)` short-circuits with
    // true, a bare `cancel()` with the type's default, and a callback that
    // does neither leaves the method to run.
    let first = fire(
        &jar,
        &registrations,
        &mut host,
        "java_grow_crop",
        &grow_args,
        None,
    )
    .unwrap();
    assert!(first.cancelled);
    assert_eq!(first.returned, Some(Value::Int(1)));
    let second = fire(
        &jar,
        &registrations,
        &mut host,
        "java_grow_crop",
        &grow_args,
        None,
    )
    .unwrap();
    assert!(second.cancelled);
    assert_eq!(second.returned, Some(Value::Int(0)));
    let third = fire(
        &jar,
        &registrations,
        &mut host,
        "java_grow_crop",
        &grow_args,
        None,
    )
    .unwrap();
    assert!(!third.cancelled);
    assert_eq!(third.returned, None);
    assert!(
        sink(&host)
            .iter()
            .any(|(_, args)| args == &[Value::Ref(Some("grow-untouched".into()))])
    );

    // A void Java-tier point returns nothing.
    let left = fire(
        &jar,
        &registrations,
        &mut host,
        "java_player_list_remove",
        &[Value::Ref(Some("game/player/1.0".into()))],
        None,
    )
    .unwrap();
    assert_eq!((left.ran, left.cancelled, left.returned), (1, false, None));
}

#[test]
fn value_shapes_a_site_cannot_serve_are_refused_with_a_reason() {
    let fixture = Fixture::build("refused", REFUSED);
    let jar = fixture.load();
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    for owner in [
        "demo/HeadSpeedMixin",
        "demo/PlainInfoSpeedMixin",
        "demo/CancelStoppedMixin",
    ] {
        let reason = weave
            .skipped
            .iter()
            .find(|plan| plan.callback.owner == owner)
            .unwrap_or_else(|| panic!("{owner} should be skipped: {:#?}", weave.game_hooks))
            .reason
            .clone();
        assert!(
            matches!(reason, MixinSkipReason::GameHookMismatch(_)),
            "{owner}: {reason:?}"
        );
    }

    // A listen-only callback that sets a result binds (its shape is fine),
    // but Mixin throws when it runs: the firing fails and returns nothing.
    assert_eq!(
        weave
            .game_hooks
            .iter()
            .map(|plan| plan.plan.callback.owner.as_str())
            .collect::<Vec<_>>(),
        ["demo/IllegalGrowMixin"]
    );
    let registrations = jar.register_game_hooks(&weave).unwrap();
    let error = fire(
        &jar,
        &registrations,
        &mut StdHost::new(),
        "java_grow_crop",
        &[Value::Ref(None), Value::Ref(None), Value::Ref(None)],
        None,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("CancellationException"),
        "{error}"
    );
}

// Config plugins (an answering plugin, a declined Mixin, a plugin whose
// apply hook does work) are covered by `fixture_mixin_plugins.rs`, against
// the real `IMixinConfigPlugin` descriptors.
