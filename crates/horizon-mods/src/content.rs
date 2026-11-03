//! A content mod's `assets/` and `data/` (API 0.2, docs/modpacks.md): the
//! files it adds to the game's resource tree, and the declarations it makes in
//! `data/<id>/horizon/` — blocks, items and mob effects in `block/`, `item/`
//! and `mob_effect/`, and the typed rows in `entity/`, `data_component/`,
//! `attachment/`, `menu/` and `block_entity/`. [`Content::from_native`] writes
//! all of them, so a native content root keeps its whole declaration set when
//! it is exported to a folder or a `.hmod`. Reading a content root validates
//! those files with the same parsers the native path writes them with, so a
//! root round-trips.
//!
//! Content is data, not code: nothing here runs in the sandbox. What a mod
//! may add is fenced instead. Its own namespaces (its id and the ones its
//! manifest lists), plus additions to vanilla tags and to the convention
//! tags (`c`, which NeoForge and Fabric mods share). Paths are plain, files
//! and totals are capped, vanilla ids cannot be redefined, and no two mods
//! share a namespace ([`clash`]). Native declarations ([`Content::from_native`])
//! add blocks, items, mob effects, entities, data components, attachments,
//! menus and block entities under the same fences.
use crate::manifest::{Capability, MANIFEST, Manifest};
use crate::native::{
    AttachmentDeclaration, BlockEntityDeclaration, DataComponentDeclaration, EntityDeclaration,
    MAX_NATIVE_TEXT, MenuDeclaration, NativeDeclarations,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
};

/// Content files per mod, bytes per file and bytes per mod.
pub const MAX_FILES: usize = 65_536;
pub const MAX_FILE: u64 = 16 << 20;
pub const MAX_TOTAL: u64 = 512 << 20;

/// Version of the typed JSON Horizon writes under `data/<namespace>/horizon/`
/// (entity, data-component, attachment, menu and block-entity rows). A reader
/// checks it before it trusts a row; a file without the marker is read as 1,
/// so rows written before it existed still load.
pub const NATIVE_FORMAT: u32 = 1;

/// Checks a content path: `assets/` or `data/`, then `/`-separated parts of
/// `[a-z0-9_.-]`, none of them `.` or `..` (vanilla's resource-location
/// characters, so the path means the same on every OS).
pub fn check_path(name: &str) -> Result<(), String> {
    let bad = || format!("{name:?} is not a plain assets/ or data/ path");
    let mut parts = name.split('/');
    if !matches!(parts.next(), Some("assets" | "data")) {
        return Err(bad());
    }
    let mut count = 0;
    for part in parts {
        count += 1;
        let chars_ok = part
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
        if part.is_empty() || part == "." || part == ".." || !chars_ok {
            return Err(bad());
        }
    }
    // assets/<namespace>/<file> at least.
    if count < 2 {
        return Err(bad());
    }
    Ok(())
}

/// A vanilla identifier, `minecraft:path`.
fn vanilla_id(id: &str) -> bool {
    id.strip_prefix("minecraft:").is_some_and(|path| {
        !path.is_empty()
            && path
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b))
    })
}

/// Native declarations may add new namespaces, but never redefine vanilla ids.
fn native_id(id: &str) -> bool {
    let Some((namespace, path)) = id.split_once(':') else {
        return false;
    };
    namespace != "minecraft"
        && !namespace.is_empty()
        && !path.is_empty()
        && namespace
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
        && path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b))
}

/// What a declared block changes from its template; unset keeps the
/// template's value.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockProperties {
    /// Destroy time in seconds of hand mining (-1: unbreakable).
    pub hardness: Option<f32>,
    pub explosion_resistance: Option<f32>,
    /// Light emitted, 0 to 15.
    pub light: Option<u8>,
    pub requires_tool: Option<bool>,
    pub friction: Option<f32>,
    /// A `MapColor` id, 0 to 61.
    pub map_color: Option<u8>,
    /// A vanilla block whose sounds this block uses.
    pub sounds: Option<String>,
}

/// Per-state outline and collision boxes, selected by one state property.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BlockShapes {
    pub property: String,
    pub values: BTreeMap<String, Vec<[f64; 6]>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockFile {
    template: String,
    #[serde(default)]
    properties: BlockProperties,
    shapes: Option<BlockShapes>,
    #[serde(default)]
    default_state: BTreeMap<String, String>,
    #[serde(default = "yes")]
    item: bool,
    #[serde(default)]
    components: BTreeMap<String, serde_json::Value>,
}
fn yes() -> bool {
    true
}

/// A block the mod adds: a vanilla template's behaviour, states and shapes
/// under a new id, with `properties` applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// `<mod id>:<path>`.
    pub id: String,
    /// The vanilla block it copies, `minecraft:<path>`.
    pub template: String,
    pub properties: BlockProperties,
    pub shapes: Option<BlockShapes>,
    /// State properties overlaid on the template default.
    pub default_state: BTreeMap<String, String>,
    /// Also add a block item with the same id.
    pub item: bool,
    /// Its item's component patch, as an [`Item`]'s.
    pub components: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemFile {
    template: String,
    max_stack_size: Option<u8>,
    rarity: Option<Rarity>,
    #[serde(default)]
    components: BTreeMap<String, serde_json::Value>,
    crafting_remainder: Option<String>,
}

/// An item the mod adds: a vanilla template item's components under a new
/// id, with these changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub id: String,
    /// The vanilla item it copies, `minecraft:<path>`.
    pub template: String,
    pub max_stack_size: Option<u8>,
    pub rarity: Option<Rarity>,
    /// A 26.3 data component patch over the template's: `!minecraft:<type>` removes one.
    pub components: BTreeMap<String, serde_json::Value>,
    /// What it leaves in the crafting grid (`Item.Properties.craftRemainder`):
    /// a vanilla item or one of the mod's own.
    pub crafting_remainder: Option<String>,
}

/// `MobEffectCategory`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EffectCategory {
    Beneficial,
    Harmful,
    Neutral,
}

/// A mob effect the mod adds: what `new MobEffect(category, color)` takes.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MobEffect {
    /// `<mod id>:<path>`.
    #[serde(skip)]
    pub id: String,
    pub category: EffectCategory,
    /// RGB, `0x000000` to `0xFFFFFF`.
    pub color: u32,
}

// The typed declaration files [`Content::from_native`] writes under
// `data/<id>/horizon/`. Each carries [`NATIVE_FORMAT`]; the marker is optional
// so a row written before it existed reads as format 1.

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntityFile {
    #[serde(default)]
    format: Option<u32>,
    template: String,
    #[serde(default)]
    properties: BTreeMap<String, String>,
    /// The typed view of `properties`. A reader rebuilds it from `properties`
    /// and refuses a file where the two disagree.
    #[serde(default)]
    spawn: Option<serde_json::Value>,
    #[serde(default)]
    attributes: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DataComponentFile {
    #[serde(default)]
    format: Option<u32>,
    codec: String,
    /// A data component is persistent unless it says otherwise, as in
    /// `native::NativeDeclarations`.
    #[serde(default = "yes")]
    persistent: bool,
    #[serde(default)]
    network_synced: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentFile {
    #[serde(default)]
    format: Option<u32>,
    codec: String,
    #[serde(default = "null_text")]
    default: String,
    #[serde(default)]
    copy_on_death: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuFile {
    #[serde(default)]
    format: Option<u32>,
    title_key: String,
    slots: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockEntityFile {
    #[serde(default)]
    format: Option<u32>,
    template: String,
    #[serde(default)]
    ticker: bool,
    valid_blocks: Vec<String>,
}

/// An attachment's default when a file leaves it out, as the native sink
/// defaults it.
fn null_text() -> String {
    "null".to_owned()
}

#[derive(Debug, Clone)]
enum Source {
    Dir(PathBuf),
    Zip(PathBuf),
}

/// A mod's content: its files and declarations.
#[derive(Debug, Clone)]
pub struct Content {
    source: Source,
    /// Every content file (`assets/...`, `data/...`), sorted.
    pub files: Vec<String>,
    pub blocks: Vec<Block>,
    pub items: Vec<Item>,
    pub effects: Vec<MobEffect>,
    /// Typed entity registrations emitted by a native Java adapter.
    pub entities: Vec<crate::native::EntityDeclaration>,
    /// Typed data-component registrations emitted by a native Java adapter.
    pub data_components: Vec<crate::native::DataComponentDeclaration>,
    /// Typed attachment registrations emitted by a native Java adapter.
    pub attachments: Vec<crate::native::AttachmentDeclaration>,
    /// Typed menu registrations emitted by a native Java adapter.
    pub menus: Vec<crate::native::MenuDeclaration>,
    /// Typed block-entity registrations emitted by a native Java adapter.
    pub block_entities: Vec<crate::native::BlockEntityDeclaration>,
    /// Data files supplied by a native adapter (recipes and ingredients). These
    /// are written into the same resource overlay as packaged mod files.
    pub native_files: BTreeMap<String, Vec<u8>>,
    /// The namespaces it owns: the mod id, then the manifest's `namespaces`.
    pub namespaces: Vec<String>,
}

/// Reads a mod's content files ([`Content::reader`]).
pub enum FileReader<'a> {
    Dir(&'a Path),
    Zip(Box<zip::ZipArchive<std::fs::File>>),
}
impl FileReader<'_> {
    /// One content file's bytes.
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, String> {
        check_path(name)?;
        let reader: Box<dyn Read + '_> = match self {
            Self::Dir(root) => {
                Box::new(std::fs::File::open(root.join(name)).map_err(|e| format!("{name}: {e}"))?)
            }
            Self::Zip(zip) => Box::new(zip.by_name(name).map_err(|e| format!("{name}: {e}"))?),
        };
        let mut out = Vec::new();
        reader
            .take(MAX_FILE + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("{name}: {e}"))?;
        if out.len() as u64 > MAX_FILE {
            return Err(format!("{name} is over {} MiB", MAX_FILE >> 20));
        }
        Ok(out)
    }
}

/// Why two content mods cannot be on together: a namespace both own (one's
/// id or listed namespace is the other's), which would mix their files.
/// `a` and `b` are (mod id, owned namespaces).
pub fn clash(a: (&str, &[String]), b: (&str, &[String])) -> Option<String> {
    let shared = a.1.iter().find(|ns| b.1.contains(ns))?;
    Some(format!(
        "mods {} and {} both use the namespace {shared}; turn one of them off",
        a.0, b.0
    ))
}

impl Content {
    /// The content of the mod at `path` (a `.hmod` or a mod folder).
    pub fn read(path: &Path, manifest: &Manifest) -> Result<Content, String> {
        let (source, sized) = if path.is_dir() {
            let mut out = Vec::new();
            for top in ["assets", "data"] {
                walk(path, &path.join(top), &mut out)?;
            }
            (Source::Dir(path.to_owned()), out)
        } else {
            let mut zip = open_zip(path)?;
            let mut out = Vec::new();
            for i in 0..zip.len() {
                let entry = zip.by_index_raw(i).map_err(|e| e.to_string())?;
                let name = entry.name();
                if (name.starts_with("assets/") || name.starts_with("data/")) && !entry.is_dir() {
                    out.push((name.to_owned(), entry.size()));
                }
            }
            (Source::Zip(path.to_owned()), out)
        };
        let mut files = Vec::with_capacity(sized.len());
        let mut total = 0u64;
        for (name, size) in sized {
            check_path(&name)?;
            if size > MAX_FILE {
                return Err(format!("{name} is over {} MiB", MAX_FILE >> 20));
            }
            total += size;
            files.push(name);
        }
        if files.len() > MAX_FILES {
            return Err(format!("more than {MAX_FILES} content files"));
        }
        if total > MAX_TOTAL {
            return Err(format!("content is over {} MiB", MAX_TOTAL >> 20));
        }
        files.sort();
        files.dedup();
        let mut content = Content {
            source,
            files,
            blocks: Vec::new(),
            items: Vec::new(),
            effects: Vec::new(),
            entities: Vec::new(),
            data_components: Vec::new(),
            attachments: Vec::new(),
            menus: Vec::new(),
            block_entities: Vec::new(),
            native_files: BTreeMap::new(),
            namespaces: manifest.owned_namespaces().map(str::to_owned).collect(),
        };
        content.check(manifest)?;
        Ok(content)
    }

    /// Where the files are read from: the mod's folder or its `.hmod`.
    pub fn source(&self) -> &Path {
        match &self.source {
            Source::Dir(path) | Source::Zip(path) => path,
        }
    }

    /// No files: a mod with code only.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.blocks.is_empty()
            && self.items.is_empty()
            && self.effects.is_empty()
            && self.entities.is_empty()
            && self.data_components.is_empty()
            && self.attachments.is_empty()
            && self.menus.is_empty()
            && self.block_entities.is_empty()
            && self.native_files.is_empty()
    }

    /// Builds the declaration-only content half of a native Java mod.
    ///
    /// The resulting content has no source files, but it participates in the
    /// same resource-root planning as a packaged `.hmod`. Blocks, items and
    /// mob effects are written in the very shapes the packaged path reads
    /// (`block/`, `item/` and `mob_effect/` files); those reject unknown keys,
    /// so they take no format marker. The typed rows carry a [`NATIVE_FORMAT`]
    /// marker: entities add their typed [`crate::native::SpawnRestriction`] and
    /// [`crate::native::EntityAttributes`] blocks, data components and
    /// attachments their codec flags, menus their title key and slot count,
    /// and block entities their vanilla template, ticker and valid blocks.
    /// Custom registry kinds stay outside this vocabulary until Horizon has a
    /// typed adapter for them.
    pub fn from_native(
        id: impl Into<String>,
        declarations: crate::native::NativeDeclarations,
    ) -> Result<Self, String> {
        let id = id.into();
        let check_id = |kind: &str, value: &str| {
            native_id(value).then_some(()).ok_or_else(|| {
                format!("native {kind} declaration {value:?} is outside a mod namespace")
            })
        };
        let mut block_items = BTreeSet::new();
        let mut block_ids = BTreeSet::new();
        for block in &declarations.blocks {
            check_id("block", &block.id)?;
            if !block_ids.insert(block.id.clone()) {
                return Err(format!("duplicate native block {}", block.id));
            }
            if block.item {
                block_items.insert(block.id.clone());
            }
        }
        let mut item_ids = BTreeSet::new();
        for item in &declarations.items {
            check_id("item", &item.id)?;
            if !item_ids.insert(item.id.clone()) {
                return Err(format!("duplicate native item {}", item.id));
            }
            if block_items.contains(&item.id) {
                return Err(format!(
                    "native item {} conflicts with a block item",
                    item.id
                ));
            }
        }
        let check_unique = |kind: &str, ids: Vec<&str>| -> Result<(), String> {
            let mut seen = BTreeSet::new();
            for value in ids {
                check_id(kind, value)?;
                if !seen.insert(value) {
                    return Err(format!("duplicate native {kind} {value}"));
                }
            }
            Ok(())
        };
        check_unique(
            "mob effect",
            declarations
                .effects
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "entity",
            declarations
                .entities
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "data component",
            declarations
                .data_components
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "attachment",
            declarations
                .attachments
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "menu",
            declarations
                .menus
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "block entity",
            declarations
                .block_entities
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "recipe",
            declarations
                .recipes
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        check_unique(
            "ingredient",
            declarations
                .ingredients
                .iter()
                .map(|row| row.id.as_str())
                .collect(),
        )?;
        // A codec id is a resource location as well, but vanilla codecs
        // (`minecraft:int`) are legitimate, so this takes vanilla ids too.
        let check_codec = |kind: &str, value: &str| {
            (vanilla_id(value) || native_id(value))
                .then_some(())
                .ok_or_else(|| {
                    format!("native {kind} codec {value:?} is not a namespaced codec id")
                })
        };
        for component in &declarations.data_components {
            check_codec("data component", &component.codec)?;
        }
        for attachment in &declarations.attachments {
            check_codec("attachment", &attachment.codec)?;
        }
        // A menu or block-entity row may also be built by hand rather than
        // parsed by the registry sink; re-check the shape `from_native` emits.
        for menu in &declarations.menus {
            menu.validate()?;
        }
        for block_entity in &declarations.block_entities {
            block_entity.validate()?;
        }
        if !declarations.custom.is_empty() {
            let mut details = declarations
                .custom
                .iter()
                .take(8)
                .map(|custom| format!("{}/{} ({})", custom.registry, custom.id, custom.kind))
                .collect::<Vec<_>>();
            if declarations.custom.len() > details.len() {
                details.push("...".to_owned());
            }
            return Err(format!(
                "native mod {id} has {} unsupported custom registrations: {}",
                declarations.custom.len(),
                details.join(", ")
            ));
        }
        let mut namespaces = Vec::new();
        let mut add_namespace = |value: &str| {
            let namespace = value
                .split_once(':')
                .map_or(value, |(namespace, _)| namespace);
            if !namespaces.iter().any(|owned| owned == namespace) {
                namespaces.push(namespace.to_owned());
            }
        };
        add_namespace(&id);
        for block in &declarations.blocks {
            add_namespace(&block.id);
        }
        for item in &declarations.items {
            add_namespace(&item.id);
        }
        for effect in &declarations.effects {
            add_namespace(&effect.id);
        }
        for entity in &declarations.entities {
            add_namespace(&entity.id);
        }
        for component in &declarations.data_components {
            add_namespace(&component.id);
        }
        for attachment in &declarations.attachments {
            add_namespace(&attachment.id);
        }
        for menu in &declarations.menus {
            add_namespace(&menu.id);
        }
        for block_entity in &declarations.block_entities {
            add_namespace(&block_entity.id);
        }
        let mut native_files = BTreeMap::new();
        let mut native_total = 0u64;
        let mut add_native_file = |file: String, bytes: Vec<u8>| -> Result<(), String> {
            check_path(&file)?;
            if bytes.len() as u64 > MAX_FILE {
                return Err(format!("{file} is over {} MiB", MAX_FILE >> 20));
            }
            if native_files.contains_key(&file) {
                return Err(format!("native declarations collide at {file}"));
            }
            if native_files.len() >= MAX_FILES {
                return Err(format!("more than {MAX_FILES} native content files"));
            }
            native_total = native_total.saturating_add(bytes.len() as u64);
            if native_total > MAX_TOTAL {
                return Err(format!(
                    "native declaration data is over {} MiB",
                    MAX_TOTAL >> 20
                ));
            }
            native_files.insert(file, bytes);
            Ok(())
        };
        for recipe in &declarations.recipes {
            add_namespace(&recipe.id);
            let (namespace, path) = recipe
                .id
                .split_once(':')
                .ok_or_else(|| format!("native recipe {} has no namespace", recipe.id))?;
            let file = format!("data/{namespace}/recipe/{path}.json");
            add_native_file(file, recipe.json.clone())?;
        }
        for ingredient in &declarations.ingredients {
            add_namespace(&ingredient.id);
            let (namespace, path) = ingredient
                .id
                .split_once(':')
                .ok_or_else(|| format!("native ingredient {} has no namespace", ingredient.id))?;
            let file = format!("data/{namespace}/ingredient/{path}.json");
            add_native_file(file, ingredient.json.clone())?;
        }
        // Blocks, items and mob effects are written in the shapes the packaged
        // path parses (`BlockFile`, `ItemFile`, `MobEffect`), so the same root
        // reads back. Those shapes reject unknown keys and so carry no marker.
        for block in &declarations.blocks {
            let (namespace, path) = block
                .id
                .split_once(':')
                .ok_or_else(|| format!("native block {} has no namespace", block.id))?;
            let file = format!("data/{namespace}/horizon/block/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "template": block.template,
                "properties": properties_json(&block.properties),
                "shapes": block.shapes.as_ref().map(|shapes| serde_json::json!({
                    "property": shapes.property,
                    "values": shapes.values,
                })),
                "default_state": block.default_state,
                "item": block.item,
                "components": block.components,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!("native block {} cannot be encoded: {error}", block.id)
                })?,
            )?;
        }
        for item in &declarations.items {
            let (namespace, path) = item
                .id
                .split_once(':')
                .ok_or_else(|| format!("native item {} has no namespace", item.id))?;
            let file = format!("data/{namespace}/horizon/item/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "template": item.template,
                "max_stack_size": item.max_stack_size,
                "rarity": item.rarity.map(rarity_name),
                "components": item.components,
                "crafting_remainder": item.crafting_remainder,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!("native item {} cannot be encoded: {error}", item.id)
                })?,
            )?;
        }
        for effect in &declarations.effects {
            let (namespace, path) = effect
                .id
                .split_once(':')
                .ok_or_else(|| format!("native mob effect {} has no namespace", effect.id))?;
            let file = format!("data/{namespace}/horizon/mob_effect/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "category": category_name(effect.category),
                "color": effect.color,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!("native mob effect {} cannot be encoded: {error}", effect.id)
                })?,
            )?;
        }
        for entity in &declarations.entities {
            let (namespace, path) = entity
                .id
                .split_once(':')
                .ok_or_else(|| format!("native entity {} has no namespace", entity.id))?;
            let file = format!("data/{namespace}/horizon/entity/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "format": NATIVE_FORMAT,
                "template": entity.template,
                "properties": entity.properties,
                "spawn": entity.spawn,
                "attributes": entity.attributes,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!("native entity {} cannot be encoded: {error}", entity.id)
                })?,
            )?;
        }
        for component in &declarations.data_components {
            let (namespace, path) = component.id.split_once(':').ok_or_else(|| {
                format!("native data component {} has no namespace", component.id)
            })?;
            let file = format!("data/{namespace}/horizon/data_component/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "format": NATIVE_FORMAT,
                "codec": component.codec,
                "persistent": component.persistent,
                "network_synced": component.network_synced,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!(
                        "native data component {} cannot be encoded: {error}",
                        component.id
                    )
                })?,
            )?;
        }
        for attachment in &declarations.attachments {
            let (namespace, path) = attachment
                .id
                .split_once(':')
                .ok_or_else(|| format!("native attachment {} has no namespace", attachment.id))?;
            let file = format!("data/{namespace}/horizon/attachment/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "format": NATIVE_FORMAT,
                "codec": attachment.codec,
                "default": attachment.default_value,
                "copy_on_death": attachment.copy_on_death,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!(
                        "native attachment {} cannot be encoded: {error}",
                        attachment.id
                    )
                })?,
            )?;
        }
        for menu in &declarations.menus {
            let (namespace, path) = menu
                .id
                .split_once(':')
                .ok_or_else(|| format!("native menu {} has no namespace", menu.id))?;
            let file = format!("data/{namespace}/horizon/menu/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "format": NATIVE_FORMAT,
                "title_key": menu.title_key,
                "slots": menu.slots,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!("native menu {} cannot be encoded: {error}", menu.id)
                })?,
            )?;
        }
        for block_entity in &declarations.block_entities {
            let (namespace, path) = block_entity.id.split_once(':').ok_or_else(|| {
                format!("native block entity {} has no namespace", block_entity.id)
            })?;
            let file = format!("data/{namespace}/horizon/block_entity/{path}.json");
            check_path(&file)?;
            let json = serde_json::json!({
                "format": NATIVE_FORMAT,
                "template": block_entity.template,
                "ticker": block_entity.ticker,
                "valid_blocks": block_entity.valid_blocks,
            });
            add_native_file(
                file,
                serde_json::to_vec(&json).map_err(|error| {
                    format!(
                        "native block entity {} cannot be encoded: {error}",
                        block_entity.id
                    )
                })?,
            )?;
        }
        Ok(Self {
            source: Source::Dir(PathBuf::new()),
            files: Vec::new(),
            blocks: declarations.blocks,
            items: declarations.items,
            effects: declarations.effects,
            entities: declarations.entities,
            data_components: declarations.data_components,
            attachments: declarations.attachments,
            menus: declarations.menus,
            block_entities: declarations.block_entities,
            native_files,
            namespaces,
        })
    }

    /// One content file's bytes.
    pub fn file(&self, name: &str) -> Result<Vec<u8>, String> {
        self.reader()?.read(name)
    }

    /// One content file as typed JSON, naming it in a parse error.
    fn read_typed<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T, String> {
        let bytes = self.file(name)?;
        serde_json::from_slice(&bytes).map_err(|e| format!("{name}: {e}"))
    }

    /// A reader of its content files. A packaged mod's archive is opened
    /// once for all of them (opening it reads its whole index).
    pub fn reader(&self) -> Result<FileReader<'_>, String> {
        Ok(match &self.source {
            Source::Dir(root) => FileReader::Dir(root),
            Source::Zip(path) => FileReader::Zip(Box::new(open_zip(path)?)),
        })
    }

    /// Where content may go, and the declarations.
    fn check(&mut self, manifest: &Manifest) -> Result<(), String> {
        let id = manifest.id.as_str();
        if !self.files.is_empty() && !manifest.capabilities.contains(&Capability::Registries) {
            return Err(format!(
                "has assets/ or data/ but not capability registries in {MANIFEST}"
            ));
        }
        let mut blocks = Vec::new();
        let mut items = Vec::new();
        let mut effects = Vec::new();
        let mut entity_files = Vec::new();
        let mut component_files = Vec::new();
        let mut attachment_files = Vec::new();
        let mut menu_files = Vec::new();
        let mut block_entity_files = Vec::new();
        for name in &self.files {
            let mut parts = name.splitn(3, '/');
            let (top, namespace, rest) = (
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
                parts.next().unwrap_or(""),
            );
            let own = namespace == id;
            let owned = own || manifest.namespaces.iter().any(|n| n == namespace);
            // Vanilla's own files stay vanilla's; a mod may only add to its
            // tags, and to the convention tags mods share.
            let shared = matches!(namespace, "minecraft" | "c");
            let allowed = owned || (top == "data" && shared && rest.starts_with("tags/"));
            if !allowed {
                return Err(format!(
                    "{name}: a mod adds files in its own namespaces ({top}/{id}/ and those listed in namespaces) or to vanilla and convention (c) tags"
                ));
            }
            if !own && top == "data" && rest.starts_with("horizon/") {
                return Err(format!(
                    "{name}: declarations live in data/{id}/horizon/ (block, item, mob_effect, entity, data_component, attachment, menu, block_entity)"
                ));
            }
            let Some(decl) = (own && top == "data")
                .then(|| rest.strip_prefix("horizon/"))
                .flatten()
            else {
                continue;
            };
            let parse = |kind: &str| -> Option<String> {
                let path = decl
                    .strip_prefix(kind)?
                    .strip_prefix('/')?
                    .strip_suffix(".json")?;
                Some(format!("{id}:{path}"))
            };
            if let Some(block) = parse("block") {
                blocks.push((block, name.clone()));
            } else if let Some(item) = parse("item") {
                items.push((item, name.clone()));
            } else if let Some(effect) = parse("mob_effect") {
                effects.push((effect, name.clone()));
            } else if let Some(entity) = parse("entity") {
                entity_files.push((entity, name.clone()));
            } else if let Some(component) = parse("data_component") {
                component_files.push((component, name.clone()));
            } else if let Some(attachment) = parse("attachment") {
                attachment_files.push((attachment, name.clone()));
            } else if let Some(menu) = parse("menu") {
                menu_files.push((menu, name.clone()));
            } else if let Some(block_entity) = parse("block_entity") {
                block_entity_files.push((block_entity, name.clone()));
            } else {
                return Err(format!(
                    "{name}: data/{id}/horizon/ holds block/, item/, mob_effect/, entity/, data_component/, attachment/, menu/ and block_entity/*.json only"
                ));
            }
        }
        let mut ids = BTreeSet::new();
        for (block, name) in blocks {
            let file: BlockFile =
                serde_json::from_slice(&self.file(&name)?).map_err(|e| format!("{name}: {e}"))?;
            check_block(&file).map_err(|e| format!("{name}: {e}"))?;
            if file.item {
                ids.insert(block.clone());
            }
            self.blocks.push(Block {
                id: block,
                template: file.template,
                properties: file.properties,
                shapes: file.shapes,
                default_state: file.default_state,
                item: file.item,
                components: file.components,
            });
        }
        let mut remainders = Vec::new();
        for (item, name) in items {
            let file: ItemFile =
                serde_json::from_slice(&self.file(&name)?).map_err(|e| format!("{name}: {e}"))?;
            if !vanilla_id(&file.template) {
                return Err(format!(
                    "{name}: template {:?} is not a vanilla item (minecraft:...)",
                    file.template
                ));
            }
            if file.max_stack_size.is_some_and(|n| !(1..=99).contains(&n)) {
                return Err(format!("{name}: max_stack_size is 1 to 99"));
            }
            check_components(&file.components, &["max_stack_size", "rarity"])
                .map_err(|e| format!("{name}: {e}"))?;
            if !ids.insert(item.clone()) {
                return Err(format!("{name}: {item} is also a block's item"));
            }
            if let Some(rest) = &file.crafting_remainder {
                remainders.push((name, rest.clone()));
            }
            self.items.push(Item {
                id: item,
                template: file.template,
                max_stack_size: file.max_stack_size,
                rarity: file.rarity,
                components: file.components,
                crafting_remainder: file.crafting_remainder,
            });
        }
        for (effect, name) in effects {
            let file: MobEffect =
                serde_json::from_slice(&self.file(&name)?).map_err(|e| format!("{name}: {e}"))?;
            if file.color > 0xFF_FFFF {
                return Err(format!("{name}: color is RGB, 0 to 16777215 (0xFFFFFF)"));
            }
            self.effects.push(MobEffect { id: effect, ..file });
        }
        let mut entities = BTreeMap::new();
        for (id, name) in entity_files {
            let file: EntityFile = self.read_typed(&name)?;
            check_format(&name, file.format)?;
            let entity = entity_row(id, file).map_err(|e| format!("{name}: {e}"))?;
            push_row(&mut entities, entity.id.clone(), &name, entity)?;
        }
        let mut data_components = BTreeMap::new();
        for (id, name) in component_files {
            let file: DataComponentFile = self.read_typed(&name)?;
            check_format(&name, file.format)?;
            NativeDeclarations::ensure_codec("data component", &id, &file.codec)
                .map_err(|e| format!("{name}: {e}"))?;
            push_row(
                &mut data_components,
                id.clone(),
                &name,
                DataComponentDeclaration {
                    id,
                    codec: file.codec,
                    persistent: file.persistent,
                    network_synced: file.network_synced,
                },
            )?;
        }
        let mut attachments = BTreeMap::new();
        for (id, name) in attachment_files {
            let file: AttachmentFile = self.read_typed(&name)?;
            check_format(&name, file.format)?;
            NativeDeclarations::ensure_codec("attachment", &id, &file.codec)
                .map_err(|e| format!("{name}: {e}"))?;
            if file.default.len() > MAX_NATIVE_TEXT {
                return Err(format!(
                    "{name}: default is at most {MAX_NATIVE_TEXT} bytes"
                ));
            }
            if file.default.chars().any(char::is_control) {
                return Err(format!("{name}: default has control characters"));
            }
            push_row(
                &mut attachments,
                id.clone(),
                &name,
                AttachmentDeclaration {
                    id,
                    codec: file.codec,
                    default_value: file.default,
                    copy_on_death: file.copy_on_death,
                },
            )?;
        }
        let mut menus = BTreeMap::new();
        for (id, name) in menu_files {
            let file: MenuFile = self.read_typed(&name)?;
            check_format(&name, file.format)?;
            let menu = MenuDeclaration {
                id,
                title_key: file.title_key,
                slots: file.slots,
            };
            menu.validate().map_err(|e| format!("{name}: {e}"))?;
            push_row(&mut menus, menu.id.clone(), &name, menu)?;
        }
        let mut block_entities = BTreeMap::new();
        for (id, name) in block_entity_files {
            let file: BlockEntityFile = self.read_typed(&name)?;
            check_format(&name, file.format)?;
            let mut block_entity = BlockEntityDeclaration {
                id,
                template: file.template,
                ticker: file.ticker,
                valid_blocks: file.valid_blocks,
            };
            // The native sink sorts the block list; keep the row canonical.
            block_entity.valid_blocks.sort();
            block_entity
                .validate()
                .map_err(|e| format!("{name}: {e}"))?;
            push_row(
                &mut block_entities,
                block_entity.id.clone(),
                &name,
                block_entity,
            )?;
        }
        // Sorted by id, so file order cannot change the declaration tables.
        self.entities = entities.into_values().map(|(_, row)| row).collect();
        self.data_components = data_components.into_values().map(|(_, row)| row).collect();
        self.attachments = attachments.into_values().map(|(_, row)| row).collect();
        self.menus = menus.into_values().map(|(_, row)| row).collect();
        self.block_entities = block_entities.into_values().map(|(_, row)| row).collect();
        // After every item, so a remainder may be declared in a later file.
        for (name, rest) in remainders {
            if rest == "minecraft:air" {
                return Err(format!(
                    "{name}: crafting_remainder {rest:?} is no remainder (leave it out)"
                ));
            }
            if !vanilla_id(&rest) && !ids.contains(&rest) {
                return Err(format!(
                    "{name}: crafting_remainder {rest:?} is not a vanilla item (minecraft:...) or one of this mod's"
                ));
            }
        }
        Ok(())
    }
}

/// A declaration file's [`NATIVE_FORMAT`] marker. Absent counts as 1: a file
/// written before the marker existed is still the same shape.
fn check_format(name: &str, format: Option<u32>) -> Result<(), String> {
    match format {
        None | Some(NATIVE_FORMAT) => Ok(()),
        Some(other) => Err(format!(
            "{name}: format {other} is not the supported {NATIVE_FORMAT}"
        )),
    }
}

/// One entity declaration file, rebuilt through the parser a loader adapter's
/// row goes through. `spawn` and `attributes` are the writer's typed view of
/// `properties`; a file where they disagree is refused rather than read as a
/// half-declared entity.
fn entity_row(id: String, file: EntityFile) -> Result<EntityDeclaration, String> {
    if !vanilla_id(&file.template) {
        return Err(format!(
            "template {:?} is not a vanilla entity (minecraft:...)",
            file.template
        ));
    }
    let mut properties = file.properties;
    if let Some(template) = properties.remove("template")
        && template != file.template
    {
        return Err(format!(
            "properties.template {template:?} disagrees with template {:?}",
            file.template
        ));
    }
    let entity = EntityDeclaration::from_registry_properties(id, file.template, &properties)?;
    let spawn = serde_json::to_value(&entity.spawn).map_err(|e| e.to_string())?;
    let attributes = serde_json::to_value(&entity.attributes).map_err(|e| e.to_string())?;
    if file.spawn.as_ref().is_some_and(|value| value != &spawn) {
        return Err("spawn disagrees with properties".into());
    }
    if file
        .attributes
        .as_ref()
        .is_some_and(|value| value != &attributes)
    {
        return Err("attributes disagree with properties".into());
    }
    Ok(entity)
}

/// One declaration row, stored under its id so the table comes out sorted and
/// a second file claiming the same id is refused, naming both files.
fn push_row<T>(
    rows: &mut BTreeMap<String, (String, T)>,
    id: String,
    file: &str,
    row: T,
) -> Result<(), String> {
    if let Some((first, _)) = rows.insert(id.clone(), (file.to_owned(), row)) {
        return Err(format!("{file}: {id} is also declared in {first}"));
    }
    Ok(())
}

/// Vanilla component types only, removals as `{}`, and none of `fields`,
/// which the file sets with its own keys.
fn check_components(
    components: &BTreeMap<String, serde_json::Value>,
    fields: &[&str],
) -> Result<(), String> {
    for (key, value) in components {
        let (removed, kind) = match key.strip_prefix('!') {
            Some(kind) => (true, kind),
            None => (false, key.as_str()),
        };
        if !vanilla_id(kind) {
            return Err(format!(
                "component {key:?} is not a vanilla type (minecraft:...)"
            ));
        }
        if let Some(own) = kind
            .strip_prefix("minecraft:")
            .filter(|k| fields.contains(k))
        {
            return Err(format!("component {key:?}: set {own} instead"));
        }
        if removed && !value.as_object().is_some_and(|o| o.is_empty()) {
            return Err(format!("component {key:?}: a removal's value is {{}}"));
        }
    }
    Ok(())
}

/// A block's `properties` object as the reader parses it: only the values the
/// declaration changes, since an unset field keeps the template's.
fn properties_json(properties: &BlockProperties) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let mut put = |key: &str, value: Option<serde_json::Value>| {
        if let Some(value) = value {
            out.insert(key.to_owned(), value);
        }
    };
    put("hardness", properties.hardness.map(serde_json::Value::from));
    put(
        "explosion_resistance",
        properties.explosion_resistance.map(serde_json::Value::from),
    );
    put("light", properties.light.map(serde_json::Value::from));
    put(
        "requires_tool",
        properties.requires_tool.map(serde_json::Value::from),
    );
    put("friction", properties.friction.map(serde_json::Value::from));
    put(
        "map_color",
        properties.map_color.map(serde_json::Value::from),
    );
    put(
        "sounds",
        properties.sounds.as_deref().map(serde_json::Value::from),
    );
    serde_json::Value::Object(out)
}

/// [`Rarity`]'s file name, which [`ItemFile`] reads back.
fn rarity_name(rarity: Rarity) -> &'static str {
    match rarity {
        Rarity::Common => "common",
        Rarity::Uncommon => "uncommon",
        Rarity::Rare => "rare",
        Rarity::Epic => "epic",
    }
}

/// [`EffectCategory`]'s file name, which [`MobEffect`] reads back.
fn category_name(category: EffectCategory) -> &'static str {
    match category {
        EffectCategory::Beneficial => "beneficial",
        EffectCategory::Harmful => "harmful",
        EffectCategory::Neutral => "neutral",
    }
}

fn check_block(file: &BlockFile) -> Result<(), String> {
    if !vanilla_id(&file.template) {
        return Err(format!(
            "template {:?} is not a vanilla block (minecraft:...)",
            file.template
        ));
    }
    if let Some(shapes) = &file.shapes {
        if shapes.property.is_empty() || shapes.values.is_empty() {
            return Err("shapes needs a property and values".into());
        }
        for boxes in shapes.values.values() {
            if boxes.len() > 16
                || boxes
                    .iter()
                    .any(|b| b.iter().any(|n| !n.is_finite()) || (0..3).any(|i| b[i] > b[i + 3]))
            {
                return Err("shape needs at most 16 finite boxes with ordered bounds".into());
            }
        }
    }
    let p = &file.properties;
    let finite = |v: Option<f32>| v.is_none_or(f32::is_finite);
    if !finite(p.hardness) || !finite(p.explosion_resistance) || !finite(p.friction) {
        return Err("hardness, explosion_resistance and friction are numbers".into());
    }
    if p.hardness.is_some_and(|h| h < 0.0 && h != -1.0) {
        return Err("hardness is 0 or more, or -1 (unbreakable)".into());
    }
    if p.explosion_resistance.is_some_and(|r| r < 0.0) {
        return Err("explosion_resistance is 0 or more".into());
    }
    if p.friction.is_some_and(|f| !(0.0..=2.0).contains(&f)) {
        return Err("friction is 0 to 2".into());
    }
    if p.light.is_some_and(|l| l > 15) {
        return Err("light is 0 to 15".into());
    }
    if p.map_color.is_some_and(|c| c > 61) {
        return Err("map_color is a MapColor id, 0 to 61".into());
    }
    if p.sounds.as_deref().is_some_and(|s| !vanilla_id(s)) {
        return Err("sounds names a vanilla block (minecraft:...)".into());
    }
    if !file.item && !file.components.is_empty() {
        return Err("item is false, so the block has no item to take components".into());
    }
    check_components(&file.components, &[])?;
    // A block file has no `max_stack_size` / `rarity` keys, so those two take the same checks here.
    if let Some(n) = file.components.get("minecraft:max_stack_size")
        && !n.as_u64().is_some_and(|n| (1..=99).contains(&n))
    {
        return Err("component \"minecraft:max_stack_size\" is an integer 1 to 99".into());
    }
    if let Some(r) = file.components.get("minecraft:rarity")
        && serde_json::from_value::<Rarity>(r.clone()).is_err()
    {
        return Err("component \"minecraft:rarity\" is common, uncommon, rare or epic".into());
    }
    Ok(())
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(file).map_err(|e| e.to_string())
}

/// Files under `dir`, relative to `root` with `/` separators. Symbolic links
/// are not followed: a mod folder holds its own files.
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) -> Result<(), String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in read.flatten() {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let parts: Option<Vec<&str>> = rel.iter().map(|p| p.to_str()).collect();
            let name = parts.ok_or_else(|| format!("{} is not UTF-8", rel.display()))?;
            out.push((name.join("/"), meta.len()));
        }
        if out.len() > MAX_FILES {
            return Err(format!("more than {MAX_FILES} content files"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Package;
    use crate::native::{
        AttachmentDeclaration, BlockEntityDeclaration, DataComponentDeclaration, EntityDeclaration,
        MenuDeclaration, NativeDeclarations,
    };
    use horizon_native_java::{RegistryDeclaration, RegistryEntry};

    const MANIFEST_TEXT: &str = r#"
id = "ruby"
name = "Ruby"
version = "0.1.0"
api = "0.2"
side = "both"
capabilities = ["registries"]
"#;
    const BLOCK: &[u8] = br#"{"template": "minecraft:iron_block",
        "properties": {"hardness": 4.0, "light": 3, "sounds": "minecraft:amethyst_block"}}"#;
    const ITEM: &[u8] = br#"{"template": "minecraft:diamond", "rarity": "uncommon"}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("horizon-content-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    /// A typed custom row as a loader adapter emits it.
    fn native_row(kind: &str, id: &str, properties: &[(&str, &str)]) -> RegistryEntry {
        let registry = match kind {
            "entity" => "minecraft:entity_type",
            "data_component" => "minecraft:data_component_type",
            "menu" => "minecraft:menu",
            "block_entity" => "minecraft:block_entity_type",
            _ => "neoforge:attachment",
        };
        RegistryEntry::typed(
            registry,
            id,
            RegistryDeclaration::Custom {
                kind: kind.into(),
                properties: properties
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            },
        )
    }
    /// One native JSON file from a planned content.
    fn native_json(content: &Content, file: &str) -> serde_json::Value {
        serde_json::from_slice(content.native_files.get(file).unwrap()).unwrap()
    }
    fn hmod(name: &str, manifest: &str, files: &[(&str, &[u8])]) -> Result<Content, String> {
        let dir = scratch(name);
        let path = dir.join("m.hmod");
        std::fs::write(&path, Package::write_files(manifest, None, files)?).unwrap();
        let package = Package::read(&path);
        let content = package.and_then(|p| Content::read(&path, &p.manifest));
        std::fs::remove_dir_all(&dir).unwrap();
        content
    }

    #[test]
    fn content_only_mod_round_trips() {
        let files: &[(&str, &[u8])] = &[
            ("data/ruby/horizon/block/ruby_block.json", BLOCK),
            ("data/ruby/horizon/item/ruby.json", ITEM),
            ("data/ruby/recipe/ruby_block.json", b"{}"),
            ("data/minecraft/tags/block/mineable/pickaxe.json", b"{}"),
            ("data/c/tags/item/gems/ruby.json", b"{}"),
            ("assets/ruby/textures/block/ruby_block.png", b"png"),
        ];
        let content = hmod("ok", MANIFEST_TEXT, files).unwrap();
        assert_eq!(content.files.len(), 6);
        assert_eq!(
            content.blocks,
            [Block {
                id: "ruby:ruby_block".into(),
                template: "minecraft:iron_block".into(),
                shapes: None,
                default_state: BTreeMap::new(),
                properties: BlockProperties {
                    hardness: Some(4.0),
                    light: Some(3),
                    sounds: Some("minecraft:amethyst_block".into()),
                    ..BlockProperties::default()
                },
                item: true,
                components: BTreeMap::new(),
            }]
        );
        assert_eq!(content.items[0].id, "ruby:ruby");
        assert_eq!(content.items[0].rarity, Some(Rarity::Uncommon));
        assert!(content.items[0].components.is_empty());
        assert_eq!(content.items[0].crafting_remainder, None);
    }

    #[test]
    fn native_custom_registration_diagnostic_names_entity_kind() {
        let mut declarations = crate::native::NativeDeclarations::default();
        declarations.custom.push(crate::native::CustomDeclaration {
            registry: "minecraft:entity_type".into(),
            id: "ruby:bird".into(),
            kind: "entity".into(),
            properties: BTreeMap::new(),
        });
        let error = Content::from_native("ruby", declarations).unwrap_err();
        assert!(
            error.contains("minecraft:entity_type/ruby:bird (entity)"),
            "{error}"
        );
    }

    #[test]
    fn items_name_their_crafting_remainder() {
        let files: &[(&str, &[u8])] = &[
            (
                "data/ruby/horizon/item/a_stew.json",
                br#"{"template": "minecraft:mushroom_stew", "crafting_remainder": "minecraft:bowl"}"#,
            ),
            (
                "data/ruby/horizon/item/b_flask.json",
                br#"{"template": "minecraft:potion", "crafting_remainder": "ruby:c_flask"}"#,
            ),
            ("data/ruby/horizon/item/c_flask.json", ITEM),
        ];
        let content = hmod("remainder", MANIFEST_TEXT, files).unwrap();
        let rests: Vec<_> = content
            .items
            .iter()
            .map(|i| i.crafting_remainder.as_deref())
            .collect();
        assert_eq!(rests, [Some("minecraft:bowl"), Some("ruby:c_flask"), None]);
    }

    #[test]
    fn items_patch_their_template_components() {
        let sword = br#"{"template": "minecraft:iron_sword", "components": {
            "minecraft:max_damage": 900,
            "minecraft:attribute_modifiers": [{"type": "minecraft:attack_damage",
                "id": "minecraft:base_attack_damage", "amount": 7.0,
                "operation": "add_value", "slot": "mainhand"}],
            "!minecraft:repairable": {}}}"#;
        let files: &[(&str, &[u8])] = &[("data/ruby/horizon/item/ruby_sword.json", sword)];
        let content = hmod("components", MANIFEST_TEXT, files).unwrap();
        let components = &content.items[0].components;
        assert_eq!(
            components.keys().collect::<Vec<_>>(),
            [
                "!minecraft:repairable",
                "minecraft:attribute_modifiers",
                "minecraft:max_damage"
            ]
        );
        assert_eq!(components["minecraft:max_damage"], 900);
    }

    #[test]
    fn mob_effects_take_a_category_and_a_colour() {
        let files: &[(&str, &[u8])] = &[
            (
                "data/ruby/horizon/mob_effect/glow.json",
                br#"{"category": "beneficial", "color": 16711680}"#,
            ),
            (
                "data/ruby/horizon/mob_effect/curse/dark.json",
                br#"{"category": "harmful", "color": 0}"#,
            ),
        ];
        let content = hmod("effects", MANIFEST_TEXT, files).unwrap();
        let effects: Vec<_> = content
            .effects
            .iter()
            .map(|e| (e.id.as_str(), e.category, e.color))
            .collect();
        assert_eq!(
            effects,
            [
                ("ruby:curse/dark", EffectCategory::Harmful, 0),
                ("ruby:glow", EffectCategory::Beneficial, 0xFF0000),
            ]
        );
        for (case, file, why) in [
            (
                "effect-colour",
                r#"{"category": "neutral", "color": 16777216}"#,
                "color is RGB",
            ),
            (
                "effect-category",
                r#"{"category": "good", "color": 1}"#,
                "unknown variant",
            ),
            (
                "effect-key",
                r#"{"category": "neutral", "color": 1, "id": "x"}"#,
                "unknown field",
            ),
            (
                "effect-missing",
                r#"{"category": "neutral"}"#,
                "missing field",
            ),
        ] {
            let files: &[(&str, &[u8])] =
                &[("data/ruby/horizon/mob_effect/a.json", file.as_bytes())];
            let e = hmod(case, MANIFEST_TEXT, files).unwrap_err();
            assert!(e.contains(why), "{case}: {e}");
        }
    }

    #[test]
    fn block_items_patch_their_components() {
        let block = br#"{"template": "minecraft:iron_block", "components": {
            "minecraft:max_stack_size": 16, "!minecraft:lore": {}}}"#;
        let files: &[(&str, &[u8])] = &[("data/ruby/horizon/block/ruby_block.json", block)];
        let content = hmod("block-components", MANIFEST_TEXT, files).unwrap();
        let components = &content.blocks[0].components;
        assert_eq!(
            components.keys().collect::<Vec<_>>(),
            ["!minecraft:lore", "minecraft:max_stack_size"]
        );
        assert_eq!(components["minecraft:max_stack_size"], 16);
    }

    #[test]
    fn folder_mod_reads_the_same() {
        let dir = scratch("folder");
        std::fs::write(dir.join(MANIFEST), MANIFEST_TEXT).unwrap();
        let block = dir.join("data/ruby/horizon/block");
        std::fs::create_dir_all(&block).unwrap();
        std::fs::write(block.join("ruby_block.json"), BLOCK).unwrap();
        let package = Package::read(&dir).unwrap();
        assert!(package.wasm.is_none());
        let content = Content::read(&dir, &package.manifest).unwrap();
        assert_eq!(content.files, ["data/ruby/horizon/block/ruby_block.json"]);
        assert_eq!(content.file(&content.files[0]).unwrap(), BLOCK);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_content_outside_its_fence() {
        let refused = |name: &str, files: &[(&str, &[u8])]| {
            let e = hmod(name, MANIFEST_TEXT, files).unwrap_err();
            assert!(!e.is_empty());
            e
        };
        let e = refused("other-ns", &[("data/other/horizon/block/a.json", BLOCK)]);
        assert!(e.contains("own namespaces"), "{e}");
        refused(
            "vanilla-asset",
            &[("assets/minecraft/textures/block/stone.png", b"x")],
        );
        refused(
            "vanilla-recipe",
            &[("data/minecraft/recipe/stone.json", b"{}")],
        );
        refused("convention-recipe", &[("data/c/recipe/stone.json", b"{}")]);
        refused("convention-asset", &[("assets/c/lang/en_us.json", b"{}")]);
        let e = refused(
            "modded-template",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "ruby:other"}"#,
            )],
        );
        assert!(e.contains("not a vanilla block"), "{e}");
        refused(
            "light",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "minecraft:stone", "properties": {"light": 16}}"#,
            )],
        );
        refused(
            "unknown-key",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template": "minecraft:stone", "glow": true}"#,
            )],
        );
        refused("shape-reversed", &[("data/ruby/horizon/block/a.json", br#"{"template":"minecraft:stone","shapes":{"property":"axis","values":{"x":[[1,0,0,0,1,1]]}}}"#)]);
        refused("shape-malformed", &[("data/ruby/horizon/block/a.json", br#"{"template":"minecraft:stone","shapes":{"property":"axis","values":{"x":[[0,0,0,1,1]]}}}"#)]);
        refused(
            "shape-no-values",
            &[(
                "data/ruby/horizon/block/a.json",
                br#"{"template":"minecraft:stone","shapes":{"property":"axis","values":{}}}"#,
            )],
        );
        let e = refused(
            "clash",
            &[
                ("data/ruby/horizon/block/a.json", BLOCK),
                ("data/ruby/horizon/item/a.json", ITEM),
            ],
        );
        assert!(e.contains("also a block's item"), "{e}");
        refused("unknown-kind", &[("data/ruby/horizon/fluid/a.json", b"{}")]);
        refused(
            "stack",
            &[(
                "data/ruby/horizon/item/a.json",
                br#"{"template": "minecraft:stick", "max_stack_size": 100}"#,
            )],
        );
        for (case, components, why) in [
            (
                "modded-component",
                r#"{"ruby:mana": 3}"#,
                "not a vanilla type",
            ),
            (
                "bare-component",
                r#"{"max_damage": 3}"#,
                "not a vanilla type",
            ),
            (
                "own-key",
                r#"{"minecraft:rarity": "epic"}"#,
                "set rarity instead",
            ),
            (
                "own-removal",
                r#"{"!minecraft:max_stack_size": {}}"#,
                "set max_stack_size",
            ),
            (
                "removal-value",
                r#"{"!minecraft:food": true}"#,
                "a removal's value is {}",
            ),
        ] {
            let item = format!(r#"{{"template": "minecraft:stick", "components": {components}}}"#);
            let e = refused(case, &[("data/ruby/horizon/item/a.json", item.as_bytes())]);
            assert!(e.contains(why), "{case}: {e}");
        }
        let glass = br#"{"template": "minecraft:glass", "item": false}"#;
        for (case, rest) in [
            ("bare-remainder", "bowl"),
            ("missing-remainder", "ruby:bowl"),
            ("other-mod-remainder", "jade:bowl"),
            ("itemless-remainder", "ruby:glass"),
            ("air-remainder", "minecraft:air"),
        ] {
            let item =
                format!(r#"{{"template": "minecraft:stick", "crafting_remainder": "{rest}"}}"#);
            let files: &[(&str, &[u8])] = &[
                ("data/ruby/horizon/item/a.json", item.as_bytes()),
                ("data/ruby/horizon/block/glass.json", glass),
            ];
            let e = refused(case, files);
            assert!(e.contains("crafting_remainder"), "{case}: {e}");
        }
        for (case, block, why) in [
            (
                "block-modded-component",
                r#"{"template": "minecraft:stone", "components": {"ruby:mana": 3}}"#,
                "not a vanilla type",
            ),
            (
                "block-removal-value",
                r#"{"template": "minecraft:stone", "components": {"!minecraft:lore": []}}"#,
                "a removal's value is {}",
            ),
            (
                "block-stack-zero",
                r#"{"template": "minecraft:stone", "components": {"minecraft:max_stack_size": 0}}"#,
                "integer 1 to 99",
            ),
            (
                "block-stack-100",
                r#"{"template": "minecraft:stone", "components": {"minecraft:max_stack_size": 100}}"#,
                "integer 1 to 99",
            ),
            (
                "block-stack-string",
                r#"{"template": "minecraft:stone", "components": {"minecraft:max_stack_size": "16"}}"#,
                "integer 1 to 99",
            ),
            (
                "block-rarity",
                r#"{"template": "minecraft:stone", "components": {"minecraft:rarity": "bogus"}}"#,
                "common, uncommon",
            ),
            (
                "block-without-item",
                r#"{"template": "minecraft:glass", "item": false,
                    "components": {"minecraft:max_stack_size": 16}}"#,
                "item is false",
            ),
        ] {
            let e = refused(
                case,
                &[("data/ruby/horizon/block/a.json", block.as_bytes())],
            );
            assert!(e.contains(why), "{case}: {e}");
        }
    }

    #[test]
    fn listed_namespaces_hold_content() {
        let manifest = format!("{MANIFEST_TEXT}namespaces = [\"kaisyn\"]\n");
        let files: &[(&str, &[u8])] = &[
            ("data/kaisyn/worldgen/template_pool/a.json", b"{}"),
            ("data/kaisyn/structure/a.nbt", b"nbt"),
            ("assets/kaisyn/lang/en_us.json", b"{}"),
        ];
        let content = hmod("listed", &manifest, files).unwrap();
        assert_eq!(content.files.len(), 3);
        assert_eq!(content.namespaces, ["ruby", "kaisyn"]);
        // Declarations stay in the id's namespace.
        let decl: &[(&str, &[u8])] = &[("data/kaisyn/horizon/block/a.json", BLOCK)];
        let e = hmod("listed-decl", &manifest, decl).unwrap_err();
        assert!(e.contains("live in data/ruby/horizon/"), "{e}");
        // A namespace it does not list is still outside.
        let other: &[(&str, &[u8])] = &[("data/other/structure/a.nbt", b"nbt")];
        assert!(hmod("listed-other", &manifest, other).is_err());
    }

    #[test]
    fn mods_sharing_a_namespace_clash() {
        let t_and_t = ["t_and_t".to_owned(), "kaisyn".to_owned()];
        let kaisyn = ["kaisyn".to_owned()];
        let ruby = ["ruby".to_owned(), "gems".to_owned()];
        assert_eq!(
            clash(("t_and_t", &t_and_t), ("kaisyn", &kaisyn)).as_deref(),
            Some("mods t_and_t and kaisyn both use the namespace kaisyn; turn one of them off")
        );
        assert_eq!(clash(("t_and_t", &t_and_t), ("ruby", &ruby)), None);
    }

    #[test]
    fn content_needs_the_registries_capability() {
        let code_only = MANIFEST_TEXT
            .replace("api = \"0.2\"", "api = \"0.1\"")
            .replace("capabilities = [\"registries\"]", "capabilities = []");
        // No code and no registries: nothing to load.
        let e = hmod("empty", &code_only, &[]).unwrap_err();
        assert!(e.contains("mod.wasm: missing"), "{e}");
        // Registries on API 0.1.
        let old = MANIFEST_TEXT.replace("api = \"0.2\"", "api = \"0.1\"");
        assert!(Manifest::parse(&old).unwrap_err().contains("needs api"));
        // Content without the capability, in a mod with code.
        let dir = scratch("no-cap");
        let path = dir.join("m.hmod");
        let files: &[(&str, &[u8])] = &[("assets/ruby/lang/en_us.json", b"{}")];
        std::fs::write(
            &path,
            Package::write_files(&code_only, Some(b"\0asm"), files).unwrap(),
        )
        .unwrap();
        let package = Package::read(&path).unwrap();
        let e = Content::read(&path, &package.manifest).err().unwrap();
        assert!(e.contains("not capability registries"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn native_entity_rows_carry_typed_spawn_and_attributes() {
        let entry = native_row(
            "entity",
            "ruby:bird",
            &[
                ("template", "minecraft:chicken"),
                ("width", "0.6"),
                ("max_health", "4"),
                ("speed", "0.25"),
                ("spawn_category", "on_ground"),
                ("spawn_heightmap", "motion_blocking"),
                ("spawn_min_height", "-64"),
                ("spawn_max_height", "320"),
            ],
        );
        let declarations = NativeDeclarations::from_registry_entries([&entry]).unwrap();
        assert_eq!(
            declarations.entities[0].spawn.as_ref().unwrap().category,
            "ON_GROUND"
        );
        let content = Content::from_native("ruby", declarations).unwrap();
        let entity = native_json(&content, "data/ruby/horizon/entity/bird.json");
        assert_eq!(entity["format"], NATIVE_FORMAT);
        assert_eq!(entity["template"], "minecraft:chicken");
        assert_eq!(entity["spawn"]["category"], "ON_GROUND");
        assert_eq!(entity["spawn"]["heightmap"], "MOTION_BLOCKING");
        assert_eq!(entity["spawn"]["min_height"], -64);
        assert_eq!(entity["spawn"]["max_height"], 320);
        assert_eq!(entity["attributes"]["health"], 4.0);
        assert_eq!(entity["attributes"]["speed"], 0.25);
        assert!(entity["attributes"]["attack_damage"].is_null());

        // A bare entity has the same shape, with the typed blocks absent.
        let bare = EntityDeclaration::from_registry_properties(
            "ruby:plain",
            "minecraft:chicken",
            &BTreeMap::new(),
        )
        .unwrap();
        let content = Content::from_native(
            "ruby",
            NativeDeclarations {
                entities: vec![bare],
                ..NativeDeclarations::default()
            },
        )
        .unwrap();
        let plain = native_json(&content, "data/ruby/horizon/entity/plain.json");
        assert_eq!(plain["format"], NATIVE_FORMAT);
        assert!(plain["spawn"].is_null());
        assert!(plain["attributes"].is_null());
    }

    #[test]
    fn native_auxiliary_rows_carry_format_and_check_codecs() {
        let component = native_row(
            "data_component",
            "ruby:mana",
            &[("codec", "minecraft:int"), ("network_synced", "true")],
        );
        let attachment = native_row(
            "attachment",
            "ruby:mana",
            &[
                ("codec", "ruby:mana"),
                ("default", "0"),
                ("copy_on_death", "true"),
            ],
        );
        let content = Content::from_native(
            "ruby",
            NativeDeclarations::from_registry_entries([&component, &attachment]).unwrap(),
        )
        .unwrap();
        let component = native_json(&content, "data/ruby/horizon/data_component/mana.json");
        assert_eq!(component["format"], NATIVE_FORMAT);
        assert_eq!(component["codec"], "minecraft:int");
        assert_eq!(component["network_synced"].as_bool(), Some(true));
        let attachment = native_json(&content, "data/ruby/horizon/attachment/mana.json");
        assert_eq!(attachment["format"], NATIVE_FORMAT);
        assert_eq!(attachment["codec"], "ruby:mana");
        assert_eq!(attachment["copy_on_death"].as_bool(), Some(true));

        // A bare codec is outside every namespace, and the batch is refused as
        // a whole: `from_native` never emits a partial plan.
        let declarations = NativeDeclarations {
            data_components: vec![DataComponentDeclaration {
                id: "ruby:mana".into(),
                codec: "mana".into(),
                persistent: true,
                network_synced: false,
            }],
            attachments: vec![AttachmentDeclaration {
                id: "ruby:mana".into(),
                codec: "ruby:mana".into(),
                default_value: "0".into(),
                copy_on_death: false,
            }],
            ..NativeDeclarations::default()
        };
        let error = Content::from_native("ruby", declarations).unwrap_err();
        assert!(error.contains("not a namespaced codec id"), "{error}");
    }

    #[test]
    fn native_menu_and_block_entity_rows_become_planner_files() {
        let menu = native_row(
            "menu",
            "ruby:crate",
            &[("title_key", "container.ruby.crate"), ("slots", "27")],
        );
        let block_entity = native_row(
            "block_entity",
            "ruby:crate",
            &[
                ("template", "minecraft:barrel"),
                ("ticker", "true"),
                ("valid_blocks", "ruby:crate_block,minecraft:barrel"),
            ],
        );
        // A row in another mod's namespace joins the content's namespace list.
        let foreign = native_row(
            "menu",
            "jade:crate",
            &[("title_key", "container.jade"), ("slots", "0")],
        );
        let content = Content::from_native(
            "ruby",
            NativeDeclarations::from_registry_entries([&menu, &block_entity, &foreign]).unwrap(),
        )
        .unwrap();
        assert!(!content.is_empty());
        assert_eq!(content.menus.len(), 2);
        assert_eq!(content.block_entities.len(), 1);
        assert_eq!(content.namespaces, ["ruby", "jade"]);

        let menu = native_json(&content, "data/ruby/horizon/menu/crate.json");
        assert_eq!(menu["format"], NATIVE_FORMAT);
        assert_eq!(menu["title_key"], "container.ruby.crate");
        assert_eq!(menu["slots"], 27);

        let block_entity = native_json(&content, "data/ruby/horizon/block_entity/crate.json");
        assert_eq!(block_entity["format"], NATIVE_FORMAT);
        assert_eq!(block_entity["template"], "minecraft:barrel");
        assert_eq!(block_entity["ticker"].as_bool(), Some(true));
        assert_eq!(
            block_entity["valid_blocks"],
            serde_json::json!(["minecraft:barrel", "ruby:crate_block"])
        );
    }

    #[test]
    fn native_menu_and_block_entity_rows_fence_ids_and_duplicates() {
        let menu = MenuDeclaration {
            id: "ruby:crate".into(),
            title_key: "container.ruby".into(),
            slots: 9,
        };
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                menus: vec![menu.clone(), menu.clone()],
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("duplicate native menu"), "{error}");

        let block_entity = BlockEntityDeclaration {
            id: "ruby:crate".into(),
            template: "minecraft:barrel".into(),
            ticker: false,
            valid_blocks: vec!["ruby:crate_block".into()],
        };
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                block_entities: vec![block_entity.clone(), block_entity.clone()],
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("duplicate native block entity"), "{error}");

        // A vanilla id is not a mod's to define; the whole plan is refused.
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                menus: vec![menu],
                block_entities: vec![BlockEntityDeclaration {
                    id: "minecraft:barrel".into(),
                    ..block_entity
                }],
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("outside a mod namespace"), "{error}");
    }

    #[test]
    fn native_declaration_files_round_trip_through_a_package() {
        let entity = native_row(
            "entity",
            "ruby:bird",
            &[
                ("template", "minecraft:chicken"),
                ("width", "0.6"),
                ("max_health", "4"),
                ("spawn_category", "on_ground"),
                ("spawn_heightmap", "motion_blocking"),
            ],
        );
        let component = native_row(
            "data_component",
            "ruby:mana",
            &[("codec", "minecraft:int"), ("network_synced", "true")],
        );
        let attachment = native_row(
            "attachment",
            "ruby:soul",
            &[
                ("codec", "ruby:mana"),
                ("default", "0"),
                ("copy_on_death", "true"),
            ],
        );
        let menu = native_row(
            "menu",
            "ruby:crate",
            &[("title_key", "container.ruby.crate"), ("slots", "27")],
        );
        let block_entity = native_row(
            "block_entity",
            "ruby:crate",
            &[
                ("template", "minecraft:barrel"),
                ("ticker", "true"),
                ("valid_blocks", "ruby:crate_block,minecraft:barrel"),
            ],
        );
        let written = Content::from_native(
            "ruby",
            NativeDeclarations::from_registry_entries([
                &entity,
                &component,
                &attachment,
                &menu,
                &block_entity,
            ])
            .unwrap(),
        )
        .unwrap();
        assert_eq!(written.native_files.len(), 5);
        // A packaged content root is the shipped form of those files.
        let files: Vec<(&str, &[u8])> = written
            .native_files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        let read = hmod("native-root", MANIFEST_TEXT, &files).unwrap();
        assert_eq!(read.files.len(), 5);
        assert_eq!(read.entities, written.entities);
        assert_eq!(read.data_components, written.data_components);
        assert_eq!(read.attachments, written.attachments);
        assert_eq!(read.menus, written.menus);
        assert_eq!(read.block_entities, written.block_entities);
        assert_eq!(read.namespaces, written.namespaces);
    }

    #[test]
    fn native_block_item_and_effect_rows_round_trip_through_a_package() {
        let block = Block {
            id: "ruby:ruby_block".into(),
            template: "minecraft:iron_block".into(),
            properties: BlockProperties {
                hardness: Some(4.0),
                light: Some(3),
                sounds: Some("minecraft:amethyst_block".into()),
                ..BlockProperties::default()
            },
            shapes: Some(BlockShapes {
                property: "axis".into(),
                values: BTreeMap::from([("x".to_owned(), vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]])]),
            }),
            default_state: BTreeMap::from([("axis".to_owned(), "x".to_owned())]),
            item: true,
            components: BTreeMap::from([(
                "minecraft:max_stack_size".to_owned(),
                serde_json::json!(16),
            )]),
        };
        let bowl = Item {
            id: "ruby:bowl".into(),
            template: "minecraft:bowl".into(),
            max_stack_size: None,
            rarity: None,
            components: BTreeMap::new(),
            crafting_remainder: None,
        };
        let stew = Item {
            id: "ruby:stew".into(),
            template: "minecraft:mushroom_stew".into(),
            max_stack_size: Some(1),
            rarity: Some(Rarity::Rare),
            components: BTreeMap::from([("!minecraft:lore".to_owned(), serde_json::json!({}))]),
            crafting_remainder: Some("ruby:bowl".into()),
        };
        let effect = MobEffect {
            id: "ruby:glow".into(),
            category: EffectCategory::Beneficial,
            color: 0xFF_0000,
        };
        let written = Content::from_native(
            "ruby",
            NativeDeclarations {
                blocks: vec![block.clone()],
                // Declared out of order: the reader sorts each table by id.
                items: vec![stew.clone(), bowl.clone()],
                effects: vec![effect.clone()],
                ..NativeDeclarations::default()
            },
        )
        .unwrap();
        assert_eq!(written.native_files.len(), 4);
        let files: Vec<(&str, &[u8])> = written
            .native_files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        let read = hmod("native-typed-root", MANIFEST_TEXT, &files).unwrap();
        assert_eq!(read.files.len(), 4);
        assert_eq!(read.blocks, [block]);
        assert_eq!(read.items, [bowl, stew]);
        assert_eq!(read.effects, [effect]);
        assert_eq!(read.namespaces, written.namespaces);
        // The files are the packaged shapes, without a format marker.
        let block = native_json(&written, "data/ruby/horizon/block/ruby_block.json");
        assert!(block.get("format").is_none());
        assert_eq!(block["item"], true);
        assert_eq!(block["properties"]["light"], 3);
        assert_eq!(block["default_state"]["axis"], "x");
        assert_eq!(block["shapes"]["property"], "axis");
        let effect = native_json(&written, "data/ruby/horizon/mob_effect/glow.json");
        assert!(effect.get("format").is_none());
        assert_eq!(effect["category"], "beneficial");
        assert_eq!(effect["color"], 0xFF_0000);
    }

    #[test]
    fn native_block_item_and_item_still_clash_by_id() {
        let block = Block {
            id: "ruby:lamp".into(),
            template: "minecraft:sea_lantern".into(),
            properties: BlockProperties::default(),
            shapes: None,
            default_state: BTreeMap::new(),
            item: true,
            components: BTreeMap::new(),
        };
        let item = Item {
            id: "ruby:lamp".into(),
            template: "minecraft:stick".into(),
            max_stack_size: None,
            rarity: None,
            components: BTreeMap::new(),
            crafting_remainder: None,
        };
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                blocks: vec![block.clone()],
                items: vec![item.clone()],
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert_eq!(error, "native item ruby:lamp conflicts with a block item");
        // A block without an item leaves the id to the item, and the two rows
        // sit in different folders, so they do not collide on disk.
        let content = Content::from_native(
            "ruby",
            NativeDeclarations {
                blocks: vec![Block {
                    item: false,
                    ..block
                }],
                items: vec![item],
                ..NativeDeclarations::default()
            },
        )
        .unwrap();
        assert!(!content.blocks[0].item);
        assert!(
            content
                .native_files
                .contains_key("data/ruby/horizon/block/lamp.json")
        );
        assert!(
            content
                .native_files
                .contains_key("data/ruby/horizon/item/lamp.json")
        );
    }

    #[test]
    fn native_typed_rows_count_toward_the_content_limits() {
        let effects: Vec<MobEffect> = (0..=MAX_FILES)
            .map(|i| MobEffect {
                id: format!("ruby:effect_{i}"),
                category: EffectCategory::Neutral,
                color: 0,
            })
            .collect();
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                effects,
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert!(
            error.contains(&format!("more than {MAX_FILES} native content files")),
            "{error}"
        );
        // A single row is capped at MAX_FILE like any other content file.
        let block = Block {
            id: "ruby:huge".into(),
            template: "minecraft:stone".into(),
            properties: BlockProperties::default(),
            shapes: None,
            default_state: BTreeMap::new(),
            item: true,
            components: BTreeMap::from([(
                "minecraft:lore".to_owned(),
                serde_json::json!(["x".repeat(MAX_FILE as usize)]),
            )]),
        };
        let error = Content::from_native(
            "ruby",
            NativeDeclarations {
                blocks: vec![block],
                ..NativeDeclarations::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("is over 16 MiB"), "{error}");
    }

    #[test]
    fn native_declaration_files_are_validated_and_name_their_file() {
        let refused = |case: &str, file: &str, body: &str| -> String {
            let files: &[(&str, &[u8])] = &[(file, body.as_bytes())];
            let e = hmod(case, MANIFEST_TEXT, files).expect_err("the row should be refused");
            assert!(e.contains(file), "{case}: {e}");
            e
        };
        let e = refused(
            "native-unknown-kind",
            "data/ruby/horizon/fluid/oil.json",
            "{}",
        );
        assert!(e.contains("block_entity/"), "{e}");

        let e = refused(
            "native-format",
            "data/ruby/horizon/menu/a.json",
            r#"{"format": 2, "title_key": "container.ruby", "slots": 0}"#,
        );
        assert!(e.contains("format 2"), "{e}");

        let e = refused(
            "native-property",
            "data/ruby/horizon/entity/a.json",
            r#"{"format": 1, "template": "minecraft:chicken",
                "properties": {"hardness": "1"}}"#,
        );
        assert!(e.contains("unknown native entity property"), "{e}");

        let e = refused(
            "native-template",
            "data/ruby/horizon/entity/a.json",
            r#"{"template": "ruby:bird", "properties": {}}"#,
        );
        assert!(e.contains("not a vanilla entity"), "{e}");

        let e = refused(
            "native-spawn",
            "data/ruby/horizon/entity/a.json",
            r#"{"template": "minecraft:chicken", "properties": {},
                "spawn": {"category": "ON_GROUND", "heightmap": "MOTION_BLOCKING"}}"#,
        );
        assert!(e.contains("spawn disagrees with properties"), "{e}");

        let e = refused(
            "native-slot",
            "data/ruby/horizon/menu/a.json",
            r#"{"title_key": "container.ruby", "slots": 257}"#,
        );
        assert!(e.contains("slots is 0 to"), "{e}");

        let e = refused(
            "native-codec",
            "data/ruby/horizon/data_component/a.json",
            r#"{"codec": "int"}"#,
        );
        assert!(e.contains("has no namespace"), "{e}");

        // A row written before the marker existed is format 1.
        let files: &[(&str, &[u8])] = &[(
            "data/ruby/horizon/menu/a.json",
            br#"{"title_key": "container.ruby", "slots": 9}"#,
        )];
        let content = hmod("native-legacy", MANIFEST_TEXT, files).unwrap();
        assert_eq!(content.menus[0].slots, 9);
    }

    #[test]
    fn duplicate_ids_are_refused_with_their_file() {
        let files: &[(&str, &[u8])] = &[
            ("data/ruby/horizon/block/a.json", BLOCK),
            ("data/ruby/horizon/item/a.json", ITEM),
        ];
        let e = hmod("native-duplicate", MANIFEST_TEXT, files).unwrap_err();
        assert!(e.contains("also a block's item"), "{e}");
        assert!(e.contains("data/ruby/horizon/item/a.json"), "{e}");
    }

    #[test]
    fn native_declaration_files_stay_in_the_owning_namespace() {
        let manifest = format!("{MANIFEST_TEXT}namespaces = [\"kaisyn\"]\n");
        let files: &[(&str, &[u8])] = &[(
            "data/kaisyn/horizon/entity/a.json",
            br#"{"template": "minecraft:chicken"}"#,
        )];
        let e = hmod("native-foreign", &manifest, files).unwrap_err();
        assert!(e.contains("data/ruby/horizon/"), "{e}");
    }

    #[test]
    fn paths_are_plain() {
        assert!(check_path("assets/ruby/textures/block/ruby_block.png").is_ok());
        assert!(check_path("data/ruby/loot_table/blocks/a.json").is_ok());
        for bad in [
            "assets/ruby",
            "assets/../x/y",
            "assets/ruby//a",
            "assets/Ruby/a",
            "assets/ruby/a b",
            "/assets/ruby/a",
            "other/ruby/a",
            "assets\\ruby\\a",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
    }
}
