//! The declared Java content a hosted world ships to its guests at login.
//!
//! A content mod's ids live in the host's registries; a guest without the mod
//! jars cannot resolve them, so the host sends this manifest on [`CHANNEL`]
//! right after the payload channel registration. The format is bounded and
//! deterministic: registries in one order, ids and rows sorted and
//! deduplicated, explicit counts and lengths, and trailing bytes reject.

use crate::mod_content::{
    self, BlockEntityRow, DeclaredExtraIds, DeclaredIds, EntityRow, MAX_DECLARED_BLOCK_ENTITIES,
    MAX_DECLARED_ENTITIES, MAX_DECLARED_IDS, MAX_DECLARED_MENUS, MAX_DECLARED_OPEN_BLOCKS,
    MAX_DECLARED_TEXT, MenuRow,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The channel the login manifest travels on.
pub const CHANNEL: &str = "horizon:java_content";

/// The manifest version this build writes: a v3 header (the declared-content
/// epoch and a content hash), then the v1 body and the v2 menu and entity
/// template sections.
pub const VERSION: u16 = 3;

/// The second manifest version, still decoded: the v1 body and the bounded
/// template sections, with no header.
pub const VERSION_V2: u16 = 2;

/// The first manifest version, still decoded: ids and block-entity rows only.
pub const VERSION_V1: u16 = 1;

/// The largest manifest frame, the custom-payload limit.
pub const MAX_FRAME: usize = 1024 * 1024;

/// The most ids one registry may carry.
pub const MAX_IDS: usize = MAX_DECLARED_IDS;

/// The most block-entity rows one manifest may carry.
pub const MAX_BLOCK_ENTITIES: usize = MAX_DECLARED_BLOCK_ENTITIES;

/// The most menu rows, entity rows and open blocks one section may carry.
pub const MAX_MENUS: usize = MAX_DECLARED_MENUS;
pub const MAX_ENTITIES: usize = MAX_DECLARED_ENTITIES;
pub const MAX_OPEN_BLOCKS: usize = MAX_DECLARED_OPEN_BLOCKS;

/// The longest id or resource location one entry may name.
pub const MAX_TEXT: usize = 1024;

/// The longest text one v2 section row may carry.
pub const MAX_SECTION_TEXT: usize = MAX_DECLARED_TEXT;

/// The registries in wire order: blocks, items, effects, entity types, data
/// components, attachments, menus and block entity types.
const REGISTRIES: usize = 8;

/// The v2 sections, in wire order. A section is a `u8` kind and a `u16` count;
/// kinds must strictly ascend, so every manifest has one canonical byte order.
/// Kind 3 stays reserved for block and item templates: `mod_content` keeps
/// their declared ids but no stored template row, and this build does not
/// invent one, so the section is never written.
const SECTION_MENUS: u8 = 1;
const SECTION_ENTITIES: u8 = 2;
const SECTIONS: usize = 2;

/// Everything a content root declares, ready to travel. Ids are stored as
/// declared; [`encode`](Self::encode) sorts and deduplicates them, so the
/// same content always gives the same bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JavaContentManifest {
    /// The declared-content epoch the host wrote in the v3 header
    /// ([`mod_content::declared_java_content_epoch`]); 0 for a v1 or v2 frame,
    /// which carries no header.
    pub epoch: u64,
    pub blocks: Vec<String>,
    pub items: Vec<String>,
    pub effects: Vec<String>,
    pub entity_types: Vec<String>,
    pub data_components: Vec<String>,
    pub attachments: Vec<String>,
    pub menus: Vec<String>,
    pub block_entity_types: Vec<String>,
    pub block_entities: Vec<BlockEntityRow>,
    /// v2 menu templates: the bounded title text, slots and open blocks.
    pub menu_rows: Vec<MenuRow>,
    /// v2 entity templates: vanilla template, category and spawn flags.
    pub entity_rows: Vec<EntityRow>,
}

impl JavaContentManifest {
    /// The declarations the last content root recorded on this process: what
    /// the host sends, and what a guest would have if it had the mods.
    pub fn from_stored() -> Self {
        let (epoch, stored) = mod_content::declared_java_content_snapshot();
        let mod_content::DeclaredJavaContent {
            ids,
            extra,
            block_entities,
            menus,
            entities,
        } = stored;
        let block_entities = block_entities
            .into_iter()
            .map(|(block, row)| BlockEntityRow {
                block,
                id: row.id,
                template: row.template,
                ticker: row.ticker,
            })
            .collect();
        Self {
            epoch,
            blocks: ids.blocks,
            items: ids.items,
            effects: ids.effects,
            entity_types: ids.entity_types,
            data_components: extra.data_components,
            attachments: extra.attachments,
            menus: ids.menus,
            block_entity_types: ids.block_entity_types,
            block_entities,
            menu_rows: menus.into_values().collect(),
            entity_rows: entities.into_values().collect(),
        }
    }

    /// No declaration to send; the host skips the packet.
    pub fn is_empty(&self) -> bool {
        self.ids().iter().all(|ids| ids.is_empty())
            && self.block_entities.is_empty()
            && self.menu_rows.is_empty()
            && self.entity_rows.is_empty()
    }

    fn ids(&self) -> [&[String]; REGISTRIES] {
        [
            self.blocks.as_slice(),
            self.items.as_slice(),
            self.effects.as_slice(),
            self.entity_types.as_slice(),
            self.data_components.as_slice(),
            self.attachments.as_slice(),
            self.menus.as_slice(),
            self.block_entity_types.as_slice(),
        ]
    }

    /// The frame: a `u16` version 3 header (`u64` declared-content epoch and
    /// `u64` content hash), then the v1 body (per registry a `u16` count and
    /// its ids, then a `u16` row count and per row the block, declared id and
    /// template, and a `u8` ticker flag), then a `u16` section count and the
    /// menu and entity sections in kind order. The hash covers the
    /// declarations, so the same content always gives the same header.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut body = Writer::default();
        self.encode_body(&mut body)?;
        self.encode_sections(&mut body)?;
        let mut writer = Writer::default();
        writer.u16(VERSION)?;
        writer.u64(self.epoch)?;
        writer.u64(self.content_hash()?)?;
        writer.raw(&body.bytes)?;
        Ok(writer.bytes)
    }

    /// The v2 menu and entity template sections, in ascending kind order.
    fn encode_sections(&self, writer: &mut Writer) -> Result<(), String> {
        let mut menus = self.menu_rows.clone();
        sort_unique_menus(&mut menus)?;
        if menus.len() > MAX_MENUS {
            return Err(format!("declared menus {} exceed {MAX_MENUS}", menus.len()));
        }
        let mut entities = self.entity_rows.clone();
        sort_unique_entities(&mut entities)?;
        if entities.len() > MAX_ENTITIES {
            return Err(format!(
                "declared entities {} exceed {MAX_ENTITIES}",
                entities.len()
            ));
        }
        writer.count(SECTIONS)?;
        writer.u8(SECTION_MENUS)?;
        writer.count(menus.len())?;
        for row in &menus {
            writer.section_text(&row.id)?;
            writer.section_text(&row.title)?;
            writer.u16(row.slots)?;
            let mut blocks = row.open_blocks.clone();
            blocks.sort();
            blocks.dedup();
            if blocks.len() > MAX_OPEN_BLOCKS {
                return Err(format!(
                    "declared menu {} has {} open blocks (max {MAX_OPEN_BLOCKS})",
                    row.id,
                    blocks.len()
                ));
            }
            writer.count(blocks.len())?;
            for block in &blocks {
                writer.section_text(block)?;
            }
        }
        writer.u8(SECTION_ENTITIES)?;
        writer.count(entities.len())?;
        for row in &entities {
            writer.section_text(&row.id)?;
            writer.section_text(&row.template)?;
            writer.section_text(&row.category)?;
            if row.flags & !mod_content::ENTITY_FLAG_MASK != 0 {
                return Err(format!(
                    "declared entity {} flags {:#04x} are not bounded spawn flags",
                    row.id, row.flags
                ));
            }
            writer.u8(row.flags)?;
        }
        Ok(())
    }

    /// These declarations as one normalized snapshot, without touching the
    /// stores: the same ids and rows [`apply`](Self::apply) would store.
    fn declared(&self) -> Result<mod_content::DeclaredJavaContent, String> {
        mod_content::normalized_declared_java_content(
            DeclaredIds {
                blocks: self.blocks.clone(),
                items: self.items.clone(),
                effects: self.effects.clone(),
                entity_types: self.entity_types.clone(),
                menus: self.menus.clone(),
                block_entity_types: self.block_entity_types.clone(),
            },
            DeclaredExtraIds {
                data_components: self.data_components.clone(),
                attachments: self.attachments.clone(),
            },
            self.block_entities.clone(),
            self.menu_rows.clone(),
            self.entity_rows.clone(),
        )
    }

    /// The declarations' content hash, as the v3 header carries it: equal for
    /// two manifests declaring the same thing, whatever their row order.
    pub fn content_hash(&self) -> Result<u64, String> {
        Ok(self.declared()?.content_hash())
    }

    /// The v1 body, shared by both versions: the eight registries and the
    /// block-entity rows.
    fn encode_body(&self, writer: &mut Writer) -> Result<(), String> {
        for ids in self.ids() {
            let mut sorted: Vec<&String> = ids.iter().collect();
            sorted.sort();
            sorted.dedup();
            if sorted.len() > MAX_IDS {
                return Err(format!("declared ids {} exceed {MAX_IDS}", sorted.len()));
            }
            writer.count(sorted.len())?;
            for id in sorted {
                writer.text(id)?;
            }
        }
        let mut rows = self.block_entities.clone();
        rows.sort();
        rows.dedup();
        if rows.len() > MAX_BLOCK_ENTITIES {
            return Err(format!(
                "declared block entities {} exceed {MAX_BLOCK_ENTITIES}",
                rows.len()
            ));
        }
        writer.count(rows.len())?;
        for row in &rows {
            writer.text(&row.block)?;
            writer.text(&row.id)?;
            writer.text(&row.template)?;
            writer.u8(u8::from(row.ticker))?;
        }
        Ok(())
    }

    /// Decode one frame, v1, v2 or v3. Every count and length is checked
    /// against its cap, text is validated as UTF-8, the ticker flag must be 0
    /// or 1, section kinds must ascend without repeats (an unknown kind
    /// rejects), and any byte after the last row rejects. A v3 header's hash
    /// must match the declarations it covers. Ids and rows come back sorted
    /// and deduplicated; a v1 frame has no templates.
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        Self::decode_frame(data).map(|(manifest, _)| manifest)
    }

    /// [`decode`](Self::decode) plus the version the frame declared, so a
    /// caller can apply a v3 header's checks and leave a v1 or v2 frame to the
    /// plain merge rules.
    pub fn decode_frame(data: &[u8]) -> Result<(Self, u16), String> {
        if data.len() > MAX_FRAME {
            return Err(format!(
                "declared Java content is {} bytes (max {MAX_FRAME})",
                data.len()
            ));
        }
        let mut reader = Reader { bytes: data, at: 0 };
        let version = reader.u16()?;
        if version != VERSION && version != VERSION_V2 && version != VERSION_V1 {
            return Err(format!(
                "declared Java content version {version} is not {VERSION}, \
                 {VERSION_V2} or {VERSION_V1}"
            ));
        }
        let mut manifest = Self::default();
        let declared_hash = if version == VERSION {
            manifest.epoch = reader.u64()?;
            Some(reader.u64()?)
        } else {
            None
        };
        manifest.decode_body(&mut reader)?;
        if version == VERSION || version == VERSION_V2 {
            manifest.decode_sections(&mut reader)?;
        }
        if reader.at != data.len() {
            return Err(format!(
                "declared Java content carries {} trailing bytes",
                data.len() - reader.at
            ));
        }
        if let Some(declared_hash) = declared_hash {
            let hash = manifest.content_hash()?;
            if hash != declared_hash {
                return Err(format!(
                    "declared Java content hash {hash:#018x} is not the header's \
                     {declared_hash:#018x}"
                ));
            }
        }
        Ok((manifest, version))
    }

    fn decode_body(&mut self, reader: &mut Reader) -> Result<(), String> {
        let fields = [
            &mut self.blocks,
            &mut self.items,
            &mut self.effects,
            &mut self.entity_types,
            &mut self.data_components,
            &mut self.attachments,
            &mut self.menus,
            &mut self.block_entity_types,
        ];
        for field in fields {
            let count = reader.count(MAX_IDS, "ids")?;
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(reader.text()?);
            }
            ids.sort();
            ids.dedup();
            *field = ids;
        }
        let count = reader.count(MAX_BLOCK_ENTITIES, "block entity rows")?;
        let mut rows = Vec::with_capacity(count);
        for _ in 0..count {
            let block = reader.text()?;
            let id = reader.text()?;
            let template = reader.text()?;
            let ticker = match reader.u8()? {
                0 => false,
                1 => true,
                other => {
                    return Err(format!(
                        "declared Java content ticker flag {other} is not 0 or 1"
                    ));
                }
            };
            rows.push(BlockEntityRow {
                block,
                id,
                template,
                ticker,
            });
        }
        rows.sort();
        rows.dedup();
        self.block_entities = rows;
        Ok(())
    }

    fn decode_sections(&mut self, reader: &mut Reader) -> Result<(), String> {
        let sections = reader.count(SECTIONS, "sections")?;
        let mut last = 0u8;
        for _ in 0..sections {
            let kind = reader.u8()?;
            if kind <= last {
                return Err(format!(
                    "declared Java content section {kind} is out of order"
                ));
            }
            last = kind;
            match kind {
                SECTION_MENUS => {
                    let count = reader.count(MAX_MENUS, "menus")?;
                    let mut rows = Vec::with_capacity(count);
                    for _ in 0..count {
                        let id = reader.section_text()?;
                        let title = reader.section_text()?;
                        let slots = reader.u16()?;
                        let blocks = reader.count(MAX_OPEN_BLOCKS, "menu open blocks")?;
                        let mut open_blocks = Vec::with_capacity(blocks);
                        for _ in 0..blocks {
                            open_blocks.push(reader.section_text()?);
                        }
                        rows.push(MenuRow {
                            id,
                            title,
                            slots,
                            open_blocks,
                        });
                    }
                    sort_unique_menus(&mut rows)?;
                    self.menu_rows = rows;
                }
                SECTION_ENTITIES => {
                    let count = reader.count(MAX_ENTITIES, "entities")?;
                    let mut rows = Vec::with_capacity(count);
                    for _ in 0..count {
                        let id = reader.section_text()?;
                        let template = reader.section_text()?;
                        let category = reader.section_text()?;
                        let flags = reader.u8()?;
                        if flags & !mod_content::ENTITY_FLAG_MASK != 0 {
                            return Err(format!(
                                "declared entity {id} flags {flags:#04x} are not bounded spawn flags"
                            ));
                        }
                        rows.push(EntityRow {
                            id,
                            template,
                            category,
                            flags,
                        });
                    }
                    sort_unique_entities(&mut rows)?;
                    self.entity_rows = rows;
                }
                other => {
                    return Err(format!("declared Java content has unknown section {other}"));
                }
            }
        }
        Ok(())
    }

    /// Store this manifest into the process's declaration stores, returning
    /// how many declarations it applied. See
    /// [`mod_content::merge_declared_java_content`] for the conflict rule.
    pub fn apply(&self) -> Result<usize, String> {
        mod_content::apply_declared_java_content(
            DeclaredIds {
                blocks: self.blocks.clone(),
                items: self.items.clone(),
                effects: self.effects.clone(),
                entity_types: self.entity_types.clone(),
                menus: self.menus.clone(),
                block_entity_types: self.block_entity_types.clone(),
            },
            DeclaredExtraIds {
                data_components: self.data_components.clone(),
                attachments: self.attachments.clone(),
            },
            self.block_entities.clone(),
            self.menu_rows.clone(),
            self.entity_rows.clone(),
        )
    }
}

/// Sort rows by id and drop exact repeats; a second, different row for one id
/// is ambiguous and rejects rather than picking a winner.
fn sort_unique_menus(rows: &mut Vec<MenuRow>) -> Result<(), String> {
    for row in rows.iter_mut() {
        row.open_blocks.sort();
        row.open_blocks.dedup();
    }
    rows.sort();
    rows.dedup();
    if let Some(pair) = rows.windows(2).find(|pair| pair[0].id == pair[1].id) {
        return Err(format!("declared menu {} is listed twice", pair[0].id));
    }
    Ok(())
}

fn sort_unique_entities(rows: &mut Vec<EntityRow>) -> Result<(), String> {
    rows.sort();
    rows.dedup();
    if let Some(pair) = rows.windows(2).find(|pair| pair[0].id == pair[1].id) {
        return Err(format!("declared entity {} is listed twice", pair[0].id));
    }
    Ok(())
}

/// What one received manifest did to the declarations this process holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The manifest was stored; this many declarations resolve now.
    Applied(usize),
    /// The frame was well formed but the client already holds a different,
    /// non-empty declaration set; the local content is unchanged and the
    /// bounded one-line reason is for the log.
    Refused(String),
}

/// The declared-content epoch of the last v3 manifest this client applied, 0
/// until one is. Diagnostics, and a later post-login re-send, use it to tell
/// "the host's declarations changed" from "the same content again".
static APPLIED_EPOCH: AtomicU64 = AtomicU64::new(0);

/// The declared-content epoch of the last v3 manifest this client applied.
pub fn applied_epoch() -> u64 {
    APPLIED_EPOCH.load(Ordering::Acquire)
}

/// Decode one received frame and store it; the client's whole handler. A v3
/// header is checked against the local declarations first, so a guest with its
/// own mods keeps them.
pub fn apply(data: &[u8]) -> Result<Outcome, String> {
    let (manifest, version) = JavaContentManifest::decode_frame(data)?;
    manifest.apply_frame(version)
}

impl JavaContentManifest {
    /// Apply a decoded frame of `version`. A v1 or v2 frame follows the plain
    /// merge rule. A v3 frame is refused, leaving the stores untouched, when
    /// the process already holds different non-empty declarations; the header
    /// hash was already checked by [`decode_frame`](Self::decode_frame).
    fn apply_frame(&self, version: u16) -> Result<Outcome, String> {
        if version != VERSION {
            return self.apply().map(Outcome::Applied);
        }
        if !self.is_empty() {
            let local = mod_content::stored_declared_java_content_hash();
            let incoming = self.content_hash()?;
            if local != 0 && local != incoming {
                return Ok(Outcome::Refused(refusal_reason(
                    self.epoch, local, incoming,
                )));
            }
        }
        let count = self.apply()?;
        APPLIED_EPOCH.store(self.epoch, Ordering::Release);
        Ok(Outcome::Applied(count))
    }
}

/// The longest refusal line [`refusal_reason`] builds; the log and any future
/// chat line stay bounded whatever the host sends.
pub const MAX_REFUSAL: usize = 160;

/// One bounded, single-line reason for keeping the local declarations.
fn refusal_reason(epoch: u64, local: u64, incoming: u64) -> String {
    let reason = format!(
        "server declared Java content {incoming:#018x} (epoch {epoch}) but this client \
         already holds {local:#018x}; keeping the local content"
    );
    // Control characters never reach the log line, and it stays within
    // `MAX_REFUSAL` bytes whatever the host's numbers were.
    let mut bounded = String::with_capacity(MAX_REFUSAL);
    for c in reason.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if bounded.len() + c.len_utf8() > MAX_REFUSAL {
            break;
        }
        bounded.push(c);
    }
    bounded
}

static REJECTED: AtomicBool = AtomicBool::new(false);

/// Log a rejected manifest once: a broken or conflicting host should not
/// fill the log with one line per packet.
pub fn warn_once(error: &str) {
    if !REJECTED.swap(true, Ordering::Relaxed) {
        log::warn!("Horizon: declared Java content was rejected: {error}");
    }
}

static REFUSED: AtomicBool = AtomicBool::new(false);

/// Log a refused manifest once: a host re-sending conflicting declarations
/// should not fill the log one line per frame.
pub fn warn_refused_once(reason: &str) {
    if !REFUSED.swap(true, Ordering::Relaxed) {
        log::warn!("Horizon: kept the local Java content: {reason}");
    }
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn reserve(&self, extra: usize) -> Result<(), String> {
        if self.bytes.len().saturating_add(extra) > MAX_FRAME {
            return Err(format!("declared Java content exceeds {MAX_FRAME} bytes"));
        }
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), String> {
        self.reserve(1)?;
        self.bytes.push(value);
        Ok(())
    }

    fn u16(&mut self, value: u16) -> Result<(), String> {
        self.reserve(2)?;
        self.bytes.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn u64(&mut self, value: u64) -> Result<(), String> {
        self.reserve(8)?;
        self.bytes.extend_from_slice(&value.to_be_bytes());
        Ok(())
    }

    fn raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn count(&mut self, count: usize) -> Result<(), String> {
        let count = u16::try_from(count)
            .map_err(|_| format!("declared Java content count {count} overflows"))?;
        self.u16(count)
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        self.text_max(text, MAX_TEXT)
    }

    fn section_text(&mut self, text: &str) -> Result<(), String> {
        self.text_max(text, MAX_SECTION_TEXT)
    }

    fn text_max(&mut self, text: &str, max: usize) -> Result<(), String> {
        let len = text.len();
        if len > max {
            return Err(format!(
                "declared Java content text is {len} bytes (max {max})"
            ));
        }
        self.count(len)?;
        self.reserve(len)?;
        self.bytes.extend_from_slice(text.as_bytes());
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u8(&mut self) -> Result<u8, String> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| "declared Java content is truncated".to_owned())?;
        self.at += 1;
        Ok(byte)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let hi = self.u8()?;
        let lo = self.u8()?;
        Ok(u16::from_be_bytes([hi, lo]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let mut bytes = [0u8; 8];
        for byte in &mut bytes {
            *byte = self.u8()?;
        }
        Ok(u64::from_be_bytes(bytes))
    }

    fn count(&mut self, max: usize, what: &str) -> Result<usize, String> {
        let count = self.u16()? as usize;
        if count > max {
            return Err(format!(
                "declared Java content lists {count} {what} (max {max})"
            ));
        }
        Ok(count)
    }

    fn text(&mut self) -> Result<String, String> {
        self.text_max(MAX_TEXT)
    }

    fn section_text(&mut self) -> Result<String, String> {
        self.text_max(MAX_SECTION_TEXT)
    }

    fn text_max(&mut self, max: usize) -> Result<String, String> {
        let len = self.u16()? as usize;
        if len > max {
            return Err(format!(
                "declared Java content text is {len} bytes (max {max})"
            ));
        }
        let end = self
            .at
            .checked_add(len)
            .ok_or_else(|| "declared Java content length overflows".to_owned())?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or_else(|| "declared Java content is truncated".to_owned())?;
        self.at = end;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| "declared Java content text is not UTF-8".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// The stores are process-wide; the tests that write them take turns.
    static STORE_LOCK: Mutex<()> = Mutex::new(());

    fn manifest() -> JavaContentManifest {
        JavaContentManifest {
            epoch: 0,
            blocks: vec!["ruby:block".into()],
            items: vec!["ruby:block".into(), "ruby:gem".into()],
            effects: vec!["ruby:glow".into()],
            entity_types: vec!["ruby:beast".into()],
            data_components: vec!["ruby:charge".into()],
            attachments: vec!["ruby:mark".into()],
            menus: vec!["ruby:crate".into()],
            block_entity_types: vec!["ruby:crate_entity".into()],
            block_entities: vec![
                BlockEntityRow {
                    block: "ruby:box".into(),
                    id: "ruby:crate_entity".into(),
                    template: "minecraft:barrel".into(),
                    ticker: true,
                },
                BlockEntityRow {
                    block: "ruby:crate".into(),
                    id: "ruby:crate_entity".into(),
                    template: "minecraft:barrel".into(),
                    ticker: true,
                },
            ],
            menu_rows: vec![MenuRow {
                id: "ruby:crate".into(),
                title: "container.ruby.crate".into(),
                slots: 27,
                open_blocks: vec!["ruby:box".into(), "ruby:crate".into()],
            }],
            entity_rows: vec![EntityRow {
                id: "ruby:beast".into(),
                template: "minecraft:pig".into(),
                category: "creature".into(),
                flags: mod_content::ENTITY_FLAG_SPAWN | mod_content::ENTITY_FLAG_FIRE_IMMUNE,
            }],
        }
    }

    fn row(block: &str, id: &str, template: &str) -> BlockEntityRow {
        BlockEntityRow {
            block: block.into(),
            id: id.into(),
            template: template.into(),
            ticker: false,
        }
    }

    #[test]
    fn every_registry_and_block_entities_round_trip() {
        let source = manifest();
        let bytes = source.encode().unwrap();
        let decoded = JavaContentManifest::decode(&bytes).unwrap();
        assert_eq!(decoded, source);
        // Every registry came back sorted and deduplicated.
        assert_eq!(decoded.items, ["ruby:block", "ruby:gem"]);
        assert_eq!(decoded.blocks, ["ruby:block"]);
        assert_eq!(decoded.effects, ["ruby:glow"]);
        assert_eq!(decoded.entity_types, ["ruby:beast"]);
        assert_eq!(decoded.data_components, ["ruby:charge"]);
        assert_eq!(decoded.attachments, ["ruby:mark"]);
        assert_eq!(decoded.menus, ["ruby:crate"]);
        assert_eq!(decoded.block_entity_types, ["ruby:crate_entity"]);
        assert_eq!(decoded.block_entities.len(), 2);
        // The v2 template sections came back too.
        assert_eq!(decoded.menu_rows.len(), 1);
        assert_eq!(decoded.menu_rows[0].title, "container.ruby.crate");
        assert_eq!(decoded.menu_rows[0].slots, 27);
        assert_eq!(decoded.entity_rows[0].template, "minecraft:pig");
        assert_eq!(decoded.entity_rows[0].category, "creature");
    }

    /// A v1 frame still decodes: ids and block-entity rows, no templates.
    #[test]
    fn v1_frames_decode_without_templates() {
        let source = manifest();
        let mut writer = Writer::default();
        writer.u16(VERSION_V1).unwrap();
        source.encode_body(&mut writer).unwrap();
        let decoded = JavaContentManifest::decode(&writer.bytes).unwrap();
        assert_eq!(decoded.blocks, ["ruby:block"]);
        assert_eq!(decoded.block_entities.len(), 2);
        assert!(decoded.menu_rows.is_empty());
        assert!(decoded.entity_rows.is_empty());
    }

    #[test]
    fn empty_manifest_round_trips_and_is_skipped() {
        let empty = JavaContentManifest::default();
        assert!(empty.is_empty());
        let decoded = JavaContentManifest::decode(&empty.encode().unwrap()).unwrap();
        assert_eq!(decoded, empty);
        assert!(!manifest().is_empty());
    }

    #[test]
    fn encoding_is_deterministic_whatever_the_order() {
        let mut shuffled = manifest();
        shuffled.items = vec!["ruby:gem".into(), "ruby:block".into(), "ruby:gem".into()];
        shuffled.block_entities.reverse();
        shuffled.menu_rows[0].open_blocks.reverse();
        shuffled.menu_rows.push(shuffled.menu_rows[0].clone());
        shuffled.entity_rows.push(shuffled.entity_rows[0].clone());
        assert_eq!(
            manifest().encode().unwrap(),
            shuffled.encode().unwrap(),
            "same declarations, same bytes"
        );
    }

    #[test]
    fn truncated_and_trailing_frames_reject() {
        let bytes = manifest().encode().unwrap();
        for len in [0, 1, 2, bytes.len() - 1] {
            assert!(
                JavaContentManifest::decode(&bytes[..len]).is_err(),
                "a {len}-byte prefix must not decode"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(JavaContentManifest::decode(&trailing).is_err());
    }

    #[test]
    fn oversize_counts_and_text_reject() {
        let mut too_many = manifest();
        too_many.blocks = (0..=MAX_IDS).map(|i| format!("ruby:b{i}")).collect();
        assert!(too_many.encode().is_err());

        let mut too_long = manifest();
        too_long.effects = vec!["r".repeat(MAX_TEXT + 1)];
        assert!(too_long.encode().is_err());

        // A hand-built frame may lie about a count or length.
        let mut frame = Vec::new();
        frame.extend_from_slice(&VERSION_V1.to_be_bytes());
        frame.extend_from_slice(&(u16::try_from(MAX_IDS + 1).unwrap()).to_be_bytes());
        assert!(JavaContentManifest::decode(&frame).is_err());

        let mut frame = Vec::new();
        frame.extend_from_slice(&VERSION_V1.to_be_bytes());
        frame.extend_from_slice(&0u16.to_be_bytes()); // blocks
        frame.extend_from_slice(&1u16.to_be_bytes()); // items
        frame.extend_from_slice(&(u16::try_from(MAX_TEXT + 1).unwrap()).to_be_bytes());
        assert!(JavaContentManifest::decode(&frame).is_err());
    }

    #[test]
    fn invalid_utf8_version_and_ticker_flags_reject() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&(VERSION + 1).to_be_bytes());
        assert!(JavaContentManifest::decode(&frame).is_err());

        let mut frame = Vec::new();
        frame.extend_from_slice(&VERSION_V1.to_be_bytes());
        for _ in 0..REGISTRIES {
            frame.extend_from_slice(&0u16.to_be_bytes());
        }
        frame.extend_from_slice(&1u16.to_be_bytes()); // one row
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.push(0xff); // block is not UTF-8
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.push(b'a');
        frame.extend_from_slice(&1u16.to_be_bytes());
        frame.push(b'a');
        frame.push(0);
        assert!(JavaContentManifest::decode(&frame).is_err());

        let mut frame = Vec::new();
        frame.extend_from_slice(&VERSION_V1.to_be_bytes());
        for _ in 0..REGISTRIES {
            frame.extend_from_slice(&0u16.to_be_bytes());
        }
        frame.extend_from_slice(&1u16.to_be_bytes());
        for _ in 0..3 {
            frame.extend_from_slice(&1u16.to_be_bytes());
            frame.push(b'a');
        }
        frame.push(2); // not a boolean
        assert!(JavaContentManifest::decode(&frame).is_err());
    }

    #[test]
    fn v2_sections_are_bounded_and_reject_bad_frames() {
        let source = manifest();
        // Text, open blocks and flags are capped.
        let mut long = source.clone();
        long.menu_rows[0].title = "x".repeat(MAX_SECTION_TEXT + 1);
        assert!(long.encode().is_err());
        let mut many = source.clone();
        many.menu_rows[0].open_blocks = (0..=MAX_OPEN_BLOCKS)
            .map(|i| format!("ruby:b{i}"))
            .collect();
        assert!(many.encode().is_err());
        let mut bad_flags = source.clone();
        bad_flags.entity_rows[0].flags = 0xff;
        assert!(bad_flags.encode().is_err());
        // Two different rows for one id are ambiguous.
        let mut twice = source.clone();
        let mut other = twice.menu_rows[0].clone();
        other.title = "container.ruby.other".into();
        twice.menu_rows.push(other);
        assert!(twice.encode().is_err());

        // Unknown, repeated and out-of-order section kinds reject.
        let body = |writer: &mut Writer| {
            writer.u16(VERSION_V2).unwrap();
            source.encode_body(writer).unwrap();
        };
        let mut frame = Writer::default();
        body(&mut frame);
        frame.count(1).unwrap();
        frame.u8(9).unwrap();
        assert!(JavaContentManifest::decode(&frame.bytes).is_err());
        let mut frame = Writer::default();
        body(&mut frame);
        frame.count(2).unwrap();
        frame.u8(SECTION_ENTITIES).unwrap();
        frame.count(0).unwrap();
        frame.u8(SECTION_MENUS).unwrap();
        frame.count(0).unwrap();
        assert!(JavaContentManifest::decode(&frame.bytes).is_err());
        let mut frame = Writer::default();
        body(&mut frame);
        frame.count(1).unwrap();
        frame.u8(SECTION_MENUS).unwrap();
        frame.count(1).unwrap();
        frame.count(1).unwrap();
        frame.u8(0xff).unwrap(); // not UTF-8
        assert!(JavaContentManifest::decode(&frame.bytes).is_err());
        // A v2 frame cut before its sections is truncated.
        let mut frame = Writer::default();
        frame.u16(VERSION_V2).unwrap();
        source.encode_body(&mut frame).unwrap();
        assert!(JavaContentManifest::decode(&frame.bytes).is_err());
        assert!(JavaContentManifest::decode(&frame.bytes[..frame.bytes.len() - 1]).is_err());
    }

    /// A v3 frame carries the declaring root's epoch and a hash of the
    /// declarations in its header; the epoch survives the round trip, and a
    /// header hash that does not match the declarations rejects.
    #[test]
    fn v3_header_carries_the_epoch_and_a_content_hash() {
        let mut source = manifest();
        source.epoch = 7;
        let bytes = source.encode().unwrap();
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), VERSION);
        let (decoded, version) = JavaContentManifest::decode_frame(&bytes).unwrap();
        assert_eq!(version, VERSION);
        assert_eq!(decoded, source);
        assert_eq!(decoded.epoch, 7);
        // Another epoch is the same declarations, so the hash is unchanged
        // and only the epoch bytes differ.
        let mut other_epoch = manifest();
        other_epoch.epoch = 9;
        assert_eq!(
            source.content_hash().unwrap(),
            other_epoch.content_hash().unwrap()
        );
        let other = other_epoch.encode().unwrap();
        assert_ne!(bytes[2..10], other[2..10], "the epoch is in the header");
        assert_eq!(bytes[10..], other[10..], "the declarations are identical");
        // A changed declaration changes the hash.
        let mut other_content = manifest();
        other_content.blocks.push("ruby:other".into());
        assert_ne!(
            source.content_hash().unwrap(),
            other_content.content_hash().unwrap()
        );
        // Flipping a header hash byte rejects before anything is applied.
        let mut wrong_hash = bytes.clone();
        wrong_hash[17] ^= 0xff;
        assert!(JavaContentManifest::decode(&wrong_hash).is_err());
    }

    /// A v2 frame still decodes exactly as before: body and template sections,
    /// no header and therefore epoch 0.
    #[test]
    fn v2_frames_decode_without_a_header() {
        let source = manifest();
        let mut writer = Writer::default();
        writer.u16(VERSION_V2).unwrap();
        source.encode_body(&mut writer).unwrap();
        source.encode_sections(&mut writer).unwrap();
        let (decoded, version) = JavaContentManifest::decode_frame(&writer.bytes).unwrap();
        assert_eq!(version, VERSION_V2);
        assert_eq!(decoded.epoch, 0);
        assert_eq!(decoded.menu_rows, source.menu_rows);
        assert_eq!(decoded.entity_rows, source.entity_rows);
        assert_eq!(JavaContentManifest::decode(&writer.bytes).unwrap(), source);
    }

    #[test]
    fn applying_a_manifest_stores_it_for_the_guest() {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The stores are process-wide, so start from no declarations.
        mod_content::clear_declared_java_content();
        let manifest = manifest();
        let epoch_before = mod_content::declared_java_content_epoch();
        let count = manifest.apply().unwrap();
        assert!(count > 0);
        assert!(mod_content::declared_java_content_epoch() > epoch_before);
        let ids = mod_content::stored_declared_ids();
        assert_eq!(ids.blocks, ["ruby:block"]);
        assert_eq!(ids.items, ["ruby:block", "ruby:gem"]);
        assert_eq!(ids.menus, ["ruby:crate"]);
        assert_eq!(
            mod_content::stored_declared_extra_ids().data_components,
            ["ruby:charge"]
        );
        let entities = mod_content::stored_declared_block_entities();
        assert_eq!(entities["ruby:crate"].template, "minecraft:barrel");
        assert_eq!(
            entities["ruby:crate"].valid_blocks,
            ["ruby:box", "ruby:crate"]
        );
        let menus = mod_content::stored_declared_menus();
        assert_eq!(menus["ruby:crate"].title, "container.ruby.crate");
        assert_eq!(menus["ruby:crate"].slots, 27);
        let entity_rows = mod_content::stored_declared_entities();
        assert_eq!(entity_rows["ruby:beast"].template, "minecraft:pig");
        assert_eq!(entity_rows["ruby:beast"].category, "creature");
        // The host rebuilds the same v2 sections from the stores.
        let stored = JavaContentManifest::from_stored();
        assert_eq!(stored.menu_rows, manifest.menu_rows);
        assert_eq!(stored.entity_rows, manifest.entity_rows);
        // The same manifest again is idempotent.
        assert_eq!(manifest.apply().unwrap(), count);
        // A different non-empty manifest conflicts and leaves the stores.
        let mut other = manifest.clone();
        other.blocks = vec!["ruby:other".into()];
        assert!(other.apply().is_err());
        assert_eq!(mod_content::stored_declared_ids().blocks, ["ruby:block"]);
    }

    #[test]
    fn applying_a_manifest_over_local_declarations_conflicts() {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        mod_content::clear_declared_java_content();
        let local = DeclaredIds {
            blocks: vec!["ruby:local".into()],
            ..Default::default()
        };
        mod_content::apply_declared_java_content(local, DeclaredExtraIds::default(), [], [], [])
            .unwrap();
        assert!(manifest().apply().is_err());
        // An empty incoming manifest is not a conflict and changes nothing.
        let empty = JavaContentManifest::default();
        assert_eq!(empty.apply().unwrap(), 1);
        assert_eq!(
            mod_content::stored_declared_ids().blocks,
            ["ruby:local"],
            "local declarations survive"
        );
    }

    /// A v3 frame whose declarations differ from the non-empty local ones is
    /// refused with a bounded one-line reason: nothing is applied, no epoch is
    /// recorded, and the same frame applies once the local content is gone.
    #[test]
    fn a_v3_manifest_over_different_local_declarations_is_refused() {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        mod_content::clear_declared_java_content();
        let local = DeclaredIds {
            blocks: vec!["ruby:local".into()],
            ..Default::default()
        };
        mod_content::apply_declared_java_content(local, DeclaredExtraIds::default(), [], [], [])
            .unwrap();
        assert_ne!(mod_content::stored_declared_java_content_hash(), 0);
        let epoch_before = mod_content::declared_java_content_epoch();
        let applied_before = applied_epoch();

        let mut incoming = manifest();
        incoming.epoch = 12;
        let bytes = incoming.encode().unwrap();
        match apply(&bytes).unwrap() {
            Outcome::Refused(reason) => {
                assert!(reason.contains("keeping the local content"), "{reason}");
                assert!(reason.len() <= MAX_REFUSAL);
                assert!(!reason.contains('\n'));
                assert!(reason.is_ascii());
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(mod_content::stored_declared_ids().blocks, ["ruby:local"]);
        assert_eq!(mod_content::declared_java_content_epoch(), epoch_before);
        assert_eq!(applied_epoch(), applied_before, "a refusal records nothing");

        // With no local declarations the same frame applies and is recorded.
        mod_content::clear_declared_java_content();
        assert_eq!(
            apply(&bytes).unwrap(),
            Outcome::Applied(mod_content::stored_declared_java_content().count())
        );
        assert_eq!(mod_content::stored_declared_ids().blocks, ["ruby:block"]);
        assert_eq!(applied_epoch(), 12);
        // Re-sending the same content is the same hash, so not a conflict.
        assert!(matches!(apply(&bytes).unwrap(), Outcome::Applied(_)));
        assert_eq!(applied_epoch(), 12);
    }

    /// An empty v3 frame is not a conflict, as an empty v2 frame never was.
    #[test]
    fn an_empty_v3_frame_over_local_declarations_is_not_refused() {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        mod_content::clear_declared_java_content();
        let local = DeclaredIds {
            blocks: vec!["ruby:local".into()],
            ..Default::default()
        };
        mod_content::apply_declared_java_content(local, DeclaredExtraIds::default(), [], [], [])
            .unwrap();
        let empty = JavaContentManifest::default();
        let outcome = apply(&empty.encode().unwrap()).unwrap();
        assert!(matches!(outcome, Outcome::Applied(_)), "{outcome:?}");
        assert_eq!(mod_content::stored_declared_ids().blocks, ["ruby:local"]);
    }

    #[test]
    fn rows_keep_their_optional_ticker_flag() {
        let mut source = manifest();
        source.block_entities = vec![
            BlockEntityRow {
                block: "ruby:crate".into(),
                id: "ruby:crate_entity".into(),
                template: "minecraft:barrel".into(),
                ticker: true,
            },
            row("ruby:still", "ruby:still_entity", "minecraft:chest"),
        ];
        let decoded = JavaContentManifest::decode(&source.encode().unwrap()).unwrap();
        let ticker: Vec<bool> = decoded
            .block_entities
            .iter()
            .map(|row| row.ticker)
            .collect();
        assert_eq!(ticker, [true, false]);
    }
}
