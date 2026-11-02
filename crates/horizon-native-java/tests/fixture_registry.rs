use horizon_native_java::{NativeJar, RegistryEntry, StdHost, TypedRegistrySink, Value};
use std::fs;

struct Recording(Vec<RegistryEntry>);

impl TypedRegistrySink for Recording {
    fn register_typed(&mut self, entry: &RegistryEntry) -> Result<(), String> {
        self.0.push(entry.clone());
        Ok(())
    }
}

#[test]
fn fabric_fixture_runs_entrypoint_and_carries_resource_and_registry_declaration() {
    let root = std::env::temp_dir().join(format!(
        "horizon-native-java-fixture-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let jar_path = root.join("fixture-registry.jar");
    fs::write(&jar_path, include_bytes!("fixtures/fixture-registry.jar")).unwrap();

    let jar = NativeJar::from_path(&jar_path).unwrap();
    assert_eq!(jar.fabric_id.as_deref(), Some("fixture-registry"));
    assert_eq!(jar.required_mods, Vec::<String>::new());
    let entry = jar.discover_entrypoint("main").unwrap();
    assert_eq!(entry.class, "fixture.RegistryEntrypoint");
    assert_eq!(jar.native_declarations.len(), 1);
    assert_eq!(
        jar.native_declarations[0].key(),
        "minecraft:item/fixture:crystal"
    );

    let mut host =
        StdHost::new().with_resource("assets/fixture-registry/config.json", "fixture-config");
    let report = jar
        .execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    assert_eq!(report.class, "fixture.RegistryEntrypoint");
    assert!(matches!(
        host.resource("assets/fixture-registry/config.json"),
        Some("fixture-config")
    ));
    let mut sink = Recording(Vec::new());
    for declaration in &jar.native_declarations {
        sink.register_typed(declaration).unwrap();
    }
    assert_eq!(sink.0.len(), 1);
    assert_eq!(sink.0[0].key(), "minecraft:item/fixture:crystal");
    assert!(matches!(sink.0[0].value, Value::Ref(Some(_))));

    fs::remove_dir_all(root).unwrap();
}
