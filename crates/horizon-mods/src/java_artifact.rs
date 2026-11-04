//! The key under which a compat artifact — the launcher's per-jar check report,
//! a woven class set, a generated façade — is reused, and the reason it must be
//! rebuilt when that key moves.
//!
//! `docs/java-mod-distribution.md` §5 names what a façade's key is made of: the
//! game, the generator, the `--extra` API jars, the JDK and the mod ABI. This
//! module is that list as code, for any artifact: the class bytes and settings
//! it was made from — its *sources* — hashed together with the versions every
//! compatibility result depends on ([`Format`]: the game, the mod ABI, the hook
//! level and the IR version). The JDK and `--extra` jars are sources: a
//! `Source::of_file` each, so their own bytes move the key.
//!
//! A source hashes both the content it was made from ([`Source::of_bytes`], a
//! SHA-256) and the path that holds it, so swapping two classes' bytes under
//! each other is a different class set, not the same one. Two jars with
//! byte-identical classes give the same key, and a mod re-zipped or re-signed
//! does not rebuild what did not change.
//!
//! Nothing here builds anything. A stale artifact is detected by recomputing the
//! key from what is on disk now and comparing it with the one recorded beside
//! the artifact — the digest is of the inputs, the inputs are not the digest —
//! and every difference is reported in the vocabulary of the version that moved,
//! so a caller says "Horizon's mod ABI moved" instead of "key mismatch"
//! ([`Provenance::check`] → [`Status::explain`]).
//!
//! ```
//! use horizon_mods::java_artifact::{Abi, Format, Provenance, Source, Status};
//!
//! let format = Format::new(Abi {
//!     game: "26.3".into(),
//!     horizon_mod_abi: 7,
//!     native_hooks: None,
//! });
//! let sources = [Source::of_bytes("class", "example/Mod.class", b"\xca\xfe\xba\xbe")];
//! let recorded = Provenance::new("check", format.clone(), sources.clone(), "check.json");
//!
//! // The same classes against the same versions: reuse the report.
//! assert_eq!(recorded.check(format.clone(), &sources), Status::Fresh);
//!
//! // A mod-API function was added: the report is rebuilt, and the reason says why.
//! let mut later = format;
//! later.abi.horizon_mod_abi += 1;
//! assert_eq!(
//!     recorded.check(later, &sources).explain(),
//!     "rebuild: Horizon mod ABI 7 -> 8"
//! );
//! ```
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

/// Which versions a compat artifact was made against (`docs/java-mod-distribution.md`
/// §5, "Invalidation"). Each is a component of the key, so a move rebuilds what
/// it invalidates and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Abi {
    /// What Minecraft states it is: `horizon_core::version::VERSION_ID`, `26.3`.
    /// A move invalidates every artifact: the game's names and shapes are what
    /// they were generated and checked against.
    pub game: String,
    /// Horizon's own mod ABI: the level a mod's code needs
    /// (`horizon_mods::api_level::LEVEL`), i.e. `horizon_mod_abi_version`.
    /// A UI-only release leaves it alone and reprocesses nothing.
    pub horizon_mod_abi: u32,
    /// Horizon's native hook points (`NATIVE_HOOKS_API`), which a woven class
    /// set or a hook shim is translated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_hooks: Option<u32>,
}

/// The intermediate format an artifact was produced in. A producer bumps
/// [`IR_VERSION`] when a smarter check, planner or weaver can now answer
/// differently for the same inputs: the old answer is not reused, because it
/// was produced by a different program.
pub const IR_VERSION: u32 = 1;

/// How a call answers today: the IR version and the versions it depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub ir_version: u32,
    pub abi: Abi,
}

impl Format {
    /// This crate's IR version with a caller's versions.
    pub fn new(abi: Abi) -> Self {
        Self {
            ir_version: IR_VERSION,
            abi,
        }
    }
}

/// One input an artifact was made from.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// `class`, `jar`, `facade`, `jdk`, `config`, `setting`: what the thing is,
    /// so a difference reads as "the JDK moved", not "entry 3 changed".
    pub kind: String,
    /// The jar entry for a class (`a/b/C.class`), else the path or the flag
    /// name. [`Source::absent`] prefixes `~` so a needed input that was not
    /// there keys apart from one that was.
    pub path: String,
    /// The input's SHA-256, lower-case hex.
    pub sha256: String,
}

impl Source {
    /// A source from content held in memory, normally one class file.
    ///
    /// `path` is compared as well as the bytes, so two classes swapping bytes
    /// is a different artifact.
    pub fn of_bytes(kind: &str, path: &str, bytes: &[u8]) -> Self {
        Self {
            kind: kind.into(),
            path: path.into(),
            sha256: sha256_hex(bytes),
        }
    }

    /// A source from a file on disk.
    pub fn of_file(kind: &str, path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self::of_bytes(kind, &path.display().to_string(), &bytes))
    }

    /// The source for an input whose absence must still change the key: a check
    /// that looked for a game class in the jar it was given answered
    /// differently depending on whether it found it, and both answers are keyed
    /// apart.
    pub fn absent(kind: &str, path: &str) -> Self {
        Self::of_bytes(kind, &format!("~{path}"), b"")
    }
}

/// The recorded provenance of one artifact: what it was made from, against
/// which versions, and the key that answers "may this be reused?".
///
/// The key is the SHA-256 of [`Provenance::canonical`], the provenance with the
/// digest field left out, as compact JSON with sorted keys — the same shape
/// Python's `json.dumps(sort_keys=True, separators=(",", ":"))` writes, so a
/// tool can check a key without this crate. [`Provenance::check`] re-derives it,
/// so an edit to any other field of a stored record is caught, not trusted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// What the artifact is: `check`, `facade`, `woven`, `tier`, `probe`.
    pub report: String,
    pub format: Format,
    /// Sorted by `(kind, path, sha256)` when built by [`Provenance::new`], so a
    /// caller's enumeration order cannot change the key.
    pub inputs: Vec<Source>,
    /// The artifact's own name inside the cache folder (`check.json`).
    pub output: String,
    /// The key derived from everything above.
    pub key: String,
}

impl Provenance {
    /// Record an artifact made from `inputs` against `format`.
    pub fn new(
        report: &str,
        format: Format,
        inputs: impl IntoIterator<Item = Source>,
        output: &str,
    ) -> Self {
        let mut this = Self {
            report: report.into(),
            format,
            inputs: normalized(inputs),
            output: output.into(),
            key: String::new(),
        };
        this.key = this.derive();
        this
    }

    /// The compact, sorted, digest-free form the key hashes: the record without
    /// `key`, as JSON with its keys sorted, which is the shape Python's
    /// `json.dumps(sort_keys=True, separators=(",", ":"))` writes. Non-ASCII is
    /// escaped as `\uXXXX` (Python's default, `ensure_ascii=True`), so a key is
    /// the same bytes whichever tool computes it.
    pub fn canonical(&self) -> String {
        let inputs = normalized(self.inputs.iter().cloned())
            .iter()
            .map(|source| {
                serde_json::json!({
                    "kind": source.kind,
                    "path": source.path,
                    "sha256": source.sha256,
                })
            })
            .collect::<Vec<_>>();
        // `serde_json`'s map is a `BTreeMap` (this crate does not enable
        // `preserve_order`), so its output is already sorted by key.
        let mut abi = serde_json::json!({
            "game": self.format.abi.game,
            "horizon_mod_abi": self.format.abi.horizon_mod_abi,
        });
        if let Some(hooks) = self.format.abi.native_hooks {
            abi["native_hooks"] = serde_json::json!(hooks);
        }
        let json = serde_json::json!({
            "report": self.report,
            "format": {
                "ir_version": self.format.ir_version,
                "abi": abi,
            },
            "inputs": inputs,
            "output": self.output,
        });
        escape_non_ascii(serde_json::to_string(&json).expect("a record of strings and numbers"))
    }

    /// The key [`Self::canonical`] gives.
    pub fn derive(&self) -> String {
        sha256_hex(self.canonical().as_bytes())
    }

    /// Whether this record still describes an artifact made from exactly these
    /// sources against this format — and whether its stored key is the one its
    /// own contents give. A record or canonical form edited by hand, or a file
    /// written halfway, fails here, so a corrupt entry re-derives instead of
    /// being reused.
    pub fn check(&self, format: Format, inputs: &[Source]) -> Status {
        if self.derive() != self.key {
            return Status::Corrupt;
        }
        let sources = normalized(inputs.iter().cloned());
        let now = Provenance {
            format: format.clone(),
            inputs: sources.clone(),
            ..self.clone()
        }
        .derive();
        if now == self.key {
            return Status::Fresh;
        }
        let mut changes = self.differences(&format, &sources);
        changes.sort();
        changes.dedup();
        Status::Stale(changes)
    }

    /// Every component that differs, in the words the causing version uses.
    fn differences(&self, format: &Format, inputs: &[Source]) -> Vec<String> {
        let mut changes = Vec::new();
        let (was, now) = (self.format.clone(), format);
        if was.ir_version != now.ir_version {
            changes.push(format!(
                "IR format {} -> {} (this program answers differently)",
                was.ir_version, now.ir_version
            ));
        }
        if was.abi.game != now.abi.game {
            changes.push(format!(
                "Minecraft {} -> {} (names and shapes regenerated)",
                was.abi.game, now.abi.game
            ));
        }
        if was.abi.horizon_mod_abi != now.abi.horizon_mod_abi {
            changes.push(format!(
                "Horizon mod ABI {} -> {}",
                was.abi.horizon_mod_abi, now.abi.horizon_mod_abi
            ));
        }
        if was.abi.native_hooks != now.abi.native_hooks {
            let name = |v: Option<u32>| v.map_or_else(|| "none".into(), |v| v.to_string());
            changes.push(format!(
                "native hooks {} -> {}",
                name(was.abi.native_hooks),
                name(now.abi.native_hooks)
            ));
        }
        let same = |a: &Source, b: &Source| a.kind == b.kind && a.path == b.path;
        let mut before: Vec<&Source> = self.inputs.iter().collect();
        let mut after: Vec<&Source> = inputs.iter().collect();
        before.sort();
        after.sort();
        before.dedup();
        after.dedup();
        // One report per input: an input at the same kind and path with new
        // bytes changed, one that is gone has left, one that is new has
        // appeared. An edit is one line, never "left" plus "appeared".
        for source in &before {
            match after.iter().find(|now| same(source, now)) {
                Some(now) if now.sha256 != source.sha256 => {
                    changes.push(format!("{} {} changed", source.kind, source.path));
                }
                Some(_) => {}
                None => changes.push(format!("{} {} left", source.kind, source.path)),
            }
        }
        for source in &after {
            if !before.iter().any(|was| same(was, source)) {
                changes.push(format!("{} {} appeared", source.kind, source.path));
            }
        }
        changes
    }
}

impl std::fmt::Display for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} [{}] from {} input(s), Minecraft {}, Horizon mod ABI {}, IR {}",
            self.report,
            self.output,
            self.inputs.len(),
            self.format.abi.game,
            self.format.abi.horizon_mod_abi,
            self.format.ir_version
        )
    }
}

/// What [`Provenance::check`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Same sources, same versions, consistent record: reuse the artifact.
    Fresh,
    /// Different sources or versions: rebuild, for these reasons.
    Stale(Vec<String>),
    /// The record's own key does not match its contents: rebuild, and say so in
    /// the log rather than trusting a hand-edited or half-written file.
    Corrupt,
}

impl Status {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Self::Fresh)
    }

    /// The reasons a rebuild is needed, empty when fresh.
    pub fn reasons(&self) -> Vec<String> {
        match self {
            Self::Fresh => Vec::new(),
            Self::Stale(reasons) => reasons.clone(),
            Self::Corrupt => vec!["recorded key does not match its own contents".into()],
        }
    }

    /// One line for a log, naming what moved.
    pub fn explain(&self) -> String {
        match self {
            Self::Fresh => "reusable".into(),
            _ => format!("rebuild: {}", self.reasons().join("; ")),
        }
    }
}

fn normalized(inputs: impl IntoIterator<Item = Source>) -> Vec<Source> {
    let mut inputs: Vec<Source> = inputs.into_iter().collect();
    inputs.sort();
    inputs.dedup();
    inputs
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// JSON string escaping as `serde_json` and Python's `json.dumps` write it:
/// the two-character escapes where JSON names one, `\u00XX` for the other
/// control characters, and everything ASCII verbatim. Non-ASCII is escaped
/// too, so a key is the same bytes whichever tool computes it; `serde_json`
/// and Python read the escapes back to the same string, and the fixture test
/// pins the equivalence.
fn escape_non_ascii(json: String) -> String {
    if json.is_ascii() {
        return json;
    }
    let mut out = String::with_capacity(json.len() + 8);
    for ch in json.chars() {
        if ch.is_ascii() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
