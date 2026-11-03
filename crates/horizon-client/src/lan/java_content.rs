//! The declared Java content a hosted world ships to its guests at login.
//!
//! A content mod's ids live in the host's registries; a guest without the mod
//! jars cannot resolve them, so the host sends this manifest on [`CHANNEL`]
//! right after the payload channel registration. The format is bounded and
//! deterministic: registries in one order, ids and rows sorted and
//! deduplicated, explicit counts and lengths, and trailing bytes reject.

use crate::mod_content::{
    self, BlockEntityRow, DeclaredExtraIds, DeclaredIds, MAX_DECLARED_BLOCK_ENTITIES,
    MAX_DECLARED_IDS,
};
use std::sync::atomic::{AtomicBool, Ordering};

/// The channel the login manifest travels on.
pub const CHANNEL: &str = "horizon:java_content";

/// The manifest version this build writes and accepts.
pub const VERSION: u16 = 1;

/// The largest manifest frame, the custom-payload limit.
pub const MAX_FRAME: usize = 1024 * 1024;

/// The most ids one registry may carry.
pub const MAX_IDS: usize = MAX_DECLARED_IDS;

/// The most block-entity rows one manifest may carry.
pub const MAX_BLOCK_ENTITIES: usize = MAX_DECLARED_BLOCK_ENTITIES;

/// The longest id or resource location one entry may name.
pub const MAX_TEXT: usize = 1024;

/// The registries in wire order: blocks, items, effects, entity types, data
/// components, attachments, menus and block entity types.
const REGISTRIES: usize = 8;

/// Everything a content root declares, ready to travel. Ids are stored as
/// declared; [`encode`](Self::encode) sorts and deduplicates them, so the
/// same content always gives the same bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JavaContentManifest {
    pub blocks: Vec<String>,
    pub items: Vec<String>,
    pub effects: Vec<String>,
    pub entity_types: Vec<String>,
    pub data_components: Vec<String>,
    pub attachments: Vec<String>,
    pub menus: Vec<String>,
    pub block_entity_types: Vec<String>,
    pub block_entities: Vec<BlockEntityRow>,
}

impl JavaContentManifest {
    /// The declarations the last content root recorded on this process: what
    /// the host sends, and what a guest would have if it had the mods.
    pub fn from_stored() -> Self {
        let ids = mod_content::stored_declared_ids();
        let extra = mod_content::stored_declared_extra_ids();
        let block_entities = mod_content::stored_declared_block_entities()
            .into_iter()
            .map(|(block, row)| BlockEntityRow {
                block,
                id: row.id,
                template: row.template,
                ticker: row.ticker,
            })
            .collect();
        Self {
            blocks: ids.blocks,
            items: ids.items,
            effects: ids.effects,
            entity_types: ids.entity_types,
            data_components: extra.data_components,
            attachments: extra.attachments,
            menus: ids.menus,
            block_entity_types: ids.block_entity_types,
            block_entities,
        }
    }

    /// No declaration to send; the host skips the packet.
    pub fn is_empty(&self) -> bool {
        self.ids().iter().all(|ids| ids.is_empty()) && self.block_entities.is_empty()
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

    /// The frame: `u16` version, then per registry a `u16` count and its ids
    /// (`u16` length, UTF-8 bytes), then a `u16` row count and per row the
    /// block, declared id and template, and a `u8` ticker flag.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut writer = Writer::default();
        writer.u16(VERSION)?;
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
        Ok(writer.bytes)
    }

    /// Decode one frame. Every count and length is checked against its cap,
    /// text is validated as UTF-8, the ticker flag must be 0 or 1, and any
    /// byte after the last row rejects. Ids and rows come back sorted and
    /// deduplicated.
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        if data.len() > MAX_FRAME {
            return Err(format!(
                "declared Java content is {} bytes (max {MAX_FRAME})",
                data.len()
            ));
        }
        let mut reader = Reader { bytes: data, at: 0 };
        let version = reader.u16()?;
        if version != VERSION {
            return Err(format!(
                "declared Java content version {version} is not {VERSION}"
            ));
        }
        let mut manifest = Self::default();
        let mut fields = [
            &mut manifest.blocks,
            &mut manifest.items,
            &mut manifest.effects,
            &mut manifest.entity_types,
            &mut manifest.data_components,
            &mut manifest.attachments,
            &mut manifest.menus,
            &mut manifest.block_entity_types,
        ];
        for field in &mut fields {
            let count = reader.count(MAX_IDS, "ids")?;
            let mut ids = Vec::with_capacity(count);
            for _ in 0..count {
                ids.push(reader.text()?);
            }
            ids.sort();
            ids.dedup();
            **field = ids;
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
        manifest.block_entities = rows;
        if reader.at != data.len() {
            return Err(format!(
                "declared Java content carries {} trailing bytes",
                data.len() - reader.at
            ));
        }
        Ok(manifest)
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
        )
    }
}

/// Decode one received frame and store it; the client's whole handler.
pub fn apply(data: &[u8]) -> Result<usize, String> {
    JavaContentManifest::decode(data)?.apply()
}

static REJECTED: AtomicBool = AtomicBool::new(false);

/// Log a rejected manifest once: a broken or conflicting host should not
/// fill the log with one line per packet.
pub fn warn_once(error: &str) {
    if !REJECTED.swap(true, Ordering::Relaxed) {
        log::warn!("Horizon: declared Java content was rejected: {error}");
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

    fn count(&mut self, count: usize) -> Result<(), String> {
        let count = u16::try_from(count)
            .map_err(|_| format!("declared Java content count {count} overflows"))?;
        self.u16(count)
    }

    fn text(&mut self, text: &str) -> Result<(), String> {
        let len = text.len();
        if len > MAX_TEXT {
            return Err(format!(
                "declared Java content text is {len} bytes (max {MAX_TEXT})"
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
        let len = self.u16()? as usize;
        if len > MAX_TEXT {
            return Err(format!(
                "declared Java content text is {len} bytes (max {MAX_TEXT})"
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
        frame.extend_from_slice(&VERSION.to_be_bytes());
        frame.extend_from_slice(&(u16::try_from(MAX_IDS + 1).unwrap()).to_be_bytes());
        assert!(JavaContentManifest::decode(&frame).is_err());

        let mut frame = Vec::new();
        frame.extend_from_slice(&VERSION.to_be_bytes());
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
        frame.extend_from_slice(&VERSION.to_be_bytes());
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
        frame.extend_from_slice(&VERSION.to_be_bytes());
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
    fn applying_a_manifest_stores_it_for_the_guest() {
        let _guard = STORE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The stores are process-wide, so start from no declarations.
        mod_content::clear_declared_java_content();
        let manifest = manifest();
        let count = manifest.apply().unwrap();
        assert!(count > 0);
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
        mod_content::apply_declared_java_content(local, DeclaredExtraIds::default(), []).unwrap();
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
