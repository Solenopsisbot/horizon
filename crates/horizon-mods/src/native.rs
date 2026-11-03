//! Typed declarations exported by the native Java compatibility boundary.
//!
//! Java registration callbacks do not carry enough information to construct a
//! Horizon block or item by themselves. A loader adapter first enriches a
//! [`horizon_native_java::RegistryEntry`] with a typed declaration, then this
//! sink validates the small declaration vocabulary used by the content loader:
//! blocks, items, mob effects, entities, data components, attachments, menus
//! and block-entity types. Recipes and ingredients stay data. It deliberately
//! collects declarations; the game applies them before its immutable resource
//! tables are built.

use crate::content::{Block, BlockProperties, EffectCategory, Item, MobEffect, Rarity};
use horizon_native_java::{RegistryDeclaration, RegistryEntry, TypedRegistrySink};
use serde::Serialize;
use std::collections::BTreeMap;

/// Native declarations are metadata, not an unbounded Java object graph.
pub const MAX_NATIVE_DECLARATIONS: usize = 8_192;
pub const MAX_NATIVE_PROPERTIES: usize = 32;
pub const MAX_NATIVE_TEXT: usize = 4_096;
/// Recipe and ingredient JSON is data, not executable code; keep each declaration bounded.
pub const MAX_NATIVE_JSON: usize = 1 << 20;
/// The largest hitbox edge a native entity may declare, in blocks. Vanilla's
/// biggest (the ender dragon, the wither) stay far below this.
pub const MAX_ENTITY_SIZE: f64 = 64.0;
/// Vanilla attribute defaults are plain doubles; this is the largest one a
/// declaration may set (health, damage, follow range).
pub const MAX_ENTITY_ATTRIBUTE: f64 = 1.0e6;
/// The height window of a spawn placement is an `i32` inside the build range.
pub const MAX_SPAWN_HEIGHT: i32 = 2_048;
/// A menu's slots, its own plus the player inventory's. Vanilla's largest
/// (`GENERIC_9x6` with the inventory) is 90; a declared menu is capped well
/// above the biggest station slot count so a row stays plainly bounded.
pub const MAX_MENU_SLOTS: u32 = 256;
/// The blocks that open one declared menu (`open_blocks`). A station is opened
/// by a handful of blocks; 64 keeps the row bounded and matches
/// [`MAX_BLOCK_ENTITY_BLOCKS`].
pub const MAX_MENU_OPEN_BLOCKS: usize = 64;
/// `BlockEntityType.validBlocks` for one declared type. Vanilla's longest
/// list (the signs') stays far below this.
pub const MAX_BLOCK_ENTITY_BLOCKS: usize = 64;

/// Vanilla's `MobCategory`, as a native entity's builder records it.
const MOB_CATEGORIES: [&str; 8] = [
    "AMBIENT",
    "AXOLOTLS",
    "CREATURE",
    "MISC",
    "MONSTER",
    "UNDERGROUND_WATER_CREATURE",
    "WATER_AMBIENT",
    "WATER_CREATURE",
];
/// Vanilla's `SpawnPlacementTypes`.
const SPAWN_CATEGORIES: [&str; 4] = ["IN_LAVA", "IN_WATER", "NO_RESTRICTIONS", "ON_GROUND"];
/// Vanilla's `Heightmap.Types`, the ones `SpawnPlacements.register` accepts.
const SPAWN_HEIGHTMAPS: [&str; 4] = [
    "MOTION_BLOCKING",
    "MOTION_BLOCKING_NO_LEAVES",
    "OCEAN_FLOOR",
    "WORLD_SURFACE",
];

/// A custom registration kind whose concrete Horizon adapter is not available
/// yet. It is retained so diagnostics can name the registration precisely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomDeclaration {
    pub registry: String,
    pub id: String,
    pub kind: String,
    pub properties: BTreeMap<String, String>,
}

/// A recipe declaration that becomes a data-pack JSON file before the
/// resource root is loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecipeDeclaration {
    pub id: String,
    pub json: Vec<u8>,
}

/// An ingredient declaration that becomes a data-pack JSON file before the
/// resource root is loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngredientDeclaration {
    pub id: String,
    pub json: Vec<u8>,
}

/// Where a mob may spawn: vanilla's `SpawnPlacementType` plus the height
/// window `SpawnPlacements.register` takes. Both names are canonicalised on
/// parse, so a declaration and its merge compare equal whatever case the
/// adapter used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SpawnRestriction {
    /// `ON_GROUND`, `IN_WATER`, `NO_RESTRICTIONS` or `IN_LAVA`.
    pub category: String,
    /// The `Heightmap.Types` the check samples: `MOTION_BLOCKING`,
    /// `MOTION_BLOCKING_NO_LEAVES`, `OCEAN_FLOOR` or `WORLD_SURFACE`.
    pub heightmap: String,
    /// Inclusive height window; absent keeps the placement type's own.
    pub min_height: Option<i32>,
    pub max_height: Option<i32>,
}

/// The four vanilla attribute defaults a loader adapter can set on a new
/// entity. Vanilla's `Attribute` values are `double` (`MAX_HEALTH`,
/// `MOVEMENT_SPEED`, `ATTACK_DAMAGE`, `FOLLOW_RANGE` are all registered with
/// double defaults), so these are `f64` rather than `i32`: a hand-written
/// `1` would otherwise silently mean 1.0 health. Values are checked for
/// finiteness and a bound on parse, so no `Eq` (floats are only ordered).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct EntityAttributes {
    /// `minecraft:generic.max_health`, above 0.
    pub health: Option<f64>,
    /// `minecraft:generic.movement_speed`.
    pub speed: Option<f64>,
    /// `minecraft:generic.attack_damage`.
    pub attack_damage: Option<f64>,
    /// `minecraft:generic.follow_range`.
    pub follow_range: Option<f64>,
}

/// A bounded entity registration carried by a loader adapter. The native
/// world owns the actual entity implementation; this record preserves the
/// vanilla template and scalar builder options until the live registry is
/// assembled.
///
/// `properties` keeps the adapter's strings, which the live registry assembler
/// reads; `spawn` and `attributes` are the validated typed view of the same
/// rows ([`EntityDeclaration::from_registry_properties`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EntityDeclaration {
    pub id: String,
    pub template: String,
    pub properties: BTreeMap<String, String>,
    /// Set when the adapter declared a spawn placement.
    pub spawn: Option<SpawnRestriction>,
    /// Set when the adapter declared at least one attribute.
    pub attributes: Option<EntityAttributes>,
}

impl EntityDeclaration {
    /// Parses one native entity row from the free-form keys a loader adapter
    /// records. `properties` holds every key except `template`, which the
    /// caller has already validated as a vanilla id; unknown keys are rejected
    /// so a typo cannot silently drop a builder option.
    ///
    /// Recognized keys: `width`, `height`, `eye_height` (a size in blocks, 0
    /// to [`MAX_ENTITY_SIZE`]), `category` (a vanilla `MobCategory`),
    /// `tracking_range` and `update_interval` (a non-zero `u16`),
    /// `fire_immune` (bool), the [`EntityAttributes`] keys `max_health`,
    /// `speed`, `attack_damage`, `follow_range` (a finite `f64`, 0 to
    /// [`MAX_ENTITY_ATTRIBUTE`]), and the [`SpawnRestriction`] keys
    /// `spawn_category`, `spawn_heightmap` (both required together) and
    /// `spawn_min_height`, `spawn_max_height` (an `i32` within
    /// ±[`MAX_SPAWN_HEIGHT`]). Parsing is locale-free: numbers use Rust's
    /// `f64`/integer syntax, which always takes `.` as the separator.
    pub fn from_registry_properties(
        id: impl Into<String>,
        template: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut attributes = EntityAttributes::default();
        let mut has_attribute = false;
        let mut spawn_category = None;
        let mut spawn_heightmap = None;
        let mut min_height = None;
        let mut max_height = None;
        for (key, value) in properties {
            match key.as_str() {
                "width" | "height" | "eye_height" => {
                    parse_entity_size(key, value)?;
                }
                "category" => {
                    parse_choice(key, value, &MOB_CATEGORIES)?;
                }
                "tracking_range" | "update_interval" => {
                    if parse_u16(key, value)? == 0 {
                        return Err(format!("native entity property {key} is 1 or more"));
                    }
                }
                "fire_immune" => {
                    parse_bool(key, value)?;
                }
                "max_health" => {
                    let health = parse_attribute(key, value)?;
                    if health <= 0.0 {
                        return Err("native entity property max_health is above 0".into());
                    }
                    attributes.health = Some(health);
                    has_attribute = true;
                }
                "speed" => {
                    attributes.speed = Some(parse_attribute(key, value)?);
                    has_attribute = true;
                }
                "attack_damage" => {
                    attributes.attack_damage = Some(parse_attribute(key, value)?);
                    has_attribute = true;
                }
                "follow_range" => {
                    attributes.follow_range = Some(parse_attribute(key, value)?);
                    has_attribute = true;
                }
                "spawn_category" => {
                    spawn_category = Some(parse_choice(key, value, &SPAWN_CATEGORIES)?);
                }
                "spawn_heightmap" => {
                    spawn_heightmap = Some(parse_choice(key, value, &SPAWN_HEIGHTMAPS)?);
                }
                "spawn_min_height" => min_height = Some(parse_spawn_height(key, value)?),
                "spawn_max_height" => max_height = Some(parse_spawn_height(key, value)?),
                other => return Err(format!("unknown native entity property {other:?}")),
            }
        }
        let spawn = match (spawn_category, spawn_heightmap) {
            (None, None) if min_height.is_none() && max_height.is_none() => None,
            (Some(category), Some(heightmap)) => {
                if let (Some(min), Some(max)) = (min_height, max_height)
                    && min > max
                {
                    return Err(format!(
                        "native entity spawn height window {min}..{max} is reversed"
                    ));
                }
                Some(SpawnRestriction {
                    category,
                    heightmap,
                    min_height,
                    max_height,
                })
            }
            _ => {
                return Err(
                    "native entity spawn restriction needs spawn_category and spawn_heightmap"
                        .into(),
                );
            }
        };
        Ok(Self {
            id: id.into(),
            template: template.into(),
            properties: properties.clone(),
            spawn,
            attributes: has_attribute.then_some(attributes),
        })
    }
}

/// A typed data-component registration. Arbitrary Java codecs remain on the
/// JVM fallback; native adapters provide a stable codec id and bounded flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataComponentDeclaration {
    pub id: String,
    pub codec: String,
    pub persistent: bool,
    pub network_synced: bool,
}

/// A typed attachment registration shared by Forge and NeoForge adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentDeclaration {
    pub id: String,
    pub codec: String,
    pub default_value: String,
    pub copy_on_death: bool,
}

/// A typed menu registration (`MenuType`): the id a server opens with
/// `ClientboundOpenScreenPacket`, the translation key its title resolves
/// through, how many slots it holds, and the blocks that open it. The live
/// menu still belongs to the game; this row only names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuDeclaration {
    pub id: String,
    /// A translation key (`container.ruby.crate`), not a rendered title.
    pub title_key: String,
    /// Slots the menu holds, 0 to [`MAX_MENU_SLOTS`].
    pub slots: u32,
    /// The blocks that open it, sorted and without repeats: a resource
    /// location each, vanilla (`minecraft:barrel`) allowed. Empty leaves the
    /// menu bound to no block.
    pub open_blocks: Vec<String>,
}

impl MenuDeclaration {
    /// Parses one native menu row from the free-form keys a loader adapter
    /// records. `title_key` and `slots` are both required: a `MenuType`
    /// without a title or a slot count has nothing to open. `open_blocks` is
    /// optional (comma-separated block ids, at most
    /// [`MAX_MENU_OPEN_BLOCKS`] of them, sorted and without repeats). Every
    /// other key is rejected so a typo cannot silently drop a constructor
    /// option.
    pub fn from_registry_properties(
        id: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut title_key = None;
        let mut slots = None;
        let mut open_blocks = None;
        for (key, value) in properties {
            match key.as_str() {
                "title_key" => title_key = Some(value.clone()),
                "slots" => slots = Some(parse_u32(key, value)?),
                "open_blocks" => open_blocks = Some(parse_open_blocks(value)?),
                other => return Err(format!("unknown native menu property {other:?}")),
            }
        }
        let menu = Self {
            id: id.into(),
            title_key: title_key.ok_or_else(|| "native menu has no title_key".to_owned())?,
            slots: slots.ok_or_else(|| "native menu has no slots".to_owned())?,
            open_blocks: open_blocks.unwrap_or_default(),
        };
        menu.validate()?;
        Ok(menu)
    }

    /// The fences a menu row passes whether the registry sink parsed it or it
    /// was built by hand: a namespaced id, a translation key (`a-z0-9_.-`,
    /// bounded, no control characters), 0 to [`MAX_MENU_SLOTS`] slots and up
    /// to [`MAX_MENU_OPEN_BLOCKS`] distinct block ids.
    pub fn validate(&self) -> Result<(), String> {
        NativeDeclarations::ensure_id(&self.id)?;
        if self.title_key.is_empty()
            || self.title_key.len() > MAX_NATIVE_TEXT
            || !self
                .title_key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err(format!(
                "native menu {} title_key {:?} is a key of a-z0-9_.-",
                self.id, self.title_key
            ));
        }
        if self.slots > MAX_MENU_SLOTS {
            return Err(format!(
                "native menu {} slots is 0 to {MAX_MENU_SLOTS}",
                self.id
            ));
        }
        if self.open_blocks.len() > MAX_MENU_OPEN_BLOCKS {
            return Err(format!(
                "native menu {} has at most {MAX_MENU_OPEN_BLOCKS} open blocks",
                self.id
            ));
        }
        let mut sorted = self.open_blocks.clone();
        sorted.sort();
        if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(format!("native menu {} open_blocks repeats an id", self.id));
        }
        for block in &self.open_blocks {
            NativeDeclarations::ensure_id(block)?;
        }
        Ok(())
    }
}

/// A typed block-entity registration: a vanilla `BlockEntityType` template
/// under a new id, the blocks that carry it and whether it ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockEntityDeclaration {
    pub id: String,
    /// The vanilla block entity type it copies, `minecraft:<path>`.
    pub template: String,
    /// Whether the game ticks it (`BlockEntityTicker`).
    pub ticker: bool,
    /// `BlockEntityType.validBlocks`: the blocks that carry it, sorted.
    pub valid_blocks: Vec<String>,
}

impl BlockEntityDeclaration {
    /// Parses one native block-entity row from the free-form keys a loader
    /// adapter records. `template` (a vanilla block entity type) and
    /// `valid_blocks` (comma-separated ids, at most
    /// [`MAX_BLOCK_ENTITY_BLOCKS`] of them) are required; `ticker` is a bool
    /// and defaults to false. The block list is sorted and may not repeat an
    /// id, so metadata order cannot change the declaration. Every other key
    /// is rejected.
    pub fn from_registry_properties(
        id: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut template = None;
        let mut ticker = false;
        let mut valid_blocks = None;
        for (key, value) in properties {
            match key.as_str() {
                "template" => template = Some(value.clone()),
                "ticker" => ticker = parse_bool(key, value)?,
                "valid_blocks" => valid_blocks = Some(parse_valid_blocks(value)?),
                other => return Err(format!("unknown native block entity property {other:?}")),
            }
        }
        let block_entity = Self {
            id: id.into(),
            template: template.ok_or_else(|| "native block entity has no template".to_owned())?,
            ticker,
            valid_blocks: valid_blocks
                .ok_or_else(|| "native block entity has no valid_blocks".to_owned())?,
        };
        block_entity.validate()?;
        Ok(block_entity)
    }

    /// The fences a block-entity row passes whether the registry sink parsed it
    /// or it was built by hand: a namespaced id, a vanilla template, and 1 to
    /// [`MAX_BLOCK_ENTITY_BLOCKS`] distinct block ids.
    pub fn validate(&self) -> Result<(), String> {
        NativeDeclarations::ensure_id(&self.id)?;
        NativeDeclarations::ensure_id(&self.template)?;
        if !self.template.starts_with("minecraft:") {
            return Err(format!(
                "native block entity {} template {:?} must be a vanilla id",
                self.id, self.template
            ));
        }
        if self.valid_blocks.is_empty() || self.valid_blocks.len() > MAX_BLOCK_ENTITY_BLOCKS {
            return Err(format!(
                "native block entity {} has 1 to {MAX_BLOCK_ENTITY_BLOCKS} valid blocks",
                self.id
            ));
        }
        let mut sorted = self.valid_blocks.clone();
        sorted.sort();
        if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(format!(
                "native block entity {} valid_blocks repeats an id",
                self.id
            ));
        }
        for block in &self.valid_blocks {
            NativeDeclarations::ensure_id(block)?;
        }
        Ok(())
    }
}

/// Typed registrations waiting for the pre-resource load phase.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct NativeDeclarations {
    pub blocks: Vec<Block>,
    pub items: Vec<Item>,
    pub effects: Vec<MobEffect>,
    pub entities: Vec<EntityDeclaration>,
    pub data_components: Vec<DataComponentDeclaration>,
    pub attachments: Vec<AttachmentDeclaration>,
    pub menus: Vec<MenuDeclaration>,
    pub block_entities: Vec<BlockEntityDeclaration>,
    pub recipes: Vec<RecipeDeclaration>,
    pub ingredients: Vec<IngredientDeclaration>,
    pub custom: Vec<CustomDeclaration>,
}

impl NativeDeclarations {
    /// Converts a batch of typed loader registrations into the declaration
    /// set consumed by [`crate::Content::from_native`].  The conversion is
    /// intentionally all-or-nothing: an opaque or invalid entry stops the
    /// batch before callers can accidentally apply a partial registry.
    pub fn from_registry_entries<'a, I>(entries: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = &'a RegistryEntry>,
    {
        let mut declarations = Self::default();
        declarations.register_entries(entries)?;
        Ok(declarations)
    }

    /// Adds typed entries to this declaration set.  This is the bridge used
    /// by the game-facing loader to combine declarations parsed from
    /// `META-INF/horizon-native.json` with typed values recorded while the
    /// Java entrypoint executes.
    pub fn register_entries<'a, I>(&mut self, entries: I) -> Result<(), String>
    where
        I: IntoIterator<Item = &'a RegistryEntry>,
    {
        // Stage the batch so a later invalid or opaque entry cannot leave a
        // partially applied declaration set visible to the resource planner.
        let mut staged = self.clone();
        for entry in entries {
            staged.register_typed(entry)?;
        }
        staged.check_count()?;
        *self = staged;
        Ok(())
    }

    /// Merge declarations from another adapter transactionally.
    ///
    /// Equal rows are coalesced, while a row with the same id and different
    /// values is rejected. Rows are sorted by id after the merge so metadata
    /// order and entrypoint execution order cannot change the resource plan.
    pub fn merge(&mut self, incoming: Self) -> Result<(), String> {
        let mut staged = self.clone();
        merge_rows("block", &mut staged.blocks, incoming.blocks, |row| &row.id)?;
        merge_rows("item", &mut staged.items, incoming.items, |row| &row.id)?;
        merge_rows("mob effect", &mut staged.effects, incoming.effects, |row| {
            &row.id
        })?;
        merge_rows("entity", &mut staged.entities, incoming.entities, |row| {
            &row.id
        })?;
        merge_rows(
            "data component",
            &mut staged.data_components,
            incoming.data_components,
            |row| &row.id,
        )?;
        merge_rows(
            "attachment",
            &mut staged.attachments,
            incoming.attachments,
            |row| &row.id,
        )?;
        merge_rows("menu", &mut staged.menus, incoming.menus, |row| &row.id)?;
        merge_rows(
            "block entity",
            &mut staged.block_entities,
            incoming.block_entities,
            |row| &row.id,
        )?;
        merge_rows("recipe", &mut staged.recipes, incoming.recipes, |row| {
            &row.id
        })?;
        merge_rows(
            "ingredient",
            &mut staged.ingredients,
            incoming.ingredients,
            |row| &row.id,
        )?;
        for row in incoming.custom {
            if let Some(existing) = staged
                .custom
                .iter()
                .find(|existing| existing.registry == row.registry && existing.id == row.id)
            {
                if existing != &row {
                    return Err(format!(
                        "conflicting native custom registration {}/{}",
                        row.registry, row.id
                    ));
                }
            } else if staged
                .custom
                .iter()
                .any(|existing| existing.id == row.id && existing.registry != row.registry)
            {
                return Err(format!("conflicting native custom registration {}", row.id));
            } else {
                staged.custom.push(row);
            }
        }
        staged.custom.sort_by(|a, b| {
            a.registry
                .cmp(&b.registry)
                .then_with(|| a.id.cmp(&b.id))
                .then_with(|| a.kind.cmp(&b.kind))
        });
        staged.check_count()?;
        *self = staged;
        Ok(())
    }

    fn check_count(&self) -> Result<(), String> {
        let count = self.blocks.len()
            + self.items.len()
            + self.effects.len()
            + self.entities.len()
            + self.data_components.len()
            + self.attachments.len()
            + self.menus.len()
            + self.block_entities.len()
            + self.recipes.len()
            + self.ingredients.len()
            + self.custom.len();
        if count > MAX_NATIVE_DECLARATIONS {
            return Err(format!(
                "native declarations exceed {MAX_NATIVE_DECLARATIONS} rows"
            ));
        }
        Ok(())
    }

    /// A short summary of the declaration set, for the launcher's mod page.
    ///
    /// Purely informational: it validates nothing and changes nothing. The
    /// first line counts every typed row; a second line lists the custom
    /// registrations by kind, which are the ones Horizon has no adapter for
    /// (an unknown kind is rejected while it is registered, not here).
    pub fn diagnostics(&self) -> Vec<String> {
        let mut out = vec![format!(
            "native declarations: {} blocks, {} items, {} mob effects, {} entities, {} data components, {} attachments, {} menus, {} block entities, {} recipes, {} ingredients, {} custom",
            self.blocks.len(),
            self.items.len(),
            self.effects.len(),
            self.entities.len(),
            self.data_components.len(),
            self.attachments.len(),
            self.menus.len(),
            self.block_entities.len(),
            self.recipes.len(),
            self.ingredients.len(),
            self.custom.len(),
        )];
        if !self.custom.is_empty() {
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            for row in &self.custom {
                *kinds.entry(row.kind.as_str()).or_default() += 1;
            }
            let kinds = kinds
                .into_iter()
                .map(|(kind, count)| format!("{kind}={count}"))
                .collect::<Vec<_>>();
            out.push(format!(
                "native custom registration kinds: {}",
                kinds.join(", ")
            ));
        }
        out
    }

    fn ensure_id(id: &str) -> Result<(), String> {
        let Some((namespace, path)) = id.split_once(':') else {
            return Err(format!("native registration {id:?} has no namespace"));
        };
        if id.len() > MAX_NATIVE_TEXT
            || namespace.is_empty()
            || path.is_empty()
            || !namespace
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
            || !path
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b))
        {
            return Err(format!("native registration id {id:?} is invalid"));
        }
        Ok(())
    }

    fn ensure_registration_id(id: &str) -> Result<(), String> {
        Self::ensure_id(id)?;
        if id.starts_with("minecraft:") {
            return Err(format!(
                "native registration {id:?} cannot redefine the minecraft namespace"
            ));
        }
        Ok(())
    }

    fn check_properties(kind: &str, properties: &BTreeMap<String, String>) -> Result<(), String> {
        if properties.len() > MAX_NATIVE_PROPERTIES {
            return Err(format!(
                "native {kind} has more than {MAX_NATIVE_PROPERTIES} properties"
            ));
        }
        if properties.iter().any(|(key, value)| {
            key.is_empty() || key.len() > MAX_NATIVE_TEXT || value.len() > MAX_NATIVE_TEXT
        }) {
            return Err(format!(
                "native {kind} property names and values are at most {MAX_NATIVE_TEXT} bytes"
            ));
        }
        Ok(())
    }

    fn parse_block_properties(
        properties: &BTreeMap<String, String>,
    ) -> Result<BlockProperties, String> {
        let mut out = BlockProperties::default();
        for (key, value) in properties {
            match key.as_str() {
                "hardness" => out.hardness = Some(parse_f32(key, value)?),
                "explosion_resistance" => out.explosion_resistance = Some(parse_f32(key, value)?),
                "light" => out.light = Some(parse_u8(key, value)?),
                "requires_tool" => out.requires_tool = Some(parse_bool(key, value)?),
                "friction" => out.friction = Some(parse_f32(key, value)?),
                "map_color" => out.map_color = Some(parse_u8(key, value)?),
                "sounds" => out.sounds = Some(value.clone()),
                other => return Err(format!("unknown native block property {other:?}")),
            }
        }
        Ok(out)
    }

    fn add_block(
        &mut self,
        entry: &RegistryEntry,
        template: &str,
        properties: &BTreeMap<String, String>,
        item: bool,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("block", properties)?;
        Self::ensure_id(template)?;
        if self.blocks.iter().any(|block| block.id == entry.id) {
            return Err(format!("duplicate native block {}", entry.id));
        }
        if item && self.items.iter().any(|existing| existing.id == entry.id) {
            return Err(format!("native block {} conflicts with an item", entry.id));
        }
        self.blocks.push(Block {
            id: entry.id.clone(),
            template: template.to_owned(),
            properties: Self::parse_block_properties(properties)?,
            shapes: None,
            default_state: BTreeMap::new(),
            item,
            components: BTreeMap::new(),
        });
        Ok(())
    }

    fn add_item(
        &mut self,
        entry: &RegistryEntry,
        template: &str,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("item", properties)?;
        Self::ensure_id(template)?;
        if self.items.iter().any(|item| item.id == entry.id) {
            return Err(format!("duplicate native item {}", entry.id));
        }
        if self
            .blocks
            .iter()
            .any(|block| block.id == entry.id && block.item)
        {
            return Err(format!(
                "native item {} conflicts with a block item",
                entry.id
            ));
        }
        let mut max_stack_size = None;
        let mut rarity = None;
        let mut crafting_remainder = None;
        let mut components = BTreeMap::new();
        for (key, value) in properties {
            match key.as_str() {
                "max_stack_size" => max_stack_size = Some(parse_u8(key, value)?),
                "rarity" => {
                    rarity = Some(match value.to_ascii_lowercase().as_str() {
                        "common" => Rarity::Common,
                        "uncommon" => Rarity::Uncommon,
                        "rare" => Rarity::Rare,
                        "epic" => Rarity::Epic,
                        _ => return Err(format!("invalid native item rarity {value:?}")),
                    })
                }
                "crafting_remainder" => {
                    Self::ensure_id(value)?;
                    crafting_remainder = Some(value.clone());
                }
                key if key.starts_with("component:") => {
                    components.insert(
                        key.trim_start_matches("component:").to_owned(),
                        serde_json::Value::String(value.clone()),
                    );
                }
                other => return Err(format!("unknown native item property {other:?}")),
            }
        }
        self.items.push(Item {
            id: entry.id.clone(),
            template: template.to_owned(),
            max_stack_size,
            rarity,
            components,
            crafting_remainder,
        });
        Ok(())
    }

    fn add_recipe(&mut self, entry: &RegistryEntry, json: &str) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        if json.len() > MAX_NATIVE_JSON {
            return Err(format!(
                "native recipe {} exceeds {MAX_NATIVE_JSON} bytes",
                entry.id
            ));
        }
        if self.recipes.iter().any(|recipe| recipe.id == entry.id) {
            return Err(format!("duplicate native recipe {}", entry.id));
        }
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|error| format!("native recipe {} is invalid JSON: {error}", entry.id))?;
        if !value.is_object() {
            return Err(format!("native recipe {} is not a JSON object", entry.id));
        }
        let json = serde_json::to_vec(&value)
            .map_err(|error| format!("native recipe {} cannot be encoded: {error}", entry.id))?;
        if json.len() > MAX_NATIVE_JSON {
            return Err(format!(
                "native recipe {} exceeds {MAX_NATIVE_JSON} bytes",
                entry.id
            ));
        }
        self.recipes.push(RecipeDeclaration {
            id: entry.id.clone(),
            json,
        });
        Ok(())
    }

    fn add_ingredient(&mut self, entry: &RegistryEntry, json: &str) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        if json.len() > MAX_NATIVE_JSON {
            return Err(format!(
                "native ingredient {} exceeds {MAX_NATIVE_JSON} bytes",
                entry.id
            ));
        }
        if self
            .ingredients
            .iter()
            .any(|ingredient| ingredient.id == entry.id)
        {
            return Err(format!("duplicate native ingredient {}", entry.id));
        }
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|error| format!("native ingredient {} is invalid JSON: {error}", entry.id))?;
        if !value.is_object() {
            return Err(format!(
                "native ingredient {} is not a JSON object",
                entry.id
            ));
        }
        let json = serde_json::to_vec(&value).map_err(|error| {
            format!("native ingredient {} cannot be encoded: {error}", entry.id)
        })?;
        if json.len() > MAX_NATIVE_JSON {
            return Err(format!(
                "native ingredient {} exceeds {MAX_NATIVE_JSON} bytes",
                entry.id
            ));
        }
        self.ingredients.push(IngredientDeclaration {
            id: entry.id.clone(),
            json,
        });
        Ok(())
    }

    fn add_entity(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("entity", properties)?;
        let template = properties
            .get("template")
            .ok_or_else(|| format!("native entity {} has no template", entry.id))?;
        Self::ensure_id(template)?;
        if !template.starts_with("minecraft:") {
            return Err(format!(
                "native entity {} template must be a vanilla id",
                entry.id
            ));
        }
        if self.entities.iter().any(|entity| entity.id == entry.id) {
            return Err(format!("duplicate native entity {}", entry.id));
        }
        let mut retained = properties.clone();
        retained.remove("template");
        let entity = EntityDeclaration::from_registry_properties(
            entry.id.clone(),
            template.clone(),
            &retained,
        )?;
        self.entities.push(entity);
        Ok(())
    }

    /// A data-component or attachment codec is a resource location. Rejecting
    /// control characters here means a diagnostic can never carry terminal
    /// escapes copied from an adapter string into the launcher's log.
    ///
    /// Also the reader's fence: a packaged declaration file under
    /// `data/<id>/horizon/` goes through it, so a content root validates the
    /// same codec ids the sink accepted.
    pub(crate) fn ensure_codec(kind: &str, id: &str, codec: &str) -> Result<(), String> {
        Self::ensure_id(codec)?;
        if id.chars().chain(codec.chars()).any(char::is_control) {
            return Err(format!(
                "native {kind} {id:?} codec {codec:?} has control characters"
            ));
        }
        Ok(())
    }

    fn add_data_component(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("data component", properties)?;
        let codec = properties
            .get("codec")
            .ok_or_else(|| format!("native data component {} has no codec", entry.id))?;
        Self::ensure_codec("data component", &entry.id, codec)?;
        if self
            .data_components
            .iter()
            .any(|component| component.id == entry.id)
        {
            return Err(format!("duplicate native data component {}", entry.id));
        }
        let persistent = properties
            .get("persistent")
            .map_or(Ok(true), |value| parse_bool("persistent", value))?;
        let network_synced = properties
            .get("network_synced")
            .or_else(|| properties.get("network-synced"))
            .map_or(Ok(false), |value| parse_bool("network_synced", value))?;
        for key in properties.keys() {
            if !matches!(
                key.as_str(),
                "codec" | "persistent" | "network_synced" | "network-synced"
            ) {
                return Err(format!("unknown native data component property {key:?}"));
            }
        }
        self.data_components.push(DataComponentDeclaration {
            id: entry.id.clone(),
            codec: codec.clone(),
            persistent,
            network_synced,
        });
        Ok(())
    }

    fn add_attachment(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("attachment", properties)?;
        let codec = properties
            .get("codec")
            .ok_or_else(|| format!("native attachment {} has no codec", entry.id))?;
        Self::ensure_codec("attachment", &entry.id, codec)?;
        if self
            .attachments
            .iter()
            .any(|attachment| attachment.id == entry.id)
        {
            return Err(format!("duplicate native attachment {}", entry.id));
        }
        let default_value = properties
            .get("default")
            .or_else(|| properties.get("default_value"))
            .cloned()
            .unwrap_or_else(|| "null".to_owned());
        if default_value.len() > MAX_NATIVE_TEXT {
            return Err(format!(
                "native attachment {} default is at most {MAX_NATIVE_TEXT} bytes",
                entry.id
            ));
        }
        if default_value.chars().any(char::is_control) {
            return Err(format!(
                "native attachment {} default has control characters",
                entry.id
            ));
        }
        let copy_on_death = properties
            .get("copy_on_death")
            .or_else(|| properties.get("copy-on-death"))
            .map_or(Ok(false), |value| parse_bool("copy_on_death", value))?;
        for key in properties.keys() {
            if !matches!(
                key.as_str(),
                "codec" | "default" | "default_value" | "copy_on_death" | "copy-on-death"
            ) {
                return Err(format!("unknown native attachment property {key:?}"));
            }
        }
        self.attachments.push(AttachmentDeclaration {
            id: entry.id.clone(),
            codec: codec.clone(),
            default_value,
            copy_on_death,
        });
        Ok(())
    }

    fn add_menu(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("menu", properties)?;
        if self.menus.iter().any(|menu| menu.id == entry.id) {
            return Err(format!("duplicate native menu {}", entry.id));
        }
        self.menus.push(MenuDeclaration::from_registry_properties(
            entry.id.clone(),
            properties,
        )?);
        Ok(())
    }

    fn add_block_entity(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_registration_id(&entry.id)?;
        Self::check_properties("block entity", properties)?;
        if self
            .block_entities
            .iter()
            .any(|block_entity| block_entity.id == entry.id)
        {
            return Err(format!("duplicate native block entity {}", entry.id));
        }
        self.block_entities
            .push(BlockEntityDeclaration::from_registry_properties(
                entry.id.clone(),
                properties,
            )?);
        Ok(())
    }
}

impl TypedRegistrySink for NativeDeclarations {
    fn register_typed(&mut self, entry: &RegistryEntry) -> Result<(), String> {
        let Some(declaration) = entry.declaration.as_ref() else {
            return Err(format!(
                "native registration {} has no typed declaration",
                entry.key()
            ));
        };
        match declaration {
            RegistryDeclaration::Block {
                template,
                properties,
                item,
            } => self.add_block(entry, template, properties, *item),
            RegistryDeclaration::Item {
                template,
                properties,
            } => self.add_item(entry, template, properties),
            RegistryDeclaration::MobEffect { category, color } => {
                Self::ensure_registration_id(&entry.id)?;
                if *color > 0xFF_FFFF {
                    return Err(format!(
                        "native mob effect {} color is RGB, 0 to 16777215 (0xFFFFFF)",
                        entry.id
                    ));
                }
                if self.effects.iter().any(|effect| effect.id == entry.id) {
                    return Err(format!("duplicate native mob effect {}", entry.id));
                }
                let category = match category.to_ascii_lowercase().as_str() {
                    "beneficial" => EffectCategory::Beneficial,
                    "harmful" => EffectCategory::Harmful,
                    "neutral" => EffectCategory::Neutral,
                    _ => return Err(format!("invalid native mob effect category {category:?}")),
                };
                self.effects.push(MobEffect {
                    id: entry.id.clone(),
                    category,
                    color: *color,
                });
                Ok(())
            }
            RegistryDeclaration::Recipe { json } => self.add_recipe(entry, json),
            RegistryDeclaration::Ingredient { json } => self.add_ingredient(entry, json),
            RegistryDeclaration::Custom { kind, properties } => {
                match kind.to_ascii_lowercase().as_str() {
                    "entity" | "entity_type" | "entity-type" => {
                        return self.add_entity(entry, properties);
                    }
                    "data_component" | "data-component" | "component" => {
                        return self.add_data_component(entry, properties);
                    }
                    "attachment" | "attachments" => {
                        return self.add_attachment(entry, properties);
                    }
                    "menu" | "menus" => {
                        return self.add_menu(entry, properties);
                    }
                    "block_entity" | "block-entity" | "blockentity" => {
                        return self.add_block_entity(entry, properties);
                    }
                    _ => {}
                }
                Self::ensure_registration_id(&entry.id)?;
                Self::check_properties("custom", properties)?;
                self.custom.push(CustomDeclaration {
                    registry: entry.registry.clone(),
                    id: entry.id.clone(),
                    kind: kind.clone(),
                    properties: properties.clone(),
                });
                Ok(())
            }
        }
    }
}

fn merge_rows<T, F>(kind: &str, target: &mut Vec<T>, incoming: Vec<T>, id: F) -> Result<(), String>
where
    T: Clone + PartialEq,
    F: Fn(&T) -> &str + Copy,
{
    for row in incoming {
        if let Some(existing) = target.iter().find(|existing| id(existing) == id(&row)) {
            if existing != &row {
                return Err(format!(
                    "conflicting native {kind} registration {}",
                    id(&row)
                ));
            }
        } else {
            target.push(row);
        }
    }
    target.sort_by(|a, b| id(a).cmp(id(b)));
    Ok(())
}

fn parse_f32(key: &str, value: &str) -> Result<f32, String> {
    let parsed = value
        .parse::<f32>()
        .map_err(|_| format!("native property {key} is not a number: {value:?}"))?;
    parsed
        .is_finite()
        .then_some(parsed)
        .ok_or_else(|| format!("native property {key} is not finite"))
}

fn parse_u8(key: &str, value: &str) -> Result<u8, String> {
    value
        .parse::<u8>()
        .map_err(|_| format!("native property {key} is not an unsigned byte: {value:?}"))
}

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("native property {key} is not boolean: {value:?}")),
    }
}

fn parse_u16(key: &str, value: &str) -> Result<u16, String> {
    value
        .parse::<u16>()
        .map_err(|_| format!("native property {key} is not an unsigned short: {value:?}"))
}

/// A hitbox edge in blocks: finite and no larger than [`MAX_ENTITY_SIZE`].
fn parse_entity_size(key: &str, value: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("native entity property {key} is not a number: {value:?}"))?;
    if !parsed.is_finite() || !(0.0..=MAX_ENTITY_SIZE).contains(&parsed) {
        return Err(format!(
            "native entity property {key} is a number from 0 to {MAX_ENTITY_SIZE}"
        ));
    }
    Ok(parsed)
}

/// A vanilla attribute default: finite and no larger than
/// [`MAX_ENTITY_ATTRIBUTE`].
fn parse_attribute(key: &str, value: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("native entity property {key} is not a number: {value:?}"))?;
    if !parsed.is_finite() || !(0.0..=MAX_ENTITY_ATTRIBUTE).contains(&parsed) {
        return Err(format!(
            "native entity property {key} is a number from 0 to {MAX_ENTITY_ATTRIBUTE}"
        ));
    }
    Ok(parsed)
}

/// A height-window bound, inside the build range.
fn parse_spawn_height(key: &str, value: &str) -> Result<i32, String> {
    let parsed = value
        .parse::<i32>()
        .map_err(|_| format!("native entity property {key} is not an integer: {value:?}"))?;
    if !(-MAX_SPAWN_HEIGHT..=MAX_SPAWN_HEIGHT).contains(&parsed) {
        return Err(format!(
            "native entity property {key} is an integer from -{MAX_SPAWN_HEIGHT} to {MAX_SPAWN_HEIGHT}"
        ));
    }
    Ok(parsed)
}

/// One of a fixed vanilla vocabulary, canonicalised to upper case.
fn parse_choice(key: &str, value: &str, allowed: &[&str]) -> Result<String, String> {
    let canonical = value.to_ascii_uppercase();
    if !allowed.contains(&canonical.as_str()) {
        return Err(format!(
            "native entity property {key} is one of {}: {value:?}",
            allowed.join(", ")
        ));
    }
    Ok(canonical)
}

/// An unsigned integer property.
fn parse_u32(key: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("native property {key} is not an unsigned integer: {value:?}"))
}

/// `BlockEntityType.validBlocks` as a comma-separated list: at most
/// [`MAX_BLOCK_ENTITY_BLOCKS`] ids, each a resource location, sorted and
/// without repeats.
fn parse_valid_blocks(value: &str) -> Result<Vec<String>, String> {
    parse_id_list(
        "native block entity valid_blocks",
        value,
        MAX_BLOCK_ENTITY_BLOCKS,
    )
}

/// A menu's `open_blocks` as a comma-separated list: at most
/// [`MAX_MENU_OPEN_BLOCKS`] ids, each a resource location, sorted and without
/// repeats, so metadata order cannot change the declaration.
fn parse_open_blocks(value: &str) -> Result<Vec<String>, String> {
    parse_id_list("native menu open_blocks", value, MAX_MENU_OPEN_BLOCKS)
}

/// A bounded comma-separated id list property: each entry is a resource
/// location, the result is sorted and no id appears twice.
fn parse_id_list(label: &str, value: &str, max: usize) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("{label} has an empty id"));
        }
        NativeDeclarations::ensure_id(part)?;
        ids.push(part.to_owned());
        if ids.len() > max {
            return Err(format!("{label} has more than {max} ids"));
        }
    }
    ids.sort();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(format!("{label} repeats an id"));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_native_java::{RegistryDeclaration, Value};

    #[test]
    fn converts_typed_content_before_resource_load() {
        let mut sink = NativeDeclarations::default();
        let block = RegistryEntry::typed(
            "minecraft:block",
            "ruby:lamp",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::from([
                    ("hardness".into(), "2.5".into()),
                    ("light".into(), "12".into()),
                    ("requires_tool".into(), "true".into()),
                ]),
                item: true,
            },
        );
        sink.register_typed(&block).unwrap();
        assert_eq!(sink.blocks[0].id, "ruby:lamp");
        assert_eq!(sink.blocks[0].template, "minecraft:stone");
        assert_eq!(sink.blocks[0].properties.light, Some(12));
        assert_eq!(
            block.value,
            Value::Ref(Some("typed/minecraft:block/ruby:lamp".into()))
        );
    }

    #[test]
    fn rejects_opaque_or_invalid_declarations() {
        let mut sink = NativeDeclarations::default();
        let opaque = RegistryEntry {
            registry: "minecraft:block".into(),
            id: "ruby:lamp".into(),
            value: Value::Ref(Some("registered/lamp".into())),
            declaration: None,
        };
        assert!(sink.register_typed(&opaque).is_err());
        let bad = RegistryEntry::typed(
            "minecraft:item",
            "ruby:bad",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::from([("max_stack_size".into(), "256".into())]),
            },
        );
        assert!(sink.register_typed(&bad).is_err());
    }

    #[test]
    fn declaration_sink_becomes_content_for_resource_planning() {
        let mut sink = NativeDeclarations::default();
        let block = RegistryEntry::typed(
            "minecraft:block",
            "ruby:lamp",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::new(),
                item: false,
            },
        );
        sink.register_typed(&block).unwrap();
        let content = crate::Content::from_native("ruby", sink).unwrap();
        assert!(!content.is_empty());
        assert_eq!(content.namespaces, ["ruby"]);
        assert_eq!(content.blocks[0].id, "ruby:lamp");
    }

    #[test]
    fn recipe_declaration_becomes_native_data_file() {
        let recipe = RegistryEntry::typed(
            "minecraft:recipe",
            "ruby:lamp",
            RegistryDeclaration::Recipe {
                json: r##"{"type":"minecraft:crafting_shaped","pattern":["#"]}"##.into(),
            },
        );
        let declarations = NativeDeclarations::from_registry_entries([&recipe]).unwrap();
        assert_eq!(declarations.recipes.len(), 1);
        let content = crate::Content::from_native("ruby", declarations).unwrap();
        let recipe: serde_json::Value = serde_json::from_slice(
            content
                .native_files
                .get("data/ruby/recipe/lamp.json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            recipe.get("type").and_then(serde_json::Value::as_str),
            Some("minecraft:crafting_shaped")
        );
        assert_eq!(
            recipe
                .get("pattern")
                .and_then(serde_json::Value::as_array)
                .and_then(|values| values.first())
                .and_then(serde_json::Value::as_str),
            Some("#")
        );
    }

    #[test]
    fn ingredient_declaration_becomes_native_data_file() {
        let ingredient = RegistryEntry::typed(
            "minecraft:ingredient",
            "ruby:lamp",
            RegistryDeclaration::Ingredient {
                json: r##"{"items":["minecraft:stone"]}"##.into(),
            },
        );
        let declarations = NativeDeclarations::from_registry_entries([&ingredient]).unwrap();
        assert_eq!(declarations.ingredients.len(), 1);
        let content = crate::Content::from_native("ruby", declarations).unwrap();
        let value: serde_json::Value = serde_json::from_slice(
            content
                .native_files
                .get("data/ruby/ingredient/lamp.json")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            value
                .get("items")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(serde_json::Value::as_str),
            Some("minecraft:stone")
        );
    }

    #[test]
    fn registry_entry_batch_is_atomic_and_content_ready() {
        let block = RegistryEntry::typed(
            "minecraft:block",
            "ruby:lamp",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::new(),
                item: true,
            },
        );
        let effect = RegistryEntry::typed(
            "minecraft:mob_effect",
            "ruby:glow",
            RegistryDeclaration::MobEffect {
                category: "beneficial".into(),
                color: 0x55aa11,
            },
        );
        let declarations = NativeDeclarations::from_registry_entries([&block, &effect]).unwrap();
        assert_eq!(declarations.blocks.len(), 1);
        assert_eq!(declarations.effects.len(), 1);
        assert_eq!(declarations.effects[0].color, 0x55aa11);
        let content = crate::Content::from_native("ruby", declarations).unwrap();
        assert_eq!(content.blocks[0].id, "ruby:lamp");
        assert_eq!(content.effects[0].id, "ruby:glow");
        assert_eq!(content.namespaces, ["ruby"]);

        let mut foreign = NativeDeclarations::default();
        let item = RegistryEntry::typed(
            "minecraft:item",
            "shared:wand",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::new(),
            },
        );
        foreign.register_typed(&item).unwrap();
        let content = crate::Content::from_native("ruby", foreign).unwrap();
        assert_eq!(content.namespaces, ["ruby", "shared"]);

        let opaque = RegistryEntry {
            registry: "minecraft:item".into(),
            id: "ruby:opaque".into(),
            value: Value::Ref(Some("opaque".into())),
            declaration: None,
        };
        let error = NativeDeclarations::from_registry_entries([&block, &opaque]).unwrap_err();
        assert!(error.contains("has no typed declaration"), "{error}");
    }

    #[test]
    fn register_entries_rolls_back_when_a_later_entry_is_invalid() {
        let existing = RegistryEntry::typed(
            "minecraft:item",
            "ruby:existing",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::new(),
            },
        );
        let added = RegistryEntry::typed(
            "minecraft:item",
            "ruby:added",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::new(),
            },
        );
        let opaque = RegistryEntry {
            registry: "minecraft:item".into(),
            id: "ruby:opaque".into(),
            value: Value::Ref(Some("opaque".into())),
            declaration: None,
        };
        let mut declarations = NativeDeclarations::default();
        declarations.register_typed(&existing).unwrap();
        let error = declarations
            .register_entries([&added, &opaque])
            .unwrap_err();
        assert!(error.contains("has no typed declaration"), "{error}");
        assert_eq!(
            declarations
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["ruby:existing"]
        );
    }

    #[test]
    fn custom_loader_kinds_become_typed_entity_component_and_attachment_rows() {
        let rows = [
            RegistryEntry::typed(
                "minecraft:entity_type",
                "ruby:bird",
                RegistryDeclaration::Custom {
                    kind: "entity".into(),
                    properties: BTreeMap::from([
                        ("template".into(), "minecraft:chicken".into()),
                        ("width".into(), "0.6".into()),
                        ("height".into(), "0.8".into()),
                        ("tracking_range".into(), "64".into()),
                    ]),
                },
            ),
            RegistryEntry::typed(
                "minecraft:data_component_type",
                "ruby:mana",
                RegistryDeclaration::Custom {
                    kind: "data_component".into(),
                    properties: BTreeMap::from([
                        ("codec".into(), "ruby:mana".into()),
                        ("persistent".into(), "true".into()),
                        ("network_synced".into(), "true".into()),
                    ]),
                },
            ),
            RegistryEntry::typed(
                "neoforge:attachment",
                "ruby:mana",
                RegistryDeclaration::Custom {
                    kind: "attachment".into(),
                    properties: BTreeMap::from([
                        ("codec".into(), "ruby:mana".into()),
                        ("default".into(), "0".into()),
                        ("copy_on_death".into(), "true".into()),
                    ]),
                },
            ),
        ];
        let declarations = NativeDeclarations::from_registry_entries(rows.iter()).unwrap();
        assert_eq!(declarations.custom, []);
        assert_eq!(declarations.entities[0].template, "minecraft:chicken");
        assert_eq!(declarations.entities[0].properties["tracking_range"], "64");
        assert!(declarations.data_components[0].network_synced);
        assert_eq!(declarations.attachments[0].default_value, "0");
        assert!(declarations.attachments[0].copy_on_death);
    }

    #[test]
    fn typed_live_rows_export_planner_files() {
        let entity = RegistryEntry::typed(
            "minecraft:entity_type",
            "ruby:bird",
            RegistryDeclaration::Custom {
                kind: "entity".into(),
                properties: BTreeMap::from([("template".into(), "minecraft:chicken".into())]),
            },
        );
        let component = RegistryEntry::typed(
            "minecraft:data_component_type",
            "ruby:mana",
            RegistryDeclaration::Custom {
                kind: "data_component".into(),
                properties: BTreeMap::from([("codec".into(), "ruby:mana".into())]),
            },
        );
        let attachment = RegistryEntry::typed(
            "neoforge:attachment",
            "ruby:mana",
            RegistryDeclaration::Custom {
                kind: "attachment".into(),
                properties: BTreeMap::from([("codec".into(), "ruby:mana".into())]),
            },
        );
        let content = crate::Content::from_native(
            "ruby",
            NativeDeclarations::from_registry_entries([&entity, &component, &attachment]).unwrap(),
        )
        .unwrap();
        assert_eq!(content.entities.len(), 1);
        assert_eq!(content.data_components.len(), 1);
        assert_eq!(content.attachments.len(), 1);
        assert!(
            content
                .native_files
                .contains_key("data/ruby/horizon/entity/bird.json")
        );
        assert!(
            content
                .native_files
                .contains_key("data/ruby/horizon/data_component/mana.json")
        );
        assert!(
            content
                .native_files
                .contains_key("data/ruby/horizon/attachment/mana.json")
        );
    }

    #[test]
    fn declaration_merge_coalesces_equal_rows_and_sorts_custom_kinds() {
        let entity = RegistryEntry::typed(
            "minecraft:entity_type",
            "ruby:zebra",
            RegistryDeclaration::Custom {
                kind: "entity".into(),
                properties: BTreeMap::from([("template".into(), "minecraft:horse".into())]),
            },
        );
        let attachment = RegistryEntry::typed(
            "neoforge:attachment",
            "ruby:mana",
            RegistryDeclaration::Custom {
                kind: "attachment".into(),
                properties: BTreeMap::from([("codec".into(), "ruby:mana".into())]),
            },
        );
        let left = NativeDeclarations::from_registry_entries([&entity]).unwrap();
        let right = NativeDeclarations::from_registry_entries([&attachment, &entity]).unwrap();
        let mut merged = left;
        merged.merge(right).unwrap();
        assert_eq!(merged.entities.len(), 1);
        assert_eq!(merged.attachments[0].id, "ruby:mana");
        assert!(merged.custom.is_empty());

        let custom_a = NativeDeclarations {
            custom: vec![CustomDeclaration {
                registry: "z:registry".into(),
                id: "ruby:z".into(),
                kind: "future".into(),
                properties: BTreeMap::new(),
            }],
            ..NativeDeclarations::default()
        };
        let custom_b = NativeDeclarations {
            custom: vec![CustomDeclaration {
                registry: "a:registry".into(),
                id: "ruby:a".into(),
                kind: "future".into(),
                properties: BTreeMap::new(),
            }],
            ..NativeDeclarations::default()
        };
        merged.merge(custom_a).unwrap();
        merged.merge(custom_b).unwrap();
        assert_eq!(
            merged
                .custom
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["ruby:a", "ruby:z"]
        );
    }

    #[test]
    fn declaration_merge_rejects_conflicts_and_rolls_back() {
        let first = RegistryEntry::typed(
            "minecraft:entity_type",
            "ruby:bird",
            RegistryDeclaration::Custom {
                kind: "entity".into(),
                properties: BTreeMap::from([("template".into(), "minecraft:chicken".into())]),
            },
        );
        let second = RegistryEntry::typed(
            "minecraft:entity_type",
            "ruby:bird",
            RegistryDeclaration::Custom {
                kind: "entity".into(),
                properties: BTreeMap::from([("template".into(), "minecraft:bat".into())]),
            },
        );
        let mut declarations = NativeDeclarations::from_registry_entries([&first]).unwrap();
        let error = declarations
            .merge(NativeDeclarations::from_registry_entries([&second]).unwrap())
            .unwrap_err();
        assert!(error.contains("conflicting native entity"), "{error}");
        assert_eq!(declarations.entities[0].template, "minecraft:chicken");
    }

    #[test]
    fn native_registration_fences_and_payloads_are_bounded() {
        let vanilla = RegistryEntry::typed(
            "minecraft:item",
            "minecraft:stone",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::new(),
            },
        );
        let error = NativeDeclarations::from_registry_entries([&vanilla]).unwrap_err();
        assert!(
            error.contains("cannot redefine the minecraft namespace"),
            "{error}"
        );

        let block = RegistryEntry::typed(
            "minecraft:block",
            "ruby:shared",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::new(),
                item: true,
            },
        );
        let item = RegistryEntry::typed(
            "minecraft:item",
            "ruby:shared",
            RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: BTreeMap::new(),
            },
        );
        let mut declarations = NativeDeclarations::default();
        declarations.register_typed(&block).unwrap();
        let error = declarations.register_typed(&item).unwrap_err();
        assert!(error.contains("conflicts with a block item"), "{error}");

        let effect = RegistryEntry::typed(
            "minecraft:mob_effect",
            "ruby:bad_color",
            RegistryDeclaration::MobEffect {
                category: "beneficial".into(),
                color: 0x1_00_00_00,
            },
        );
        let error = NativeDeclarations::from_registry_entries([&effect]).unwrap_err();
        assert!(error.contains("color is RGB"), "{error}");

        let oversized = format!(r#"{{"payload":"{}"}}"#, "x".repeat(MAX_NATIVE_JSON));
        let recipe = RegistryEntry::typed(
            "minecraft:recipe",
            "ruby:oversized",
            RegistryDeclaration::Recipe { json: oversized },
        );
        let error = NativeDeclarations::from_registry_entries([&recipe]).unwrap_err();
        assert!(error.contains("exceeds"), "{error}");
    }

    #[test]
    fn native_custom_properties_are_bounded() {
        let properties = (0..=MAX_NATIVE_PROPERTIES)
            .map(|index| (format!("p{index}"), "x".to_owned()))
            .collect();
        let entry = RegistryEntry::typed(
            "minecraft:entity_type",
            "ruby:too_many",
            RegistryDeclaration::Custom {
                kind: "entity".into(),
                properties,
            },
        );
        let error = NativeDeclarations::from_registry_entries([&entry]).unwrap_err();
        assert!(error.contains("more than"), "{error}");
    }

    /// A typed custom row: the shape loader adapters use for entity,
    /// data-component, attachment, menu and block-entity registrations.
    fn custom(kind: &str, id: &str, properties: &[(&str, &str)]) -> RegistryEntry {
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

    #[test]
    fn entity_declaration_parses_typed_spawn_and_attributes() {
        let entry = custom(
            "entity",
            "ruby:bird",
            &[
                ("template", "minecraft:chicken"),
                ("width", "0.6"),
                ("height", "0.8"),
                ("eye_height", "0.7"),
                ("category", "creature"),
                ("tracking_range", "64"),
                ("update_interval", "3"),
                ("fire_immune", "false"),
                ("max_health", "4"),
                ("speed", "0.25"),
                ("attack_damage", "0"),
                ("follow_range", "16.0"),
                ("spawn_category", "on_ground"),
                ("spawn_heightmap", "motion_blocking"),
                ("spawn_min_height", "-64"),
                ("spawn_max_height", "320"),
            ],
        );
        let declarations = NativeDeclarations::from_registry_entries([&entry]).unwrap();
        let entity = &declarations.entities[0];
        assert_eq!(
            entity.spawn,
            Some(SpawnRestriction {
                category: "ON_GROUND".into(),
                heightmap: "MOTION_BLOCKING".into(),
                min_height: Some(-64),
                max_height: Some(320),
            })
        );
        assert_eq!(
            entity.attributes,
            Some(EntityAttributes {
                health: Some(4.0),
                speed: Some(0.25),
                attack_damage: Some(0.0),
                follow_range: Some(16.0),
            })
        );
        // The scalars stay in `properties` for the live registry assembler.
        assert_eq!(entity.properties["category"], "creature");

        let parse = |properties: &[(&str, &str)]| {
            let properties = properties
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            EntityDeclaration::from_registry_properties(
                "ruby:bird",
                "minecraft:chicken",
                &properties,
            )
        };
        let error = parse(&[("weigth", "1")]).unwrap_err();
        assert!(
            error.contains("unknown native entity property \"weigth\""),
            "{error}"
        );
        let error = parse(&[("spawn_category", "IN_WATER")]).unwrap_err();
        assert!(
            error.contains("needs spawn_category and spawn_heightmap"),
            "{error}"
        );
        let error = parse(&[("speed", "NaN")]).unwrap_err();
        assert!(error.contains("speed is a number"), "{error}");
        let error = parse(&[("width", "65")]).unwrap_err();
        assert!(error.contains("width is a number from 0 to"), "{error}");
        let error = parse(&[
            ("spawn_category", "ON_GROUND"),
            ("spawn_heightmap", "WORLD_SURFACE"),
            ("spawn_min_height", "10"),
            ("spawn_max_height", "9"),
        ])
        .unwrap_err();
        assert!(error.contains("is reversed"), "{error}");
        // No typed rows for an entity that declares neither.
        let bare = parse(&[("width", "0.6")]).unwrap();
        assert_eq!((bare.spawn, bare.attributes), (None, None));
    }

    #[test]
    fn auxiliary_rows_reject_bad_codecs_and_stay_atomic() {
        let row = |id: &str, codec: &str| custom("data_component", id, &[("codec", codec)]);
        let mut declarations = NativeDeclarations::default();
        declarations
            .register_entries([&row("ruby:mana", "ruby:mana")])
            .unwrap();
        let error = declarations
            .register_entries([&row("ruby:grit", "ruby:grit"), &row("ruby:broken", "mana")])
            .unwrap_err();
        assert!(error.contains("has no namespace"), "{error}");
        assert_eq!(
            declarations
                .data_components
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["ruby:mana"]
        );

        // An attachment default is free text, but not control characters.
        let control = custom(
            "attachment",
            "ruby:tab",
            &[("codec", "ruby:tab"), ("default", "a\tb")],
        );
        let error = declarations.register_entries([&control]).unwrap_err();
        assert!(error.contains("control characters"), "{error}");
        assert!(declarations.attachments.is_empty());
    }

    #[test]
    fn diagnostics_summarise_counts_and_custom_kinds() {
        let block = RegistryEntry::typed(
            "minecraft:block",
            "ruby:lamp",
            RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: BTreeMap::new(),
                item: false,
            },
        );
        let effect = RegistryEntry::typed(
            "minecraft:mob_effect",
            "ruby:glow",
            RegistryDeclaration::MobEffect {
                category: "beneficial".into(),
                color: 0x55aa11,
            },
        );
        let mut declarations =
            NativeDeclarations::from_registry_entries([&block, &effect]).unwrap();
        assert_eq!(
            declarations.diagnostics(),
            [
                "native declarations: 1 blocks, 0 items, 1 mob effects, 0 entities, 0 data components, 0 attachments, 0 menus, 0 block entities, 0 recipes, 0 ingredients, 0 custom"
            ]
        );
        for (registry, id, kind) in [
            ("minecraft:entity_type", "ruby:bird", "entity"),
            ("neoforge:attachment", "ruby:mana", "attachment"),
            ("future:thing", "ruby:widget", "entity"),
        ] {
            declarations.custom.push(CustomDeclaration {
                registry: registry.into(),
                id: id.into(),
                kind: kind.into(),
                properties: BTreeMap::new(),
            });
        }
        assert_eq!(
            declarations.diagnostics()[1],
            "native custom registration kinds: attachment=1, entity=2"
        );
        assert_eq!(declarations.diagnostics().len(), 2);
    }

    #[test]
    fn menu_and_block_entity_rows_survive_the_registry_batch() {
        let menu = custom(
            "menu",
            "ruby:crate",
            &[
                ("title_key", "container.ruby.crate"),
                ("slots", "27"),
                // Out of order and spaced: the row keeps a sorted set.
                ("open_blocks", "ruby:crate_block, minecraft:barrel"),
            ],
        );
        // Out of order and spaced: the row keeps a sorted set either way.
        let block_entity = custom(
            "block_entity",
            "ruby:crate",
            &[
                ("template", "minecraft:barrel"),
                ("ticker", "true"),
                ("valid_blocks", "ruby:crate_block, minecraft:barrel"),
            ],
        );
        let declarations =
            NativeDeclarations::from_registry_entries([&menu, &block_entity]).unwrap();
        // Both kinds are typed, so neither lands in the custom bucket.
        assert_eq!(declarations.custom, []);
        assert_eq!(
            declarations.menus[0],
            MenuDeclaration {
                id: "ruby:crate".into(),
                title_key: "container.ruby.crate".into(),
                slots: 27,
                open_blocks: vec!["minecraft:barrel".into(), "ruby:crate_block".into()],
            }
        );
        assert_eq!(
            declarations.block_entities[0],
            BlockEntityDeclaration {
                id: "ruby:crate".into(),
                template: "minecraft:barrel".into(),
                ticker: true,
                valid_blocks: vec!["minecraft:barrel".into(), "ruby:crate_block".into()],
            }
        );
        assert!(declarations.diagnostics()[0].contains("1 menus, 1 block entities"));
        // A kind without a typed adapter still reaches the custom bucket.
        let unknown = custom("widget", "ruby:widget", &[]);
        let declarations = NativeDeclarations::from_registry_entries([&unknown]).unwrap();
        assert_eq!(declarations.custom[0].kind, "widget");
    }

    #[test]
    fn menu_and_block_entity_rows_reject_bad_values_atomically() {
        let menu = |id: &str, title: &str, slots: &str| {
            custom("menu", id, &[("title_key", title), ("slots", slots)])
        };
        let block_entity = |id: &str, template: &str, blocks: &str| {
            custom(
                "block_entity",
                id,
                &[("template", template), ("valid_blocks", blocks)],
            )
        };
        let menu_with_open = |id: &str, blocks: &str| {
            custom(
                "menu",
                id,
                &[
                    ("title_key", "container.ruby"),
                    ("slots", "9"),
                    ("open_blocks", blocks),
                ],
            )
        };
        let mut declarations = NativeDeclarations::default();
        declarations
            .register_entries([&menu("ruby:crate", "container.ruby", "9")])
            .unwrap();
        let too_many = (0..=MAX_BLOCK_ENTITY_BLOCKS)
            .map(|index| format!("ruby:b{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let too_many_blocks = (0..=MAX_MENU_OPEN_BLOCKS)
            .map(|index| format!("ruby:b{index}"))
            .collect::<Vec<_>>()
            .join(",");
        for (row, why) in [
            (menu("ruby:blank", "container ruby", "9"), "title_key"),
            (menu("ruby:wide", "container.ruby", "257"), "is 0 to"),
            (
                custom("menu", "ruby:missing", &[("title_key", "ruby")]),
                "has no slots",
            ),
            (
                custom(
                    "menu",
                    "ruby:extra",
                    &[("title_key", "ruby"), ("slots", "9"), ("tabs", "2")],
                ),
                "unknown native menu property",
            ),
            (
                menu_with_open("ruby:repeat_blocks", "ruby:a,ruby:a"),
                "open_blocks repeats an id",
            ),
            (
                menu_with_open("ruby:bare_block", "crate"),
                "has no namespace",
            ),
            (
                menu_with_open("ruby:many_blocks", &too_many_blocks),
                "more than",
            ),
            (
                block_entity("ruby:template", "ruby:barrel", "ruby:crate"),
                "must be a vanilla id",
            ),
            (
                block_entity("ruby:bare", "minecraft:barrel", "crate"),
                "has no namespace",
            ),
            (
                block_entity("ruby:empty", "minecraft:barrel", ""),
                "empty id",
            ),
            (
                block_entity("ruby:repeat", "minecraft:barrel", "ruby:a,ruby:a"),
                "repeats an id",
            ),
            (
                custom(
                    "block_entity",
                    "ruby:none",
                    &[("template", "minecraft:barrel")],
                ),
                "has no valid_blocks",
            ),
            (
                block_entity("ruby:many", "minecraft:barrel", &too_many),
                "more than",
            ),
        ] {
            let error = declarations.register_entries([&row]).unwrap_err();
            assert!(error.contains(why), "{error}");
            // The batch rolled back: the good menu is still the only one.
            assert_eq!(declarations.menus.len(), 1);
            assert!(declarations.block_entities.is_empty());
        }
        let error = declarations
            .register_entries([&menu("ruby:crate", "container.ruby", "9")])
            .unwrap_err();
        assert!(error.contains("duplicate native menu"), "{error}");
    }

    #[test]
    fn menu_open_blocks_parse_sorted_and_fenced() {
        let menu = MenuDeclaration::from_registry_properties(
            "ruby:crate",
            &BTreeMap::from([
                ("title_key".to_owned(), "container.ruby.crate".to_owned()),
                ("slots".to_owned(), "27".to_owned()),
                (
                    "open_blocks".to_owned(),
                    "ruby:crate_block, minecraft:barrel".to_owned(),
                ),
            ]),
        )
        .unwrap();
        assert_eq!(menu.open_blocks, ["minecraft:barrel", "ruby:crate_block"]);
        // Leaving the key out leaves the menu bound to no block.
        let unbound = MenuDeclaration::from_registry_properties(
            "ruby:loose",
            &BTreeMap::from([
                ("title_key".to_owned(), "container.ruby.loose".to_owned()),
                ("slots".to_owned(), "0".to_owned()),
            ]),
        )
        .unwrap();
        assert!(unbound.open_blocks.is_empty());

        let parse = |value: &str| {
            MenuDeclaration::from_registry_properties(
                "ruby:crate",
                &BTreeMap::from([
                    ("title_key".to_owned(), "container.ruby.crate".to_owned()),
                    ("slots".to_owned(), "27".to_owned()),
                    ("open_blocks".to_owned(), value.to_owned()),
                ]),
            )
        };
        for (value, why) in [
            ("", "empty id"),
            ("crate", "has no namespace"),
            ("ruby:a,ruby:a", "repeats an id"),
        ] {
            let error = parse(value).unwrap_err();
            assert!(error.contains(why), "{value:?}: {error}");
        }
        let too_many = (0..=MAX_MENU_OPEN_BLOCKS)
            .map(|index| format!("ruby:b{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let error = parse(&too_many).unwrap_err();
        assert!(error.contains("more than"), "{error}");

        // A hand-built row passes the same fences.
        let mut hand_built = menu.clone();
        hand_built.open_blocks = vec!["ruby:a".into(), "ruby:a".into()];
        assert!(hand_built.validate().is_err());
        hand_built.open_blocks = vec!["crate".into()];
        assert!(hand_built.validate().is_err());
        hand_built.open_blocks = (0..=MAX_MENU_OPEN_BLOCKS)
            .map(|index| format!("ruby:b{index}"))
            .collect();
        assert!(hand_built.validate().is_err());
    }
}
