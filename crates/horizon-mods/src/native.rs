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

/// Typed registrations waiting for the pre-resource load phase.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct NativeDeclarations {
    pub blocks: Vec<Block>,
    pub items: Vec<Item>,
    pub effects: Vec<MobEffect>,
    pub recipes: Vec<RecipeDeclaration>,
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
        *self = staged;
        Ok(())
    }

    fn ensure_id(id: &str) -> Result<(), String> {
        let Some((namespace, path)) = id.split_once(':') else {
            return Err(format!("native registration {id:?} has no namespace"));
        };
        if namespace.is_empty()
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
            RegistryDeclaration::Custom { kind, properties } => {
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
}
