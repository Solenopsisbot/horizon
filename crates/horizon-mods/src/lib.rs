//! Horizon's mod files (`docs/mods.md`): content mods (`.hmod`: blocks, items, data and
//! assets, no code), modpacks and, with `index`, the mod registry's index. Code mods are
//! native (docs/native-mods.md).
pub mod api_level;
pub mod content;
#[cfg(feature = "index")]
pub mod index;
/// The cache key of a native Java compat artifact (`docs/java-mod-distribution.md`
/// §5): its class bytes, Horizon's ABI and the IR version.
pub mod java_artifact;
pub mod manifest;
pub mod modpack;
pub mod native;
pub mod payloads;

pub use content::Content;
pub use manifest::{Capability, Manifest, Package, Side, find};
