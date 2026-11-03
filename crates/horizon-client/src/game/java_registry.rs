//! Bounded Java content registry state owned by a server thread.
//!
//! The resource root remains the source for vanilla block/item tables. These
//! auxiliary rows are kept separately so Java declarations can be observed by
//! the authoritative server before a future full registry reload wires them
//! into entity/component/attachment stores.
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct JavaRegistrySnapshot {
    pub blocks: Vec<String>,
    pub items: Vec<String>,
    pub entities: Vec<String>,
    pub data_components: Vec<String>,
    pub attachments: Vec<String>,
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
        assert_eq!(snapshot.items, ["ruby:pickaxe"]);
        assert_eq!(snapshot.entities, ["ruby:zombie"]);
        assert_eq!(snapshot.data_components, ["ruby:power"]);
        assert_eq!(snapshot.attachments, ["ruby:power"]);
        assert_eq!(*state().read().unwrap(), snapshot);
    }
}
