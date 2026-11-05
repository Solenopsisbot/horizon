//! Java mods (Fabric, NeoForge, Forge) from the browser (docs/mod-browser.md). Horizon doesn't
//! run their code; it ports what they carry (blocks, items, recipes, loot, tags, worldgen) with
//! `horizon-port-pack`. Before a mod is offered for install its jar is downloaded (checked as
//! every download is), carried over and its classes read (`horizon_port_pack::check`): the
//! verdict (Works, Partly, Doesn't run) and the lists of what ports and what doesn't come from
//! that, and are kept per file so a jar is checked once.
//!
//! Installing copies the carried-over folder mod into `horizon-mods/<mod id>/` beside
//! `options.txt` and turns it on in `horizon-mods.txt`, where the game's Options > Mods reads it.
use super::{installed::Entry, pick, FileRef, Kind, Project, Source, Support, Version};
use crate::i18n::{self, Language};
use horizon_mods::java_artifact;
use horizon_port_pack::check::{self, Check, Verdict};
use horizon_port_pack::classes::Feature;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Where the checks are kept, under the game folder. Its name changes when the check finds
/// more, so older answers are checked again.
const CHECKS: &str = "mod-browser/checks-1";
/// The game's mod folder and on-list, beside `options.txt` (`horizon_client::mod_list`).
pub const MODS_DIR: &str = "horizon-mods";
const MODS_LIST: &str = "horizon-mods.txt";
/// How deep required mods are followed.
const MAX_DEPTH: usize = 3;

/// The loaders whose jars port-pack reads.
pub const LOADERS: [&str; 4] = ["neoforge", "fabric", "forge", "quilt"];

/// A probe message on one line, cut to a player-sized length.
fn bounded(text: &str) -> String {
    const MAX_DETAIL: usize = 240;
    let mut detail = text.replace(['\n', '\r'], " ");
    if detail.chars().count() > MAX_DETAIL {
        detail = detail.chars().take(MAX_DETAIL - 1).collect();
        detail.push('…');
    }
    detail
}

/// Adds the bounded native-probe result to the browser verdict. A probe failure does not
/// change what content can be carried, but it tells the player that Java compatibility will
/// need the embedded JVM path once that runtime is enabled.
fn compatibility_note(check: &Check) -> Option<String> {
    let native = check.native.as_ref()?;
    let error = native.error.as_deref()?;
    let detail = bounded(error);
    let entry = native
        .entrypoint
        .as_deref()
        .map(|name| format!(" for {}", name))
        .unwrap_or_default();
    let kind = native
        .kind
        .as_deref()
        .map(|kind| format!(" ({kind})"))
        .unwrap_or_default();
    Some(format!(
        "Java compatibility probe stopped{kind}{entry} after {} host calls; embedded JVM fallback may be needed: {detail}",
        native.calls
    ))
}

pub fn support_of(check: &Check) -> Support {
    let reason = || match compatibility_note(check) {
        Some(note) if check.summary.is_empty() => note,
        Some(note) => format!("{} {note}", check.summary),
        None => check.summary.clone(),
    };
    match check.verdict {
        Verdict::Works
            if check
                .native
                .as_ref()
                .is_none_or(|native| native.error.is_none()) =>
        {
            Support::Works
        }
        Verdict::Works => Support::Partial(reason()),
        Verdict::Partly => Support::Partial(reason()),
        Verdict::No => Support::No(reason()),
    }
}

/// What an installed Java mod's cached check says about how it runs, as lines the Mods page
/// shows: the loader, the probe's verdict, which entrypoint it tried, how many host calls it
/// made, what it registered, whether Mixins went unapplied and why it stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostics {
    /// `Works`, `Partial` (the probe stopped: the embedded JVM may be needed) or `Not probed`.
    pub verdict: &'static str,
    /// Whether the probe stopped on an error.
    pub failed: bool,
    pub lines: Vec<String>,
}

const RUNTIME_SCHEMA: u32 = 1;
const RUNTIME_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeReport {
    schema_version: u32,
    jars: Vec<RuntimeJar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeJar {
    file: String,
    loader: String,
    engine: String,
    woven: usize,
    skipped: usize,
    first_skip_reason: Option<String>,
    registrations: usize,
    opaque: usize,
    payload_channels: Vec<String>,
    isolation_failure: Option<String>,
}

/// Reads the game's bounded runtime sidecar. Unknown schema versions are
/// deliberately ignored so a newer game never produces misleading lines.
pub fn runtime_report(home: &Path) -> Option<serde_json::Value> {
    let bytes = fs::read(home.join("java-runtime.json")).ok()?;
    if bytes.len() > RUNTIME_MAX_BYTES {
        return None;
    }
    let report: RuntimeReport = serde_json::from_slice(&bytes).ok()?;
    (report.schema_version == RUNTIME_SCHEMA)
        .then(|| serde_json::to_value(report).ok())
        .flatten()
}

/// Bounded runtime lines for one installed jar, matched by its file name.
pub fn runtime_lines_in(
    language: Language,
    report: &serde_json::Value,
    file_name: &str,
) -> Option<Vec<String>> {
    let report: RuntimeReport = serde_json::from_value(report.clone()).ok()?;
    let jar = report.jars.into_iter().find(|jar| {
        Path::new(&jar.file)
            .file_name()
            .is_some_and(|name| name == file_name)
            || jar.file == file_name
    })?;
    let woven = jar.woven.to_string();
    let skipped = jar.skipped.to_string();
    let registrations = jar.registrations.to_string();
    let opaque = jar.opaque.to_string();
    let payloads = jar.payload_channels.len().to_string();
    let mut lines = vec![say(
        language,
        "Runtime: {engine} · loader: {loader} · woven: {woven} · skipped: {skipped} · registrations: {registrations} · opaque: {opaque} · payloads: {payloads}",
        &[
            ("engine", &jar.engine),
            ("loader", &jar.loader),
            ("woven", &woven),
            ("skipped", &skipped),
            ("registrations", &registrations),
            ("opaque", &opaque),
            ("payloads", &payloads),
        ],
    )];
    if let Some(reason) = jar.first_skip_reason {
        lines.push(say(
            language,
            "Runtime skipped: {reason}",
            &[("reason", &bounded(&reason))],
        ));
    }
    if let Some(error) = jar.isolation_failure {
        lines.push(say(
            language,
            "Runtime isolation: {error}",
            &[("error", &bounded(&error))],
        ));
    }
    Some(lines)
}

/// One summary line when the report has no matching installed file.
pub fn runtime_summary_in(language: Language, report: &serde_json::Value) -> Option<String> {
    let report: RuntimeReport = serde_json::from_value(report.clone()).ok()?;
    let count = report.jars.len().to_string();
    Some(say(
        language,
        "Runtime: {count} jar(s) reported",
        &[("count", &count)],
    ))
}

/// The most entrypoint names listed before "+N more".
const LISTED_ENTRYPOINTS: usize = 2;

/// Reads `check` into [`Diagnostics`]. Every line is bounded, so a hostile jar can't grow the
/// panel: the probe's message is cut to [`bounded`] and entrypoints are counted past two.
/// The labels are said in the launcher's language; [`diagnostics_in`] takes it as an argument.
pub fn diagnostics(check: &Check) -> Diagnostics {
    diagnostics_in(i18n::current(), check)
}

/// [`diagnostics`] in `language`, so the page and its tests needn't depend on the global setting.
pub fn diagnostics_in(language: Language, check: &Check) -> Diagnostics {
    let loader = match check.loader.as_str() {
        "" => i18n::tr_in(language, "plain zip"),
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        other => other,
    };
    let mut lines = vec![say(language, "Loader: {loader}", &[("loader", loader)])];
    let (verdict, failed) = match &check.native {
        None => ("Not probed", false),
        Some(native) if native.error.is_some() => ("Partial", true),
        Some(_) => ("Works", false),
    };
    let verdict_said = i18n::tr_in(language, verdict);
    lines.push(say(
        language,
        "Probe: {verdict}",
        &[("verdict", verdict_said)],
    ));
    if let Some(native) = &check.native {
        let mut names: Vec<&str> = native.entrypoints.iter().map(String::as_str).collect();
        if names.is_empty() {
            names.extend(native.entrypoint.as_deref());
        }
        let kind = native
            .kind
            .as_deref()
            .unwrap_or_else(|| i18n::tr_in(language, "unknown"));
        let listed = names
            .iter()
            .take(LISTED_ENTRYPOINTS)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(match names.len() {
            0 => say(
                language,
                "Entrypoint: {kind}, none found",
                &[("kind", kind)],
            ),
            n if n > LISTED_ENTRYPOINTS => say(
                language,
                "Entrypoint: {kind} · {listed} (+{count} more)",
                &[
                    ("kind", kind),
                    ("listed", &listed),
                    ("count", &(n - LISTED_ENTRYPOINTS).to_string()),
                ],
            ),
            _ => say(
                language,
                "Entrypoint: {kind} · {listed}",
                &[("kind", kind), ("listed", &listed)],
            ),
        });
        lines.push(say(
            language,
            "Host calls: {calls} · registrations: {registrations} · listeners: {listeners} · payloads: {payloads}",
            &[
                ("calls", &native.calls.to_string()),
                ("registrations", &native.registrations.to_string()),
                ("listeners", &native.listeners.to_string()),
                ("payloads", &native.payloads.to_string()),
            ],
        ));
    }
    let mixins = check
        .code
        .features
        .get(&Feature::Mixin)
        .copied()
        .unwrap_or(0);
    if mixins > 0 {
        let key = if mixins == 1 {
            "Mixins: {count} class not applied by the native probe; behaviour they change may be missing"
        } else {
            "Mixins: {count} classes not applied by the native probe; behaviour they change may be missing"
        };
        lines.push(say(language, key, &[("count", &mixins.to_string())]));
    }
    if let Some(error) = check.native.as_ref().and_then(|n| n.error.as_deref()) {
        lines.push(say(
            language,
            "Stopped: {error} (the embedded JVM fallback may be needed)",
            &[("error", &bounded(error))],
        ));
    }
    Diagnostics {
        verdict,
        failed,
        lines,
    }
}

/// `key` as `language` says it, its `{name}` holes filled with `args`.
fn say(language: Language, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = i18n::tr_in(language, key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

/// Which mod's diagnostics come first: a probe that stopped, then more Mixins, then more host
/// calls. The page breaks ties by name.
pub fn most_relevant_first(a: &Check, b: &Check) -> std::cmp::Ordering {
    let key = |c: &Check| {
        (
            c.native.as_ref().is_some_and(|n| n.error.is_some()),
            c.code.features.get(&Feature::Mixin).copied().unwrap_or(0),
            c.native.as_ref().map_or(0, |n| n.calls),
        )
    };
    key(b).cmp(&key(a))
}

fn checks(home: &Path) -> PathBuf {
    home.join(CHECKS)
}

/// The folder a file's check is kept in, by its checksum.
fn checked_dir(home: &Path, file: &FileRef) -> Option<PathBuf> {
    let (_, hash) = file.checksum()?;
    checked_dir_of(home, hash)
}

/// The folder for a checksum's hex digits. Only hex survives and the name is cut to 40 digits,
/// so a hostile value can't leave the checks folder.
fn checked_dir_of(home: &Path, hash: &str) -> Option<PathBuf> {
    let hash: String = hash
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(40)
        .collect();
    (hash.len() >= 32).then(|| checks(home).join(hash))
}

/// The folder for an installed entry's `ALGO:hex` checksum (`installed::Entry::checksum`).
fn checked_dir_for_entry(home: &Path, checksum: &str) -> Option<PathBuf> {
    checked_dir_of(
        home,
        checksum.split_once(':').map_or(checksum, |(_, hex)| hex),
    )
}

fn read_check(dir: &Path) -> Option<Check> {
    let check: Check =
        serde_json::from_str(&fs::read_to_string(dir.join("check.json")).ok()?).ok()?;
    // The report describes exact bytes. `check.provenance.json` beside it names
    // the jar it was made from; a copy of the folder under another jar's
    // checksum, or a record that no longer matches its own key, is not that
    // jar's answer, and the jar is checked again. A folder written before
    // records existed keeps its report.
    if let Ok(text) = fs::read_to_string(dir.join(check::PROVENANCE)) {
        let record: Option<java_artifact::Provenance> = serde_json::from_str(&text).ok();
        let fresh = record.is_some_and(|record| record.derive() == record.key);
        if !fresh {
            return None;
        }
    }
    Some(check)
}

/// The check of `file` if it was made before.
pub fn cached(home: &Path, file: &FileRef) -> Option<Check> {
    read_check(&checked_dir(home, file)?)
}

/// The cached check of an installed mod, by the `ALGO:hex` checksum its entry keeps.
pub fn cached_for_entry(home: &Path, checksum: &str) -> Option<Check> {
    read_check(&checked_dir_for_entry(home, checksum)?)
}

/// Forgets one installed mod's cached check (and any half-written copy of it) so the next
/// check of that jar reads it again. Only that checksum's folder goes; every other mod's
/// check, and the `.downloads` scratch folder, stay. Returns whether anything was removed.
pub fn clear_check(home: &Path, checksum: &str) -> Result<bool, String> {
    let dir = checked_dir_for_entry(home, checksum)
        .ok_or_else(|| format!("{checksum} isn't a checksum a check is kept under."))?;
    let mut removed = false;
    for path in [dir.clone(), dir.with_extension("part")] {
        match fs::remove_dir_all(&path) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(removed)
}

/// Checks `file`: the cached answer, else its jar downloaded, carried over and read. The jar
/// itself isn't kept; the carry is, when something of it ports, for the install to copy.
pub fn check_file(
    home: &Path,
    source: &dyn Source,
    file: &FileRef,
    progress: &dyn Fn(f32),
) -> Result<Check, String> {
    let dir = checked_dir(home, file).ok_or("Its source gives no checksum.")?;
    if let Some(c) = read_check(&dir) {
        return Ok(c);
    }
    let jar = source.fetch(file, &checks(home).join(".downloads"), progress)?;
    let part = dir.with_extension("part");
    let _ = fs::remove_dir_all(&part);
    let result = check::check(&jar, &part);
    let _ = fs::remove_file(&jar);
    let c = result.inspect_err(|_| {
        let _ = fs::remove_dir_all(&part);
    })?;
    if c.verdict == Verdict::No {
        for d in ["carry", "outside"] {
            let _ = fs::remove_dir_all(part.join(d));
        }
    }
    let _ = fs::remove_dir_all(&dir);
    fs::rename(&part, &dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(c)
}

/// The version of `project` to install and its check.
pub fn check_project(
    home: &Path,
    source: &dyn Source,
    project: &Project,
    everything: bool,
    progress: &dyn Fn(f32),
) -> Result<(Version, Check), String> {
    let versions = source.versions(project)?;
    let version = pick(Kind::Mod, &versions, everything)
        .cloned()
        .ok_or_else(|| match versions.iter().find_map(|v| v.blocked.clone()) {
            Some(why) => format!("{} can't be downloaded from here: {why}", project.title),
            None => format!(
                "{} has no Fabric, NeoForge or Forge version Horizon can port.",
                project.title
            ),
        })?;
    let file = version.file().expect("pick keeps versions with a file");
    let check = check_file(home, source, file, progress)?;
    Ok((version, check))
}

/// Whether mod `id` is on in `horizon-mods.txt`.
pub fn is_enabled(home: &Path, id: &str) -> bool {
    let key = format!("mod.{id}");
    fs::read_to_string(home.join(MODS_LIST))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(':'))
        .any(|(k, v)| k.trim() == key && v.trim() == "true")
}

/// Turns mod `id` on (or off) in `horizon-mods.txt`, keeping the other lines.
pub fn set_enabled(home: &Path, id: &str, on: bool) -> Result<(), String> {
    let path = home.join(MODS_LIST);
    let text = fs::read_to_string(&path).unwrap_or_default();
    let key = format!("mod.{id}");
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.split_once(':').is_none_or(|(k, _)| k.trim() != key))
        .map(str::to_owned)
        .collect();
    if on {
        lines.push(format!("{key}:true"));
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    fs::write(&path, out).map_err(|e| format!("{}: {e}", path.display()))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    Ok(())
}

/// Installs the Java mod `project`: checked, carried over into `horizon-mods/<id>/` and turned
/// on, with the mods it requires that port too. A required mod with nothing to port (a library)
/// is listed in what was skipped. Returns the entries installed, the asked-for one last.
pub fn install(
    source: &dyn Source,
    project: &Project,
    everything: bool,
    home: &Path,
    progress: &dyn Fn(&str, f32),
) -> Result<Vec<Entry>, String> {
    let job = Job {
        source,
        everything,
        home,
        progress,
    };
    let mut done = Vec::new();
    job.add(project, false, 0, &mut done)?;
    Ok(done)
}

/// One install: where from, where to, and how it reports.
struct Job<'a> {
    source: &'a dyn Source,
    everything: bool,
    home: &'a Path,
    progress: &'a dyn Fn(&str, f32),
}

impl Job<'_> {
    fn add(
        &self,
        project: &Project,
        dependency: bool,
        depth: usize,
        done: &mut Vec<Entry>,
    ) -> Result<(), String> {
        let Job {
            source,
            everything,
            home,
            progress,
        } = *self;
        if done.iter().any(|e| e.project == project.id) {
            return Ok(());
        }
        let (version, check) = check_project(home, source, project, everything, &|f| {
            progress(&format!("Checking {}", project.title), f)
        })?;
        if check.verdict == Verdict::No {
            return Err(format!(
                "{} doesn't run in Horizon: {}",
                project.title, check.summary
            ));
        }
        let mut skipped = check.skipped.clone();
        for id in &version.required {
            let Ok(needed) = source.project(id) else {
                continue;
            };
            if needed.kind != Kind::Mod || depth >= MAX_DEPTH {
                continue;
            }
            match self.add(&needed, true, depth + 1, done) {
                Ok(()) => {}
                Err(_) if dependency => {}
                Err(why) => skipped.push(format!("needs {}: {why}", needed.title)),
            }
        }
        let file = version.file().expect("pick keeps versions with a file");
        let carry = checked_dir(home, file)
            .expect("checked above")
            .join("carry");
        let relative = format!("{MODS_DIR}/{}", check.mod_id);
        let mut installed = super::installed::Installed::load(home);
        let ours = |e: &Entry| e.source == project.source && e.project == project.id;
        if let Some(other) = installed.entries.iter().find(|e| {
            e.kind == Kind::Mod
                && !ours(e)
                && (e.file == relative || e.namespaces.iter().any(|n| check.namespaces.contains(n)))
        }) {
            return Err(format!(
                "{} shares its id or namespace with {}, installed already: remove one first.",
                project.title, other.title
            ));
        }
        let target = home.join(&relative);
        let old = installed.entries.iter().find(|e| ours(e)).cloned();
        if target.exists() && old.as_ref().is_none_or(|o| o.file != relative) {
            return Err(format!(
                "{relative} is already there and wasn't installed by the browser: move it away first."
            ));
        }
        progress(&format!("Installing {}", project.title), 1.);
        let part = home.join(MODS_DIR).join(format!(".{}.part", check.mod_id));
        let _ = fs::remove_dir_all(&part);
        copy_dir(&carry, &part).inspect_err(|_| {
            let _ = fs::remove_dir_all(&part);
        })?;
        if let Some(old) = &old {
            let _ = fs::remove_dir_all(home.join(&old.file));
            if old.file != relative {
                let _ = set_enabled(home, old.file.trim_start_matches("horizon-mods/"), false);
            }
        }
        fs::rename(&part, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        set_enabled(home, &check.mod_id, true)?;
        let (algo, hash) = file.checksum().expect("checked files have one");
        let entry = Entry {
            source: project.source,
            project: project.id.clone(),
            slug: project.slug.clone(),
            title: project.title.clone(),
            kind: Kind::Mod,
            version: version.id.clone(),
            number: version.number.clone(),
            file: relative,
            checksum: format!("{algo}:{hash}"),
            page: project.page.clone(),
            dependency: dependency && old.as_ref().is_none_or(|o| o.dependency),
            verdict: Some(check.verdict.label().to_owned()),
            skipped,
            namespaces: check.namespaces.clone(),
        };
        installed.entries.retain(|e| !ours(e));
        installed.entries.push(entry.clone());
        installed.save(home)?;
        done.push(entry);
        Ok(())
    }
}

/// Removes an installed mod's folder and turns it off.
pub fn remove(home: &Path, entry: &Entry) -> Result<(), String> {
    let Some(id) = entry.file.strip_prefix("horizon-mods/") else {
        return Err(format!("{} isn't in {MODS_DIR}/.", entry.file));
    };
    match fs::remove_dir_all(home.join(&entry.file)) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", entry.file)),
    }
    set_enabled(home, id, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_on_list_keeps_other_lines() {
        let home = std::env::temp_dir().join(format!("horizon-java-mods-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(MODS_LIST),
            "mod.coords_hud:true\nmod.ruby:false\n",
        )
        .unwrap();
        set_enabled(&home, "ruby", true).unwrap();
        set_enabled(&home, "gem", true).unwrap();
        assert_eq!(
            fs::read_to_string(home.join(MODS_LIST)).unwrap(),
            "mod.coords_hud:true\nmod.ruby:true\nmod.gem:true\n"
        );
        set_enabled(&home, "ruby", false).unwrap();
        set_enabled(&home, "gem", false).unwrap();
        assert_eq!(
            fs::read_to_string(home.join(MODS_LIST)).unwrap(),
            "mod.coords_hud:true\n"
        );
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn native_probe_failure_is_bounded_and_explains_jvm_fallback() {
        let check = Check {
            mod_id: "demo".into(),
            name: "Demo".into(),
            version: "1".into(),
            loader: "fabric".into(),
            license: "MIT".into(),
            verdict: Verdict::Works,
            summary: "Content ports cleanly.".into(),
            ports: Vec::new(),
            skipped: Vec::new(),
            depends: Vec::new(),
            namespaces: vec!["demo".into()],
            code: Default::default(),
            native: Some(horizon_port_pack::check::NativeProbe {
                kind: Some("main".into()),
                entrypoint: Some("demo.Main".into()),
                entrypoints: vec!["demo.Main".into()],
                seeded_mods: vec!["demo".into()],
                calls: 0,
                registrations: 0,
                listeners: 0,
                payloads: 0,
                error: Some("unsupported operation ".to_owned() + &"x".repeat(400)),
            }),
        };
        let Support::Partial(reason) = support_of(&check) else {
            panic!("probe failures should be visible in the browser verdict")
        };
        assert!(reason.starts_with("Content ports cleanly. Java compatibility probe stopped (main) for demo.Main after 0 host calls;"));
        assert!(reason.ends_with('…'));
        assert!(reason.chars().count() < 400);
    }

    #[test]
    fn checks_are_kept_by_checksum() {
        let home = Path::new("/h");
        let file = |sha512: Option<&str>, sha1: Option<&str>| FileRef {
            url: String::new(),
            name: "a.jar".into(),
            size: 1,
            sha512: sha512.map(str::to_owned),
            sha256: None,
            sha1: sha1.map(str::to_owned),
            primary: true,
        };
        let long = "ab".repeat(64);
        assert_eq!(
            checked_dir(home, &file(Some(&long), None)).unwrap(),
            home.join(CHECKS).join("ab".repeat(20))
        );
        assert!(checked_dir(home, &file(None, Some("../../etc"))).is_none());
        assert!(checked_dir(home, &file(None, None)).is_none());
    }

    /// The report is reused only when the record beside it still describes
    /// itself. A folder copied under another jar's checksum, or a record edited
    /// by hand, is checked again; a folder written before records existed keeps
    /// the answer it has.
    #[test]
    fn a_cached_check_is_reused_only_with_a_record_that_describes_it() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("java-provenance-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let file = FileRef {
            url: String::new(),
            name: "a.jar".into(),
            size: 1,
            sha512: Some("ab".repeat(64)),
            sha256: None,
            sha1: None,
            primary: true,
        };
        let dir = checked_dir(&home, &file).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let check = Check {
            mod_id: "demo".into(),
            name: "Demo".into(),
            version: "1".into(),
            loader: "fabric".into(),
            license: "MIT".into(),
            verdict: Verdict::Works,
            summary: "Content ports cleanly.".into(),
            ports: Vec::new(),
            skipped: Vec::new(),
            depends: Vec::new(),
            namespaces: vec!["demo".into()],
            code: Default::default(),
            native: None,
        };
        fs::write(dir.join("check.json"), serde_json::to_vec(&check).unwrap()).unwrap();

        // Written before records existed: the old behaviour.
        assert_eq!(cached(&home, &file), Some(check.clone()));

        let record = |abi: u32, output: &str| {
            java_artifact::Provenance::new(
                "check",
                java_artifact::Format::new(java_artifact::Abi {
                    game: "26.3".into(),
                    horizon_mod_abi: abi,
                    native_hooks: None,
                }),
                [java_artifact::Source::of_bytes("jar", "a.jar", b"zip")],
                output,
            )
        };
        let consistent = record(7, "check.json");
        fs::write(
            dir.join("check.provenance.json"),
            serde_json::to_vec(&consistent).unwrap(),
        )
        .unwrap();
        assert_eq!(cached(&home, &file), Some(check.clone()));

        // The record's key no longer matches its own contents.
        let mut edited = consistent.clone();
        edited.output = "other.json".into();
        fs::write(
            dir.join("check.provenance.json"),
            serde_json::to_vec(&edited).unwrap(),
        )
        .unwrap();
        assert_eq!(cached(&home, &file), None);

        // Unreadable, and a record of a future format.
        for text in ["{", r#"{"report":"check","future":1}"#] {
            fs::write(dir.join("check.provenance.json"), text).unwrap();
            assert_eq!(cached(&home, &file), None, "{text}");
        }
        fs::remove_dir_all(&home).unwrap();
    }

    fn demo_check(native: Option<horizon_port_pack::check::NativeProbe>) -> Check {
        Check {
            mod_id: "demo".into(),
            name: "Demo".into(),
            version: "1".into(),
            loader: "fabric".into(),
            license: "MIT".into(),
            verdict: Verdict::Works,
            summary: "Content ports cleanly.".into(),
            ports: Vec::new(),
            skipped: Vec::new(),
            depends: Vec::new(),
            namespaces: vec!["demo".into()],
            code: Default::default(),
            native,
        }
    }

    fn probe(error: Option<&str>) -> horizon_port_pack::check::NativeProbe {
        horizon_port_pack::check::NativeProbe {
            kind: Some("main".into()),
            entrypoint: Some("demo.Main".into()),
            entrypoints: vec!["a.A".into(), "b.B".into(), "c.C".into(), "d.D".into()],
            seeded_mods: vec!["demo".into()],
            calls: 12,
            registrations: 3,
            listeners: 1,
            payloads: 0,
            error: error.map(str::to_owned),
        }
    }

    #[test]
    fn diagnostics_report_a_clean_probe() {
        let d = diagnostics_in(Language::English, &demo_check(Some(probe(None))));
        assert_eq!(d.verdict, "Works");
        assert!(!d.failed);
        assert_eq!(
            d.lines,
            [
                "Loader: Fabric",
                "Probe: Works",
                "Entrypoint: main · a.A, b.B (+2 more)",
                "Host calls: 12 · registrations: 3 · listeners: 1 · payloads: 0",
            ]
        );
        assert_eq!(
            diagnostics_in(Language::English, &demo_check(None)).verdict,
            "Not probed"
        );
    }

    #[test]
    fn diagnostics_bound_the_failure_and_warn_about_mixins() {
        let mut check = demo_check(Some(probe(Some(&format!("boom\n{}", "x".repeat(500))))));
        check.code.features.insert(Feature::Mixin, 2);
        let d = diagnostics_in(Language::English, &check);
        assert_eq!((d.verdict, d.failed), ("Partial", true));
        assert!(d
            .lines
            .iter()
            .any(|l| l.starts_with("Mixins: 2 classes not applied")));
        let stopped = d.lines.last().unwrap();
        assert!(stopped.starts_with("Stopped: boom x"));
        assert!(!stopped.contains('\n'));
        assert!(stopped.chars().count() < 320, "{}", stopped.chars().count());
    }

    /// The panel's body and badge are said in the launcher's language, and the `{name}` holes
    /// translate with their line: a translated string keeps the values the probe found.
    #[test]
    fn diagnostics_are_said_in_the_launchers_language() {
        let check = demo_check(Some(probe(None)));
        let german = diagnostics_in(Language::German, &check);
        assert_eq!(german.lines[0], "Lader: Fabric");
        assert_eq!(german.lines[1], "Prüfung: Funktioniert");
        assert_eq!(
            german.lines[2],
            "Einstiegspunkt: main · a.A, b.B (+2 weitere)"
        );
        assert_eq!(
            german.lines[3],
            "Host-Aufrufe: 12 · Registrierungen: 3 · Listener: 1 · Nutzlasten: 0"
        );
        for line in &german.lines {
            assert!(!line.contains(['{', '}']), "unfilled hole: {line}");
        }

        // The badge is drawn through `tr`, so the same verdict key reaches the player.
        assert_eq!(
            i18n::tr_in(Language::German, german.verdict),
            "Funktioniert"
        );
        assert_eq!(i18n::tr_in(Language::German, "Not probed"), "Nicht geprüft");

        // Singular and plural Mixins are separate sentences, each keeping its count.
        let mut one = demo_check(Some(probe(None)));
        one.code.features.insert(Feature::Mixin, 1);
        let french = diagnostics_in(Language::French, &one);
        assert!(french
            .lines
            .iter()
            .any(|l| l == "Mixins : 1 classe non appliquée par la sonde native ; le comportement qu'elles modifient peut manquer"));

        // A stopped probe is still bounded after translation.
        let mut failed = demo_check(Some(probe(Some(&"é".repeat(500)))));
        failed.code.features.insert(Feature::Mixin, 1);
        let stopped = diagnostics_in(Language::French, &failed)
            .lines
            .pop()
            .unwrap();
        assert!(stopped.starts_with("Arrêté : é"), "{stopped}");
        assert!(stopped.ends_with(')'), "{stopped}");
        assert!(stopped.chars().count() < 320, "{}", stopped.chars().count());
    }

    #[test]
    fn runtime_report_matches_file_and_bounds_rendered_lines() {
        let report = serde_json::json!({
            "schema_version": 1,
            "jars": [{
                "file": "mods/demo.jar", "loader": "fabric", "engine": "native-qualified",
                "woven": 2, "skipped": 1, "first_skip_reason": "bad target",
                "registrations": 4, "opaque": 1, "payload_channels": ["play"],
                "isolation_failure": null
            }]
        });
        let lines = runtime_lines_in(Language::English, &report, "demo.jar").unwrap();
        assert!(lines[0].contains("native-qualified"));
        assert!(lines[1].contains("bad target"));
        assert!(runtime_lines_in(Language::English, &report, "other.jar").is_none());
        let stale = serde_json::json!({"schema_version": 2, "jars": []});
        assert!(runtime_summary_in(Language::English, &stale).is_some());
    }

    #[test]
    fn failed_and_mixin_heavy_mods_sort_first() {
        let clean = demo_check(Some(probe(None)));
        let failed = demo_check(Some(probe(Some("no"))));
        let mut mixed = demo_check(Some(probe(None)));
        mixed.code.features.insert(Feature::Mixin, 1);
        let mut order = [clean.clone(), mixed.clone(), failed.clone()];
        order.sort_by(most_relevant_first);
        assert_eq!(order, [failed, mixed, clean]);
    }

    /// Clearing one mod's check leaves the others (and the download scratch folder) alone, and
    /// the cache still answers for what remains.
    #[test]
    fn clearing_a_check_keeps_the_others() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("java-clear-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        let (a, b) = ("ab".repeat(64), "cd".repeat(64));
        let check = demo_check(None);
        for hex in [&a, &b] {
            let dir = checked_dir_of(&home, hex).unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("check.json"), serde_json::to_vec(&check).unwrap()).unwrap();
        }
        fs::create_dir_all(checks(&home).join(".downloads")).unwrap();
        let (key_a, key_b) = (format!("SHA-512:{a}"), format!("SHA-512:{b}"));
        assert_eq!(cached_for_entry(&home, &key_a), Some(check.clone()));

        assert_eq!(clear_check(&home, &key_a), Ok(true));
        assert_eq!(cached_for_entry(&home, &key_a), None);
        assert_eq!(cached_for_entry(&home, &key_b), Some(check));
        assert!(checks(&home).join(".downloads").exists());
        // Already gone: nothing to remove, and not an error.
        assert_eq!(clear_check(&home, &key_a), Ok(false));
        // A value that isn't a checksum never reaches the file system.
        assert!(clear_check(&home, "SHA-512:../..").is_err());
        assert!(checks(&home).exists());
        fs::remove_dir_all(&home).unwrap();
    }
}
