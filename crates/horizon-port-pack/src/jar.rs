//! What a mod jar holds, read without decompiling: its metadata
//! (`neoforge.mods.toml`, `mods.toml`, `fabric.mod.json`), the jars nested in
//! it, and a census of its data, assets and classes.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek},
    path::Path,
};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct JarInfo {
    pub mod_id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    /// Required dependencies by mod id (loaders and `minecraft` left out).
    pub depends: Vec<String>,
    /// Loader entrypoints declared by the jar. Fabric permits one or more
    /// classes per entrypoint key; keeping the key lets the JVM runner choose
    /// the right adapter (`main`, `client`, `server`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoints: Vec<Entrypoint>,
    /// Fabric's optional access-widener resource, relative to the jar root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_widener: Option<String>,
    /// Jars nested under the loader's JiJ folders (`META-INF/jars/`,
    /// `META-INF/jarjar/`) and `META-INF/versions/`, identity only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested_jars: Vec<NestedJar>,
    /// `(entry, reason)` for nested entries whose own metadata was malformed
    /// or unreadable. The entry is still reported, with a `None` identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nested_jar_errors: Vec<(String, String)>,
    pub census: Census,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entrypoint {
    pub kind: String,
    pub class: String,
    /// Optional Fabric adapter retained so the launcher can explain why a
    /// bounded native invocation cannot execute this entrypoint yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Census {
    pub blockstates: usize,
    pub item_models: usize,
    pub recipes: usize,
    pub loot_tables: usize,
    pub tags: usize,
    pub worldgen: usize,
    pub structures: usize,
    pub languages: usize,
    pub textures: usize,
    pub sounds: usize,
    pub classes: usize,
    pub mixin_configs: usize,
    /// Mixin configuration paths found in the jar. Kept alongside the count
    /// so reports can identify the loader inputs without opening or executing
    /// configuration files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixin_config_names: Vec<String>,
    /// Typed Horizon registration kinds declared in `META-INF/horizon-native.json`.
    /// Values are metadata only; the native adapter validates declarations later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_declaration_kinds: Vec<String>,
}

/// A jar nested in another jar's loader-specific JiJ folder. Only the identity
/// the nested jar declares for itself is kept; its classes, data and assets
/// stay in the archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestedJar {
    /// Entry path in the outer jar, as the loader sees it.
    pub entry: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Which loader's nesting folder the entry came from.
    #[serde(default)]
    pub kind: NestedJarKind,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NestedJarKind {
    #[serde(rename = "fabric")]
    Fabric,
    #[serde(rename = "neoforge")]
    NeoForge,
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
}

/// Nested-jar reads are bounded: a bundler can nest a whole loader, and the
/// census only needs identities. Entries are taken in path order.
const MAX_NESTED_JARS: usize = 64;
const MAX_NESTED_BYTES: u64 = 32 * 1024 * 1024;
/// Loader metadata inside a nested jar is small; stop at this size.
const MAX_NESTED_METADATA: u64 = 64 * 1024;
/// One reported reason stays one bounded line.
const MAX_NESTED_ERROR: usize = 160;

const NOT_MODS: [&str; 5] = ["minecraft", "neoforge", "forge", "fabricloader", "java"];

pub fn read(path: &Path) -> Result<JarInfo, String> {
    let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = |zip: &mut zip::ZipArchive<fs::File>, name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };
    let mut info = if let Some(t) = text(&mut zip, "META-INF/neoforge.mods.toml")
        .or_else(|| text(&mut zip, "META-INF/mods.toml"))
    {
        mods_toml(&t)?
    } else if let Some(t) = text(&mut zip, "fabric.mod.json") {
        fabric(&t)?
    } else {
        JarInfo::default()
    };
    // `version = "${file.jarVersion}"` is filled in from the manifest at load.
    if info.version.starts_with("${") {
        info.version = text(&mut zip, "META-INF/MANIFEST.MF")
            .and_then(|m| manifest_value(&m, "Implementation-Version"))
            .unwrap_or_default();
    }
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    info.census = census(&names);
    if let Some(metadata) = text(&mut zip, "META-INF/horizon-native.json") {
        info.census.native_declaration_kinds = native_declaration_kinds(&metadata);
    }
    let (nested, nested_errors) = nested_jars_from(&mut zip, MAX_NESTED_JARS, MAX_NESTED_BYTES);
    info.nested_jars = nested;
    info.nested_jar_errors = nested_errors;
    if info.mod_id.is_empty() {
        info.mod_id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    Ok(info)
}

/// The jars nested inside `path`, in archive path order, with the identity
/// each declares and the reasons any metadata could not be read. Discovery
/// looks at Fabric's `META-INF/jars/`, NeoForge's `META-INF/jarjar/`, and
/// multi-release `META-INF/versions/<n>/` paths. Nested jars are opened from
/// the outer archive (no temporary files) under a 64-entry, 32 MiB cap, so a
/// bundler cannot turn metadata discovery into an unbounded archive walk.
pub fn nested_jars(path: &Path) -> (Vec<NestedJar>, Vec<(String, String)>) {
    let mut zip = match fs::File::open(path)
        .map_err(|e| e.to_string())
        .and_then(|file| zip::ZipArchive::new(file).map_err(|e| e.to_string()))
    {
        Ok(zip) => zip,
        Err(e) => {
            return (
                Vec::new(),
                vec![(
                    String::new(),
                    bounded_reason(format!("{}: {e}", path.display())),
                )],
            );
        }
    };
    nested_jars_from(&mut zip, MAX_NESTED_JARS, MAX_NESTED_BYTES)
}

fn nested_jars_from<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    max_entries: usize,
    max_bytes: u64,
) -> (Vec<NestedJar>, Vec<(String, String)>) {
    let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
    let mut jars = Vec::new();
    let mut errors = Vec::new();
    let mut budget = max_bytes;
    for (entry, kind) in nested_entries(&names).into_iter().take(max_entries) {
        let Some(index) = zip.index_for_name(&entry) else {
            errors.push((entry, "nested jar is missing from the archive".to_owned()));
            continue;
        };
        let Ok(file) = zip.by_index(index) else {
            errors.push((entry, "nested jar is unreadable".to_owned()));
            continue;
        };
        if file.size() > budget {
            errors.push((entry, nested_budget_reason(max_bytes)));
            continue;
        }
        // Do not trust the declared size alone: read one byte past the
        // remaining budget and reject the entry if it grew.
        let mut bytes = Vec::new();
        if file.take(budget + 1).read_to_end(&mut bytes).is_err() {
            errors.push((entry, "nested jar is unreadable".to_owned()));
            continue;
        }
        if bytes.len() as u64 > budget {
            errors.push((entry, nested_budget_reason(max_bytes)));
            continue;
        }
        budget -= bytes.len() as u64;
        let (id, version, error) = nested_identity(&bytes);
        jars.push(NestedJar {
            entry: entry.clone(),
            id,
            version,
            kind,
        });
        if let Some(reason) = error {
            errors.push((entry, bounded_reason(reason)));
        }
    }
    (jars, errors)
}

fn nested_budget_reason(max_bytes: u64) -> String {
    bounded_reason(format!("nested jar exceeds the {max_bytes}-byte budget"))
}

/// Nested entries in stable path order.
///
/// Fabric nests direct `META-INF/jars/<name>.jar` entries and NeoForge nests
/// `META-INF/jarjar/<name>.jar`; subfolders are not loader classpath entries.
/// A multi-release `META-INF/versions/<n>/` copy of one of those is skipped
/// because the base entry is the one the loader reads; any other `.jar` under
/// `META-INF/versions/<n>/` is kept as `Unknown` so an unusual layout is still
/// visible in the report.
fn nested_entries(names: &[String]) -> Vec<(String, NestedJarKind)> {
    let mut entries: Vec<(String, NestedJarKind)> = names
        .iter()
        .filter_map(|name| {
            if let Some(kind) = direct_nested_entry(name) {
                return Some((name.clone(), kind));
            }
            let inner = versioned_entry(name)?;
            // A versioned copy of a base nested entry is a duplicate; the base
            // is the one the loader reads. Other versioned jars stay visible.
            if direct_nested_entry(inner).is_some() || !is_nested_jar_file(inner) {
                return None;
            }
            Some((name.clone(), NestedJarKind::Unknown))
        })
        .collect();
    entries.sort();
    entries.dedup();
    entries
}

fn direct_nested_entry(name: &str) -> Option<NestedJarKind> {
    if let Some(rest) = name.strip_prefix("META-INF/jars/") {
        return is_nested_jar_file(rest).then_some(NestedJarKind::Fabric);
    }
    if let Some(rest) = name.strip_prefix("META-INF/jarjar/") {
        return is_nested_jar_file(rest).then_some(NestedJarKind::NeoForge);
    }
    None
}

fn is_nested_jar_file(rest: &str) -> bool {
    !rest.is_empty() && !rest.contains('/') && rest.ends_with(".jar")
}

/// The path inside `META-INF/versions/<n>/`, when the version is non-empty.
fn versioned_entry(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("META-INF/versions/")?;
    let (version, inner) = rest.split_once('/')?;
    (!version.is_empty()).then_some(inner)
}

/// Identity from a nested jar's own loader metadata, read from memory. A jar
/// without metadata is a plain bundled library: no identity, no error.
fn nested_identity(bytes: &[u8]) -> (Option<String>, Option<String>, Option<String>) {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return (
            None,
            None,
            Some("nested jar is not a readable zip".to_owned()),
        );
    };
    match entry_text(&mut archive, "fabric.mod.json") {
        Ok(Some(text)) => return fabric_identity(&text),
        Ok(None) => {}
        Err(e) => return (None, None, Some(e)),
    }
    for name in ["META-INF/neoforge.mods.toml", "META-INF/mods.toml"] {
        match entry_text(&mut archive, name) {
            Ok(Some(text)) => return mods_toml_identity(&text),
            Ok(None) => {}
            Err(e) => return (None, None, Some(e)),
        }
    }
    (None, None, None)
}

fn fabric_identity(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => return (None, None, Some(format!("fabric.mod.json: {e}"))),
    };
    (
        identity_field(&value, "id"),
        identity_field(&value, "version"),
        None,
    )
}

fn mods_toml_identity(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let doc: toml::Table = match toml::from_str(text) {
        Ok(doc) => doc,
        Err(e) => return (None, None, Some(format!("mods.toml: {e}"))),
    };
    let first = doc
        .get("mods")
        .and_then(|m| m.as_array())
        .and_then(|m| m.first());
    let field = |key: &str| {
        first
            .and_then(|m| m.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    };
    (field("modId"), field("version"), None)
}

fn identity_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// One nested entry's metadata file, bounded.
fn entry_text<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, String> {
    let Some(index) = archive.index_for_name(name) else {
        return Ok(None);
    };
    let file = archive
        .by_index(index)
        .map_err(|e| format!("{name}: {e}"))?;
    if file.size() > MAX_NESTED_METADATA {
        return Err(format!("{name} exceeds {MAX_NESTED_METADATA} bytes"));
    }
    let mut text = String::new();
    file.take(MAX_NESTED_METADATA + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{name}: {e}"))?;
    if text.len() as u64 > MAX_NESTED_METADATA {
        return Err(format!("{name} exceeds {MAX_NESTED_METADATA} bytes"));
    }
    Ok(Some(text))
}

fn bounded_reason(reason: String) -> String {
    reason
        .replace(['\r', '\n'], " ")
        .chars()
        .take(MAX_NESTED_ERROR)
        .collect()
}

/// A main-section attribute of `META-INF/MANIFEST.MF`.
fn manifest_value(manifest: &str, key: &str) -> Option<String> {
    manifest
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// `[[mods]]` (the first one is the jar's mod), the top-level `license`, and
/// `[[dependencies.<id>]]` with `type = "required"` (or the older
/// `mandatory = true`).
pub fn mods_toml(text: &str) -> Result<JarInfo, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| format!("mods.toml: {e}"))?;
    let first = doc
        .get("mods")
        .and_then(|m| m.as_array())
        .and_then(|m| m.first())
        .and_then(|m| m.as_table());
    let get = |t: Option<&toml::Table>, k: &str| {
        t.and_then(|t| t.get(k))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let mod_id = get(first, "modId");
    let mut depends = Vec::new();
    if let Some(deps) = doc
        .get("dependencies")
        .and_then(|d| d.get(&mod_id))
        .and_then(|d| d.as_array())
    {
        for d in deps.iter().filter_map(|d| d.as_table()) {
            let required = d.get("type").and_then(|t| t.as_str()).map_or_else(
                || d.get("mandatory").and_then(|m| m.as_bool()) == Some(true),
                |t| t.eq_ignore_ascii_case("required"),
            );
            let id = get(Some(d), "modId");
            if required && !NOT_MODS.contains(&id.as_str()) && !id.is_empty() {
                depends.push(id);
            }
        }
    }
    Ok(JarInfo {
        name: get(first, "displayName"),
        version: get(first, "version"),
        license: get(Some(&doc), "license"),
        mod_id,
        depends,
        entrypoints: Vec::new(),
        access_widener: None,
        nested_jars: Vec::new(),
        nested_jar_errors: Vec::new(),
        census: Census::default(),
    })
}

pub fn fabric(text: &str) -> Result<JarInfo, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("fabric.mod.json: {e}"))?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let license = match &v["license"] {
        serde_json::Value::Array(l) => l
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" OR "),
        l => l.as_str().unwrap_or("").to_owned(),
    };
    let depends = v["depends"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, _)| id.clone())
        .filter(|id| !NOT_MODS.contains(&id.as_str()) && !id.starts_with("fabric"))
        .collect();
    let mut entrypoints = Vec::new();
    if let Some(groups) = v.get("entrypoints").and_then(|e| e.as_object()) {
        for (kind, values) in groups {
            let values = match values {
                serde_json::Value::String(class) => vec![(class.clone(), None)],
                serde_json::Value::Object(object) => object
                    .get("value")
                    .and_then(|value| value.as_str())
                    .map(|class| {
                        (
                            class.to_owned(),
                            object
                                .get("adapter")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned),
                        )
                    })
                    .into_iter()
                    .collect(),
                serde_json::Value::Array(values) => values
                    .iter()
                    .filter_map(|value| match value {
                        serde_json::Value::String(class) => Some((class.to_owned(), None)),
                        serde_json::Value::Object(object) => object
                            .get("value")
                            .and_then(|value| value.as_str())
                            .map(|class| {
                                (
                                    class.to_owned(),
                                    object
                                        .get("adapter")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_owned),
                                )
                            }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for (class, adapter) in values {
                if !class.is_empty() {
                    entrypoints.push(Entrypoint {
                        kind: kind.clone(),
                        class,
                        adapter,
                    });
                }
            }
        }
    }
    entrypoints.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.class.cmp(&b.class)));
    let access_widener = v
        .get("accessWidener")
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .map(str::to_owned);
    Ok(JarInfo {
        mod_id: s("id"),
        name: s("name"),
        version: s("version"),
        license,
        depends,
        entrypoints,
        access_widener,
        nested_jars: Vec::new(),
        nested_jar_errors: Vec::new(),
        census: Census::default(),
    })
}

pub fn census(names: &[String]) -> Census {
    let mut c = Census {
        mixin_config_names: mixin_config_names(names),
        ..Census::default()
    };
    for n in names {
        let parts: Vec<&str> = n.split('/').collect();
        if n.ends_with(".class") {
            c.classes += 1;
        } else if is_mixin_config(n, parts.len()) {
            c.mixin_configs += 1;
        }
        // assets/<ns>/<kind>/... and data/<ns>/<kind>/...
        let (Some(root), Some(kind)) = (parts.first(), parts.get(2)) else {
            continue;
        };
        let json = n.ends_with(".json");
        match (*root, *kind) {
            ("assets", "blockstates") if json => c.blockstates += 1,
            ("assets", "models") if json && parts.get(3) == Some(&"item") => c.item_models += 1,
            ("assets", "lang") => c.languages += 1,
            ("assets", "textures") if n.ends_with(".png") => c.textures += 1,
            ("assets", "sounds.json") => c.sounds += 1,
            ("data", "recipe" | "recipes") if json => c.recipes += 1,
            ("data", "loot_table" | "loot_tables") if json => c.loot_tables += 1,
            ("data", "tags") if json => c.tags += 1,
            ("data", "worldgen") if json => c.worldgen += 1,
            ("data", "structure" | "structures") if n.ends_with(".nbt") => c.structures += 1,
            _ => {}
        }
    }
    c
}

/// Extract the stable set of typed Horizon registration kinds from the
/// explicit native metadata file. Unknown or malformed metadata remains
/// report-empty; validation belongs to `horizon_native_java`/`horizon-mods`.
pub fn native_declaration_kinds(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let mut kinds = BTreeSet::new();
    if let Some(entries) = value.get("registrations").and_then(|v| v.as_array()) {
        for kind in entries
            .iter()
            .filter_map(|entry| entry.get("kind"))
            .filter_map(|kind| kind.as_str())
        {
            kinds.insert(kind.to_owned());
        }
    }
    kinds.into_iter().collect()
}

/// Returns the mixin configuration paths in stable jar order. This only
/// inspects entry names; config JSON is deliberately left opaque because the
/// loader-specific schema is not needed by the census.
pub fn mixin_config_names(names: &[String]) -> Vec<String> {
    let mut configs: Vec<String> = names
        .iter()
        .filter(|name| is_mixin_config(name, name.split('/').count()))
        .cloned()
        .collect();
    configs.sort();
    configs
}

fn is_mixin_config(name: &str, path_parts: usize) -> bool {
    name.ends_with(".mixins.json") || (path_parts == 1 && name.contains("mixins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_neoforge_metadata() {
        let info = mods_toml(
            r#"modLoader = "javafml"
license = "MIT"
[[mods]]
modId = "ruby"
version = "1.2.0"
displayName = "Ruby"
[[dependencies.ruby]]
modId = "neoforge"
type = "required"
[[dependencies.ruby]]
modId = "geckolib"
type = "required"
[[dependencies.ruby]]
modId = "jei"
type = "optional"
[[dependencies.ruby]]
modId = "curios"
mandatory = true
"#,
        )
        .unwrap();
        assert_eq!(
            (
                info.mod_id.as_str(),
                info.version.as_str(),
                info.license.as_str()
            ),
            ("ruby", "1.2.0", "MIT")
        );
        assert_eq!(info.depends, ["geckolib", "curios"]);
    }

    #[test]
    fn reads_fabric_metadata() {
        let info = fabric(
            r#"{"id": "gem", "version": "3", "name": "Gem", "license": ["MIT", "Apache-2.0"],
                "depends": {"fabricloader": "*", "fabric-api": "*", "minecraft": "1.21.1", "cloth-config": "*"},
                "entrypoints": {"main": "com.example.Gem", "client": ["com.example.GemClient", ""], "server": {"value": "com.example.GemServer", "adapter": "default"}},
                "accessWidener": "gem.accesswidener"}"#,
        )
        .unwrap();
        assert_eq!(info.license, "MIT OR Apache-2.0");
        assert_eq!(info.depends, ["cloth-config"]);
        assert_eq!(info.access_widener.as_deref(), Some("gem.accesswidener"));
        assert_eq!(
            info.entrypoints,
            [
                Entrypoint {
                    kind: "client".into(),
                    class: "com.example.GemClient".into(),
                    adapter: None
                },
                Entrypoint {
                    kind: "main".into(),
                    class: "com.example.Gem".into(),
                    adapter: None
                },
                Entrypoint {
                    kind: "server".into(),
                    class: "com.example.GemServer".into(),
                    adapter: Some("default".into())
                },
            ]
        );
    }

    #[test]
    fn counts_files_by_kind() {
        let names = [
            "assets/ruby/blockstates/ruby_block.json",
            "assets/ruby/models/item/ruby.json",
            "assets/ruby/models/block/ruby_block.json",
            "assets/ruby/textures/item/ruby.png",
            "assets/ruby/lang/en_us.json",
            "data/ruby/recipe/ruby.json",
            "data/ruby/loot_table/blocks/ruby_block.json",
            "data/minecraft/tags/block/mineable/pickaxe.json",
            "data/ruby/structure/hut.nbt",
            "com/example/Ruby.class",
            "ruby.mixins.json",
        ]
        .map(String::from);
        let c = census(&names);
        assert_eq!(
            (c.blockstates, c.item_models, c.textures, c.languages),
            (1, 1, 1, 1)
        );
        assert_eq!(
            (c.recipes, c.loot_tables, c.tags, c.structures),
            (1, 1, 1, 1)
        );
        assert_eq!((c.classes, c.mixin_configs), (1, 1));
        assert_eq!(c.mixin_config_names, ["ruby.mixins.json"]);
    }

    #[test]
    fn reports_typed_native_declaration_kinds_stably() {
        assert_eq!(
            native_declaration_kinds(
                r#"{"registrations":[{"kind":"item"},{"kind":"block"},{"kind":"item"},{"kind":3}]}"#
            ),
            ["block", "item"]
        );
        assert!(native_declaration_kinds("not json").is_empty());
    }

    #[test]
    fn mixin_config_names_are_sorted_and_ignore_unrelated_paths() {
        let names = [
            "z.mixins.json".to_owned(),
            "META-INF/mixins.json".to_owned(),
            "a.mixins.json".to_owned(),
            "assets/example/mixins.json".to_owned(),
            "readme.txt".to_owned(),
        ];
        assert_eq!(
            mixin_config_names(&names),
            ["a.mixins.json", "z.mixins.json"]
        );
    }

    fn zip_files<W: std::io::Write + Seek>(out: W, files: &[(&str, &[u8])]) -> W {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(out);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in files {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap()
    }

    fn nested(files: &[(&str, &[u8])]) -> Vec<u8> {
        zip_files(std::io::Cursor::new(Vec::new()), files).into_inner()
    }

    fn bundle(path: &Path, entries: &[(&str, &[u8])]) {
        zip_files(fs::File::create(path).unwrap(), entries);
    }

    #[test]
    fn nested_jars_report_identity_kind_and_bounded_errors() {
        let dir = std::env::temp_dir().join("horizon-jar-nested-identity");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bundle.jar");
        let fabric = nested(&[
            (
                "fabric.mod.json",
                br#"{"id":"fabric-nested","version":"1.0"}"#,
            ),
            ("example/Fabric.class", b"cafebabe"),
        ]);
        let neo = nested(&[(
            "META-INF/neoforge.mods.toml",
            b"[[mods]]\nmodId = \"neo-nested\"\nversion = \"2.0\"\n",
        )]);
        let plain = nested(&[("example/Plain.class", b"cafebabe")]);
        let broken = nested(&[("fabric.mod.json", b"{not json")]);
        let versioned = nested(&[("example/Versioned.class", b"cafebabe")]);
        bundle(
            &path,
            &[
                ("META-INF/jarjar/neo.jar", neo.as_slice()),
                ("META-INF/jars/fabric.jar", fabric.as_slice()),
                ("META-INF/jars/plain.jar", plain.as_slice()),
                ("META-INF/jars/broken.jar", broken.as_slice()),
                // A multi-release copy of `META-INF/jars/fabric.jar`: the
                // base entry is the one the loader reads, so it is skipped.
                (
                    "META-INF/versions/9/META-INF/jars/fabric.jar",
                    fabric.as_slice(),
                ),
                // A versioned jar with no base entry is still reported.
                ("META-INF/versions/9/extra.jar", versioned.as_slice()),
            ],
        );
        let (jars, errors) = nested_jars(&path);
        let seen: Vec<(&str, Option<&str>, NestedJarKind)> = jars
            .iter()
            .map(|j| (j.entry.as_str(), j.id.as_deref(), j.kind))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "META-INF/jarjar/neo.jar",
                    Some("neo-nested"),
                    NestedJarKind::NeoForge
                ),
                ("META-INF/jars/broken.jar", None, NestedJarKind::Fabric),
                (
                    "META-INF/jars/fabric.jar",
                    Some("fabric-nested"),
                    NestedJarKind::Fabric
                ),
                ("META-INF/jars/plain.jar", None, NestedJarKind::Fabric),
                (
                    "META-INF/versions/9/extra.jar",
                    None,
                    NestedJarKind::Unknown
                ),
            ]
        );
        assert_eq!(jars[0].version.as_deref(), Some("2.0"));
        assert_eq!(jars[2].version.as_deref(), Some("1.0"));
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "META-INF/jars/broken.jar");
        assert!(
            errors[0].1.starts_with("fabric.mod.json:"),
            "{}",
            errors[0].1
        );
        assert!(errors[0].1.len() <= MAX_NESTED_ERROR);
        // `read` folds the same discovery into the per-jar metadata.
        let info = read(&path).unwrap();
        assert_eq!(info.nested_jars, jars);
        assert_eq!(info.nested_jar_errors, errors);
    }

    #[test]
    fn jar_without_nested_entries_reports_nothing() {
        let dir = std::env::temp_dir().join("horizon-jar-no-nested");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("flat.jar");
        bundle(
            &path,
            &[
                (
                    "fabric.mod.json",
                    br#"{"id":"flat","version":"1"}"#.as_slice(),
                ),
                ("example/Flat.class", b"cafebabe"),
            ],
        );
        let (jars, errors) = nested_jars(&path);
        assert!(jars.is_empty());
        assert!(errors.is_empty());
        let info = read(&path).unwrap();
        assert_eq!(info.mod_id, "flat");
        assert!(info.nested_jars.is_empty());
        assert!(info.nested_jar_errors.is_empty());
    }

    #[test]
    fn nested_discovery_is_bounded_by_entry_count_and_bytes() {
        assert_eq!((MAX_NESTED_JARS, MAX_NESTED_BYTES), (64, 32 * 1024 * 1024));
        let dir = std::env::temp_dir().join("horizon-jar-nested-caps");
        fs::create_dir_all(&dir).unwrap();
        let leaf = nested(&[("fabric.mod.json", br#"{"id":"leaf","version":"1"}"#)]);
        let names: Vec<String> = (0..(MAX_NESTED_JARS + 2))
            .map(|i| format!("META-INF/jars/jar{i:02}.jar"))
            .collect();
        let entries: Vec<(&str, &[u8])> = names
            .iter()
            .map(|name| (name.as_str(), leaf.as_slice()))
            .collect();
        let path = dir.join("many.jar");
        bundle(&path, &entries);
        let (jars, errors) = nested_jars(&path);
        assert_eq!(jars.len(), MAX_NESTED_JARS);
        assert!(errors.is_empty());
        assert_eq!(jars[0].entry, "META-INF/jars/jar00.jar");
        assert_eq!(jars[MAX_NESTED_JARS - 1].entry, "META-INF/jars/jar63.jar");

        // A small explicit budget exercises the byte cap without building a
        // 32 MiB fixture: the oversized entry is reported and skipped, and a
        // later entry that still fits is read. The payload is incompressible
        // so the nested jar itself exceeds the budget.
        let noise: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
            .collect();
        let big = nested(&[
            ("payload.bin", noise.as_slice()),
            ("fabric.mod.json", br#"{"id":"big"}"#),
        ]);
        let small = nested(&[("fabric.mod.json", br#"{"id":"small"}"#)]);
        let path = dir.join("budget.jar");
        bundle(
            &path,
            &[
                ("META-INF/jars/a-big.jar", big.as_slice()),
                ("META-INF/jars/b-small.jar", small.as_slice()),
            ],
        );
        let file = fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let (jars, errors) = nested_jars_from(&mut zip, MAX_NESTED_JARS, 1024);
        assert_eq!(
            jars.iter().map(|j| j.id.as_deref()).collect::<Vec<_>>(),
            [Some("small")]
        );
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "META-INF/jars/a-big.jar");
        assert!(errors[0].1.contains("budget"), "{}", errors[0].1);
    }
}
