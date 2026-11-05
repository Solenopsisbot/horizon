//! Runtime-built fixtures for the load-time Mixin weave.
//!
//! These drive the path the game's loader takes: `NativeJar::from_path`, then
//! `plan_load_time_weave`, then `with_woven_classes`, then an ordinary
//! `execute`. No JVM and no `--weave` jar is involved, so a callback that runs
//! here ran from the rewritten bytecode of its target.
//!
//! `demo/Probe` exists only at compile time: leaving it out of the jar makes
//! every `Probe.mark` call cross the host boundary, where `StdHost` records the
//! order and the arguments.

use horizon_native_java::{MixinSide, MixinSkipReason, NativeJar, StdHost, Value};
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

/// `@Mixin`, with the retention the stub declares. Real Mixin uses `CLASS`;
/// `RUNTIME` is what makes the discovery path reachable from a fixture.
fn mixin_stub(retention: &str) -> String {
    format!(
        r#"package org.spongepowered.asm.mixin;
import java.lang.annotation.*;
@Retention(RetentionPolicy.{retention})
@Target(ElementType.TYPE)
public @interface Mixin {{
    Class<?>[] value() default {{}};
    String[] targets() default {{}};
    int priority() default 1000;
}}
"#
    )
}

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
}
"#;

const PROBE: &str = r#"package demo;
public final class Probe {
    public static void mark(String marker, int value) {}
}
"#;

/// Everything every fixture jar shares: the Mixin stubs, which are compile-time
/// only in a real jar too, and `Probe`.
const COMPILE_ONLY: &[&str] = &[
    "org/spongepowered/asm/mixin/Mixin.class",
    "org/spongepowered/asm/mixin/injection/At.class",
    "org/spongepowered/asm/mixin/injection/Inject.class",
    "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.class",
    "demo/Probe.class",
];

/// A fixture mod: `demo.Main` is its `main` entrypoint, `configs` are declared
/// by `fabric.mod.json` and shipped at the jar root.
struct Fixture {
    root: PathBuf,
    jar: PathBuf,
}

impl Fixture {
    fn build(
        name: &str,
        retention: &str,
        sources: &[(&str, &str)],
        configs: &[(&str, &str)],
    ) -> Self {
        let root = std::env::temp_dir().join(format!(
            "horizon-native-java-mixin-woven-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let classes = root.join("classes");
        fs::create_dir_all(&classes).unwrap();
        let mixin = mixin_stub(retention);
        let shared = [
            ("org/spongepowered/asm/mixin/Mixin.java", mixin.as_str()),
            ("org/spongepowered/asm/mixin/injection/At.java", AT),
            ("org/spongepowered/asm/mixin/injection/Inject.java", INJECT),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
                CALLBACK_INFO,
            ),
            ("demo/Probe.java", PROBE),
        ];
        let mut paths = Vec::new();
        for (relative, body) in shared.iter().chain(sources) {
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

        let declared = configs
            .iter()
            .map(|(path, _)| format!("\"{path}\""))
            .collect::<Vec<_>>()
            .join(",");
        let metadata = format!(
            r#"{{"id":"woven-{name}","version":"1.0","entrypoints":{{"main":["demo.Main"]}},"mixins":[{declared}]}}"#
        );
        let jar = root.join(format!("{name}.jar"));
        let options = SimpleFileOptions::default();
        let mut zip = zip::ZipWriter::new(fs::File::create(&jar).unwrap());
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(metadata.as_bytes()).unwrap();
        for (path, body) in configs {
            zip.start_file(*path, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        for class in files_under(&classes) {
            let entry = class
                .strip_prefix(&classes)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if COMPILE_ONLY.contains(&entry.as_str()) {
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

/// Run the jar's `main` entrypoint and return the `Probe.mark` calls in order.
fn run_main(jar: &NativeJar) -> Vec<(String, i32)> {
    let mut host = jar.seed_host(StdHost::new());
    jar.execute("demo.Main", None, "main", &mut host).unwrap();
    host.invocations()
        .iter()
        .filter(|invocation| invocation.method.owner == "demo/Probe")
        .map(|invocation| match invocation.args.as_slice() {
            [Value::Ref(Some(marker)), Value::Int(value)] => (marker.clone(), *value),
            other => panic!("unexpected Probe.mark arguments: {other:?}"),
        })
        .collect()
}

fn marks(calls: &[(&str, i32)]) -> Vec<(String, i32)> {
    calls
        .iter()
        .map(|(marker, value)| ((*marker).to_owned(), *value))
        .collect()
}

/// The jar the game would execute: planned and rebuilt for `side`.
fn woven(jar: NativeJar, side: MixinSide) -> (NativeJar, horizon_native_java::LoadTimeMixinWeave) {
    let weave = jar.plan_load_time_weave(side);
    let jar = jar.with_woven_classes(&weave.classes).unwrap();
    (jar, weave)
}

const MAIN_RUNS_TARGET: &str = r#"package demo;
public final class Main {
    public void onInitialize() {
        Target.run();
        Probe.mark("after", Counter.value);
    }
}
"#;

const COUNTER: &str = r#"package demo;
public final class Counter {
    public static int value;
}
"#;

#[test]
fn a_woven_head_callback_runs_when_its_target_runs() {
    let fixture = Fixture::build(
        "counter",
        "RUNTIME",
        &[
            ("demo/Main.java", MAIN_RUNS_TARGET),
            ("demo/Counter.java", COUNTER),
            (
                "demo/Target.java",
                r#"package demo;
public final class Target {
    public static void run() { Probe.mark("target", Counter.value); }
}
"#,
            ),
            (
                "demo/TargetMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
@Mixin(Target.class)
public abstract class TargetMixin {
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void onRun() {
        Counter.value++;
        Probe.mark("callback", Counter.value);
    }
}
"#,
            ),
        ],
        &[(
            "demo.mixins.json",
            r#"{"package":"demo","mixins":["TargetMixin"]}"#,
        )],
    );

    // The jar alone never runs the mixin: this is what production did before.
    let plain = fixture.load();
    assert_eq!(
        run_main(&plain),
        marks(&[("target", 0), ("after", 0)]),
        "an unwoven jar must not reach the callback"
    );

    let (jar, weave) = woven(plain.clone(), MixinSide::Server);
    assert_eq!(weave.woven.len(), 1);
    assert_eq!(weave.skipped, [], "nothing should be left out");
    assert_eq!(
        run_main(&jar),
        marks(&[("callback", 1), ("target", 1), ("after", 1)]),
        "the callback runs first and its static write is visible to the target"
    );

    // The weave changes classes, not what the jar says about itself.
    assert_eq!(jar.mixin_injection_plans().len(), 1);
    assert_eq!(jar.mixin_injection_plans(), plain.mixin_injection_plans());
    assert_eq!(jar.fabric_id.as_deref(), Some("woven-counter"));
    assert_eq!(jar.fabric_entrypoints, plain.fabric_entrypoints);
    assert_eq!(
        jar.resource_names().collect::<Vec<_>>(),
        plain.resource_names().collect::<Vec<_>>()
    );
    assert_eq!(jar.mixin_configs(), plain.mixin_configs());
}

/// The names are deliberately not alphabetical: discovery sorts by callback
/// name, so a fixture of `first`/`second` would pass by accident.
#[test]
fn head_callbacks_run_in_mixin_apply_order() {
    let fixture = Fixture::build(
        "order",
        "RUNTIME",
        &[
            ("demo/Main.java", MAIN_RUNS_TARGET),
            ("demo/Counter.java", COUNTER),
            (
                "demo/Target.java",
                r#"package demo;
public final class Target {
    public static void run() { Probe.mark("target", 0); }
}
"#,
            ),
            (
                "demo/ZetaMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
@Mixin(Target.class)
public abstract class ZetaMixin {
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void zulu() { Probe.mark("zulu", 0); }
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void bravo() { Probe.mark("bravo", 0); }
}
"#,
            ),
            (
                "demo/AlphaMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
@Mixin(Target.class)
public abstract class AlphaMixin {
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void alpha() { Probe.mark("alpha", 0); }
}
"#,
            ),
            (
                "demo/EarlyMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
@Mixin(value = Target.class, priority = 500)
public abstract class EarlyMixin {
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void early() { Probe.mark("early", 0); }
}
"#,
            ),
        ],
        &[(
            "demo.mixins.json",
            r#"{"package":"demo","mixins":["ZetaMixin","AlphaMixin","EarlyMixin"]}"#,
        )],
    );
    let (jar, weave) = woven(fixture.load(), MixinSide::Client);
    assert_eq!(weave.skipped, []);
    let expected = marks(&[
        // Priority beats list position, list position beats the class name, and
        // a class's own callbacks keep the order of its methods.
        ("early", 0),
        ("zulu", 0),
        ("bravo", 0),
        ("alpha", 0),
        ("target", 0),
        ("after", 0),
    ]);
    assert_eq!(run_main(&jar), expected);
    // The plans say the same thing the bytecode does.
    let names = jar
        .mixin_plans_in_apply_order()
        .into_iter()
        .map(|plan| plan.callback.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["early", "zulu", "bravo", "alpha"]);
    assert_eq!(
        weave
            .woven
            .iter()
            .map(|plan| plan.callback.name.as_str())
            .collect::<Vec<_>>(),
        names
    );
}

#[test]
fn a_mixed_jar_weaves_one_target_and_reports_the_other() {
    let fixture = Fixture::build(
        "mixed",
        "RUNTIME",
        &[
            (
                "demo/Main.java",
                r#"package demo;
public final class Main {
    public void onInitialize() {
        Target.run();
        Other.run();
    }
}
"#,
            ),
            (
                "demo/Target.java",
                r#"package demo;
public final class Target {
    public static void run() { Probe.mark("target", 0); }
}
"#,
            ),
            (
                "demo/Other.java",
                r#"package demo;
public final class Other {
    public static void run() { Probe.mark("other", 0); }
}
"#,
            ),
            (
                "demo/PlainMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
@Mixin(Target.class)
public abstract class PlainMixin {
    @Inject(method = "run()V", at = @At("HEAD"))
    private static void woven() { Probe.mark("woven", 0); }
}
"#,
            ),
            (
                "demo/CancelMixin.java",
                r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(Other.class)
public abstract class CancelMixin {
    @Inject(method = "run()V", at = @At("HEAD"), cancellable = true)
    private static void cancels(CallbackInfo info) { info.cancel(); }
}
"#,
            ),
        ],
        &[(
            "demo.mixins.json",
            r#"{"package":"demo","mixins":["PlainMixin","CancelMixin"]}"#,
        )],
    );
    let (jar, weave) = woven(fixture.load(), MixinSide::Server);
    assert_eq!(jar.mixin_injection_plans().len(), 2);
    assert_eq!(weave.woven.len(), 1);
    assert_eq!(weave.woven[0].callback.name, "woven");
    assert_eq!(weave.skipped.len(), 1);
    assert_eq!(weave.skipped[0].reason, MixinSkipReason::Cancellable);
    assert_eq!(weave.skipped[0].callback.name, "cancels");
    assert_eq!(
        weave.skipped[0].to_string(),
        "cancellable HEAD demo/Other#run()V <- demo/CancelMixin#cancels\
         (Lorg/spongepowered/asm/mixin/injection/callback/CallbackInfo;)V"
    );
    // The skipped target runs untouched, its cancel never applied, rather than
    // half woven.
    assert_eq!(
        run_main(&jar),
        marks(&[("woven", 0), ("target", 0), ("other", 0)])
    );
    assert_eq!(weave.classes.classes.len(), 1);
    assert!(weave.classes.classes.contains_key("demo/Target"));
}
