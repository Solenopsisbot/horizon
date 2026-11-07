//! Which entrypoints a jar runs for one execution role.
//!
//! A jar can carry metadata for more than one loader: "universal" builds ship a
//! `fabric.mod.json` beside a `META-INF/neoforge.mods.toml` and a `@Mod` class.
//! Each real loader reads only its own metadata. Fabric Loader never scans
//! classes for `@Mod`; it runs the `fabric.mod.json` entrypoints for the side it
//! is on (`main` everywhere, `client` or `server` for that side) and nothing
//! else. FML never reads `fabric.mod.json`.
//!
//! The native runtime treats a jar with Fabric metadata as a Fabric mod (the
//! same rule the game's compatibility report names the loader by), so it runs
//! only Fabric entrypoint kinds for it. Falling back to the NeoForge `@Mod`
//! class when the Fabric side declares nothing for this role would run code no
//! loader runs in that situation: zoomify declares only a `client` entrypoint,
//! and its `@Mod` constructor on a server reaches the client-only
//! `KeyMapping`. On a real Fabric server zoomify runs nothing, and so does the
//! native path.

use crate::{Entrypoint, NativeJar};

/// The entrypoint kinds Fabric Loader runs on a client, in the order the
/// native path tries them.
const FABRIC_CLIENT_KINDS: &[&str] = &["client", "main"];
/// The entrypoint kinds Fabric Loader runs on a dedicated server.
const FABRIC_SERVER_KINDS: &[&str] = &["server", "main"];
/// A NeoForge (or legacy Forge) jar's only entrypoint kind: its `@Mod` classes,
/// constructed on both sides.
const NEOFORGE_KINDS: &[&str] = &["neoforge"];

impl NativeJar {
    /// Whether the jar carries Fabric metadata, which makes it a Fabric mod
    /// for native execution whatever other loader's metadata it also ships.
    pub fn has_fabric_metadata(&self) -> bool {
        self.fabric_id.is_some()
            || !self.fabric_entrypoints.is_empty()
            || self.resource_names().any(|name| name == "fabric.mod.json")
    }

    /// The entrypoint kinds this jar runs for a role, in priority order.
    ///
    /// A Fabric jar gets its role's Fabric kinds only; any other jar gets
    /// `neoforge`, its `@Mod` classes.
    pub fn role_entrypoint_kinds(&self, client: bool) -> &'static [&'static str] {
        match (self.has_fabric_metadata(), client) {
            (true, true) => FABRIC_CLIENT_KINDS,
            (true, false) => FABRIC_SERVER_KINDS,
            (false, _) => NEOFORGE_KINDS,
        }
    }

    /// The first of [`Self::role_entrypoint_kinds`] that declares entrypoints,
    /// with every entrypoint of that kind, or `None` when the jar runs nothing
    /// in this role.
    pub fn role_entrypoints(&self, client: bool) -> Option<(&'static str, Vec<Entrypoint>)> {
        self.role_entrypoint_kinds(client).iter().find_map(|kind| {
            let entries = self.discover_entrypoints(kind);
            (!entries.is_empty()).then_some((*kind, entries))
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::NativeJar;
    use std::io::Write;

    /// A jar of the given resources and class files, read the way the game
    /// reads a mod (`NativeJar::from_path`), so `@Mod` discovery is the real one.
    fn jar(resources: &[(&str, &[u8])]) -> NativeJar {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "horizon-entrypoint-role-{}-{}.jar",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, data) in resources {
            zip.start_file(*name, options).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        let jar = NativeJar::from_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        jar
    }

    /// A class file `demo/NeoMod` whose constant pool names NeoForge's `@Mod`
    /// annotation, which is what discovery keys a `@Mod` class on.
    fn neoforge_mod_class() -> Vec<u8> {
        fn utf8(out: &mut Vec<u8>, value: &str) {
            out.push(1);
            out.extend_from_slice(&(value.len() as u16).to_be_bytes());
            out.extend_from_slice(value.as_bytes());
        }
        // magic, version 52, five pool slots (1..=5).
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52, 0, 6];
        utf8(&mut out, "demo/NeoMod");
        out.extend_from_slice(&[7, 0, 1]);
        utf8(&mut out, "java/lang/Object");
        out.extend_from_slice(&[7, 0, 3]);
        utf8(&mut out, "Lnet/neoforged/fml/common/Mod;");
        // public class, this #2, super #4, no interfaces, fields, methods or
        // attributes.
        out.extend_from_slice(&[0, 0x21, 0, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        out
    }

    const UNIVERSAL_FABRIC_JSON: &[u8] = br#"{"schemaVersion":1,"id":"demo","version":"1",
        "entrypoints":{"client":["demo.ClientInit"]}}"#;
    const NEOFORGE_TOML: &[u8] = br#"modLoader = "javafml"
loaderVersion = "[1,)"
license = "MIT"
[[mods]]
modId = "demo"
version = "1"
"#;

    #[test]
    fn a_universal_jar_runs_only_its_fabric_entrypoints() {
        // zoomify's shape: Fabric declares a client entrypoint only, the
        // NeoForge half has an `@Mod` class. Fabric Loader on a server runs
        // nothing; on a client it runs `client`. The `@Mod` class is never a
        // Fabric entrypoint.
        let class = neoforge_mod_class();
        let jar = jar(&[
            ("fabric.mod.json", UNIVERSAL_FABRIC_JSON),
            ("META-INF/neoforge.mods.toml", NEOFORGE_TOML),
            ("demo/NeoMod.class", &class),
        ]);
        assert!(
            !jar.discover_entrypoints("neoforge").is_empty(),
            "the fixture's @Mod class must be discoverable for this test to mean anything"
        );
        assert_eq!(jar.role_entrypoints(false), None);
        let (kind, entries) = jar.role_entrypoints(true).unwrap();
        assert_eq!(kind, "client");
        assert_eq!(entries[0].class, "demo.ClientInit");
    }

    #[test]
    fn a_neoforge_jar_runs_its_mod_class_on_both_sides() {
        let class = neoforge_mod_class();
        let jar = jar(&[
            ("META-INF/neoforge.mods.toml", NEOFORGE_TOML),
            ("demo/NeoMod.class", &class),
        ]);
        for client in [false, true] {
            let (kind, entries) = jar.role_entrypoints(client).unwrap();
            assert_eq!(kind, "neoforge");
            assert_eq!(entries[0].class, "demo.NeoMod");
        }
    }

    #[test]
    fn a_fabric_server_prefers_server_then_main() {
        let jar = jar(&[(
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"demo","version":"1",
                "entrypoints":{"main":["demo.Main"],"server":["demo.Server"],"client":["demo.Client"]}}"#,
        )]);
        assert_eq!(jar.role_entrypoints(false).unwrap().0, "server");
        assert_eq!(jar.role_entrypoints(true).unwrap().0, "client");
        let main_only = self::jar(&[(
            "fabric.mod.json",
            br#"{"schemaVersion":1,"id":"demo","version":"1","entrypoints":{"main":["demo.Main"]}}"#,
        )]);
        assert_eq!(main_only.role_entrypoints(false).unwrap().0, "main");
        assert_eq!(main_only.role_entrypoints(true).unwrap().0, "main");
    }
}
