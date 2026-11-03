//! Bounded Java content registry state owned by a server thread.
//!
//! The resource root remains the source for vanilla block/item tables. These
//! auxiliary rows are kept separately so Java declarations can be observed by
//! the authoritative server before a future full registry reload wires them
//! into entity/component/attachment stores. Block-entity rows are read here by
//! the game's `EntityBlock.newBlockEntity` (`declared_block_entity`), which
//! gives a mod block its template's real block entity.
use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use horizon_client::{containers::BlockEntity, text::Text};
use horizon_mods::content::{BlockProperties, BlockShapes, Rarity};
use horizon_mods::native::{
    AttachmentDeclaration, BlockEntityDeclaration, DataComponentDeclaration, EntityAttributes,
    EntityDeclaration, MenuDeclaration, SpawnRestriction,
};
use horizon_mods::payloads::{PayloadCodec, PayloadDeclaration};

/// Most block → menu bindings the [`JavaRegistrySnapshot::menu_bindings`] list
/// and its diagnostics line name; a larger registry only counts the rest.
const MAX_MENU_BINDINGS: usize = 16;

/// Most channel ids a [`PayloadSummary`] names; a larger root only counts the
/// rest.
pub(super) const MAX_PAYLOAD_IDS: usize = 16;

/// The prefix every [`JavaRegistrySnapshot::compatibility_lines`] line starts
/// with, so a log reader can pick the summary out of a mixed stream.
pub(super) const COMPATIBILITY_PREFIX: &str = "java compatibility: ";

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
    pub menus: Vec<MenuDeclaration>,
    pub block_entities: Vec<BlockEntityDeclaration>,
    /// Rows folded away while coalescing duplicates, by table name. Sorted, so
    /// two batches that coalesce the same rows report the same map.
    pub coalesced: BTreeMap<String, usize>,
}

/// Counts over one [`JavaRegistrySnapshot`], in a fixed order: every table the
/// command registries merge, plus the registry's menu bindings and the blocks
/// a declared block entity carries. Output stays constant-size.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct RegistrySummary {
    pub blocks: usize,
    pub items: usize,
    pub entities: usize,
    pub data_components: usize,
    pub attachments: usize,
    pub menus: usize,
    pub block_entities: usize,
    /// Distinct `(block, menu)` pairs.
    pub menu_bindings: usize,
    /// Distinct blocks a declared block entity carries.
    pub block_entity_blocks: usize,
}

/// Counts over the payload channels a run declared, sorted by id.
/// `directions` is in [`PayloadDirection`](horizon_mods::payloads::PayloadDirection)
/// wire order, so two summaries print the same fields in the same order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct PayloadSummary {
    pub channels: usize,
    pub directions: [usize; 5],
    /// Declarations whose codec the transport models and validates.
    pub supported_codecs: usize,
    /// Declarations with no codec or one the transport does not model.
    pub opaque_codecs: usize,
    /// The channel ids, sorted, at most [`MAX_PAYLOAD_IDS`].
    pub ids: Vec<String>,
    pub ids_truncated: bool,
}

#[allow(dead_code)]
impl PayloadSummary {
    /// Summarize `declared` ([`horizon_client::lan::payloads::declared_by_id`]),
    /// counting each channel once and naming the first [`MAX_PAYLOAD_IDS`] in
    /// id order.
    pub(super) fn from_declarations(declared: &BTreeMap<String, PayloadDeclaration>) -> Self {
        let mut directions = [0; 5];
        let mut supported_codecs = 0;
        let mut opaque_codecs = 0;
        for declaration in declared.values() {
            directions[declaration.direction.wire() as usize] += 1;
            if declaration
                .codec
                .as_deref()
                .and_then(PayloadCodec::parse)
                .is_some()
            {
                supported_codecs += 1;
            } else {
                opaque_codecs += 1;
            }
        }
        let ids: Vec<String> = declared.keys().take(MAX_PAYLOAD_IDS).cloned().collect();
        Self {
            channels: declared.len(),
            directions,
            supported_codecs,
            opaque_codecs,
            ids,
            ids_truncated: declared.len() > MAX_PAYLOAD_IDS,
        }
    }

    /// The channels the last content root recorded. Empty until one records
    /// some, so a window that never stored declarations logs zeros.
    pub(super) fn declared() -> Self {
        Self::from_declarations(&horizon_client::lan::payloads::declared_by_id())
    }

    /// The counts line, then the bounded ids. Always at least one line, so a
    /// caller can log an empty run without a special case.
    pub(super) fn lines(&self) -> Vec<String> {
        let [
            to_client,
            to_server,
            config_to_client,
            config_to_server,
            both,
        ] = self.directions;
        let mut lines = vec![format!(
            "payload channels: {} ({to_client} playToClient, {to_server} playToServer, \
             {config_to_client} configurationToClient, {config_to_server} configurationToServer, \
             {both} playBidirectional), {} supported codec(s), {} opaque",
            self.channels, self.supported_codecs, self.opaque_codecs,
        )];
        if !self.ids.is_empty() {
            lines.push(format!(
                "payload ids: {}{}",
                self.ids.join(", "),
                if self.ids_truncated { ", ..." } else { "" }
            ));
        }
        lines
    }
}

/// A declared menu a block opens, as a block use needs it: the definition id,
/// the translation key its title resolves through, and its slot count. Copied
/// out of the registry row, so the caller does not hold the snapshot lock; a
/// validated [`MenuDeclaration`] bounds the id and title key to
/// [`horizon_mods::native::MAX_NATIVE_TEXT`] bytes and the slots to
/// [`horizon_mods::native::MAX_MENU_SLOTS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MenuInfo {
    pub id: String,
    pub title_key: String,
    pub slots: u32,
}

impl MenuInfo {
    fn from_declaration(menu: &MenuDeclaration) -> Self {
        Self {
            id: menu.id.clone(),
            title_key: menu.title_key.clone(),
            slots: menu.slots,
        }
    }

    /// The title to send with the menu (`Component.translatable(title_key)`).
    pub(super) fn title(&self) -> Text {
        Text::translate(&self.title_key, &[])
    }
}

/// The chest rows a declared menu of `slots` slots needs: `ceil(slots / 9)`
/// from one (`generic_9x1`) to six (`generic_9x6`), the sizes a server menu
/// can show.
pub(super) fn menu_rows(slots: u32) -> u8 {
    slots.div_ceil(9).clamp(1, 6) as u8
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

    pub(super) fn menu(&self, id: &str) -> Option<&MenuDeclaration> {
        self.menus
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.menus[index])
    }

    /// The menus a block opens from, sorted by id: the server routes a block
    /// use to one of them. Empty when no declared menu binds `block`.
    pub(super) fn menus_for_block(&self, block: &str) -> Vec<&MenuDeclaration> {
        let mut menus: Vec<&MenuDeclaration> = self
            .menus
            .iter()
            .filter(|menu| menu.open_blocks.iter().any(|open| open.as_str() == block))
            .collect();
        menus.sort_by(|a, b| a.id.cmp(&b.id));
        menus
    }

    /// The first declared menu `block` opens from, by id, when the snapshot
    /// declares any: the menu a block use opens.
    pub(super) fn declared_menu_for_block(&self, block: &str) -> Option<MenuInfo> {
        self.menus_for_block(block)
            .first()
            .map(|menu| MenuInfo::from_declaration(menu))
    }

    /// The declared block → menu bindings as `(block, menu)` pairs, sorted and
    /// without repeats, for diagnostics. Only the first
    /// [`MAX_MENU_BINDINGS`] are named; a longer registry ends the list with
    /// `...` so a reader can tell it was cut.
    pub(super) fn menu_bindings(&self) -> Vec<(String, String)> {
        let mut bindings: Vec<(String, String)> = self
            .menus
            .iter()
            .flat_map(|menu| {
                menu.open_blocks
                    .iter()
                    .map(|block| (block.clone(), menu.id.clone()))
            })
            .collect();
        bindings.sort();
        bindings.dedup();
        bindings.truncate(MAX_MENU_BINDINGS);
        bindings
    }

    /// Counts for the command registries, menu bindings and block entities:
    /// constant-size output whatever the batch size.
    pub(super) fn summary(&self) -> RegistrySummary {
        let mut pairs: Vec<(&str, &str)> = self
            .menus
            .iter()
            .flat_map(|menu| {
                menu.open_blocks
                    .iter()
                    .map(move |block| (block.as_str(), menu.id.as_str()))
            })
            .collect();
        pairs.sort();
        pairs.dedup();
        let mut blocks: Vec<&str> = self
            .block_entities
            .iter()
            .flat_map(|row| row.valid_blocks.iter().map(String::as_str))
            .collect();
        blocks.sort();
        blocks.dedup();
        RegistrySummary {
            blocks: self.blocks.len(),
            items: self.items.len(),
            entities: self.entities.len(),
            data_components: self.data_components.len(),
            attachments: self.attachments.len(),
            menus: self.menus.len(),
            block_entities: self.block_entities.len(),
            menu_bindings: pairs.len(),
            block_entity_blocks: blocks.len(),
        }
    }

    /// A deterministic multi-line compatibility summary for the log and
    /// status: the snapshot's counts, what content mods declared, and the
    /// payload channels the run stored. Every line starts with
    /// [`COMPATIBILITY_PREFIX`], and the detail lists are the bounded ones the
    /// summaries expose, so even a maximal root stays a handful of lines.
    pub(super) fn compatibility_lines(
        &self,
        declared: &horizon_client::mod_content::DeclarationSummary,
        payloads: &PayloadSummary,
    ) -> Vec<String> {
        let summary = self.summary();
        let mut lines = vec![format!(
            "{COMPATIBILITY_PREFIX}registry: {} block ids, {} item ids, {} entity ids, {} data component ids, {} attachment ids, {} menu ids, {} block entity ids, {} menu bindings, {} block entity blocks",
            summary.blocks,
            summary.items,
            summary.entities,
            summary.data_components,
            summary.attachments,
            summary.menus,
            summary.block_entities,
            summary.menu_bindings,
            summary.block_entity_blocks,
        )];
        let declared_lines = declared.lines();
        if declared_lines.is_empty() {
            lines.push(format!("{COMPATIBILITY_PREFIX}declared: none"));
        } else {
            lines.extend(
                declared_lines
                    .into_iter()
                    .map(|line| format!("{COMPATIBILITY_PREFIX}declared: {line}")),
            );
        }
        lines.extend(
            payloads
                .lines()
                .into_iter()
                .map(|line| format!("{COMPATIBILITY_PREFIX}{line}")),
        );
        lines
    }

    /// Every block a declared menu opens from must resolve inside this
    /// registry: a block the snapshot declares, a block a declared block
    /// entity carries, or a vanilla one.
    ///
    /// The snapshot holds mod declarations only and never reads the vanilla
    /// block table, so `minecraft:*` is accepted on its namespace alone: the
    /// prefix says vanilla owns the id, not that this batch loaded the block.
    fn check_menu_bindings(&self) -> Result<(), String> {
        let carried = |block: &str| {
            self.block_entities
                .iter()
                .any(|row| row.valid_blocks.iter().any(|valid| valid == block))
        };
        for menu in &self.menus {
            for block in &menu.open_blocks {
                if block.starts_with("minecraft:")
                    || self
                        .blocks
                        .binary_search_by(|row| row.as_str().cmp(block))
                        .is_ok()
                    || carried(block)
                {
                    continue;
                }
                return Err(format!(
                    "native menu {} opens from {block}, which is not a declared block, \
                     a block entity's valid block, or a vanilla block",
                    menu.id
                ));
            }
        }
        Ok(())
    }

    pub(super) fn block_entity(&self, id: &str) -> Option<&BlockEntityDeclaration> {
        self.block_entities
            .binary_search_by(|row| row.id.as_str().cmp(id))
            .ok()
            .map(|index| &self.block_entities[index])
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
                "java registry: {} block ids, {} item ids, {} entity ids, {} data component ids, {} attachment ids, {} menu ids, {} block entity ids",
                self.blocks.len(),
                self.items.len(),
                self.entities.len(),
                self.data_components.len(),
                self.attachments.len(),
                self.menus.len(),
                self.block_entities.len(),
            ),
            format!(
                "java registry templates: {} blocks, {} items, {} entities, {} data components, {} attachments, {} menus, {} block entities",
                self.block_templates.len(),
                self.item_templates.len(),
                self.entity_declarations.len(),
                self.data_component_declarations.len(),
                self.attachment_declarations.len(),
                self.menus.len(),
                self.block_entities.len(),
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
        let bindings = self.menu_bindings();
        if !bindings.is_empty() {
            let mut rows = bindings
                .iter()
                .map(|(block, menu)| format!("{block} opens {menu}"))
                .collect::<Vec<_>>();
            // Menu ids and each menu's `open_blocks` are without repeats, so
            // the sum counts distinct (block, menu) pairs.
            let total: usize = self.menus.iter().map(|menu| menu.open_blocks.len()).sum();
            if total > rows.len() {
                rows.push("...".to_owned());
            }
            out.push(format!("java registry menu bindings: {}", rows.join(", ")));
        }
        out
    }
}

static SNAPSHOT: OnceLock<RwLock<JavaRegistrySnapshot>> = OnceLock::new();

fn state() -> &'static RwLock<JavaRegistrySnapshot> {
    SNAPSHOT.get_or_init(|| RwLock::new(JavaRegistrySnapshot::default()))
}

/// The first declared menu `block` opens from, by id: the published snapshot
/// when it declares menus, else the loaded Java content mods (a window that
/// never started a server thread has no snapshot yet). Every bound block is a
/// declared block or an explicit `open_blocks` entry, so a vanilla block with
/// no declared binding answers None.
pub(super) fn declared_menu_for_block(
    mods: &[horizon_client::mod_content::ContentMod],
    block: &str,
) -> Option<MenuInfo> {
    let snapshot = state().read().expect("java registry lock poisoned");
    if !snapshot.menus.is_empty() {
        return snapshot.declared_menu_for_block(block);
    }
    drop(snapshot);
    let menus = mods
        .iter()
        .flat_map(|m| m.content.menus_for_block(block))
        .min_by(|a, b| a.id.cmp(&b.id))?;
    Some(MenuInfo::from_declaration(menus))
}

/// Most block-entity rows [`declared_block_entity_for_block`] reads: a content
/// root with more is outside the bound this lookup works within, and a block
/// carried by a row past it starts without its declared block entity.
pub(super) const MAX_DECLARED_BLOCK_ENTITY_ROWS: usize = 1024;

/// The declared block entity `block` carries (`BlockEntityType.validBlocks`):
/// the row that names it, lowest id first when several do. Reads at most
/// [`MAX_DECLARED_BLOCK_ENTITY_ROWS`] rows in content-mod order, so a
/// pathological registry cannot stall a block placement, and picks by id so
/// metadata order cannot change the answer.
pub(super) fn declared_block_entity_for_block<'a>(
    mods: &'a [horizon_client::mod_content::ContentMod],
    block: &str,
) -> Option<&'a BlockEntityDeclaration> {
    mods.iter()
        .flat_map(|m| m.content.block_entities.iter())
        .take(MAX_DECLARED_BLOCK_ENTITY_ROWS)
        .filter(|row| row.valid_blocks.iter().any(|valid| valid == block))
        .min_by(|a, b| a.id.cmp(&b.id))
}

/// The block entity `block` starts as (`EntityBlock.newBlockEntity`): the
/// row that carries it and a vanilla-template block entity (`minecraft:barrel`
/// gives the barrel's container, so its items and menus work), or none when no
/// row carries the block or its template has no block entity (a template that
/// is only a marker, as `minecraft:stone` is).
///
/// The row comes back with the entity for the callers where the declared type
/// id (`ruby:crate_entity`) matters more than the template's.
pub(super) fn declared_block_entity<'a>(
    mods: &'a [horizon_client::mod_content::ContentMod],
    block: &str,
) -> Option<(&'a BlockEntityDeclaration, BlockEntity)> {
    let row = declared_block_entity_for_block(mods, block)?;
    let entity = BlockEntity::for_block(&row.template)?;
    Some((row, entity))
}

/// Whether `block`'s block entity ticks (`EntityBlock.getTicker`): a declared
/// row's `ticker`, and only when its template has a block entity at all — a
/// row with no entity has nothing to tick.
pub(super) fn declared_block_entity_ticks(
    mods: &[horizon_client::mod_content::ContentMod],
    block: &str,
) -> bool {
    declared_block_entity(mods, block).is_some_and(|(row, _)| row.ticker)
}

/// Apply typed auxiliary declarations in deterministic id order.
///
/// Rows are sorted by id, so metadata order and entrypoint execution order
/// cannot change the snapshot. Rows repeated by a later startup call coalesce
/// when they are equal and are counted in [`JavaRegistrySnapshot::coalesced`];
/// two rows of a validated table (entities, menus, block entities) that
/// disagree on any field are an error naming that id instead of a silent
/// first-wins.
///
/// The batch is built and validated aside, so a rejected row leaves the
/// previously published snapshot untouched: the swap is all-or-nothing. A menu
/// opening from a block neither this batch declares nor vanilla defines
/// rejects the batch too.
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
        menus: mods
            .iter()
            .flat_map(|m| m.content.menus.iter().cloned())
            .collect(),
        block_entities: mods
            .iter()
            .flat_map(|m| m.content.block_entities.iter().cloned())
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
        coalesce_typed(&mut next.entity_declarations, "entity", |row| {
            row.id.as_str()
        })?,
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
    note_coalesced(
        &mut coalesced,
        "menus",
        coalesce_typed(&mut next.menus, "menu", |row| row.id.as_str())?,
    );
    note_coalesced(
        &mut coalesced,
        "block entities",
        coalesce_typed(&mut next.block_entities, "block entity", |row| {
            row.id.as_str()
        })?,
    );
    next.coalesced = coalesced;
    next.check_menu_bindings()?;
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

/// Typed rows are validated before coalescing: only a row that agrees on every
/// field may share another row's id. `table` names the row kind in the error.
fn coalesce_typed<T: PartialEq>(
    rows: &mut Vec<T>,
    table: &str,
    id: impl Fn(&T) -> &str,
) -> Result<usize, String> {
    rows.sort_by(|a, b| id(a).cmp(id(b)));
    if let Some(pair) = rows
        .windows(2)
        .find(|pair| id(&pair[0]) == id(&pair[1]) && pair[0] != pair[1])
    {
        return Err(format!(
            "conflicting native {table} declaration {}",
            id(&pair[0])
        ));
    }
    let before = rows.len();
    rows.dedup_by(|a, b| id(a) == id(b));
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
        AttachmentDeclaration, BlockEntityDeclaration, DataComponentDeclaration, EntityAttributes,
        EntityDeclaration, MenuDeclaration, NativeDeclarations, SpawnRestriction,
    };
    use horizon_mods::payloads::PayloadDirection;
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

    fn menu(id: &str) -> MenuDeclaration {
        MenuDeclaration {
            id: id.into(),
            title_key: "container.ruby.crate".into(),
            slots: 27,
            open_blocks: Vec::new(),
        }
    }

    /// A block row whose id is all these tests read back.
    fn block(id: &str) -> horizon_mods::content::Block {
        horizon_mods::content::Block {
            id: id.into(),
            template: "minecraft:stone".into(),
            properties: Default::default(),
            shapes: None,
            default_state: Default::default(),
            item: false,
            components: Default::default(),
        }
    }

    /// A menu bound to `blocks`, as the registry sink reads `open_blocks`.
    fn bound_menu(id: &str, blocks: &[&str]) -> MenuDeclaration {
        MenuDeclaration {
            open_blocks: blocks.iter().map(|block| (*block).to_owned()).collect(),
            ..menu(id)
        }
    }

    fn block_entity(id: &str) -> BlockEntityDeclaration {
        BlockEntityDeclaration {
            id: id.into(),
            template: "minecraft:barrel".into(),
            ticker: true,
            valid_blocks: vec!["ruby:crate_block".into()],
        }
    }

    /// One declared payload channel, as `lan::payloads` stores it.
    fn payload(id: &str, direction: PayloadDirection, codec: Option<&str>) -> PayloadDeclaration {
        PayloadDeclaration {
            id: id.into(),
            direction,
            codec: codec.map(str::to_owned),
        }
    }

    /// The declaration map `lan::payloads::declared_by_id` returns: sorted by
    /// id, one row per channel.
    fn payload_map(
        rows: impl IntoIterator<Item = PayloadDeclaration>,
    ) -> BTreeMap<String, PayloadDeclaration> {
        rows.into_iter().map(|row| (row.id.clone(), row)).collect()
    }

    #[test]
    fn auxiliary_rows_are_sorted_deduplicated_and_atomic() {
        let _guard = test_guard();
        let declarations = NativeDeclarations {
            blocks: vec![block("ruby:stone")],
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
            menus: vec![menu("ruby:crate")],
            block_entities: vec![block_entity("ruby:crate")],
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
        assert_eq!(snapshot.menus.len(), 1);
        assert_eq!(snapshot.block_entities.len(), 1);
        assert_eq!(snapshot.coalesced.get("entities"), Some(&1));
        assert_eq!(snapshot.coalesced.get("data components"), Some(&1));
        assert_eq!(snapshot.coalesced.get("attachments"), Some(&1));
        assert_eq!(snapshot.coalesced.get("menus"), Some(&1));
        assert_eq!(snapshot.coalesced.get("block entities"), Some(&1));
    }

    #[test]
    fn menu_and_block_entity_rows_round_trip() {
        let _guard = test_guard();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                menus: vec![menu("ruby:crate")],
                block_entities: vec![block_entity("ruby:crate")],
                ..NativeDeclarations::default()
            },
        )]);
        let row = snapshot.menu("ruby:crate").unwrap();
        assert_eq!(row.title_key, "container.ruby.crate");
        assert_eq!(row.slots, 27);
        let row = snapshot.block_entity("ruby:crate").unwrap();
        assert_eq!(row.template, "minecraft:barrel");
        assert!(row.ticker);
        assert_eq!(row.valid_blocks, ["ruby:crate_block"]);
        assert!(snapshot.menu("ruby:missing").is_none());
        assert!(snapshot.block_entity("ruby:missing").is_none());
        assert_eq!(*state().read().unwrap(), snapshot);
    }

    #[test]
    fn menus_bound_to_a_block_are_listed_by_id() {
        let _guard = test_guard();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                menus: vec![
                    bound_menu("ruby:zeta", &["ruby:crate_block", "minecraft:barrel"]),
                    bound_menu("ruby:crate", &["ruby:crate_block"]),
                    menu("ruby:loose"),
                ],
                ..NativeDeclarations::default()
            },
        )]);
        let ids = |block: &str| -> Vec<String> {
            snapshot
                .menus_for_block(block)
                .iter()
                .map(|menu| menu.id.clone())
                .collect()
        };
        assert_eq!(ids("ruby:crate_block"), ["ruby:crate", "ruby:zeta"]);
        assert_eq!(ids("minecraft:barrel"), ["ruby:zeta"]);
        assert!(ids("ruby:unbound").is_empty());
        assert_eq!(
            snapshot.menu_bindings(),
            [
                ("minecraft:barrel".to_owned(), "ruby:zeta".to_owned()),
                ("ruby:crate_block".to_owned(), "ruby:crate".to_owned()),
                ("ruby:crate_block".to_owned(), "ruby:zeta".to_owned()),
            ]
        );
    }

    #[test]
    fn menu_rows_round_up_and_stay_within_a_chest() {
        assert_eq!(menu_rows(0), 1);
        assert_eq!(menu_rows(1), 1);
        assert_eq!(menu_rows(9), 1);
        assert_eq!(menu_rows(10), 2);
        assert_eq!(menu_rows(18), 2);
        assert_eq!(menu_rows(27), 3);
        assert_eq!(menu_rows(46), 6);
        assert_eq!(menu_rows(54), 6);
        assert_eq!(menu_rows(horizon_mods::native::MAX_MENU_SLOTS), 6);
    }

    #[test]
    fn declared_block_entity_maps_a_block_to_its_template() {
        let _guard = test_guard();
        let mods = [content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                block_entities: vec![
                    block_entity("ruby:crate"),
                    BlockEntityDeclaration {
                        id: "ruby:marker".into(),
                        template: "minecraft:stone".into(),
                        ticker: true,
                        valid_blocks: vec!["ruby:marker_block".into()],
                    },
                ],
                ..NativeDeclarations::default()
            },
        )];
        // A block a row carries starts as the row's template: the barrel's
        // container, which a declared menu's slots read and hoppers fill.
        // Horizon has no block entity of its own for the mod block, so
        // without the declaration it would start with none.
        assert!(BlockEntity::for_block("ruby:crate_block").is_none());
        let (row, entity) = declared_block_entity(&mods, "ruby:crate_block").unwrap();
        assert_eq!(row.id, "ruby:crate");
        assert_eq!(entity.slots().len(), 27);
        assert!(matches!(entity, BlockEntity::Chest(_)));
        // A block no row carries has none, and a template with no block
        // entity leaves the block without one. The row is still found: it is
        // the template that has nothing to copy.
        assert!(declared_block_entity(&mods, "ruby:unbound").is_none());
        assert!(declared_block_entity_for_block(&mods, "ruby:unbound").is_none());
        assert!(declared_block_entity(&mods, "ruby:marker_block").is_none());
        assert_eq!(
            declared_block_entity_for_block(&mods, "ruby:marker_block")
                .unwrap()
                .id,
            "ruby:marker"
        );
    }

    #[test]
    fn declared_block_entity_ticker_applies_only_with_an_entity() {
        let _guard = test_guard();
        let mods = [content_mod(
            "ruby",
            NativeDeclarations {
                block_entities: vec![
                    block_entity("ruby:ticking"),
                    BlockEntityDeclaration {
                        id: "ruby:still".into(),
                        template: "minecraft:barrel".into(),
                        ticker: false,
                        valid_blocks: vec!["ruby:still_block".into()],
                    },
                    BlockEntityDeclaration {
                        id: "ruby:hollow".into(),
                        template: "minecraft:stone".into(),
                        ticker: true,
                        valid_blocks: vec!["ruby:hollow_block".into()],
                    },
                ],
                ..NativeDeclarations::default()
            },
        )];
        assert!(declared_block_entity_ticks(&mods, "ruby:crate_block"));
        assert!(!declared_block_entity_ticks(&mods, "ruby:still_block"));
        // A ticker on a template with no block entity ticks nothing.
        assert!(!declared_block_entity_ticks(&mods, "ruby:hollow_block"));
        assert!(!declared_block_entity_ticks(&mods, "ruby:unbound"));
    }

    #[test]
    fn declared_block_entity_lookup_is_lowest_id_and_bounded() {
        let _guard = test_guard();
        // Two rows carry one block: the lower id answers, wherever it sits.
        let mods = [content_mod(
            "ruby",
            NativeDeclarations {
                block_entities: vec![
                    BlockEntityDeclaration {
                        id: "ruby:zeta".into(),
                        template: "minecraft:hopper".into(),
                        ticker: true,
                        valid_blocks: vec!["ruby:shared".into()],
                    },
                    BlockEntityDeclaration {
                        id: "ruby:alpha".into(),
                        template: "minecraft:barrel".into(),
                        ticker: true,
                        valid_blocks: vec!["ruby:shared".into()],
                    },
                ],
                ..NativeDeclarations::default()
            },
        )];
        assert_eq!(
            declared_block_entity_for_block(&mods, "ruby:shared")
                .unwrap()
                .id,
            "ruby:alpha"
        );
        // Rows past the bound are not read, and the bound is exact.
        let rows: Vec<BlockEntityDeclaration> = (0..=MAX_DECLARED_BLOCK_ENTITY_ROWS)
            .map(|i| BlockEntityDeclaration {
                id: format!("ruby:e{i:04}"),
                template: "minecraft:barrel".into(),
                ticker: false,
                valid_blocks: vec![format!("ruby:b{i:04}")],
            })
            .collect();
        let mods = [content_mod(
            "ruby",
            NativeDeclarations {
                block_entities: rows,
                ..NativeDeclarations::default()
            },
        )];
        let last = MAX_DECLARED_BLOCK_ENTITY_ROWS - 1;
        assert!(declared_block_entity_for_block(&mods, "ruby:b0000").is_some());
        assert!(declared_block_entity_for_block(&mods, &format!("ruby:b{last:04}")).is_some());
        assert!(
            declared_block_entity_for_block(
                &mods,
                &format!("ruby:b{MAX_DECLARED_BLOCK_ENTITY_ROWS:04}")
            )
            .is_none()
        );
    }

    #[test]
    fn declared_menu_for_block_reads_the_snapshot() {
        let _guard = test_guard();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                menus: vec![
                    bound_menu("ruby:zeta", &["ruby:crate_block", "minecraft:barrel"]),
                    bound_menu("ruby:crate", &["ruby:crate_block"]),
                    menu("ruby:loose"),
                ],
                ..NativeDeclarations::default()
            },
        )]);
        // The first binding by id wins; its title comes from its key.
        let info = snapshot
            .declared_menu_for_block("ruby:crate_block")
            .unwrap();
        assert_eq!(info.id, "ruby:crate");
        assert_eq!(info.title_key, "container.ruby.crate");
        assert_eq!(info.slots, 27);
        assert_eq!(info.title().translation_key(), Some("container.ruby.crate"));
        // An explicit binding of a vanilla block opens it too; an unbound
        // block, vanilla or not, does not.
        assert_eq!(
            snapshot
                .declared_menu_for_block("minecraft:barrel")
                .unwrap()
                .id,
            "ruby:zeta"
        );
        assert!(snapshot.declared_menu_for_block("ruby:unbound").is_none());
        assert!(
            snapshot
                .declared_menu_for_block("minecraft:chest")
                .is_none()
        );
        assert!(snapshot.declared_menu_for_block("ruby:loose").is_none());
    }

    #[test]
    fn declared_menu_for_block_falls_back_to_the_content_mods() {
        let _guard = test_guard();
        // No snapshot yet: the loaded content mods answer, first menu by id.
        synchronize(&[]);
        let mods = [
            content_mod(
                "ruby",
                NativeDeclarations {
                    blocks: vec![block("ruby:crate_block")],
                    menus: vec![
                        bound_menu("ruby:zeta", &["ruby:crate_block"]),
                        bound_menu("ruby:crate", &["ruby:crate_block"]),
                    ],
                    ..NativeDeclarations::default()
                },
            ),
            content_mod(
                "other",
                NativeDeclarations {
                    blocks: vec![block("other:block")],
                    menus: vec![bound_menu("other:menu", &["other:block"])],
                    ..NativeDeclarations::default()
                },
            ),
        ];
        let info = declared_menu_for_block(&mods, "ruby:crate_block").unwrap();
        assert_eq!(info.id, "ruby:crate");
        assert_eq!(info.title_key, "container.ruby.crate");
        assert_eq!(
            declared_menu_for_block(&mods, "other:block").unwrap().id,
            "other:menu"
        );
        assert!(declared_menu_for_block(&mods, "ruby:unbound").is_none());
        assert!(declared_menu_for_block(&mods, "minecraft:chest").is_none());
        // Once a snapshot declares menus, it answers instead of the mods.
        synchronize(&[content_mod(
            "third",
            NativeDeclarations {
                blocks: vec![block("third:block")],
                menus: vec![bound_menu("third:menu", &["third:block"])],
                ..NativeDeclarations::default()
            },
        )]);
        assert!(declared_menu_for_block(&mods, "ruby:crate_block").is_none());
        assert_eq!(
            declared_menu_for_block(&mods, "third:block").unwrap().id,
            "third:menu"
        );
    }

    #[test]
    fn menu_bindings_accept_declared_and_vanilla_blocks_and_reject_unknown() {
        let _guard = test_guard();
        // A block the batch declares, and a vanilla one, both resolve.
        let snapshot = try_synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                menus: vec![bound_menu(
                    "ruby:crate",
                    &["ruby:crate_block", "minecraft:barrel"],
                )],
                ..NativeDeclarations::default()
            },
        )])
        .unwrap();
        assert_eq!(snapshot.menus_for_block("ruby:crate_block").len(), 1);
        assert_eq!(snapshot.menus_for_block("minecraft:barrel").len(), 1);
        // A block a declared block entity carries resolves too.
        let snapshot = try_synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                block_entities: vec![block_entity("ruby:crate")],
                menus: vec![bound_menu("ruby:crate", &["ruby:crate_block"])],
                ..NativeDeclarations::default()
            },
        )])
        .unwrap();
        assert_eq!(snapshot.menus_for_block("ruby:crate_block").len(), 1);
        // An id nobody declares is rejected, naming the menu and the block.
        // `from_native` fences the same thing per mod, so bypass it here the
        // way a hand-built row would.
        let mut unknown = content_mod("ruby", NativeDeclarations::default());
        let mut row = menu("ruby:ghost");
        row.open_blocks = vec!["othermod:ghost".into()];
        unknown.content.menus.push(row);
        assert_eq!(
            try_synchronize(&[unknown]).unwrap_err(),
            "native menu ruby:ghost opens from othermod:ghost, which is not a declared block, \
             a block entity's valid block, or a vanilla block"
        );
    }

    #[test]
    fn menu_binding_diagnostics_are_bounded() {
        let _guard = test_guard();
        let count = MAX_MENU_BINDINGS + 2;
        let blocks: Vec<_> = (0..count)
            .map(|i| block(&format!("ruby:block_{i:02}")))
            .collect();
        let menus: Vec<_> = (0..count)
            .map(|i| {
                let block = format!("ruby:block_{i:02}");
                bound_menu(&format!("ruby:menu_{i:02}"), &[&block])
            })
            .collect();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                blocks,
                menus,
                ..NativeDeclarations::default()
            },
        )]);
        let bindings = snapshot.menu_bindings();
        assert_eq!(bindings.len(), MAX_MENU_BINDINGS);
        assert_eq!(bindings[0].0, "ruby:block_00");
        assert_eq!(bindings[MAX_MENU_BINDINGS - 1].0, "ruby:block_15");
        let line = snapshot.diagnostics().pop().unwrap();
        assert!(line.starts_with("java registry menu bindings: "), "{line}");
        assert!(line.ends_with(", ..."), "{line}");
        assert_eq!(line.matches(" opens ").count(), MAX_MENU_BINDINGS);
    }

    #[test]
    fn conflicting_menu_and_block_entity_rows_naming_the_id_are_rejected() {
        let _guard = test_guard();
        let mut menu_changed = menu("ruby:crate");
        menu_changed.slots = 54;
        assert_eq!(
            try_synchronize(&[
                content_mod(
                    "ruby",
                    NativeDeclarations {
                        menus: vec![menu("ruby:crate")],
                        ..NativeDeclarations::default()
                    },
                ),
                content_mod(
                    "ruby-extra",
                    NativeDeclarations {
                        menus: vec![menu_changed],
                        ..NativeDeclarations::default()
                    },
                ),
            ])
            .unwrap_err(),
            "conflicting native menu declaration ruby:crate"
        );
        let mut block_entity_changed = block_entity("ruby:crate");
        block_entity_changed.valid_blocks = vec!["ruby:other".into()];
        assert_eq!(
            try_synchronize(&[
                content_mod(
                    "ruby",
                    NativeDeclarations {
                        block_entities: vec![block_entity("ruby:crate")],
                        ..NativeDeclarations::default()
                    },
                ),
                content_mod(
                    "ruby-extra",
                    NativeDeclarations {
                        block_entities: vec![block_entity_changed],
                        ..NativeDeclarations::default()
                    },
                ),
            ])
            .unwrap_err(),
            "conflicting native block entity declaration ruby:crate"
        );
    }

    #[test]
    fn diagnostics_report_counts_and_coalesced_rows() {
        let _guard = test_guard();
        assert_eq!(
            JavaRegistrySnapshot::default().diagnostics(),
            [
                "java registry: 0 block ids, 0 item ids, 0 entity ids, 0 data component ids, 0 attachment ids, 0 menu ids, 0 block entity ids".to_string(),
                "java registry templates: 0 blocks, 0 items, 0 entities, 0 data components, 0 attachments, 0 menus, 0 block entities".to_string(),
            ]
        );
        let snapshot = synchronize(&[
            content_mod(
                "ruby",
                NativeDeclarations {
                    entities: vec![zombie_with_auxiliary()],
                    data_components: vec![data_component("ruby:power")],
                    menus: vec![menu("ruby:crate")],
                    block_entities: vec![block_entity("ruby:crate")],
                    ..NativeDeclarations::default()
                },
            ),
            content_mod(
                "ruby-extra",
                NativeDeclarations {
                    entities: vec![zombie_with_auxiliary()],
                    menus: vec![menu("ruby:crate")],
                    ..NativeDeclarations::default()
                },
            ),
        ]);
        assert_eq!(
            snapshot.diagnostics(),
            [
                "java registry: 0 block ids, 0 item ids, 1 entity ids, 1 data component ids, 0 attachment ids, 1 menu ids, 1 block entity ids".to_string(),
                "java registry templates: 0 blocks, 0 items, 1 entities, 1 data components, 0 attachments, 1 menus, 1 block entities".to_string(),
                "java registry coalesced duplicate rows: entities=1, menus=1".to_string(),
            ]
        );
    }

    #[test]
    fn diagnostics_name_the_menu_bindings() {
        let _guard = test_guard();
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                menus: vec![bound_menu("ruby:crate", &["ruby:crate_block"])],
                ..NativeDeclarations::default()
            },
        )]);
        assert_eq!(
            snapshot.diagnostics().last().map(String::as_str),
            Some("java registry menu bindings: ruby:crate_block opens ruby:crate")
        );
        // A menu bound to no block adds no line.
        let snapshot = synchronize(&[content_mod(
            "ruby",
            NativeDeclarations {
                menus: vec![menu("ruby:loose")],
                ..NativeDeclarations::default()
            },
        )]);
        assert_eq!(snapshot.diagnostics().len(), 2);
    }

    #[test]
    fn compatibility_lines_tie_registry_declarations_and_payloads_together() {
        use PayloadDirection::*;
        let _guard = test_guard();
        let mods = [content_mod(
            "ruby",
            NativeDeclarations {
                blocks: vec![block("ruby:crate_block")],
                items: vec![horizon_mods::content::Item {
                    id: "ruby:shard".into(),
                    template: "minecraft:stone".into(),
                    max_stack_size: None,
                    rarity: None,
                    components: Default::default(),
                    crafting_remainder: None,
                }],
                entities: vec![entity("ruby:zombie", "minecraft:zombie")],
                data_components: vec![data_component("ruby:power")],
                attachments: vec![attachment("ruby:power")],
                menus: vec![bound_menu("ruby:crate", &["ruby:crate_block"])],
                block_entities: vec![block_entity("ruby:crate")],
                ..NativeDeclarations::default()
            },
        )];
        let snapshot = synchronize(&mods);
        let declared = horizon_client::mod_content::declaration_summary(&mods);
        let payloads = PayloadSummary::from_declarations(&payload_map([
            payload("demo:up", PlayToServer, Some("minecraft:string")),
            payload("demo:down", PlayToClient, None),
        ]));
        assert_eq!(
            snapshot.summary(),
            RegistrySummary {
                blocks: 1,
                items: 1,
                entities: 1,
                data_components: 1,
                attachments: 1,
                menus: 1,
                block_entities: 1,
                menu_bindings: 1,
                block_entity_blocks: 1,
            }
        );
        assert_eq!(
            snapshot.compatibility_lines(&declared, &payloads),
            [
                "java compatibility: registry: 1 block ids, 1 item ids, 1 entity ids, 1 data component ids, 1 attachment ids, 1 menu ids, 1 block entity ids, 1 menu bindings, 1 block entity blocks",
                "java compatibility: declared: 1 block, 1 item, 1 entity, 1 data component, 1 attachment, 1 menu, 1 block entity",
                "java compatibility: declared: namespaces: ruby",
                "java compatibility: declared: ids: ruby:crate, ruby:crate_block, ruby:power, ruby:shard, ruby:zombie",
                "java compatibility: payload channels: 2 (1 playToClient, 1 playToServer, 0 configurationToClient, 0 configurationToServer, 0 playBidirectional), 1 supported codec(s), 1 opaque",
                "java compatibility: payload ids: demo:down, demo:up",
            ]
        );
        // Nothing declared and no channels: three fixed lines, same prefix.
        let empty = JavaRegistrySnapshot::default()
            .compatibility_lines(&Default::default(), &PayloadSummary::default());
        assert_eq!(
            empty,
            [
                "java compatibility: registry: 0 block ids, 0 item ids, 0 entity ids, 0 data component ids, 0 attachment ids, 0 menu ids, 0 block entity ids, 0 menu bindings, 0 block entity blocks",
                "java compatibility: declared: none",
                "java compatibility: payload channels: 0 (0 playToClient, 0 playToServer, 0 configurationToClient, 0 configurationToServer, 0 playBidirectional), 0 supported codec(s), 0 opaque",
            ]
        );
        assert!(
            empty
                .iter()
                .all(|line| line.starts_with(COMPATIBILITY_PREFIX))
        );
    }

    #[test]
    fn payload_summary_counts_directions_codecs_and_bounds_ids() {
        use PayloadDirection::*;
        let summary = PayloadSummary::from_declarations(&payload_map([
            payload("demo:a", PlayToClient, Some("minecraft:string")),
            payload("demo:b", PlayToServer, Some("minecraft:raw")),
            payload("demo:c", ConfigurationToClient, Some("minecraft:unit")),
            payload("demo:d", ConfigurationToServer, None),
            payload("demo:e", PlayBidirectional, Some("demo:codec")),
        ]));
        assert_eq!(summary.channels, 5);
        assert_eq!(summary.directions, [1, 1, 1, 1, 1]);
        assert_eq!(summary.supported_codecs, 3);
        assert_eq!(summary.opaque_codecs, 2);
        assert_eq!(
            summary.ids,
            ["demo:a", "demo:b", "demo:c", "demo:d", "demo:e"]
        );
        assert!(!summary.ids_truncated);
        // Past the bound only the count and the sorted prefix remain.
        let rows =
            (0..MAX_PAYLOAD_IDS + 2).map(|i| payload(&format!("demo:c{i:03}"), PlayToServer, None));
        let summary = PayloadSummary::from_declarations(&payload_map(rows));
        assert_eq!(summary.channels, MAX_PAYLOAD_IDS + 2);
        assert_eq!(summary.ids.len(), MAX_PAYLOAD_IDS);
        assert!(summary.ids_truncated);
        assert_eq!(summary.ids.first().map(String::as_str), Some("demo:c000"));
        let last = format!("demo:c{:03}", MAX_PAYLOAD_IDS - 1);
        assert_eq!(summary.ids.last(), Some(&last));
        let line = summary.lines().pop().unwrap();
        assert!(line.starts_with("payload ids: demo:c000, "), "{line}");
        assert!(line.ends_with(", ..."), "{line}");
    }

    #[test]
    fn rejected_batch_keeps_the_published_snapshot() {
        let _guard = test_guard();
        let good = content_mod(
            "ruby",
            NativeDeclarations {
                entities: vec![zombie_with_auxiliary()],
                menus: vec![menu("ruby:crate")],
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
        assert!(try_synchronize(&[good.clone(), conflicting]).is_err());
        assert_eq!(*state().read().unwrap(), published);
        // A menu opening from a block nobody declares rejects the batch too.
        let mut unknown = content_mod("ruby-extra", NativeDeclarations::default());
        let mut row = menu("ruby:ghost");
        row.open_blocks = vec!["othermod:ghost".into()];
        unknown.content.menus.push(row);
        assert!(try_synchronize(&[good.clone(), unknown]).is_err());
        assert_eq!(*state().read().unwrap(), published);
        let mut changed_menu = menu("ruby:crate");
        changed_menu.title_key = "container.ruby.other".into();
        let conflicting = content_mod(
            "ruby-extra",
            NativeDeclarations {
                menus: vec![changed_menu],
                ..NativeDeclarations::default()
            },
        );
        assert!(try_synchronize(&[good, conflicting]).is_err());
        assert_eq!(*state().read().unwrap(), published);
    }
}
