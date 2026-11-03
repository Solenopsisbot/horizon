//! Mods in the game (`docs/mods.md`): content mods' blocks, items, data and
//! assets (`content_root`), and the Mods page's rows. Code mods are native
//! (`native_hooks.rs`); WebAssembly code mods don't run any more.
use super::{Game, execution_role::ExecutionRole};
use horizon_client::{
    mod_list::{self, Installed},
    text::Text,
};
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct ModsState {
    /// `--dev-mod PATH`: always on.
    pub(super) dev: Vec<PathBuf>,
    /// The WebAssembly mods found that no longer run, for the one-time notice.
    stopped: Vec<String>,
}

/// `root` with the content mods that are on layered over it
/// (docs/modpacks.md): their blocks, items, data and assets. Read once at
/// start, before the resources load; a mod whose content is refused is left
/// out here and its row on the Mods page says why.
pub(super) fn content_root(
    root: &std::path::Path,
    enabled: &std::collections::BTreeSet<String>,
    dev: &[std::path::PathBuf],
    native: &[horizon_client::mod_content::ContentMod],
) -> std::path::PathBuf {
    use horizon_client::mod_content::{self, ContentMod};
    use horizon_mods::{Capability, Content, Package};
    let here = std::env::current_dir().unwrap_or_default();
    let mut paths = horizon_mods::find(&here.join(mod_list::DIR));
    paths.retain(|p| !dev.contains(p));
    paths.extend(dev.iter().cloned());
    let mut mods: Vec<ContentMod> = native.to_vec();
    for path in &paths {
        let Ok(package) = Package::read(path) else {
            continue;
        };
        let m = &package.manifest;
        let on = enabled.contains(&m.id) || dev.contains(path);
        if !on
            || !m.capabilities.contains(&Capability::Registries)
            || mods.iter().any(|c| c.id == m.id)
        {
            continue;
        }
        let content = Content::read(path, m).and_then(|content| {
            // The first of two mods owning one namespace stays on.
            let own = (m.id.as_str(), content.namespaces.as_slice());
            match mods
                .iter()
                .find_map(|c| horizon_mods::content::clash((&c.id, &c.content.namespaces), own))
            {
                Some(e) => Err(e),
                None => Ok(content),
            }
        });
        match content {
            Ok(content) => mods.push(ContentMod {
                id: m.id.clone(),
                content,
            }),
            Err(e) => log::warn!("Horizon: mod {} content not loaded: {e}", m.id),
        }
    }
    match mod_content::resource_root(root, &mods, &here.join(".horizon-cache")) {
        Ok(out) => {
            if out != root {
                log::info!(
                    "Horizon: content from {} mod(s) in {}",
                    mods.len(),
                    out.display()
                );
            }
            register_mob_effects(&mods);
            out
        }
        Err(e) => {
            log::warn!("Horizon: content mods not loaded: {e}");
            register_mob_effects(&[]);
            root.to_owned()
        }
    }
}

/// Runs explicitly supplied Java entrypoints through the bounded native
/// interpreter before resource workers start. Java jars are never discovered
/// implicitly: a launcher or developer must pass `--java-mod PATH`.
fn opaque_registration_keys(entries: &[horizon_native_java::RegistryEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.declaration.is_none())
        .map(horizon_native_java::RegistryEntry::key)
        .collect()
}

/// Merge typed declarations from the jar metadata with registrations observed
/// during entrypoint execution.  A repeated id is safe only when both sources
/// describe the same registry and typed declaration; otherwise the loader must
/// stop before content reaches the immutable resource tables.
fn merge_native_entries(
    entries: &mut Vec<horizon_native_java::RegistryEntry>,
    extra: impl IntoIterator<Item = horizon_native_java::RegistryEntry>,
) -> Result<(), String> {
    for entry in extra {
        if let Some(index) = entries
            .iter()
            .position(|existing| existing.registry == entry.registry && existing.id == entry.id)
        {
            let existing = &entries[index];
            match (&existing.declaration, &entry.declaration) {
                // A metadata declaration is the explicit adapter for an
                // opaque callback registration. Prefer it while preserving
                // the all-or-nothing merge boundary.
                (None, Some(_)) => entries[index] = entry,
                // Keep an already-typed callback registration when metadata
                // contributes only the same opaque key.
                (Some(_), None) | (None, None) => {}
                (Some(existing), Some(incoming)) if existing == incoming => {}
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "conflicting native registration {} ({} vs {})",
                        entry.id,
                        existing.key(),
                        entry.key()
                    ));
                }
            }
            continue;
        }
        if entries.iter().any(|existing| existing.id == entry.id) {
            return Err(format!(
                "conflicting native registration {} ({} vs {})",
                entry.id,
                entries
                    .iter()
                    .find(|existing| existing.id == entry.id)
                    .expect("checked above")
                    .key(),
                entry.key()
            ));
        }
        entries.push(entry);
    }
    Ok(())
}

fn native_entrypoint_kinds(jar: &horizon_native_java::NativeJar) -> Vec<String> {
    let mut kinds: Vec<String> = jar.fabric_entrypoints.keys().cloned().collect();
    if !jar.neoforge_entrypoints.is_empty() {
        kinds.push("neoforge".to_owned());
    }
    kinds.sort();
    kinds.dedup();
    kinds
}

fn unsupported_fabric_adapter(entries: &[horizon_native_java::Entrypoint]) -> Option<(&str, &str)> {
    entries.iter().find_map(|entry| {
        entry
            .adapter
            .as_deref()
            .map(|adapter| (entry.class.as_str(), adapter))
    })
}

fn seeded_native_host(
    jar: &horizon_native_java::NativeJar,
    path: &std::path::Path,
    role: ExecutionRole,
) -> horizon_native_java::StdHost {
    let environment = match role {
        ExecutionRole::Client => "CLIENT",
        ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer => "SERVER",
    };
    let id = jar
        .native_id
        .as_deref()
        .or(jar.fabric_id.as_deref())
        .or(jar.neoforge_id.as_deref())
        .or_else(|| path.file_stem().and_then(|stem| stem.to_str()));
    let version = jar
        .fabric_version
        .as_deref()
        .or(jar.neoforge_version.as_deref())
        .unwrap_or("unknown");
    let mut host = horizon_native_java::StdHost::new().with_environment_type(environment);
    if let Some(id) = id {
        host = host.with_mod(id, version);
    }
    for dependency in &jar.required_mods {
        host = host.with_mod(dependency, "unknown");
    }
    // Seed resources discovered in the explicitly supplied jar as well as its
    // loader identity.  The native interpreter never scans the process
    // classpath, so this is the game-facing bridge for bundled assets.
    jar.seed_host(host)
}

pub(super) fn run_java_mods(
    paths: &[PathBuf],
    role: ExecutionRole,
) -> Result<Vec<horizon_client::mod_content::ContentMod>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let kinds: &[&str] = match role {
        ExecutionRole::Client => &["client", "main", "neoforge"],
        ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer => {
            &["server", "main", "neoforge"]
        }
    };
    let mut native_content = Vec::new();
    for path in paths {
        let jar = horizon_native_java::NativeJar::from_path(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if jar.client_side_only
            && matches!(
                role,
                ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer
            )
        {
            return Err(format!(
                "{}: clientSideOnly mod cannot run for server execution",
                path.display()
            ));
        }
        let (kind, entrypoints) = kinds
            .iter()
            .find_map(|kind| {
                let entries = jar.discover_entrypoints(kind);
                (!entries.is_empty()).then_some((*kind, entries))
            })
            .ok_or_else(|| {
                let available = native_entrypoint_kinds(&jar);
                let available = if available.is_empty() {
                    "none".to_owned()
                } else {
                    available.join(", ")
                };
                let execution = match role {
                    ExecutionRole::Client => "client",
                    ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer => "server",
                };
                format!(
                    "{}: no native Fabric or NeoForge entrypoint for {execution} execution (available: {available})",
                    path.display(),
                )
            })?;
        if let Some((class, adapter)) = unsupported_fabric_adapter(&entrypoints) {
            return Err(format!(
                "{} ({kind}, {class}): Fabric entrypoint adapter {adapter} is not supported by the native path",
                path.display()
            ));
        }
        let mut host = seeded_native_host(&jar, path, role);
        let mut calls = 0;
        for entry in &entrypoints {
            let report = jar
                .execute(&entry.class, entry.method.as_deref(), kind, &mut host)
                .map_err(|error| {
                    format!("{} ({kind}, {}): {error}", path.display(), entry.class)
                })?;
            calls += report.calls;
        }
        log::info!(
            "Horizon: native Java {} {} entrypoint(s) completed ({} host calls, {} registrations, {} listeners)",
            path.display(),
            entrypoints.len(),
            calls,
            host.registrations().len(),
            host.listener_registrations().count(),
        );
        let opaque_keys = opaque_registration_keys(host.registrations());
        if !opaque_keys.is_empty() {
            log::warn!(
                "Horizon: native Java {} recorded opaque registrations [{}]; typed content declarations are required before they enter live registries",
                path.display(),
                opaque_keys.join(", ")
            );
        }
        let mut entries = host
            .registrations()
            .iter()
            .filter(|entry| entry.declaration.is_some())
            .cloned()
            .collect::<Vec<_>>();
        merge_native_entries(&mut entries, jar.native_declarations.clone())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if !entries.is_empty() {
            let declarations =
                horizon_mods::native::NativeDeclarations::from_registry_entries(entries.iter())
                    .map_err(|error| format!("{}: {error}", path.display()))?;
            let id = jar
                .native_id
                .clone()
                .or_else(|| jar.fabric_id.clone())
                .or_else(|| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().into_owned())
                })
                .ok_or_else(|| format!("{}: native metadata has no mod id", path.display()))?;
            let content = horizon_mods::Content::from_native(id.clone(), declarations)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            native_content.push(horizon_client::mod_content::ContentMod { id, content });
        }
    }
    Ok(native_content)
}

/// The mods' mob effects join vanilla's (`brewing::effect`), once the root
/// holding their network ids is built. The startup root's effects, none
/// included, stay until restart: a pack reload after a Mods switch builds a
/// root with others.
pub(super) fn register_mob_effects(mods: &[horizon_client::mod_content::ContentMod]) {
    let effects = horizon_client::mod_content::mob_effects(mods);
    let ids: Vec<String> = effects.iter().map(|e| e.id.clone()).collect();
    let set = horizon_client::brewing::register_mod_effects(effect_rows(effects.into_iter()));
    if !set
        && horizon_client::brewing::mod_effects()
            .iter()
            .map(|e| e.id)
            .ne(ids.iter().map(String::as_str))
    {
        log::warn!("Horizon: the mods' mob effects changed; restart the game to use them");
    }
}

fn effect_rows<'a>(
    effects: impl IntoIterator<Item = &'a horizon_mods::content::MobEffect>,
) -> Vec<(String, u32, horizon_client::brewing::Category)> {
    effects
        .into_iter()
        .map(|effect| {
            let category = match effect.category {
                horizon_mods::content::EffectCategory::Beneficial => {
                    horizon_client::brewing::Category::Beneficial
                }
                horizon_mods::content::EffectCategory::Harmful => {
                    horizon_client::brewing::Category::Harmful
                }
                horizon_mods::content::EffectCategory::Neutral => {
                    horizon_client::brewing::Category::Neutral
                }
            };
            (effect.id.clone(), effect.color, category)
        })
        .collect()
}

/// What the launcher writes beside a game it built with native mods: their ids under `mods`.
const BUILD_INFO: &str = "native-mods.json";

/// A row for each native mod the launcher installed in `dir` (`<id>.hnmod`) that this game wasn't
/// built with (`built`: its [`BUILD_INFO`], empty for a release or Nightly) and that has no row
/// yet. The page said "No mods" before, and players copied the files into `horizon-mods` (#4770).
/// Its toggle is the `mod.<id>` line the launcher's next build follows.
fn not_built(dir: &std::path::Path, built: &str, rows: &[Installed]) -> Vec<Installed> {
    let info: serde_json::Value = serde_json::from_str(built).unwrap_or_default();
    let in_build = |id: &str| {
        info["mods"]
            .as_array()
            .is_some_and(|mods| mods.iter().any(|m| m["id"] == id))
    };
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.retain(|p| p.extension().is_some_and(|e| e == "hnmod"));
    paths.sort();
    let mut out: Vec<Installed> = Vec::new();
    for path in paths {
        let Ok(bundle) = horizon_mods::index::native::Bundle::read(&path) else {
            continue;
        };
        let m = bundle.manifest;
        let listed = rows.iter().chain(&out).any(|row| row.id == m.id);
        if !listed && !in_build(&m.id) {
            out.push(Installed {
                title: format!("{} {}", m.id, m.version),
                id: m.id,
                description: String::new(),
                problem: Some(mod_list::NOT_BUILT.to_owned()),
                dev: false,
                native: None,
            });
        }
    }
    out
}

/// A WebAssembly code mod's row on the Mods page.
const STOPPED: &str = "Doesn't run any more: needs a native port";
/// The player was told once about the WebAssembly mods that stopped (`horizon-mods/<this>`).
const NOTICE_SEEN: &str = ".native-notice";

impl Game<'_> {
    /// Lists the installed mods on the Mods page. Content mods load with the resources
    /// (`content_root`); native ones are built into the game (`native_hooks.rs`). WebAssembly code
    /// mods don't run any more: their rows say so, and the player hears it once in the chat
    /// (`tell_native_now`).
    pub(super) fn start_mods(&mut self) {
        let dir = Self::options_path().with_file_name(mod_list::DIR);
        let mut paths = horizon_mods::find(&dir);
        paths.retain(|p| !self.mods.dev.contains(p));
        paths.extend(self.mods.dev.iter().cloned());
        self.settings.mods.installed = paths
            .iter()
            .map(|path| {
                let file = path.file_name().unwrap_or_default().to_string_lossy();
                let package = match horizon_mods::Package::read(path) {
                    Ok(package) => package,
                    Err(e) => {
                        return Installed {
                            id: String::new(),
                            title: file.into_owned(),
                            description: String::new(),
                            problem: Some(format!("Can't be read: {e}")),
                            dev: false,
                            native: None,
                        };
                    }
                };
                let m = &package.manifest;
                let problem = package.wasm.is_some().then(|| STOPPED.to_owned());
                Installed {
                    id: m.id.clone(),
                    title: format!("{} {}", m.name, m.version),
                    description: m.description.clone(),
                    problem,
                    dev: self.mods.dev.contains(path),
                    native: None,
                }
            })
            .collect();
        // A mod with a native version built in says so instead, and isn't news.
        self.list_native_mods();
        let built = std::env::current_exe()
            .ok()
            .and_then(|exe| std::fs::read_to_string(exe.with_file_name(BUILD_INFO)).ok())
            .unwrap_or_default();
        let native_dir = Self::options_path().with_file_name(mod_list::NATIVE_DIR);
        let unbuilt = not_built(&native_dir, &built, &self.settings.mods.installed);
        self.settings.mods.installed.extend(unbuilt);
        let installed = &self.settings.mods.installed;
        self.mods.stopped = installed
            .iter()
            .filter(|row| row.problem.as_deref() == Some(STOPPED))
            .map(|row| row.title.clone())
            .collect();
    }
    /// Once, in the first world after the update: which WebAssembly mods stopped.
    pub(super) fn tell_native_now(&mut self) {
        if self.mods.stopped.is_empty() || self.player.is_none() {
            return;
        }
        let names = std::mem::take(&mut self.mods.stopped).join(", ");
        let seen = Self::options_path()
            .with_file_name(mod_list::DIR)
            .join(NOTICE_SEEN);
        if seen.exists() {
            return;
        }
        self.system_message(Text::literal(format!(
            "Horizon mods are native now: {names} stopped running until they have a native version. The launcher's Mods page has the native ones."
        )));
        let _ = std::fs::write(&seen, "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, io::Write, process::Command};

    #[test]
    fn native_effect_rows_preserve_typed_categories_for_server_registration() {
        let effects = [
            horizon_mods::content::MobEffect {
                id: "zeta:shock".into(),
                category: horizon_mods::content::EffectCategory::Harmful,
                color: 0x112233,
            },
            horizon_mods::content::MobEffect {
                id: "alpha:glow".into(),
                category: horizon_mods::content::EffectCategory::Beneficial,
                color: 0xAABBCC,
            },
        ];
        assert_eq!(
            effect_rows(effects.iter()),
            vec![
                (
                    "zeta:shock".into(),
                    0x112233,
                    horizon_client::brewing::Category::Harmful,
                ),
                (
                    "alpha:glow".into(),
                    0xAABBCC,
                    horizon_client::brewing::Category::Beneficial,
                ),
            ]
        );
    }

    #[test]
    fn native_entrypoint_diagnostics_list_declared_loader_kinds() {
        let mut jar = horizon_native_java::NativeJar::default();
        jar.fabric_entrypoints = BTreeMap::from([
            ("client".to_owned(), Vec::new()),
            ("main".to_owned(), Vec::new()),
        ]);
        jar.neoforge_entrypoints.push("demo/NeoMod".to_owned());
        assert_eq!(
            native_entrypoint_kinds(&jar),
            ["client", "main", "neoforge"]
        );
    }

    #[test]
    fn unsupported_fabric_adapters_are_rejected_before_execution() {
        let entries = vec![
            horizon_native_java::Entrypoint {
                class: "demo.First".into(),
                method: None,
                adapter: None,
            },
            horizon_native_java::Entrypoint {
                class: "demo.Second".into(),
                method: None,
                adapter: Some("net.fabricmc.api.ModInitializer".into()),
            },
        ];
        assert_eq!(
            unsupported_fabric_adapter(&entries),
            Some(("demo.Second", "net.fabricmc.api.ModInitializer"))
        );
    }

    #[test]
    fn native_startup_seeds_own_and_required_mod_ids() {
        let mut jar = horizon_native_java::NativeJar::default();
        jar.fabric_id = Some("demo".into());
        jar.fabric_version = Some("1.2.3".into());
        jar.required_mods = vec!["fabric-api".into(), "cloth-config".into()];
        let mut host = seeded_native_host(
            &jar,
            std::path::Path::new("demo.jar"),
            ExecutionRole::Client,
        );
        let loader = horizon_native_java::Value::Ref(Some("fabric-loader".into()));
        let loaded = horizon_native_java::MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isModLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        for id in ["demo", "fabric-api", "cloth-config"] {
            assert_eq!(
                horizon_native_java::Host::invoke_instance(
                    &mut host,
                    &loaded,
                    &loader,
                    &[horizon_native_java::Value::Ref(Some(id.into()))],
                )
                .unwrap(),
                horizon_native_java::Value::Int(1)
            );
        }
    }

    #[test]
    fn native_startup_seeds_neoforge_identity_and_dependencies() {
        let mut jar = horizon_native_java::NativeJar::default();
        jar.neoforge_id = Some("ruby".into());
        jar.neoforge_version = Some("1.2.3".into());
        jar.required_mods = vec!["geckolib".into()];
        let mut host = seeded_native_host(
            &jar,
            std::path::Path::new("ruby.jar"),
            ExecutionRole::DedicatedServer,
        );
        let loader = horizon_native_java::Value::Ref(Some("fabric-loader".into()));
        let loaded = horizon_native_java::MethodRef {
            owner: "net/fabricmc/loader/api/FabricLoader".into(),
            name: "isModLoaded".into(),
            descriptor: "(Ljava/lang/String;)Z".into(),
        };
        for id in ["ruby", "geckolib"] {
            assert_eq!(
                horizon_native_java::Host::invoke_instance(
                    &mut host,
                    &loaded,
                    &loader,
                    &[horizon_native_java::Value::Ref(Some(id.into()))],
                )
                .unwrap(),
                horizon_native_java::Value::Int(1)
            );
        }
    }

    #[test]
    fn java_mod_path_returns_typed_content_mods() {
        let root = std::env::temp_dir().join(format!(
            "horizon-game-java-content-fixture-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src/demo/TypedGameMod.java");
        let classes = root.join("classes");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&classes).unwrap();
        std::fs::write(
            &source,
            "package demo; public final class TypedGameMod { public void onInitialize() throws Exception { java.io.InputStream stream = ClassLoader.getSystemResourceAsStream(\"assets/typed-game/probe.txt\"); if (stream == null || stream.read() != 'n') throw new IllegalStateException(\"missing bundled resource bytes\"); } }\n",
        )
        .unwrap();
        let status = Command::new("javac")
            .arg("--release")
            .arg("17")
            .arg("-d")
            .arg(&classes)
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed");

        let jar_path = root.join("typed-game.jar");
        let file = std::fs::File::create(&jar_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            br#"{"id":"typed-game","version":"1.0","entrypoints":{"main":"demo.TypedGameMod"}}"#,
        )
        .unwrap();
        zip.start_file("demo/TypedGameMod.class", options).unwrap();
        zip.write_all(&std::fs::read(classes.join("demo/TypedGameMod.class")).unwrap())
            .unwrap();
        zip.start_file("assets/typed-game/probe.txt", options)
            .unwrap();
        zip.write_all(b"native-resource").unwrap();
        zip.start_file("META-INF/horizon-native.json", options)
            .unwrap();
        zip.write_all(
            br##"{"id":"typed-game","registrations":[{"registry":"minecraft:block","id":"typed:game_lamp","kind":"block","template":"minecraft:stone","item":true,"properties":{}},{"registry":"minecraft:recipe","id":"typed-game:game_lamp_recipe","kind":"recipe","recipe":{"type":"minecraft:crafting_shaped","pattern":["#"]}}]}"##,
        )
        .unwrap();
        zip.finish().unwrap();

        let content =
            run_java_mods(std::slice::from_ref(&jar_path), ExecutionRole::Client).unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0].id, "typed-game");
        assert_eq!(content[0].content.blocks[0].id, "typed:game_lamp");
        assert_eq!(
            content[0]
                .content
                .native_files
                .get("data/typed-game/recipe/game_lamp_recipe.json")
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|json| json.get("type").cloned()),
            Some(serde_json::Value::String(
                "minecraft:crafting_shaped".into()
            ))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn java_mod_role_selection_rejects_client_entrypoint_on_server() {
        let root = std::env::temp_dir().join(format!(
            "horizon-game-java-role-fixture-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src/demo/ClientOnlyMod.java");
        let classes = root.join("classes");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&classes).unwrap();
        std::fs::write(
            &source,
            "package demo; public final class ClientOnlyMod { public void onInitializeClient() {} }\n",
        )
        .unwrap();
        let status = Command::new("javac")
            .arg("--release")
            .arg("17")
            .arg("-d")
            .arg(&classes)
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed");

        let jar_path = root.join("client-only.jar");
        let file = std::fs::File::create(&jar_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(
            br#"{"id":"client-only","version":"1.0","entrypoints":{"client":"demo.ClientOnlyMod"}}"#,
        )
        .unwrap();
        zip.start_file("demo/ClientOnlyMod.class", options).unwrap();
        zip.write_all(&std::fs::read(classes.join("demo/ClientOnlyMod.class")).unwrap())
            .unwrap();
        zip.finish().unwrap();

        let client_content =
            run_java_mods(std::slice::from_ref(&jar_path), ExecutionRole::Client).unwrap();
        assert!(client_content.is_empty());
        let error = run_java_mods(
            std::slice::from_ref(&jar_path),
            ExecutionRole::DedicatedServer,
        )
        .unwrap_err();
        assert!(
            error.contains("no native Fabric or NeoForge entrypoint for server execution")
                && error.contains("client"),
            "{error}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn client_only_neoforge_mods_are_rejected_before_server_entrypoint_loading() {
        let root = std::env::temp_dir().join(format!(
            "horizon-game-java-client-only-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let jar_path = root.join("client-only-neoforge.jar");
        let file = std::fs::File::create(&jar_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "META-INF/neoforge.mods.toml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br#"[[mods]]
modId = "client_only"
version = "1.0"
clientSideOnly = true
"#,
        )
        .unwrap();
        zip.finish().unwrap();

        let error = run_java_mods(
            std::slice::from_ref(&jar_path),
            ExecutionRole::DedicatedServer,
        )
        .unwrap_err();
        assert!(
            error.contains("clientSideOnly mod cannot run for server execution"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A minimal `.hnmod` the registry accepts: its manifest and its mixin's.
    fn hnmod(dir: &std::path::Path, id: &str) {
        let horizon = "a".repeat(40);
        let manifest = serde_json::json!({
            "schema": 1, "id": id, "version": "0.3.0", "game": "26.3",
            "horizon": horizon, "mixins": "b".repeat(40),
        });
        let mixin = format!(
            "[mod]\nid = \"{id}\"\nversion = \"0.3.0\"\n[compatibility]\nhorizon_revision = \"{horizon}\"\n"
        );
        let file = std::fs::File::create(dir.join(format!("{id}.hnmod"))).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("horizon-native.json", options).unwrap();
        zip.write_all(manifest.to_string().as_bytes()).unwrap();
        zip.start_file(format!("mixins/{id}/horizon-mixin.toml"), options)
            .unwrap();
        zip.write_all(mixin.as_bytes()).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn native_mods_installed_but_not_built_get_a_row_saying_so() {
        let dir = std::env::temp_dir().join(format!("horizon-not-built-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["veinminer", "zoom", "minimap"] {
            hnmod(&dir, id);
        }
        std::fs::write(dir.join("notes.txt"), "not a mod").unwrap();
        std::fs::write(dir.join("broken.hnmod"), "not a zip").unwrap();
        // A release: none is built in; the page lists them, sorted, instead of "No mods".
        let rows = not_built(&dir, "", &[]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["minimap", "veinminer", "zoom"]);
        assert_eq!(rows[1].title, "veinminer 0.3.0");
        assert_eq!(rows[1].problem.as_deref(), Some(mod_list::NOT_BUILT));
        assert!(rows.iter().all(|r| r.native.is_none() && !r.dev));
        // A native build with zoom, and minimap's row already listed by its own hook.
        let built =
            r#"{"base":"nightly-2026-10-30-1da354b","mods":[{"id":"zoom","version":"0.3.0"}]}"#;
        let listed = Installed {
            id: "minimap".into(),
            ..rows[0].clone()
        };
        let rows = not_built(&dir, built, &[listed]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "veinminer");
        // No folder: no rows.
        assert!(not_built(&dir.join("missing"), "", &[]).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opaque_registration_diagnostics_name_registry_keys() {
        let entries = vec![
            horizon_native_java::RegistryEntry {
                registry: "minecraft:item".into(),
                id: "ruby:wand".into(),
                value: horizon_native_java::Value::Ref(Some("opaque/wand".into())),
                declaration: None,
            },
            horizon_native_java::RegistryEntry::typed(
                "minecraft:block",
                "ruby:lamp",
                horizon_native_java::RegistryDeclaration::Block {
                    template: "minecraft:stone".into(),
                    properties: std::collections::BTreeMap::new(),
                    item: false,
                },
            ),
        ];
        assert_eq!(
            opaque_registration_keys(&entries),
            ["minecraft:item/ruby:wand"]
        );
    }

    #[test]
    fn native_entry_merge_rejects_conflicting_typed_ids() {
        let block = horizon_native_java::RegistryEntry::typed(
            "minecraft:block",
            "ruby:shared",
            horizon_native_java::RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: std::collections::BTreeMap::new(),
                item: false,
            },
        );
        let item = horizon_native_java::RegistryEntry::typed(
            "minecraft:item",
            "ruby:shared",
            horizon_native_java::RegistryDeclaration::Item {
                template: "minecraft:stick".into(),
                properties: std::collections::BTreeMap::new(),
            },
        );
        let mut entries = vec![block];
        let error = merge_native_entries(&mut entries, [item]).unwrap_err();
        assert!(
            error.contains("conflicting native registration ruby:shared"),
            "{error}"
        );
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn native_entry_merge_promotes_opaque_registration_to_typed() {
        let opaque = horizon_native_java::RegistryEntry {
            registry: "minecraft:block".into(),
            id: "ruby:lamp".into(),
            value: horizon_native_java::Value::Ref(Some("opaque/lamp".into())),
            declaration: None,
        };
        let typed = horizon_native_java::RegistryEntry::typed(
            "minecraft:block",
            "ruby:lamp",
            horizon_native_java::RegistryDeclaration::Block {
                template: "minecraft:stone".into(),
                properties: std::collections::BTreeMap::new(),
                item: true,
            },
        );
        let mut entries = vec![opaque];
        merge_native_entries(&mut entries, [typed]).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].declaration.is_some());
        assert_eq!(
            entries[0].value,
            horizon_native_java::Value::Ref(Some("typed/minecraft:block/ruby:lamp".into()))
        );
    }

    #[test]
    fn explicit_java_mod_errors_name_the_jar() {
        let path = std::env::temp_dir().join(format!(
            "horizon-native-java-invalid-{}-{}.jar",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, b"not a jar").unwrap();
        let error = run_java_mods(std::slice::from_ref(&path), ExecutionRole::Client).unwrap_err();
        assert!(error.contains(&path.display().to_string()), "{error}");
        std::fs::remove_file(path).unwrap();
    }
}
