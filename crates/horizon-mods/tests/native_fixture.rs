use horizon_mods::native::NativeDeclarations;
use horizon_native_java::{NativeJar, StdHost};
use std::{fs, io::Write, process::Command};
use zip::write::SimpleFileOptions;

#[test]
fn runtime_fabric_jar_reaches_native_content_adapter() {
    let root = std::env::temp_dir().join(format!(
        "horizon-mods-native-adapter-fixture-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let source = root.join("src/demo/TypedMod.java");
    let classes = root.join("classes");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::create_dir_all(&classes).unwrap();
    fs::write(
        &source,
        "package demo; public final class TypedMod { public void onInitialize() {} }\n",
    )
    .unwrap();
    let status = Command::new("javac")
        .arg("--release")
        .arg("17")
        .arg("-d")
        .arg(&classes)
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success(), "javac failed");

    let jar_path = root.join("typed.jar");
    let file = fs::File::create(&jar_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    zip.start_file("fabric.mod.json", options).unwrap();
    zip.write_all(
        br#"{"id":"typed-fixture","version":"1.0","entrypoints":{"main":"demo.TypedMod"}}"#,
    )
    .unwrap();
    zip.start_file("demo/TypedMod.class", options).unwrap();
    zip.write_all(&fs::read(classes.join("demo/TypedMod.class")).unwrap())
        .unwrap();
    zip.start_file("META-INF/horizon-native.json", options)
        .unwrap();
    zip.write_all(
        br#"{"id":"typed-fixture","registrations":[{"registry":"minecraft:block","id":"typed:lamp","kind":"block","template":"minecraft:stone","item":true,"properties":{"light":12}},{"registry":"minecraft:item","id":"typed:wand","kind":"item","template":"minecraft:stick","properties":{"max_stack_size":16}},{"registry":"minecraft:mob_effect","id":"typed:glow","kind":"mob_effect","category":"beneficial","color":16711935}]}"#,
    )
    .unwrap();
    zip.finish().unwrap();

    let jar = NativeJar::from_path(&jar_path).unwrap();
    let entry = jar.discover_entrypoint("main").unwrap();
    let mut host = StdHost::new();
    let report = jar
        .execute(&entry.class, entry.method.as_deref(), "main", &mut host)
        .unwrap();
    assert_eq!(report.class, "demo.TypedMod");
    assert_eq!(report.method, "onInitialize");

    let declarations = NativeDeclarations::from_registry_entries(jar.native_declarations.iter())
        .expect("typed declarations should cross the Horizon adapter");
    assert_eq!(declarations.blocks.len(), 1);
    assert_eq!(declarations.blocks[0].id, "typed:lamp");
    assert_eq!(declarations.items.len(), 1);
    assert_eq!(declarations.items[0].id, "typed:wand");
    assert_eq!(declarations.effects.len(), 1);
    assert_eq!(declarations.effects[0].id, "typed:glow");

    fs::remove_dir_all(root).unwrap();
}
