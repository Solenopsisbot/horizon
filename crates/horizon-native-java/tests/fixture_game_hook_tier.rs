//! Runtime-built fixture for the game-class hook tier.
//!
//! A real mod's `@Inject` plans name `net/minecraft` methods, which are outside
//! the jar and so cannot be woven. This fixture builds such a jar with `javac`
//! and drives the tier the way a host does: plan with
//! `plan_load_time_weave_with_game_hooks`, bind the callbacks with
//! `register_game_hooks`, then fire each host hook point with `run_game_hook`.
//!
//! `demo/HookSink` is compile-only, so every `HookSink.mark` call crosses the
//! host boundary and `StdHost` records it: a callback that ran is visible in the
//! host's invocation log, not in a Rust-side stub. `demo/HookSink.ticks` is a
//! jar static, so the same test checks that the state a callback writes survives
//! to the next firing, exactly as a woven callback's would.
//!
//! The scratch jar lives under `.local/` (never the system temp directory), so
//! the run stays inside the checkout.

use horizon_native_java::{
    ExecutionBudget, GameHookContext, GameHookState, GameObject, Host, MixinSide, MixinSkipReason,
    NativeJar, StdHost, Value, game_hook_site_for, game_hook_sites,
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

/// `@Mixin` with the RUNTIME retention the fixture stubs use; real Mixin's
/// `CLASS` retention is a separate discovery story (see the crate docs).
const MIXIN: &str = r#"package org.spongepowered.asm.mixin;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.TYPE)
public @interface Mixin {
    Class<?>[] value() default {};
    String[] targets() default {};
    int priority() default 1000;
}
"#;

const AT: &str = r#"package org.spongepowered.asm.mixin.injection;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
@Target({})
public @interface At {
    String value() default "";
}
"#;

const INJECT: &str = r#"package org.spongepowered.asm.mixin.injection;
import java.lang.annotation.*;
@Retention(RetentionPolicy.RUNTIME)
@Target(ElementType.METHOD)
public @interface Inject {
    String[] method();
    At[] at();
    boolean cancellable() default false;
}
"#;

const CALLBACK_INFO: &str = r#"package org.spongepowered.asm.mixin.injection.callback;
public class CallbackInfo {
    public void cancel() {}
    public boolean isCancelled() { return false; }
}
"#;

/// Compile-only: the callback boundary records every call here.
const HOOK_SINK: &str = r#"package demo;
public final class HookSink {
    public static void mark(String point) {}
}
"#;

/// Shipped in the jar: the counter a callback writes, so the test can check
/// that a callback's static state survives to the next firing.
const COUNTER: &str = r#"package demo;
public final class Counter {
    public static int ticks;
}
"#;

/// Compile-only: the vanilla parameter type of the pickup callback. The host
/// passes the target object's marker, so the class never has to exist in the jar.
const PLAYER: &str = r#"package net.minecraft.world.entity.player;
public class Player {}
"#;

/// Compile-only stubs of the game classes the `@Mixin` annotations name. Real
/// Mixin keeps `CLASS` retention, so `javac` writes these targets as class
/// constants in the annotation and the jar never ships them.
const SERVER_PLAYER: &str = r#"package net.minecraft.server.level;
public class ServerPlayer extends net.minecraft.world.entity.player.Player {}
"#;

const ITEM_ENTITY: &str = r#"package net.minecraft.world.entity.item;
public class ItemEntity {}
"#;

const LEVEL: &str = r#"package net.minecraft.world.level;
public class Level {}
"#;

const MAIN: &str = r#"package demo;
public final class Main {
    public static void onInitialize() {}
}
"#;

fn tick_mixin(callback: &str, at: &str, cancellable: bool, class_name: &str) -> String {
    let cancel = if cancellable {
        ", cancellable = true"
    } else {
        ""
    };
    format!(
        r#"package demo;
import net.minecraft.server.level.ServerPlayer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ServerPlayer.class)
public class {class_name} {{
    @Inject(method = "tick", at = @At("{at}"){cancel})
    private static void {callback}(CallbackInfo ci) {{
        HookSink.mark("tick");
        Counter.ticks++;
    }}
}}
"#
    )
}

const PICKUP_MIXIN: &str = r#"package demo;
import net.minecraft.world.entity.item.ItemEntity;
import net.minecraft.world.entity.player.Player;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ItemEntity.class)
public class PickupMixin {
    @Inject(method = "playerTouch", at = @At("HEAD"), cancellable = true)
    private static void beforePickup(Player player, CallbackInfo ci) {
        HookSink.mark("item_picked_up");
        ci.cancel();
    }
}
"#;

const UNMAPPED_MIXIN: &str = r#"package demo;
import net.minecraft.world.level.Level;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

// `Level#getBlockState` has no host hook point: it must keep the honest
// target-outside-jar refusal.
@Mixin(Level.class)
public class UnmappedMixin {
    @Inject(method = "getBlockState", at = @At("HEAD"))
    private static void onGetBlockState(CallbackInfo ci) {
        HookSink.mark("unmapped");
    }
}
"#;

/// `playerTouch(Player)` but a callback that does not mirror the player
/// argument: the hook cannot hand the callback what it asks for.
const WRONG_ARGS_MIXIN: &str = r#"package demo;
import net.minecraft.world.entity.item.ItemEntity;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ItemEntity.class)
public class WrongArgsMixin {
    @Inject(method = "playerTouch", at = @At("HEAD"))
    private static void beforePickup(CallbackInfo ci) {
        HookSink.mark("wrong_args");
    }
}
"#;

/// Classes javac needs but a real jar would not carry.
const COMPILE_ONLY: &[&str] = &[
    "org/spongepowered/asm/mixin/Mixin.class",
    "org/spongepowered/asm/mixin/injection/At.class",
    "org/spongepowered/asm/mixin/injection/Inject.class",
    "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.class",
    "demo/HookSink.class",
    "net/minecraft/world/entity/player/Player.class",
    "net/minecraft/server/level/ServerPlayer.class",
    "net/minecraft/world/entity/item/ItemEntity.class",
    "net/minecraft/world/level/Level.class",
];

struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    fn build(name: &str) -> Self {
        let sources: BTreeMap<&str, String> = BTreeMap::from([
            ("demo/Counter.java", COUNTER.into()),
            (
                "net/minecraft/world/entity/player/Player.java",
                PLAYER.into(),
            ),
            (
                "net/minecraft/server/level/ServerPlayer.java",
                SERVER_PLAYER.into(),
            ),
            (
                "net/minecraft/world/entity/item/ItemEntity.java",
                ITEM_ENTITY.into(),
            ),
            ("net/minecraft/world/level/Level.java", LEVEL.into()),
            (
                "demo/TickMixin.java",
                tick_mixin("afterTick", "TAIL", false, "TickMixin"),
            ),
            ("demo/PickupMixin.java", PICKUP_MIXIN.into()),
            ("demo/UnmappedMixin.java", UNMAPPED_MIXIN.into()),
            (
                "demo/HeadTickMixin.java",
                tick_mixin("atHead", "HEAD", false, "HeadTickMixin"),
            ),
            (
                "demo/CancelTickMixin.java",
                tick_mixin("cancelTick", "TAIL", true, "CancelTickMixin"),
            ),
            ("demo/WrongArgsMixin.java", WRONG_ARGS_MIXIN.into()),
        ]);
        Self::build_with(
            name,
            sources,
            &[
                "TickMixin",
                "PickupMixin",
                "UnmappedMixin",
                "HeadTickMixin",
                "CancelTickMixin",
                "WrongArgsMixin",
            ],
            COMPILE_ONLY,
        )
    }

    /// Compile `extra` beside the Mixin stubs, `demo/HookSink` and
    /// `demo/Main`, and pack every class but `compile_only` with a config
    /// listing `mixins`.
    fn build_with(
        name: &str,
        extra: BTreeMap<&str, String>,
        mixins: &[&str],
        compile_only: &[&str],
    ) -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/native-java-game-hook")
            .join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let classes = root.join("classes");
        fs::create_dir_all(&classes).unwrap();
        let mut sources: BTreeMap<&str, String> = BTreeMap::from([
            ("org/spongepowered/asm/mixin/Mixin.java", MIXIN.into()),
            ("org/spongepowered/asm/mixin/injection/At.java", AT.into()),
            (
                "org/spongepowered/asm/mixin/injection/Inject.java",
                INJECT.into(),
            ),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
                CALLBACK_INFO.into(),
            ),
            ("demo/HookSink.java", HOOK_SINK.into()),
            ("demo/Main.java", MAIN.into()),
        ]);
        sources.extend(extra);
        let mut paths = Vec::new();
        for (relative, body) in &sources {
            let path = src.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, body).unwrap();
            paths.push(path);
        }
        let status = Command::new(javac())
            .arg("--release")
            .arg("17")
            .arg("-d")
            .arg(&classes)
            .args(paths)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed for {name}");

        let metadata = r#"{"id":"game-hook-tier","version":"1.0","entrypoints":{"main":["demo.Main"]},"mixins":["demo.mixins.json"]}"#;
        let config = serde_json::json!({ "package": "demo", "mixins": mixins }).to_string();
        let jar = root.join(format!("{name}.jar"));
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
            if compile_only.contains(&entry.as_str()) {
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

/// The plan targets the fixture declares, by the class that declares them.
fn skipped_reason(weave: &horizon_native_java::LoadTimeMixinWeave, owner: &str) -> MixinSkipReason {
    weave
        .skipped
        .iter()
        .find(|plan| {
            plan.callback.owner == owner && plan.reason != MixinSkipReason::ListedWithoutPlan
        })
        .or_else(|| {
            weave
                .skipped
                .iter()
                .find(|plan| plan.callback.owner == owner)
        })
        .unwrap_or_else(|| panic!("{owner} should be reported as skipped"))
        .reason
        .clone()
}

#[test]
fn game_hook_plans_register_and_run_at_the_host_point() {
    let fixture = Fixture::build("tier");
    let jar = fixture.load();

    // The shipped entry point keeps every game-class plan refused, because the
    // game does not register host hook callbacks yet.
    let refused = jar.plan_load_time_weave(MixinSide::Server);
    assert!(refused.game_hooks.is_empty());
    assert_eq!(
        skipped_reason(&refused, "demo/TickMixin"),
        MixinSkipReason::TargetOutsideJar,
        "the default weave must not claim a host hook it cannot run"
    );

    // The tier maps the two sites and reports the rest honestly.
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    assert!(weave.woven.is_empty(), "{:#?}", weave.woven);
    let hooks: BTreeMap<_, _> = weave
        .game_hooks
        .iter()
        .map(|plan| (plan.plan.callback.owner.as_str(), plan))
        .collect();
    assert_eq!(hooks.len(), 2, "{:#?}", weave.game_hooks);

    let tick = hooks["demo/TickMixin"];
    assert_eq!(tick.hook, "player_tick");
    assert_eq!(tick.version, 1);
    assert_eq!(tick.anchor, "TAIL");
    assert_eq!(
        tick.plan.target.owner,
        "net/minecraft/server/level/ServerPlayer"
    );
    assert_eq!(tick.plan.target.name, "tick");
    assert_eq!(tick.plan.target.descriptor, "()V");
    assert!(!tick.plan.cancellable);

    // A mapped class is not also reported as skipped.
    for mapped in ["demo/TickMixin", "demo/PickupMixin"] {
        assert!(
            !weave.skipped.iter().any(|row| row.callback.owner == mapped),
            "{mapped} mapped and skipped at once: {:#?}",
            weave.skipped
        );
    }

    let pickup = hooks["demo/PickupMixin"];
    assert_eq!(pickup.hook, "item_picked_up");
    assert_eq!(pickup.anchor, "HEAD");
    assert!(pickup.plan.cancellable);
    assert_eq!(
        pickup.plan.target.descriptor, "(Lnet/minecraft/world/entity/player/Player;)V",
        "an empty plan descriptor takes the site's concrete one"
    );

    // Unmappable targets keep the refusal; mappable targets with the wrong
    // shape say why instead of running somewhere else.
    assert_eq!(
        skipped_reason(&weave, "demo/UnmappedMixin"),
        MixinSkipReason::TargetOutsideJar
    );
    for owner in [
        "demo/HeadTickMixin",
        "demo/CancelTickMixin",
        "demo/WrongArgsMixin",
    ] {
        let reason = skipped_reason(&weave, owner);
        assert!(
            matches!(reason, MixinSkipReason::GameHookMismatch(_)),
            "{owner}: {reason:?}"
        );
        assert_eq!(reason.label(), "game-hook-mismatch");
        assert!(
            reason.detail().is_some_and(|detail| !detail.is_empty()),
            "{owner} needs a bounded reason"
        );
    }

    let registrations = jar.register_game_hooks(&weave).unwrap();
    assert_eq!(registrations.len(), 2);
    assert_eq!(
        registrations.hooks().collect::<Vec<_>>(),
        ["item_picked_up", "player_tick"]
    );

    let mut host = StdHost::new();

    // Firing the point runs the Java callback with the receiver the host names.
    let run = jar
        .run_game_hook(
            &registrations,
            "player_tick",
            Some(Value::Ref(Some("player/7".into()))),
            &[],
            &mut host,
        )
        .unwrap();
    assert_eq!(run.hook, "player_tick");
    assert_eq!(run.ran, 1);
    assert!(!run.cancelled);
    assert_eq!(
        host.invocations()
            .iter()
            .filter(|call| call.method.owner == "demo/HookSink" && call.method.name == "mark")
            .map(|call| call.args.clone())
            .collect::<Vec<_>>(),
        [vec![Value::Ref(Some("tick".into()))]]
    );

    // The callback's static write survives to the next firing, as a woven
    // callback's would.
    let ticks = |host: &StdHost| {
        host.persistent_class_state()
            .0
            .get("demo/Counter#ticksI")
            .cloned()
    };
    assert_eq!(ticks(&host), Some(Value::Int(1)));
    jar.run_game_hook(
        &registrations,
        "player_tick",
        Some(Value::Ref(Some("player/7".into()))),
        &[],
        &mut host,
    )
    .unwrap();
    assert_eq!(ticks(&host), Some(Value::Int(2)));

    // The cancellable hook surfaces the cancellation to the host.
    let run = jar
        .run_game_hook(
            &registrations,
            "item_picked_up",
            Some(Value::Ref(Some("item/3".into()))),
            &[Value::Ref(Some("player/7".into()))],
            &mut host,
        )
        .unwrap();
    assert_eq!(run.ran, 1);
    assert!(run.cancelled);
    assert!(host.invocations().iter().any(|call| {
        call.method.owner == "demo/HookSink"
            && call.args == [Value::Ref(Some("item_picked_up".into()))]
    }));

    // A point with no registration, and a payload of the wrong size, are
    // diagnostics rather than silent no-ops.
    assert!(
        jar.run_game_hook(&registrations, "block_changed", None, &[], &mut host)
            .is_err()
    );
    assert!(
        jar.run_game_hook(
            &registrations,
            "item_picked_up",
            Some(Value::Ref(Some("item/3".into()))),
            &[],
            &mut host,
        )
        .is_err()
    );
}

#[test]
fn the_site_table_matches_the_hook_point_census() {
    let census: serde_json::Value =
        serde_json::from_str(include_str!("../../../native-hooks.json")).unwrap();
    let hooks = census["hooks"].as_array().unwrap();
    for site in game_hook_sites() {
        let hook = hooks
            .iter()
            .find(|hook| hook["name"] == site.hook)
            .unwrap_or_else(|| panic!("{} is not in the hook point census", site.hook));
        assert_eq!(
            hook["version"], site.version,
            "{} version drifted from the census",
            site.hook
        );
        assert_eq!(
            hook["mod_api"]["contract"],
            site.contract.label(),
            "{} contract drifted from the census",
            site.hook
        );
        if let Some(cancellable) = hook.get("cancellable").and_then(|value| value.as_bool()) {
            assert_eq!(
                cancellable,
                matches!(
                    site.contract,
                    horizon_native_java::GameHookContract::Cancel { .. }
                ),
                "{} cancellation drifted from the census",
                site.hook
            );
        }
        if let Some(side) = hook.get("side").and_then(|value| value.as_str()) {
            let expected = match site.side {
                Some(MixinSide::Client) => "client",
                Some(MixinSide::Server) => "server",
                None => "both",
            };
            assert_eq!(side, expected, "{} side drifted from the census", site.hook);
        }
    }

    // Lookup fills an empty descriptor and refuses a different overload.
    let tick = horizon_native_java::MethodRef {
        owner: "net/minecraft/server/level/ServerPlayer".into(),
        name: "tick".into(),
        descriptor: String::new(),
    };
    assert_eq!(
        game_hook_site_for(&tick).map(|site| site.hook),
        Some("player_tick")
    );
    let other_overload = horizon_native_java::MethodRef {
        descriptor: "(I)V".into(),
        ..tick.clone()
    };
    assert!(game_hook_site_for(&other_overload).is_none());
}

/// Mixin's `@Shadow`, with the real annotation's `CLASS` retention: the
/// runtime reads it back from the class bytes, not from a reflective table.
const SHADOW: &str = r#"package org.spongepowered.asm.mixin;
import java.lang.annotation.*;
@Retention(RetentionPolicy.CLASS)
@Target({ElementType.METHOD, ElementType.FIELD})
public @interface Shadow {}
"#;

/// Compile-only game classes with the members the receiver fixture calls.
const RECEIVER_GAME: &[(&str, &str)] = &[
    (
        "net/minecraft/world/entity/Entity.java",
        "package net.minecraft.world.entity;\npublic class Entity { public java.util.UUID getUUID() { return null; } public int getId() { return 0; } }\n",
    ),
    (
        "net/minecraft/world/entity/LivingEntity.java",
        "package net.minecraft.world.entity;\npublic class LivingEntity extends Entity { protected int noJumpDelay; public void jumpFromGround() {} }\n",
    ),
    (
        "net/minecraft/world/entity/player/Player.java",
        "package net.minecraft.world.entity.player;\npublic class Player extends net.minecraft.world.entity.LivingEntity { public void attack(net.minecraft.world.entity.Entity target) {} }\n",
    ),
    (
        "net/minecraft/world/damagesource/DamageSource.java",
        "package net.minecraft.world.damagesource;\npublic class DamageSource {}\n",
    ),
    (
        "net/minecraft/server/level/ServerPlayer.java",
        "package net.minecraft.server.level;\npublic class ServerPlayer extends net.minecraft.world.entity.player.Player { public void tick() {} public void die(net.minecraft.world.damagesource.DamageSource source) {} public Object getFoodData() { return null; } }\n",
    ),
];

/// An interface the tick Mixin adds to every `ServerPlayer`, as real mods add
/// `...Access` interfaces to reach their own fields on another player.
const TICKS_ACCESS: &str = r#"package demo;
public interface TicksAccess {
    int demo$ticks();
}
"#;

/// An instance callback that uses its receiver the way real ones do: a cast
/// to the game class, a `@Shadow` member, a field the Mixin adds (with an
/// initialiser), and an interface the Mixin implements on the target.
const PLAYER_TICK_MIXIN: &str = r#"package demo;
import net.minecraft.server.level.ServerPlayer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ServerPlayer.class)
public abstract class PlayerTickMixin implements TicksAccess {
    @Shadow public abstract int getId();
    private int demo$ticks = 100;

    public int demo$ticks() { return demo$ticks; }

    @Inject(method = "tick", at = @At("TAIL"))
    private void onTick(CallbackInfo ci) {
        demo$ticks++;
        ServerPlayer self = (ServerPlayer) (Object) this;
        HookSink.mark(self.getUUID().toString() + "/" + getId() + "/" + demo$ticks);
        Object other = self;
        if (other instanceof TicksAccess access) {
            HookSink.mark("access/" + access.demo$ticks());
        }
    }
}
"#;

/// A cancellable HEAD callback that decides from its argument.
const ATTACK_MIXIN: &str = r#"package demo;
import net.minecraft.world.entity.Entity;
import net.minecraft.world.entity.player.Player;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(Player.class)
public class AttackMixin {
    @Inject(method = "attack", at = @At("HEAD"), cancellable = true)
    private void onAttack(Entity target, CallbackInfo ci) {
        HookSink.mark("attack/" + target.getId());
        if (target.getId() == 42) {
            ci.cancel();
        }
    }
}
"#;

/// Two callbacks that reach what the model does not hold: a game method it
/// does not answer, and a `@Shadow` field of the game object.
const DEATH_MIXIN: &str = r#"package demo;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.damagesource.DamageSource;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(ServerPlayer.class)
public class DeathMixin {
    @Inject(method = "die", at = @At("HEAD"))
    private void onDeath(DamageSource source, CallbackInfo ci) {
        HookSink.mark("before-food");
        Object food = ((ServerPlayer) (Object) this).getFoodData();
        HookSink.mark("after-food");
    }
}
"#;

const JUMP_MIXIN: &str = r#"package demo;
import net.minecraft.world.entity.LivingEntity;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.Shadow;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

@Mixin(LivingEntity.class)
public class JumpMixin {
    @Shadow protected int noJumpDelay;

    @Inject(method = "jumpFromGround", at = @At("TAIL"))
    private void onJump(CallbackInfo ci) {
        HookSink.mark("jump/" + noJumpDelay);
    }
}
"#;

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
fn game_hook_callbacks_see_a_modelled_receiver_and_refuse_the_rest() {
    let mut sources: BTreeMap<&str, String> = RECEIVER_GAME
        .iter()
        .map(|(path, body)| (*path, (*body).to_owned()))
        .collect();
    sources.insert("org/spongepowered/asm/mixin/Shadow.java", SHADOW.into());
    sources.insert("demo/TicksAccess.java", TICKS_ACCESS.into());
    sources.insert("demo/PlayerTickMixin.java", PLAYER_TICK_MIXIN.into());
    sources.insert("demo/AttackMixin.java", ATTACK_MIXIN.into());
    sources.insert("demo/DeathMixin.java", DEATH_MIXIN.into());
    sources.insert("demo/JumpMixin.java", JUMP_MIXIN.into());
    let compile_only: Vec<&str> = COMPILE_ONLY
        .iter()
        .copied()
        .chain([
            "org/spongepowered/asm/mixin/Shadow.class",
            "net/minecraft/world/entity/Entity.class",
            "net/minecraft/world/entity/LivingEntity.class",
            "net/minecraft/world/damagesource/DamageSource.class",
        ])
        .collect();
    let fixture = Fixture::build_with(
        "receiver",
        sources,
        &["PlayerTickMixin", "AttackMixin", "DeathMixin", "JumpMixin"],
        &compile_only,
    );
    let jar = fixture.load();
    let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
    let registrations = jar.register_game_hooks(&weave).unwrap();
    assert_eq!(
        registrations.hooks().collect::<Vec<_>>(),
        ["attack_entity", "living_jump", "player_died", "player_tick"],
        "{:#?}",
        weave.skipped
    );

    // Lenient, so the compile-only `HookSink` calls are recorded: every
    // refusal below is the hook tier's own, not the strict host's.
    let mut host = StdHost::new();
    let mut state = GameHookState::default();
    let budget = ExecutionBudget::within(std::time::Duration::from_secs(5));
    let alice = 0x0123_4567_89ab_cdef_0011_2233_4455_6677_u128;
    let bob = 0x00ff_u128;
    let tick = |state: &mut GameHookState, host: &mut StdHost, player: GameObject| {
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        jar.run_game_hook_in(
            &registrations,
            state,
            &mut context,
            "player_tick",
            Some(receiver),
            &[],
            host,
            &budget,
        )
    };

    // The receiver answers its UUID and id; the Mixin's initialiser ran once
    // for the object, and its field survives to the next tick; the interface
    // the Mixin adds is implemented on the player.
    let alice_object = || GameObject::player(alice, "Alice").with_entity_id(7);
    tick(&mut state, &mut host, alice_object()).unwrap();
    tick(&mut state, &mut host, alice_object()).unwrap();
    // A second player is a second object with its own field.
    tick(
        &mut state,
        &mut host,
        GameObject::player(bob, "Bob").with_entity_id(9),
    )
    .unwrap();
    assert_eq!(
        marks(&host),
        [
            "01234567-89ab-cdef-0011-223344556677/7/101",
            "access/101",
            "01234567-89ab-cdef-0011-223344556677/7/102",
            "access/102",
            "00000000-0000-0000-0000-0000000000ff/9/101",
            "access/101",
        ]
    );

    // A forgotten object (a player who left) starts fresh.
    state.forget_object(&alice_object().marker());
    tick(&mut state, &mut host, alice_object()).unwrap();
    assert_eq!(marks(&host).last().map(String::as_str), Some("access/101"));

    // The cancellable callback decides from its argument's id.
    let attack = |state: &mut GameHookState, host: &mut StdHost, target: i32| {
        let mut context = GameHookContext::new();
        let receiver = context.insert(alice_object());
        let target = context.insert(GameObject::entity(target, "minecraft:zombie", true));
        jar.run_game_hook_in(
            &registrations,
            state,
            &mut context,
            "attack_entity",
            Some(receiver),
            &[target],
            host,
            &budget,
        )
        .unwrap()
    };
    assert!(attack(&mut state, &mut host, 42).cancelled);
    assert!(!attack(&mut state, &mut host, 5).cancelled);

    // A game method the model does not answer stops the callback where it
    // was called, with an error naming it, instead of answering null.
    let mut context = GameHookContext::new();
    let receiver = context.insert(alice_object());
    let source = context.insert(GameObject::opaque(
        "net/minecraft/world/damagesource/DamageSource",
        "lava",
    ));
    let error = jar
        .run_game_hook_in(
            &registrations,
            &mut state,
            &mut context,
            "player_died",
            Some(receiver),
            &[source],
            &mut host,
            &budget,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("getFoodData"), "{error}");
    let after = marks(&host);
    assert_eq!(after.last().map(String::as_str), Some("before-food"));

    // So does a `@Shadow` field read, which the model does not hold either.
    let mut context = GameHookContext::new();
    let receiver = context.insert(GameObject::entity(3, "minecraft:zombie", true));
    let error = jar
        .run_game_hook_in(
            &registrations,
            &mut state,
            &mut context,
            "living_jump",
            Some(receiver),
            &[],
            &mut host,
            &budget,
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("noJumpDelay") && error.contains("@Shadow"),
        "{error}"
    );
    assert!(!marks(&host).iter().any(|mark| mark.starts_with("jump/")));

    // An object the firing does not describe (kept from an earlier one) is
    // not live: its members refuse.
    let mut context = GameHookContext::new();
    let error = jar
        .run_game_hook_in(
            &registrations,
            &mut state,
            &mut context,
            "player_tick",
            Some(Value::Ref(Some(alice_object().marker()))),
            &[],
            &mut host,
            &budget,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("not live"), "{error}");
}
