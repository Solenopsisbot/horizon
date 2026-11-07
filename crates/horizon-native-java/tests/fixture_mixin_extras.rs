//! Runtime-built fixture for MixinExtras injectors (`mixin_extras.rs`).
//!
//! The jar is compiled with `javac` against compile-only stubs of Mixin's
//! annotations, MixinExtras' injector annotations and the game classes, then
//! planned and fired the way the game does. Every injector kind is
//! discovered; a `@ModifyReturnValue` at `Player#getDestroySpeed`'s `RETURN`
//! binds to `java_destroy_speed` and its answer is the speed the host gets
//! back, chained through a second handler that also takes the target's
//! argument; the rest stay skipped with a reason naming the injector.

use horizon_native_java::{
    ExecutionBudget, GameHookContext, GameHookShape, GameHookState, MixinExtrasKind, MixinSide,
    MixinSkipReason, NativeJar, StdHost, Value,
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

const RUNTIME: &str = "import java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.METHOD)\n";

/// Compile-only classes: none of them is packed into the jar.
fn stubs() -> Vec<(String, String)> {
    let mut stubs = vec![
        (
            "org/spongepowered/asm/mixin/Mixin.java".to_owned(),
            "package org.spongepowered.asm.mixin;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.TYPE)\npublic @interface Mixin { Class<?>[] value() default {}; }\n".to_owned(),
        ),
        (
            "org/spongepowered/asm/mixin/injection/At.java".to_owned(),
            "package org.spongepowered.asm.mixin.injection;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target({})\npublic @interface At { String value(); String target() default \"\"; int ordinal() default -1; }\n".to_owned(),
        ),
        (
            "com/llamalad7/mixinextras/injector/wrapoperation/Operation.java".to_owned(),
            "package com.llamalad7.mixinextras.injector.wrapoperation;\npublic interface Operation<R> { R call(Object... args); }\n".to_owned(),
        ),
        (
            "net/minecraft/world/entity/player/Player.java".to_owned(),
            "package net.minecraft.world.entity.player;\npublic class Player {}\n".to_owned(),
        ),
        (
            "net/minecraft/world/level/block/state/BlockState.java".to_owned(),
            "package net.minecraft.world.level.block.state;\npublic class BlockState {}\n".to_owned(),
        ),
    ];
    for (path, body) in [
        (
            "ModifyReturnValue",
            "public @interface ModifyReturnValue { String[] method(); org.spongepowered.asm.mixin.injection.At at(); }",
        ),
        (
            "ModifyExpressionValue",
            "public @interface ModifyExpressionValue { String[] method(); org.spongepowered.asm.mixin.injection.At at(); }",
        ),
        (
            "ModifyReceiver",
            "public @interface ModifyReceiver { String[] method(); org.spongepowered.asm.mixin.injection.At at(); }",
        ),
        (
            "WrapWithCondition",
            "public @interface WrapWithCondition { String[] method(); org.spongepowered.asm.mixin.injection.At at(); }",
        ),
        (
            "wrapoperation/WrapOperation",
            "public @interface WrapOperation { String[] method(); org.spongepowered.asm.mixin.injection.At[] at(); }",
        ),
        (
            "wrapmethod/WrapMethod",
            "public @interface WrapMethod { String[] method(); }",
        ),
    ] {
        let package = format!(
            "com.llamalad7.mixinextras.injector{}",
            path.rsplit_once('/')
                .map(|(dir, _)| format!(".{dir}"))
                .unwrap_or_default()
        );
        stubs.push((
            format!("com/llamalad7/mixinextras/injector/{path}.java"),
            format!("package {package};\n{RUNTIME}{body}\n"),
        ));
    }
    stubs
}

const HEADER: &str = "package demo;
import com.llamalad7.mixinextras.injector.*;
import com.llamalad7.mixinextras.injector.wrapmethod.WrapMethod;
import com.llamalad7.mixinextras.injector.wrapoperation.*;
import net.minecraft.world.entity.player.Player;
import net.minecraft.world.level.block.state.BlockState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
";

/// The Mixin classes, in config (apply) order.
const MIXINS: &[(&str, &str)] = &[
    (
        "TripleMixin",
        "@Mixin(Player.class) public class TripleMixin {
    @ModifyReturnValue(method = \"getDestroySpeed\", at = @At(\"RETURN\"))
    private float triple(float original) { return original * 3.0f; }
}",
    ),
    (
        "PlusOneMixin",
        "@Mixin(Player.class) public class PlusOneMixin {
    @ModifyReturnValue(method = \"getDestroySpeed\", at = @At(\"RETURN\"))
    private static float plusOne(float original, BlockState state) { return original + 1.0f; }
}",
    ),
    (
        "WrongTypeMixin",
        "@Mixin(Player.class) public class WrongTypeMixin {
    @ModifyReturnValue(method = \"getDestroySpeed\", at = @At(\"RETURN\"))
    private int wrong(int original) { return original; }
}",
    ),
    (
        "WrapMixin",
        "@Mixin(Player.class) public class WrapMixin {
    @WrapMethod(method = \"getDestroySpeed\")
    private float wrap(BlockState state, Operation<Float> original) { return original.call(state); }
}",
    ),
    (
        "ExpressionMixin",
        "@Mixin(Player.class) public class ExpressionMixin {
    @ModifyExpressionValue(method = \"getDestroySpeed\", at = @At(value = \"INVOKE\", target = \"Lnet/minecraft/world/level/block/state/BlockState;getDestroySpeed()F\"))
    private float expression(float value) { return value; }
}",
    ),
    (
        "OperationMixin",
        "@Mixin(Player.class) public class OperationMixin {
    @WrapOperation(method = \"getDestroySpeed\", at = @At(value = \"INVOKE\", target = \"Lnet/minecraft/world/level/block/state/BlockState;getDestroySpeed()F\"))
    private float operation(BlockState state, Operation<Float> original) { return original.call(state); }
}",
    ),
    (
        "ConditionMixin",
        "@Mixin(Player.class) public class ConditionMixin {
    @WrapWithCondition(method = \"aiStep\", at = @At(value = \"INVOKE\", target = \"Lnet/minecraft/world/entity/player/Player;resetFallDistance()V\"))
    private boolean condition(Player player) { return true; }
}",
    ),
    (
        "ReceiverMixin",
        "@Mixin(Player.class) public class ReceiverMixin {
    @ModifyReceiver(method = \"aiStep\", at = @At(value = \"INVOKE\", target = \"Lnet/minecraft/world/entity/player/Player;resetFallDistance()V\"))
    private Player receiver(Player player) { return player; }
}",
    ),
];

struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    fn build() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/native-java-mixin-extras")
            .join(format!("extras-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let out = root.join("classes");
        fs::create_dir_all(&out).unwrap();
        let stubs = stubs();
        let mut sources: BTreeMap<String, String> = stubs.iter().cloned().collect();
        sources.insert(
            "demo/Main.java".into(),
            "package demo;\npublic final class Main { public void onInitialize() {} }\n".into(),
        );
        for (class, body) in MIXINS {
            sources.insert(format!("demo/{class}.java"), format!("{HEADER}{body}\n"));
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
        assert!(status.success(), "javac failed");
        let stems: Vec<String> = stubs
            .iter()
            .map(|(path, _)| path.trim_end_matches(".java").to_owned())
            .collect();
        let names: Vec<&str> = MIXINS.iter().map(|(class, _)| *class).collect();
        let jar = root.join("extras.jar");
        let options = SimpleFileOptions::default();
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar).unwrap());
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            serde_json::json!({
                "id": "extras-demo",
                "version": "1.0",
                "entrypoints": { "main": ["demo.Main"] },
                "mixins": ["extras.mixins.json"],
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        zip.start_file("extras.mixins.json", options).unwrap();
        zip.write_all(
            serde_json::json!({ "package": "demo", "mixins": names })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        for class in files_under(&out) {
            let entry = class
                .strip_prefix(&out)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if stems.iter().any(|stem| {
                entry
                    .strip_prefix(stem.as_str())
                    .is_some_and(|rest| rest == ".class" || rest.starts_with('$'))
            }) {
                continue;
            }
            zip.start_file(entry, options).unwrap();
            zip.write_all(&fs::read(class).unwrap()).unwrap();
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

#[test]
fn mixin_extras_injectors_are_discovered_and_modify_return_value_binds() {
    let fixture = Fixture::build();
    let jar = NativeJar::from_path(&fixture.jar).unwrap();

    let plans = jar.mixin_extras_plans();
    let discovered: BTreeMap<&str, MixinExtrasKind> = plans
        .iter()
        .map(|plan| (plan.callback.name.as_str(), plan.kind))
        .collect();
    assert_eq!(
        discovered,
        BTreeMap::from([
            ("condition", MixinExtrasKind::WrapWithCondition),
            ("expression", MixinExtrasKind::ModifyExpressionValue),
            ("operation", MixinExtrasKind::WrapOperation),
            ("plusOne", MixinExtrasKind::ModifyReturnValue),
            ("receiver", MixinExtrasKind::ModifyReceiver),
            ("triple", MixinExtrasKind::ModifyReturnValue),
            ("wrap", MixinExtrasKind::WrapMethod),
            ("wrong", MixinExtrasKind::ModifyReturnValue),
        ])
    );

    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    let bound: Vec<(&str, &str, GameHookShape)> = weave
        .game_hooks
        .iter()
        .map(|plan| (plan.plan.callback.name.as_str(), plan.hook, plan.shape))
        .collect();
    assert_eq!(
        bound,
        [
            (
                "triple",
                "java_destroy_speed",
                GameHookShape::ModifyReturnValue
            ),
            (
                "plusOne",
                "java_destroy_speed",
                GameHookShape::ModifyReturnValue
            ),
        ],
        "skipped: {:#?}",
        weave.skipped
    );
    let reason = |name: &str| {
        weave
            .skipped
            .iter()
            .find(|row| row.callback.name == name)
            .unwrap_or_else(|| panic!("{name} should be skipped: {:#?}", weave.skipped))
            .clone()
    };
    for (name, label, says) in [
        ("wrong", "@ModifyReturnValue", "does not take and return F"),
        ("wrap", "@WrapMethod", "original method to call"),
        ("expression", "@ModifyExpressionValue", "one instruction"),
        ("operation", "@WrapOperation", "one instruction"),
    ] {
        let row = reason(name);
        assert_eq!(row.at, label);
        assert!(
            matches!(&row.reason, MixinSkipReason::GameHookMismatch(text) if text.contains(says)),
            "{name}: {:?}",
            row.reason
        );
    }
    // `Player#aiStep` has no hook point at all.
    for name in ["condition", "receiver"] {
        assert_eq!(reason(name).reason, MixinSkipReason::TargetOutsideJar);
    }
    // The plain weave binds nothing and still names every injector.
    let plain = jar.plan_load_time_weave(MixinSide::Server);
    assert!(plain.game_hooks.is_empty());
    assert_eq!(
        plain
            .skipped
            .iter()
            .filter(|row| row.at.starts_with("@Modify") || row.at.starts_with("@Wrap"))
            .count(),
        8
    );

    // Fired: 1.5 tripled is 4.5, plus one is 5.5, the speed the host uses.
    let registrations = jar.register_game_hooks(&weave).unwrap();
    let run = jar
        .run_game_hook_returning(
            &registrations,
            &mut GameHookState::default(),
            &mut GameHookContext::new(),
            "java_destroy_speed",
            None,
            &[Value::Ref(Some("game/opaque/block_state".into()))],
            Some(Value::Float(1.5)),
            &mut StdHost::new(),
            &ExecutionBudget::UNBOUNDED,
        )
        .unwrap();
    assert_eq!(run.ran, 2);
    assert_eq!(run.returned, Some(Value::Float(5.5)));
}
