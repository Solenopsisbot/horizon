//! One mod jar on its own, as the launcher's mod browser installs it: what
//! of it Horizon runs, said before install (Works, Partly, Doesn't run),
//! from the jar itself. The jar is carried over ([`crate::carry`]) into a
//! folder content mod, and its classes are read ([`crate::classes`]) for
//! what the carry can't take: block entities, entities, Mixins, behaviour.
//!
//! - **Works**: content only. Its blocks, items, recipes, loot, tags and
//!   worldgen port, and its code only registers them.
//! - **Partly**: the content ports; the code features listed don't.
//! - **Doesn't run**: nothing to port (everything it does is Java code), or
//!   the jar can't be carried at all.
//!
//! Beside the report a check writes its provenance (`docs/java-mod-distribution.md`
//! §5): the record `horizon_mods::java_artifact` keys, made from the jar's bytes
//! and the versions this check depended on. Nothing reads it to skip a check —
//! the report is exactly what it was — so the record only says why this answer
//! belongs to this jar.
use crate::{carry, classes, jar};
use horizon_mods::api_level;
use horizon_mods::java_artifact::{Abi, Format, Provenance, Source};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Read, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Works,
    Partly,
    No,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Works => "Works",
            Verdict::Partly => "Partly",
            Verdict::No => "Doesn't run",
        }
    }
}

/// What checking a jar found. Written as `check.json` beside the carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub mod_id: String,
    pub name: String,
    pub version: String,
    /// `neoforge`, `forge`, `fabric`, `quilt`, or `` for a plain zip.
    pub loader: String,
    pub license: String,
    pub verdict: Verdict,
    /// One line: why this verdict.
    pub summary: String,
    /// What ports: "12 blocks", "40 recipes"...
    pub ports: Vec<String>,
    /// What doesn't, with why.
    pub skipped: Vec<String>,
    /// Required mods, by id.
    pub depends: Vec<String>,
    /// The folder mod's namespaces (its id first): two mods on at once may
    /// not share one.
    pub namespaces: Vec<String>,
    pub code: classes::Code,
    /// Result of the optional bounded native Java probe. This is evidence for
    /// the launcher and never changes the content-port verdict by itself.
    #[serde(default)]
    pub native: Option<NativeProbe>,
}

/// The report's provenance file, beside `check.json` in the same folder
/// ([`provenance`]).
pub const PROVENANCE: &str = "check.provenance.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NativeProbe {
    /// Entrypoint kind used: Fabric `main`, `client`, `server`, or NeoForge.
    #[serde(default)]
    pub kind: Option<String>,
    pub entrypoint: Option<String>,
    /// Every entrypoint attempted for the selected lifecycle kind. The
    /// singular `entrypoint` field remains the first one for backwards
    /// compatibility with existing check reports.
    #[serde(default)]
    pub entrypoints: Vec<String>,
    /// Mod ids visible to the deterministic loader during this probe,
    /// including the jar itself and its declared required dependencies.
    #[serde(default)]
    pub seeded_mods: Vec<String>,
    pub calls: usize,
    /// NeoForge registrations recorded by the deterministic host.
    #[serde(default)]
    pub registrations: usize,
    /// NeoForge listener registrations recorded by the deterministic host.
    #[serde(default)]
    pub listeners: usize,
    /// NeoForge payload registrations recorded by the deterministic host.
    #[serde(default)]
    pub payloads: usize,
    pub error: Option<String>,
}

fn seeded_mod_ids(jar_path: &Path) -> Vec<String> {
    let Ok(info) = jar::read(jar_path) else {
        return Vec::new();
    };
    let mut ids = info.depends;
    if !info.mod_id.is_empty() {
        ids.push(info.mod_id);
    }
    ids.sort();
    ids.dedup();
    ids
}

fn seeded_native_host(jar_path: &Path) -> horizon_native_java::StdHost {
    let mut host = horizon_native_java::StdHost::new();
    if let Ok(info) = jar::read(jar_path) {
        if !info.mod_id.is_empty() {
            host = host.with_mod(&info.mod_id, &info.version);
        }
        for dependency in info.depends {
            host = host.with_mod(dependency, "unknown");
        }
    }
    host
}

fn native_probe(jar_path: &Path) -> NativeProbe {
    let seeded_mods = seeded_mod_ids(jar_path);
    let jar = match horizon_native_java::NativeJar::from_path(jar_path) {
        Ok(jar) => jar,
        Err(error) => {
            return NativeProbe {
                kind: None,
                entrypoint: None,
                entrypoints: Vec::new(),
                seeded_mods,
                calls: 0,
                registrations: 0,
                listeners: 0,
                payloads: 0,
                error: Some(error.to_string()),
            };
        }
    };
    let selected = ["main", "client", "server", "neoforge"]
        .into_iter()
        .find_map(|kind| jar.discover_entrypoint(kind).map(|entry| (kind, entry)));
    let Some((kind, _entry)) = selected else {
        return NativeProbe {
            kind: None,
            entrypoint: None,
            entrypoints: Vec::new(),
            seeded_mods,
            calls: 0,
            registrations: 0,
            listeners: 0,
            payloads: 0,
            error: Some("no Fabric main/client/server or NeoForge entrypoint".into()),
        };
    };
    let kind = kind.to_owned();
    let entries = jar.discover_entrypoints(&kind);
    let entrypoint = entries.first().map(|entry| entry.class.clone());
    let entrypoints = entries
        .iter()
        .map(|entry| entry.class.clone())
        .collect::<Vec<_>>();
    // Seed the deterministic loader view from the jar's own metadata.  A
    // real entrypoint commonly gates registration on its own id or on a
    // required dependency; probing with an empty map would report both as
    // absent and make the native result misleading.
    let mut host = seeded_native_host(jar_path);
    let mut calls = 0;
    let mut errors = Vec::new();
    for entry in &entries {
        match jar.execute(&entry.class, entry.method.as_deref(), &kind, &mut host) {
            Ok(report) => calls += report.calls,
            Err(entry_error) => {
                errors.push(format!("{}: {entry_error}", entry.class));
            }
        }
    }
    NativeProbe {
        kind: Some(kind),
        entrypoint,
        entrypoints,
        seeded_mods,
        calls,
        registrations: host.registrations().len(),
        listeners: host.listener_registrations().count(),
        payloads: host.payload_registrations().len(),
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

/// The loader a jar is made for, from its metadata file.
pub(crate) fn loader(names: &[String]) -> &'static str {
    let has = |n: &str| names.iter().any(|x| x == n);
    if has("META-INF/neoforge.mods.toml") {
        "neoforge"
    } else if has("META-INF/mods.toml") {
        "forge"
    } else if has("fabric.mod.json") {
        "fabric"
    } else if has("quilt.mod.json") {
        "quilt"
    } else {
        ""
    }
}

/// Data folders named as before 1.21 (`recipes/`, `loot_tables/`) and none
/// as 1.21 names them: the game doesn't read them, and the carry upgrades
/// from 1.21.1 on. (Some jars keep a stale plural folder beside the new one.)
fn old_layout(names: &[String]) -> bool {
    let has = |kinds: [&str; 2]| {
        names.iter().any(|n| {
            let p: Vec<&str> = n.split('/').collect();
            p.len() > 3 && p[0] == "data" && kinds.contains(&p[2]) && n.ends_with(".json")
        })
    };
    has(["recipes", "loot_tables"]) && !has(["recipe", "loot_table"])
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The report's provenance, written as `check.provenance.json` beside it: the
/// jar whose bytes it describes, and the versions `horizon-port-pack check`
/// answers for.
///
/// The jar is the input (its own SHA-256, so a swapped jar is a different
/// artifact). [`Format::new`] takes this crate's IR version; the ABI is the
/// mod ABI level, and there is no hook level because `check` weaves nothing:
/// a reader expecting the hook level finds it absent, not borrowed.
pub fn provenance(jar_path: &Path) -> Result<Provenance, String> {
    format_provenance(jar_path, abi())
}

fn abi() -> Abi {
    Abi {
        game: horizon_core::version::VERSION_ID.into(),
        horizon_mod_abi: api_level::LEVEL,
        native_hooks: None,
    }
}

fn format_provenance(jar_path: &Path, abi: Abi) -> Result<Provenance, String> {
    let source = Source::of_file("jar", jar_path)?;
    Ok(Provenance::new(
        "check",
        Format::new(abi),
        [source],
        "check.json",
    ))
}

/// Checks `jar` and carries it over into `out/carry/` (`out/check.json`,
/// `out/carry-report.json` beside it). The carry is the folder mod an
/// install copies.
pub fn check(jar_path: &Path, out: &Path) -> Result<Check, String> {
    let info = jar::read(jar_path)?;
    let file = fs::File::open(jar_path).map_err(|e| format!("{}: {e}", jar_path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", jar_path.display()))?;
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut bytes = Vec::new();
    for n in names.iter().filter(|n| n.ends_with(".class")) {
        let mut b = Vec::new();
        if let Ok(mut e) = zip.by_name(n)
            && e.size() < 1 << 24
            && e.read_to_end(&mut b).is_ok()
        {
            bytes.push(b);
        }
    }
    let code = classes::census(bytes.iter().map(Vec::as_slice));
    let mut check = Check {
        mod_id: info.mod_id.clone(),
        name: info.name.clone(),
        version: info.version.clone(),
        loader: loader(&names).to_owned(),
        license: info.license.clone(),
        verdict: Verdict::No,
        summary: String::new(),
        ports: Vec::new(),
        skipped: Vec::new(),
        depends: info.depends.clone(),
        namespaces: Vec::new(),
        code,
        native: Some(native_probe(jar_path)),
    };
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let done = |check: Check| -> Result<Check, String> {
        crate::pipeline::write_json(&out.join("check.json"), &check)?;
        // Provenance is beside the report, not part of it: a check that cannot
        // describe its jar still answers (the report is what the install
        // reads), so this is logged and not returned.
        if let Err(e) = provenance(jar_path)
            .and_then(|record| crate::pipeline::write_json(&out.join(PROVENANCE), &record))
        {
            eprintln!("provenance not recorded for {}: {e}", jar_path.display());
        }
        Ok(check)
    };
    if old_layout(&names) {
        check.summary = "Made for Minecraft 1.20 or older: Horizon ports mods made for 1.21 \
                         and later."
            .into();
        return done(check);
    }
    let alone = |m: &str| m == info.mod_id || m == "minecraft";
    let pack = carry::Pack {
        loaded: &alone,
        mods: &alone,
    };
    let report = match carry::carry(
        jar_path,
        &info,
        out,
        None,
        &|_| true,
        &pack,
        &BTreeMap::new(),
    ) {
        Ok(r) => r,
        Err(e) => {
            check.summary = format!("Can't be carried over: {e}");
            return done(check);
        }
    };
    check.namespaces = std::iter::once(info.mod_id.clone())
        .chain(report.namespaces.iter().cloned())
        .collect();
    let c = &info.census;
    let recipes = report
        .recipes_upgraded
        .saturating_sub(report.recipes_unregistered.len());
    let loot = c.loot_tables + report.loot_tables_added;
    let content =
        report.blocks.len() + report.items + recipes + loot + report.worldgen + c.structures;
    for (n, one, many) in [
        (report.blocks.len(), "block", "blocks"),
        (report.items, "item", "items"),
        (recipes, "recipe", "recipes"),
        (loot, "loot table", "loot tables"),
        (report.worldgen, "worldgen file", "worldgen files"),
        (c.textures, "texture", "textures"),
    ] {
        if n > 0 {
            check.ports.push(plural(n, one, many));
        }
    }
    if !report.blocks.is_empty() {
        check
            .ports
            .push("blocks take the closest vanilla block's shape, hardness and sounds".into());
    }
    let mut skipped = Vec::new();
    if !report.blocks_unknown.is_empty() {
        skipped.push(plural(
            report.blocks_unknown.len(),
            "block with no vanilla block to copy (left out)",
            "blocks with no vanilla block to copy (left out)",
        ));
    }
    if !report.recipes_unregistered.is_empty() {
        skipped.push(plural(
            report.recipes_unregistered.len(),
            "recipe for a left-out block",
            "recipes for left-out blocks",
        ));
    }
    if !report.recipes_kept.is_empty() {
        skipped.push(plural(
            report.recipes_kept.len(),
            "recipe of a type Horizon doesn't have",
            "recipes of types Horizon doesn't have",
        ));
    }
    if !report.unreadable.is_empty() {
        skipped.push(plural(
            report.unreadable.len(),
            "file the game can't read",
            "files the game can't read",
        ));
    }
    let wg = &report.worldgen_types;
    let dropped = wg.processors_dropped.values().sum::<usize>();
    if dropped > 0 {
        skipped.push(plural(
            dropped,
            "structure processor written in Java",
            "structure processors written in Java",
        ));
    }
    let unsupported = wg.unsupported.values().sum::<usize>();
    if unsupported > 0 {
        skipped.push(plural(
            unsupported,
            "worldgen file of a type written in Java",
            "worldgen files of types written in Java",
        ));
    }
    for (f, n) in &check.code.features {
        let mut line = f.describe(*n);
        if *f == classes::Feature::Events {
            let names: Vec<&str> = check
                .code
                .events
                .iter()
                .take(3)
                .map(String::as_str)
                .collect();
            line.push_str(&format!(" ({}", names.join(", ")));
            if check.code.events.len() > 3 {
                line.push_str(", ...");
            }
            line.push(')');
        }
        skipped.push(line);
    }
    check.skipped = skipped;
    let behaviour = check.code.behaviour();
    (check.verdict, check.summary) = if content == 0 {
        let what = if behaviour || check.code.classes > 0 {
            "everything it does is Java code, which Horizon doesn't run"
        } else {
            "it has no blocks, items, recipes or worldgen"
        };
        (Verdict::No, format!("Nothing to port: {what}."))
    } else if check.skipped.is_empty() {
        (
            Verdict::Works,
            format!(
                "Content only: {} port.",
                check.ports[..check.ports.len().min(3)].join(", ")
            ),
        )
    } else {
        (
            Verdict::Partly,
            format!(
                "{} port; not {}.",
                check.ports[..check.ports.len().min(2)].join(", "),
                check.skipped[..check.skipped.len().min(2)].join(", ")
            ),
        )
    };
    done(check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_native_java::Host;
    use std::io::Write;

    fn jar(dir: &Path, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = dir.join("m.jar");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    const TOML: &[u8] = br#"modLoader = "javafml"
license = "MIT"
[[mods]]
modId = "ruby"
version = "1.0"
displayName = "Ruby"
"#;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("port-pack-check-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn content_only_works_and_code_makes_it_partly() {
        let dir = temp("content");
        let blockstate = br#"{"variants": {"": {"model": "ruby:block/ruby_block"}}}"#;
        let recipe = br#"{"type": "minecraft:crafting_shapeless", "ingredients": ["minecraft:red_dye"], "result": {"id": "ruby:ruby"}}"#;
        let plain = [
            ("META-INF/neoforge.mods.toml", TOML),
            ("assets/ruby/blockstates/ruby_block.json", blockstate),
            ("assets/ruby/models/item/ruby.json", b"{}"),
            ("data/ruby/recipe/ruby.json", recipe),
        ];
        let c = check(&jar(&dir, &plain), &dir.join("out")).unwrap();
        assert_eq!(c.verdict, Verdict::Works, "{c:?}");
        assert_eq!(c.loader, "neoforge");
        assert!(
            c.native
                .as_ref()
                .is_some_and(|probe| probe.entrypoint.is_none())
        );
        assert_eq!(&c.ports[..3], ["1 block", "1 item", "1 recipe"]);
        assert!(dir.join("out/carry/horizon-mod.toml").is_file());
        assert!(dir.join("out/check.json").is_file());

        let tile = crate::classes::tests_support::class_file(
            "ruby/Tile",
            "net/minecraft/world/level/block/entity/BlockEntity",
        );
        let mut coded = plain.to_vec();
        coded.push(("ruby/Tile.class", &tile));
        let c = check(&jar(&dir, &coded), &dir.join("out")).unwrap();
        assert_eq!(c.verdict, Verdict::Partly);
        assert!(
            c.skipped[0].starts_with("1 block entity"),
            "{:?}",
            c.skipped
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn native_probe_selects_client_when_main_is_absent() {
        let dir = temp("native-client");
        let metadata = br#"{"schemaVersion":1,"id":"ruby","version":"1.0","entrypoints":{"client":"ruby.Client"}}"#;
        let class = crate::classes::tests_support::class_file("ruby/Client", "java/lang/Object");
        let path = jar(
            &dir,
            &[("fabric.mod.json", metadata), ("ruby/Client.class", &class)],
        );
        let probe = native_probe(&path);
        assert_eq!(probe.kind.as_deref(), Some("client"));
        assert_eq!(probe.entrypoint.as_deref(), Some("ruby.Client"));
        assert_eq!(probe.entrypoints, vec!["ruby.Client"]);
        assert_eq!(probe.seeded_mods, vec!["ruby"]);
        assert!(
            probe.error.is_some(),
            "the empty fixture has no lifecycle bytecode"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_probe_reports_all_declared_entrypoint_failures() {
        let dir = temp("native-entrypoint-errors");
        let metadata = br#"{"schemaVersion":1,"id":"ruby","version":"1.0","entrypoints":{"main":["ruby.One","ruby.Two"]}}"#;
        let one = crate::classes::tests_support::class_file("ruby/One", "java/lang/Object");
        let two = crate::classes::tests_support::class_file("ruby/Two", "java/lang/Object");
        let path = jar(
            &dir,
            &[
                ("fabric.mod.json", metadata),
                ("ruby/One.class", &one),
                ("ruby/Two.class", &two),
            ],
        );
        let probe = native_probe(&path);
        assert_eq!(probe.entrypoints, ["ruby.One", "ruby.Two"]);
        let error = probe.error.expect("both empty entrypoints should fail");
        assert!(error.contains("ruby.One"), "{error}");
        assert!(error.contains("ruby.Two"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_host_seeds_mod_and_required_dependency_from_metadata() {
        let dir = temp("native-host");
        let metadata = br#"{"schemaVersion":1,"id":"ruby","version":"1.0","depends":{"architectury-api":"*","minecraft":"~26.3"}}"#;
        let path = jar(&dir, &[("fabric.mod.json", metadata)]);
        let mut host = seeded_native_host(&path);
        let loaded = horizon_native_java::MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isModLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        let loader = horizon_native_java::Value::Ref(Some("fabric-loader".into()));
        assert_eq!(
            host.invoke_instance(
                &loaded,
                &loader,
                &[horizon_native_java::Value::Ref(Some("ruby".into()))],
            )
            .unwrap(),
            horizon_native_java::Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(
                &loaded,
                &loader,
                &[horizon_native_java::Value::Ref(Some(
                    "architectury-api".into()
                ))],
            )
            .unwrap(),
            horizon_native_java::Value::Int(1)
        );
        assert_eq!(
            host.invoke_instance(
                &loaded,
                &loader,
                &[horizon_native_java::Value::Ref(Some("minecraft".into()))],
            )
            .unwrap(),
            horizon_native_java::Value::Int(0)
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recipes_for_left_out_blocks_are_left_out_too() {
        let dir = temp("left-out");
        let odd = br#"{"variants": {"wobble=1": {"model": "ruby:block/odd"}}}"#;
        let plain = br#"{"variants": {"": {"model": "ruby:block/ruby_block"}}}"#;
        let recipe = |result: &str| {
            format!(
                r#"{{"type": "minecraft:crafting_shapeless", "ingredients": ["minecraft:red_dye"], "result": {{"id": "{result}"}}}}"#
            )
        };
        let (odd_recipe, block_recipe) = (recipe("ruby:odd"), recipe("ruby:ruby_block"));
        let files = [
            ("META-INF/neoforge.mods.toml", TOML),
            ("assets/ruby/blockstates/odd.json", odd.as_slice()),
            ("assets/ruby/blockstates/ruby_block.json", plain.as_slice()),
            ("data/ruby/recipe/odd.json", odd_recipe.as_bytes()),
            ("data/ruby/recipe/ruby_block.json", block_recipe.as_bytes()),
        ];
        let c = check(&jar(&dir, &files), &dir.join("out")).unwrap();
        assert_eq!(c.verdict, Verdict::Partly);
        assert!(c.ports.contains(&"1 recipe".to_string()), "{:?}", c.ports);
        assert!(
            c.skipped
                .contains(&"1 recipe for a left-out block".to_string()),
            "{:?}",
            c.skipped
        );
        assert!(!dir.join("out/carry/data/ruby/recipe/odd.json").exists());
        assert!(dir.join("out/outside/data/ruby/recipe/odd.json").exists());
        assert!(
            dir.join("out/carry/data/ruby/recipe/ruby_block.json")
                .exists()
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn code_only_and_old_layouts_dont_run() {
        let dir = temp("code");
        let tile = crate::classes::tests_support::class_file("ruby/Mixin", "java/lang/Object");
        let c = check(
            &jar(
                &dir,
                &[
                    ("META-INF/neoforge.mods.toml", TOML),
                    ("ruby/A.class", &tile),
                ],
            ),
            &dir.join("out"),
        )
        .unwrap();
        assert_eq!(c.verdict, Verdict::No);
        assert!(c.summary.starts_with("Nothing to port"), "{}", c.summary);
        let c = check(
            &jar(
                &dir,
                &[
                    ("META-INF/mods.toml", TOML),
                    ("data/ruby/recipes/ruby.json", b"{}"),
                ],
            ),
            &dir.join("out"),
        )
        .unwrap();
        assert_eq!(c.verdict, Verdict::No);
        assert!(c.summary.contains("1.20 or older"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The report is what it was; the record beside it says which jar and
    /// which versions answered for it.
    #[test]
    fn a_check_records_its_provenance_beside_the_report() {
        let dir = temp("provenance");
        let path = jar(
            &dir,
            &[
                ("META-INF/neoforge.mods.toml", TOML),
                (
                    "assets/ruby/blockstates/ruby_block.json",
                    br#"{"variants": {"": {"model": "ruby:block/ruby_block"}}}"#,
                ),
            ],
        );
        let c = check(&path, &dir.join("out")).unwrap();
        assert_eq!(c.verdict, Verdict::Works, "{c:?}");

        // Exactly the record the crate computes from the jar on disk.
        let text = fs::read_to_string(dir.join("out").join(PROVENANCE)).unwrap();
        let record: Provenance = serde_json::from_str(&text).unwrap();
        assert_eq!(record, provenance(&path).unwrap());
        assert_eq!(record.report, "check");
        assert_eq!(record.output, "check.json");
        // Not inside `check.json`: the report's fields are unchanged.
        let report = fs::read_to_string(dir.join("out/check.json")).unwrap();
        assert!(!report.contains("provenance"), "{report}");
        assert!(!report.contains("\"key\""), "{report}");

        assert_eq!(record.format.abi.game, horizon_core::version::VERSION_ID);
        assert_eq!(record.format.abi.horizon_mod_abi, api_level::LEVEL);
        assert_eq!(
            record.format.ir_version,
            horizon_mods::java_artifact::IR_VERSION
        );
        assert_eq!(
            record.format.abi.native_hooks, None,
            "a check weaves nothing"
        );
        assert_eq!(record.inputs.len(), 1, "{:?}", record.inputs);
        assert_eq!(record.inputs[0].kind, "jar");
        assert_eq!(record.inputs[0].sha256, sha256_of(&path));
        // Nothing to reuse against a jar that is not this one.
        assert!(
            record
                .check(record.format.clone(), &record.inputs)
                .is_fresh()
        );
        let mut abi = record.format.abi.clone();
        abi.horizon_mod_abi += 1;
        assert_eq!(
            record
                .check(Format::new(abi), &record.inputs)
                .reasons()
                .len(),
            1
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The record is written for a report that ports nothing too, it holds the
    /// jar's own bytes, and it is stale for any other jar.
    #[test]
    fn a_refused_jar_still_gets_the_provenance_of_the_answer() {
        let dir = temp("provenance-no");
        let tile = crate::classes::tests_support::class_file("ruby/A", "java/lang/Object");
        let path = jar(
            &dir,
            &[
                ("META-INF/neoforge.mods.toml", TOML),
                ("ruby/A.class", &tile),
            ],
        );
        let c = check(&path, &dir.join("out")).unwrap();
        assert_eq!(c.verdict, Verdict::No);
        let record: Provenance =
            serde_json::from_str(&fs::read_to_string(dir.join("out").join(PROVENANCE)).unwrap())
                .unwrap();
        assert_eq!(record.inputs[0].sha256, sha256_of(&path));

        // Another jar — a byte different, and a jar name of its own — does not
        // reuse this answer.
        let other = dir.join("n.jar");
        let mut bytes = fs::read(&path).unwrap();
        bytes.push(0);
        fs::write(&other, bytes).unwrap();
        let later = provenance(&other).unwrap();
        let status = record.check(later.format.clone(), &later.inputs);
        assert!(!status.is_fresh(), "{status:?}");
        assert_eq!(
            status.reasons(),
            [
                format!("jar {} left", path.display()),
                format!("jar {} appeared", other.display()),
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The key's input is the jar as bytes: a check of the same jar twice is
    /// one artifact, so a re-check does not invent a new one.
    #[test]
    fn the_same_jar_is_the_same_artifact() {
        let dir = temp("provenance-twice");
        let path = jar(&dir, &[("META-INF/neoforge.mods.toml", TOML)]);
        assert_eq!(
            provenance(&path).unwrap().key,
            provenance(&path).unwrap().key
        );
        assert!(provenance(&dir.join("missing.jar")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn sha256_of(path: &Path) -> String {
        horizon_mods::java_artifact::sha256_hex(&fs::read(path).unwrap())
    }
}
