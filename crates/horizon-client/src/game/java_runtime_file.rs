//! Bounded, atomic diagnostics for the Java compatibility runtime.
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub(crate) const SCHEMA_VERSION: u32 = 1;
const MAX_BYTES: usize = 256 * 1024;
const MAX_JARS: usize = 256;
const MAX_STRING: usize = 240;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeJar {
    pub(crate) file: String,
    pub(crate) loader: String,
    pub(crate) engine: String,
    pub(crate) woven: usize,
    pub(crate) skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_skip_reason: Option<String>,
    pub(crate) registrations: usize,
    pub(crate) opaque: usize,
    pub(crate) payload_channels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) isolation_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeReport {
    pub(crate) schema_version: u32,
    pub(crate) jars: Vec<RuntimeJar>,
}

fn cut(value: &str) -> String {
    let mut out: String = value.chars().take(MAX_STRING).collect();
    if value.chars().count() > MAX_STRING {
        out.push('…');
    }
    out
}

fn bounded(mut report: RuntimeReport) -> RuntimeReport {
    report.jars.truncate(MAX_JARS);
    for jar in &mut report.jars {
        jar.file = cut(&jar.file);
        jar.loader = cut(&jar.loader);
        jar.engine = cut(&jar.engine);
        jar.first_skip_reason = jar.first_skip_reason.as_deref().map(cut);
        jar.isolation_failure = jar.isolation_failure.as_deref().map(cut);
        jar.payload_channels = jar
            .payload_channels
            .iter()
            .map(|channel| cut(channel))
            .take(64)
            .collect();
    }
    report
}

/// Write the report under `home` without exposing a partially written file.
/// The final document is always valid JSON and is capped at 256 KiB.
pub(crate) fn write(home: &Path, report: RuntimeReport) -> std::io::Result<()> {
    let mut report = bounded(report);
    while serde_json::to_vec(&report).map_or(true, |bytes| bytes.len() > MAX_BYTES) {
        if report.jars.pop().is_none() {
            break;
        }
    }
    let bytes = serde_json::to_vec(&report).expect("runtime report is serializable");
    fs::create_dir_all(home)?;
    let path = home.join("java-runtime.json");
    let part = home.join(".java-runtime.json.part");
    fs::write(&part, bytes)?;
    fs::rename(part, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bounds() {
        let dir = std::env::temp_dir().join(format!("horizon-java-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let report = RuntimeReport {
            schema_version: SCHEMA_VERSION,
            jars: (0..400)
                .map(|_| RuntimeJar {
                    file: "x".repeat(10_000),
                    loader: "fabric".into(),
                    engine: "native".into(),
                    woven: 1,
                    skipped: 1,
                    first_skip_reason: Some("reason".repeat(1000)),
                    registrations: 2,
                    opaque: 3,
                    payload_channels: vec!["play".repeat(1000)],
                    isolation_failure: None,
                })
                .collect(),
        };
        write(&dir, report).unwrap();
        let bytes = fs::read(dir.join("java-runtime.json")).unwrap();
        assert!(bytes.len() <= MAX_BYTES);
        let parsed: RuntimeReport = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert!(parsed.jars.len() <= MAX_JARS);
        assert!(
            parsed
                .jars
                .iter()
                .all(|jar| jar.file.chars().count() <= MAX_STRING + 1)
        );
        let _ = fs::remove_dir_all(dir);
    }
}
