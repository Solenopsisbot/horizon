//! Probe the committed source-only Fabric fixture through the native runtime.
//!
//! The fixture builder compiles Java with `javac` and packages the resulting
//! class files. Keeping compilation here makes this test exercise classfile
//! parsing and execution against a representative jar instead of Rust-built
//! test doubles or hand-written bytecode.

use horizon_native_java::{NativeJar, StdHost};
use std::{
    env, fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn scratch_dir() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    env::temp_dir().join(format!(
        "horizon-compiled-fabric-{}-{stamp}",
        std::process::id()
    ))
}

#[test]
fn native_probe_executes_compiled_fabric_fixture() {
    if Command::new("javac").arg("-version").output().is_err()
        || Command::new("jar").arg("--version").output().is_err()
    {
        return;
    }

    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root")
        .to_owned();
    let output = scratch_dir();
    let status = Command::new("python3")
        .arg(repo.join("tools/real_mod_fixtures/build.py"))
        .arg("--output")
        .arg(&output)
        .current_dir(&repo)
        .status()
        .expect("launch fixture builder");
    assert!(status.success(), "fixture builder failed with {status}");

    let jar = NativeJar::from_path(&output.join("fabric-fixture.jar"))
        .expect("load compiled Fabric fixture");
    assert_eq!(jar.fabric_id.as_deref(), Some("native_fixture_fabric"));

    let entry = jar
        .discover_entrypoint("main")
        .expect("discover Fabric main entrypoint");
    assert_eq!(entry.class, "demo.fabric.Mod");

    let configs = jar.mixin_configs();
    assert_eq!(configs.len(), 1);
    assert_eq!(configs[0].path, "native_fixture.mixins.json");
    let plans = jar.mixin_injection_plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].target.owner, "demo/fabric/Target");
    assert_eq!(plans[0].target.name, "tick");

    let mut host = jar.seed_host(StdHost::new());
    let report = jar
        .execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .expect("execute compiled Fabric initializer");
    assert!(report.calls > 0, "initializer should execute bytecode");
    assert!(host.invocations().iter().any(|invocation| {
        invocation.method.owner == "net/minecraft/resources/Identifier"
            && invocation.method.name == "fromNamespaceAndPath"
    }));
    assert!(host.invocations().iter().any(|invocation| {
        invocation.method.owner == "net/fabricmc/fabric/api/networking/v1/ServerPlayNetworking"
            && invocation.method.name == "registerGlobalReceiver"
    }));
}
