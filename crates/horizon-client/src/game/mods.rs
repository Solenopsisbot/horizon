//! Mods in the game (`docs/mods.md`): content mods' blocks, items, data and
//! assets (`content_root`), and the Mods page's rows. Code mods are native
//! (`native_hooks.rs`); WebAssembly code mods don't run any more. The opt-in
//! real NeoForge loader preflight is `fml.rs`.
use super::{Game, execution_role::ExecutionRole};
use horizon_client::{
    mod_content::{self, ContentMod, DeclarationSummary},
    mod_list::{self, Installed},
    text::Text,
};
use std::path::PathBuf;

// The opt-in real-loader preflight lives in `game/fml.rs`; it is registered
// here because it reports beside this module's Java compatibility lines.
#[path = "fml.rs"]
pub(super) mod fml;

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
    native: &[ContentMod],
) -> std::path::PathBuf {
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

/// Most rows one jar's compatibility report keeps from its unbounded native
/// records. Sorting first makes the truncation deterministic, and the startup
/// log stays readable whatever a jar registers.
const MAX_REPORT_ROWS: usize = 12;

/// How much of a panic's message an isolation report keeps. A malformed mod
/// must not be able to flood the startup log with its own panic text.
const MAX_ISOLATION_MESSAGE: usize = 160;

/// The loader metadata a jar declares. All three run through the same bounded
/// interpreter; the kind only names which metadata the native path read, so a
/// server operator can tell a Fabric jar from a NeoForge one in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JavaLoader {
    Fabric,
    NeoForge,
    /// `META-INF/mods.toml` without `META-INF/neoforge.mods.toml`.
    LegacyForge,
}

impl JavaLoader {
    fn name(self) -> &'static str {
        match self {
            Self::Fabric => "fabric",
            Self::NeoForge => "neoforge",
            Self::LegacyForge => "legacy-forge",
        }
    }

    /// Which metadata wins. Fabric comes first because its entrypoint kinds are
    /// searched first, and a jar converted from Forge often keeps its old
    /// `mods.toml` beside `fabric.mod.json`.
    fn of(jar: &horizon_native_java::NativeJar) -> Option<Self> {
        let declares = |name: &str| jar.resource_names().any(|resource| resource == name);
        if jar.fabric_id.is_some()
            || !jar.fabric_entrypoints.is_empty()
            || declares("fabric.mod.json")
        {
            Some(Self::Fabric)
        } else if declares("META-INF/neoforge.mods.toml") || !jar.neoforge_entrypoints.is_empty() {
            Some(Self::NeoForge)
        } else if declares("META-INF/mods.toml") {
            Some(Self::LegacyForge)
        } else if jar.neoforge_id.is_some() {
            // A jar built from classes alone keeps no metadata file to name.
            Some(Self::NeoForge)
        } else {
            None
        }
    }
}

/// The role names the compatibility diagnostics use. `ExecutionRole` is
/// internal to the bin and has no display of its own.
fn role_name(role: ExecutionRole) -> &'static str {
    match role {
        ExecutionRole::Client => "client",
        ExecutionRole::IntegratedServer => "integrated-server",
        ExecutionRole::DedicatedServer => "dedicated-server",
    }
}

/// The Mixin translation planning one jar declares, as a report keeps it: the
/// plans the bounded runtime would weave, the methods they hook (sorted and
/// bounded), the selectors it could not read, and the configs it read. Scanned
/// once per jar, so the window, its server thread and the dedicated server
/// report the same numbers.
#[derive(Debug, Default, Clone)]
pub(super) struct MixinPlanning {
    pub(super) plans: usize,
    pub(super) targets: Vec<String>,
    pub(super) unresolved: usize,
    pub(super) configs: usize,
    pub(super) config_errors: usize,
}

impl MixinPlanning {
    /// Planning is a property of the jar, not of a run: a jar that never
    /// executed entrypoints (a refused one) still plans the same way, and a jar
    /// that could not be read (`None`) plans nothing.
    pub(super) fn of(jar: Option<&horizon_native_java::NativeJar>) -> Self {
        let Some(jar) = jar else {
            return Self::default();
        };
        let plans = jar.mixin_injection_plans();
        Self {
            plans: plans.len(),
            targets: bounded_sorted(plans.iter().map(|plan| mixin_target_label(&plan.target))),
            unresolved: jar.unresolved_invoke_plans().len(),
            configs: jar.mixin_configs().len(),
            config_errors: jar.mixin_config_errors().len(),
        }
    }
}

/// One jar's bounded compatibility decision: what the native path read, what it
/// ran, and what actually reached the typed content tables. Lists are sorted and
/// truncated at [`MAX_REPORT_ROWS`], so the window, its server thread and the
/// dedicated server log the same lines for the same jar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct JavaCompatibilityReport {
    pub(super) jar: PathBuf,
    /// The mod id the native host seeded: metadata first, then the file name.
    pub(super) id: Option<String>,
    pub(super) version: Option<String>,
    pub(super) loader: Option<JavaLoader>,
    /// Every entrypoint kind the jar declares, sorted.
    pub(super) declared_entrypoints: Vec<String>,
    /// The kind this role selected; empty when the jar never got that far.
    pub(super) entrypoint_kind: String,
    pub(super) executed_entrypoints: usize,
    pub(super) host_calls: usize,
    /// Typed rows that reached `Content::from_native`.
    pub(super) typed_registrations: usize,
    /// Registrations left opaque: kept out of live registries until a typed
    /// declaration arrives. A promoted row is not repeated here.
    pub(super) opaque_registrations: Vec<String>,
    /// Opaque rows a typed declaration of the same registry and id replaced.
    pub(super) promoted: Vec<String>,
    pub(super) payload_directions: Vec<String>,
    pub(super) listeners: usize,
    /// `@Inject` plans the bounded runtime would weave, counted as translation
    /// planning rather than weaving: nothing is patched into a class.
    pub(super) mixin_plans: usize,
    /// The methods those plans hook, `owner#name(descriptor)`, sorted and
    /// bounded like every other list here.
    pub(super) mixin_targets: Vec<String>,
    /// `@At("INVOKE")` plans whose call site the bounded runtime cannot read,
    /// so they stay on the JVM fallback instead of a guessed target.
    pub(super) mixin_unresolved: usize,
    /// Mixin JSON configs the jar declares and this runtime could read.
    pub(super) mixin_configs: usize,
    /// Declared configs that could not be read, counted rather than named.
    pub(super) mixin_config_errors: usize,
    /// True only for the diagnostics a server role logs when it refuses a jar
    /// marked `clientSideOnly`; an accepted jar is never rejected.
    pub(super) client_side_only_rejected: bool,
    /// The bounded message of a panic isolated while this jar's entrypoints
    /// ran. `None` for a jar that ran or was refused.
    pub(super) isolation_failure: Option<String>,
    pub(super) role: ExecutionRole,
}

/// What one entrypoint run recorded. Kept apart from the jar (and from the
/// native host) so the report builder is testable without compiling Java.
#[derive(Default)]
struct NativeRunFacts<'a> {
    kind: &'a str,
    executed: usize,
    host_calls: usize,
    listeners: usize,
    recorded: &'a [horizon_native_java::RegistryEntry],
    payloads: &'a [horizon_native_java::PayloadRegistration],
    typed: usize,
    promoted: Vec<String>,
    rejected: bool,
}

impl JavaCompatibilityReport {
    fn new(
        jar: &horizon_native_java::NativeJar,
        path: &std::path::Path,
        role: ExecutionRole,
        facts: NativeRunFacts<'_>,
    ) -> Self {
        let mut promoted = facts.promoted;
        promoted.sort();
        promoted.dedup();
        // A promoted row is no longer opaque: the typed declaration replaced it.
        let opaque = bounded_sorted(
            facts
                .recorded
                .iter()
                .filter(|entry| entry.declaration.is_none())
                .map(horizon_native_java::RegistryEntry::key)
                .filter(|key| !promoted.contains(key)),
        );
        promoted.truncate(MAX_REPORT_ROWS);
        // Mixin planning comes from the jar's annotations and configs, so a
        // refused jar reports what it would have woven just like a run one.
        let mixin = MixinPlanning::of(Some(jar));
        Self {
            jar: path.to_owned(),
            id: native_mod_id(jar, path).map(str::to_owned),
            version: native_mod_version(jar).map(str::to_owned),
            loader: JavaLoader::of(jar),
            declared_entrypoints: native_entrypoint_kinds(jar),
            entrypoint_kind: facts.kind.to_owned(),
            executed_entrypoints: facts.executed,
            host_calls: facts.host_calls,
            typed_registrations: facts.typed,
            opaque_registrations: opaque,
            promoted,
            payload_directions: bounded_sorted(
                facts
                    .payloads
                    .iter()
                    .map(|payload| payload.direction.clone()),
            ),
            listeners: facts.listeners,
            mixin_plans: mixin.plans,
            mixin_targets: mixin.targets,
            mixin_unresolved: mixin.unresolved,
            mixin_configs: mixin.configs,
            mixin_config_errors: mixin.config_errors,
            client_side_only_rejected: facts.rejected,
            isolation_failure: None,
            role,
        }
    }

    /// The startup log's lines for this jar: a fixed order over fixed fields,
    /// with every list bounded and sorted.
    pub(super) fn summary(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "Horizon: Java compatibility {}: role={} loader={} id={} version={}",
            self.jar.display(),
            role_name(self.role),
            self.loader.map_or("unknown", JavaLoader::name),
            self.id.as_deref().unwrap_or("unknown"),
            self.version.as_deref().unwrap_or("unknown"),
        )];
        lines.push(format!(
            "  entrypoints: declared=[{}] selected={} executed={} host-calls={} listeners={}",
            self.declared_entrypoints.join(", "),
            if self.entrypoint_kind.is_empty() {
                "none"
            } else {
                &self.entrypoint_kind
            },
            self.executed_entrypoints,
            self.host_calls,
            self.listeners,
        ));
        lines.push(format!(
            "  registrations: typed={} opaque=[{}] promoted=[{}]",
            self.typed_registrations,
            self.opaque_registrations.join(", "),
            self.promoted.join(", "),
        ));
        lines.push(format!(
            "  payloads: [{}]",
            self.payload_directions.join(", ")
        ));
        // Mixin planning, always printed so a jar with none reads as planned
        // nothing rather than missing the line; the targets stay bounded.
        lines.push(format!(
            "  mixin: {} plan(s), {} target(s), {} unresolved, {} config, {} config error(s){}",
            self.mixin_plans,
            self.mixin_targets.len(),
            self.mixin_unresolved,
            self.mixin_configs,
            self.mixin_config_errors,
            if self.mixin_targets.is_empty() {
                String::new()
            } else {
                format!(" targets=[{}]", self.mixin_targets.join(", "))
            },
        ));
        if self.client_side_only_rejected {
            lines.push(format!(
                "  rejected: clientSideOnly metadata for {} execution",
                role_name(self.role)
            ));
        }
        if let Some(failure) = &self.isolation_failure {
            lines.push(format!("  isolated: {failure}"));
        }
        lines
    }

    /// The one warning a jar's Mixin planning earns when part of it could not be
    /// translated. The counts only: the bounded lists are in [`Self::summary`].
    pub(super) fn mixin_warning(&self) -> Option<String> {
        if self.mixin_unresolved == 0 && self.mixin_config_errors == 0 {
            return None;
        }
        Some(format!(
            "Horizon: native Java {} has {} unresolved Mixin plan(s) and {} unreadable Mixin config(s); those stay on the JVM fallback",
            self.jar.display(),
            self.mixin_unresolved,
            self.mixin_config_errors,
        ))
    }
}

/// A woven method labeled the way the native probe labels it:
/// `owner#name(descriptor)`.
fn mixin_target_label(method: &horizon_native_java::MethodRef) -> String {
    format!("{}#{}{}", method.owner, method.name, method.descriptor)
}

/// Sorted, deduplicated and truncated at [`MAX_REPORT_ROWS`].
fn bounded_sorted(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut values: Vec<String> = values.into_iter().collect();
    values.sort();
    values.dedup();
    values.truncate(MAX_REPORT_ROWS);
    values
}

/// The mod id the native host seeds: its own metadata name, else the file's.
fn native_mod_id<'a>(
    jar: &'a horizon_native_java::NativeJar,
    path: &'a std::path::Path,
) -> Option<&'a str> {
    jar.native_id
        .as_deref()
        .or(jar.fabric_id.as_deref())
        .or(jar.neoforge_id.as_deref())
        .or_else(|| path.file_stem().and_then(|stem| stem.to_str()))
}

fn native_mod_version(jar: &horizon_native_java::NativeJar) -> Option<&str> {
    jar.fabric_version
        .as_deref()
        .or(jar.neoforge_version.as_deref())
}

fn opaque_registration_keys(entries: &[horizon_native_java::RegistryEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| entry.declaration.is_none())
        .map(horizon_native_java::RegistryEntry::key)
        .collect()
}

/// Opaque registrations a typed declaration of the same registry and id
/// supersedes. The typed row is kept and the collision is reported once; the
/// merge itself is unchanged.
fn promoted_registrations(
    recorded: &[horizon_native_java::RegistryEntry],
    declared: &[horizon_native_java::RegistryEntry],
) -> Vec<String> {
    bounded_sorted(
        recorded
            .iter()
            .filter(|entry| {
                entry.declaration.is_none()
                    && declared.iter().any(|declared| {
                        declared.declaration.is_some()
                            && declared.registry == entry.registry
                            && declared.id == entry.id
                    })
            })
            .map(horizon_native_java::RegistryEntry::key),
    )
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
    let id = native_mod_id(jar, path);
    let version = native_mod_version(jar).unwrap_or("unknown");
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

/// The kinds a [`DeclarationSummary`] counts, in the order its lines print
/// them, with the name each count is logged under: the aggregate line and the
/// content lines read the same table, so they never disagree.
fn declared_counts(summary: &DeclarationSummary) -> [(usize, &'static str); 10] {
    [
        (summary.blocks, "block"),
        (summary.items, "item"),
        (summary.effects, "mob effect"),
        (summary.entities, "entity"),
        (summary.data_components, "data component"),
        (summary.attachments, "attachment"),
        (summary.menus, "menu"),
        (summary.block_entities, "block entity"),
        (summary.recipes, "recipe"),
        (summary.ingredients, "ingredient"),
    ]
}

/// The run's typed declarations as log lines: each of the summary's own lines,
/// prefixed so the game log groups them. Nothing declared logs nothing.
fn declaration_lines(summary: &DeclarationSummary) -> Vec<String> {
    summary
        .lines()
        .into_iter()
        .map(|line| format!("Horizon: Java content: {line}"))
        .collect()
}

/// The aggregate line for a run: the reports' totals, then what the typed
/// tables declared. The declared part is bounded to three fields — the blocks
/// and items by name, every other kind in one count — and is left off
/// entirely when nothing was declared, so a run of code-only jars logs exactly
/// the line it always did.
fn aggregate_line(reports: &[JavaCompatibilityReport], summary: &DeclarationSummary) -> String {
    let typed = reports
        .iter()
        .map(|report| report.typed_registrations)
        .sum::<usize>();
    let opaque = reports
        .iter()
        .map(|report| report.opaque_registrations.len())
        .sum::<usize>();
    let payloads = reports
        .iter()
        .map(|report| report.payload_directions.len())
        .sum::<usize>();
    let mixin_plans = reports
        .iter()
        .map(|report| report.mixin_plans)
        .sum::<usize>();
    let mixin_unresolved = reports
        .iter()
        .map(|report| report.mixin_unresolved)
        .sum::<usize>();
    let counts = declared_counts(summary);
    let mut declared: Vec<String> = counts
        .iter()
        .filter(|(count, kind)| *count > 0 && matches!(*kind, "block" | "item"))
        .map(|(count, kind)| format!("{count} declared {kind}(s)"))
        .collect();
    let other = counts
        .iter()
        .filter(|(count, kind)| *count > 0 && !matches!(*kind, "block" | "item"))
        .map(|(count, _)| count)
        .sum::<usize>();
    if other > 0 {
        declared.push(format!("{other} other declared row(s)"));
    }
    let declared = if declared.is_empty() {
        String::new()
    } else {
        format!(", {}", declared.join(", "))
    };
    format!(
        "Horizon: Java compatibility: {} jar(s), {typed} typed registrations, {opaque} opaque, {payloads} payload directions, {mixin_plans} mixin plan(s), {mixin_unresolved} unresolved{declared}",
        reports.len(),
    )
}

/// The startup line naming the jars whose native execution was isolated: one
/// bounded list, sorted so the window, its server thread and the dedicated
/// server log the same line. `None` when nothing was isolated.
fn isolation_summary(reports: &[JavaCompatibilityReport]) -> Option<String> {
    let mut jars: Vec<String> = reports
        .iter()
        .filter(|report| report.isolation_failure.is_some())
        .map(|report| report.jar.display().to_string())
        .collect();
    if jars.is_empty() {
        return None;
    }
    jars.sort();
    let (total, extra) = (jars.len(), jars.len().saturating_sub(MAX_REPORT_ROWS));
    jars.truncate(MAX_REPORT_ROWS);
    let extra = if extra > 0 {
        format!(" (+{extra} more)")
    } else {
        String::new()
    };
    Some(format!(
        "Horizon: Java compatibility: java isolation: {total} jar(s) failed: [{}]{extra}",
        jars.join(", ")
    ))
}

/// `HORIZON_JAVA_STRICT=1` asks for the first isolated jar to stop startup once
/// every jar has been attempted; the default logs and keeps going.
fn strict_isolation_enabled() -> bool {
    std::env::var("HORIZON_JAVA_STRICT").is_ok_and(|value| value == "1")
}

/// The startup error a strict run returns after every jar was attempted: the
/// first isolated failure in input order, with its jar and bounded message.
fn isolation_startup_error(reports: &[JavaCompatibilityReport], strict: bool) -> Option<String> {
    if !strict {
        return None;
    }
    let report = reports
        .iter()
        .find(|report| report.isolation_failure.is_some())?;
    Some(format!(
        "{}: isolated native Java panic: {}",
        report.jar.display(),
        report
            .isolation_failure
            .as_deref()
            .unwrap_or("non-string panic payload"),
    ))
}

/// Runs one jar's native execution, turning a panic into its bounded message
/// instead of unwinding the game or the server thread.
fn isolate_jar<T>(run: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .map_err(|payload| panic_payload_message(payload.as_ref()))
}

/// The message a caught panic carries: `&str` and `String` payloads are read,
/// anything else keeps a fixed text rather than a debug dump.
fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

/// One line, flattened and at most [`MAX_ISOLATION_MESSAGE`] characters: a
/// mod's panic message must not be able to flood the startup log.
fn bounded_isolation_message(message: &str) -> String {
    let mut bounded = String::new();
    let mut truncated = false;
    for character in message.chars() {
        if bounded.chars().count() == MAX_ISOLATION_MESSAGE {
            truncated = true;
            break;
        }
        bounded.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    let bounded = bounded.trim();
    if bounded.is_empty() {
        return "empty panic message".to_owned();
    }
    if truncated {
        format!("{bounded}...")
    } else {
        bounded.to_owned()
    }
}

/// The opt-in real-loader preflight (`fml.rs`), logged once under the Java
/// compatibility prefix and run before the native path below. It is skipped
/// unless `HORIZON_NEOFORGE_SERVER` and `HORIZON_JVM_BIN` are set, and its
/// failure only stops startup under `HORIZON_NEOFORGE_STRICT=1`.
// Wired by the integrated server's startup (game/integrated.rs, top of
// `run_server_java_mods`): `super::mods::fml_preflight(role, &options.java_mods)?;`
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn fml_preflight(
    role: ExecutionRole,
    jars: &[PathBuf],
) -> Result<Option<fml::FmlReport>, String> {
    let Some(outcome) = fml::preflight(role, jars) else {
        return Ok(None);
    };
    let level = if outcome.report.succeeded() {
        log::Level::Info
    } else {
        log::Level::Warn
    };
    for line in outcome.report.summary() {
        log::log!(level, "Horizon: Java compatibility: {line}");
    }
    match outcome.strict_error() {
        Some(error) => Err(error),
        None => Ok(Some(outcome.report)),
    }
}

/// Runs explicitly supplied Java entrypoints through the bounded native
/// interpreter before resource workers start. Java jars are never discovered
/// implicitly: a launcher or developer must pass `--java-mod PATH`.
pub(super) fn run_java_mods(
    paths: &[PathBuf],
    role: ExecutionRole,
) -> Result<Vec<ContentMod>, String> {
    run_java_mods_with_report(paths, role).map(|(content, _)| content)
}

/// [`run_java_mods`], keeping each jar's compatibility report so the window, its
/// server thread and the dedicated server can log the same decisions from their
/// own startup path.
pub(super) fn run_java_mods_with_report(
    paths: &[PathBuf],
    role: ExecutionRole,
) -> Result<(Vec<ContentMod>, Vec<JavaCompatibilityReport>), String> {
    let mut reports = Vec::new();
    if paths.is_empty() {
        return Ok((Vec::new(), reports));
    }
    let kinds: &[&str] = match role {
        ExecutionRole::Client => &["client", "main", "neoforge"],
        ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer => {
            &["server", "main", "neoforge"]
        }
    };
    let mut native_content = Vec::new();
    let mut payload_declarations = Vec::new();
    horizon_client::lan::payloads::clear_handler_jars();
    for path in paths {
        let jar = horizon_native_java::NativeJar::from_path(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if jar.client_side_only
            && matches!(
                role,
                ExecutionRole::IntegratedServer | ExecutionRole::DedicatedServer
            )
        {
            // The refusal is a compatibility decision too: log it in the same
            // shape as an accepted jar, then keep today's error.
            let report = JavaCompatibilityReport::new(
                &jar,
                path,
                role,
                NativeRunFacts {
                    rejected: true,
                    ..NativeRunFacts::default()
                },
            );
            for line in report.summary() {
                log::warn!("{line}");
            }
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
        // The interpreter and the host callbacks it reaches are mod code: a
        // panic in them fails this jar, not the game or the server thread.
        let execution = isolate_jar(|| {
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
            Ok::<(usize, horizon_native_java::StdHost), String>((calls, host))
        });
        let (calls, mut host) = match execution {
            Ok(Ok(run)) => run,
            // A refused entrypoint stays the startup error it always was.
            Ok(Err(error)) => return Err(error),
            Err(message) => {
                let mut report = JavaCompatibilityReport::new(
                    &jar,
                    path,
                    role,
                    NativeRunFacts {
                        kind,
                        ..NativeRunFacts::default()
                    },
                );
                report.isolation_failure = Some(bounded_isolation_message(&message));
                for line in report.summary() {
                    log::warn!("{line}");
                }
                log::warn!(
                    "Horizon: native Java {} was isolated after a panic; later jars still run",
                    path.display()
                );
                reports.push(report);
                continue;
            }
        };
        for (id, direction, codec) in host.payload_channels() {
            match horizon_mods::payloads::PayloadDirection::parse(&direction) {
                Ok(direction) => {
                    payload_declarations.push(horizon_mods::payloads::PayloadDeclaration {
                        id,
                        direction,
                        codec,
                    })
                }
                Err(error) => log::warn!(
                    "Horizon: native Java {} payload channel {id} was skipped: {error}",
                    path.display()
                ),
            }
        }
        // Publish this host's channels first, then any frames its bootstrap
        // queued; a bad frame is a warning, not a startup failure.
        if let Err(error) =
            horizon_client::lan::payloads::store_declared(payload_declarations.clone())
        {
            log::warn!("Horizon: Java payload channels were not registered: {error}");
        }
        if let Err(error) = horizon_client::lan::payloads::publish_from_host(&mut host) {
            log::warn!(
                "Horizon: native Java {} payload send was skipped: {error}",
                path.display()
            );
        }
        horizon_client::lan::payloads::store_handler_jar(
            jar.clone(),
            host.payload_registrations().to_vec(),
        );
        let listeners = host.listener_registrations().count();
        log::info!(
            "Horizon: native Java {} {} entrypoint(s) completed ({} host calls, {} registrations, {} listeners)",
            path.display(),
            entrypoints.len(),
            calls,
            host.registrations().len(),
            listeners,
        );
        let opaque_keys = opaque_registration_keys(host.registrations());
        if !opaque_keys.is_empty() {
            log::warn!(
                "Horizon: native Java {} recorded opaque registrations [{}]; typed content declarations are required before they enter live registries",
                path.display(),
                opaque_keys.join(", ")
            );
        }
        let promoted = promoted_registrations(host.registrations(), &jar.native_declarations);
        if !promoted.is_empty() {
            log::warn!(
                "Horizon: native Java {} declares typed and opaque rows for [{}]; the typed row is kept",
                path.display(),
                promoted.join(", ")
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
        let report = JavaCompatibilityReport::new(
            &jar,
            path,
            role,
            NativeRunFacts {
                kind,
                executed: entrypoints.len(),
                host_calls: calls,
                listeners,
                recorded: host.registrations(),
                payloads: host.payload_registrations(),
                typed: entries.len(),
                promoted,
                rejected: false,
            },
        );
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
            native_content.push(ContentMod { id, content });
        }
        if let Some(warning) = report.mixin_warning() {
            log::warn!("{warning}");
        }
        for line in report.summary() {
            log::info!("{line}");
        }
        reports.push(report);
    }
    let summary = mod_content::declaration_summary(&native_content);
    // No warning for a jar whose typed rows leave the summary empty: every
    // typed declaration that reaches `Content::from_native` lands in a table
    // the summary counts (a custom row stops the load with an error instead),
    // and rows the resource planner refuses later are not visible here.
    for line in declaration_lines(&summary) {
        log::info!("{line}");
    }
    log::info!("{}", aggregate_line(&reports, &summary));
    if let Some(line) = isolation_summary(&reports) {
        log::warn!("{line}");
    }
    if let Err(error) = horizon_client::lan::payloads::store_declared(payload_declarations) {
        log::warn!("Horizon: Java payload channels were not registered: {error}");
    }
    // Every jar has been attempted: the strict opt-in stops here, naming the
    // first isolated failure instead of carrying on with the rest.
    if let Some(error) = isolation_startup_error(&reports, strict_isolation_enabled()) {
        return Err(error);
    }
    Ok((native_content, reports))
}

/// The mods' mob effects join vanilla's (`brewing::effect`), once the root
/// holding their network ids is built. The startup root's effects, none
/// included, stay until restart: a pack reload after a Mods switch builds a
/// root with others.
pub(super) fn register_mob_effects(mods: &[ContentMod]) {
    let effects = horizon_client::mod_content::mob_effects(mods);
    let ids: Vec<String> = effects.iter().map(|e| e.id.clone()).collect();
    let set = horizon_client::brewing::register_mod_effects(effect_rows(effects));
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
    fn fml_preflight_never_runs_for_a_client() {
        // A client is refused before either environment path is read, so this
        // answer cannot depend on the machine the test runs on.
        assert!(
            fml_preflight(ExecutionRole::Client, &[])
                .expect("a client is never a preflight failure")
                .is_none()
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

        let message = format!(
            "{}: clientSideOnly mod cannot run for server execution",
            jar_path.display()
        );
        // Both server roles refuse it, with the same message the dedicated
        // server has always used.
        for role in [
            ExecutionRole::IntegratedServer,
            ExecutionRole::DedicatedServer,
        ] {
            let error = run_java_mods(std::slice::from_ref(&jar_path), role).unwrap_err();
            assert_eq!(error, message, "{role:?}");
        }
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

    /// A jar that carries only loader metadata, to check which loader the
    /// report names without compiling Java.
    fn metadata_jar(root: &std::path::Path, name: &str, file: &str, text: &str) -> PathBuf {
        std::fs::create_dir_all(root).unwrap();
        let path = root.join(format!("{name}.jar"));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file(file, options).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
        zip.finish().unwrap();
        path
    }

    #[test]
    fn java_loader_kinds_name_the_metadata_a_jar_ships() {
        let root = std::env::temp_dir().join(format!("horizon-java-loader-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (name, file, text, want) in [
            (
                "fabric",
                "fabric.mod.json",
                r#"{"id":"demo","version":"1.0","entrypoints":{"main":"demo.Mod"}}"#,
                "fabric",
            ),
            (
                "neoforge",
                "META-INF/neoforge.mods.toml",
                "[[mods]]\nmodId = \"demo\"\nversion = \"1.0\"\n",
                "neoforge",
            ),
            (
                "legacy-forge",
                "META-INF/mods.toml",
                "modLoader = \"javafml\"\n[[mods]]\nmodId = \"demo\"\nversion = \"1.0\"\n",
                "legacy-forge",
            ),
        ] {
            let path = metadata_jar(&root, name, file, text);
            let jar = horizon_native_java::NativeJar::from_path(&path).unwrap();
            assert_eq!(
                JavaLoader::of(&jar).map(JavaLoader::name),
                Some(want),
                "{name}"
            );
        }
        // A jar with no loader metadata is named as unknown rather than guessed.
        assert_eq!(
            JavaLoader::of(&horizon_native_java::NativeJar::default()),
            None
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn java_compatibility_summary_is_deterministic() {
        let mut jar = horizon_native_java::NativeJar::default();
        jar.fabric_id = Some("demo".into());
        jar.fabric_version = Some("1.2.3".into());
        jar.fabric_entrypoints = BTreeMap::from([(
            "main".to_owned(),
            vec![horizon_native_java::Entrypoint {
                class: "demo.Mod".into(),
                method: None,
                adapter: None,
            }],
        )]);
        let opaque = |id: &str| horizon_native_java::RegistryEntry {
            registry: "minecraft:item".into(),
            id: id.into(),
            value: horizon_native_java::Value::Ref(Some("opaque".into())),
            declaration: None,
        };
        let recorded = [
            opaque("demo:token"),
            horizon_native_java::RegistryEntry::typed(
                "minecraft:item",
                "demo:gem",
                horizon_native_java::RegistryDeclaration::Item {
                    template: "minecraft:stick".into(),
                    properties: BTreeMap::new(),
                },
            ),
        ];
        let payloads = [horizon_native_java::PayloadRegistration {
            direction: "play_to_server".into(),
            payload_type: horizon_native_java::Value::Ref(Some("payload".into())),
            codec: horizon_native_java::Value::Ref(Some("codec".into())),
            handler: horizon_native_java::Value::Ref(Some("handler".into())),
        }];
        let report = JavaCompatibilityReport::new(
            &jar,
            std::path::Path::new("/mods/demo.jar"),
            ExecutionRole::DedicatedServer,
            NativeRunFacts {
                kind: "main",
                executed: 1,
                host_calls: 9,
                listeners: 2,
                recorded: &recorded,
                payloads: &payloads,
                typed: 1,
                promoted: vec!["minecraft:item/demo:gem".into()],
                rejected: false,
            },
        );
        let lines = vec![
            "Horizon: Java compatibility /mods/demo.jar: role=dedicated-server loader=fabric \
             id=demo version=1.2.3"
                .to_owned(),
            "  entrypoints: declared=[main] selected=main executed=1 host-calls=9 listeners=2"
                .to_owned(),
            "  registrations: typed=1 opaque=[minecraft:item/demo:token] \
             promoted=[minecraft:item/demo:gem]"
                .to_owned(),
            "  payloads: [play_to_server]".to_owned(),
            "  mixin: 0 plan(s), 0 target(s), 0 unresolved, 0 config, 0 config error(s)".to_owned(),
        ];
        assert_eq!(report.summary(), lines);
        assert_eq!(report.summary(), lines);

        // Each role prints its own name, so a startup log says which one ran.
        for (role, name) in [
            (ExecutionRole::Client, "client"),
            (ExecutionRole::IntegratedServer, "integrated-server"),
            (ExecutionRole::DedicatedServer, "dedicated-server"),
        ] {
            assert_eq!(role_name(role), name);
        }

        // A refused jar reports the decision instead of a run.
        let rejected = JavaCompatibilityReport::new(
            &jar,
            std::path::Path::new("/mods/demo.jar"),
            ExecutionRole::IntegratedServer,
            NativeRunFacts {
                rejected: true,
                ..NativeRunFacts::default()
            },
        );
        assert!(rejected.client_side_only_rejected);
        assert_eq!(
            rejected.summary().last().map(String::as_str),
            Some("  rejected: clientSideOnly metadata for integrated-server execution")
        );

        // Too many rows are sorted first, so the rows a jar keeps never vary.
        let many: Vec<_> = (0..MAX_REPORT_ROWS + 4)
            .rev()
            .map(|index| opaque(&format!("demo:item_{index:02}")))
            .collect();
        let bounded = JavaCompatibilityReport::new(
            &jar,
            std::path::Path::new("/mods/demo.jar"),
            ExecutionRole::Client,
            NativeRunFacts {
                recorded: &many,
                ..NativeRunFacts::default()
            },
        );
        assert_eq!(bounded.opaque_registrations.len(), MAX_REPORT_ROWS);
        assert_eq!(
            bounded.opaque_registrations[0],
            "minecraft:item/demo:item_00"
        );
    }

    /// A jar's Mixin planning is one bounded line: the plans, the methods they
    /// hook, the selectors this runtime could not read, and the configs it
    /// read. Built from execution facts, so no jar has to be compiled here.
    #[test]
    fn java_compatibility_summary_reports_mixin_planning() {
        let mut jar = horizon_native_java::NativeJar::default();
        jar.fabric_id = Some("demo".into());
        jar.fabric_version = Some("1.0".into());
        let report = JavaCompatibilityReport {
            mixin_plans: 2,
            mixin_targets: vec![
                "demo/Target#greet(Ljava/lang/String;)Ljava/lang/String;".into(),
                "demo/Target#tick()V".into(),
            ],
            mixin_unresolved: 1,
            mixin_configs: 1,
            mixin_config_errors: 0,
            ..JavaCompatibilityReport::new(
                &jar,
                std::path::Path::new("/mods/demo.jar"),
                ExecutionRole::Client,
                NativeRunFacts::default(),
            )
        };
        let summary = report.summary();
        // Header, entrypoints, registrations, payloads, then Mixin planning.
        assert_eq!(
            summary[4],
            "  mixin: 2 plan(s), 2 target(s), 1 unresolved, 1 config, 0 config error(s) \
             targets=[demo/Target#greet(Ljava/lang/String;)Ljava/lang/String;, \
             demo/Target#tick()V]"
        );
        assert_eq!(summary, report.summary());
        // Nothing planned reads as nothing planned instead of a missing line.
        let quiet = JavaCompatibilityReport {
            mixin_plans: 0,
            mixin_targets: Vec::new(),
            mixin_unresolved: 0,
            mixin_configs: 0,
            mixin_config_errors: 0,
            ..report.clone()
        };
        assert_eq!(
            quiet.summary()[4],
            "  mixin: 0 plan(s), 0 target(s), 0 unresolved, 0 config, 0 config error(s)"
        );
    }

    /// The jar-derived path: `fabric.mod.json` and the root `*.mixins.json`
    /// entries fill the fields with no entrypoint declared, and a malformed
    /// config is counted rather than hidden.
    #[test]
    fn java_compatibility_report_reads_mixin_configs_from_the_jar() {
        let root = std::env::temp_dir().join(format!(
            "horizon-java-mixin-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("demo.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).unwrap();
        zip.write_all(br#"{"id":"demo","version":"1.0","mixins":["demo.mixins.json"]}"#)
            .unwrap();
        zip.start_file("demo.mixins.json", options).unwrap();
        zip.write_all(br#"{"required":true,"package":"demo.mixin","mixins":["DemoMixin"]}"#)
            .unwrap();
        // A config this bounded reader cannot parse: a diagnostic, not a stop.
        zip.start_file("broken.mixins.json", options).unwrap();
        zip.write_all(br#"{"mixins":5}"#).unwrap();
        zip.finish().unwrap();

        let jar = horizon_native_java::NativeJar::from_path(&path).unwrap();
        let report = JavaCompatibilityReport::new(
            &jar,
            &path,
            ExecutionRole::Client,
            NativeRunFacts::default(),
        );
        // No class carries a mixin annotation, so nothing is planned; both
        // configs were found and one could not be read.
        assert_eq!(report.mixin_plans, 0);
        assert!(report.mixin_targets.is_empty());
        assert_eq!(report.mixin_unresolved, 0);
        assert_eq!(report.mixin_configs, 1);
        assert_eq!(report.mixin_config_errors, 1);
        assert_eq!(
            report.summary()[4],
            "  mixin: 0 plan(s), 0 target(s), 0 unresolved, 1 config, 1 config error(s)"
        );
        assert!(
            report
                .mixin_warning()
                .is_some_and(|warning| warning.contains(&path.display().to_string())),
            "{:?}",
            report.mixin_warning()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// A jar whose Mixin planning is partly untranslatable warns once, naming
    /// the jar and the counts; nothing planned warns not at all.
    #[test]
    fn java_compatibility_mixin_warning_names_the_jar_and_counts() {
        let jar = horizon_native_java::NativeJar::default();
        let report = JavaCompatibilityReport::new(
            &jar,
            std::path::Path::new("/mods/demo.jar"),
            ExecutionRole::DedicatedServer,
            NativeRunFacts::default(),
        );
        assert_eq!(report.mixin_warning(), None);
        let untranslatable = JavaCompatibilityReport {
            mixin_unresolved: 1,
            mixin_config_errors: 2,
            ..report
        };
        assert_eq!(
            untranslatable.mixin_warning().as_deref(),
            Some(
                "Horizon: native Java /mods/demo.jar has 1 unresolved Mixin plan(s) and 2 unreadable Mixin config(s); those stay on the JVM fallback"
            )
        );
    }

    /// One compiled initializer, shipped under two typed declarations: the
    /// callback registers `report:gem` and `report:token` opaquely, so the
    /// first declaration collides with a row and the second does not.
    #[test]
    fn java_mod_reports_separate_typed_promoted_and_opaque_rows() {
        let root = std::env::temp_dir().join(format!(
            "horizon-game-java-report-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let classes = root.join("classes");
        std::fs::create_dir_all(&classes).unwrap();
        let sources = [
            (
                "net/fabricmc/api/ModInitializer.java",
                "package net.fabricmc.api; public interface ModInitializer { void onInitialize(); }",
            ),
            (
                "net/minecraft/resources/Identifier.java",
                r#"package net.minecraft.resources; public final class Identifier { private final String namespace; private final String path; private Identifier(String n, String p) { namespace = n; path = p; } public static Identifier fromNamespaceAndPath(String n, String p) { return new Identifier(n, p); } public String toString() { return namespace + ":" + path; } }"#,
            ),
            (
                "net/minecraft/core/Registry.java",
                "package net.minecraft.core; import net.minecraft.resources.Identifier; public interface Registry<T> { static <T> T register(Registry<T> registry, Identifier id, T value) { return value; } }",
            ),
            (
                "net/minecraft/core/registries/Registries.java",
                "package net.minecraft.core.registries; import net.minecraft.core.Registry; public final class Registries { public static final Registry<Object> ITEM = null; }",
            ),
            (
                "report/ReportMod.java",
                r#"package report; import net.fabricmc.api.ModInitializer; import net.minecraft.core.Registry; import net.minecraft.core.registries.Registries; import net.minecraft.resources.Identifier; public final class ReportMod implements ModInitializer { public void onInitialize() { Registry.register(Registries.ITEM, Identifier.fromNamespaceAndPath("report", "gem"), new Object()); Registry.register(Registries.ITEM, Identifier.fromNamespaceAndPath("report", "token"), new Object()); } }"#,
            ),
        ];
        let mut paths = Vec::new();
        for (name, text) in sources {
            let path = src.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            paths.push(path);
        }
        let status = Command::new("javac")
            .arg("--release")
            .arg("17")
            .arg("-d")
            .arg(&classes)
            .args(&paths)
            .status()
            .unwrap();
        assert!(status.success(), "javac failed");
        let write = |name: &str, declared: &str| -> PathBuf {
            let path = root.join(format!("{name}.jar"));
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("fabric.mod.json", options).unwrap();
            zip.write_all(
                br#"{"id":"report-mod","version":"1.0","entrypoints":{"main":"report.ReportMod"}}"#,
            )
            .unwrap();
            zip.start_file("report/ReportMod.class", options).unwrap();
            zip.write_all(&std::fs::read(classes.join("report/ReportMod.class")).unwrap())
                .unwrap();
            zip.start_file("META-INF/horizon-native.json", options)
                .unwrap();
            zip.write_all(declared.as_bytes()).unwrap();
            zip.finish().unwrap();
            path
        };
        let colliding = write(
            "report-colliding",
            r#"{"id":"report-mod","registrations":[{"registry":"minecraft:item","id":"report:gem","kind":"item","template":"minecraft:stick"}]}"#,
        );
        let distinct = write(
            "report-distinct",
            r#"{"id":"report-mod","registrations":[{"registry":"minecraft:item","id":"report:ore","kind":"item","template":"minecraft:stick"}]}"#,
        );
        let (content, reports) =
            run_java_mods_with_report(&[colliding, distinct], ExecutionRole::DedicatedServer)
                .unwrap();
        assert_eq!(reports.len(), 2);
        let report = &reports[0];
        assert_eq!(role_name(report.role), "dedicated-server");
        assert_eq!(report.loader.map(JavaLoader::name), Some("fabric"));
        assert_eq!(report.id.as_deref(), Some("report-mod"));
        assert_eq!(report.version.as_deref(), Some("1.0"));
        assert_eq!(report.declared_entrypoints, ["main"]);
        assert_eq!(report.entrypoint_kind, "main");
        assert_eq!(report.executed_entrypoints, 1);
        assert!(report.host_calls > 0, "{report:?}");
        // The typed metadata row wins over the colliding callback row; the
        // other callback row stays opaque.
        assert_eq!(report.typed_registrations, 1);
        assert_eq!(report.opaque_registrations, ["minecraft:item/report:token"]);
        assert_eq!(report.promoted, ["minecraft:item/report:gem"]);
        let summary = report.summary();
        assert!(
            summary[0].contains("role=dedicated-server loader=fabric id=report-mod version=1.0"),
            "{summary:?}"
        );
        assert!(
            summary[1].contains("selected=main executed=1"),
            "{summary:?}"
        );
        // No id collides in the second jar, so nothing is promoted.
        assert!(reports[1].promoted.is_empty());
        assert_eq!(
            reports[1].opaque_registrations,
            ["minecraft:item/report:gem", "minecraft:item/report:token"]
        );
        assert_eq!(content.len(), 2);
        assert_eq!(content[0].content.items[0].id, "report:gem");
        assert_eq!(content[1].content.items[0].id, "report:ore");
        // A synthetic content mod joining the run's native ones: the summary
        // is over the merged tables, not one jar. Items only, so the aggregate
        // keeps its shape and appends one bounded count.
        let mut merged = content.clone();
        merged.push(item_mod("synthetic"));
        let summary = mod_content::declaration_summary(&merged);
        assert_eq!((summary.blocks, summary.items), (0, 3));
        let lines = declaration_lines(&summary);
        assert_eq!(lines[0], "Horizon: Java content: 3 items");
        assert_eq!(
            lines.last().unwrap(),
            "Horizon: Java content: ids: report:gem, report:ore, synthetic:widget"
        );
        assert_eq!(
            aggregate_line(&reports, &summary),
            "Horizon: Java compatibility: 2 jar(s), 2 typed registrations, 3 opaque, \
             0 payload directions, 0 mixin plan(s), 0 unresolved, 3 declared item(s)"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// Declarations holding one item with `id`, for the summary tests below.
    fn one_item(id: &str) -> horizon_mods::native::NativeDeclarations {
        horizon_mods::native::NativeDeclarations {
            items: vec![horizon_mods::content::Item {
                id: id.into(),
                template: "minecraft:diamond".into(),
                max_stack_size: None,
                rarity: None,
                components: Default::default(),
                crafting_remainder: None,
            }],
            ..Default::default()
        }
    }

    /// A content mod declaring `<mod_id>:widget`.
    fn item_mod(mod_id: &str) -> ContentMod {
        ContentMod {
            id: mod_id.into(),
            content: horizon_mods::content::Content::from_native(
                mod_id,
                one_item(&format!("{mod_id}:widget")),
            )
            .unwrap(),
        }
    }

    /// A run whose jars declare no typed rows logs no content lines and keeps
    /// the aggregate line exactly as it was before the summary joined it. A
    /// content mod with files or a namespace alone counts as no declaration.
    #[test]
    fn java_run_without_declarations_logs_no_content_lines() {
        let jar = horizon_native_java::NativeJar::default();
        let reports = [JavaCompatibilityReport::new(
            &jar,
            std::path::Path::new("/mods/code-only.jar"),
            ExecutionRole::Client,
            NativeRunFacts::default(),
        )];
        let legacy = "Horizon: Java compatibility: 1 jar(s), 0 typed registrations, 0 opaque, \
                      0 payload directions, 0 mixin plan(s), 0 unresolved";
        for summary in [
            mod_content::declaration_summary(&[]),
            mod_content::declaration_summary(&[ContentMod {
                id: "helper".into(),
                content: horizon_mods::content::Content::from_native(
                    "helper",
                    horizon_mods::native::NativeDeclarations::default(),
                )
                .unwrap(),
            }]),
        ] {
            assert!(summary.is_empty());
            assert!(declaration_lines(&summary).is_empty());
            assert_eq!(aggregate_line(&reports, &summary), legacy);
        }
    }

    /// The declared part of the aggregate line stays bounded: blocks and items
    /// are named, every other kind folds into one count, and a run that
    /// declares nothing but those other kinds still says so.
    #[test]
    fn java_aggregate_line_bounds_the_other_declared_kinds() {
        let reports = [JavaCompatibilityReport::new(
            &horizon_native_java::NativeJar::default(),
            std::path::Path::new("/mods/demo.jar"),
            ExecutionRole::Client,
            NativeRunFacts::default(),
        )];
        let mut mods = vec![item_mod("ruby")];
        mods.push(ContentMod {
            id: "ruby".into(),
            content: horizon_mods::content::Content::from_native(
                "ruby",
                horizon_mods::native::NativeDeclarations {
                    recipes: vec![horizon_mods::native::RecipeDeclaration {
                        id: "ruby:widget_recipe".into(),
                        json: b"{}".to_vec(),
                    }],
                    ..Default::default()
                },
            )
            .unwrap(),
        });
        let summary = mod_content::declaration_summary(&mods);
        assert_eq!((summary.items, summary.recipes), (1, 1));
        assert_eq!(
            aggregate_line(&reports, &summary),
            "Horizon: Java compatibility: 1 jar(s), 0 typed registrations, 0 opaque, \
             0 payload directions, 0 mixin plan(s), 0 unresolved, 1 declared item(s), \
             1 other declared row(s)"
        );
        // Recipes alone: named as one other count rather than losing them.
        let summary = mod_content::declaration_summary(&mods[1..]);
        assert_eq!(
            aggregate_line(&reports, &summary),
            "Horizon: Java compatibility: 1 jar(s), 0 typed registrations, 0 opaque, \
             0 payload directions, 0 mixin plan(s), 0 unresolved, 1 other declared row(s)"
        );
    }

    /// A report for a jar whose native execution was isolated.
    fn isolated_report(path: &str, role: ExecutionRole, message: &str) -> JavaCompatibilityReport {
        let mut report = JavaCompatibilityReport::new(
            &horizon_native_java::NativeJar::default(),
            std::path::Path::new(path),
            role,
            NativeRunFacts {
                kind: "main",
                ..NativeRunFacts::default()
            },
        );
        report.isolation_failure = Some(bounded_isolation_message(message));
        report
    }

    /// A panic in a jar's native execution is caught at the helper boundary:
    /// the next run is unaffected, so a malformed mod fails only itself.
    #[test]
    fn native_java_panic_isolation_keeps_later_jars_running() {
        assert_eq!(isolate_jar(|| 7), Ok(7));
        assert_eq!(
            isolate_jar(|| panic!("demo mod exploded")).unwrap_err(),
            "demo mod exploded"
        );
        assert_eq!(
            isolate_jar(|| std::panic::panic_any(7u8)).unwrap_err(),
            "non-string panic payload"
        );
        // Nothing survives the caught run: the following jar still executes.
        assert_eq!(isolate_jar(|| "later jar"), Ok("later jar"));

        // The recorded message is one bounded, flattened line.
        let long = "x".repeat(MAX_ISOLATION_MESSAGE * 4);
        let bounded = bounded_isolation_message(&long);
        assert_eq!(bounded.chars().count(), MAX_ISOLATION_MESSAGE + 3);
        assert!(bounded.ends_with("..."));
        assert_eq!(
            bounded_isolation_message("line one\nline two\r\n\tthree"),
            "line one line two   three"
        );
        assert_eq!(bounded_isolation_message("  \n "), "empty panic message");
    }

    /// An isolated jar keeps its metadata and role, its line is the bounded
    /// panic message rather than a stack, and the run summary sorts the jars.
    #[test]
    fn java_compatibility_summary_reports_isolated_panics() {
        let report = isolated_report("/mods/bad.jar", ExecutionRole::DedicatedServer, "boom");
        let summary = report.summary();
        assert!(
            summary[0].contains("role=dedicated-server loader=unknown"),
            "{summary:?}"
        );
        assert_eq!(summary.last().map(String::as_str), Some("  isolated: boom"));
        assert_eq!(summary, report.summary());

        let quiet = JavaCompatibilityReport::new(
            &horizon_native_java::NativeJar::default(),
            std::path::Path::new("/mods/good.jar"),
            ExecutionRole::Client,
            NativeRunFacts::default(),
        );
        assert!(quiet.isolation_failure.is_none());
        let reports = [
            isolated_report("/mods/z.jar", ExecutionRole::Client, "boom"),
            isolated_report("/mods/a.jar", ExecutionRole::Client, "boom"),
            quiet,
        ];
        assert_eq!(
            isolation_summary(&reports).as_deref(),
            Some(
                "Horizon: Java compatibility: java isolation: 2 jar(s) failed: \
                 [/mods/a.jar, /mods/z.jar]"
            )
        );
        assert_eq!(isolation_summary(&[]), None);
    }

    #[test]
    fn java_isolation_strict_mode_stops_after_all_jars() {
        let reports = [
            isolated_report("/mods/z.jar", ExecutionRole::Client, "boom"),
            isolated_report("/mods/a.jar", ExecutionRole::Client, "boom"),
        ];
        // Default: log-and-continue.
        assert_eq!(isolation_startup_error(&reports, false), None);
        // Strict: the first failure in input order, not the sorted one.
        let error = isolation_startup_error(&reports, true).unwrap();
        assert!(
            error.starts_with("/mods/z.jar: isolated native Java panic: boom"),
            "{error}"
        );
        // Nothing isolated leaves strict startup with nothing to stop.
        let quiet = [JavaCompatibilityReport::new(
            &horizon_native_java::NativeJar::default(),
            std::path::Path::new("/mods/good.jar"),
            ExecutionRole::Client,
            NativeRunFacts::default(),
        )];
        assert_eq!(isolation_startup_error(&quiet, true), None);
    }
}
