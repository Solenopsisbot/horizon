//! The declaration tables of [`crate::native`] that are not blocks or items:
//! data components and attachments, and the rule deciding which declared
//! recipes a content root may load.
//!
//! Data components (`DataComponentType.builder()...build()` registered into
//! `minecraft:data_component_type`, NeoForge's
//! `DeferredRegister.DataComponents`) and attachments (Fabric's
//! `AttachmentRegistry`, NeoForge's `AttachmentType`) are code: their codecs are
//! Java objects the bounded runtime does not model. What crosses the boundary
//! is the shape the builder chain names: whether the value is saved
//! (`persistent`/`serialize`), synced (`networkSynchronized`/`syncWith`),
//! kept on respawn (`copyOnDeath`), and a codec's label when the runtime
//! knows one. The rows travel in the registry snapshot and the login manifest
//! so a guest and diagnostics see the same ids; no component value is
//! encoded from them.
//!
//! A declared recipe is data and becomes `data/<ns>/recipe/<path>.json` in
//! the content root, but only when the game can hold what it names: a
//! vanilla recipe type (a mod's own serializer is code), and items that are
//! vanilla or declared by the same set. A recipe naming an item the jar
//! registered opaquely would craft an id no registry numbers, so it is left
//! out with a reason instead ([`recipe_refusal`]).
use std::collections::{BTreeMap, BTreeSet};

/// The longest codec label a row keeps; a label is a name, not a payload.
pub const MAX_CODEC_LABEL: usize = 256;

/// A typed data component type registration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DataComponentDeclaration {
    pub id: String,
    /// `Builder.persistent(codec)`: the component is saved with its stack.
    pub persistent: bool,
    /// `Builder.networkSynchronized(codec)`: it is sent to clients.
    pub network: bool,
    /// `Builder.cacheEncoding()`.
    pub cache_encoding: bool,
    /// The persistent codec's label, when the runtime could name it.
    pub codec: Option<String>,
}

impl DataComponentDeclaration {
    /// Parses one row from the keys a loader adapter records: `persistent`,
    /// `network`, `cache_encoding` (booleans, default false) and `codec` (a
    /// bounded label). Unknown keys are refused.
    pub fn from_registry_properties(
        id: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut row = Self {
            id: id.into(),
            ..Self::default()
        };
        for (key, value) in properties {
            match key.as_str() {
                "persistent" => row.persistent = parse_flag(key, value)?,
                "network" => row.network = parse_flag(key, value)?,
                "cache_encoding" => row.cache_encoding = parse_flag(key, value)?,
                "codec" => row.codec = Some(codec_label(key, value)?),
                other => {
                    return Err(format!("unknown native data component property {other:?}"));
                }
            }
        }
        Ok(row)
    }
}

/// A typed attachment type registration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentDeclaration {
    pub id: String,
    /// Saved with its holder (`persistent`, NeoForge `serialize`).
    pub persistent: bool,
    /// Synced to clients (`syncWith`, NeoForge `sync`).
    pub synced: bool,
    /// Kept when a player respawns (`copyOnDeath`).
    pub copy_on_death: bool,
    /// Has a default value (`initializer`, `createDefaulted`, NeoForge's
    /// `AttachmentType.builder(Supplier)`).
    pub initializer: bool,
    /// The persistent codec's label, when the runtime could name it.
    pub codec: Option<String>,
}

impl AttachmentDeclaration {
    /// Parses one row: `persistent`, `synced`, `copy_on_death`, `initializer`
    /// (booleans, default false) and `codec`. Unknown keys are refused.
    pub fn from_registry_properties(
        id: impl Into<String>,
        properties: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut row = Self {
            id: id.into(),
            ..Self::default()
        };
        for (key, value) in properties {
            match key.as_str() {
                "persistent" => row.persistent = parse_flag(key, value)?,
                "synced" => row.synced = parse_flag(key, value)?,
                "copy_on_death" => row.copy_on_death = parse_flag(key, value)?,
                "initializer" => row.initializer = parse_flag(key, value)?,
                "codec" => row.codec = Some(codec_label(key, value)?),
                other => return Err(format!("unknown native attachment property {other:?}")),
            }
        }
        Ok(row)
    }
}

fn parse_flag(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("native property {key} is not boolean: {value:?}")),
    }
}

/// A codec label: printable ASCII without spaces, bounded.
fn codec_label(key: &str, value: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > MAX_CODEC_LABEL
        || !value.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(format!(
            "native property {key} is a label of 1 to {MAX_CODEC_LABEL} printable characters"
        ));
    }
    Ok(value.to_owned())
}

/// Keys of a recipe whose string values are not item ids: the recipe's own
/// type and book fields, pattern rows, load conditions and ingredient types.
const NOT_ITEMS: [&str; 11] = [
    "type",
    "category",
    "group",
    "pattern",
    "show_notification",
    "neoforge:ingredient_type",
    "fabric:type",
    "neoforge:conditions",
    "fabric:load_conditions",
    "conditions",
    "tag",
];

/// Why a declared recipe cannot load, or `None` when it can: its type is a
/// vanilla recipe type (`minecraft:...`), every item id it names is vanilla or
/// in `items`, and its result sets only vanilla components. Tags (`#ns:path`
/// or a `tag` key) are left to the game, which resolves an unknown tag to no
/// item and leaves the recipe out as vanilla does.
pub fn recipe_refusal(recipe: &serde_json::Value, items: &BTreeSet<&str>) -> Option<String> {
    let Some(object) = recipe.as_object() else {
        return Some("is not a JSON object".into());
    };
    let Some(kind) = object.get("type").and_then(serde_json::Value::as_str) else {
        return Some("has no type".into());
    };
    // An id without a namespace is vanilla's (`Identifier.parse`).
    if kind.contains(':') && !kind.starts_with("minecraft:") {
        return Some(format!(
            "type {kind} is a mod's own recipe serializer, which is code"
        ));
    }
    unresolved(recipe, items)
}

fn unresolved(value: &serde_json::Value, items: &BTreeSet<&str>) -> Option<String> {
    use serde_json::Value;
    match value {
        Value::Object(map) => map.iter().find_map(|(key, value)| {
            if NOT_ITEMS.contains(&key.as_str()) {
                return None;
            }
            if key == "components" {
                return value.as_object().and_then(|components| {
                    components.keys().find_map(|component| {
                        let kind = component.strip_prefix('!').unwrap_or(component);
                        (!kind.starts_with("minecraft:"))
                            .then(|| format!("sets component {kind}, which is not vanilla"))
                    })
                });
            }
            unresolved(value, items)
        }),
        Value::Array(values) => values.iter().find_map(|value| unresolved(value, items)),
        Value::String(text) => {
            let id = text.as_str();
            let looks_like_id = !id.starts_with('#')
                && id.split_once(':').is_some_and(|(ns, path)| {
                    !ns.is_empty()
                        && !path.is_empty()
                        && ns.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b)
                        })
                        && path.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b)
                        })
                });
            (looks_like_id && !id.starts_with("minecraft:") && !items.contains(id))
                .then(|| format!("names {id}, which no declaration provides"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn props(rows: &[(&str, &str)]) -> BTreeMap<String, String> {
        rows.iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn component_and_attachment_rows_parse_their_flags() {
        let component = DataComponentDeclaration::from_registry_properties(
            "ruby:charge",
            &props(&[("persistent", "true"), ("network", "true")]),
        )
        .unwrap();
        assert!(component.persistent && component.network && !component.cache_encoding);
        assert_eq!(component.codec, None);
        let attachment = AttachmentDeclaration::from_registry_properties(
            "ruby:mana",
            &props(&[("copy_on_death", "true"), ("codec", "codec/INT")]),
        )
        .unwrap();
        assert!(attachment.copy_on_death && !attachment.persistent);
        assert_eq!(attachment.codec.as_deref(), Some("codec/INT"));
        for bad in [
            props(&[("persistant", "true")]),
            props(&[("persistent", "yes")]),
            props(&[("codec", "two words")]),
        ] {
            assert!(DataComponentDeclaration::from_registry_properties("ruby:x", &bad).is_err());
        }
    }

    #[test]
    fn a_recipe_loads_only_when_every_item_resolves() {
        let items: BTreeSet<&str> = ["ruby:gem", "ruby:block"].into();
        let shaped = json!({
            "type": "minecraft:crafting_shaped",
            "pattern": ["##", "#X"],
            "key": {"#": "ruby:gem", "X": ["minecraft:stick", "#c:rods/wooden"]},
            "result": {"id": "ruby:block", "count": 1}
        });
        assert_eq!(recipe_refusal(&shaped, &items), None);
        let foreign = json!({
            "type": "minecraft:smelting",
            "ingredient": "ruby:ore",
            "result": {"id": "ruby:gem"}
        });
        assert_eq!(
            recipe_refusal(&foreign, &items).as_deref(),
            Some("names ruby:ore, which no declaration provides")
        );
        let bare = json!({"type": "crafting_shapeless", "ingredients": ["stick"], "result": {"id": "ruby:gem"}});
        assert_eq!(recipe_refusal(&bare, &items), None);
        let serializer = json!({"type": "farmersdelight:cutting", "result": []});
        assert!(
            recipe_refusal(&serializer, &items)
                .unwrap()
                .contains("own recipe serializer")
        );
        let component = json!({
            "type": "minecraft:crafting_shapeless",
            "ingredients": ["minecraft:stick"],
            "result": {"id": "ruby:gem", "components": {"ruby:charge": 3}}
        });
        assert!(
            recipe_refusal(&component, &items)
                .unwrap()
                .contains("ruby:charge")
        );
    }
}
