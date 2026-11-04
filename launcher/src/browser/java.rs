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
use horizon_mods::java_artifact;
use horizon_port_pack::check::{self, Check, Verdict};
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

/// Adds the bounded native-probe result to the browser verdict. A probe failure does not
/// change what content can be carried, but it tells the player that Java compatibility will
/// need the embedded JVM path once that runtime is enabled.
fn compatibility_note(check: &Check) -> Option<String> {
    let native = check.native.as_ref()?;
    let error = native.error.as_deref()?;
    let mut detail = error.replace(['\n', '\r'], " ");
    const MAX_DETAIL: usize = 240;
    if detail.chars().count() > MAX_DETAIL {
        detail = detail.chars().take(MAX_DETAIL - 1).collect();
        detail.push('…');
    }
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

fn checks(home: &Path) -> PathBuf {
    home.join(CHECKS)
}

/// The folder a file's check is kept in, by its checksum.
fn checked_dir(home: &Path, file: &FileRef) -> Option<PathBuf> {
    let (_, hash) = file.checksum()?;
    let hash: String = hash
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(40)
        .collect();
    (hash.len() >= 32).then(|| checks(home).join(hash))
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
}
