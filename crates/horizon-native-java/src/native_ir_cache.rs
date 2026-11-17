//! A bounded cache envelope for native-Java IR produced by a caller.
//!
//! This module does not parse bytecode or compile Java. It gives a backend a
//! durable artifact path with enough provenance to reject an artifact made for
//! different bytes, an engine, or a routing decision. The payload is opaque to
//! this crate and is limited so a malformed jar cannot grow the cache without
//! bound.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io, path::PathBuf, time::SystemTime};

/// The on-disk envelope version. A changed envelope is a cache miss.
pub const ARTIFACT_SCHEMA: u32 = 1;
/// Maximum encoded IR payload in one artifact.
pub const MAX_PAYLOAD_BYTES: usize = 8 << 20;
/// Maximum engine or hook identifier length.
const MAX_LABEL_BYTES: usize = 128;
/// Maximum cache entries. The cache's total byte limit is enforced as well.
pub const MAX_ENTRIES: usize = 4096;
/// Maximum total bytes retained by a cache root.
pub const MAX_CACHE_BYTES: u64 = 256 << 20;
/// An entry larger than this is treated as corrupt before parsing it.
const MAX_ARTIFACT_BYTES: u64 = 16 << 20;

/// The native backend that owns an artifact.
///
/// A fallback JVM is intentionally absent. Such execution is not native IR
/// and must be recorded by the caller's compatibility report instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeIrBackend {
    Interpreter,
    HostHook,
}

/// The routing decision that selected a native backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeIrRoute {
    pub backend: NativeIrBackend,
    /// The stable hook name when `backend` is `HostHook`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<String>,
}

impl NativeIrRoute {
    /// Route an artifact through the bounded bytecode interpreter.
    pub fn interpreter() -> Self {
        Self {
            backend: NativeIrBackend::Interpreter,
            hook: None,
        }
    }

    /// Route an artifact through one named Horizon host hook.
    pub fn host_hook(hook: impl Into<String>) -> Result<Self, CacheError> {
        let hook = bounded_label(hook.into(), "hook")?;
        Ok(Self {
            backend: NativeIrBackend::HostHook,
            hook: Some(hook),
        })
    }

    fn validate(&self) -> Result<(), CacheError> {
        match (&self.backend, &self.hook) {
            (NativeIrBackend::Interpreter, None) => Ok(()),
            (NativeIrBackend::Interpreter, Some(_)) => Err(CacheError::InvalidMetadata(
                "interpreter routing cannot carry a hook".into(),
            )),
            (NativeIrBackend::HostHook, Some(hook)) => {
                bounded_label(hook.clone(), "hook").map(|_| ())
            }
            (NativeIrBackend::HostHook, None) => Err(CacheError::InvalidMetadata(
                "host-hook routing requires a hook".into(),
            )),
        }
    }
}

/// Source and engine identity carried by every artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeIrProvenance {
    /// SHA-256 of the exact source jar/class bytes used to build the payload.
    pub source_sha256: String,
    pub source_size: u64,
    /// Build identity of the backend that produced the payload.
    pub engine: String,
}

impl NativeIrProvenance {
    /// Build provenance from source bytes and an engine identity.
    pub fn from_source(source: &[u8], engine: impl Into<String>) -> Result<Self, CacheError> {
        let mut digest = Sha256::new();
        digest.update(source);
        Ok(Self {
            source_sha256: hex(&digest.finalize()),
            source_size: source.len() as u64,
            engine: bounded_label(engine.into(), "engine")?,
        })
    }

    fn validate(&self) -> Result<(), CacheError> {
        if !is_sha256(&self.source_sha256) {
            return Err(CacheError::InvalidMetadata(
                "source_sha256 must be 64 hexadecimal characters".into(),
            ));
        }
        bounded_label(self.engine.clone(), "engine").map(|_| ())
    }
}

/// One immutable native IR artifact envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeIrArtifact {
    pub schema: u32,
    pub provenance: NativeIrProvenance,
    pub route: NativeIrRoute,
    /// Backend-owned serialized IR. This crate never interprets it.
    pub payload: Vec<u8>,
}

impl NativeIrArtifact {
    /// Construct an artifact after checking its bounded metadata and payload.
    pub fn new(
        provenance: NativeIrProvenance,
        route: NativeIrRoute,
        payload: Vec<u8>,
    ) -> Result<Self, CacheError> {
        let artifact = Self {
            schema: ARTIFACT_SCHEMA,
            provenance,
            route,
            payload,
        };
        artifact.validate()?;
        Ok(artifact)
    }

    fn validate(&self) -> Result<(), CacheError> {
        if self.schema != ARTIFACT_SCHEMA {
            return Err(CacheError::Schema(self.schema));
        }
        self.provenance.validate()?;
        self.route.validate()?;
        if self.payload.len() > MAX_PAYLOAD_BYTES {
            return Err(CacheError::PayloadTooLarge(self.payload.len()));
        }
        Ok(())
    }
}

/// A cache operation failed or rejected an artifact.
#[derive(Debug)]
pub enum CacheError {
    Io(io::Error),
    Json(serde_json::Error),
    Schema(u32),
    PayloadTooLarge(usize),
    ArtifactTooLarge(u64),
    InvalidMetadata(String),
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "native IR cache I/O error: {error}"),
            Self::Json(error) => write!(f, "native IR cache JSON error: {error}"),
            Self::Schema(schema) => write!(f, "unsupported native IR cache schema {schema}"),
            Self::PayloadTooLarge(size) => {
                write!(f, "native IR payload is {size} bytes, over the bound")
            }
            Self::ArtifactTooLarge(size) => {
                write!(f, "native IR artifact is {size} bytes, over the bound")
            }
            Self::InvalidMetadata(reason) => write!(f, "invalid native IR metadata: {reason}"),
        }
    }
}

impl std::error::Error for CacheError {}

impl From<io::Error> for CacheError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// A filesystem-backed native IR cache.
#[derive(Debug, Clone)]
pub struct NativeIrCache {
    root: PathBuf,
}

impl NativeIrCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The artifact path for metadata. The source hash and engine keep entries
    /// from different inputs apart, while the route is verified from JSON.
    pub fn artifact_path(&self, provenance: &NativeIrProvenance) -> Result<PathBuf, CacheError> {
        provenance.validate()?;
        Ok(self.root.join(format!(
            "{}-{}.json",
            provenance.source_sha256,
            file_label(&provenance.engine)
        )))
    }

    /// Read a matching artifact. Corrupt, stale, or oversized files are
    /// removed and reported as a miss.
    pub fn load(
        &self,
        provenance: &NativeIrProvenance,
        route: &NativeIrRoute,
    ) -> Result<Option<NativeIrArtifact>, CacheError> {
        provenance.validate()?;
        route.validate()?;
        let path = self.artifact_path(provenance)?;
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if metadata.len() > MAX_ARTIFACT_BYTES {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let artifact: NativeIrArtifact = match serde_json::from_slice(&bytes) {
            Ok(artifact) => artifact,
            Err(_) => {
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
        };
        if artifact.validate().is_err()
            || artifact.provenance != *provenance
            || artifact.route != *route
        {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
            let _ = file.set_modified(SystemTime::now());
        }
        Ok(Some(artifact))
    }

    /// Atomically store an artifact. A temporary file prevents a crash during
    /// serialization from producing a partially readable cache entry.
    pub fn store(&self, artifact: &NativeIrArtifact) -> Result<(), CacheError> {
        artifact.validate()?;
        let path = self.artifact_path(&artifact.provenance)?;
        let bytes = serde_json::to_vec(artifact)?;
        if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
            return Err(CacheError::ArtifactTooLarge(bytes.len() as u64));
        }
        fs::create_dir_all(&self.root)?;
        let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
        fs::write(&temporary, bytes)?;
        if let Err(error) = fs::rename(&temporary, &path) {
            let _ = fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }

    /// Remove least-recently-used entries until count and byte bounds hold.
    pub fn prune(&self) -> Result<(), CacheError> {
        let Ok(directory) = fs::read_dir(&self.root) else {
            return Ok(());
        };
        let mut entries = Vec::new();
        for item in directory {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let metadata = item.metadata()?;
            if metadata.is_file() {
                entries.push((
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    metadata.len(),
                    path,
                ));
            }
        }
        let mut bytes = entries.iter().map(|(_, size, _)| *size).sum::<u64>();
        let mut count = entries.len();
        entries.sort_by_key(|(modified, _, _)| *modified);
        for (_, size, path) in entries {
            if bytes <= MAX_CACHE_BYTES && count <= MAX_ENTRIES {
                break;
            }
            if fs::remove_file(path).is_ok() {
                bytes = bytes.saturating_sub(size);
                count -= 1;
            }
        }
        Ok(())
    }
}

fn bounded_label(value: String, field: &str) -> Result<String, CacheError> {
    if value.is_empty()
        || value.len() > MAX_LABEL_BYTES
        || value.contains('/')
        || value.contains('\\')
    {
        return Err(CacheError::InvalidMetadata(format!(
            "{field} must be 1..={MAX_LABEL_BYTES} bytes without path separators"
        )));
    }
    Ok(value)
}

fn file_label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_' || byte == b'-' {
                byte as char
            } else {
                '_'
            }
        })
        .collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0xf)] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("horizon-native-ir-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn provenance(source: &[u8]) -> NativeIrProvenance {
        NativeIrProvenance::from_source(source, "engine-test").unwrap()
    }

    #[test]
    fn route_and_provenance_survive_a_cache_round_trip() {
        let root = scratch("round-trip");
        let cache = NativeIrCache::new(&root);
        let provenance = provenance(b"class bytes");
        let route = NativeIrRoute::host_hook("player_tick").unwrap();
        let artifact =
            NativeIrArtifact::new(provenance.clone(), route.clone(), vec![1, 2, 3]).unwrap();
        cache.store(&artifact).unwrap();
        let loaded = cache.load(&provenance, &route).unwrap().unwrap();
        assert_eq!(loaded, artifact);
        assert!(cache.artifact_path(&provenance).unwrap().starts_with(&root));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn changed_source_or_route_is_a_miss() {
        let root = scratch("identity");
        let cache = NativeIrCache::new(&root);
        let first = provenance(b"first");
        let route = NativeIrRoute::interpreter();
        cache
            .store(&NativeIrArtifact::new(first.clone(), route.clone(), vec![7]).unwrap())
            .unwrap();
        assert!(
            cache
                .load(&provenance(b"second"), &route)
                .unwrap()
                .is_none()
        );
        let hook = NativeIrRoute::host_hook("other_hook").unwrap();
        assert!(cache.load(&first, &hook).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_entries_are_deleted_and_oversized_payloads_are_rejected() {
        let root = scratch("bounds");
        let cache = NativeIrCache::new(&root);
        let provenance = provenance(b"source");
        let path = cache.artifact_path(&provenance).unwrap();
        fs::write(&path, b"not json").unwrap();
        assert!(
            cache
                .load(&provenance, &NativeIrRoute::interpreter())
                .unwrap()
                .is_none()
        );
        assert!(!path.exists());
        assert!(matches!(
            NativeIrArtifact::new(
                provenance,
                NativeIrRoute::interpreter(),
                vec![0; MAX_PAYLOAD_BYTES + 1]
            ),
            Err(CacheError::PayloadTooLarge(_))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_keeps_the_newest_entries_inside_the_count_bound() {
        let root = scratch("prune");
        let cache = NativeIrCache::new(&root);
        let now = SystemTime::now();
        for index in 0..(MAX_ENTRIES + 2) {
            let artifact = NativeIrArtifact::new(
                provenance(format!("source-{index}").as_bytes()),
                NativeIrRoute::interpreter(),
                vec![index as u8],
            )
            .unwrap();
            cache.store(&artifact).unwrap();
            let path = cache.artifact_path(&artifact.provenance).unwrap();
            if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
                let _ = file.set_modified(now - Duration::from_secs(index as u64));
            }
        }
        cache.prune().unwrap();
        let count = fs::read_dir(&root).unwrap().count();
        assert!(count <= MAX_ENTRIES);
        let _ = fs::remove_dir_all(root);
    }
}
