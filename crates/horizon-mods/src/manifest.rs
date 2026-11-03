//! `horizon-mod.toml`, a mod's manifest, and the `.hmod` file around it.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    path::{Path, PathBuf},
};

/// The manifest's file name, beside `mod.wasm`.
pub const MANIFEST: &str = "horizon-mod.toml";
/// The component's file name in a `.hmod` or a mod folder.
pub const WASM: &str = "mod.wasm";
/// The API (WIT package `horizon:mods`) versions this build runs: a mod
/// built for any 0.1.x SDK.
pub const API: &str = "0.1";
/// The manifest `api` lines this build loads. 0.2 is 0.1 plus content:
/// `assets/` and `data/` in the `.hmod`, with blocks, items and mob effects declared in
/// `data/<id>/horizon/` (docs/modpacks.md). Code still links API 0.1.
/// 0.3 is a server mod (experimental): the `horizon-server-mod` world.
pub const APIS: [&str; 3] = [API, "0.2", SERVER_API];
/// The line of server mods, experimental: `side = "server"`.
pub const SERVER_API: &str = "0.3";
/// Namespaces no mod may claim: vanilla's, the convention tags' (mods add
/// to those, docs/mods.md) and the game's own.
const RESERVED_NAMESPACES: [&str; 3] = ["minecraft", "c", "horizon"];
/// A mod's data (capability `storage`), in `horizon-mods/` beside the mods
/// (`find` never mistakes it for a mod: it has no manifest).
pub const STORAGE_DIR: &str = ".horizon-mod-data";
/// The biggest `mod.wasm` a mod may ship.
const MAX_WASM: u64 = 32 << 20;
/// `fabric.mod.json`, at a Java mod jar's root.
const FABRIC: &str = "fabric.mod.json";
/// `META-INF/neoforge.mods.toml`: NeoForge's own metadata file, which it
/// reads instead of Forge's `META-INF/mods.toml`.
const NEOFORGE: &str = "META-INF/neoforge.mods.toml";
/// A mod id's length: the same bound a Horizon manifest keeps.
const MAX_ID: usize = 64;
/// A version or version range, as text: longer than any range a loader writes.
const MAX_VERSION: usize = 128;
/// A name, entrypoint, mixin config or nested jar path, as text.
const MAX_TEXT: usize = 256;
/// Rows in one list of Java loader metadata (dependencies, mods, entrypoints,
/// mixins, jars): the bound that keeps a hostile file from making the loader
/// hold more than it will ever use.
const MAX_LIST: usize = 256;
/// Characters of a string an error message repeats before `…`.
const EXCERPT: usize = 24;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `[a-z0-9_]`, 1 to 64 characters: the mod's folder, log and settings key.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub authors: Vec<String>,
    /// The API line the mod was built for: `0.1`, or `0.2` for content.
    pub api: String,
    #[serde(default)]
    pub side: Side,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    /// Namespaces besides `id` that the mod's `assets/` and `data/` use, for
    /// a content mod whose files sit under other names than its id (Towns
    /// and Towers ships `kaisyn` and `towns_and_towers`). No two content mods
    /// that are on may share one (`content::clash`).
    #[serde(default)]
    pub namespaces: Vec<String>,
    /// The experimental capabilities the mod opts into (`["ui"]`): each one
    /// it lists in `capabilities` must be named here too, since it may change
    /// in any release (docs/api/stability.md). One no longer experimental is
    /// accepted here, so the manifest keeps loading once it is promoted.
    #[serde(default)]
    pub experimental: Vec<Capability>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// Runs in the player's game only (HUD, keys, chat commands).
    #[default]
    Client,
    /// Runs where the world is simulated (rules, blocks): `api = "0.3"`.
    Server,
    /// Both, and the server tells clients which version it expects.
    Both,
}

/// What a mod may do beyond writing its log. Each is a WIT interface the
/// component imports; the player sees them before turning the mod on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    GameRead,
    Chat,
    Hud,
    Storage,
    /// Runs vanilla commands as the player, with the player's permissions.
    Command,
    /// New blocks, items and mob effects from the mod's `data/` (API 0.2): no WIT
    /// interface, the game reads them before the world loads.
    Registries,
    /// A screen the mod owns and a settings page in Options > Mods, drawn
    /// with the menus' own widgets (interface `ui`, calls `ui-events`).
    Ui,
    /// Shapes on the HUD and the camera's shake, drawn by the game's own
    /// renderer (interface `render`).
    Render,
    /// The use button on items the mod claims, and its own keys
    /// (interface `input`).
    Input,
    /// The players on the server and messages to them (server mods).
    Players,
    /// Blocks in the server's loaded chunks, and changes to them (server
    /// mods; client mods later, where the game runs the world).
    World,
    /// The recipes and items the game knows, for recipe viewers
    /// (interface `recipes`).
    Recipes,
}
impl Capability {
    pub const ALL: [Capability; 12] = [
        Capability::GameRead,
        Capability::Chat,
        Capability::Hud,
        Capability::Storage,
        Capability::Command,
        Capability::Registries,
        Capability::Ui,
        Capability::Render,
        Capability::Input,
        Capability::Players,
        Capability::World,
        Capability::Recipes,
    ];
    /// Whether a server mod may have it: the others act on a local player.
    pub fn on_server(self) -> bool {
        matches!(
            self,
            Capability::Storage | Capability::Players | Capability::World
        )
    }
    /// The WIT interface it grants, in `horizon:mods`.
    pub fn interface(self) -> &'static str {
        match self {
            Capability::GameRead => "game",
            Capability::Chat => "chat",
            Capability::Hud => "hud",
            Capability::Storage => "storage",
            Capability::Command => "command",
            Capability::Registries => "registries",
            Capability::Ui => "ui",
            Capability::Render => "render",
            Capability::Input => "input",
            Capability::Players => "players",
            Capability::World => "world",
            Capability::Recipes => "recipes",
        }
    }
    /// May change in any release: a mod uses it only with
    /// `experimental = ["<name>"]` in its manifest.
    pub fn is_experimental(self) -> bool {
        matches!(
            self,
            Capability::Ui | Capability::Render | Capability::Input | Capability::Recipes
        )
    }
    /// What the player reads before turning the mod on: "May <this>".
    pub fn describe(self) -> &'static str {
        match self {
            Capability::GameRead => "see your position, health and the time",
            Capability::Chat => "show you chat messages",
            Capability::Hud => "draw text on your screen",
            Capability::Storage => "save its own settings to disk",
            Capability::Command => "run commands as you, with your permissions",
            Capability::Registries => "add blocks, items and mob effects to your worlds",
            Capability::Ui => "show its own screens and a settings page",
            Capability::Render => "draw on your screen, shake and zoom the camera",
            Capability::Input => {
                "see the keys you hold and your clicks, take over the use button on its own items and the mouse wheel, and add keys"
            }
            Capability::Players => "see who is on the server and message them",
            Capability::World => "read and change the world's blocks",
            Capability::Recipes => {
                "read the game's recipes and items and which recipes you know, place recipes in your crafting grid and, in creative mode, give you items"
            }
        }
    }
    /// The capability a component import needs, `Ok(None)` for one every
    /// mod has; `Err` for an import Horizon does not offer.
    pub fn of_import(name: &str) -> Result<Option<Capability>, String> {
        let unknown = || format!("imports {name}, which Horizon does not offer");
        let rest = name.strip_prefix("horizon:mods/").ok_or_else(unknown)?;
        let (interface, version) = rest.split_once('@').ok_or_else(unknown)?;
        if !version.starts_with("0.1.") {
            return Err(format!("needs API {version}; this Horizon runs {API}"));
        }
        if interface == "log" {
            return Ok(None);
        }
        // A recipe viewer's panel is part of capability `ui`.
        if interface == "browser" {
            return Ok(Some(Capability::Ui));
        }
        Capability::ALL
            .into_iter()
            .filter(|c| *c != Capability::Registries)
            .find(|c| c.interface() == interface)
            .map(Some)
            .ok_or_else(unknown)
    }
}
impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Capability::GameRead => "game-read",
            Capability::Chat => "chat",
            Capability::Hud => "hud",
            Capability::Storage => "storage",
            Capability::Command => "command",
            Capability::Registries => "registries",
            Capability::Ui => "ui",
            Capability::Render => "render",
            Capability::Input => "input",
            Capability::Players => "players",
            Capability::World => "world",
            Capability::Recipes => "recipes",
        };
        f.write_str(name)
    }
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        let m: Manifest = toml::from_str(text).map_err(|e| format!("{MANIFEST}: {e}"))?;
        let id_ok = (1..=64).contains(&m.id.len())
            && m.id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !id_ok {
            return Err(format!(
                "{MANIFEST}: id {:?} is not [a-z0-9_]{{1,64}}",
                m.id
            ));
        }
        if !APIS.contains(&m.api.as_str()) {
            return Err(format!(
                "built for API {}; this Horizon runs {}",
                m.api,
                APIS.join(" and ")
            ));
        }
        if m.capabilities.contains(&Capability::Registries) && m.api == API {
            return Err(format!(
                "{MANIFEST}: capability registries needs api = \"0.2\""
            ));
        }
        let server = m.side == Side::Server;
        if server != (m.api == SERVER_API) {
            return Err(format!(
                "{MANIFEST}: a server mod is side = \"server\" with api = \"{SERVER_API}\", and only it"
            ));
        }
        if let Some(c) = m
            .capabilities
            .iter()
            .find(|c| c.on_server() != server && **c != Capability::Storage)
        {
            let side = if server { "client" } else { "server" };
            return Err(format!("{MANIFEST}: capability {c} is for {side} mods"));
        }
        if let Some(c) = m
            .capabilities
            .iter()
            .find(|c| c.is_experimental() && !m.experimental.contains(c))
        {
            return Err(format!(
                "{MANIFEST}: capability {c} is experimental and may change in any release: \
                 add experimental = [\"{c}\"] to use it"
            ));
        }
        if !m.namespaces.is_empty() && !m.capabilities.contains(&Capability::Registries) {
            return Err(format!(
                "{MANIFEST}: namespaces are for content, which needs capability registries"
            ));
        }
        for (i, ns) in m.namespaces.iter().enumerate() {
            // Vanilla's namespace characters, at most as long as an id.
            let chars_ok = (1..=64).contains(&ns.len())
                && ns
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
            if !chars_ok {
                return Err(format!(
                    "{MANIFEST}: namespace {ns:?} is not [a-z0-9_.-]{{1,64}}"
                ));
            }
            if RESERVED_NAMESPACES.contains(&ns.as_str()) {
                return Err(format!(
                    "{MANIFEST}: namespace {ns} is not a mod's to claim (only to add to its tags)"
                ));
            }
            if *ns == m.id || m.namespaces[..i].contains(ns) {
                return Err(format!("{MANIFEST}: namespace {ns} is listed twice"));
            }
        }
        Ok(m)
    }
    /// The namespaces the mod's content owns: its id, then `namespaces`.
    pub fn owned_namespaces(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.id.as_str()).chain(self.namespaces.iter().map(String::as_str))
    }
    /// Checks that the component's imports need only declared capabilities.
    pub fn check_imports<'a>(
        &self,
        imports: impl IntoIterator<Item = &'a str>,
    ) -> Result<(), String> {
        for name in imports {
            if let Some(c) = Capability::of_import(name)?
                && !self.capabilities.contains(&c)
            {
                return Err(format!("uses capability {c}, missing from {MANIFEST}"));
            }
        }
        Ok(())
    }
}

// ---- Java loader metadata -----------------------------------------------
// A Java mod jar says what it is and what it needs in `fabric.mod.json` or
// `META-INF/neoforge.mods.toml` (docs/java-mod-compat.md). The port and loader
// pipeline reads the identity, side, entrypoints, dependency declarations and
// nested jars from here; version ranges stay text, since only a loader knows
// how to compare them.

/// Which loader wrote a jar's metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JavaLoader {
    Fabric,
    NeoForge,
}

impl JavaLoader {
    pub fn name(self) -> &'static str {
        match self {
            JavaLoader::Fabric => "fabric",
            JavaLoader::NeoForge => "neoforge",
        }
    }
}

/// Which side of the connection a jar or one of its dependencies is for:
/// Fabric's `environment` (`"*"`, `"client"`, `"server"`) and NeoForge's
/// dependency `side` (`BOTH`, `CLIENT`, `SERVER`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DependencySide {
    #[default]
    Both,
    Client,
    Server,
}

/// Where a dependency's mod must load relative to this one, and Fabric's
/// nested jars (`Nested`: a jar in `jars`, which loads with its parent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DependencyOrdering {
    #[default]
    None,
    Before,
    After,
    Nested,
}

/// One dependency a Java mod declares: Fabric's `depends` and its four
/// siblings, or a NeoForge `[[dependencies.<modid>]]` table.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaDependency {
    /// `[a-z0-9_.-]{1,64}`: the other mod, as the loader knows it.
    pub id: String,
    /// The loader's version range (`">=1.20"`, `"[1.0,2.0)"`), uninterpreted:
    /// only the loader compares ranges.
    pub version: Option<String>,
    pub side: DependencySide,
    /// The loader refuses to start without it: Fabric `depends`, NeoForge
    /// `mandatory = true` or `type = "required"`.
    pub mandatory: bool,
    pub ordering: DependencyOrdering,
}

/// A jar inside a jar (`jars` in `fabric.mod.json`), which Fabric loads with
/// its parent.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaNestedJar {
    /// Its path in the jar, under `META-INF/jars/` by convention.
    pub file: String,
    /// The id and version the nested jar's own metadata declares, when the
    /// descriptor repeats them.
    pub id: Option<String>,
    pub version: Option<String>,
}

/// One Java mod as its loader metadata declares it: a `[[mods]]` table of a
/// NeoForge file, or the single mod of `fabric.mod.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaMod {
    pub id: String,
    pub version: Option<String>,
    pub name: Option<String>,
    /// `depends`: the ones the loader requires.
    pub depends: Vec<JavaDependency>,
    /// `recommends`: wanted, not required.
    pub recommends: Vec<JavaDependency>,
    /// `suggests`: mentioned only.
    pub suggests: Vec<JavaDependency>,
    /// `conflicts`: refused alongside.
    pub conflicts: Vec<JavaDependency>,
    /// `breaks`: refused at these versions.
    pub breaks: Vec<JavaDependency>,
}

impl JavaMod {
    /// Every dependency the mod declares, in the file's order: `depends`, then
    /// `recommends`, `suggests`, `conflicts` and `breaks`.
    pub fn dependencies(&self) -> impl Iterator<Item = &JavaDependency> {
        self.depends
            .iter()
            .chain(&self.recommends)
            .chain(&self.suggests)
            .chain(&self.conflicts)
            .chain(&self.breaks)
    }
}

/// What a Java mod jar's loader metadata declares: identity, side, entrypoints
/// and dependency declarations, the subset the port and loader pipeline needs.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaModMetadata {
    pub loader: JavaLoader,
    /// The file's mods in its order (always one for a Fabric jar).
    pub mods: Vec<JavaMod>,
    /// The jar's own side: Fabric's `environment`. NeoForge states side per
    /// dependency, so its jars are `Both` until a mod says otherwise.
    pub side: DependencySide,
    /// Entrypoint kind (`main`, `client`, `server`) to its classes, in the
    /// file's order.
    pub entrypoints: BTreeMap<String, Vec<String>>,
    /// The mixin configs to run, as paths in the jar.
    pub mixins: Vec<String>,
    /// Jars inside the jar, loaded with it (Fabric `jars`).
    pub jars: Vec<JavaNestedJar>,
    /// NeoForge's `modLoader`, `loaderVersion` and `license`, for the loader's
    /// log: `None` for a Fabric jar.
    pub mod_loader: Option<String>,
    pub loader_version: Option<String>,
    pub license: Option<String>,
}

impl JavaModMetadata {
    /// The id of the mod the file names first.
    pub fn id(&self) -> &str {
        self.mods.first().map_or("", |m| m.id.as_str())
    }
    /// Every dependency of every mod, in the file's order.
    pub fn dependencies(&self) -> impl Iterator<Item = &JavaDependency> {
        self.mods.iter().flat_map(JavaMod::dependencies)
    }
}

/// Reads the subset of `fabric.mod.json` the loader needs: `id`, `version`,
/// `name`, `environment`, `entrypoints`, the five dependency maps, `mixins`
/// and `jars`. Only `id` is required; a field of an unexpected shape is an
/// error naming it, so a jar is refused rather than half-read. Never panics.
pub fn parse_fabric_mod_json(bytes: &[u8]) -> Result<JavaModMetadata, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("{FABRIC}: {e}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{FABRIC}: the metadata is not one JSON object"))?;
    let field = |key: &str| object.get(key).and_then(serde_json::Value::as_str);
    let id = field("id").ok_or_else(|| format!("{FABRIC}: no id"))?;
    if !mod_id_ok(id) {
        return Err(format!(
            "{FABRIC}: id {:?} is not [a-z0-9_.-]{{1,{MAX_ID}}}",
            excerpt(id)
        ));
    }
    let version = field("version")
        .map(|text| bounded(text, MAX_VERSION, "version", FABRIC))
        .transpose()?;
    let name = field("name")
        .map(|text| bounded(text, MAX_TEXT, "name", FABRIC))
        .transpose()?;
    let side = match field("environment") {
        None | Some("*") => DependencySide::Both,
        Some("client") => DependencySide::Client,
        Some("server") => DependencySide::Server,
        Some(other) => {
            return Err(format!(
                "{FABRIC}: environment {:?} is not \"*\", \"client\" or \"server\"",
                excerpt(other)
            ));
        }
    };

    let mut entrypoints = BTreeMap::new();
    if let Some(value) = object.get("entrypoints") {
        let kinds = value
            .as_object()
            .ok_or_else(|| format!("{FABRIC}: entrypoints is not an object"))?;
        for (kind, list) in kinds {
            let classes = json_entries(list, kind)?
                .into_iter()
                .map(|entry| {
                    let class = json_named(entry, "value")
                        .ok_or_else(|| format!("{FABRIC}: entrypoint {kind} has no value"))?;
                    bounded(class, MAX_TEXT, "entrypoint", FABRIC)
                })
                .collect::<Result<Vec<_>, String>>()?;
            entrypoints.insert(kind.clone(), bounded_list(classes, "entrypoints", FABRIC)?);
        }
    }

    let mut mixins = Vec::new();
    if let Some(value) = object.get("mixins") {
        for entry in json_entries(value, "mixins")? {
            let config = json_named(entry, "config")
                .ok_or_else(|| format!("{FABRIC}: a mixins entry has no config"))?;
            mixins.push(bounded(config, MAX_TEXT, "mixin config", FABRIC)?);
        }
    }
    let mixins = bounded_list(mixins, "mixin configs", FABRIC)?;

    let mut jars = Vec::new();
    if let Some(value) = object.get("jars") {
        for entry in json_entries(value, "jars")? {
            let file = json_named(entry, "file")
                .ok_or_else(|| format!("{FABRIC}: a jars entry has no file"))?;
            let field = |key: &str| entry.get(key).and_then(serde_json::Value::as_str);
            jars.push(JavaNestedJar {
                file: bounded(file, MAX_TEXT, "nested jar path", FABRIC)?,
                id: field("id")
                    .map(|text| bounded(text, MAX_ID, "nested jar id", FABRIC))
                    .transpose()?,
                version: field("version")
                    .map(|text| bounded(text, MAX_VERSION, "nested jar version", FABRIC))
                    .transpose()?,
            });
        }
    }
    let jars = bounded_list(jars, "nested jars", FABRIC)?;

    let mod_entry = JavaMod {
        id: id.to_owned(),
        version,
        name,
        depends: fabric_dependencies(object, "depends", true)?,
        recommends: fabric_dependencies(object, "recommends", false)?,
        suggests: fabric_dependencies(object, "suggests", false)?,
        conflicts: fabric_dependencies(object, "conflicts", false)?,
        breaks: fabric_dependencies(object, "breaks", false)?,
    };
    if mod_entry.dependencies().count() > MAX_LIST {
        return Err(format!(
            "{FABRIC}: declares more than {MAX_LIST} dependencies"
        ));
    }
    Ok(JavaModMetadata {
        loader: JavaLoader::Fabric,
        mods: vec![mod_entry],
        side,
        entrypoints,
        mixins,
        jars,
        mod_loader: None,
        loader_version: None,
        license: None,
    })
}

/// Reads `META-INF/neoforge.mods.toml`: `modLoader`, `loaderVersion`,
/// `license`, each `[[mods]]` table, each `[[dependencies.<modid>]]` table and
/// each `[[mixins]]` config. Never panics; unknown `side`, `ordering` or `type`
/// values are refused with the line that wrote them.
pub fn parse_neoforge_toml(text: &str) -> Result<JavaModMetadata, String> {
    let value: toml::Value = toml::from_str(text).map_err(|e| format!("{NEOFORGE}: {e}"))?;
    let table = value
        .as_table()
        .ok_or_else(|| format!("{NEOFORGE}: the metadata is not one TOML table"))?;
    let field = |key: &str| table.get(key).and_then(toml::Value::as_str);
    let mod_loader = field("modLoader")
        .map(|text| bounded(text, MAX_TEXT, "modLoader", NEOFORGE))
        .transpose()?;
    let loader_version = field("loaderVersion")
        .map(|text| bounded(text, MAX_VERSION, "loaderVersion", NEOFORGE))
        .transpose()?;
    let license = field("license")
        .map(|text| bounded(text, MAX_TEXT, "license", NEOFORGE))
        .transpose()?;

    let mut mods: Vec<JavaMod> = Vec::new();
    if let Some(value) = table.get("mods") {
        let entries = value
            .as_array()
            .ok_or_else(|| format!("{NEOFORGE}: mods is not a list of [[mods]] tables"))?;
        if entries.len() > MAX_LIST {
            return Err(format!("{NEOFORGE}: declares more than {MAX_LIST} mods"));
        }
        for (index, entry) in entries.iter().enumerate() {
            let entry = entry
                .as_table()
                .ok_or_else(|| format!("{NEOFORGE}: [[mods]] #{} is not a table", index + 1))?;
            let id = entry
                .get("modId")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| format!("{NEOFORGE}: [[mods]] #{} has no modId", index + 1))?;
            if !mod_id_ok(id) {
                return Err(format!(
                    "{NEOFORGE}: [[mods]] #{} modId {:?} is not [a-z0-9_.-]{{1,{MAX_ID}}}",
                    index + 1,
                    excerpt(id)
                ));
            }
            if mods.iter().any(|other| other.id == id) {
                return Err(format!("{NEOFORGE}: mod {id} is listed twice"));
            }
            let entry_field = |key: &str| entry.get(key).and_then(toml::Value::as_str);
            mods.push(JavaMod {
                id: id.to_owned(),
                version: entry_field("version")
                    .map(|text| bounded(text, MAX_VERSION, "version", NEOFORGE))
                    .transpose()?,
                name: entry_field("displayName")
                    .map(|text| bounded(text, MAX_TEXT, "displayName", NEOFORGE))
                    .transpose()?,
                depends: Vec::new(),
                recommends: Vec::new(),
                suggests: Vec::new(),
                conflicts: Vec::new(),
                breaks: Vec::new(),
            });
        }
    }
    if mods.is_empty() {
        return Err(format!("{NEOFORGE}: no [[mods]] table names the mod"));
    }

    // The line of each `side`/`ordering` key, per `[[dependencies.<modid>]]`
    // table, for the errors below.
    let lines = dependency_lines(text);
    if let Some(value) = table.get("dependencies") {
        let owners = value.as_table().ok_or_else(|| {
            format!("{NEOFORGE}: dependencies is not a table of [[dependencies.<modid>]] lists")
        })?;
        for (owner, list) in owners {
            let owner_index = mods.iter().position(|m| m.id == *owner).ok_or_else(|| {
                format!(
                    "{NEOFORGE}: [[dependencies.{owner}]] names a mod the file does not declare"
                )
            })?;
            let list = list.as_array().ok_or_else(|| {
                format!("{NEOFORGE}: dependencies.{owner} is not a list of tables")
            })?;
            for (index, entry) in list.iter().enumerate() {
                let entry = entry.as_table().ok_or_else(|| {
                    format!(
                        "{NEOFORGE}: [[dependencies.{owner}]] #{} is not a table",
                        index + 1
                    )
                })?;
                let id = entry
                    .get("modId")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "{NEOFORGE}: [[dependencies.{owner}]] #{} has no modId",
                            index + 1
                        )
                    })?;
                check_dependency_id(id, NEOFORGE)?;
                let (side_line, ordering_line) = lines
                    .get(owner.as_str())
                    .and_then(|tables| tables.get(index))
                    .copied()
                    .unwrap_or((None, None));
                let (kind, mandatory) = dependency_kind(entry, owner, index)?;
                let dependency = JavaDependency {
                    id: id.to_owned(),
                    version: entry
                        .get("versionRange")
                        .and_then(toml::Value::as_str)
                        .map(|text| bounded(text, MAX_VERSION, "versionRange", NEOFORGE))
                        .transpose()?,
                    side: dependency_side(entry.get("side"), side_line)?,
                    mandatory,
                    ordering: dependency_ordering(entry.get("ordering"), ordering_line)?,
                };
                let target = match kind {
                    NeoKind::Depends => &mut mods[owner_index].depends,
                    NeoKind::Conflicts => &mut mods[owner_index].conflicts,
                    NeoKind::Breaks => &mut mods[owner_index].breaks,
                };
                push_dependency(target, dependency, NEOFORGE)?;
            }
        }
    }
    if mods.iter().map(|m| m.dependencies().count()).sum::<usize>() > MAX_LIST {
        return Err(format!(
            "{NEOFORGE}: declares more than {MAX_LIST} dependencies"
        ));
    }

    let mut mixins = Vec::new();
    if let Some(value) = table.get("mixins") {
        let entries = value
            .as_array()
            .ok_or_else(|| format!("{NEOFORGE}: mixins is not a list of [[mixins]] tables"))?;
        for (index, entry) in entries.iter().enumerate() {
            let entry = entry
                .as_table()
                .ok_or_else(|| format!("{NEOFORGE}: [[mixins]] #{} is not a table", index + 1))?;
            let config = entry
                .get("config")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| format!("{NEOFORGE}: [[mixins]] #{} has no config", index + 1))?;
            mixins.push(bounded(config, MAX_TEXT, "mixin config", NEOFORGE)?);
        }
    }
    let mixins = bounded_list(mixins, "mixin configs", NEOFORGE)?;

    Ok(JavaModMetadata {
        loader: JavaLoader::NeoForge,
        mods,
        side: DependencySide::Both,
        entrypoints: BTreeMap::new(),
        mixins,
        jars: Vec::new(),
        mod_loader,
        loader_version,
        license,
    })
}

/// The list a NeoForge dependency's declaration belongs in.
#[derive(Clone, Copy)]
enum NeoKind {
    Depends,
    Conflicts,
    Breaks,
}

/// `value` as the entries of a list: also the single value a loader allows
/// where it takes a list.
fn json_entries<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<Vec<&'a serde_json::Value>, String> {
    match value {
        serde_json::Value::Array(entries) => Ok(entries.iter().collect()),
        serde_json::Value::Null => Err(format!("{FABRIC}: {key} is null")),
        one => Ok(vec![one]),
    }
}

/// A Fabric entry that is its string, or an object holding it under `key`
/// (`{"config": ...}`, `{"value": ...}`).
fn json_named<'a>(entry: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    match entry {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Object(object) => object.get(key).and_then(serde_json::Value::as_str),
        _ => None,
    }
}

/// `list`, refused over the bound so no file makes the loader hold more than
/// it will ever use.
fn bounded_list<T>(list: Vec<T>, what: &str, origin: &str) -> Result<Vec<T>, String> {
    match list.len() > MAX_LIST {
        true => Err(format!("{origin}: more than {MAX_LIST} {what}")),
        false => Ok(list),
    }
}

/// One of `depends`, `recommends`, `suggests`, `conflicts` or `breaks`: a map
/// of mod id to version range. An object's keys are unique — serde_json keeps
/// the last of a repeated key — so a repeated id cannot reach this.
fn fabric_dependencies(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    mandatory: bool,
) -> Result<Vec<JavaDependency>, String> {
    let mut list = Vec::new();
    let Some(value) = object.get(key) else {
        return Ok(list);
    };
    let ranges = value
        .as_object()
        .ok_or_else(|| format!("{FABRIC}: {key} is not an object of id to version range"))?;
    for (id, range) in ranges {
        check_dependency_id(id, FABRIC)?;
        let version = match range {
            serde_json::Value::String(range) => {
                Some(bounded(range, MAX_VERSION, "version range", FABRIC)?)
            }
            // A range list (">=1.20", "<1.22") is one text for the log.
            serde_json::Value::Array(ranges) => {
                let text = ranges
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                match text.is_empty() {
                    true => None,
                    false => Some(bounded(&text, MAX_VERSION, "version range", FABRIC)?),
                }
            }
            _ => {
                return Err(format!(
                    "{FABRIC}: the version range for {id} is not a string"
                ));
            }
        };
        push_dependency(
            &mut list,
            JavaDependency {
                id: id.clone(),
                version,
                side: DependencySide::Both,
                mandatory,
                ordering: DependencyOrdering::None,
            },
            FABRIC,
        )?;
    }
    Ok(list)
}

/// The line of each `side` and `ordering` key, by `[[dependencies.<modid>]]`
/// table, in the file's order: entry `i` of an owner is the `i`-th dependency
/// read for it, so an error can name the line that made it.
fn dependency_lines(text: &str) -> BTreeMap<String, Vec<(Option<usize>, Option<usize>)>> {
    let mut found: BTreeMap<String, Vec<(Option<usize>, Option<usize>)>> = BTreeMap::new();
    let mut current: Option<(String, usize)> = None;
    for (number, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.starts_with('[') {
            current = line
                .strip_prefix("[[dependencies.")
                .map(|rest| rest.trim_end_matches(']').trim().to_owned())
                .map(|owner| {
                    let tables = found.entry(owner.clone()).or_default();
                    tables.push((None, None));
                    (owner, tables.len() - 1)
                });
            continue;
        }
        let (Some((owner, index)), Some((key, _))) = (&current, line.split_once('=')) else {
            continue;
        };
        let Some(table) = found
            .get_mut(owner)
            .and_then(|tables| tables.get_mut(*index))
        else {
            continue;
        };
        match key.trim() {
            "side" => table.0 = Some(number + 1),
            "ordering" => table.1 = Some(number + 1),
            _ => {}
        }
    }
    found
}

/// A NeoForge dependency's `side`, `BOTH` when the key is absent.
fn dependency_side(
    value: Option<&toml::Value>,
    line: Option<usize>,
) -> Result<DependencySide, String> {
    let Some(value) = value else {
        return Ok(DependencySide::Both);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{NEOFORGE}: side{} is not a string", at(line)))?;
    match text.to_ascii_lowercase().as_str() {
        "both" => Ok(DependencySide::Both),
        "client" => Ok(DependencySide::Client),
        "server" => Ok(DependencySide::Server),
        _ => Err(format!(
            "{NEOFORGE}: side {:?}{} is not BOTH, CLIENT or SERVER",
            excerpt(text),
            at(line)
        )),
    }
}

/// A NeoForge dependency's `ordering`, `NONE` when the key is absent.
fn dependency_ordering(
    value: Option<&toml::Value>,
    line: Option<usize>,
) -> Result<DependencyOrdering, String> {
    let Some(value) = value else {
        return Ok(DependencyOrdering::None);
    };
    let text = value
        .as_str()
        .ok_or_else(|| format!("{NEOFORGE}: ordering{} is not a string", at(line)))?;
    match text.to_ascii_lowercase().as_str() {
        "none" => Ok(DependencyOrdering::None),
        "before" => Ok(DependencyOrdering::Before),
        "after" => Ok(DependencyOrdering::After),
        "nested" => Ok(DependencyOrdering::Nested),
        _ => Err(format!(
            "{NEOFORGE}: ordering {:?}{} is not NONE, BEFORE, AFTER or NESTED",
            excerpt(text),
            at(line)
        )),
    }
}

/// Which list a NeoForge dependency belongs in, and whether it is mandatory:
/// `mandatory` (default true) or 1.21's `type`. Unknown types are refused
/// rather than guessed at.
fn dependency_kind(
    entry: &toml::Table,
    owner: &str,
    index: usize,
) -> Result<(NeoKind, bool), String> {
    if let Some(value) = entry.get("mandatory") {
        let mandatory = value.as_bool().ok_or_else(|| {
            format!(
                "{NEOFORGE}: [[dependencies.{owner}]] #{} mandatory is not true or false",
                index + 1
            )
        })?;
        return Ok((NeoKind::Depends, mandatory));
    }
    let kind = match entry.get("type").and_then(toml::Value::as_str) {
        None => (NeoKind::Depends, true),
        Some(kind) if kind.eq_ignore_ascii_case("required") => (NeoKind::Depends, true),
        Some(kind) if kind.eq_ignore_ascii_case("optional") => (NeoKind::Depends, false),
        Some(kind) if kind.eq_ignore_ascii_case("incompatible") => (NeoKind::Breaks, false),
        Some(kind) if kind.eq_ignore_ascii_case("discouraged") => (NeoKind::Conflicts, false),
        Some(other) => {
            return Err(format!(
                "{NEOFORGE}: [[dependencies.{owner}]] #{} type {:?} is not required, optional, \
                 incompatible or discouraged",
                index + 1,
                excerpt(other)
            ));
        }
    };
    Ok(kind)
}

/// Adds one dependency to its list, refusing a repeated id instead of letting
/// the last declaration win: the order a file states is the order it means.
fn push_dependency(
    list: &mut Vec<JavaDependency>,
    dependency: JavaDependency,
    origin: &str,
) -> Result<(), String> {
    if list.iter().any(|other| other.id == dependency.id) {
        return Err(format!(
            "{origin}: dependency {} is listed twice",
            dependency.id
        ));
    }
    list.push(dependency);
    Ok(())
}

/// A mod id's grammar: lowercase letters, digits and `_-.`, at most
/// [`MAX_ID`] characters.
fn mod_id_ok(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b))
}

fn check_dependency_id(id: &str, origin: &str) -> Result<(), String> {
    match mod_id_ok(id) {
        true => Ok(()),
        false => Err(format!(
            "{origin}: dependency id {:?} is not [a-z0-9_.-]{{1,{MAX_ID}}}",
            excerpt(id)
        )),
    }
}

/// `text`, refusing one over `max` characters so no metadata file decides how
/// much a name costs the loader. Reads at most `max + 1` characters.
fn bounded(text: &str, max: usize, what: &str, origin: &str) -> Result<String, String> {
    if text.len() > max && text.char_indices().nth(max).is_some() {
        return Err(format!(
            "{origin}: {what} {:?} is over {max} characters",
            excerpt(text)
        ));
    }
    Ok(text.to_owned())
}

/// The start of `text`, so an error about a huge string stays readable.
fn excerpt(text: &str) -> String {
    match text.char_indices().nth(EXCERPT) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// " (line 12)", or nothing when the line could not be found.
fn at(line: Option<usize>) -> String {
    line.map_or(String::new(), |line| format!(" (line {line})"))
}

/// A mod as found on disk: its manifest and component bytes.
pub struct Package {
    pub manifest: Manifest,
    /// None for a content-only mod (capability `registries`, no code).
    pub wasm: Option<Vec<u8>>,
}
impl Package {
    /// A `.hmod` (a zip holding `horizon-mod.toml`, `mod.wasm` and, for
    /// content, `assets/` and `data/`) or a folder holding the same.
    pub fn read(path: &Path) -> Result<Self, String> {
        let (toml, wasm) = if path.is_dir() {
            let toml = std::fs::read_to_string(path.join(MANIFEST));
            let wasm = path.join(WASM);
            let wasm = match wasm.exists() {
                true => Some(std::fs::read(wasm).map_err(|e| format!("{WASM}: {e}"))?),
                false => None,
            };
            (toml.map_err(|e| format!("{MANIFEST}: {e}"))?, wasm)
        } else {
            let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
            let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
            let mut entry = |name: &str| -> Result<Option<Vec<u8>>, String> {
                let f = match zip.by_name(name) {
                    Ok(f) => f,
                    Err(zip::result::ZipError::FileNotFound) => return Ok(None),
                    Err(e) => return Err(format!("{name}: {e}")),
                };
                let mut out = Vec::new();
                f.take(MAX_WASM + 1)
                    .read_to_end(&mut out)
                    .map_err(|e| e.to_string())?;
                Ok(Some(out))
            };
            let toml = entry(MANIFEST)?.ok_or_else(|| format!("{MANIFEST}: missing"))?;
            let toml = String::from_utf8(toml).map_err(|_| format!("{MANIFEST}: not UTF-8"))?;
            (toml, entry(WASM)?)
        };
        if wasm.as_ref().is_some_and(|w| w.len() as u64 > MAX_WASM) {
            return Err(format!("{WASM} is over {} MiB", MAX_WASM >> 20));
        }
        let manifest = Manifest::parse(&toml)?;
        if wasm.is_none() && !manifest.capabilities.contains(&Capability::Registries) {
            return Err(format!(
                "{WASM}: missing (only a content mod, with capability registries, may have no code)"
            ));
        }
        Ok(Package { manifest, wasm })
    }
    /// The `.hmod` bytes for `manifest_text` and a component.
    pub fn write(manifest_text: &str, wasm: &[u8]) -> Result<Vec<u8>, String> {
        Self::write_files(manifest_text, Some(wasm), &[])
    }
    /// The `.hmod` bytes for `manifest_text`, a component if the mod has
    /// code, and content `files` (`assets/...`, `data/...`) by path.
    pub fn write_files(
        manifest_text: &str,
        wasm: Option<&[u8]>,
        files: &[(&str, &[u8])],
    ) -> Result<Vec<u8>, String> {
        use std::io::Write;
        Manifest::parse(manifest_text)?;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let err = |e: zip::result::ZipError| e.to_string();
        let mut put = |name: &str, bytes: &[u8]| -> Result<(), String> {
            zip.start_file(name, options).map_err(err)?;
            zip.write_all(bytes).map_err(|e| e.to_string())
        };
        put(MANIFEST, manifest_text.as_bytes())?;
        if let Some(wasm) = wasm {
            put(WASM, wasm)?;
        }
        for (name, bytes) in files {
            crate::content::check_path(name)?;
            put(name, bytes)?;
        }
        Ok(zip.finish().map_err(err)?.into_inner())
    }
}

/// The `.hmod` files and mod folders in `dir`, by name.
pub fn find(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "hmod") || p.join(MANIFEST).is_file())
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
id = "coords_hud"
name = "Coordinates HUD"
version = "0.1.0"
api = "0.1"
capabilities = ["game-read", "hud"]
"#;

    #[test]
    fn parses_and_checks_imports() {
        let m = Manifest::parse(EXAMPLE).unwrap();
        assert_eq!(m.side, Side::Client);
        assert_eq!(m.capabilities, [Capability::GameRead, Capability::Hud]);
        let ok = [
            "horizon:mods/log@0.1.0",
            "horizon:mods/game@0.1.0",
            "horizon:mods/hud@0.1.2",
        ];
        assert_eq!(m.check_imports(ok), Ok(()));
        let chat = m.check_imports(["horizon:mods/chat@0.1.0"]);
        assert_eq!(
            chat,
            Err("uses capability chat, missing from horizon-mod.toml".into())
        );
        assert!(m.check_imports(["wasi:filesystem/types@0.2.0"]).is_err());
        assert!(m.check_imports(["horizon:mods/game@0.2.0"]).is_err());
        let storage = m.check_imports(["horizon:mods/storage@0.1.0"]);
        assert_eq!(
            storage,
            Err("uses capability storage, missing from horizon-mod.toml".into())
        );
        assert_eq!(
            m.check_imports(["horizon:mods/command@0.1.0"]),
            Err("uses capability command, missing from horizon-mod.toml".into())
        );
        let with_storage = EXAMPLE.replace("\"hud\"", "\"hud\", \"storage\"");
        let m = Manifest::parse(&with_storage).unwrap();
        assert_eq!(m.check_imports(["horizon:mods/storage@0.1.0"]), Ok(()));
    }

    #[test]
    fn refuses_bad_ids_and_apis() {
        let bad_id = EXAMPLE.replace("coords_hud", "Coords HUD");
        assert!(Manifest::parse(&bad_id).unwrap_err().contains("is not"));
        let bad_api = EXAMPLE.replace("api = \"0.1\"", "api = \"0.4\"");
        assert!(Manifest::parse(&bad_api).unwrap_err().contains("API 0.4"));
        let unknown = format!("{EXAMPLE}network = true\n");
        assert!(Manifest::parse(&unknown).is_err());
    }

    #[test]
    fn server_mods_are_api_0_3_with_server_capabilities() {
        let server = |api: &str, side: &str, caps: &str| {
            Manifest::parse(&format!(
                "id = \"w\"\nname = \"W\"\nversion = \"1\"\napi = \"{api}\"\nside = \"{side}\"\ncapabilities = [{caps}]\n"
            ))
        };
        let m = server("0.3", "server", "\"players\", \"storage\"").unwrap();
        let imports = ["horizon:mods/players@0.1.0", "horizon:mods/storage@0.1.0"];
        assert_eq!(m.check_imports(imports), Ok(()));
        for (api, side) in [("0.3", "client"), ("0.3", "both"), ("0.1", "server")] {
            let e = server(api, side, "").unwrap_err();
            assert!(e.contains("a server mod is side"), "{e}");
        }
        let e = server("0.3", "server", "\"hud\"").unwrap_err();
        assert!(e.contains("capability hud is for client mods"), "{e}");
        let e = server("0.1", "client", "\"players\"").unwrap_err();
        assert!(e.contains("capability players is for server mods"), "{e}");
    }

    #[test]
    fn checks_extra_namespaces() {
        let content = "id = \"t_and_t\"\nname = \"T\"\nversion = \"1\"\napi = \"0.2\"\ncapabilities = [\"registries\"]\n";
        let with = |list: &str| Manifest::parse(&format!("{content}namespaces = [{list}]\n"));
        let m = with("\"kaisyn\", \"towns_and_towers\"").unwrap();
        let owned: Vec<&str> = m.owned_namespaces().collect();
        assert_eq!(owned, ["t_and_t", "kaisyn", "towns_and_towers"]);
        for reserved in ["minecraft", "c", "horizon"] {
            let e = with(&format!("\"{reserved}\"")).unwrap_err();
            assert!(e.contains("not a mod's to claim"), "{e}");
        }
        assert!(with("\"t_and_t\"").unwrap_err().contains("twice"));
        assert!(with("\"a\", \"a\"").unwrap_err().contains("twice"));
        assert!(with("\"Kaisyn\"").unwrap_err().contains("is not"));
        assert!(with("\"\"").unwrap_err().contains("is not"));
        let code = format!("{EXAMPLE}namespaces = [\"kaisyn\"]\n");
        assert!(Manifest::parse(&code).unwrap_err().contains("registries"));
    }

    #[test]
    fn hmod_round_trips() {
        let dir = std::env::temp_dir().join(format!("horizon-mods-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("coords_hud.hmod");
        std::fs::write(&path, Package::write(EXAMPLE, b"\0asm").unwrap()).unwrap();
        let p = Package::read(&path).unwrap();
        assert_eq!(p.manifest.id, "coords_hud");
        assert_eq!(p.wasm.as_deref(), Some(&b"\0asm"[..]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn experimental_capabilities_need_the_opt_in() {
        let base = "id = \"m\"\nname = \"M\"\nversion = \"1\"\napi = \"0.1\"\n";
        let err = Manifest::parse(&format!("{base}capabilities = [\"ui\"]\n")).unwrap_err();
        assert!(err.contains("experimental = [\"ui\"]"), "{err}");
        let m = Manifest::parse(&format!(
            "{base}capabilities = [\"ui\"]\nexperimental = [\"ui\"]\n"
        ))
        .unwrap();
        assert_eq!(m.experimental, [Capability::Ui]);
        // A capability that is (or became) stable is fine in the list.
        Manifest::parse(&format!(
            "{base}capabilities = [\"hud\"]\nexperimental = [\"hud\"]\n"
        ))
        .unwrap();
    }

    const FABRIC_MOD: &str = r#"{
  "schemaVersion": 1,
  "id": "sodium_extras",
  "version": "0.5.11+mc1.21.1",
  "name": "Sodium Extras",
  "environment": "client",
  "entrypoints": {
    "main": ["com.example.extras.Extras"],
    "client": [{"adapter": "net.fabricmc.api.ClientModInitializer", "value": "com.example.extras.client.ExtrasClient"}]
  },
  "mixins": ["extras.mixins.json", {"config": "extras.client.mixins.json", "environment": "client"}],
  "depends": {
    "fabricloader": ">=0.15",
    "minecraft": ["1.21", "1.21.1"],
    "sodium": ">=0.6"
  },
  "recommends": {"indium": "*"},
  "suggests": {"modmenu": ">=11"},
  "conflicts": {"embeddium": "*"},
  "breaks": {"sodium": "<0.5"},
  "jars": [{"file": "META-INF/jars/extras-core.jar", "id": "extrascore", "version": "1.0"}]
}"#;

    const NEOFORGE_MOD: &str = r#"
modLoader = "javafml"
loaderVersion = "[4,)"
license = "MIT"

[[mods]]
modId = "ruby"
version = "1.2.3"
displayName = "Ruby"

[[mods]]
modId = "ruby_worldgen"
version = "1.0"
displayName = "Ruby Worldgen"

[[dependencies.ruby]]
modId = "neoforge"
type = "required"
versionRange = "[21.1,)"
ordering = "NONE"
side = "BOTH"

[[dependencies.ruby]]
modId = "geckolib"
mandatory = false
versionRange = "[4,)"

[[dependencies.ruby]]
modId = "jei"
type = "optional"
side = "CLIENT"

[[dependencies.ruby]]
modId = "optifine"
type = "incompatible"
versionRange = "*"

[[dependencies.ruby_worldgen]]
modId = "ruby"
mandatory = true
ordering = "AFTER"
side = "SERVER"

[[mixins]]
config = "ruby.mixins.json"

[[mixins]]
config = "ruby.client.mixins.json"
"#;

    #[test]
    fn fabric_metadata_reads_every_dependency_kind() {
        let m = parse_fabric_mod_json(FABRIC_MOD.as_bytes()).unwrap();
        assert_eq!(m.loader, JavaLoader::Fabric);
        assert_eq!(m.loader.name(), "fabric");
        assert_eq!(m.id(), "sodium_extras");
        assert_eq!(m.side, DependencySide::Client);
        let entry = &m.mods[0];
        assert_eq!(entry.version.as_deref(), Some("0.5.11+mc1.21.1"));
        assert_eq!(entry.name.as_deref(), Some("Sodium Extras"));
        // `depends` keeps the map's (sorted) order, one flag for all of it.
        let ids: Vec<&str> = entry.depends.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["fabricloader", "minecraft", "sodium"]);
        assert!(entry.depends.iter().all(|d| d.mandatory));
        assert_eq!(entry.depends[1].version.as_deref(), Some("1.21, 1.21.1"));
        assert_eq!(entry.recommends[0].id, "indium");
        assert!(!entry.recommends[0].mandatory);
        assert_eq!(entry.suggests[0].id, "modmenu");
        assert_eq!(entry.conflicts[0].id, "embeddium");
        // The same id may be depended on and broken: different lists.
        assert_eq!(entry.breaks[0].id, "sodium");
        assert_eq!(entry.breaks[0].version.as_deref(), Some("<0.5"));
        assert_eq!(m.dependencies().count(), 7);
        assert_eq!(m.entrypoints["main"], ["com.example.extras.Extras"]);
        assert_eq!(
            m.entrypoints["client"],
            ["com.example.extras.client.ExtrasClient"]
        );
        assert_eq!(
            m.mixins,
            ["extras.mixins.json", "extras.client.mixins.json"]
        );
        assert_eq!(m.jars[0].file, "META-INF/jars/extras-core.jar");
        assert_eq!(m.jars[0].id.as_deref(), Some("extrascore"));
        assert_eq!(m.jars[0].version.as_deref(), Some("1.0"));
        assert_eq!(m.mod_loader.as_deref(), None);
    }

    #[test]
    fn neoforge_metadata_reads_two_mods_and_their_dependencies() {
        let m = parse_neoforge_toml(NEOFORGE_MOD).unwrap();
        assert_eq!(m.loader, JavaLoader::NeoForge);
        assert_eq!(m.id(), "ruby");
        assert_eq!(m.mods.len(), 2);
        assert_eq!(m.mods[1].id, "ruby_worldgen");
        assert_eq!(m.mods[0].name.as_deref(), Some("Ruby"));
        assert_eq!(m.mod_loader.as_deref(), Some("javafml"));
        assert_eq!(m.loader_version.as_deref(), Some("[4,)"));
        assert_eq!(m.license.as_deref(), Some("MIT"));
        let ruby = &m.mods[0];
        assert_eq!(ruby.depends.len(), 3);
        assert_eq!(ruby.depends[0].id, "neoforge");
        assert!(ruby.depends[0].mandatory);
        assert_eq!(ruby.depends[0].version.as_deref(), Some("[21.1,)"));
        assert_eq!(ruby.depends[0].ordering, DependencyOrdering::None);
        assert_eq!(ruby.depends[0].side, DependencySide::Both);
        // `mandatory = false` and `type = "optional"` are the soft pair.
        assert!(!ruby.depends[1].mandatory);
        assert_eq!(ruby.depends[2].side, DependencySide::Client);
        assert!(!ruby.depends[2].mandatory);
        // `type = "incompatible"` is a break, not a dependency.
        assert_eq!(ruby.breaks.len(), 1);
        assert_eq!(ruby.breaks[0].id, "optifine");
        assert_eq!(m.mods[1].depends[0].ordering, DependencyOrdering::After);
        assert_eq!(m.mods[1].depends[0].side, DependencySide::Server);
        assert_eq!(m.mods[1].depends[0].id, "ruby");
        assert_eq!(m.mixins, ["ruby.mixins.json", "ruby.client.mixins.json"]);
        assert_eq!(m.dependencies().count(), 5);
        assert_eq!(m.side, DependencySide::Both);
    }

    #[test]
    fn dependency_sides_and_ordering_are_checked() {
        let fabric_side = |environment: &str| {
            let json = format!(r#"{{"id":"m","environment":"{environment}"}}"#);
            parse_fabric_mod_json(json.as_bytes()).map(|m| m.side)
        };
        assert_eq!(fabric_side("*").unwrap(), DependencySide::Both);
        assert_eq!(fabric_side("client").unwrap(), DependencySide::Client);
        assert_eq!(fabric_side("server").unwrap(), DependencySide::Server);
        let error = fabric_side("dedicated").unwrap_err();
        assert!(error.contains("environment"), "{error}");

        // Every `[[dependencies.*]]` is line 3, and `{extra}` is line 5.
        let neoforge = |extra: &str| {
            parse_neoforge_toml(&format!(
                "[[mods]]\nmodId = \"m\"\n[[dependencies.m]]\nmodId = \"a\"\n{extra}"
            ))
            .map(|m| m.mods[0].depends[0].clone())
        };
        assert_eq!(
            neoforge("side = \"CLIENT\"\n").unwrap().side,
            DependencySide::Client
        );
        assert_eq!(
            neoforge("side = \"server\"\n").unwrap().side,
            DependencySide::Server
        );
        let error = neoforge("side = \"dedicated\"\n").unwrap_err();
        assert!(error.contains("is not BOTH, CLIENT or SERVER"), "{error}");
        assert!(error.contains("(line 5)"), "{error}");
        assert_eq!(
            neoforge("ordering = \"BEFORE\"\n").unwrap().ordering,
            DependencyOrdering::Before
        );
        assert_eq!(
            neoforge("ordering = \"after\"\n").unwrap().ordering,
            DependencyOrdering::After
        );
        // A nested jar loads with its parent.
        assert_eq!(
            neoforge("ordering = \"NESTED\"\n").unwrap().ordering,
            DependencyOrdering::Nested
        );
        let error = neoforge("ordering = \"sideways\"\n").unwrap_err();
        assert!(
            error.contains("is not NONE, BEFORE, AFTER or NESTED"),
            "{error}"
        );
        assert!(error.contains("(line 5)"), "{error}");
        let error = neoforge("side = 3\n").unwrap_err();
        assert!(error.contains("is not a string"), "{error}");
    }

    #[test]
    fn refuses_duplicate_and_malformed_declarations() {
        let duplicate = "[[mods]]\nmodId = \"m\"\n[[dependencies.m]]\nmodId = \"a\"\n\
                         [[dependencies.m]]\nmodId = \"a\"\n";
        let error = parse_neoforge_toml(duplicate).unwrap_err();
        assert!(error.contains("listed twice"), "{error}");
        let same_mod = "[[mods]]\nmodId = \"m\"\n[[mods]]\nmodId = \"m\"\n";
        assert!(parse_neoforge_toml(same_mod).unwrap_err().contains("twice"));
        let stray = "[[mods]]\nmodId = \"m\"\n[[dependencies.other]]\nmodId = \"a\"\n";
        let error = parse_neoforge_toml(stray).unwrap_err();
        assert!(error.contains("does not declare"), "{error}");
        let unknown_type =
            "[[mods]]\nmodId = \"m\"\n[[dependencies.m]]\nmodId = \"a\"\ntype = \"maybe\"\n";
        let error = parse_neoforge_toml(unknown_type).unwrap_err();
        assert!(error.contains("is not required, optional"), "{error}");
        let loose =
            "[[mods]]\nmodId = \"m\"\n[[dependencies.m]]\nmodId = \"a\"\nmandatory = \"yes\"\n";
        assert!(
            parse_neoforge_toml(loose)
                .unwrap_err()
                .contains("true or false")
        );

        let bad_id = r#"{"id":"Bad Id"}"#;
        assert!(
            parse_fabric_mod_json(bad_id.as_bytes())
                .unwrap_err()
                .contains("is not")
        );
        let long_id = format!(r#"{{"id":"{}"}}"#, "a".repeat(MAX_ID + 1));
        assert!(
            parse_fabric_mod_json(long_id.as_bytes())
                .unwrap_err()
                .contains("is not")
        );
        let long_version = format!(
            r#"{{"id":"m","depends":{{"a":"{}"}}}}"#,
            "1".repeat(MAX_VERSION + 1)
        );
        let error = parse_fabric_mod_json(long_version.as_bytes()).unwrap_err();
        assert!(error.contains("is over"), "{error}");

        // One more dependency than the bound.
        let many = (0..=MAX_LIST)
            .map(|index| format!("\"mod{index}\": \"*\""))
            .collect::<Vec<_>>()
            .join(",");
        let bounded = format!(r#"{{"id":"m","depends":{{{many}}}}}"#);
        let error = parse_fabric_mod_json(bounded.as_bytes()).unwrap_err();
        assert!(error.contains("more than 256"), "{error}");

        // Malformed input is an error, never a panic.
        for input in ["not json", "[]", "{}", "null"] {
            assert!(parse_fabric_mod_json(input.as_bytes()).is_err(), "{input}");
        }
        for input in [
            "",
            "not toml =",
            "[mods]",
            "[[mods]]\nversion = \"1\"\n",
            "[[mods]]\nmodId = \"m\"\n[[mixins]]\nconfig = 3\n",
        ] {
            assert!(parse_neoforge_toml(input).is_err(), "{input}");
        }
    }

    #[test]
    fn loader_metadata_round_trips() {
        // Parsing the same bytes twice gives the same metadata, and every field
        // the loader pipeline reads survives it.
        let first = parse_fabric_mod_json(FABRIC_MOD.as_bytes()).unwrap();
        let second = parse_fabric_mod_json(FABRIC_MOD.as_bytes()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.mods[0].dependencies().count(), 7);
        assert_eq!(first.entrypoints.len(), 2);
        assert_eq!(first.mixins[1], "extras.client.mixins.json");
        assert_eq!(first.jars[0].id.as_deref(), Some("extrascore"));

        let neo = parse_neoforge_toml(NEOFORGE_MOD).unwrap();
        assert_eq!(parse_neoforge_toml(NEOFORGE_MOD).unwrap(), neo);
        let ids: Vec<&str> = neo.dependencies().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["neoforge", "geckolib", "jei", "optifine", "ruby"]);
        assert_eq!(neo.mods[1].version.as_deref(), Some("1.0"));
    }
}
