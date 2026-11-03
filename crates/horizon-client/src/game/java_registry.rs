//! Bounded Java content registry state owned by a server thread.
//!
//! The resource root remains the source for vanilla block/item tables. These
//! auxiliary rows are kept separately so Java declarations can be observed by
//! the authoritative server before a future full registry reload wires them
//! into entity/component/attachment stores.
use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use horizon_mods::content::{BlockProperties, BlockShapes, Rarity};
use horizon_mods::native::{
    AttachmentDeclaration, DataComponentDeclaration, EntityAttributes, EntityDeclaration,
    SpawnRestriction,
};

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
    /// Rows folded away while coalescing duplicates, by table name. Sorted, so
    /// two batches that coalesce the same rows report the same map.
    pub coalesced: BTreeMap<String, usize>,
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

    /// The spawn placement declared for `id`, when the adapter set one.
    pub(super) fn entity_spawn(&self, id: &str) -> Option<&SpawnRestriction> {
        self.entity(id).and_then(|row| row.spawn.as_ref())
    }

    /// The attribute defaults declared for `id`, when the adapter set any.
    pub(super) fn entity_attributes(&self, id: &str) -> Option<&EntityAttributes> {
        self.entity(id).and_then(|row| row.attributes.as_ref())
    }

    /// Deterministic one-line-per-table summary for the server-thread log.
    ///
    /// The id lists mirror the typed tables, so they are counted separately to
    /// show the registry a gameplay lookup sees; repeats folded by coalescing
    /// are reported on their own line, sorted by table name.
    pub(super) fn diagnostics(&self) -> Vec<String> {
        let mut out = vec![
            format!(
                "java registry: {} block ids, {} item ids, {} entity ids, {} data component ids, {} attachment ids",
                self.blocks.len(),
                self.items.len(),
                self.entities.len(),
                self.data_components.len(),
                self.attachments.len(),
            ),
            format!(
                "java registry templates: {} blocks, {} items, {} entities, {} data components, {} attachments",
                self.block_templates.len(),
                self.item_templates.len(),
                self.entity_declarations.len(),
                self.data_component_declarations.len(),
                self.attachment_declarations.len(),
            ),
        ];
        if !self.coalesced.is_empty() {
            let rows = self
                .coalesced
                .iter()
                .map(|(table, count)| format!("{table}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("java registry coalesced duplicate rows: {rows}"));
        }
        out
    }
}

static SNAPSHOT: OnceLock<RwLock<JavaRegistrySnapshot>> = OnceLock::new();

fn state() -> &'static RwLock<JavaRegistrySnapshot> {
    SNAPSHOT.get_or_init(|| RwLock::new(JavaRegistrySnapshot::default()))
}

/// Apply typed auxiliary declarations in deterministic id order.
///
/// Rows are sorted by id, so metadata order and entrypoint execution order
/// cannot change the snapshot. Rows repeated by a later startup call coalesce
/// when they are equal and are counted in [`JavaRegistrySnapshot::coalesced`];
/// two entity rows for one id that disagree on template, properties, spawn or
/// attributes are an error naming that id instead of a silent first-wins.
///
/// The batch is built and validated aside, so a rejected row leaves the
/// previously published snapshot untouched: the swap is all-or-nothing.
pub(super) fn try_synchronize(
    mods: &[horizon_client::mod_content::ContentMod],
) -> Result<JavaRegistrySnapshot, String> {
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
        coalesced: BTreeMap::new(),
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
    let mut coalesced = BTreeMap::new();
    note_coalesced(
        &mut coalesced,
        "blocks",
        coalesce_sorted(&mut next.block_templates, |row| row.id.as_str()),
    );
    note_coalesced(
        &mut coalesced,
        "items",
        coalesce_sorted(&mut next.item_templates, |row| row.id.as_str()),
    );
    note_coalesced(
        &mut coalesced,
        "entities",
        coalesce_entities(&mut next.entity_declarations)?,
    );
    note_coalesced(
        &mut coalesced,
        "data components",
        coalesce_sorted(&mut next.data_component_declarations, |row| row.id.as_str()),
    );
    note_coalesced(
        &mut coalesced,
        "attachments",
        coalesce_sorted(&mut next.attachment_declarations, |row| row.id.as_str()),
    );
    next.coalesced = coalesced;
    *state().write().expect("java registry lock poisoned") = next.clone();
    Ok(next)
}

/// Startup entry point for the server thread: publish the batch, or keep the
/// previous snapshot (logging the rejection) when a row is invalid.
///
/// [`try_synchronize`] is the same work with the error returned instead of
/// logged, for callers that prefer to stop startup.
#[cfg(test)]
pub(super) fn synchronize(
    mods: &[horizon_client::mod_content::ContentMod],
) -> JavaRegistrySnapshot {
    match try_synchronize(mods) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::error!(
                "Horizon: rejecting Java registry batch, keeping the previous snapshot: {error}"
            );
            state().read().expect("java registry lock poisoned").clone()
        }
    }
}

/// Sort rows by id, drop rows that repeat an id, and report how many were
/// folded away. Callers use this only where an equal id means an equal row;
/// the entity table is checked before it gets here.
fn coalesce_sorted<T>(rows: &mut Vec<T>, id: impl Fn(&T) -> &str) -> usize {
    rows.sort_by(|a, b| id(a).cmp(id(b)));
    let before = rows.len();
    rows.dedup_by(|a, b| id(a) == id(b));
    before - rows.len()
}

/// Entity rows are validated before coalescing: only a row that agrees on
/// template, properties, spawn and attributes may share another row's id.
fn coalesce_entities(rows: &mut Vec<EntityDeclaration>) -> Result<usize, String> {
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = rows
        .windows(2)
        .find(|pair| pair[0].id == pair[1].id && pair[0] != pair[1])
    {
        return Err(format!(
            "conflicting native entity declaration {}",
            pair[0].id
        ));
    }
    let before = rows.len();
    rows.dedup_by(|a, b| a.id == b.id);
    Ok(before - rows.len())
}

fn note_coalesced(coalesced: &mut BTreeMap<String, usize>, table: &str, folded: usize) {
    if folded > 0 {
        *coalesced.entry(table.to_string()).or_default() += folded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_mods::content::Content;
    use horizon_mods::native::{
        AttachmentDeclaration, DataComponentDeclaration, EntityAttributes, EntityDeclaration,
        NativeDeclarations, SpawnRestriction,
    };
    use std::sync::Mutex;

    /// The snapshot lives in a process-wide `OnceLock`, so tests that publish a
    /// batch take turns instead of observing each other's rows.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn content_mod(
        id: &str,
        declarations: NativeDeclarations,
    ) -> horizon_client::mod_content::ContentMod {
        horizon_client::mod_content::ContentMod {
            id: id.into(),
            content: Content::from_native(id, declarations).unwrap(),
        }
    }

    fn entity(id: &str, template: &str) -> EntityDeclaration {
        EntityDeclaration {
            id: id.into(),
            template: template.into(),
            ..EntityDeclaration::default()
        }
    }

    /// A row that exercises every typed field the snapshot has to keep.
    fn zombie_with_auxiliary() -> EntityDeclaration {
        EntityDeclaration {
            spawn: Some(SpawnRestriction {
                category: "ON_GROUND".into(),
                heightmap: "MOTION_BLOCKING_NO_LEAVES".into(),
                min_height: Some(50),
                max_height: None,
            }),
            attributes: Some(EntityAttributes {
                health: Some(24.0),
                speed: Some(0.23),
                attack_damage: None,
                follow_range: Some(35.0),
            }),
            ..entity("ruby:zombie", "minecraft:zombie")
        }
    }

    fn data_component(id: &str) -> DataComponentDeclaration {
        DataComponentDeclaration {
            id: id.into(),
            codec: "minecraft:int".into(),
            persistent: true,
            network_synced: true,
        }
    }

    fn attachment(id: &str) -> AttachmentDeclaration {
        AttachmentDeclaration {
            id: id.into(),
            codec: "minecraft:int".into(),
            default_value: "0".into(),
            copy_on_death: true,
        }
    }

    #[test]
    fn auxiliary_rows_are_sorted_deduplicated_and_atomic() {
        let _guard = test_guard();
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
            entities: vec![zombie_with_auxiliary()],
            data_components: vec![data_component("ruby:power")],
            attachments: vec![attachment("ruby:power")],
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
        assert_eq!(snapshot.coalesced.get("entities"), Some(&1));
        assert_eq!(snapshot.coalesced.get("blocks"), Some(&1));
        assert_eq!(*state().read().unwrap(), snapshot);
    }

    #[test]
    fn entity_spawn_and_attributes_round_trip() {
        let _guard = test_guard();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                entities: vec![
                    zombie_with_auxiliary(),
                    entity("ruby:plain", "minecraft:pig"),
                ],
                ..NativeDeclarations::default()
            },
        )]);
        assert_eq!(snapshot.entities, ["ruby:plain", "ruby:zombie"]);
        let spawn = snapshot.entity_spawn("ruby:zombie").unwrap();
        assert_eq!(spawn.category, "ON_GROUND");
        assert_eq!(spawn.heightmap, "MOTION_BLOCKING_NO_LEAVES");
        assert_eq!((spawn.min_height, spawn.max_height), (Some(50), None));
        let attributes = snapshot.entity_attributes("ruby:zombie").unwrap();
        assert_eq!(attributes.health, Some(24.0));
        assert_eq!(attributes.speed, Some(0.23));
        assert_eq!(attributes.attack_damage, None);
        assert_eq!(attributes.follow_range, Some(35.0));
        assert!(snapshot.entity_spawn("ruby:plain").is_none());
        assert!(snapshot.entity_attributes("ruby:plain").is_none());
        assert!(snapshot.entity_spawn("ruby:missing").is_none());
        assert!(snapshot.entity_attributes("ruby:missing").is_none());
    }

    #[test]
    fn conflicting_entity_rows_naming_the_id_are_rejected() {
        let _guard = test_guard();
        let mut changed = zombie_with_auxiliary();
        changed.attributes.as_mut().unwrap().health = Some(40.0);
        assert_eq!(
            try_synchronize(&[
                content_mod(
                    "ruby",
                    NativeDeclarations {
                        entities: vec![zombie_with_auxiliary()],
                        ..NativeDeclarations::default()
                    },
                ),
                content_mod(
                    "ruby-extra",
                    NativeDeclarations {
                        entities: vec![changed],
                        ..NativeDeclarations::default()
                    },
                ),
            ])
            .unwrap_err(),
            "conflicting native entity declaration ruby:zombie"
        );
    }

    #[test]
    fn equal_auxiliary_rows_coalesce() {
        let _guard = test_guard();
        let declarations = || NativeDeclarations {
            entities: vec![zombie_with_auxiliary()],
            data_components: vec![data_component("ruby:power")],
            attachments: vec![attachment("ruby:power")],
            ..NativeDeclarations::default()
        };
        let snapshot = try_synchronize(&[
            content_mod("ruby", declarations()),
            content_mod("ruby-extra", declarations()),
        ])
        .unwrap();
        assert_eq!(snapshot.entity_declarations.len(), 1);
        assert_eq!(snapshot.data_component_declarations.len(), 1);
        assert_eq!(snapshot.attachment_declarations.len(), 1);
        assert_eq!(snapshot.coalesced.get("entities"), Some(&1));
        assert_eq!(snapshot.coalesced.get("data components"), Some(&1));
        assert_eq!(snapshot.coalesced.get("attachments"), Some(&1));
    }

    #[test]
    fn diagnostics_report_counts_and_coalesced_rows() {
        let _guard = test_guard();
        assert_eq!(
            JavaRegistrySnapshot::default().diagnostics(),
            [
                "java registry: 0 block ids, 0 item ids, 0 entity ids, 0 data component ids, 0 attachment ids".to_string(),
                "java registry templates: 0 blocks, 0 items, 0 entities, 0 data components, 0 attachments".to_string(),
            ]
        );
        let snapshot = synchronize(&[
            content_mod(
                "ruby",
                NativeDeclarations {
                    entities: vec![zombie_with_auxiliary()],
                    data_components: vec![data_component("ruby:power")],
                    ..NativeDeclarations::default()
                },
            ),
            content_mod(
                "ruby-extra",
                NativeDeclarations {
                    entities: vec![zombie_with_auxiliary()],
                    ..NativeDeclarations::default()
                },
            ),
        ]);
        assert_eq!(
            snapshot.diagnostics(),
            [
                "java registry: 0 block ids, 0 item ids, 1 entity ids, 1 data component ids, 0 attachment ids".to_string(),
                "java registry templates: 0 blocks, 0 items, 1 entities, 1 data components, 0 attachments".to_string(),
                "java registry coalesced duplicate rows: entities=1".to_string(),
            ]
        );
    }

    #[test]
    fn rejected_batch_keeps_the_published_snapshot() {
        let _guard = test_guard();
        let good = content_mod(
            "ruby",
            NativeDeclarations {
                entities: vec![zombie_with_auxiliary()],
                ..NativeDeclarations::default()
            },
        );
        let published = synchronize(std::slice::from_ref(&good));
        assert_eq!(*state().read().unwrap(), published);
        let mut changed = zombie_with_auxiliary();
        changed.template = "minecraft:husk".into();
        let conflicting = content_mod(
            "ruby-extra",
            NativeDeclarations {
                entities: vec![changed],
                ..NativeDeclarations::default()
            },
        );
        assert!(try_synchronize(&[good, conflicting]).is_err());
        assert_eq!(*state().read().unwrap(), published);
    }
}
