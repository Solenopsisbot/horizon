//! Typed declarations exported by the native Java compatibility boundary.
//!
//! Java registration callbacks do not carry enough information to construct a
//! Horizon block or item by themselves. A loader adapter first enriches a
//! [`horizon_native_java::RegistryEntry`] with a typed declaration, then this
//! sink validates the small declaration vocabulary used by the content loader.
//! It deliberately collects declarations; the game applies them before its
//! immutable resource tables are built.

use crate::content::{Block, BlockProperties, EffectCategory, Item, MobEffect, Rarity};
use horizon_native_java::{RegistryDeclaration, RegistryEntry, TypedRegistrySink};
use std::collections::BTreeMap;

/// Native declarations are metadata, not an unbounded Java object graph.
pub const MAX_NATIVE_DECLARATIONS: usize = 8_192;
pub const MAX_NATIVE_PROPERTIES: usize = 32;
pub const MAX_NATIVE_TEXT: usize = 4_096;

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

/// A bounded entity registration carried by a loader adapter. The native
/// world owns the actual entity implementation; this record preserves the
/// vanilla template and scalar builder options until the live registry is
/// assembled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityDeclaration {
    pub id: String,
    pub template: String,
    pub properties: BTreeMap<String, String>,
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

/// Typed registrations waiting for the pre-resource load phase.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct NativeDeclarations {
    pub blocks: Vec<Block>,
    pub items: Vec<Item>,
    pub effects: Vec<MobEffect>,
    pub entities: Vec<EntityDeclaration>,
    pub data_components: Vec<DataComponentDeclaration>,
    pub attachments: Vec<AttachmentDeclaration>,
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
        Self::ensure_id(&entry.id)?;
        Self::check_properties("block", properties)?;
        Self::ensure_id(template)?;
        if self.blocks.iter().any(|block| block.id == entry.id) {
            return Err(format!("duplicate native block {}", entry.id));
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
        Self::ensure_id(&entry.id)?;
        Self::check_properties("item", properties)?;
        Self::ensure_id(template)?;
        if self.items.iter().any(|item| item.id == entry.id) {
            return Err(format!("duplicate native item {}", entry.id));
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
        Self::ensure_id(&entry.id)?;
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
        self.recipes.push(RecipeDeclaration {
            id: entry.id.clone(),
            json,
        });
        Ok(())
    }

    fn add_ingredient(&mut self, entry: &RegistryEntry, json: &str) -> Result<(), String> {
        Self::ensure_id(&entry.id)?;
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
        Self::ensure_id(&entry.id)?;
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
        for key in retained.keys() {
            if !matches!(
                key.as_str(),
                "width"
                    | "height"
                    | "tracking_range"
                    | "update_interval"
                    | "fire_immune"
                    | "summonable"
                    | "spawnable"
            ) {
                return Err(format!("unknown native entity property {key:?}"));
            }
        }
        for key in ["width", "height"] {
            if let Some(value) = retained.get(key) {
                let parsed = parse_f32(key, value)?;
                if parsed < 0.0 {
                    return Err(format!("native entity property {key} is 0 or more"));
                }
            }
        }
        for key in ["tracking_range", "update_interval"] {
            if let Some(value) = retained.get(key) {
                value.parse::<u16>().map_err(|_| {
                    format!("native entity property {key} is not an unsigned short: {value:?}")
                })?;
            }
        }
        for key in ["fire_immune", "summonable", "spawnable"] {
            if let Some(value) = retained.get(key) {
                parse_bool(key, value)?;
            }
        }
        self.entities.push(EntityDeclaration {
            id: entry.id.clone(),
            template: template.clone(),
            properties: retained,
        });
        Ok(())
    }

    fn add_data_component(
        &mut self,
        entry: &RegistryEntry,
        properties: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        Self::ensure_id(&entry.id)?;
        Self::check_properties("data component", properties)?;
        let codec = properties
            .get("codec")
            .ok_or_else(|| format!("native data component {} has no codec", entry.id))?;
        Self::ensure_id(codec)?;
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
        Self::ensure_id(&entry.id)?;
        Self::check_properties("attachment", properties)?;
        let codec = properties
            .get("codec")
            .ok_or_else(|| format!("native attachment {} has no codec", entry.id))?;
        Self::ensure_id(codec)?;
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
                Self::ensure_id(&entry.id)?;
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
                    _ => {}
                }
                Self::ensure_id(&entry.id)?;
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
        assert_eq!(declarations.data_components[0].network_synced, true);
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
}
