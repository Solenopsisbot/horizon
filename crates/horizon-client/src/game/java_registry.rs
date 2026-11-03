//! Bounded Java content registry state owned by a server thread.
//!
//! The resource root remains the source for vanilla block/item tables. These
//! auxiliary rows are kept separately so Java declarations can be observed by
//! the authoritative server before a future full registry reload wires them
//! into entity/component/attachment stores.
use std::sync::{OnceLock, RwLock};

use horizon_mods::content::{BlockProperties, BlockShapes, Rarity};
use horizon_mods::native::{AttachmentDeclaration, DataComponentDeclaration, EntityDeclaration};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct BlockTemplate {
    pub id: String,
    pub template: String,
    pub properties: BlockProperties,
    pub shapes: Option<BlockShapes>,
    pub default_state: BTreeMap<String, String>,
    pub item: bool,
    pub components: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ItemTemplate {
    pub id: String,
    pub template: String,
    pub max_stack_size: Option<u8>,
    pub rarity: Option<Rarity>,
    pub components: BTreeMap<String, serde_json::Value>,
    pub crafting_remainder: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct JavaRegistrySnapshot {
    pub blocks: Vec<String>,
    pub items: Vec<String>,
    pub entities: Vec<String>,
    pub data_components: Vec<String>,
    pub attachments: Vec<String>,
    pub block_templates: Vec<BlockTemplate>,
    pub item_templates: Vec<ItemTemplate>,
    pub entity_declarations: Vec<EntityDeclaration>,
    pub data_component_declarations: Vec<DataComponentDeclaration>,
    pub attachment_declarations: Vec<AttachmentDeclaration>,
}

#[allow(dead_code)]
impl JavaRegistrySnapshot {
    pub(super) fn block_template(&self, id: &str) -> Option<&BlockTemplate> {
        self.block_templates
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.block_templates[index])
    }

    pub(super) fn item_template(&self, id: &str) -> Option<&ItemTemplate> {
        self.item_templates
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.item_templates[index])
    }

    pub(super) fn entity(&self, id: &str) -> Option<&EntityDeclaration> {
        self.entity_declarations
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.entity_declarations[index])
    }

    pub(super) fn data_component(&self, id: &str) -> Option<&DataComponentDeclaration> {
        self.data_component_declarations
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.data_component_declarations[index])
    }

    pub(super) fn attachment(&self, id: &str) -> Option<&AttachmentDeclaration> {
        self.attachment_declarations
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.attachment_declarations[index])
    }
}

static SNAPSHOT: OnceLock<RwLock<JavaRegistrySnapshot>> = OnceLock::new();

fn state() -> &'static RwLock<JavaRegistrySnapshot> {
    SNAPSHOT.get_or_init(|| RwLock::new(JavaRegistrySnapshot::default()))
}

/// Apply typed auxiliary declarations in deterministic id order.
///
/// Duplicate rows from repeated startup calls coalesce, while the snapshot is
/// replaced atomically so a server thread never exposes a partially applied
/// Java registration batch.
pub(super) fn synchronize(
    mods: &[horizon_client::mod_content::ContentMod],
) -> JavaRegistrySnapshot {
    let mut next = JavaRegistrySnapshot {
        blocks: mods
            .iter()
            .flat_map(|m| m.content.blocks.iter().map(|row| row.id.clone()))
            .collect(),
        items: mods
            .iter()
            .flat_map(|m| m.content.items.iter().map(|row| row.id.clone()))
            .collect(),
        entities: mods
            .iter()
            .flat_map(|m| m.content.entities.iter().map(|row| row.id.clone()))
            .collect(),
        data_components: mods
            .iter()
            .flat_map(|m| m.content.data_components.iter().map(|row| row.id.clone()))
            .collect(),
        attachments: mods
            .iter()
            .flat_map(|m| m.content.attachments.iter().map(|row| row.id.clone()))
            .collect(),
        block_templates: mods
            .iter()
            .flat_map(|m| {
                m.content.blocks.iter().map(|row| BlockTemplate {
                    id: row.id.clone(),
                    template: row.template.clone(),
                    properties: row.properties.clone(),
                    shapes: row.shapes.clone(),
                    default_state: row.default_state.clone(),
                    item: row.item,
                    components: row.components.clone(),
                })
            })
            .collect(),
        item_templates: mods
            .iter()
            .flat_map(|m| {
                m.content.items.iter().map(|row| ItemTemplate {
                    id: row.id.clone(),
                    template: row.template.clone(),
                    max_stack_size: row.max_stack_size,
                    rarity: row.rarity,
                    components: row.components.clone(),
                    crafting_remainder: row.crafting_remainder.clone(),
                })
            })
            .collect(),
        entity_declarations: mods
            .iter()
            .flat_map(|m| m.content.entities.iter().cloned())
            .collect(),
        data_component_declarations: mods
            .iter()
            .flat_map(|m| m.content.data_components.iter().cloned())
            .collect(),
        attachment_declarations: mods
            .iter()
            .flat_map(|m| m.content.attachments.iter().cloned())
            .collect(),
    };
    next.blocks.sort();
    next.blocks.dedup();
    next.items.sort();
    next.items.dedup();
    next.entities.sort();
    next.entities.dedup();
    next.data_components.sort();
    next.data_components.dedup();
    next.attachments.sort();
    next.attachments.dedup();
    next.block_templates.sort_by(|a, b| a.id.cmp(&b.id));
    next.block_templates.dedup_by(|a, b| a.id == b.id);
    next.item_templates.sort_by(|a, b| a.id.cmp(&b.id));
    next.item_templates.dedup_by(|a, b| a.id == b.id);
    next.entity_declarations.sort_by(|a, b| a.id.cmp(&b.id));
    next.entity_declarations.dedup_by(|a, b| a.id == b.id);
    next.data_component_declarations
        .sort_by(|a, b| a.id.cmp(&b.id));
    next.data_component_declarations
        .dedup_by(|a, b| a.id == b.id);
    next.attachment_declarations.sort_by(|a, b| a.id.cmp(&b.id));
    next.attachment_declarations.dedup_by(|a, b| a.id == b.id);
    *state().write().expect("java registry lock poisoned") = next.clone();
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_mods::content::Content;
    use horizon_mods::native::{
        AttachmentDeclaration, DataComponentDeclaration, EntityDeclaration, NativeDeclarations,
    };
    use std::collections::BTreeMap;

    #[test]
    fn auxiliary_rows_are_sorted_deduplicated_and_atomic() {
        let declarations = NativeDeclarations {
            blocks: vec![horizon_mods::content::Block {
                id: "ruby:stone".into(),
                template: "minecraft:stone".into(),
                properties: Default::default(),
                shapes: None,
                default_state: Default::default(),
                item: true,
                components: Default::default(),
            }],
            items: vec![horizon_mods::content::Item {
                id: "ruby:shard".into(),
                template: "minecraft:stone".into(),
                max_stack_size: None,
                rarity: None,
                components: Default::default(),
                crafting_remainder: None,
            }],
            entities: vec![EntityDeclaration {
                id: "ruby:zombie".into(),
                template: "minecraft:zombie".into(),
                properties: BTreeMap::new(),
                ..EntityDeclaration::default()
            }],
            data_components: vec![DataComponentDeclaration {
                id: "ruby:power".into(),
                codec: "minecraft:int".into(),
                persistent: true,
                network_synced: true,
            }],
            attachments: vec![AttachmentDeclaration {
                id: "ruby:power".into(),
                codec: "minecraft:int".into(),
                default_value: "0".into(),
                copy_on_death: true,
            }],
            ..NativeDeclarations::default()
        };
        let content = Content::from_native("ruby", declarations).unwrap();
        let snapshot = synchronize(&[
            horizon_client::mod_content::ContentMod {
                id: "ruby".into(),
                content: content.clone(),
            },
            horizon_client::mod_content::ContentMod {
                id: "ruby-duplicate".into(),
                content,
            },
        ]);
        assert_eq!(snapshot.blocks, ["ruby:stone"]);
        assert_eq!(snapshot.items, ["ruby:shard"]);
        assert_eq!(snapshot.entities, ["ruby:zombie"]);
        assert_eq!(snapshot.data_components, ["ruby:power"]);
        assert_eq!(snapshot.attachments, ["ruby:power"]);
        assert_eq!(
            snapshot.block_template("ruby:stone").unwrap().template,
            "minecraft:stone"
        );
        assert_eq!(
            snapshot.item_template("ruby:shard").unwrap().id,
            "ruby:shard"
        );
        assert_eq!(
            snapshot.entity("ruby:zombie").unwrap().template,
            "minecraft:zombie"
        );
        assert_eq!(
            snapshot.data_component("ruby:power").unwrap().codec,
            "minecraft:int"
        );
        assert_eq!(
            snapshot.attachment("ruby:power").unwrap().default_value,
            "0"
        );
        assert!(snapshot.block_template("ruby:missing").is_none());
        assert!(snapshot.item_template("ruby:missing").is_none());
        assert!(snapshot.entity("ruby:missing").is_none());
        assert!(snapshot.data_component("ruby:missing").is_none());
        assert!(snapshot.attachment("ruby:missing").is_none());
        assert_eq!(*state().read().unwrap(), snapshot);
    }
}
