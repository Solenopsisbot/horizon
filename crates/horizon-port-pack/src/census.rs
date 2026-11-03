//! The attachment surface of Java mod jars (`docs/java-mod-compat.md`,
//! phase 0): every class, member and Mixin point their code names, ranked, so
//! the compatibility façade is built in the order the ecosystem asks for.
//!
//! Class files only: nothing is decompiled, and no jar is run.
use crate::{check, classes, jar};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Packages whose use makes a class impossible to translate statically: the
/// closed world a Rust build needs cannot see through them.
const DYNAMIC_PACKAGES: [&str; 7] = [
    "java/lang/reflect/",
    "java/lang/invoke/",
    "javax/script/",
    "jdk/internal/",
    "sun/misc/",
    "sun/reflect/",
    "java/net/URLClassLoader",
];

/// JDK service discovery is runtime classpath input.  Keep it in the census
/// even when a class only names `ServiceLoader` (without a method reference),
/// so a static translation never silently drops an optional provider.
const SERVICE_LOADER: &str = "java/util/ServiceLoader";

/// `java.lang.Class` methods that mean reflection.
const DYNAMIC_CLASS_METHODS: [&str; 6] = [
    "forName",
    "getDeclared",
    "getMethod",
    "getConstructor",
    "getField",
    "newInstance",
];

/// Constants `javac` writes into every class that uses a lambda, a method
/// reference, string concatenation or a record. They name `java.lang.invoke`
/// without the class doing anything dynamic.
const INVOKE_SUGAR: [&str; 9] = [
    "java/lang/invoke/LambdaMetafactory",
    "java/lang/invoke/StringConcatFactory",
    "java/lang/invoke/MethodHandles",
    "java/lang/invoke/MethodHandles$Lookup",
    "java/lang/invoke/MethodType",
    "java/lang/invoke/MethodHandle",
    "java/lang/invoke/CallSite",
    "java/lang/invoke/TypeDescriptor",
    "java/lang/invoke/TypeDescriptor$OfField",
];

/// Mixin forms that need a hook at a vanilla method. `@Shadow`, `@Accessor`
/// and `@Invoker` need the member to exist, not a call site.
const NEEDS_HOOK: [&str; 12] = [
    "inject",
    "redirect",
    "wrap-operation",
    "wrap-with-condition",
    "modify-variable",
    "modify-arg",
    "modify-args",
    "modify-constant",
    "modify-expression-value",
    "modify-receiver",
    "modify-return-value",
    "overwrite",
];

/// Every jar's and class's attachment surface.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Census {
    pub totals: Totals,
    pub jars: Vec<JarCensus>,
    /// `net/minecraft/...` and friends: how many classes name it.
    pub classes: BTreeMap<String, usize>,
    /// `owner#name descriptor`: how many classes name the member.
    pub members: BTreeMap<String, usize>,
    /// Where a descriptor-bearing member reference is handled by the bridge.
    /// The descriptor remains in `members`; this aggregate gives the loader,
    /// façade and JDK work queues a stable split without losing overloads.
    #[serde(default)]
    pub member_disposition: BTreeMap<AbiDisposition, usize>,
    /// ABI member shape counts, including JVM constructors and static initializers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub member_shapes: BTreeMap<String, usize>,
    /// Loader event and capability types named by the mod classes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub api_refs: BTreeMap<String, usize>,
    /// Mixin target: how many mixins weave into it.
    pub mixin_targets: BTreeMap<String, usize>,
    /// Mixin target classes declared by discovered JSON configs (report only).
    #[serde(default)]
    pub mixin_config_targets: BTreeMap<String, usize>,
    /// `inject`, `redirect`, `overwrite`...: how many.
    pub injections: BTreeMap<String, usize>,
    /// Presence counts for Mixin injection ordering/contract metadata. Values
    /// are reported only; the census never applies these semantics.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_annotation_fields: BTreeMap<String, usize>,
    /// `HEAD`, `INVOKE|target:getBlockState`...: how many.
    pub at: BTreeMap<String, usize>,
    /// One row per (target, method, form, anchor), with the hooks at it.
    pub points: Vec<PointCount>,
    /// Injection points grouped by the native hook lookup key (`target#method`).
    /// Member-only Mixin points are omitted; each row retains its `@At` value,
    /// form and count so native hook translation need not scan `points`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hook_points: BTreeMap<String, Vec<HookPoint>>,
    /// Exact JVM descriptors retained for Mixin target methods. Hook lookup
    /// remains descriptor-free, while this map preserves overload identity
    /// for a future translator.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_method_descriptors: BTreeMap<String, Vec<String>>,
    /// Mixin points grouped by constructor versus ordinary target method.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_method_kinds: BTreeMap<String, usize>,
    /// Exact descriptor-bearing `@At(target:...)` selectors, keyed by the
    /// serialized `@At` value. This is report-only input for call-site
    /// translation planning; no bytecode is woven.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_at_targets: BTreeMap<String, Vec<String>>,
    /// Deterministic native-hook translation candidates. These plans preserve
    /// the injector mode and anchor for a later code generator; a `candidate`
    /// status means only that the target method has a matching native hook.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixin_translation_plans: Vec<MixinTranslationPlan>,
    /// Access-widener targets, counted across jars (discovery only).
    #[serde(default)]
    pub access_widener_targets: BTreeMap<String, usize>,
    /// Access-widener header namespaces, counted across jars. A namespace
    /// other than `named` needs a mapping table before native translation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub access_widener_namespaces: BTreeMap<String, usize>,
    /// Access-transformer targets, counted across jars (discovery only).
    #[serde(default)]
    pub access_transformer_targets: BTreeMap<String, usize>,
}

/// The first routing decision for a descriptor-bearing ABI reference.
///
/// `Facade` is a generated `net.minecraft` class, `Loader` is a Fabric or
/// NeoForge host contract, `Jdk` is handled by the native standard-library
/// runtime, and `External` needs a carried dependency or an explicit shim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AbiDisposition {
    Facade,
    Loader,
    Jdk,
    External,
}

pub fn member_disposition(member: &classes::Member) -> AbiDisposition {
    let owner = member.owner.as_str();
    // Horizon's generated façade covers Mojang's companion APIs as well as
    // the game namespace (Brigadier and DataFixer/Codec types are referenced
    // directly by real mod entrypoints).
    if owner.starts_with("net/minecraft/") || owner.starts_with("com/mojang/") {
        AbiDisposition::Facade
    } else if owner.starts_with("net/fabricmc/")
        || owner.starts_with("net/neoforged/")
        || owner.starts_with("net/minecraftforge/")
        // Mixin's annotations and callback metadata are supplied by the
        // loader/transform layer and are part of the generated façade
        // contract, rather than an external carried dependency.
        || owner.starts_with("org/spongepowered/asm/mixin/")
        // Mixin Extras is the companion injector library used by many Fabric
        // and NeoForge mods. Its annotations and callback contracts belong to
        // the same transform layer, so route them with Mixin rather than as a
        // carried external dependency.
        || owner.starts_with("com/llamalad7/mixinextras/")
        || owner.starts_with("org/quiltmc/")
    {
        AbiDisposition::Loader
    } else if owner.starts_with("java/")
        || owner.starts_with("javax/")
        || owner.starts_with("jdk/")
        || owner.starts_with("sun/")
    {
        AbiDisposition::Jdk
    } else {
        AbiDisposition::External
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Totals {
    pub jars: usize,
    pub classes: usize,
    pub references: usize,
    pub members: usize,
    pub mixins: usize,
    pub injections: usize,
    /// Jars with no reflection, dynamic loading or scripting: candidates for
    /// a static translation to Rust.
    pub static_jars: usize,
    /// Injection points that need a call site, after `@Shadow` and friends.
    pub points_needing_hooks: usize,
    /// Of those, how many land on a method a hook already names.
    pub points_covered: usize,
    /// Injection points for which no native hook is known. These remain
    /// report-only: the census never attempts to weave or emulate them.
    #[serde(default)]
    pub points_report_only: usize,
    /// Mixin targets found only by reading configuration JSON. These are
    /// report-only declarations, not proof that a target class was woven.
    #[serde(default)]
    pub mixin_config_targets_report_only: usize,
    /// `@Accessor`, `@Invoker` and `@Shadow` points that need a member shape,
    /// but no Rust call-site hook.
    pub member_only_points: usize,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct JarCensus {
    pub file: String,
    pub mod_id: String,
    pub name: String,
    pub version: String,
    pub loader: String,
    pub minecraft: String,
    /// Loader entrypoints: Fabric/Quilt metadata entries, or NeoForge classes
    /// carrying `@net.neoforged.fml.common.Mod`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entrypoints: Vec<crate::jar::Entrypoint>,
    pub classes: usize,
    /// Mixin configuration paths declared by this jar (entry names only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixin_config_names: Vec<String>,
    /// Bounded target classes read from this jar's Mixin config JSON (report only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mixin_config_targets: Vec<String>,
    /// Common Mixin config edge fields, keyed by config path (report only).
    /// Values are limited to the known metadata names; configs are never
    /// executed and these fields do not affect weaving.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_config_fields: BTreeMap<String, Vec<String>>,
    /// Scalar values for common Mixin config edge fields (report only).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mixin_config_values: BTreeMap<String, BTreeMap<String, String>>,
    /// Jars nested under `META-INF/jars/` that were read too.
    pub nested: usize,
    pub references: usize,
    pub members: usize,
    pub mixins: usize,
    pub injections: usize,
    /// Bounded Fabric access-widener targets found in this jar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_widener_targets: Vec<String>,
    /// Fabric access-widener namespace from its header (`named`,
    /// `intermediary`, or another bounded label). This is diagnostic only;
    /// the census does not apply mappings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_widener_namespace: Option<String>,
    /// Bounded NeoForge/Forge access-transformer targets found in this jar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access_transformer_targets: Vec<String>,
    pub static_code: bool,
    /// Why not, if not (up to eight).
    pub dynamic: Vec<String>,
    /// The metadata could not be read; the classes were still counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One attachment point: a Mixin weaving into `target#method`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PointCount {
    pub target: String,
    pub method: String,
    pub kind: String,
    pub at: String,
    pub count: usize,
    /// The hooks that name this method, if any.
    ///
    /// The explicit `native_hooks` JSON name distinguishes Horizon's native
    /// hook contract from Mixin metadata. `hooks` remains an input alias for
    /// census files written before this contract was named.
    #[serde(
        rename = "native_hooks",
        alias = "hooks",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hooks: Vec<String>,
}

/// A call-site injection under [`Census::hook_points`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HookPoint {
    pub kind: String,
    pub at: String,
    pub count: usize,
    /// Native hook names whose catalog `vanilla` contract matches this
    /// `target#method`. The old `hooks` spelling is accepted when reading.
    #[serde(
        rename = "native_hooks",
        alias = "hooks",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hooks: Vec<String>,
}

/// Report-only input for the native Mixin hook generator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MixinTranslationPlan {
    pub target: String,
    pub method: String,
    pub kind: String,
    pub at: String,
    /// Normalized operation family (`callback`, `redirect`, `modify-argument`,
    /// `modify-variable`, `replace-method`, or `unknown`).
    pub mode: String,
    /// `executable` means a bounded native operation can be emitted; `candidate` means a native hook matched but still needs codegen; `report-only` means it did not.
    pub status: String,
    pub count: usize,
    #[serde(
        rename = "native_hooks",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub hooks: Vec<String>,
    pub reason: String,
}

/// The hooks' vanilla methods, from `native-hooks.json`'s `vanilla` fields.
#[derive(Debug, Default, Clone)]
pub struct HookIndex {
    by_method: BTreeMap<(String, String), Vec<String>>,
}

#[derive(Deserialize)]
struct HooksFile {
    hooks: Vec<HookEntry>,
}

#[derive(Deserialize)]
struct HookEntry {
    name: String,
    #[serde(default)]
    vanilla: Option<String>,
}

impl HookIndex {
    pub fn read(path: &Path) -> Result<HookIndex, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: HooksFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let mut by_method = BTreeMap::new();
        for hook in file.hooks {
            let Some(vanilla) = hook.vanilla else {
                continue;
            };
            let Some((class, method)) = vanilla.split_once('#') else {
                continue;
            };
            by_method
                // Mixin metadata commonly includes a JVM descriptor while
                // the census point key is descriptor-free. Keep the catalog
                // and census on the same method contract so overloads do not
                // become false report-only points.
                .entry((class.to_owned(), strip_descriptor(method).to_owned()))
                .or_insert_with(Vec::new)
                .push(hook.name);
        }
        for names in by_method.values_mut() {
            names.sort();
            names.dedup();
        }
        Ok(HookIndex { by_method })
    }

    fn hooks_for(&self, class: &str, method: &str) -> Option<&Vec<String>> {
        self.by_method.get(&(class.to_owned(), method.to_owned()))
    }

    pub fn len(&self) -> usize {
        self.by_method.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_method.is_empty()
    }
}

/// Reads a jar, or every jar under a folder, and merges their censuses.
pub fn read(path: &Path) -> Result<Census, String> {
    read_with(path, None)
}

/// Reads and, given the hook catalog, joins every point against it.
pub fn read_with(path: &Path, hooks: Option<&HookIndex>) -> Result<Census, String> {
    let mut census = Census::default();
    let mut points: BTreeMap<(String, String, String, String), usize> = BTreeMap::new();
    for jar in jars(path)? {
        census.add(&jar, &mut points)?;
    }
    for descriptors in census.mixin_method_descriptors.values_mut() {
        descriptors.sort();
        descriptors.dedup();
    }
    for selectors in census.mixin_at_targets.values_mut() {
        selectors.sort();
        selectors.dedup();
    }
    census.points = points
        .into_iter()
        .map(|((target, method, kind, at), count)| PointCount {
            target,
            method,
            kind,
            at,
            count,
            hooks: Vec::new(),
        })
        .collect();
    // Always classify points. Without --hooks every call-site point is
    // explicitly report-only; no transform is attempted.
    let empty_hooks = HookIndex::default();
    census.join(hooks.unwrap_or(&empty_hooks));
    census.rebuild_hook_points();
    Ok(census)
}

fn jars(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "jar") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

impl Census {
    fn add(
        &mut self,
        path: &Path,
        points: &mut BTreeMap<(String, String, String, String), usize>,
    ) -> Result<(), String> {
        // A broken manifest is one jar's problem, not the run's: the classes
        // are still worth counting.
        let (info, error) = match jar::read(path) {
            Ok(info) => (info, None),
            Err(e) => (jar::JarInfo::default(), Some(e)),
        };
        let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
        let names: Vec<String> = zip.file_names().map(str::to_owned).collect();
        let loader = check::loader(&names).to_owned();
        let minecraft = minecraft_version(&mut zip, &loader).unwrap_or_default();
        let nested = names
            .iter()
            .filter(|n| n.starts_with("META-INF/jars/") && n.ends_with(".jar"))
            .take(64)
            .count();
        // Fabric's and NeoForge's jars-in-a-jar (JiJ): a bundler's classes are
        // its nested jars'.
        let parsed = classes::read_jar(path)?;
        let mut entrypoints = info.entrypoints.clone();
        if loader == "neoforge" {
            entrypoints.extend(neoforge_entrypoints(&parsed));
            entrypoints.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.class.cmp(&b.class)));
            entrypoints.dedup();
        }
        let mut jar = JarCensus {
            file: path.display().to_string(),
            mod_id: if info.mod_id.is_empty() {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                info.mod_id
            },
            name: info.name,
            version: info.version,
            loader,
            minecraft,
            entrypoints,
            classes: parsed.len(),
            mixin_config_names: jar::mixin_config_names(&names),
            mixin_config_targets: mixin_config_targets(&mut zip, &names),
            mixin_config_fields: mixin_config_fields(&mut zip, &names),
            mixin_config_values: mixin_config_values(&mut zip, &names),
            nested,
            error,
            ..JarCensus::default()
        };
        if let Some(path) = info.access_widener.as_deref() {
            if let Ok(text) = read_zip_text(&mut zip, path) {
                jar.access_widener_namespace = access_widener_namespace(&text);
                jar.access_widener_targets = access_widener_targets(&text);
            }
        }
        for name in names.iter().filter(|name| is_access_transformer(name)) {
            if let Ok(text) = read_zip_text(&mut zip, name) {
                jar.access_transformer_targets
                    .extend(access_transformer_targets(&text));
            }
        }
        jar.access_widener_targets.sort();
        jar.access_widener_targets.dedup();
        jar.access_transformer_targets.sort();
        jar.access_transformer_targets.dedup();
        for target in &jar.mixin_config_targets {
            *self.mixin_config_targets.entry(target.clone()).or_default() += 1;
        }
        self.totals.mixin_config_targets_report_only += jar.mixin_config_targets.len();
        for target in &jar.access_widener_targets {
            *self
                .access_widener_targets
                .entry(target.clone())
                .or_default() += 1;
        }
        if let Some(namespace) = &jar.access_widener_namespace {
            *self
                .access_widener_namespaces
                .entry(namespace.clone())
                .or_default() += 1;
        }
        for target in &jar.access_transformer_targets {
            *self
                .access_transformer_targets
                .entry(target.clone())
                .or_default() += 1;
        }
        for class in &parsed {
            for r in &class.refs {
                *self.classes.entry(r.clone()).or_default() += 1;
            }
            for m in &class.members {
                *self.members.entry(m.key()).or_default() += 1;
                *self
                    .member_disposition
                    .entry(member_disposition(m))
                    .or_default() += 1;
                *self.member_shapes.entry(member_shape(m)).or_default() += 1;
            }
            for reference in &class.refs {
                if let Some(kind) = api_reference_kind(reference) {
                    *self.api_refs.entry(kind.to_owned()).or_default() += 1;
                }
            }
            for target in &class.mixin_targets {
                *self.mixin_targets.entry(target.clone()).or_default() += 1;
            }
            for injection in &class.injections {
                *self.injections.entry(injection.kind.clone()).or_default() += 1;
                for field in &injection.metadata {
                    *self
                        .mixin_annotation_fields
                        .entry(field.clone())
                        .or_default() += 1;
                }
                if !injection.at.is_empty() {
                    *self.at.entry(injection.at.clone()).or_default() += 1;
                    let selectors = at_target_selectors(&injection.at)
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    if !selectors.is_empty() {
                        self.mixin_at_targets
                            .entry(injection.at.clone())
                            .or_default()
                            .extend(selectors);
                    }
                }
                // Mixin permits one annotation to name several target
                // methods. `classes::injections` keeps that source shape as
                // a `|`-joined string; expand it here so each target method
                // can match its own native hook instead of becoming one
                // permanently report-only `foo|bar` point.
                for target in &class.mixin_targets {
                    for method in injection.method.split('|') {
                        let Some(descriptor) = method_descriptor(method) else {
                            continue;
                        };
                        self.mixin_method_descriptors
                            .entry(format!("{}#{}", target, strip_descriptor(method)))
                            .or_default()
                            .push(descriptor.to_owned());
                    }
                }
                for method in injection_methods(&injection.method) {
                    for target in &class.mixin_targets {
                        *self
                            .mixin_method_kinds
                            .entry(mixin_method_kind(method).to_string())
                            .or_default() += 1;
                        *points
                            .entry((
                                target.clone(),
                                method.to_owned(),
                                injection.kind.clone(),
                                injection.at.clone(),
                            ))
                            .or_default() += 1;
                    }
                }
            }
            jar.references += class.refs.len();
            jar.members += class.members.len();
            jar.mixins += class.mixin_targets.len();
            jar.injections += class.injections.len();
            if let Some(reason) = dynamic_reason(class)
                && jar.dynamic.len() < 8
                && !jar.dynamic.contains(&reason)
            {
                jar.dynamic.push(reason);
            }
        }
        // A jar with no classes (a bundler of nested jars) is not code, so it
        // neither counts as static nor as dynamic.
        jar.static_code = jar.classes > 0 && jar.dynamic.is_empty();
        self.totals.jars += 1;
        self.totals.classes += jar.classes;
        self.totals.references += jar.references;
        self.totals.members += jar.members;
        self.totals.mixins += jar.mixins;
        self.totals.injections += jar.injections;
        self.totals.static_jars += usize::from(jar.static_code);
        self.jars.push(jar);
        Ok(())
    }

    /// Marks every point with the hooks at its method, and counts coverage.
    pub fn join(&mut self, hooks: &HookIndex) {
        self.totals.points_needing_hooks = 0;
        self.totals.points_covered = 0;
        self.totals.points_report_only = 0;
        self.totals.member_only_points = 0;
        for point in &mut self.points {
            if !NEEDS_HOOK.contains(&point.kind.as_str()) {
                self.totals.member_only_points += point.count;
                continue;
            }
            self.totals.points_needing_hooks += point.count;
            point.hooks = hooks
                .hooks_for(&point.target, &point.method)
                .cloned()
                .unwrap_or_default();
            if !point.hooks.is_empty() {
                self.totals.points_covered += point.count;
            } else {
                self.totals.points_report_only += point.count;
            }
        }
        self.rebuild_hook_points();
        self.rebuild_translation_plans();
    }

    fn rebuild_hook_points(&mut self) {
        self.hook_points.clear();
        for point in &self.points {
            if NEEDS_HOOK.contains(&point.kind.as_str()) {
                self.hook_points
                    .entry(format!("{}#{}", point.target, point.method))
                    .or_default()
                    .push(HookPoint {
                        kind: point.kind.clone(),
                        at: point.at.clone(),
                        count: point.count,
                        hooks: point.hooks.clone(),
                    });
            }
        }
    }

    fn rebuild_translation_plans(&mut self) {
        self.mixin_translation_plans.clear();
        for point in &self.points {
            if !NEEDS_HOOK.contains(&point.kind.as_str()) {
                continue;
            }
            let mode = match point.kind.as_str() {
                "inject" => "callback",
                "redirect" => "redirect",
                "modify-arg" | "modify-args" => "modify-argument",
                "modify-variable" => "modify-variable",
                "modify-constant" | "modify-expression-value" | "modify-return-value" => {
                    "modify-variable"
                }
                "modify-receiver" => "modify-receiver",
                "wrap-operation" | "wrap-with-condition" => "redirect",
                "overwrite" => "replace-method",
                _ => "unknown",
            }
            .to_owned();
            let matched = !point.hooks.is_empty();
            // A single-hook @Overwrite has no callback ordering, cancellation,
            // or call-site selector to translate. Emit it as a direct method
            // replacement operation; the runtime can bind the mod body to the
            // named native hook without interpreting bytecode at the target.
            let executable_overwrite = point.kind == "overwrite"
                && point.at.is_empty()
                && point.hooks.len() == 1;
            let (status, reason) = if executable_overwrite {
                (
                    "executable".to_owned(),
                    "single native hook supports bounded direct method replacement".to_owned(),
                )
            } else if matched {
                (
                    "candidate".to_owned(),
                    "native hook matched; callback codegen, ordering, and cancellation remain unimplemented"
                        .to_owned(),
                )
            } else {
                (
                    "report-only".to_owned(),
                    "no native hook matches the target method".to_owned(),
                )
            };
            self.mixin_translation_plans.push(MixinTranslationPlan {
                target: point.target.clone(),
                method: point.method.clone(),
                kind: point.kind.clone(),
                at: point.at.clone(),
                mode,
                status,
                count: point.count,
                hooks: point.hooks.clone(),
                reason,
            });
        }
    }

    /// The ranked surface, `top` rows of each, as the tool prints it.
    pub fn summary(&self, top: usize) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} jars, {} classes: {} class refs, {} member refs, {} mixins, {} injections\n",
            self.totals.jars,
            self.totals.classes,
            self.totals.references,
            self.totals.members,
            self.totals.mixins,
            self.totals.injections
        ));
        if self.totals.member_only_points > 0 {
            out.push_str(&format!(
                "member-only Mixin points (Accessor/Invoker/Shadow): {}\n",
                self.totals.member_only_points
            ));
        }
        let code = self.jars.iter().filter(|j| j.classes > 0).count();
        out.push_str(&format!(
            "no reflection or dynamic loading: {} of {} jars with code\n",
            self.totals.static_jars, code
        ));
        let broken = self.jars.iter().filter(|j| j.error.is_some()).count();
        if broken > 0 {
            out.push_str(&format!("{broken} jars with unreadable metadata\n"));
        }
        let widener_targets: usize = self.access_widener_targets.values().sum();
        let transformer_targets: usize = self.access_transformer_targets.values().sum();
        if widener_targets > 0 || transformer_targets > 0 {
            out.push_str(&format!(
                "access metadata: {widener_targets} access widener targets, {transformer_targets} access transformer targets (report-only; no bytecode changes)\n"
            ));
        }
        if !self.access_widener_namespaces.is_empty() {
            let namespaces = self
                .access_widener_namespaces
                .iter()
                .map(|(namespace, count)| format!("{namespace}={count}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "access widener namespaces: {namespaces} (report-only; mappings not applied)\n"
            ));
        }
        if self.totals.points_needing_hooks > 0 {
            let share =
                100.0 * self.totals.points_covered as f64 / self.totals.points_needing_hooks as f64;
            out.push_str(&format!(
                "hooks: {} of {} injection points at a hooked method ({share:.1}%)\n",
                self.totals.points_covered, self.totals.points_needing_hooks
            ));
            out.push_str(&format!(
                "mixin injection points: {} hook-covered, {} report-only (no weaving)\n",
                self.totals.points_covered, self.totals.points_report_only
            ));
        }
        if self.totals.mixin_config_targets_report_only > 0 {
            out.push_str(&format!(
                "mixin config targets: {} report-only (declarations only; no weaving)\n",
                self.totals.mixin_config_targets_report_only
            ));
        }
        if !self.mixin_annotation_fields.is_empty() {
            out.push_str("mixin annotation metadata (presence only; no weaving)\n");
            for (field, count) in &self.mixin_annotation_fields {
                out.push_str(&format!("{count:7} {field}\n"));
            }
        }
        for (title, map) in [
            ("classes", &self.classes),
            ("members", &self.members),
            ("mixin targets", &self.mixin_targets),
            ("mixin config targets", &self.mixin_config_targets),
            ("injections", &self.injections),
            ("at", &self.at),
            ("access widener targets", &self.access_widener_targets),
            (
                "access transformer targets",
                &self.access_transformer_targets,
            ),
        ] {
            out.push_str(&format!("\n{title}\n"));
            for (name, count) in ranked(map, top) {
                out.push_str(&format!("{count:7} {name}\n"));
            }
        }
        if !self.member_shapes.is_empty() {
            out.push_str("\nmember ABI shapes\n");
            for (shape, count) in &self.member_shapes {
                out.push_str(&format!("{count:7} {shape}\n"));
            }
        }
        if !self.api_refs.is_empty() {
            out.push_str("\nloader API references\n");
            for (kind, count) in &self.api_refs {
                out.push_str(&format!("{count:7} {kind}\n"));
            }
        }
        if !self.member_disposition.is_empty() {
            out.push_str("\nmember ABI disposition\n");
            for (disposition, count) in &self.member_disposition {
                out.push_str(&format!("{count:7} {disposition:?}\n"));
            }
        }
        let unhooked: Vec<&PointCount> = self
            .points
            .iter()
            .filter(|p| NEEDS_HOOK.contains(&p.kind.as_str()) && p.hooks.is_empty())
            .collect();
        if !unhooked.is_empty() {
            out.push_str("\nno hook yet\n");
            let mut rows: Vec<(&PointCount, usize)> =
                unhooked.iter().map(|p| (*p, p.count)).collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.target.cmp(&b.0.target)));
            for (point, count) in rows.into_iter().take(top) {
                out.push_str(&format!(
                    "{count:7} {}#{} {} {}\n",
                    point.target, point.method, point.kind, point.at
                ));
            }
        }
        out
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))
    }
}

fn member_shape(member: &classes::Member) -> String {
    if member.name == "<init>" {
        "constructor".to_owned()
    } else if member.name == "<clinit>" {
        "static-initializer".to_owned()
    } else {
        match member.kind {
            classes::MemberKind::Field => "field",
            classes::MemberKind::Method => "method",
            classes::MemberKind::InterfaceMethod => "interface-method",
        }
        .to_owned()
    }
}

fn mixin_method_kind(method: &str) -> &'static str {
    if method
        .split('|')
        .any(|m| m == "<init>" || m.starts_with("<init> ") || m.starts_with("<init>("))
    {
        "constructor"
    } else {
        "method"
    }
}

fn api_reference_kind(reference: &str) -> Option<&'static str> {
    if reference.starts_with("net/neoforged/neoforge/event/")
        || reference.starts_with("net/minecraftforge/event/")
        || reference.starts_with("net/fabricmc/fabric/api/event/")
        || reference.starts_with("net/fabricmc/fabric/api/entity/event/")
    {
        Some("event")
    } else if reference.starts_with("net/neoforged/neoforge/capabilities/")
        || reference.starts_with("net/minecraftforge/common/capabilities/")
    {
        Some("capability")
    } else {
        None
    }
}

const MAX_ACCESS_TARGETS: usize = 256;

fn read_zip_text(zip: &mut zip::ZipArchive<fs::File>, name: &str) -> Result<String, String> {
    let mut text = String::new();
    zip.by_name(name)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    Ok(text)
}

fn is_access_transformer(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with("accesstransformer.cfg")
}

const MAX_MIXIN_CONFIG_BYTES: u64 = 1 << 20;
const MAX_MIXIN_CONFIG_TARGETS: usize = 256;
const MAX_MIXIN_CONFIG_FIELDS: usize = 256;
const MIXIN_CONFIG_EDGE_FIELDS: [&str; 6] = [
    "plugin",
    "refmap",
    "required",
    "minVersion",
    "compatibilityLevel",
    "priority",
];
const MIXIN_CONFIG_INJECTOR_FIELDS: [&str; 3] = ["defaultRequire", "maxShiftBy", "injectionPoints"];

/// Reads only bounded JSON config entries and reports safe class names under
/// `target` keys. No Mixin semantics are applied and malformed/oversized
/// configs are ignored.
fn mixin_config_targets(zip: &mut zip::ZipArchive<fs::File>, names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for name in jar::mixin_config_names(names) {
        let Ok(mut entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_MIXIN_CONFIG_BYTES {
            continue;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        collect_mixin_targets(&value, &mut out);
        if out.len() >= MAX_MIXIN_CONFIG_TARGETS {
            break;
        }
    }
    out.sort();
    out.dedup();
    out.truncate(MAX_MIXIN_CONFIG_TARGETS);
    out
}

/// Reads a bounded set of common Mixin config edge fields. This intentionally
/// reports presence only: plugin callbacks and compatibility requirements are
/// not interpreted by the census.
fn mixin_config_fields(
    zip: &mut zip::ZipArchive<fs::File>,
    names: &[String],
) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for name in jar::mixin_config_names(names)
        .into_iter()
        .take(MAX_MIXIN_CONFIG_FIELDS)
    {
        let Ok(mut entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_MIXIN_CONFIG_BYTES {
            continue;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let Ok(serde_json::Value::Object(object)) = serde_json::from_str(&text) else {
            continue;
        };
        let mut fields: Vec<String> = MIXIN_CONFIG_EDGE_FIELDS
            .iter()
            .filter(|field| object.contains_key(**field))
            .map(|field| (*field).to_owned())
            .collect();
        if let Some(injectors) = object
            .get("injectors")
            .and_then(serde_json::Value::as_object)
        {
            fields.extend(
                MIXIN_CONFIG_INJECTOR_FIELDS
                    .iter()
                    .filter(|field| injectors.contains_key(**field))
                    .map(|field| format!("injectors.{field}")),
            );
        }
        if !fields.is_empty() {
            out.insert(name, fields);
        }
    }
    out
}

/// Reads scalar values for the common Mixin config edge fields. This is still
/// report-only: values are never interpreted as loader or weaving policy.
fn mixin_config_values(
    zip: &mut zip::ZipArchive<fs::File>,
    names: &[String],
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in jar::mixin_config_names(names)
        .into_iter()
        .take(MAX_MIXIN_CONFIG_FIELDS)
    {
        let Ok(mut entry) = zip.by_name(&name) else {
            continue;
        };
        if entry.size() > MAX_MIXIN_CONFIG_BYTES {
            continue;
        }
        let mut text = String::new();
        if entry.read_to_string(&mut text).is_err() {
            continue;
        }
        let Ok(serde_json::Value::Object(object)) = serde_json::from_str(&text) else {
            continue;
        };
        let mut values = MIXIN_CONFIG_EDGE_FIELDS
            .iter()
            .filter_map(|field| {
                object
                    .get(*field)
                    .and_then(config_scalar_text)
                    .map(|value| ((*field).to_owned(), value))
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(injectors) = object
            .get("injectors")
            .and_then(serde_json::Value::as_object)
        {
            for field in MIXIN_CONFIG_INJECTOR_FIELDS {
                if let Some(value) = injectors.get(field).and_then(config_scalar_text) {
                    values.insert(format!("injectors.{field}"), value);
                }
            }
        }
        if !values.is_empty() {
            out.insert(name, values);
        }
    }
    out
}

fn config_scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn collect_mixin_targets(value: &serde_json::Value, out: &mut Vec<String>) {
    if out.len() >= MAX_MIXIN_CONFIG_TARGETS {
        return;
    }
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(target)) = object.get("target") {
                let target = target.trim();
                if safe_mixin_target(target) {
                    out.push(target.to_owned());
                }
            }
            for value in object.values() {
                collect_mixin_targets(value, out);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_mixin_targets(value, out);
            }
        }
        _ => {}
    }
}

fn safe_mixin_target(target: &str) -> bool {
    target.len() <= 256
        && (target.contains('.') || target.contains('/'))
        && target
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.' | '/'))
}

/// Catalogs the target portion of Fabric's v1/v2 access-widener lines.
/// Modifiers and descriptors stay in the key so overloads remain distinct;
/// the parser never changes classfile access.
fn access_widener_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with("accessWidener ") {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else { continue };
        if !matches!(
            kind,
            "accessible"
                | "extendable"
                | "mutable"
                | "transitive-accessible"
                | "transitive-extendable"
                | "transitive-mutable"
        ) {
            continue;
        }
        let target = fields.collect::<Vec<_>>().join(" ");
        if target.is_empty() {
            continue;
        }
        out.push(format!("{kind} {target}"));
        if out.len() >= MAX_ACCESS_TARGETS {
            break;
        }
    }
    out
}

/// Reads the namespace from a Fabric access-widener header. The namespace is
/// needed to decide whether a target can be matched directly against the
/// generated named façade; no remapping is attempted here.
fn access_widener_namespace(text: &str) -> Option<String> {
    let mut fields = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("accessWidener "))?
        .split_whitespace();
    let magic = fields.next()?;
    let version = fields.next()?;
    let namespace = fields.next()?;
    if magic == "accessWidener"
        && version
            .strip_prefix('v')
            .is_some_and(|v| v.parse::<u32>().is_ok())
        && !namespace.is_empty()
        && namespace.len() <= 32
        && namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        Some(namespace.to_owned())
    } else {
        None
    }
}

/// Catalogs NeoForge/Forge AT lines after the access/final modifiers. This is
/// intentionally a report-only parser: no transformer semantics are applied.
fn access_transformer_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(access) = fields.next() else {
            continue;
        };
        if !matches!(
            access,
            "public"
                | "protected"
                | "private"
                | "default"
                | "public-f"
                | "protected-f"
                | "private-f"
        ) {
            continue;
        }
        let rest = fields.collect::<Vec<_>>().join(" ");
        if rest.is_empty() {
            continue;
        }
        out.push(rest);
        if out.len() >= MAX_ACCESS_TARGETS {
            break;
        }
    }
    out
}

/// NeoForge discovers a mod's bootstrap class from its `@Mod("id")`
/// annotation rather than from `neoforge.mods.toml`. Keep this in the census
/// so the JVM runner can select the class without loading the whole FML stack.
fn neoforge_entrypoints(parsed: &[classes::Class]) -> Vec<crate::jar::Entrypoint> {
    const MOD: &str = "Lnet/neoforged/fml/common/Mod;";
    let mut out = parsed
        .iter()
        .filter_map(|class| {
            let annotation = class.annotations.iter().find(|a| a.ty == MOD)?;
            match annotation.values.get("value") {
                Some(classes::Value::Str(id)) if !id.is_empty() => {}
                _ => return None,
            }
            Some(crate::jar::Entrypoint {
                kind: "neoforge".to_owned(),
                class: class.name.clone(),
                adapter: None,
            })
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.class.cmp(&b.class));
    out
}

/// `tick()V` and `tick` are the same target.
fn strip_descriptor(method: &str) -> &str {
    method.split(['(', ' ']).next().unwrap_or(method).trim()
}

fn method_descriptor(method: &str) -> Option<&str> {
    method.find('(').map(|at| &method[at..])
}

/// Extracts exact descriptor-bearing selectors from a serialized `@At` value.
/// Other selectors remain attached to the original `at` key and are not
/// interpreted here.
fn at_target_selectors(at: &str) -> impl Iterator<Item = &str> {
    at.split('|')
        .filter_map(|part| part.strip_prefix("target:"))
        .filter(|selector| !selector.is_empty())
}

/// Mixin's `method` element can be a scalar or an array. The class reader
/// preserves arrays as `|`-joined text so this boundary can expand them into
/// independently matchable native hook keys.
fn injection_methods(methods: &str) -> impl Iterator<Item = &str> {
    methods.split('|').map(strip_descriptor)
}

/// `count` first, then the name, so equal counts are stable.
pub fn ranked(map: &BTreeMap<String, usize>, top: usize) -> Vec<(&String, usize)> {
    let mut rows: Vec<(&String, usize)> = map.iter().map(|(k, v)| (k, *v)).collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.truncate(top);
    rows
}

/// What keeps this class out of a static translation, if anything.
fn dynamic_reason(class: &classes::Class) -> Option<String> {
    for r in &class.refs {
        // Compile-time sugar: lambdas, method references, string concatenation
        // and records, not reflection.
        if INVOKE_SUGAR.contains(&r.as_str()) {
            continue;
        }
        if r == SERVICE_LOADER {
            return Some(r.clone());
        }
        if DYNAMIC_PACKAGES.iter().any(|p| r.starts_with(p)) {
            return Some(r.clone());
        }
    }
    for m in &class.members {
        // The bootstrap methods above, not a real use of `java.lang.invoke`.
        if m.owner.starts_with("java/lang/invoke/")
            && !matches!(
                m.owner.as_str(),
                "java/lang/invoke/LambdaMetafactory" | "java/lang/invoke/StringConcatFactory"
            )
        {
            return Some(format!("{}#{}", m.owner, m.name));
        }
        if m.owner == "java/lang/Class"
            && DYNAMIC_CLASS_METHODS.iter().any(|n| m.name.starts_with(n))
        {
            if m.name == "forName" {
                return Some(
                    "java/lang/Class#forName (native current-jar only; JVM fallback for arbitrary loading)"
                        .to_owned(),
                );
            }
            return Some(format!("{}#{}", m.owner, m.name));
        }
        if m.owner == "java/lang/ClassLoader"
            && (m.name == "loadClass" || m.name.starts_with("getResource"))
        {
            return Some(format!("{}#{}", m.owner, m.name));
        }
        if m.owner == SERVICE_LOADER && matches!(m.name.as_str(), "load" | "loadInstalled") {
            return Some(format!("{}#{}", m.owner, m.name));
        }
    }
    None
}

/// The Minecraft version a jar says it is for.
fn minecraft_version(zip: &mut zip::ZipArchive<fs::File>, loader: &str) -> Option<String> {
    if loader == "fabric" || loader == "quilt" {
        let mut text = String::new();
        zip.by_name("fabric.mod.json")
            .ok()?
            .read_to_string(&mut text)
            .ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        return match json.get("depends")?.get("minecraft")? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Array(a) => Some(
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => None,
        };
    }
    let name = if loader == "neoforge" {
        "META-INF/neoforge.mods.toml"
    } else {
        "META-INF/mods.toml"
    };
    let mut text = String::new();
    zip.by_name(name).ok()?.read_to_string(&mut text).ok()?;
    let toml: toml::Value = text.parse().ok()?;
    for list in toml.get("dependencies")?.as_table()?.values() {
        let Some(list) = list.as_array() else {
            continue;
        };
        for dep in list {
            if dep.get("modId").and_then(|v| v.as_str()) == Some("minecraft") {
                return dep
                    .get("versionRange")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_safe_mixin_config_targets_only() {
        let dir = std::env::temp_dir().join("horizon-census-mixin-config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.jar");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("ruby.mixins.json", options).unwrap();
        std::io::Write::write_all(&mut zip, br#"{"plugin":"example.Plugin","refmap":"ruby.refmap.json","required":true,"minVersion":"0.8","compatibilityLevel":"JAVA_25","priority":1200,"injectors":{"defaultRequire":2,"maxShiftBy":3,"injectionPoints":["example.Point"]},"mixins":[{"target":"net.minecraft.Foo"},{"target":"bad target"}],"client":[{"target":"a/b/C"}]}"#).unwrap();
        zip.finish().unwrap();
        let census = read(&path).unwrap();
        assert_eq!(
            census.jars[0].mixin_config_targets,
            ["a/b/C", "net.minecraft.Foo"]
        );
        assert_eq!(
            census.mixin_config_targets.get("net.minecraft.Foo"),
            Some(&1)
        );
        assert_eq!(
            census.jars[0].mixin_config_fields["ruby.mixins.json"],
            [
                "plugin",
                "refmap",
                "required",
                "minVersion",
                "compatibilityLevel",
                "priority",
                "injectors.defaultRequire",
                "injectors.maxShiftBy",
                "injectors.injectionPoints"
            ]
        );
        assert_eq!(
            census.jars[0].mixin_config_values["ruby.mixins.json"],
            BTreeMap::from([
                ("compatibilityLevel".to_owned(), "JAVA_25".to_owned()),
                ("injectors.defaultRequire".to_owned(), "2".to_owned()),
                ("injectors.maxShiftBy".to_owned(), "3".to_owned()),
                ("minVersion".to_owned(), "0.8".to_owned()),
                ("plugin".to_owned(), "example.Plugin".to_owned()),
                ("priority".to_owned(), "1200".to_owned()),
                ("refmap".to_owned(), "ruby.refmap.json".to_owned()),
                ("required".to_owned(), "true".to_owned()),
            ])
        );
    }

    #[test]
    fn synthetic_mixin_jar_discovers_json_targets_and_hook_points() {
        let dir = std::env::temp_dir().join("horizon-census-mixin-fixture");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fixture.jar");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        // Keep the fixture close to a real Fabric mod: the census must join
        // loader metadata with report-only Mixin config discovery in one
        // deterministic read.  The adapter is retained as a diagnostic;
        // this test does not execute or weave the class.
        zip.start_file("fabric.mod.json", options).unwrap();
        std::io::Write::write_all(
            &mut zip,
            br#"{"id":"fixture-mixin","version":"1.0.0","entrypoints":{"main":{"value":"example.FixtureMixin","adapter":"net.fabricmc.api.ModInitializer"}}}"#,
        )
        .unwrap();
        zip.start_file("fixture.mixins.json", options).unwrap();
        std::io::Write::write_all(
            &mut zip,
            br#"{"mixins":[{"target":"net.minecraft.Target"}],"client":[{"target":"net.minecraft.ClientTarget"}]}"#,
        )
        .unwrap();
        zip.start_file("example/FixtureMixin.class", options)
            .unwrap();
        std::io::Write::write_all(
            &mut zip,
            &classes::tests_support::annotated_mixin(
                "example/FixtureMixin",
                "net/minecraft/Target",
                "destroyBlock()V",
            ),
        )
        .unwrap();
        zip.finish().unwrap();

        let hooks_path = dir.join("hooks.json");
        std::fs::write(
            &hooks_path,
            br#"{"hooks":[{"name":"destroy_block","vanilla":"net/minecraft/Target#destroyBlock"}]}"#,
        )
        .unwrap();
        let hooks = HookIndex::read(&hooks_path).unwrap();
        let census = read_with(&path, Some(&hooks)).unwrap();
        assert_eq!(census.jars[0].entrypoints.len(), 1);
        assert_eq!(census.jars[0].entrypoints[0].kind, "main");
        assert_eq!(census.jars[0].entrypoints[0].class, "example.FixtureMixin");
        assert_eq!(
            census.jars[0].entrypoints[0].adapter.as_deref(),
            Some("net.fabricmc.api.ModInitializer")
        );
        assert_eq!(census.jars[0].mixin_config_names, ["fixture.mixins.json"]);
        assert_eq!(
            census.jars[0].mixin_config_targets,
            ["net.minecraft.ClientTarget", "net.minecraft.Target"]
        );
        assert_eq!(census.mixin_targets.get("net/minecraft/Target"), Some(&1));
        assert_eq!(census.injections.get("inject"), Some(&1));
        assert_eq!(
            census.mixin_method_descriptors["net/minecraft/Target#destroyBlock"],
            ["()V"]
        );
        assert_eq!(
            census.mixin_at_targets["HEAD|target:Lnet/minecraft/Target;destroyBlock()V"],
            ["Lnet/minecraft/Target;destroyBlock()V"]
        );
        assert_eq!(
            census.hook_points["net/minecraft/Target#destroyBlock"][0].hooks,
            ["destroy_block"]
        );
        assert_eq!(census.mixin_translation_plans.len(), 1);
        assert_eq!(census.mixin_translation_plans[0].mode, "callback");
        assert_eq!(census.mixin_translation_plans[0].status, "candidate");
        assert!(
            census.mixin_translation_plans[0]
                .reason
                .contains("callback codegen")
        );
        assert_eq!(census.totals.points_covered, 1);
    }

    #[test]
    fn discovers_bounded_access_metadata_targets() {
        let aw = "accessWidener v2 named\naccessible class net/minecraft/Foo\nmutable field net/minecraft/Foo bar I\n# ignored\n";
        assert_eq!(access_widener_namespace(aw).as_deref(), Some("named"));
        assert_eq!(
            access_widener_targets(aw),
            [
                "accessible class net/minecraft/Foo",
                "mutable field net/minecraft/Foo bar I"
            ]
        );
        let at = "# comment\npublic net.minecraft.Foo\nprotected-f net.minecraft.Foo field I\nprivate net.minecraft.Foo method (I)V\n";
        assert_eq!(
            access_transformer_targets(at),
            [
                "net.minecraft.Foo",
                "net.minecraft.Foo field I",
                "net.minecraft.Foo method (I)V"
            ]
        );
        assert_eq!(
            access_widener_namespace("accessWidener v2 intermediary\naccessible class a/B\n")
                .as_deref(),
            Some("intermediary")
        );
        assert_eq!(access_widener_namespace("accessWidener v2\n"), None);
    }

    #[test]
    fn ranks_by_count_then_name() {
        let map = BTreeMap::from([
            ("b".to_owned(), 2),
            ("a".to_owned(), 2),
            ("c".to_owned(), 5),
        ]);
        let rows = ranked(&map, 2);
        assert_eq!(rows[0].0, "c");
        assert_eq!(rows[1].0, "a");
    }

    #[test]
    fn summary_calls_out_access_metadata_as_report_only() {
        let census = Census {
            access_widener_targets: BTreeMap::from([(String::from("net/minecraft/Foo"), 2)]),
            access_transformer_targets: BTreeMap::from([(String::from("net.minecraft.Bar"), 1)]),
            access_widener_namespaces: BTreeMap::from([(String::from("intermediary"), 1)]),
            ..Census::default()
        };
        let summary = census.summary(20);
        assert!(summary.contains(
            "access metadata: 2 access widener targets, 1 access transformer targets (report-only; no bytecode changes)"
        ));
        assert!(summary.contains(
            "access widener namespaces: intermediary=1 (report-only; mappings not applied)"
        ));
    }

    #[test]
    fn a_class_with_reflection_is_not_static() {
        let class = classes::parse(&classes::tests_support::class_with(
            "a/Reflector",
            "java/lang/Object",
            &["load"],
            &["java/lang/reflect/Method"],
            false,
        ))
        .unwrap();
        assert_eq!(
            dynamic_reason(&class).as_deref(),
            Some("java/lang/reflect/Method")
        );
    }

    #[test]
    fn a_lambda_is_not_reflection() {
        let class = classes::parse(&classes::tests_support::class_with(
            "a/Plain",
            "java/lang/Object",
            &["run"],
            &["java/lang/invoke/LambdaMetafactory"],
            false,
        ))
        .unwrap();
        assert_eq!(dynamic_reason(&class), None);
    }

    #[test]
    fn service_loader_is_reported_as_runtime_classpath_input() {
        let class = classes::Class {
            refs: [SERVICE_LOADER.to_owned()].into_iter().collect(),
            ..classes::Class::default()
        };
        assert_eq!(dynamic_reason(&class).as_deref(), Some(SERVICE_LOADER));

        let class = classes::Class {
            members: vec![classes::Member {
                owner: SERVICE_LOADER.to_owned(),
                name: "load".to_owned(),
                descriptor: "(Ljava/lang/Class;)Ljava/util/ServiceLoader;".to_owned(),
                kind: classes::MemberKind::Method,
            }],
            ..classes::Class::default()
        };
        assert_eq!(
            dynamic_reason(&class).as_deref(),
            Some("java/util/ServiceLoader#load")
        );
    }

    #[test]
    fn class_loader_resource_lookups_are_reported() {
        let class = classes::Class {
            members: vec![classes::Member {
                owner: "java/lang/ClassLoader".to_owned(),
                name: "getResourceAsStream".to_owned(),
                descriptor: "(Ljava/lang/String;)Ljava/io/InputStream;".to_owned(),
                kind: classes::MemberKind::Method,
            }],
            ..classes::Class::default()
        };
        assert_eq!(
            dynamic_reason(&class).as_deref(),
            Some("java/lang/ClassLoader#getResourceAsStream")
        );
    }

    #[test]
    fn class_for_name_reports_native_seeded_boundary() {
        let class = classes::Class {
            members: vec![classes::Member {
                owner: "java/lang/Class".to_owned(),
                name: "forName".to_owned(),
                descriptor: "(Ljava/lang/String;)Ljava/lang/Class;".to_owned(),
                kind: classes::MemberKind::Method,
            }],
            ..classes::Class::default()
        };
        assert_eq!(
            dynamic_reason(&class).as_deref(),
            Some(
                "java/lang/Class#forName (native current-jar only; JVM fallback for arbitrary loading)"
            )
        );
    }

    #[test]
    fn discovers_neoforge_mod_annotation_entrypoints() {
        let mut annotation = classes::Annotation {
            ty: "Lnet/neoforged/fml/common/Mod;".to_owned(),
            values: BTreeMap::new(),
        };
        annotation
            .values
            .insert("value".to_owned(), classes::Value::Str("ruby".to_owned()));
        let classes = vec![classes::Class {
            name: "example/RubyMod".to_owned(),
            annotations: vec![annotation],
            ..classes::Class::default()
        }];
        assert_eq!(
            neoforge_entrypoints(&classes),
            [crate::jar::Entrypoint {
                kind: "neoforge".to_owned(),
                class: "example/RubyMod".to_owned(),
                adapter: None,
            }]
        );
    }

    #[test]
    fn descriptors_are_not_part_of_the_method() {
        assert_eq!(strip_descriptor("tick()V"), "tick");
        assert_eq!(strip_descriptor("tick"), "tick");
        assert_eq!(strip_descriptor("a b"), "a");
        assert_eq!(method_descriptor("tick()V"), Some("()V"));
        assert_eq!(method_descriptor("tick"), None);
    }

    #[test]
    fn mixin_method_lists_expand_into_independent_hook_keys() {
        assert_eq!(
            injection_methods("tick()V|render()V").collect::<Vec<_>>(),
            ["tick", "render"]
        );
    }

    #[test]
    fn hook_catalog_normalizes_method_descriptors() {
        let dir = std::env::temp_dir().join("horizon-census-hook-descriptor");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hooks.json");
        std::fs::write(
            &file,
            r#"{"hooks":[{"name":"destroy_block","vanilla":"net/minecraft/server/level/ServerPlayerGameMode#destroyBlock(Lnet/minecraft/core/BlockPos;)Z"}]}"#,
        )
        .unwrap();
        let hooks = HookIndex::read(&file).unwrap();
        let mut census = Census {
            points: vec![PointCount {
                target: "net/minecraft/server/level/ServerPlayerGameMode".to_owned(),
                method: "destroyBlock".to_owned(),
                kind: "inject".to_owned(),
                at: "HEAD".to_owned(),
                count: 1,
                hooks: Vec::new(),
            }],
            ..Census::default()
        };
        census.join(&hooks);
        assert_eq!(census.points[0].hooks, ["destroy_block"]);
        assert_eq!(census.totals.points_covered, 1);
    }

    #[test]
    fn hook_catalog_deduplicates_names_for_one_method() {
        let dir = std::env::temp_dir().join("horizon-census-hook-dedup");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hooks.json");
        std::fs::write(
            &file,
            r#"{"hooks":[{"name":"zeta","vanilla":"net/minecraft/Target#tick"},{"name":"alpha","vanilla":"net/minecraft/Target#tick"},{"name":"zeta","vanilla":"net/minecraft/Target#tick"}]}"#,
        )
        .unwrap();
        let hooks = HookIndex::read(&file).unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(
            hooks.hooks_for("net/minecraft/Target", "tick").unwrap(),
            &["alpha", "zeta"]
        );
    }

    #[test]
    fn a_point_is_covered_by_a_hook_that_names_its_method() {
        let dir = std::env::temp_dir().join("horizon-census-hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hooks.json");
        std::fs::write(
            &file,
            r#"{"hooks":[{"name":"destroy_block","vanilla":"net/minecraft/server/level/ServerPlayerGameMode#destroyBlock"}]}"#,
        )
        .unwrap();
        let hooks = HookIndex::read(&file).unwrap();
        assert_eq!(hooks.len(), 1);
        let mut census = Census {
            points: vec![PointCount {
                target: "net/minecraft/server/level/ServerPlayerGameMode".to_owned(),
                method: "destroyBlock".to_owned(),
                kind: "inject".to_owned(),
                at: "HEAD".to_owned(),
                count: 2,
                hooks: Vec::new(),
            }],
            ..Census::default()
        };
        census.join(&hooks);
        assert_eq!(census.totals.points_needing_hooks, 2);
        assert_eq!(census.totals.points_covered, 2);
        assert_eq!(census.points[0].hooks, ["destroy_block"]);
        assert_eq!(census.mixin_translation_plans[0].mode, "callback");
        assert_eq!(census.mixin_translation_plans[0].status, "candidate");
        assert_eq!(
            census.hook_points["net/minecraft/server/level/ServerPlayerGameMode#destroyBlock"],
            [HookPoint {
                kind: "inject".to_owned(),
                at: "HEAD".to_owned(),
                count: 2,
                hooks: vec!["destroy_block".to_owned()],
            }]
        );
    }

    #[test]
    fn single_hook_overwrite_becomes_executable_replacement_plan() {
        let dir = std::env::temp_dir().join("horizon-census-overwrite-hooks");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("hooks.json");
        std::fs::write(
            &file,
            r#"{"hooks":[{"name":"server_tick","vanilla":"net/minecraft/server/ServerLevel#tick"}]}"#,
        )
        .unwrap();
        let hooks = HookIndex::read(&file).unwrap();
        let mut census = Census {
            points: vec![PointCount {
                target: "net/minecraft/server/ServerLevel".to_owned(),
                method: "tick".to_owned(),
                kind: "overwrite".to_owned(),
                at: String::new(),
                count: 1,
                hooks: Vec::new(),
            }],
            ..Census::default()
        };
        census.join(&hooks);
        assert_eq!(census.mixin_translation_plans[0].mode, "replace-method");
        assert_eq!(census.mixin_translation_plans[0].status, "executable");
        assert_eq!(
            census.mixin_translation_plans[0].reason,
            "single native hook supports bounded direct method replacement"
        );
    }

    #[test]
    fn hook_points_serialize_native_hook_contract_name() {
        let point = HookPoint {
            kind: "inject".to_owned(),
            at: "HEAD".to_owned(),
            count: 1,
            hooks: vec!["server_tick".to_owned()],
        };
        let json = serde_json::to_value(&point).unwrap();
        assert_eq!(json["native_hooks"], serde_json::json!(["server_tick"]));
        assert!(json.get("hooks").is_none());
        let old: HookPoint = serde_json::from_value(serde_json::json!({
            "kind": "inject",
            "at": "HEAD",
            "count": 1,
            "hooks": ["server_tick"]
        }))
        .unwrap();
        assert_eq!(old.hooks, ["server_tick"]);
    }

    #[test]
    fn translation_plans_serialize_native_hook_contract_name() {
        let plan = MixinTranslationPlan {
            target: "net/minecraft/Target".to_owned(),
            method: "tick".to_owned(),
            kind: "redirect".to_owned(),
            at: "INVOKE".to_owned(),
            mode: "redirect".to_owned(),
            status: "candidate".to_owned(),
            count: 1,
            hooks: vec!["server_tick".to_owned()],
            reason: "native hook matched".to_owned(),
        };
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["native_hooks"], serde_json::json!(["server_tick"]));
        assert!(json.get("hooks").is_none());
    }

    #[test]
    fn member_only_mixin_points_are_not_counted_as_rust_hooks() {
        let mut census = Census {
            points: vec![PointCount {
                target: "net/minecraft/world/entity/Entity".to_owned(),
                method: "DATA_ID".to_owned(),
                kind: "accessor".to_owned(),
                at: String::new(),
                count: 3,
                hooks: Vec::new(),
            }],
            ..Census::default()
        };
        census.join(&HookIndex::default());
        assert_eq!(census.totals.member_only_points, 3);
        assert_eq!(census.totals.points_needing_hooks, 0);
        assert!(census.hook_points.is_empty());
    }

    #[test]
    fn unhooked_points_are_explicitly_report_only() {
        let mut census = Census {
            points: vec![PointCount {
                target: "net/minecraft/world/entity/Entity".to_owned(),
                method: "tick".to_owned(),
                kind: "inject".to_owned(),
                at: "INVOKE".to_owned(),
                count: 4,
                hooks: Vec::new(),
            }],
            ..Census::default()
        };
        census.join(&HookIndex::default());
        assert_eq!(census.totals.points_needing_hooks, 4);
        assert_eq!(census.totals.points_covered, 0);
        assert_eq!(census.totals.points_report_only, 4);
        assert_eq!(census.mixin_translation_plans[0].mode, "callback");
        assert_eq!(census.mixin_translation_plans[0].status, "report-only");
        assert_eq!(
            census.mixin_translation_plans[0].reason,
            "no native hook matches the target method"
        );
        let summary = census.summary(20);
        assert!(summary.contains("mixin injection points: 0 hook-covered, 4 report-only"));
    }

    #[test]
    fn config_targets_are_report_only_in_summary() {
        let census = Census {
            totals: Totals {
                mixin_config_targets_report_only: 2,
                ..Totals::default()
            },
            ..Census::default()
        };
        assert!(
            census
                .summary(20)
                .contains("mixin config targets: 2 report-only")
        );
    }

    #[test]
    fn annotation_metadata_is_reported_without_interpreting_values() {
        let census = Census {
            mixin_annotation_fields: BTreeMap::from([
                ("cancellable".to_owned(), 2),
                ("require".to_owned(), 1),
                ("expect".to_owned(), 1),
                ("order".to_owned(), 1),
            ]),
            ..Census::default()
        };
        let summary = census.summary(20);
        assert!(summary.contains("mixin annotation metadata (presence only; no weaving)"));
        assert!(summary.contains("      2 cancellable"));
    }

    #[test]
    fn abi_and_mixin_diagnostics_classify_compatibility_edges() {
        let member = |name: &str, kind| classes::Member {
            owner: "net/minecraft/Target".to_owned(),
            name: name.to_owned(),
            descriptor: "()V".to_owned(),
            kind,
        };
        assert_eq!(
            member_shape(&member("<init>", classes::MemberKind::Method)),
            "constructor"
        );
        assert_eq!(
            member_shape(&member("<clinit>", classes::MemberKind::Method)),
            "static-initializer"
        );
        assert_eq!(
            member_shape(&member("value", classes::MemberKind::Field)),
            "field"
        );
        assert_eq!(mixin_method_kind("<init>(I)V|tick()V"), "constructor");
        assert_eq!(mixin_method_kind("tick()V"), "method");
        assert_eq!(
            api_reference_kind("net/neoforged/neoforge/event/entity/EntityEvent"),
            Some("event")
        );
        assert_eq!(
            api_reference_kind("net/minecraftforge/common/capabilities/Capability"),
            Some("capability")
        );
        let summary = Census {
            member_shapes: BTreeMap::from([("constructor".to_owned(), 2)]),
            api_refs: BTreeMap::from([("event".to_owned(), 3), ("capability".to_owned(), 1)]),
            ..Census::default()
        }
        .summary(10);
        assert!(summary.contains("member ABI shapes"));
        assert!(summary.contains("loader API references"));
    }

    #[test]
    fn member_disposition_routes_descriptor_bearing_abi_surfaces() {
        let member = |owner: &str| classes::Member {
            owner: owner.to_owned(),
            name: "value".to_owned(),
            descriptor: "()Ljava/lang/Object;".to_owned(),
            kind: classes::MemberKind::Method,
        };
        assert_eq!(
            member_disposition(&member("net/minecraft/world/level/Level")),
            AbiDisposition::Facade
        );
        assert_eq!(
            member_disposition(&member("com/mojang/brigadier/CommandDispatcher")),
            AbiDisposition::Facade
        );
        assert_eq!(
            member_disposition(&member("com/mojang/serialization/Codec")),
            AbiDisposition::Facade
        );
        for owner in [
            "com/mojang/serialization/MapCodec",
            "com/mojang/serialization/DataResult",
            "com/mojang/serialization/DynamicOps",
            "com/mojang/datafixers/util/Pair",
        ] {
            assert_eq!(
                member_disposition(&member(owner)),
                AbiDisposition::Facade,
                "Mojang codec/datafixer surface {owner} must stay façade-backed"
            );
        }
        assert_eq!(
            member_disposition(&member("net/neoforged/bus/api/IEventBus")),
            AbiDisposition::Loader
        );
        assert_eq!(
            member_disposition(&member("net/minecraftforge/eventbus/api/IEventBus")),
            AbiDisposition::Loader
        );
        assert_eq!(
            member_disposition(&member("org/spongepowered/asm/mixin/injection/Inject")),
            AbiDisposition::Loader
        );
        assert_eq!(
            member_disposition(&member(
                "com/llamalad7/mixinextras/injector/wrapoperation/WrapOperation"
            )),
            AbiDisposition::Loader
        );
        assert_eq!(
            member_disposition(&member("org/quiltmc/qsl/base/api/util/Holder")),
            AbiDisposition::Loader
        );
        assert_eq!(
            member_disposition(&member("java/util/Map")),
            AbiDisposition::Jdk
        );
        assert_eq!(
            member_disposition(&member("com/example/Library")),
            AbiDisposition::External
        );
    }
}
