//! Home's Mods tab (docs/design-system.md, "Night"; plan R0, screens 5 and 6): every mod of the
//! selected instance, the browser's and those put in its folder by hand, with its version, where
//! it comes from, an update when the browser found one, and a switch. A Java instance turns a mod
//! off by renaming its jar (`.disabled`, as `java_instance` does); a Horizon instance's mods are
//! the registry's, switched as the game's Options > Mods does. Adding mods is the Mods page's.
//!
//! Each row shows the mod's own icon: Modrinth's for what the browser installed from it (the URLs
//! asked for in one request and kept beside the image cache, the images through the Mods page's
//! cache at the row's size), else the one its jar names, else a generic glyph.
use super::*;
use crate::plate::alpha;
use crate::shell::{c, Action};
use horizon_launcher::browser::{
    horizon, images, installed, java, modrinth, Kind as PackKind, SourceId,
};
use servers_page::Field;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use theme::night;

/// A turned-off jar's suffix, which the loaders don't read.
const OFF: &str = ".disabled";

/// Reading a jar's own metadata is bounded: at most this many archive entries are looked at
/// (for mixin configs), and each metadata file is read up to [`MAX_META_BYTES`]. No temp files.
const MAX_JAR_ENTRIES: usize = 64;
const MAX_META_BYTES: u64 = 256 * 1024;
/// One reported reason stays one bounded line.
const MAX_REASON: usize = 160;

/// The loader a jar's own metadata names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum LoaderKind {
    Fabric,
    NeoForge,
    /// The old `mods.toml` family, out of the Java runtime's scope (`docs/java-mod-compat.md`).
    Forge,
    #[default]
    Unknown,
}

impl LoaderKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            LoaderKind::Fabric => "Fabric",
            LoaderKind::NeoForge => "NeoForge",
            LoaderKind::Forge => "Forge",
            LoaderKind::Unknown => "Unknown",
        }
    }

    fn modern(self) -> bool {
        matches!(self, LoaderKind::Fabric | LoaderKind::NeoForge)
    }
}

/// A bounded hint about whether a jar might run under Horizon's Java support. It is read from
/// the jar's loader metadata and mixin markers alone: a hint, never a verdict. A jar without
/// mixins is not promised to work, and one with mixins is not promised to fail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum JavaHint {
    /// It declares mixins or is legacy Forge: trust only its own loader (native).
    NativeOnly,
    /// A Fabric/NeoForge jar without mixin configs: the Java fallback may load it.
    JvmFallback,
    /// No loader we can read, so no hint.
    #[default]
    Unknown,
}

impl JavaHint {
    pub(super) fn label(self) -> &'static str {
        match self {
            JavaHint::NativeOnly => "Needs its own loader",
            JavaHint::JvmFallback => "Java fallback may run it",
            JavaHint::Unknown => "Loader not identified",
        }
    }
}

/// The identity a jar declares for itself in `fabric.mod.json` or `mods.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct JavaMeta {
    pub(super) loader: LoaderKind,
    pub(super) id: Option<String>,
    pub(super) version: Option<String>,
    /// Whether the archive holds any mixin config.
    pub(super) mixins: bool,
}

impl JavaMeta {
    pub(super) fn hint(&self) -> JavaHint {
        match (self.loader, self.mixins) {
            (LoaderKind::Forge, _) => JavaHint::NativeOnly,
            (loader, _) if !loader.modern() => JavaHint::Unknown,
            (_, true) => JavaHint::NativeOnly,
            _ => JavaHint::JvmFallback,
        }
    }
}

/// A mod as the tab lists it.
#[derive(Clone, Default)]
pub(super) struct ModRow {
    /// Its file in the instance, as it is when on (`mods/lumen-0.6.2.jar`).
    file: String,
    name: String,
    version: String,
    on: bool,
    /// What the browser knows of it; None: a file put in by hand.
    entry: Option<installed::Entry>,
    /// The icon its jar names, scaled to the row (`images::ROW`) and decoded once by the UI.
    icon: Option<Arc<gpui::Image>>,
    /// The jar's own loader metadata, when it declares any (a Java instance's jars).
    java: Option<JavaMeta>,
    /// Why its metadata couldn't be read, when it couldn't: one short reason.
    java_error: Option<String>,
}

impl InstanceMods {
    /// The rows' files with their jar metadata, for the Mods page's installed list.
    pub(super) fn java_rows(
        &self,
    ) -> impl Iterator<Item = (&str, Option<&JavaMeta>, Option<&str>)> {
        self.list
            .iter()
            .map(|r| (r.file.as_str(), r.java.as_ref(), r.java_error.as_deref()))
    }
}

/// Reads a jar's own loader metadata, bounded: the first [`MAX_JAR_ENTRIES`] entry names are
/// inspected for mixin configs, and each metadata file is read up to [`MAX_META_BYTES`]. The
/// archive is read in place, so no temporary files are made. `None` metadata means the jar
/// names no loader (a plain library) or its metadata was malformed, in which case `err` says
/// why in one short line.
pub(super) fn read_java_meta(path: &Path) -> (Option<JavaMeta>, Option<String>) {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) => return (None, Some(bound_reason(format!("{}: {e}", path.display())))),
    };
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(zip) => zip,
        Err(e) => return (None, Some(bound_reason(format!("{}: {e}", path.display())))),
    };
    let names: Vec<String> = zip
        .file_names()
        .take(MAX_JAR_ENTRIES)
        .map(str::to_owned)
        .collect();
    let entry_mixins = names.iter().any(|name| declares_mixin_entry(name));
    for (entry, loader) in [
        ("fabric.mod.json", LoaderKind::Fabric),
        ("META-INF/neoforge.mods.toml", LoaderKind::NeoForge),
        ("META-INF/mods.toml", LoaderKind::Forge),
    ] {
        match read_entry_text(&mut zip, entry) {
            Ok(Some(text)) => {
                let parsed = match loader {
                    LoaderKind::Fabric => horizon_port_pack::jar::fabric(&text),
                    _ => horizon_port_pack::jar::mods_toml(&text),
                };
                return match parsed {
                    Ok(info) => (
                        Some(JavaMeta {
                            loader,
                            id: non_empty(info.mod_id),
                            version: non_empty(info.version),
                            mixins: entry_mixins,
                        }),
                        None,
                    ),
                    Err(e) => (None, Some(bound_reason(e))),
                };
            }
            Ok(None) => {}
            Err(e) => return (None, Some(bound_reason(e))),
        }
    }
    (None, None)
}

/// One metadata entry's text, bounded by size.
fn read_entry_text<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, String> {
    let Some(index) = zip.index_for_name(name) else {
        return Ok(None);
    };
    let entry = zip.by_index(index).map_err(|e| format!("{name}: {e}"))?;
    if entry.size() > MAX_META_BYTES {
        return Err(format!("{name} exceeds {MAX_META_BYTES} bytes"));
    }
    let mut text = String::new();
    entry
        .take(MAX_META_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{name}: {e}"))?;
    if text.len() as u64 > MAX_META_BYTES {
        return Err(format!("{name} exceeds {MAX_META_BYTES} bytes"));
    }
    Ok(Some(text))
}

/// A declared field as `Some`, unless it is empty.
fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Mixin configs are the root `*.mixins.json` files and plain root entries naming mixins.
fn declares_mixin_entry(name: &str) -> bool {
    name.ends_with(".mixins.json") || (!name.contains('/') && name.contains("mixins"))
}

fn bound_reason(reason: String) -> String {
    reason
        .replace(['\r', '\n'], " ")
        .chars()
        .take(MAX_REASON)
        .collect()
}

/// A hint's colour: the warning tone only where it says the jar needs its own loader.
fn hint_color(hint: JavaHint, n: &night::Night) -> u32 {
    match hint {
        JavaHint::NativeOnly => n.warn,
        _ => n.muted,
    }
}

/// Modrinth projects' icon URLs by project id, kept on disk beside the image cache so the icons
/// show offline too; each id is asked for once a session.
#[derive(Default)]
pub(super) struct ProjectIcons {
    urls: HashMap<String, String>,
    loaded: bool,
    asked: HashSet<String>,
}

fn project_icons_file() -> PathBuf {
    backend::data_dir().join("mod-browser/project-icons.json")
}

#[derive(Default)]
pub(super) struct InstanceMods {
    home: Option<PathBuf>,
    fresh: bool,
    reading: bool,
    list: Vec<ModRow>,
    /// The browser was installing at the last draw: its end changes the list.
    was_busy: bool,
    /// Only the rows near the view are built (a modpack has hundreds), measured again when the
    /// shown list changes (its length and the filter).
    cull: cull::Cull,
    shown: (usize, String),
}

/// The mods of the instance at `home`: a Java one's jars in `mods/` (on or off), a Horizon one's
/// registry mods. Each jar names itself (`fabric.mod.json`, `quilt.mod.json`...), else its file.
fn read(home: &Path, java_instance: bool) -> Vec<ModRow> {
    let entries: Vec<installed::Entry> = installed::Installed::load(home)
        .entries
        .into_iter()
        .filter(|e| e.kind == PackKind::Mod)
        .collect();
    let mut rows: Vec<ModRow> = if java_instance {
        let Ok(dir) = std::fs::read_dir(home.join("mods")) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|f| {
                let name = f.file_name().to_string_lossy().into_owned();
                let on = name.ends_with(".jar");
                let jar = name.strip_suffix(OFF).unwrap_or(&name);
                (jar.ends_with(".jar") && f.path().is_file()).then(|| {
                    let file = format!("mods/{jar}");
                    let entry = entries.iter().find(|e| e.file == file).cloned();
                    let (name, version) = match &entry {
                        Some(e) => (e.title.clone(), e.number.clone()),
                        None => horizon_port_pack::jar::read(&f.path())
                            .map(|j| (j.name, j.version))
                            .unwrap_or_else(|_| {
                                (jar.trim_end_matches(".jar").into(), String::new())
                            }),
                    };
                    let icon = images::jar_icon(&f.path(), images::ROW)
                        .map(|png| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png)));
                    let (java, java_error) = read_java_meta(&f.path());
                    ModRow {
                        file,
                        name,
                        version,
                        on,
                        entry,
                        icon,
                        java,
                        java_error,
                    }
                })
            })
            .collect()
    } else {
        entries
            .into_iter()
            .map(|e| ModRow {
                file: e.file.clone(),
                name: e.title.clone(),
                version: e.number.clone(),
                on: horizon::mod_id(&e).is_some_and(|id| java::is_enabled(home, id)),
                entry: Some(e),
                icon: None,
                java: None,
                java_error: None,
            })
            .collect()
    };
    rows.sort_by_key(|r| r.name.to_lowercase());
    rows
}

/// Turns a Java instance's jar on or off by renaming it.
fn switch_jar(home: &Path, file: &str, on: bool) -> Result<(), String> {
    let (jar, off) = (home.join(file), home.join(format!("{file}{OFF}")));
    let (from, to) = if on { (off, jar) } else { (jar, off) };
    std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", from.display()))
}

impl Launcher {
    /// Whether the selected instance has mods to show: Horizon's always, a Java one with a loader.
    pub(super) fn has_mods_tab(&self) -> bool {
        self.selected_instance()
            .is_some_and(|i| i.java.as_ref().is_none_or(|j| j.loader.is_some()))
    }

    /// Reads the selected instance's mods off the UI thread, once until something changes them.
    /// The Mods page (`mods_page.rs`) borrows this list for installed jars' metadata too.
    pub(super) fn load_instance_mods(&mut self, cx: &mut Context<Self>) {
        let Some(i) = self.selected_instance() else {
            return;
        };
        let (home, java_instance) = (i.home.clone(), i.java.is_some());
        let m = &mut self.instance_mods;
        if m.home.as_ref() != Some(&home) {
            *m = InstanceMods {
                home: Some(home.clone()),
                ..InstanceMods::default()
            };
        }
        let busy = self.mods.busy.is_some();
        if m.was_busy && !busy {
            m.fresh = false;
        }
        m.was_busy = busy;
        if m.fresh || m.reading {
            return;
        }
        m.reading = true;
        let task = cx
            .background_executor()
            .spawn(async move { (home.clone(), read(&home, java_instance)) });
        cx.spawn(async move |this, cx| {
            let (home, list) = task.await;
            let _ = this.update(cx, |this, cx| {
                let m = &mut this.instance_mods;
                m.reading = false;
                if m.home.as_ref() == Some(&home) {
                    m.list = list;
                    m.fresh = true;
                }
                let ids = this
                    .instance_mods
                    .list
                    .iter()
                    .filter_map(|r| r.entry.as_ref())
                    .filter(|e| e.source == SourceId::Modrinth)
                    .map(|e| e.project.clone())
                    .collect();
                this.ask_project_icons(ids, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks Modrinth, in one request, for the icons of `ids` it hasn't been asked for.
    pub(super) fn ask_project_icons(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let icons = &mut self.project_icons;
        if !icons.loaded {
            icons.loaded = true;
            icons.urls = std::fs::read(project_icons_file())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
        }
        let todo: Vec<String> = ids
            .into_iter()
            .filter(|id| !icons.urls.contains_key(id) && icons.asked.insert(id.clone()))
            .collect();
        if todo.is_empty() {
            return;
        }
        let task = cx
            .background_executor()
            .spawn(async move { modrinth::icons(&todo) });
        cx.spawn(async move |this, cx| {
            let Ok(found) = task.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let urls = &mut this.project_icons.urls;
                urls.extend(found);
                if let Ok(json) = serde_json::to_vec(urls) {
                    let file = project_icons_file();
                    let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
                    let _ = std::fs::write(file, json);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The image of a Modrinth project's icon at a row's size, once its URL and image are here.
    pub(super) fn project_icon(&mut self, e: &installed::Entry) -> Option<PathBuf> {
        let url = (e.source == SourceId::Modrinth)
            .then(|| self.project_icons.urls.get(&e.project).cloned())
            .flatten()?;
        self.mods_image(&url, images::ROW)
    }

    /// A mod's icon at the row's size: Modrinth's, else its jar's, else a glyph.
    pub(super) fn mod_icon(
        &mut self,
        entry: Option<&installed::Entry>,
        jar: Option<Arc<gpui::Image>>,
        side: f32,
    ) -> Div {
        let n = self.theme.night;
        let remote = entry.and_then(|e| self.project_icon(e));
        let tile = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(side))
            .rounded(px(night::RADIUS_CONTROL))
            .overflow_hidden();
        match (remote, jar) {
            (Some(path), _) => tile.child(gpui::img(path).size(px(side))),
            (None, Some(image)) => tile.child(gpui::img(image).size(px(side))),
            (None, None) => tile
                .bg(alpha(n.card_hi, n.card_hi_alpha))
                .child(blocks::icon("box", side * 0.5, c(n.muted))),
        }
    }

    /// How many mods the selected instance has; None while they are read.
    pub(super) fn instance_mods_count(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        self.mods_load();
        self.load_instance_mods(cx);
        let m = &self.instance_mods;
        m.fresh.then_some(m.list.len())
    }

    fn switch_mod(&mut self, row: &ModRow) {
        let home = self.instance_mods.home.clone().unwrap_or_default();
        let java_instance = self.selected_instance().is_some_and(|i| i.java.is_some());
        match &row.entry {
            Some(e) if !java_instance => self.mods_switch(e),
            _ => {
                self.mods.message = switch_jar(&home, &row.file, !row.on).err().map(Err);
            }
        }
        self.instance_mods.fresh = false;
    }

    /// The Mods tab: its buttons (beside the tabs) and the list.
    pub(super) fn instance_mods_tab(&mut self, cx: &mut Context<Self>) -> (Div, Div) {
        let t = self.theme;
        let n = t.night;
        let busy = self.mods.busy.is_some();
        let updates = self.mods.updates.len();
        let actions = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .children((updates > 0 && !busy).then(|| {
                shell::action("instance-mods-update-all", Action::Raised, &t)
                    .child(shell::label(trf(
                        "Update all ({count})",
                        &[("count", &updates.to_string())],
                    )))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.mods_update_all();
                        cx.notify();
                    }))
            }))
            .child(
                shell::action("instance-mods-add", Action::Raised, &t)
                    .child(shell::label(tr("Add mods").to_string()))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.go(Page::Mods);
                        cx.notify();
                    })),
            );
        let loader = self
            .selected_instance()
            .and_then(|i| i.java.as_ref())
            .map(|j| j.loader.as_ref().map(|l| l.kind.name()));
        let title = match loader {
            Some(Some(loader)) => trf("{loader} mods", &[("loader", loader)]),
            _ => tr("Horizon mods").to_string(),
        };
        let filter = self
            .servers
            .value(Field::InstanceModsFilter)
            .trim()
            .to_lowercase();
        let field = self.field(Field::InstanceModsFilter, "Filter installed", 240., cx);
        let m = &self.instance_mods;
        let rows: Vec<ModRow> = m
            .list
            .iter()
            .filter(|r| filter.is_empty() || r.name.to_lowercase().contains(&filter))
            .cloned()
            .collect();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .px(px(18.))
            .py(px(14.))
            .child(
                shell::text(night::UI, c(n.ink))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(field);
        let empty = |s: &str| {
            shell::text(night::UI, c(n.muted))
                .px(px(18.))
                .py(px(16.))
                .child(tr(s).to_string())
        };
        let rule = |row: AnyElement| {
            div()
                .border_t_1()
                .border_color(alpha(n.stroke, n.rule_alpha))
                .child(row)
        };
        let body = if !m.fresh {
            rule(empty("Reading the mods…").into_any_element())
        } else if m.list.is_empty() {
            rule(empty("No mods yet: Add mods finds some for this instance.").into_any_element())
        } else if rows.is_empty() {
            rule(empty("No mod matches.").into_any_element())
        } else {
            let shown = (rows.len(), filter);
            if self.instance_mods.shown != shown {
                self.instance_mods.shown = shown;
                self.instance_mods.cull.reset();
            }
            let (cull, scroll) = (
                self.instance_mods.cull.clone(),
                self.content_scroll.handle.clone(),
            );
            cull.column(
                &scroll,
                rows.len(),
                0.,
                |_| false,
                |i| rule(self.mod_row(rows[i].clone(), busy, cx).into_any_element()),
            )
        };
        let card = plate::cut(
            night::PIXEL_CORNER,
            2,
            // Nearly opaque: a list reads over the sunset.
            Some(alpha(n.card, 0.94)),
            Some((alpha(n.stroke, n.stroke_alpha), 1.)),
        )
        .flex()
        .flex_col()
        .child(header)
        .child(body);
        (
            actions,
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .children(self.mods_busy_plate())
                .child(card),
        )
    }

    fn mod_row(&mut self, row: ModRow, busy: bool, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let n = t.night;
        let icon = self.mod_icon(row.entry.as_ref(), row.icon.clone(), 40.);
        let source = match &row.entry {
            Some(e) if e.dependency => trf(
                "{source} · needed by another mod",
                &[("source", e.source.label())],
            ),
            Some(e) => e.source.label().to_string(),
            None => trf(
                "Local file · {file}",
                &[("file", row.file.trim_start_matches("mods/"))],
            ),
        };
        let update = row.entry.as_ref().and_then(|e| {
            let key = (e.source, e.project.clone());
            let number = self.mods.updates.get(&key)?.clone();
            let (source, id): (SourceId, String) = key;
            let chip = div()
                .id(SharedString::from(format!("mod-update-{id}")))
                .flex()
                .flex_none()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .rounded(px(night::RADIUS_CONTROL))
                .bg(alpha(n.warn, 0.16))
                .cursor_pointer()
                .hover(move |s| s.bg(alpha(n.warn, 0.26)))
                .child(
                    shell::text(night::CAPTION, c(n.warn))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(trf("Update {version}", &[("version", &number)])),
                );
            Some(match busy {
                true => chip.opacity(theme::DISABLED),
                false => chip.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.mods_install(source, id.clone(), None);
                    cx.notify();
                })),
            })
        });
        let switch_row = row.clone();
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .px(px(18.))
            .py(px(12.))
            .child(icon)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(6.))
                            .child(
                                shell::text(night::UI, c(n.ink))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(row.name.clone()),
                            )
                            .child(shell::text(night::CAPTION, c(n.muted)).child(row.version)),
                    )
                    .child(
                        shell::text(night::CAPTION, c(n.muted))
                            .truncate()
                            .child(source),
                    )
                    .children(row.java.as_ref().map(|meta| {
                        let hint = meta.hint();
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                shell::text(night::CAPTION, c(n.muted))
                                    .truncate()
                                    .child(mods_page::java_identity(meta)),
                            )
                            .child(
                                shell::text(night::CAPTION, c(hint_color(hint, &n)))
                                    .child(tr(hint.label())),
                            )
                    }))
                    .children(row.java_error.as_ref().map(|reason| {
                        shell::text(night::CAPTION, c(n.muted))
                            .truncate()
                            .child(trf("Not checked: {error}", &[("error", reason)]))
                    })),
            )
            .children(update)
            .child(
                switch(format!("mod-switch-{}", row.file), row.on, &t).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.switch_mod(&switch_row);
                        cx.notify();
                    },
                )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A jar with `entries`, built at test time; the metadata reader never needs the network.
    fn jar(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in entries {
            zip.start_file(*name, options).unwrap();
            zip.write_all(body).unwrap();
        }
        zip.finish().unwrap();
    }

    fn temp(name: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("instance-mods-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("mods")).unwrap();
        home
    }

    #[test]
    fn reads_fabric_identity_mixins_and_hint() {
        let home = temp("fabric");
        let lumen = home.join("mods/lumen.jar");
        jar(
            &lumen,
            &[
                ("fabric.mod.json", br#"{"id":"lumen","version":"0.6.2"}"#),
                ("lumen.mixins.json", b"{}"),
            ],
        );
        let (meta, error) = read_java_meta(&lumen);
        let meta = meta.unwrap();
        assert!(error.is_none());
        assert_eq!(
            (
                meta.loader,
                meta.id.as_deref(),
                meta.version.as_deref(),
                meta.hint()
            ),
            (
                LoaderKind::Fabric,
                Some("lumen"),
                Some("0.6.2"),
                JavaHint::NativeOnly
            )
        );

        let plain = home.join("mods/plain.jar");
        jar(
            &plain,
            &[("fabric.mod.json", br#"{"id":"plain","version":"1.0"}"#)],
        );
        let meta = read_java_meta(&plain).0.unwrap();
        assert!(!meta.mixins);
        assert_eq!(meta.hint(), JavaHint::JvmFallback);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn reads_neoforge_identity_and_hint() {
        let home = temp("neoforge");
        let ruby = home.join("mods/ruby.jar");
        jar(
            &ruby,
            &[(
                "META-INF/neoforge.mods.toml",
                br#"[[mods]]
modId = "ruby"
version = "1.2.0"
"#,
            )],
        );
        let meta = read_java_meta(&ruby).0.unwrap();
        assert_eq!(
            (
                meta.loader,
                meta.id.as_deref(),
                meta.version.as_deref(),
                meta.hint()
            ),
            (
                LoaderKind::NeoForge,
                Some("ruby"),
                Some("1.2.0"),
                JavaHint::JvmFallback
            )
        );

        let shaded = home.join("mods/shaded.jar");
        jar(
            &shaded,
            &[
                (
                    "META-INF/neoforge.mods.toml",
                    br#"[[mods]]
modId = "shaded"
version = "2.0"
[[mixins]]
config = "shaded.mixins.json"
"#,
                ),
                ("shaded.mixins.json", b"{}"),
            ],
        );
        let meta = read_java_meta(&shaded).0.unwrap();
        assert!(meta.mixins);
        assert_eq!(meta.hint(), JavaHint::NativeOnly);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_malformed_jar_keeps_its_row_with_a_reason() {
        let home = temp("malformed");
        jar(
            &home.join("mods/broken.jar"),
            &[("fabric.mod.json", b"{not json")],
        );
        std::fs::write(home.join("mods/plain.jar"), b"nope").unwrap();
        let rows = read(&home, true);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.on));

        let broken = rows
            .iter()
            .find(|r| r.file.ends_with("broken.jar"))
            .unwrap();
        assert!(broken.java.is_none());
        assert!(broken
            .java_error
            .as_deref()
            .unwrap()
            .contains("fabric.mod.json"));

        let not_zip = rows.iter().find(|r| r.file.ends_with("plain.jar")).unwrap();
        assert!(not_zip.java.is_none());
        assert!(not_zip.java_error.is_some());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_java_instance_lists_its_jars_on_and_off() {
        let home = std::env::temp_dir().join(format!("instance-mods-{}", std::process::id()));
        let mods = home.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(mods.join("zeta-1.0.jar"), b"not a zip").unwrap();
        std::fs::write(mods.join("alpha-2.0.jar.disabled"), b"not a zip").unwrap();
        std::fs::write(mods.join("notes.txt"), b"").unwrap();
        let rows = read(&home, true);
        let seen: Vec<_> = rows.iter().map(|r| (r.name.as_str(), r.on)).collect();
        assert_eq!(seen, [("alpha-2.0", false), ("zeta-1.0", true)]);
        switch_jar(&home, "mods/alpha-2.0.jar", true).unwrap();
        assert!(read(&home, true).iter().all(|r| r.on));
        let _ = std::fs::remove_dir_all(&home);
    }
}
