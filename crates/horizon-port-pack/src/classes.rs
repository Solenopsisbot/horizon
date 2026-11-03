//! What a mod's Java code does, read from its class files without running
//! or decompiling them: each class's superclass, interfaces, declared
//! methods and the classes and annotations its constant pool names. The
//! mod's own classes are followed up to the first game class they extend,
//! and that class says what the code is: a block entity, a menu, an entity,
//! a block or item with behaviour of its own, a worldgen feature...
//!
//! Since 26.1 the game ships unobfuscated, and NeoForge has run on Mojang's
//! names since 1.20.5, so game classes and methods are read by name. Fabric
//! jars before 26.1 name them in intermediary (`net/minecraft/class_2248`)
//! and Forge's methods are SRG (`m_60503_`): a class extending such a game
//! class is counted as code of its own, whatever it overrides.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Read,
    path::Path,
};

/// One class file, as much as this module reads.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Class {
    pub name: String,
    /// JVM access flags: 0x0200 interface, 0x0400 abstract, 0x0010 final,
    /// 0x4000 enum, 0x2000 annotation.
    pub access: u16,
    pub superclass: String,
    pub interfaces: Vec<String>,
    /// Declared fields, by name.
    pub fields: Vec<String>,
    /// Declared methods, by name (constructors and lambdas left out).
    pub methods: Vec<String>,
    /// Every class the constant pool names (`net/minecraft/...`).
    pub refs: BTreeSet<String>,
    /// Every field and method the constant pool names, with its owner: what
    /// the class actually attaches to (`docs/java-mod-compat.md`, phase 0).
    pub members: Vec<Member>,
    /// Every field, method and class its method bodies name, in scan order:
    /// the call sites, which the constant pool alone does not show.
    pub body_refs: Vec<CallTarget>,
    /// Why the body scan stopped early, if it did: `class#method at offset N:
    /// ...`. The references read before it are kept, so one bad method does
    /// not lose the class.
    pub body_error: Option<String>,
    /// The class's own annotations.
    pub annotations: Vec<Annotation>,
    /// Every declared field and method, with its access flags: the shapes a
    /// façade is generated from.
    pub declared: Vec<Declared>,
    /// Declared methods' annotations, by method name.
    pub method_annotations: BTreeMap<String, Vec<Annotation>>,
    /// Declared fields' annotations, by field name.
    pub field_annotations: BTreeMap<String, Vec<Annotation>>,
    /// `@Mixin`'s targets (`net/minecraft/world/level/Level`).
    pub mixin_targets: Vec<String>,
    /// `@Inject`, `@Redirect`, `@Overwrite`...: what the mixin does, and where.
    pub injections: Vec<Injection>,
    /// The pool names Mixin's annotation: the class is (or uses) a mixin.
    pub mixin: bool,
}

/// A field or method a class names, with the class that owns it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Member {
    pub owner: String,
    pub name: String,
    /// `()V`, `Ljava/lang/String;`
    pub descriptor: String,
    pub kind: MemberKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberKind {
    Field,
    Method,
    InterfaceMethod,
}

impl Member {
    /// `owner#name descriptor`, the census's key.
    pub fn key(&self) -> String {
        format!("{}#{} {}", self.owner, self.name, self.descriptor)
    }
}

/// What one instruction in a method body names: a field, a method, a class
/// literal or a method handle. The ABI corpus ranks these by call site, which
/// the constant pool's [`Member`] list cannot do: it holds every name once,
/// called or not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallTarget {
    pub owner: String,
    /// Empty for a class literal.
    pub name: String,
    /// Empty for a class literal.
    pub descriptor: String,
    pub kind: CallKind,
}

impl CallTarget {
    /// `owner#name(descriptor)`, the census's method-body key; a class
    /// literal is its own name.
    pub fn key(&self) -> String {
        if self.kind == CallKind::Class {
            self.owner.clone()
        } else {
            format!("{}#{}({})", self.owner, self.name, self.descriptor)
        }
    }

    pub fn is_field(&self) -> bool {
        matches!(
            self.kind,
            CallKind::GetStatic | CallKind::PutStatic | CallKind::GetField | CallKind::PutField
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallKind {
    Virtual,
    Static,
    Interface,
    Special,
    GetStatic,
    PutStatic,
    GetField,
    PutField,
    /// A class literal: `ldc a/Type`.
    Class,
    /// A method handle: `ldc` of one, a bootstrap method, or a bootstrap
    /// argument (a lambda's body or a method reference).
    MethodHandle,
}

impl Class {
    /// The methods its bodies invoke, counted once per call site, keyed
    /// `owner#name(descriptor)`.
    pub fn calls(&self) -> BTreeMap<String, usize> {
        counted(&self.body_refs, |target| {
            !target.is_field() && target.kind != CallKind::Class
        })
    }

    /// The fields its bodies read or write, counted the same way.
    pub fn field_refs(&self) -> BTreeMap<String, usize> {
        counted(&self.body_refs, CallTarget::is_field)
    }
}

fn counted(targets: &[CallTarget], keep: impl Fn(&CallTarget) -> bool) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for target in targets.iter().filter(|target| keep(target)) {
        *out.entry(target.key()).or_default() += 1;
    }
    out
}

/// Every method a set of classes' bodies invokes, keyed `owner#name(descriptor)`
/// and counted per call site, sorted by key: the ABI corpus's call ranking.
pub fn method_references(classes: &[Class]) -> BTreeMap<String, usize> {
    references(classes, Class::calls)
}

/// The fields those bodies read or write, counted the same way.
pub fn field_references(classes: &[Class]) -> BTreeMap<String, usize> {
    references(classes, Class::field_refs)
}

fn references(
    classes: &[Class],
    count: impl Fn(&Class) -> BTreeMap<String, usize>,
) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for class in classes {
        for (key, sites) in count(class) {
            *out.entry(key).or_default() += sites;
        }
    }
    out
}

/// A field or method a class declares, with its flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    pub name: String,
    pub descriptor: String,
    pub access: u16,
    pub kind: MemberKind,
}

impl Declared {
    pub fn is_static(&self) -> bool {
        self.access & 0x0008 != 0
    }

    pub fn is_public(&self) -> bool {
        self.access & 0x0001 != 0
    }

    pub fn is_protected(&self) -> bool {
        self.access & 0x0004 != 0
    }

    pub fn is_abstract(&self) -> bool {
        self.access & 0x0400 != 0
    }

    pub fn is_enum_constant(&self) -> bool {
        self.kind == MemberKind::Field && self.access & 0x4000 != 0
    }
}

/// One annotation, as far as the census needs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    /// `Lorg/spongepowered/asm/mixin/Mixin;`
    pub ty: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, Value>,
}

/// An annotation's element value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    /// Every integral and floating constant, widened.
    Number(f64),
    Str(String),
    /// A class literal (`Lnet/minecraft/world/item/Item;`).
    Type(String),
    Enum(String, String),
    Ann(Annotation),
    Array(Vec<Value>),
}

/// One `@Inject`/`@Redirect`/... on a mixin method.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Injection {
    /// `inject`, `redirect`, `modify-variable`, `overwrite`, `accessor`...
    pub kind: String,
    /// The mixin's `method` target, or empty for the whole class.
    pub method: String,
    /// The `At` values, `|`-joined: `HEAD`, `INVOKE|target:getBlockState`.
    pub at: String,
    /// Presence-only edge metadata used when planning native hook contracts.
    /// Values are intentionally not interpreted by the census.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata: Vec<String>,
    /// Deterministic scalar values for the same report-only metadata. Nested
    /// annotations such as `slice` are represented by the stable `present`
    /// marker; no Mixin semantics are applied.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata_values: BTreeMap<String, String>,
}

/// The annotation that makes a class a Mixin.
const MIXIN: &str = "Lorg/spongepowered/asm/mixin/Mixin;";

/// Reads a class file's header, constant pool, members, annotations and the
/// references its method bodies make. A body that cannot be walked keeps the
/// references read before it and records why in [`Class::body_error`].
pub fn parse(bytes: &[u8]) -> Option<Class> {
    let mut r = Reader { bytes, at: 0 };
    if r.u4()? != 0xCAFE_BABE {
        return None;
    }
    r.skip(4)?; // minor, major
    let mut pool = Pool::read(&mut r)?;
    let access = r.u2()?;
    let name = pool.class_name(r.u2()?)?.to_owned();
    let superclass = pool.class_name(r.u2()?).unwrap_or_default().to_owned();
    let n = r.u2()? as usize;
    let mut interfaces = Vec::with_capacity(n);
    for _ in 0..n {
        interfaces.push(pool.class_name(r.u2()?).unwrap_or_default().to_owned());
    }
    let mut fields = Vec::new();
    let mut declared = Vec::new();
    let mut field_annotations = BTreeMap::new();
    for _ in 0..r.u2()? {
        let access = r.u2()?;
        let field = pool.str(r.u2()?)?.to_owned();
        let descriptor = pool.str(r.u2()?)?.to_owned();
        let annotations = annotations(&attributes(&mut r, &pool)?, &pool);
        if !annotations.is_empty() {
            field_annotations.insert(field.clone(), annotations);
        }
        declared.push(Declared {
            name: field.clone(),
            descriptor,
            access,
            kind: MemberKind::Field,
        });
        fields.push(field);
    }
    let mut methods = Vec::new();
    let mut method_annotations = BTreeMap::new();
    let mut bodies: Vec<(String, &[u8])> = Vec::new();
    for _ in 0..r.u2()? {
        let access = r.u2()?;
        let method = pool.str(r.u2()?)?.to_owned();
        let descriptor = pool.str(r.u2()?)?.to_owned();
        let attributes = attributes(&mut r, &pool)?;
        let annotations = annotations(&attributes, &pool);
        if !annotations.is_empty() {
            method_annotations.insert(method.clone(), annotations);
        }
        for (attribute, bytes) in &attributes {
            if attribute == "Code" {
                bodies.push((method.clone(), *bytes));
            }
        }
        declared.push(Declared {
            name: method.clone(),
            descriptor,
            access,
            kind: MemberKind::Method,
        });
        // Constructors and lambdas are not members a mod declares.
        if !method.starts_with('<') && !method.starts_with("lambda$") {
            methods.push(method);
        }
    }
    let attributes = attributes(&mut r, &pool)?;
    pool.bootstrap = bootstrap_methods(&attributes);
    let annotations = annotations(&attributes, &pool);
    let mut body_refs = Vec::new();
    let mut body_error = None;
    for (method, bytes) in &bodies {
        if body_refs.len() >= BODY_REFS_MAX {
            break;
        }
        if let Err(error) = scan_method_code(bytes, &pool, &name, method, &mut body_refs) {
            body_error = Some(error);
            break;
        }
    }
    let mixin = annotations.iter().any(|a| a.ty == MIXIN) || pool.utf8.values().any(|s| s == MIXIN);
    let mixin_targets = if mixin {
        mixin_targets(&annotations)
    } else {
        Vec::new()
    };
    let injections = if mixin {
        injections(&method_annotations, &field_annotations)
    } else {
        Vec::new()
    };
    Some(Class {
        name,
        access,
        superclass,
        interfaces,
        fields,
        declared,
        methods,
        refs: pool.classes(),
        members: pool.members(),
        body_refs,
        body_error,
        mixin_targets,
        injections,
        annotations,
        method_annotations,
        field_annotations,
        mixin,
    })
}

/// Every class file in a jar, its nested jars (`META-INF/jars/`) included,
/// one level deep.
pub fn read_jar(path: &Path) -> Result<Vec<Class>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut top = Vec::new();
    let mut nested = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_owned();
        if name.ends_with(".class") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            top.push(bytes);
        } else if name.starts_with("META-INF/jars/") && name.ends_with(".jar") && nested.len() < 64
        {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            if bytes.len() < 32 * 1024 * 1024 {
                nested.push(bytes);
            }
        }
    }
    let mut classes: Vec<Class> = top.iter().filter_map(|b| parse(b)).collect();
    for bytes in &nested {
        let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes.as_slice())) else {
            continue;
        };
        for i in 0..archive.len() {
            let Ok(mut entry) = archive.by_index(i) else {
                continue;
            };
            if !entry.name().ends_with(".class") {
                continue;
            }
            let mut bytes = Vec::new();
            if entry.read_to_end(&mut bytes).is_ok()
                && let Some(class) = parse(&bytes)
            {
                classes.push(class);
            }
        }
    }
    Ok(classes)
}

/// A Fabric access widener, parsed but not yet applied: the classes and
/// members a mod says the game's own code may reach. The census reports the
/// files and their namespaces (`census::access_widener_targets`);
/// `apply_access_widener` turns them into the flags on parsed classes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessWidener {
    /// The naming the entries use (`named`, `intermediary`). Since 26.1 the
    /// game is named, so only `named` matches without a remap; remapping is
    /// the caller's business, nothing here rewrites a name.
    pub namespace: String,
    /// In file order, duplicates kept: applying the same entry twice is not
    /// an error, it changes nothing the second time.
    pub entries: Vec<AccessWidenerEntry>,
}

/// One `accessible`/`extendable` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessWidenerEntry {
    pub kind: AccessWidenerKind,
    pub access: AccessWidenerAccess,
    /// An internal name: `net/minecraft/world/level/block/Block`.
    pub owner: String,
    /// The member's name, empty for a class entry.
    pub name: String,
    /// `None` widens every member of that name (all overloads); a class entry
    /// never carries one.
    pub descriptor: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessWidenerKind {
    Class,
    Method,
    Field,
}

impl AccessWidenerKind {
    /// How an error names the entry.
    fn label(self) -> &'static str {
        match self {
            AccessWidenerKind::Class => "class",
            AccessWidenerKind::Method => "method",
            AccessWidenerKind::Field => "field",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessWidenerAccess {
    /// Callers outside the declaring class may use it.
    Accessible,
    /// Subclasses outside the declaring class may extend it.
    Extendable,
}

/// JVM access flags an access widener rewrites.
const ACC_PUBLIC: u16 = 0x0001;
const ACC_PRIVATE: u16 = 0x0002;
const ACC_PROTECTED: u16 = 0x0004;
const ACC_FINAL: u16 = 0x0010;

/// The most text read, and the most entries taken from it. A 1 MiB file holds
/// at most ~55k entries of even the shortest line (`accessible class a`), so
/// the entry bound is the one that can fire: 16k is still far past any real
/// widener and keeps the second guard reachable, and testable.
const ACCESS_WIDENER_TEXT_MAX: usize = 1024 * 1024;
const ACCESS_WIDENER_ENTRIES_MAX: usize = 16 * 1024;

/// Reads a Fabric access widener's v1 or v2 text: a header naming the
/// namespace, then one `accessible`/`extendable` line per class or member.
/// `#` starts a comment and blank lines are skipped; v2's `transitive-` and
/// `mixin.` forms are not applied and are rejected with the line that uses
/// them, as is any version but v1 and v2.
pub fn parse_access_widener(text: &str) -> Result<AccessWidener, String> {
    if text.len() > ACCESS_WIDENER_TEXT_MAX {
        return Err(format!(
            "access widener is {} bytes, the limit is {ACCESS_WIDENER_TEXT_MAX}",
            text.len()
        ));
    }
    let mut widener = AccessWidener {
        namespace: String::new(),
        entries: Vec::new(),
    };
    let mut header = false;
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.split_once('#').map_or(raw, |(before, _)| before).trim();
        if line.is_empty() {
            continue;
        }
        if !header {
            widener.namespace = access_widener_header(line, number)?;
            header = true;
            continue;
        }
        if widener.entries.len() >= ACCESS_WIDENER_ENTRIES_MAX {
            return Err(format!(
                "line {number}: more than {ACCESS_WIDENER_ENTRIES_MAX} entries"
            ));
        }
        widener.entries.push(access_widener_entry(line, number)?);
    }
    if !header {
        return Err("missing the `accessWidener v1 <namespace>` header".to_owned());
    }
    Ok(widener)
}

/// The namespace of the header line, which must be the first thing in the
/// file that is not a comment.
fn access_widener_header(line: &str, number: usize) -> Result<String, String> {
    let mut fields = line.split_whitespace();
    if fields.next() != Some("accessWidener") {
        return Err(format!(
            "line {number}: expected `accessWidener v1 <namespace>`"
        ));
    }
    let Some(version) = fields.next() else {
        return Err(format!("line {number}: the header names no version"));
    };
    if version != "v1" && version != "v2" {
        return Err(format!(
            "line {number}: unsupported access widener version `{version}` (only v1 and v2)"
        ));
    }
    let Some(namespace) = fields.next() else {
        return Err(format!("line {number}: the header names no namespace"));
    };
    if namespace.len() > 32
        || !namespace
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(format!(
            "line {number}: `{namespace}` is not an access widener namespace"
        ));
    }
    if fields.next().is_some() {
        return Err(format!("line {number}: text after the header"));
    }
    Ok(namespace.to_owned())
}

fn access_widener_entry(line: &str, number: usize) -> Result<AccessWidenerEntry, String> {
    let mut fields = line.split_whitespace();
    let Some(word) = fields.next() else {
        return Err(format!("line {number}: empty entry"));
    };
    let access = match word {
        "accessible" => AccessWidenerAccess::Accessible,
        "extendable" => AccessWidenerAccess::Extendable,
        _ if word.starts_with("transitive-") => {
            return Err(format!(
                "line {number}: `{word}`: transitive wideners are not applied"
            ));
        }
        _ => {
            return Err(format!(
                "line {number}: `{word}` is not `accessible` or `extendable`"
            ));
        }
    };
    let Some(word) = fields.next() else {
        return Err(format!("line {number}: the entry names no kind"));
    };
    let kind = match word {
        "class" => AccessWidenerKind::Class,
        "method" => AccessWidenerKind::Method,
        "field" => AccessWidenerKind::Field,
        _ if word.starts_with("mixin.") => {
            return Err(format!(
                "line {number}: `{word}`: mixin targets are not applied"
            ));
        }
        _ => {
            return Err(format!(
                "line {number}: `{word}` is not `class`, `method` or `field`"
            ));
        }
    };
    let Some(owner) = fields.next() else {
        return Err(format!("line {number}: the entry names no class"));
    };
    if !is_internal_name(owner) {
        return Err(format!(
            "line {number}: `{owner}` is not an internal class name"
        ));
    }
    let mut entry = AccessWidenerEntry {
        kind,
        access,
        owner: owner.to_owned(),
        name: String::new(),
        descriptor: None,
    };
    if kind == AccessWidenerKind::Class {
        if fields.next().is_some() {
            return Err(format!("line {number}: a class entry names no member"));
        }
        return Ok(entry);
    }
    let Some(name) = fields.next() else {
        return Err(format!(
            "line {number}: a {} entry names no member",
            kind.label()
        ));
    };
    if !is_member_name(name) {
        return Err(format!("line {number}: `{name}` is not a member name"));
    }
    entry.name = name.to_owned();
    if let Some(descriptor) = fields.next() {
        if !is_descriptor(descriptor) {
            return Err(format!("line {number}: `{descriptor}` is not a descriptor"));
        }
        entry.descriptor = Some(descriptor.to_owned());
    }
    if fields.next().is_some() {
        return Err(format!(
            "line {number}: text after the {} entry",
            kind.label()
        ));
    }
    Ok(entry)
}

/// A bounded JVM internal name, `/`-separated, as the class file writes it.
fn is_internal_name(text: &str) -> bool {
    text.len() <= 512
        && !text.starts_with('/')
        && !text.ends_with('/')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '/'))
}

/// A bounded JVM descriptor: `()V`, `(I)I`, `Lnet/minecraft/item/Item;`.
fn is_descriptor(text: &str) -> bool {
    text.len() <= 1024
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '(' | ')' | '[' | ';' | '/' | '_' | '$')
        })
}

/// Applies a widener to parsed classes, returning the number of declarations
/// whose flags changed: a class entry counts as one, each widened member as
/// one. Matching is by the names the classes carry, so the namespace is
/// recorded and not acted on.
///
/// A target no class or member matches is an error naming it: a widener that
/// silently widened nothing must not read as applied. The caller therefore
/// passes every class the widener targets, the jar's own and the game's where
/// they have been read.
pub fn apply_access_widener(
    classes: &mut [Class],
    widener: &AccessWidener,
) -> Result<usize, String> {
    let mut changed = 0;
    for entry in &widener.entries {
        let Some(class) = classes.iter_mut().find(|class| class.name == entry.owner) else {
            return Err(format!("{}: no class of that name was read", entry.owner));
        };
        if entry.kind == AccessWidenerKind::Class {
            if widen(&mut class.access, entry.access) {
                changed += 1;
            }
            continue;
        }
        let mut matched = 0;
        for declared in &mut class.declared {
            if declared.name != entry.name || !kind_matches(declared, entry.kind) {
                continue;
            }
            if entry
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| declared.descriptor != *descriptor)
            {
                continue;
            }
            matched += 1;
            if widen(&mut declared.access, entry.access) {
                changed += 1;
            }
        }
        if matched == 0 {
            return Err(match &entry.descriptor {
                Some(descriptor) => format!(
                    "{}#{} {descriptor}: no such {} declared",
                    entry.owner,
                    entry.name,
                    entry.kind.label()
                ),
                None => format!(
                    "{}#{}: no such {} declared",
                    entry.owner,
                    entry.name,
                    entry.kind.label()
                ),
            });
        }
    }
    Ok(changed)
}

/// A `method` entry names methods (constructors included); a `field` entry
/// names fields.
fn kind_matches(declared: &Declared, kind: AccessWidenerKind) -> bool {
    match kind {
        AccessWidenerKind::Field => declared.kind == MemberKind::Field,
        AccessWidenerKind::Method => declared.kind != MemberKind::Field,
        AccessWidenerKind::Class => false,
    }
}

/// Widens one declaration's flags: `accessible` drops the narrower access,
/// `extendable` does the same and lets subclasses in too. Returns whether
/// anything changed, so a second application counts nothing.
fn widen(access: &mut u16, widening: AccessWidenerAccess) -> bool {
    let before = *access;
    *access &= !(ACC_PRIVATE | ACC_PROTECTED);
    *access |= ACC_PUBLIC;
    if widening == AccessWidenerAccess::Extendable {
        *access &= !ACC_FINAL;
    }
    *access != before
}

/// A Forge/NeoForge access transformer (`META-INF/accesstransformer.cfg`),
/// parsed but not yet applied: `<access> <class> [member [descriptor]]` per
/// line, class names in dots, like a widener matched against the names the
/// classes carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTransformer {
    /// In file order, duplicates kept: applying an entry twice changes nothing.
    pub entries: Vec<AccessTransformerEntry>,
}

/// One access transformer line: a class, or one of its members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessTransformerEntry {
    pub access: AccessTransformerAccess,
    /// An internal name: `net/minecraft/world/level/Level`.
    pub owner: String,
    /// The member's name, empty for a class entry.
    pub name: String,
    /// `()V` for a method, `I`/`Ljava/lang/String;` for a field. `None` matches
    /// every declared member of `name`, constructors included.
    pub descriptor: Option<String>,
}

/// What a line sets. The `-f` forms also drop `final`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessTransformerAccess {
    Public,
    Protected,
    Private,
    PublicNoFinal,
    ProtectedNoFinal,
    PrivateNoFinal,
}

/// The most text and entries read. A 1 MiB file holds more than 64k of the
/// shortest line (`public a`), so both guards can fire; 64k is far past real
/// transformers.
const ACCESS_TRANSFORMER_TEXT_MAX: usize = 1024 * 1024;
const ACCESS_TRANSFORMER_ENTRIES_MAX: usize = 64 * 1024;

/// Reads a transformer's `.cfg` text. `#` starts a comment, blank lines are
/// skipped, and a member token may carry its descriptor (`run(I)V`) or take it
/// from the next token (`run (I)V`); a descriptor without parentheses is a
/// field's. A member with no descriptor matches every member of that name.
pub fn parse_access_transformer(text: &str) -> Result<AccessTransformer, String> {
    if text.len() > ACCESS_TRANSFORMER_TEXT_MAX {
        return Err(format!(
            "access transformer is {} bytes, the limit is {ACCESS_TRANSFORMER_TEXT_MAX}",
            text.len()
        ));
    }
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.split_once('#').map_or(raw, |(before, _)| before).trim();
        if line.is_empty() {
            continue;
        }
        if entries.len() >= ACCESS_TRANSFORMER_ENTRIES_MAX {
            return Err(format!(
                "line {number}: more than {ACCESS_TRANSFORMER_ENTRIES_MAX} entries"
            ));
        }
        entries.push(access_transformer_entry(line, number)?);
    }
    Ok(AccessTransformer { entries })
}

fn access_transformer_entry(line: &str, number: usize) -> Result<AccessTransformerEntry, String> {
    let mut fields = line.split_whitespace();
    let Some(word) = fields.next() else {
        return Err(format!("line {number}: empty entry"));
    };
    let access = match word {
        "public" => AccessTransformerAccess::Public,
        "protected" => AccessTransformerAccess::Protected,
        "private" => AccessTransformerAccess::Private,
        "public-f" => AccessTransformerAccess::PublicNoFinal,
        "protected-f" => AccessTransformerAccess::ProtectedNoFinal,
        "private-f" => AccessTransformerAccess::PrivateNoFinal,
        _ => return Err(format!("line {number}: `{word}` is not an access value")),
    };
    let Some(class) = fields.next() else {
        return Err(format!("line {number}: the entry names no class"));
    };
    let Some(owner) = access_transformer_class(class) else {
        return Err(format!("line {number}: `{class}` is not a class name"));
    };
    let Some(member) = fields.next() else {
        return Ok(AccessTransformerEntry {
            access,
            owner,
            name: String::new(),
            descriptor: None,
        });
    };
    // `name(desc)` carries the method descriptor inside the member token.
    let (name, inline) = match member.split_once('(') {
        Some((name, rest)) => (name, Some(format!("({rest}"))),
        None => (member, None),
    };
    if !is_member_name(name) {
        return Err(format!("line {number}: `{name}` is not a member name"));
    }
    let descriptor = match inline.or_else(|| fields.next().map(str::to_owned)) {
        None => None,
        Some(descriptor) => {
            let valid = if descriptor.starts_with('(') {
                is_method_descriptor(&descriptor)
            } else {
                is_field_descriptor(&descriptor)
            };
            if !valid {
                return Err(format!("line {number}: `{descriptor}` is not a descriptor"));
            }
            if fields.next().is_some() {
                return Err(format!("line {number}: text after the descriptor"));
            }
            Some(descriptor)
        }
    };
    Ok(AccessTransformerEntry {
        access,
        owner,
        name: name.to_owned(),
        descriptor,
    })
}

/// A bounded member name, as the class file writes it: `run`, `<init>`.
fn is_member_name(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 256
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '<' | '>'))
}

/// Converts an access transformer class name's dots to the JVM's `/`
/// (`net.minecraft.world.level.Level`), rejecting tokens that are not a
/// bounded class name.
fn access_transformer_class(name: &str) -> Option<String> {
    if name.is_empty()
        || name.len() > 512
        || name.starts_with('.')
        || name.ends_with('.')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '.'))
    {
        return None;
    }
    Some(name.replace('.', "/"))
}

/// A JVM field descriptor: a primitive, an array of one, or `L...;`.
fn is_field_descriptor(text: &str) -> bool {
    let base = text.trim_start_matches('[');
    text.len() <= 1024
        && match base.as_bytes().first() {
            Some(b'B' | b'C' | b'D' | b'F' | b'I' | b'J' | b'S' | b'Z') => base.len() == 1,
            Some(b'L') => {
                base.len() > 2 && base.ends_with(';') && is_internal_name(&base[1..base.len() - 1])
            }
            _ => false,
        }
}

/// A JVM method descriptor: `(` parameters `)` return, `V` allowed as return.
fn is_method_descriptor(text: &str) -> bool {
    let Some((arguments, returns)) = text.strip_prefix('(').and_then(|r| r.split_once(')')) else {
        return false;
    };
    text.len() <= 1024
        && is_descriptor(arguments)
        && (returns == "V" || is_field_descriptor(returns))
}

/// Applies a transformer to parsed classes, returning how many declarations'
/// flags changed (a class entry as one, each changed member as one). A target
/// no class or member matches is an error naming it, in file order, like the
/// widener: a transformer that silently changed nothing must not read as
/// applied. A member with no descriptor matches every member of the name,
/// constructors included; a descriptor narrows it to one kind.
pub fn apply_access_transformer(
    classes: &mut [Class],
    transformer: &AccessTransformer,
) -> Result<usize, String> {
    let mut changed = 0;
    for entry in &transformer.entries {
        let Some(class) = classes.iter_mut().find(|class| class.name == entry.owner) else {
            return Err(format!("{}: no class of that name was read", entry.owner));
        };
        if entry.name.is_empty() {
            if transform(&mut class.access, entry.access) {
                changed += 1;
            }
            continue;
        }
        let mut matched = 0;
        for declared in &mut class.declared {
            if declared.name != entry.name {
                continue;
            }
            if let Some(descriptor) = &entry.descriptor {
                let kind = descriptor_kind(descriptor);
                if declared.descriptor != *descriptor || !kind_matches(declared, kind) {
                    continue;
                }
            }
            matched += 1;
            if transform(&mut declared.access, entry.access) {
                changed += 1;
            }
        }
        if matched == 0 {
            return Err(match &entry.descriptor {
                Some(descriptor) => format!(
                    "{}#{} {descriptor}: no such {} declared",
                    entry.owner,
                    entry.name,
                    descriptor_kind(descriptor).label()
                ),
                None => format!("{}#{}: no such member declared", entry.owner, entry.name),
            });
        }
    }
    Ok(changed)
}

/// The kind a descriptor's shape names: `(...)T` is a method, anything else a
/// field.
fn descriptor_kind(descriptor: &str) -> AccessWidenerKind {
    if descriptor.starts_with('(') {
        AccessWidenerKind::Method
    } else {
        AccessWidenerKind::Field
    }
}

/// Sets a declaration's visibility and, for the `-f` forms, drops `final`.
/// Unlike widening this may narrow access. Returns whether anything changed,
/// so a second application counts nothing.
fn transform(access: &mut u16, change: AccessTransformerAccess) -> bool {
    let before = *access;
    *access &= !(ACC_PUBLIC | ACC_PRIVATE | ACC_PROTECTED);
    *access |= match change {
        AccessTransformerAccess::Public | AccessTransformerAccess::PublicNoFinal => ACC_PUBLIC,
        AccessTransformerAccess::Protected | AccessTransformerAccess::ProtectedNoFinal => {
            ACC_PROTECTED
        }
        AccessTransformerAccess::Private | AccessTransformerAccess::PrivateNoFinal => ACC_PRIVATE,
    };
    if matches!(
        change,
        AccessTransformerAccess::PublicNoFinal
            | AccessTransformerAccess::ProtectedNoFinal
            | AccessTransformerAccess::PrivateNoFinal
    ) {
        *access &= !ACC_FINAL;
    }
    *access != before
}

/// Forge's canonical access-transformer path, and the folder beside it.
const ACCESS_TRANSFORMER_PATH: &str = "META-INF/accesstransformer.cfg";
const ACCESS_TRANSFORMER_DIR: &str = "META-INF/accesstransformer/";

/// The jar's access-transformer files: `META-INF/accesstransformer.cfg` and
/// every `META-INF/accesstransformer/*.cfg` beside it. The canonical path is a
/// candidate even when `names` omits it, and a path already present is not
/// repeated; sorted, so the order does not depend on the jar's listing.
pub fn access_transformer_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = names
        .iter()
        .filter(|name| is_access_transformer_path(name))
        .cloned()
        .collect();
    if !out
        .iter()
        .any(|name| name.eq_ignore_ascii_case(ACCESS_TRANSFORMER_PATH))
    {
        out.push(ACCESS_TRANSFORMER_PATH.to_owned());
    }
    out.sort();
    out.dedup();
    out
}

fn is_access_transformer_path(name: &str) -> bool {
    if name.eq_ignore_ascii_case(ACCESS_TRANSFORMER_PATH) {
        return true;
    }
    let Some(rest) = name.strip_prefix(ACCESS_TRANSFORMER_DIR) else {
        return false;
    };
    !rest.is_empty() && !rest.contains('/') && rest.to_ascii_lowercase().ends_with(".cfg")
}

/// The constant pool, with the entries a class file's members and
/// annotations need.
#[derive(Default)]
struct Pool {
    utf8: HashMap<usize, String>,
    class_of: HashMap<usize, usize>,
    ints: HashMap<usize, i32>,
    longs: HashMap<usize, i64>,
    floats: HashMap<usize, f32>,
    doubles: HashMap<usize, f64>,
    /// NameAndType: name, descriptor.
    pairs: HashMap<usize, (usize, usize)>,
    /// Fieldref, Methodref, InterfaceMethodref by pool index: tag, class,
    /// NameAndType.
    refs: HashMap<usize, (u8, usize, usize)>,
    /// MethodHandle by pool index: reference kind, member's pool index.
    handles: HashMap<usize, (u8, usize)>,
    /// Dynamic and InvokeDynamic by pool index: bootstrap method index,
    /// NameAndType.
    dynamic: HashMap<usize, (u16, usize)>,
    /// `BootstrapMethods`, in the attribute's order: the handle's pool index
    /// and its argument pool indexes. Read after the methods, since the
    /// attribute sits at the end of the class file.
    bootstrap: Vec<(usize, Vec<usize>)>,
}

impl Pool {
    fn read(r: &mut Reader) -> Option<Pool> {
        let count = r.u2()? as usize;
        let mut p = Pool::default();
        let mut i = 1;
        while i < count {
            let tag = r.u1()?;
            match tag {
                1 => {
                    let len = r.u2()? as usize;
                    let s = r.take(len)?;
                    p.utf8.insert(i, String::from_utf8_lossy(s).into_owned());
                }
                3 => {
                    p.ints.insert(i, r.u4()? as i32);
                }
                4 => {
                    p.floats.insert(i, f32::from_bits(r.u4()?));
                }
                5 => {
                    let b: [u8; 8] = r.take(8)?.try_into().ok()?;
                    p.longs.insert(i, i64::from_be_bytes(b));
                    i += 1;
                }
                6 => {
                    let b: [u8; 8] = r.take(8)?.try_into().ok()?;
                    p.doubles.insert(i, f64::from_be_bytes(b));
                    i += 1;
                }
                7 => {
                    p.class_of.insert(i, r.u2()? as usize);
                }
                8 | 16 | 19 | 20 => r.skip(2)?,
                9..=11 => {
                    let class = r.u2()? as usize;
                    let pair = r.u2()? as usize;
                    p.refs.insert(i, (tag, class, pair));
                }
                12 => {
                    let name = r.u2()? as usize;
                    let descriptor = r.u2()? as usize;
                    p.pairs.insert(i, (name, descriptor));
                }
                15 => {
                    let kind = r.u1()?;
                    p.handles.insert(i, (kind, r.u2()? as usize));
                }
                17 | 18 => {
                    let bootstrap = r.u2()?;
                    p.dynamic.insert(i, (bootstrap, r.u2()? as usize));
                }
                _ => return None,
            }
            i += 1;
        }
        Some(p)
    }

    fn str(&self, i: u16) -> Option<&str> {
        self.utf8.get(&(i as usize)).map(String::as_str)
    }

    fn utf8_at(&self, i: usize) -> Option<&str> {
        self.utf8.get(&i).map(String::as_str)
    }

    fn class_name(&self, i: u16) -> Option<&str> {
        self.class_of
            .get(&(i as usize))
            .and_then(|u| self.str(*u as u16))
    }

    /// The class a `CONSTANT_Class` pool index names.
    fn class_at(&self, i: usize) -> Option<&str> {
        self.class_of
            .get(&i)
            .and_then(|u| self.utf8.get(u))
            .map(String::as_str)
    }

    fn classes(&self) -> BTreeSet<String> {
        self.class_of
            .values()
            .filter_map(|u| self.utf8.get(u))
            .filter(|s| !s.starts_with('['))
            .cloned()
            .collect()
    }

    fn members(&self) -> Vec<Member> {
        // Pool order, not the map's: a class's members stay in the order its
        // constant pool writes them.
        let mut indexes: Vec<usize> = self.refs.keys().copied().collect();
        indexes.sort_unstable();
        let mut out = Vec::new();
        for index in indexes {
            let &(tag, class, pair) = &self.refs[&index];
            let Some(owner) = self.class_at(class) else {
                continue;
            };
            if owner.starts_with('[') {
                continue;
            }
            let Some(&(name, descriptor)) = self.pairs.get(&pair) else {
                continue;
            };
            let (Some(name), Some(descriptor)) =
                (self.str(name as u16), self.str(descriptor as u16))
            else {
                continue;
            };
            out.push(Member {
                owner: owner.to_owned(),
                name: name.to_owned(),
                descriptor: descriptor.to_owned(),
                kind: match tag {
                    9 => MemberKind::Field,
                    10 => MemberKind::Method,
                    _ => MemberKind::InterfaceMethod,
                },
            });
        }
        out
    }
}

/// The most references one class's bodies contribute, and the most code
/// bytes walked in one method. Both are far past a real class and bounded so
/// a crafted jar cannot make the census allocate at will.
const BODY_REFS_MAX: usize = 100_000;
const METHOD_CODE_MAX: usize = 64 * 1024; // the JVM's own limit on one method
/// The most bootstrap arguments read from one `BootstrapMethods` entry.
const BOOTSTRAP_ARGUMENTS_MAX: usize = 64;

/// Walks one method's `Code` attribute, appending what its instructions name.
/// An error names the class, the method and the offset in the code array. A
/// pool index that names nothing is skipped: the census reads what it can,
/// the JVM's verifier rejects the rest.
fn scan_method_code(
    bytes: &[u8],
    pool: &Pool,
    class: &str,
    method: &str,
    out: &mut Vec<CallTarget>,
) -> Result<(), String> {
    let mut r = Reader { bytes, at: 0 };
    r.skip(4)
        .ok_or_else(|| body_error(class, method, 0, "truncated Code attribute"))?; // max_stack, max_locals
    let length = r
        .u4()
        .ok_or_else(|| body_error(class, method, 0, "truncated Code attribute"))?
        as usize;
    if length > METHOD_CODE_MAX {
        return Err(body_error(
            class,
            method,
            0,
            &format!("code is {length} bytes, the limit is {METHOD_CODE_MAX}"),
        ));
    }
    let code = r
        .take(length)
        .ok_or_else(|| body_error(class, method, 0, "code runs past the attribute"))?;
    scan_instructions(code, pool, class, method, out)
}

fn scan_instructions(
    code: &[u8],
    pool: &Pool,
    class: &str,
    method: &str,
    out: &mut Vec<CallTarget>,
) -> Result<(), String> {
    let mut at = 0;
    while at < code.len() {
        if out.len() >= BODY_REFS_MAX {
            return Ok(());
        }
        let op = code[at];
        let len = match op {
            0x00..=0x0f
            | 0x1a..=0x35
            | 0x3b..=0x83
            | 0x85..=0x98
            | 0xac..=0xb1
            | 0xbe
            | 0xbf
            | 0xc2
            | 0xc3 => 1,
            0x10 | 0x12 | 0x15..=0x19 | 0x36..=0x3a | 0xa9 | 0xbc => 2,
            0x11
            | 0x13
            | 0x14
            | 0x84
            | 0x99..=0xa8
            | 0xb2..=0xb8
            | 0xbb
            | 0xbd
            | 0xc0
            | 0xc1
            | 0xc6
            | 0xc7 => 3,
            0xc5 => 4,
            0xb9 | 0xba | 0xc8 | 0xc9 => 5,
            0xaa => tableswitch_len(code, at, class, method)?,
            0xab => lookupswitch_len(code, at, class, method)?,
            0xc4 => wide_len(code, at, class, method)?,
            _ => {
                return Err(body_error(
                    class,
                    method,
                    at,
                    &format!("unknown opcode {op:#04x}"),
                ));
            }
        };
        if at + len > code.len() {
            return Err(body_error(class, method, at, "operand runs past the code"));
        }
        let index = || u2_be(code, at + 1) as usize;
        match op {
            0x12 => ldc_target(pool, code[at + 1] as usize, out),
            0x13 => ldc_target(pool, index(), out),
            0xb2..=0xb9 => {
                if let Some(target) = member_target(pool, index(), call_kind(op)) {
                    out.push(target);
                }
            }
            0xba => indy_targets(pool, index(), out),
            _ => {}
        }
        at += len;
    }
    Ok(())
}

/// The length of a `tableswitch`: padding to the instruction's 4-byte
/// alignment, then `default` and `low`/`high` and the jump table.
fn tableswitch_len(code: &[u8], at: usize, class: &str, method: &str) -> Result<usize, String> {
    let body = at + 1 + switch_padding(at);
    let past = || body_error(class, method, at, "tableswitch runs past the code");
    let low = i4_be(code, body + 4).ok_or_else(past)?;
    let high = i4_be(code, body + 8).ok_or_else(past)?;
    let count = i64::from(high) - i64::from(low) + 1;
    // `high` must not be below `low`: a high of `low - 1` is an empty table,
    // which the JVM's verifier rejects as well.
    if count < 1 {
        return Err(body_error(class, method, at, "tableswitch high below low"));
    }
    let len = 1 + switch_padding(at) + 12 + count as usize * 4;
    if at + len > code.len() {
        return Err(past());
    }
    Ok(len)
}

/// The length of a `lookupswitch`: padding, `default`, then one
/// match/offset pair per count.
fn lookupswitch_len(code: &[u8], at: usize, class: &str, method: &str) -> Result<usize, String> {
    let body = at + 1 + switch_padding(at);
    let past = || body_error(class, method, at, "lookupswitch runs past the code");
    let pairs = i4_be(code, body + 4).ok_or_else(past)?;
    if pairs < 0 {
        return Err(body_error(
            class,
            method,
            at,
            "lookupswitch pairs below zero",
        ));
    }
    let len = 1 + switch_padding(at) + 8 + pairs as usize * 8;
    if at + len > code.len() {
        return Err(past());
    }
    Ok(len)
}

/// The length of a `wide`: `iinc` carries two more bytes than the rest.
fn wide_len(code: &[u8], at: usize, class: &str, method: &str) -> Result<usize, String> {
    let Some(&op) = code.get(at + 1) else {
        return Err(body_error(class, method, at, "wide runs past the code"));
    };
    match op {
        0x15..=0x19 | 0x36..=0x3a | 0xa9 => Ok(4),
        0x84 => Ok(6),
        _ => Err(body_error(
            class,
            method,
            at,
            &format!("wide of opcode {op:#04x}"),
        )),
    }
}

/// The bytes between an instruction and its 4-byte-aligned operand.
fn switch_padding(at: usize) -> usize {
    (4 - (at + 1) % 4) % 4
}

fn call_kind(op: u8) -> CallKind {
    match op {
        0xb2 => CallKind::GetStatic,
        0xb3 => CallKind::PutStatic,
        0xb4 => CallKind::GetField,
        0xb5 => CallKind::PutField,
        0xb6 => CallKind::Virtual,
        0xb7 => CallKind::Special,
        0xb8 => CallKind::Static,
        _ => CallKind::Interface,
    }
}

/// `ldc`/`ldc_w`: a class literal or a method handle. A `Dynamic` constant
/// (conditional dynamic, `condy`) is a known gap: its bootstrap is not read.
fn ldc_target(pool: &Pool, index: usize, out: &mut Vec<CallTarget>) {
    match pool.class_at(index) {
        Some(owner) if !owner.starts_with('[') && !owner.is_empty() => out.push(CallTarget {
            owner: owner.to_owned(),
            name: String::new(),
            descriptor: String::new(),
            kind: CallKind::Class,
        }),
        Some(_) => {}
        None => {
            if let Some(target) = handle_target(pool, index) {
                out.push(target);
            }
        }
    }
}

fn member_target(pool: &Pool, index: usize, kind: CallKind) -> Option<CallTarget> {
    let &(_, class, pair) = pool.refs.get(&index)?;
    let owner = pool.class_at(class)?;
    if owner.starts_with('[') {
        return None;
    }
    let &(name, descriptor) = pool.pairs.get(&pair)?;
    Some(CallTarget {
        owner: owner.to_owned(),
        name: pool.utf8_at(name)?.to_owned(),
        descriptor: pool.utf8_at(descriptor)?.to_owned(),
        kind,
    })
}

fn handle_target(pool: &Pool, index: usize) -> Option<CallTarget> {
    let &(_, reference) = pool.handles.get(&index)?;
    member_target(pool, reference, CallKind::MethodHandle)
}

/// `invokedynamic`: the bootstrap method handle and the method handles among
/// its arguments (a lambda's body, a method reference).
fn indy_targets(pool: &Pool, index: usize, out: &mut Vec<CallTarget>) {
    let Some(&(bootstrap, _)) = pool.dynamic.get(&index) else {
        return;
    };
    let Some((handle, arguments)) = pool.bootstrap.get(bootstrap as usize) else {
        return;
    };
    if let Some(target) = handle_target(pool, *handle) {
        out.push(target);
    }
    for &argument in arguments {
        if let Some(target) = handle_target(pool, argument) {
            out.push(target);
        }
    }
}

fn body_error(class: &str, method: &str, offset: usize, reason: &str) -> String {
    format!("{class}#{method} at offset {offset}: {reason}")
}

fn u2_be(code: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([code[at], code[at + 1]])
}

fn i4_be(code: &[u8], at: usize) -> Option<i32> {
    let b = code.get(at..at.checked_add(4)?)?;
    Some(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// The `BootstrapMethods` attribute: each bootstrap method's handle and its
/// arguments' pool indexes, in order. A bounded prefix is kept.
fn bootstrap_methods(attributes: &[(String, &[u8])]) -> Vec<(usize, Vec<usize>)> {
    let mut out = Vec::new();
    for (name, bytes) in attributes {
        if name != "BootstrapMethods" {
            continue;
        }
        let mut r = Reader { bytes, at: 0 };
        let Some(n) = r.u2() else { return out };
        for _ in 0..n {
            let (Some(handle), Some(count)) = (r.u2(), r.u2()) else {
                return out;
            };
            let mut arguments = Vec::new();
            for _ in 0..(count as usize).min(BOOTSTRAP_ARGUMENTS_MAX) {
                let Some(argument) = r.u2() else { break };
                arguments.push(argument as usize);
            }
            out.push((handle as usize, arguments));
        }
        break; // only one BootstrapMethods attribute is legal
    }
    out
}

/// A class's, method's or field's attributes: name and bytes.
fn attributes<'a>(r: &mut Reader<'a>, pool: &Pool) -> Option<Vec<(String, &'a [u8])>> {
    let n = r.u2()?;
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let name = pool.str(r.u2()?)?.to_owned();
        let len = r.u4()? as usize;
        out.push((name, r.take(len)?));
    }
    Some(out)
}

/// The `RuntimeVisibleAnnotations` (and invisible ones) an attribute list holds.
fn annotations(attributes: &[(String, &[u8])], pool: &Pool) -> Vec<Annotation> {
    let mut out = Vec::new();
    for (name, bytes) in attributes {
        if name != "RuntimeVisibleAnnotations" && name != "RuntimeInvisibleAnnotations" {
            continue;
        }
        let mut r = Reader { bytes, at: 0 };
        let Some(n) = r.u2() else { continue };
        for _ in 0..n {
            let Some(a) = annotation(&mut r, pool) else {
                break;
            };
            out.push(a);
        }
    }
    out
}

fn annotation(r: &mut Reader, pool: &Pool) -> Option<Annotation> {
    let ty = pool.str(r.u2()?)?.to_owned();
    let n = r.u2()?;
    let mut values = BTreeMap::new();
    for _ in 0..n {
        let name = pool.str(r.u2()?)?.to_owned();
        values.insert(name, value(r, pool)?);
    }
    Some(Annotation { ty, values })
}

fn value(r: &mut Reader, pool: &Pool) -> Option<Value> {
    let tag = r.u1()?;
    Some(match tag {
        b'B' | b'C' | b'S' | b'I' => {
            Value::Number(pool.ints.get(&(r.u2()? as usize)).copied()? as f64)
        }
        b'Z' => Value::Bool(pool.ints.get(&(r.u2()? as usize)).copied()? != 0),
        b'J' => Value::Number(pool.longs.get(&(r.u2()? as usize)).copied()? as f64),
        b'F' => Value::Number(pool.floats.get(&(r.u2()? as usize)).copied()? as f64),
        b'D' => Value::Number(pool.doubles.get(&(r.u2()? as usize)).copied()?),
        b's' => Value::Str(pool.str(r.u2()?)?.to_owned()),
        b'e' => {
            let ty = pool.str(r.u2()?)?.to_owned();
            let name = pool.str(r.u2()?)?.to_owned();
            Value::Enum(ty, name)
        }
        b'c' => Value::Type(pool.str(r.u2()?)?.to_owned()),
        b'@' => Value::Ann(annotation(r, pool)?),
        b'[' => {
            let n = r.u2()?;
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                items.push(value(r, pool)?);
            }
            Value::Array(items)
        }
        _ => return None,
    })
}

/// `@Mixin(Foo.class)` and `@Mixin(targets = "a.B")`.
fn mixin_targets(annotations: &[Annotation]) -> Vec<String> {
    let mut out = Vec::new();
    for a in annotations.iter().filter(|a| a.ty == MIXIN) {
        for key in ["value", "targets"] {
            let Some(v) = a.values.get(key) else { continue };
            for item in items(v) {
                match item {
                    Value::Type(t) => {
                        let name = t.trim_matches(|c| c == 'L' || c == '[' || c == ';');
                        out.push(name.to_owned());
                    }
                    Value::Str(s) => out.push(s.replace('.', "/")),
                    _ => {}
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn items(value: &Value) -> &[Value] {
    match value {
        Value::Array(v) => v,
        other => std::slice::from_ref(other),
    }
}

/// The mixin annotations a method or field carries, as injections.
fn injections(
    methods: &BTreeMap<String, Vec<Annotation>>,
    fields: &BTreeMap<String, Vec<Annotation>>,
) -> Vec<Injection> {
    let mut out = Vec::new();
    for (name, annotations) in methods.iter().chain(fields) {
        for a in annotations {
            let Some(kind) = injection_kind(&a.ty) else {
                continue;
            };
            let method = a
                .values
                .get("method")
                .map(|v| {
                    items(v)
                        .iter()
                        .filter_map(text)
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .unwrap_or_default();
            let at = a
                .values
                .get("at")
                .map(|v| {
                    items(v)
                        .iter()
                        .filter_map(at_value)
                        .collect::<Vec<_>>()
                        .join("|")
                })
                .unwrap_or_default();
            let metadata_values: BTreeMap<String, String> = [
                "require",
                "expect",
                "cancellable",
                "order",
                "priority",
                "slice",
            ]
            .into_iter()
            .filter_map(|field| {
                a.values
                    .get(field)
                    .and_then(metadata_text)
                    .map(|value| (field.to_owned(), value))
            })
            .collect();
            let group = annotations
                .iter()
                .find(|annotation| annotation_simple(annotation.ty.as_str()) == "Group")
                .and_then(group_metadata);
            let mut metadata_values = metadata_values;
            if let Some(group) = group {
                metadata_values.insert("group".to_owned(), group);
            }
            let mut metadata = [
                "require",
                "expect",
                "cancellable",
                "order",
                "priority",
                "slice",
            ]
            .into_iter()
            .filter(|field| a.values.contains_key(*field))
            .map(str::to_owned)
            .collect::<Vec<_>>();
            if metadata_values.contains_key("group") {
                metadata.push("group".to_owned());
            }
            out.push(Injection {
                kind: kind.to_owned(),
                method: if method.is_empty() {
                    name.clone()
                } else {
                    method
                },
                at,
                metadata,
                metadata_values,
            });
        }
    }
    out
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Enum(_, name) => Some(name.clone()),
        _ => None,
    }
}

/// The longest `@At` text kept: a longer selector is cut at a character
/// boundary and marked with `…`, so no report carries an unbounded string.
const AT_TEXT_MAX: usize = 512;

fn at_value(value: &Value) -> Option<String> {
    let Value::Ann(a) = value else { return None };
    let mut out = Vec::new();
    if let Some(v) = a.values.get("value") {
        out.extend(items(v).iter().filter_map(text));
    }
    if let Some(v) = a.values.get("target") {
        out.extend(
            items(v)
                .iter()
                .filter_map(|v| text(v).map(|t| format!("target:{t}"))),
        );
    }
    if let Some(v) = a.values.get("args") {
        let args = items(v)
            .iter()
            .filter_map(scalar_text)
            .collect::<Vec<_>>()
            .join(",");
        out.push(format!("args:{args}"));
    }
    for key in ["ordinal", "shift", "by"] {
        if let Some(v) = a.values.get(key) {
            out.extend(
                items(v)
                    .iter()
                    .filter_map(scalar_text)
                    .map(|t| format!("{key}:{t}")),
            );
        }
    }
    // Added after the keys above so existing text keeps its shape.
    for key in ["desc", "id"] {
        if let Some(v) = a.values.get(key) {
            out.extend(
                items(v)
                    .iter()
                    .filter_map(|v| text(v).map(|t| format!("{key}:{t}"))),
            );
        }
    }
    // `remap = false` is `@At`'s default: only an explicit true is worth text.
    if a.values
        .get("remap")
        .is_some_and(|v| items(v).iter().any(|v| matches!(v, Value::Bool(true))))
    {
        out.push("remap:true".to_owned());
    }
    // Mixin Extras' `@Local`/`@Share` selectors are nested annotations: as
    // with any annotation this module does not model, presence is the text.
    for key in ["local", "share"] {
        if let Some(v) = a.values.get(key) {
            out.extend(
                items(v)
                    .iter()
                    .filter_map(|v| metadata_text(v).map(|t| format!("{key}:{t}"))),
            );
        }
    }
    Some(bounded_text(out.join("|")))
}

/// Cuts `text` to `AT_TEXT_MAX` characters, marking the cut with `…` so a
/// truncated selector is not mistaken for one that ends there.
fn bounded_text(text: String) -> String {
    if text.chars().count() <= AT_TEXT_MAX {
        return text;
    }
    let mut out: String = text.chars().take(AT_TEXT_MAX).collect();
    out.push('…');
    out
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(number) if number.fract() == 0.0 => Some(format!("{number:.0}")),
        Value::Number(number) => Some(number.to_string()),
        other => text(other),
    }
}

fn metadata_text(value: &Value) -> Option<String> {
    if let Value::Ann(annotation) = value {
        if annotation_simple(&annotation.ty) == "Slice" {
            let from = annotation.values.get("from").and_then(boundary_text);
            let to = annotation.values.get("to").and_then(boundary_text);
            if from.is_some() || to.is_some() {
                return Some(format!(
                    "from:{}|to:{}",
                    from.as_deref().unwrap_or(""),
                    to.as_deref().unwrap_or("")
                ));
            }
        }
        return Some("present".to_owned());
    }
    scalar_text(value)
}

/// A `@Slice` boundary: an `@At` renders through `at_value` (so a selector
/// like `from = @At(value = "INVOKE", target = "...")` keeps its target), a
/// string or enum names an anchor directly (`HEAD`, `TAIL`).
fn boundary_text(value: &Value) -> Option<String> {
    match value {
        Value::Ann(_) => at_value(value),
        other => text(other),
    }
}

fn annotation_simple(ty: &str) -> &str {
    ty.trim_start_matches('L')
        .trim_end_matches(';')
        .rsplit('/')
        .next()
        .unwrap_or(ty)
}

fn group_metadata(annotation: &Annotation) -> Option<String> {
    let mut values = Vec::new();
    for key in ["name", "min", "max", "log"] {
        if let Some(value) = annotation.values.get(key).and_then(scalar_text) {
            values.push(format!("{key}:{value}"));
        }
    }
    Some(values.join("|"))
}

/// The simple name of a Mixin annotation, if it is one.
fn injection_kind(ty: &str) -> Option<&'static str> {
    let simple = ty
        .trim_start_matches('L')
        .trim_end_matches(';')
        .rsplit('/')
        .next()?;
    Some(match simple {
        "Inject" => "inject",
        "Redirect" => "redirect",
        "ModifyVariable" => "modify-variable",
        "ModifyArg" => "modify-arg",
        "ModifyArgs" => "modify-args",
        "ModifyConstant" => "modify-constant",
        // Mixin Extras uses the singular `ModifyConst` spelling in older
        // releases; normalize it to the same report bucket as Mixin's
        // `ModifyConstant` injector.
        "ModifyConst" => "modify-constant",
        "ModifyExpressionValue" => "modify-expression-value",
        "ModifyReceiver" => "modify-receiver",
        "ModifyReturnValue" => "modify-return-value",
        "WrapOperation" => "wrap-operation",
        "WrapMethod" => "wrap-method",
        "WrapWithCondition" => "wrap-with-condition",
        "Overwrite" => "overwrite",
        "Accessor" => "accessor",
        "Invoker" => "invoker",
        "Shadow" => "shadow",
        _ => return None,
    })
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(drop)
    }
    fn u1(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u2(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u4(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// What a class does, as the port sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Feature {
    /// A block with behaviour of its own (use, ticks, collisions...).
    BlockBehaviour,
    /// A block with a shape of its own: it ports with its template's.
    BlockShape,
    /// An item with behaviour of its own (use, on hit...).
    ItemBehaviour,
    BlockEntity,
    Menu,
    Entity,
    Screen,
    Renderer,
    /// A worldgen feature, placement or structure type written in Java.
    Worldgen,
    /// A world or level implementation that reads or mutates simulation state.
    WorldAccess,
    /// A recipe type (its recipes don't craft) or ingredient type.
    Recipe,
    Effect,
    Enchantment,
    Particle,
    Command,
    Network,
    /// Reacts to game events (NeoForge's bus, Fabric's callbacks).
    Events,
    /// A Mixin: a patch to the game's own code.
    Mixin,
    /// Extends a game class under obfuscated names (Fabric before 26.1,
    /// Forge's methods): what it does can't be read.
    Unread,
}

impl Feature {
    /// How the coverage line names `n` of them.
    pub fn describe(self, n: usize) -> String {
        let (one, many) = match self {
            Feature::BlockBehaviour => (
                "block with behaviour of its own (use, ticks, collisions)",
                "blocks with behaviour of their own (use, ticks, collisions)",
            ),
            Feature::BlockShape => (
                "block with a shape of its own: it ports with its template's",
                "blocks with shapes of their own: they port with their templates'",
            ),
            Feature::ItemBehaviour => (
                "item with behaviour of its own (use, on hit)",
                "items with behaviour of their own (use, on hit)",
            ),
            Feature::BlockEntity => (
                "block entity (machine, container or animated block)",
                "block entities (machines, containers, animated blocks)",
            ),
            Feature::Menu => ("menu (a container screen)", "menus (container screens)"),
            Feature::Entity => ("mob or entity", "mobs or entities"),
            Feature::Screen => ("screen of its own", "screens of its own"),
            Feature::Renderer => ("renderer or model in code", "renderers or models in code"),
            Feature::Worldgen => (
                "worldgen feature or structure type in code",
                "worldgen features or structure types in code",
            ),
            Feature::WorldAccess => (
                "world or level implementation",
                "world or level implementations",
            ),
            Feature::Recipe => (
                "recipe type of its own (its recipes don't craft)",
                "recipe types of its own (their recipes don't craft)",
            ),
            Feature::Effect => ("status effect", "status effects"),
            Feature::Enchantment => ("enchantment effect in code", "enchantment effects in code"),
            Feature::Particle => ("particle type", "particle types"),
            Feature::Command => ("command", "commands"),
            Feature::Network => ("network message", "network messages"),
            Feature::Events => ("game event it reacts to", "game events it reacts to"),
            Feature::Mixin => (
                "Mixin (a patch to the game's code)",
                "Mixins (patches to the game's code)",
            ),
            Feature::Unread => (
                "class extending the game under obfuscated names",
                "classes extending the game under obfuscated names",
            ),
        };
        format!("{n} {}", if n == 1 { one } else { many })
    }
}

/// Block methods that only say how it looks or is placed: the template's
/// do the same for the states a blockstate file lists.
const BLOCK_LOOKS: [&str; 18] = [
    "codec",
    "createBlockStateDefinition",
    "getStateForPlacement",
    "rotate",
    "mirror",
    "getRenderShape",
    "skipRendering",
    "getShadeBrightness",
    "propagatesSkylightDown",
    "getCloneItemStack",
    "isPathfindable",
    "getFluidState",
    "updateShape",
    "canBeReplaced",
    "getLightBlock",
    "useShapeForLightOcclusion",
    "getDescriptionId",
    "animateTick",
];
/// Block methods that only change its outline or collision.
const BLOCK_SHAPES: [&str; 6] = [
    "getShape",
    "getCollisionShape",
    "getVisualShape",
    "getOcclusionShape",
    "getInteractionShape",
    "getBlockSupportShape",
];
/// Item methods that only say how it looks or is named.
const ITEM_LOOKS: [&str; 6] = [
    "appendHoverText",
    "getName",
    "isFoil",
    "getDescriptionId",
    "getBarColor",
    "getBarWidth",
];

/// The game's own, loader and library classes: the first of these a mod's
/// class chain reaches is what it extends.
fn external(name: &str, own: &BTreeMap<String, Class>) -> bool {
    !own.contains_key(name)
}

fn obfuscated_method(m: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    m.strip_prefix("method_").is_some_and(digits)
        || m.strip_prefix("m_")
            .and_then(|s| s.strip_suffix('_'))
            .is_some_and(digits)
}

/// What a game class (Mojang's names) is, by its package and name.
fn kind_of_game_class(name: &str) -> Option<Feature> {
    let rest = name.strip_prefix("net/minecraft/")?;
    if rest.starts_with("class_") {
        return Some(Feature::Unread);
    }
    let (package, simple) = rest.rsplit_once('/').unwrap_or(("", rest));
    Some(match package {
        "world/level/block/entity" => Feature::BlockEntity,
        p if p.starts_with("world/level/block") => Feature::BlockBehaviour,
        "world/inventory" if simple.ends_with("Menu") => Feature::Menu,
        p if p.starts_with("world/entity") => Feature::Entity,
        p if p.starts_with("world/item/crafting") => Feature::Recipe,
        p if p.starts_with("world/item/enchantment") => Feature::Enchantment,
        p if p.starts_with("world/item") => Feature::ItemBehaviour,
        p if p.starts_with("world/level/levelgen") => Feature::Worldgen,
        "world/level" => Feature::WorldAccess,
        p if p.starts_with("world/level/") => Feature::WorldAccess,
        "world/effect" => Feature::Effect,
        p if p.starts_with("client/gui/screens") || p.starts_with("client/gui/components") => {
            Feature::Screen
        }
        p if p.starts_with("client/renderer") || p.starts_with("client/model") => Feature::Renderer,
        p if p.starts_with("client/particle") || p.starts_with("core/particles") => {
            Feature::Particle
        }
        _ => return None,
    })
}

/// Events a content mod uses to register what it has: not behaviour.
fn registration_event(name: &str) -> bool {
    [
        "BuildCreativeModeTabContentsEvent",
        "RegisterEvent",
        "Register",
        "Lifecycle",
        "ModConfigEvent",
        "AddPackFindersEvent",
        "ItemGroupEvents",
        "ClientLifecycleEvents",
        "ServerLifecycleEvents",
    ]
    .iter()
    .any(|k| name.rsplit('/').next().unwrap_or(name).contains(k))
        || name.contains("/lifecycle/")
        || name.contains("/registries/")
        || name.contains("/registry/")
}

/// The code census: how many of each feature, and examples.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Code {
    pub classes: usize,
    pub features: BTreeMap<Feature, usize>,
    /// The game events it subscribes to, by simple name.
    pub events: BTreeSet<String>,
    /// Blocks and items with behaviour, and the methods they override.
    pub examples: BTreeMap<String, Vec<String>>,
}

impl Code {
    /// Whether it does anything beyond registering blocks and items.
    pub fn behaviour(&self) -> bool {
        self.features
            .keys()
            .any(|f| *f != Feature::BlockShape && *f != Feature::Particle)
    }
}

/// Reads every class of a jar. `classes` are the class files' bytes.
pub fn census<'a>(classes: impl IntoIterator<Item = &'a [u8]>) -> Code {
    let own: BTreeMap<String, Class> = classes
        .into_iter()
        .filter_map(parse)
        .map(|c| (c.name.clone(), c))
        .collect();
    let mut code = Code {
        classes: own.len(),
        ..Code::default()
    };
    let mut count = |f: Feature| *code.features.entry(f).or_default() += 1;
    for c in own.values() {
        if c.mixin {
            count(Feature::Mixin);
            continue;
        }
        // The first class outside the jar the chain reaches, and the
        // methods declared along the way.
        let mut methods: BTreeSet<&str> = c.methods.iter().map(String::as_str).collect();
        let mut at = c;
        let mut seen = 0;
        while !external(&at.superclass, &own) && seen < 64 {
            at = &own[&at.superclass];
            methods.extend(at.methods.iter().map(String::as_str));
            seen += 1;
        }
        let base = at.superclass.as_str();
        let kind = kind_of_game_class(base).or_else(|| {
            c.interfaces.iter().find_map(|i| match i.as_str() {
                "net/minecraft/network/protocol/common/custom/CustomPacketPayload" => {
                    Some(Feature::Network)
                }
                "net/minecraft/world/MenuProvider" => Some(Feature::Menu),
                _ => None,
            })
        });
        let Some(kind) = kind else {
            continue;
        };
        let feature = match kind {
            Feature::BlockBehaviour => {
                let behaviour: Vec<&str> = methods
                    .iter()
                    .copied()
                    .filter(|m| !BLOCK_LOOKS.contains(m) && !BLOCK_SHAPES.contains(m))
                    .collect();
                if !behaviour.is_empty() {
                    code.examples.insert(
                        c.name.rsplit('/').next().unwrap_or("").to_owned(),
                        behaviour.iter().map(|m| m.to_string()).collect(),
                    );
                    if behaviour.iter().any(|m| obfuscated_method(m)) {
                        Some(Feature::Unread)
                    } else {
                        Some(Feature::BlockBehaviour)
                    }
                } else if methods.iter().any(|m| BLOCK_SHAPES.contains(m)) {
                    Some(Feature::BlockShape)
                } else {
                    None
                }
            }
            Feature::ItemBehaviour => {
                let behaviour: Vec<&str> = methods
                    .iter()
                    .copied()
                    .filter(|m| !ITEM_LOOKS.contains(m))
                    .collect();
                if behaviour.is_empty() {
                    None
                } else {
                    code.examples.insert(
                        c.name.rsplit('/').next().unwrap_or("").to_owned(),
                        behaviour.iter().map(|m| m.to_string()).collect(),
                    );
                    Some(Feature::ItemBehaviour)
                }
            }
            // A class extending an obfuscated game class without declaring
            // anything adds nothing to read.
            Feature::Unread if methods.is_empty() => None,
            k => Some(k),
        };
        if let Some(f) = feature {
            count(f);
        }
    }
    for c in own.values() {
        for r in &c.refs {
            let event = (r.starts_with("net/neoforged/neoforge/event/")
                || r.starts_with("net/minecraftforge/event/")
                || r.starts_with("net/fabricmc/fabric/api/event/")
                || r.starts_with("net/fabricmc/fabric/api/entity/event/"))
                && !registration_event(r);
            if event {
                let simple = r.rsplit('/').next().unwrap_or(r);
                code.events
                    .insert(simple.split('$').next().unwrap_or(simple).to_owned());
            }
            if r.starts_with("com/mojang/brigadier/")
                && !code.features.contains_key(&Feature::Command)
            {
                code.features.insert(Feature::Command, 1);
            }
        }
    }
    if !code.events.is_empty() {
        code.features.insert(Feature::Events, code.events.len());
    }
    code
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// A class file with `name` extending `superclass`, declaring `methods`
    /// and naming `refs` in its constant pool.
    pub fn class_with(
        name: &str,
        superclass: &str,
        methods: &[&str],
        refs: &[&str],
        mixin: bool,
    ) -> Vec<u8> {
        let mut pool: Vec<Vec<u8>> = Vec::new();
        let utf = |pool: &mut Vec<Vec<u8>>, s: &str| -> u16 {
            let mut e = vec![1];
            e.extend((s.len() as u16).to_be_bytes());
            e.extend(s.as_bytes());
            pool.push(e);
            pool.len() as u16
        };
        let class = |pool: &mut Vec<Vec<u8>>, s: &str| -> u16 {
            let u = utf(pool, s);
            let mut e = vec![7];
            e.extend(u.to_be_bytes());
            pool.push(e);
            pool.len() as u16
        };
        let this = class(&mut pool, name);
        let sup = class(&mut pool, superclass);
        for r in refs {
            class(&mut pool, r);
        }
        // A long takes two slots.
        pool.push(vec![5, 0, 0, 0, 0, 0, 0, 0, 1]);
        pool.push(Vec::new());
        if mixin {
            utf(&mut pool, "Lorg/spongepowered/asm/mixin/Mixin;");
        }
        let desc = utf(&mut pool, "()V");
        let names: Vec<u16> = methods.iter().map(|m| utf(&mut pool, m)).collect();
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
        out.extend(((pool.len() + 1) as u16).to_be_bytes());
        for e in &pool {
            out.extend(e);
        }
        out.extend([0, 0x21]);
        out.extend(this.to_be_bytes());
        out.extend(sup.to_be_bytes());
        out.extend([0, 0]); // interfaces
        out.extend([0, 0]); // fields
        out.extend((names.len() as u16).to_be_bytes());
        for n in names {
            out.extend([0, 1]);
            out.extend(n.to_be_bytes());
            out.extend(desc.to_be_bytes());
            // One attribute, to be skipped.
            out.extend([0, 1, 0, 1, 0, 0, 0, 2, 0, 0]);
        }
        out.extend([0, 0]); // class attributes
        out
    }

    /// A class extending `superclass` and declaring nothing.
    pub fn class_file(name: &str, superclass: &str) -> Vec<u8> {
        class_with(name, superclass, &[], &[], false)
    }

    /// A constant pool under construction, for the class files whose method
    /// bodies the scanner tests walk: each method returns the 1-based index a
    /// code array refers to, in the order a class file writes the entries.
    #[derive(Default)]
    pub struct PoolBuilder {
        entries: Vec<Vec<u8>>,
        bootstrap: Vec<(u16, Vec<u16>)>,
    }

    impl PoolBuilder {
        /// Appends one entry and returns its 1-based index.
        fn add(&mut self, tag: u8, payload: &[u8]) -> u16 {
            let mut entry = vec![tag];
            entry.extend(payload);
            self.entries.push(entry);
            self.entries.len() as u16
        }

        pub fn utf8(&mut self, text: &str) -> u16 {
            let mut payload = (text.len() as u16).to_be_bytes().to_vec();
            payload.extend(text.as_bytes());
            self.add(1, &payload)
        }

        pub fn class(&mut self, name: &str) -> u16 {
            let name = self.utf8(name);
            self.add(7, &name.to_be_bytes())
        }

        pub fn integer(&mut self, value: i32) -> u16 {
            self.add(3, &value.to_be_bytes())
        }

        fn name_and_type(&mut self, name: &str, descriptor: &str) -> u16 {
            let name = self.utf8(name);
            let descriptor = self.utf8(descriptor);
            let mut payload = name.to_be_bytes().to_vec();
            payload.extend(descriptor.to_be_bytes());
            self.add(12, &payload)
        }

        fn member(&mut self, tag: u8, owner: &str, name: &str, descriptor: &str) -> u16 {
            let owner = self.class(owner);
            let pair = self.name_and_type(name, descriptor);
            let mut payload = owner.to_be_bytes().to_vec();
            payload.extend(pair.to_be_bytes());
            self.add(tag, &payload)
        }

        /// A `CONSTANT_Fieldref`.
        pub fn field(&mut self, owner: &str, name: &str, descriptor: &str) -> u16 {
            self.member(9, owner, name, descriptor)
        }

        /// A `CONSTANT_Methodref`.
        pub fn method(&mut self, owner: &str, name: &str, descriptor: &str) -> u16 {
            self.member(10, owner, name, descriptor)
        }

        /// A `CONSTANT_InterfaceMethodref`.
        pub fn interface_method(&mut self, owner: &str, name: &str, descriptor: &str) -> u16 {
            self.member(11, owner, name, descriptor)
        }

        /// A `CONSTANT_MethodHandle` of `kind` over `reference`.
        pub fn handle(&mut self, kind: u8, reference: u16) -> u16 {
            let mut payload = vec![kind];
            payload.extend(reference.to_be_bytes());
            self.add(15, &payload)
        }

        /// An `invokedynamic` constant over a fresh bootstrap entry: its
        /// method handle and arguments.
        pub fn dynamic(
            &mut self,
            name: &str,
            descriptor: &str,
            handle: u16,
            arguments: &[u16],
        ) -> u16 {
            let bootstrap = self.bootstrap.len() as u16;
            self.bootstrap.push((handle, arguments.to_vec()));
            let pair = self.name_and_type(name, descriptor);
            let mut payload = bootstrap.to_be_bytes().to_vec();
            payload.extend(pair.to_be_bytes());
            self.add(18, &payload)
        }

        /// The class file: `name` extending `superclass`, one public static
        /// `()V` method per `(name, code)` with a `Code` attribute around
        /// `code`, and the `BootstrapMethods` its dynamics need.
        pub fn build(
            &mut self,
            name: &str,
            superclass: &str,
            methods: &[(&str, &[u8])],
        ) -> Vec<u8> {
            let this = self.class(name);
            let sup = self.class(superclass);
            let code_attribute = self.utf8("Code");
            let descriptor = self.utf8("()V");
            let names: Vec<u16> = methods
                .iter()
                .map(|(method, _)| self.utf8(method))
                .collect();
            let bootstrap_attribute = if self.bootstrap.is_empty() {
                None
            } else {
                Some(self.utf8("BootstrapMethods"))
            };
            let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
            out.extend(((self.entries.len() + 1) as u16).to_be_bytes());
            for entry in &self.entries {
                out.extend(entry);
            }
            out.extend([0, 0x21]);
            out.extend(this.to_be_bytes());
            out.extend(sup.to_be_bytes());
            out.extend([0, 0, 0, 0]); // interfaces, fields
            out.extend((methods.len() as u16).to_be_bytes());
            for ((_, code), name) in methods.iter().zip(&names) {
                out.extend([0, 0x09]); // public static
                out.extend(name.to_be_bytes());
                out.extend(descriptor.to_be_bytes());
                let mut attribute = vec![0, 0, 0, 1]; // max_stack, max_locals
                attribute.extend((code.len() as u32).to_be_bytes());
                attribute.extend(*code);
                attribute.extend([0, 0]); // exception table
                attribute.extend([0, 0]); // code attributes
                out.extend([0, 1]);
                out.extend(code_attribute.to_be_bytes());
                out.extend((attribute.len() as u32).to_be_bytes());
                out.extend(attribute);
            }
            match bootstrap_attribute {
                Some(name) => {
                    out.extend([0, 1]);
                    out.extend(name.to_be_bytes());
                    let mut attribute = (self.bootstrap.len() as u16).to_be_bytes().to_vec();
                    for (handle, arguments) in &self.bootstrap {
                        attribute.extend(handle.to_be_bytes());
                        attribute.extend((arguments.len() as u16).to_be_bytes());
                        for argument in arguments {
                            attribute.extend(argument.to_be_bytes());
                        }
                    }
                    out.extend((attribute.len() as u32).to_be_bytes());
                    out.extend(attribute);
                }
                None => out.extend([0, 0]),
            }
            out
        }
    }

    /// A tiny class carrying a real `@Mixin(targets=...)` and method
    /// `@Inject(method=..., at=@At(...))` annotation payload. Used by the
    /// census fixture to exercise class-file annotation decoding.
    pub fn annotated_mixin(name: &str, target: &str, method: &str) -> Vec<u8> {
        let mut pool: Vec<Vec<u8>> = Vec::new();
        let utf = |pool: &mut Vec<Vec<u8>>, s: &str| -> u16 {
            let mut e = vec![1];
            e.extend((s.len() as u16).to_be_bytes());
            e.extend(s.as_bytes());
            pool.push(e);
            pool.len() as u16
        };
        let class = |pool: &mut Vec<Vec<u8>>, s: &str| -> u16 {
            let u = utf(pool, s);
            let mut e = vec![7];
            e.extend(u.to_be_bytes());
            pool.push(e);
            pool.len() as u16
        };
        let this = class(&mut pool, name);
        let sup = class(&mut pool, "java/lang/Object");
        let mixin_ty = utf(&mut pool, "Lorg/spongepowered/asm/mixin/Mixin;");
        let targets_name = utf(&mut pool, "targets");
        let target_str = utf(&mut pool, target);
        let inject_ty = utf(&mut pool, "Lorg/spongepowered/asm/mixin/inject/Inject;");
        let method_name = utf(&mut pool, "method");
        let method_str = utf(&mut pool, method);
        let at_name = utf(&mut pool, "at");
        let at_ty = utf(&mut pool, "Lorg/spongepowered/asm/mixin/injection/At;");
        let value_name = utf(&mut pool, "value");
        let head = utf(&mut pool, "HEAD");
        let target_name = utf(&mut pool, "target");
        let at_target = utf(&mut pool, &format!("L{target};{method}"));
        let slice_name = utf(&mut pool, "slice");
        let slice_ty = utf(&mut pool, "Lorg/spongepowered/asm/mixin/injection/Slice;");
        let from_name = utf(&mut pool, "from");
        let to_name = utf(&mut pool, "to");
        let invoke = utf(&mut pool, "INVOKE");
        let return_anchor = utf(&mut pool, "RETURN");
        let runtime_annotations = utf(&mut pool, "RuntimeVisibleAnnotations");
        let run = utf(&mut pool, "run");
        let desc = utf(&mut pool, "()V");
        let mut out = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 65];
        out.extend(((pool.len() + 1) as u16).to_be_bytes());
        for e in &pool {
            out.extend(e);
        }
        out.extend([0, 0x21]);
        out.extend(this.to_be_bytes());
        out.extend(sup.to_be_bytes());
        out.extend([0, 0, 0, 0]); // interfaces, fields
        out.extend([0, 1]); // one method
        out.extend([0, 1]);
        out.extend(run.to_be_bytes());
        out.extend(desc.to_be_bytes());
        let mut ma = Vec::new();
        ma.extend([0, 1]); // one annotation
        ma.extend(inject_ty.to_be_bytes());
        ma.extend([0, 3]); // method, at, slice
        ma.extend(method_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(method_str.to_be_bytes());
        ma.extend(at_name.to_be_bytes());
        ma.extend(*b"@");
        ma.extend(at_ty.to_be_bytes());
        ma.extend([0, 2]);
        ma.extend(value_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(head.to_be_bytes());
        ma.extend(target_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(at_target.to_be_bytes());
        // `slice = @Slice(from = @At(value = "INVOKE", target = ...),
        //                 to = @At("RETURN"))`: a nested annotation inside a
        // nested annotation, the shape real mods carry.
        ma.extend(slice_name.to_be_bytes());
        ma.extend(*b"@");
        ma.extend(slice_ty.to_be_bytes());
        ma.extend([0, 2]);
        ma.extend(from_name.to_be_bytes());
        ma.extend(*b"@");
        ma.extend(at_ty.to_be_bytes());
        ma.extend([0, 2]);
        ma.extend(value_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(invoke.to_be_bytes());
        ma.extend(target_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(at_target.to_be_bytes());
        ma.extend(to_name.to_be_bytes());
        ma.extend(*b"@");
        ma.extend(at_ty.to_be_bytes());
        ma.extend([0, 1]);
        ma.extend(value_name.to_be_bytes());
        ma.extend(*b"s");
        ma.extend(return_anchor.to_be_bytes());
        out.extend([0, 1]);
        out.extend(runtime_annotations.to_be_bytes());
        out.extend((ma.len() as u32).to_be_bytes());
        out.extend(ma);
        out.extend([0, 1]); // one class attribute
        out.extend(runtime_annotations.to_be_bytes());
        let mut ca = Vec::new();
        ca.extend([0, 1]);
        ca.extend(mixin_ty.to_be_bytes());
        ca.extend([0, 1]);
        ca.extend(targets_name.to_be_bytes());
        ca.extend(*b"s");
        ca.extend(target_str.to_be_bytes());
        out.extend((ca.len() as u32).to_be_bytes());
        out.extend(ca);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::{PoolBuilder, class_with as class_file};
    use super::*;

    const AT: &str = "Lorg/spongepowered/asm/mixin/injection/At;";
    const SLICE: &str = "Lorg/spongepowered/asm/mixin/injection/Slice;";

    /// A nested annotation value, built directly: the same model `value`
    /// produces from `element_value` tags `@` and `[`.
    fn ann(ty: &str, values: Vec<(&str, Value)>) -> Value {
        Value::Ann(Annotation {
            ty: ty.to_owned(),
            values: values
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        })
    }

    fn s(text: &str) -> Value {
        Value::Str(text.to_owned())
    }

    #[test]
    fn reads_the_header_methods_and_pool() {
        let bytes = class_file(
            "a/Lamp",
            "net/minecraft/world/level/block/Block",
            &["<init>", "useWithoutItem", "lambda$x$0"],
            &["net/minecraft/world/item/Item"],
            false,
        );
        let c = parse(&bytes).unwrap();
        assert_eq!(c.name, "a/Lamp");
        assert_eq!(c.superclass, "net/minecraft/world/level/block/Block");
        assert_eq!(c.methods, ["useWithoutItem"]);
        assert!(c.refs.contains("net/minecraft/world/item/Item"));
        assert!(!c.mixin);
        assert!(parse(&bytes[..bytes.len() - 3]).is_none());
        assert!(parse(b"not a class").is_none());
    }

    #[test]
    fn normalizes_mixin_extras_injector_names() {
        assert_eq!(
            injection_kind("Lcom/llamalad7/mixinextras/injector/WrapMethod;"),
            Some("wrap-method")
        );
        assert_eq!(
            injection_kind("Lcom/llamalad7/mixinextras/injector/ModifyConst;"),
            Some("modify-constant")
        );
    }

    #[test]
    fn at_metadata_preserves_ordinal_shift_and_by() {
        let at = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/At;".to_owned(),
            values: BTreeMap::from([
                ("value".to_owned(), Value::Str("INVOKE".to_owned())),
                (
                    "target".to_owned(),
                    Value::Str("Lnet/minecraft/Target;method()V".to_owned()),
                ),
                ("ordinal".to_owned(), Value::Number(2.0)),
                (
                    "shift".to_owned(),
                    Value::Enum(
                        "Lorg/spongepowered/asm/mixin/injection/At$Shift;".to_owned(),
                        "BY".to_owned(),
                    ),
                ),
                ("by".to_owned(), Value::Number(1.0)),
                (
                    "args".to_owned(),
                    Value::Array(vec![
                        Value::Str("log=true".to_owned()),
                        Value::Str("slice=outer".to_owned()),
                    ]),
                ),
            ]),
        };
        assert_eq!(
            at_value(&Value::Ann(at)),
            Some(
                "INVOKE|target:Lnet/minecraft/Target;method()V|args:log=true,slice=outer|ordinal:2|shift:BY|by:1".to_owned()
            )
        );
    }

    #[test]
    fn slice_boundaries_render_nested_at_in_both_directions() {
        let block_target = "Lnet/minecraft/world/level/block/Block;getBlockState()Lnet/minecraft/world/level/block/state/BlockState;";
        let slice = ann(
            SLICE,
            vec![
                (
                    "from",
                    ann(
                        AT,
                        vec![("value", s("INVOKE")), ("target", s(block_target))],
                    ),
                ),
                ("to", ann(AT, vec![("value", s("RETURN"))])),
            ],
        );
        assert_eq!(
            metadata_text(&slice).as_deref(),
            Some(format!("from:INVOKE|target:{block_target}|to:RETURN").as_str())
        );
        // And the other way round: a plain anchor `from`, a nested `to`.
        let reversed = ann(
            SLICE,
            vec![
                ("from", s("HEAD")),
                ("to", ann(AT, vec![("value", s("TAIL"))])),
            ],
        );
        assert_eq!(
            metadata_text(&reversed).as_deref(),
            Some("from:HEAD|to:TAIL")
        );
    }

    #[test]
    fn slice_without_boundaries_keeps_the_presence_marker() {
        assert_eq!(
            metadata_text(&ann(SLICE, vec![])).as_deref(),
            Some("present")
        );
    }

    #[test]
    fn at_value_keeps_every_selector_after_the_anchor() {
        let at = ann(
            AT,
            vec![
                ("value", s("INVOKE")),
                ("target", s("Lnet/minecraft/Target;method()V")),
                ("desc", s("(I)V")),
                ("ordinal", Value::Number(1.0)),
                (
                    "shift",
                    Value::Enum(
                        "Lorg/spongepowered/asm/mixin/injection/At$Shift;".to_owned(),
                        "BEFORE".to_owned(),
                    ),
                ),
                ("by", Value::Number(2.0)),
                ("id", s("anchor")),
                ("remap", Value::Bool(true)),
            ],
        );
        assert_eq!(
            at_value(&at).as_deref(),
            Some(
                "INVOKE|target:Lnet/minecraft/Target;method()V|ordinal:1|shift:BEFORE|by:2|desc:(I)V|id:anchor|remap:true"
            )
        );
    }

    #[test]
    fn at_value_drops_remap_false_and_marks_local_present() {
        let at = ann(
            AT,
            vec![
                ("value", s("NEW")),
                (
                    "local",
                    ann(
                        "Lcom/llamalad7/mixinextras/sugar/Local;",
                        vec![("ordinal", Value::Number(1.0))],
                    ),
                ),
                ("remap", Value::Bool(false)),
            ],
        );
        assert_eq!(at_value(&at).as_deref(), Some("NEW|local:present"));
    }

    #[test]
    fn at_arrays_keep_their_declaration_order() {
        let inject = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Inject;".to_owned(),
            values: BTreeMap::from([
                ("method".to_owned(), s("tick()V")),
                (
                    "at".to_owned(),
                    Value::Array(vec![
                        ann(
                            AT,
                            vec![("value", s("INVOKE")), ("target", s("La/First;run()V"))],
                        ),
                        ann(
                            AT,
                            vec![("value", s("FIELD")), ("target", s("La/Second;f:I"))],
                        ),
                    ]),
                ),
            ]),
        };
        let methods = BTreeMap::from([("run".to_owned(), vec![inject])]);
        let parsed = injections(&methods, &BTreeMap::new());
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].at,
            "INVOKE|target:La/First;run()V|FIELD|target:La/Second;f:I"
        );
    }

    #[test]
    fn long_at_selectors_are_truncated_deterministically() {
        let long = "x".repeat(4096);
        let at = ann(AT, vec![("value", s("INVOKE")), ("target", s(&long))]);
        let text = at_value(&at).expect("an @At always has text");
        assert_eq!(text.chars().count(), AT_TEXT_MAX + 1);
        assert!(text.starts_with("INVOKE|target:xxx"), "{text}");
        assert!(text.ends_with('…'));
        assert_eq!(at_value(&at).as_deref(), Some(text.as_str()));
    }

    #[test]
    fn reads_nested_at_and_slice_from_class_bytes() {
        let bytes = super::tests_support::annotated_mixin(
            "m/Mix",
            "net/minecraft/Target",
            "destroyBlock()V",
        );
        let c = parse(&bytes).expect("the fixture is a valid class file");
        assert_eq!(c.injections.len(), 1);
        assert_eq!(c.injections[0].kind, "inject");
        assert_eq!(
            c.injections[0].at,
            "HEAD|target:Lnet/minecraft/Target;destroyBlock()V"
        );
        assert_eq!(
            c.injections[0]
                .metadata_values
                .get("slice")
                .map(String::as_str),
            Some("from:INVOKE|target:Lnet/minecraft/Target;destroyBlock()V|to:RETURN")
        );
    }

    #[test]
    fn injection_metadata_reports_slice_presence() {
        let inject = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Inject;".to_owned(),
            values: BTreeMap::from([
                ("method".to_owned(), Value::Str("tick()V".to_owned())),
                (
                    "slice".to_owned(),
                    Value::Ann(Annotation {
                        ty: "Lorg/spongepowered/asm/mixin/injection/Slice;".to_owned(),
                        values: BTreeMap::new(),
                    }),
                ),
                ("require".to_owned(), Value::Number(2.0)),
                ("expect".to_owned(), Value::Number(4.0)),
                ("cancellable".to_owned(), Value::Bool(true)),
                ("order".to_owned(), Value::Number(900.0)),
                ("priority".to_owned(), Value::Number(1200.0)),
            ]),
        };
        let methods = BTreeMap::from([("run".to_owned(), vec![inject])]);
        let parsed = injections(&methods, &BTreeMap::new());
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].metadata,
            [
                "require",
                "expect",
                "cancellable",
                "order",
                "priority",
                "slice"
            ]
        );
        assert_eq!(
            parsed[0].metadata_values,
            BTreeMap::from([
                ("cancellable".to_owned(), "true".to_owned()),
                ("expect".to_owned(), "4".to_owned()),
                ("order".to_owned(), "900".to_owned()),
                ("priority".to_owned(), "1200".to_owned()),
                ("require".to_owned(), "2".to_owned()),
                ("slice".to_owned(), "present".to_owned()),
            ])
        );
    }

    #[test]
    fn injection_metadata_preserves_slice_boundaries() {
        let slice = Value::Ann(Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Slice;".to_owned(),
            values: BTreeMap::from([
                ("from".to_owned(), Value::Str("head".to_owned())),
                ("to".to_owned(), Value::Str("tail".to_owned())),
            ]),
        });
        let inject = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Inject;".to_owned(),
            values: BTreeMap::from([
                ("method".to_owned(), Value::Str("tick()V".to_owned())),
                ("slice".to_owned(), slice),
            ]),
        };
        let methods = BTreeMap::from([("run".to_owned(), vec![inject])]);
        let parsed = injections(&methods, &BTreeMap::new());
        assert_eq!(
            parsed[0].metadata_values.get("slice").map(String::as_str),
            Some("from:head|to:tail")
        );
    }

    #[test]
    fn injection_metadata_preserves_group_constraints() {
        let group = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Group;".to_owned(),
            values: BTreeMap::from([
                ("name".to_owned(), Value::Str("interaction".to_owned())),
                ("min".to_owned(), Value::Number(1.0)),
                ("max".to_owned(), Value::Number(2.0)),
                ("log".to_owned(), Value::Bool(true)),
            ]),
        };
        let inject = Annotation {
            ty: "Lorg/spongepowered/asm/mixin/injection/Inject;".to_owned(),
            values: BTreeMap::from([("method".to_owned(), Value::Str("tick()V".to_owned()))]),
        };
        let methods = BTreeMap::from([("run".to_owned(), vec![group, inject])]);
        let parsed = injections(&methods, &BTreeMap::new());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].metadata, ["group"]);
        assert_eq!(
            parsed[0].metadata_values.get("group").map(String::as_str),
            Some("name:interaction|min:1|max:2|log:true")
        );
    }

    #[test]
    fn follows_own_classes_to_the_game() {
        let block = "net/minecraft/world/level/block/Block";
        let files = [
            // Looks only: a template covers it.
            class_file("m/Plain", block, &["getStateForPlacement"], &[], false),
            class_file("m/Round", block, &["getShape"], &[], false),
            // Behaviour two levels down.
            class_file("m/Base", block, &["randomTick"], &[], false),
            class_file("m/Lamp", "m/Base", &[], &[], false),
            class_file(
                "m/Tile",
                "net/minecraft/world/level/block/entity/BlockEntity",
                &[],
                &[],
                false,
            ),
            class_file("m/Mix", "java/lang/Object", &[], &[], true),
            class_file(
                "m/Old",
                "net/minecraft/class_2248",
                &["method_9534"],
                &[],
                false,
            ),
            class_file(
                "m/Events",
                "java/lang/Object",
                &[],
                &[
                    "net/neoforged/neoforge/event/entity/player/PlayerInteractEvent$RightClickBlock",
                    "net/neoforged/neoforge/event/BuildCreativeModeTabContentsEvent",
                    "net/neoforged/neoforge/registries/DeferredRegister",
                ],
                false,
            ),
            class_file(
                "m/Gem",
                "net/minecraft/world/item/Item",
                &["appendHoverText"],
                &[],
                false,
            ),
        ];
        let code = census(files.iter().map(Vec::as_slice));
        assert_eq!(code.classes, 9);
        let n = |f| code.features.get(&f).copied().unwrap_or(0);
        assert_eq!(n(Feature::BlockBehaviour), 2, "{code:?}");
        assert_eq!(n(Feature::BlockShape), 1);
        assert_eq!(n(Feature::BlockEntity), 1);
        assert_eq!(n(Feature::Mixin), 1);
        assert_eq!(n(Feature::Unread), 1);
        assert_eq!(n(Feature::ItemBehaviour), 0);
        assert_eq!(
            code.events.iter().collect::<Vec<_>>(),
            ["PlayerInteractEvent"]
        );
        assert!(code.behaviour());
    }

    #[test]
    fn classifying_level_extensions_marks_world_access() {
        let files = [class_file(
            "m/WorldHooks",
            "net/minecraft/world/level/Level",
            &["getBlockState", "setBlock"],
            &[],
            false,
        )];
        let code = census(files.iter().map(Vec::as_slice));
        assert_eq!(code.features.get(&Feature::WorldAccess), Some(&1));
        assert!(code.behaviour());
        assert_eq!(
            Feature::WorldAccess.describe(2),
            "2 world or level implementations"
        );
    }

    #[test]
    fn registration_only_is_not_behaviour() {
        let files = [
            class_file(
                "m/Mod",
                "java/lang/Object",
                &["register"],
                &[
                    "net/neoforged/neoforge/registries/DeferredRegister",
                    "net/minecraft/world/level/block/Block",
                ],
                false,
            ),
            class_file(
                "m/Pillar",
                "net/minecraft/world/level/block/RotatedPillarBlock",
                &[],
                &[],
                false,
            ),
        ];
        let code = census(files.iter().map(Vec::as_slice));
        assert!(!code.behaviour(), "{code:?}");
    }

    /// A `Class` with the access flags and declarations the widener tests
    /// need; the class file reader is exercised by the tests above.
    fn widened(name: &str, access: u16, declared: Vec<Declared>) -> Class {
        Class {
            name: name.to_owned(),
            access,
            declared,
            ..Class::default()
        }
    }

    fn member(kind: MemberKind, name: &str, descriptor: &str, access: u16) -> Declared {
        Declared {
            name: name.to_owned(),
            descriptor: descriptor.to_owned(),
            access,
            kind,
        }
    }

    fn apply(classes: &mut [Class], text: &str) -> Result<usize, String> {
        let widener = parse_access_widener(text)?;
        apply_access_widener(classes, &widener)
    }

    #[test]
    fn parses_access_widener_lines_comments_and_blanks() {
        let text = "\
# a mod's widener
accessWidener v1 named

accessible class net/minecraft/world/level/block/Block  # the whole class
accessible method net/minecraft/world/level/block/Block getStateForPlacement (Lnet/minecraft/world/item/ItemStack;)Lnet/minecraft/world/level/block/state/BlockState;
extendable field a/B f I
accessible field a/B g
";
        let widener = parse_access_widener(text).expect("the fixture is a v1 widener");
        assert_eq!(widener.namespace, "named");
        assert_eq!(widener.entries.len(), 4);
        assert_eq!(
            widener.entries[0],
            AccessWidenerEntry {
                kind: AccessWidenerKind::Class,
                access: AccessWidenerAccess::Accessible,
                owner: "net/minecraft/world/level/block/Block".to_owned(),
                name: String::new(),
                descriptor: None,
            }
        );
        assert_eq!(widener.entries[1].kind, AccessWidenerKind::Method);
        assert_eq!(
            widener.entries[1].descriptor.as_deref(),
            Some(
                "(Lnet/minecraft/world/item/ItemStack;)Lnet/minecraft/world/level/block/state/BlockState;"
            )
        );
        assert_eq!(widener.entries[2].access, AccessWidenerAccess::Extendable);
        assert_eq!(widener.entries[2].descriptor.as_deref(), Some("I"));
        assert_eq!(widener.entries[3].kind, AccessWidenerKind::Field);
        assert_eq!(widener.entries[3].descriptor, None);
    }

    #[test]
    fn accepts_v2_headers_and_rejects_unknown_versions() {
        let widener =
            parse_access_widener("accessWidener v2 intermediary\naccessible class a/B\n").unwrap();
        assert_eq!(widener.namespace, "intermediary");
        assert_eq!(widener.entries.len(), 1);
        let error = |text: &str| parse_access_widener(text).unwrap_err();
        assert!(error("accessWidener v3 named\n").contains("v3"));
        assert!(error("AccessWidener v1 named\n").contains("line 1"));
        assert!(error("accessWidener v1\n").contains("namespace"));
        assert!(error("accessWidener v1 named extra\n").contains("after the header"));
        assert!(error("accessWidener v1 a/b\n").contains("not an access widener namespace"));
        assert!(error("accessible class a/B\n").contains("accessWidener v1"));
        assert!(error("").contains("missing the `accessWidener v1"));
        // A header with no entry is an empty widener, not an error.
        let empty = parse_access_widener("accessWidener v1 named\n").unwrap();
        assert!(empty.entries.is_empty());
    }

    #[test]
    fn rejects_invalid_access_widener_entries() {
        let error = |text: &str| parse_access_widener(text).unwrap_err();
        let header = "accessWidener v1 named\n";
        assert!(error(&format!("{header}mutable class a/B\n")).contains("line 2"));
        assert!(
            error(&format!("{header}transitive-accessible class a/B\n")).contains("transitive")
        );
        assert!(error(&format!("{header}accessible mixin.class a/B\n")).contains("mixin"));
        assert!(error(&format!("{header}accessible class a/B extra\n")).contains("no member"));
        assert!(error(&format!("{header}accessible class ../B\n")).contains("internal class name"));
        assert!(error(&format!("{header}accessible method a/B\n")).contains("no member"));
        assert!(error(&format!("{header}accessible field a/B f I extra\n")).contains("text after"));
        assert!(error(&format!("{header}accessible field a/B f !\n")).contains("descriptor"));
        assert!(error(&format!("{header}accessible\n")).contains("no kind"));
        // An entry with no descriptor widens every overload: valid.
        let widener =
            parse_access_widener(&format!("{header}accessible method a/B run\n")).unwrap();
        assert_eq!(widener.entries[0].descriptor, None);
    }

    #[test]
    fn rejects_oversized_access_wideners() {
        let big = format!(
            "accessWidener v1 named\n#{}",
            "x".repeat(ACCESS_WIDENER_TEXT_MAX)
        );
        assert!(parse_access_widener(&big).unwrap_err().contains("limit"));
        let mut many = String::from("accessWidener v1 named\n");
        for _ in 0..=ACCESS_WIDENER_ENTRIES_MAX {
            many.push_str("accessible class a\n");
        }
        let message = parse_access_widener(&many).unwrap_err();
        assert!(message.contains("more than 16384 entries"), "{message}");
    }

    #[test]
    fn widens_class_and_member_access_flags() {
        let mut classes = [widened(
            "m/Tile",
            0x0002 | 0x0010, // private final
            vec![
                member(MemberKind::Field, "f", "I", 0x0002 | 0x0010),
                member(MemberKind::Method, "run", "()V", 0x0004),
            ],
        )];
        let changed = apply(
            &mut classes,
            "\
accessWidener v1 named
extendable class m/Tile
accessible field m/Tile f I
extendable method m/Tile run ()V
",
        )
        .expect("every target is there");
        assert_eq!(changed, 3);
        assert_eq!(classes[0].access, ACC_PUBLIC); // extendable drops final
        // `accessible` keeps `final`: only `extendable` clears it.
        assert_eq!(classes[0].declared[0].access, ACC_PUBLIC | ACC_FINAL);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
    }

    #[test]
    fn widens_members_with_and_without_descriptors() {
        let mut classes = [widened(
            "m/Tile",
            ACC_PUBLIC,
            vec![
                member(MemberKind::Method, "get", "()V", 0x0002 | 0x0010),
                member(MemberKind::Method, "get", "(I)V", 0x0002 | 0x0010),
                member(MemberKind::Field, "get", "I", 0x0002),
            ],
        )];
        // A descriptor picks one overload and leaves the other alone.
        let changed = apply(
            &mut classes,
            "accessWidener v1 named\nextendable method m/Tile get (I)V\n",
        )
        .unwrap();
        assert_eq!(changed, 1);
        assert_eq!(classes[0].declared[0].access, 0x0002 | 0x0010);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
        // The same name as a field is not a method target.
        assert_eq!(classes[0].declared[2].access, 0x0002);
        // No descriptor widens every overload of the name.
        let changed = apply(
            &mut classes,
            "accessWidener v1 named\naccessible method m/Tile get\n",
        )
        .unwrap();
        assert_eq!(changed, 1, "only the still-private overload changed");
        // `accessible` keeps `final`; the descriptor-picked overload was
        // widened with `extendable`, so its `final` is already gone.
        assert_eq!(classes[0].declared[0].access, ACC_PUBLIC | ACC_FINAL);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
        assert_eq!(classes[0].declared[2].access, 0x0002);
    }

    #[test]
    fn widener_keeps_constructors() {
        let mut classes = [widened(
            "m/Tile",
            ACC_PUBLIC,
            vec![
                member(MemberKind::Method, "<init>", "(I)V", 0x0002),
                member(MemberKind::Method, "<clinit>", "()V", 0x0008),
                member(MemberKind::Method, "run", "()V", 0x0002),
            ],
        )];
        let before: Vec<String> = classes[0]
            .declared
            .iter()
            .map(|declared| format!("{} {}", declared.name, declared.descriptor))
            .collect();
        let changed = apply(
            &mut classes,
            "\
accessWidener v1 named
accessible method m/Tile <init> (I)V
accessible method m/Tile <clinit>
",
        )
        .unwrap();
        assert_eq!(changed, 2);
        let after: Vec<String> = classes[0]
            .declared
            .iter()
            .map(|declared| format!("{} {}", declared.name, declared.descriptor))
            .collect();
        assert_eq!(before, after, "constructors stay declared, in order");
        assert_eq!(classes[0].declared[0].access, ACC_PUBLIC);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC | 0x0008);
        assert_eq!(classes[0].declared[2].access, 0x0002);
    }

    #[test]
    fn widener_reports_missing_targets() {
        let mut classes = [widened(
            "m/Tile",
            ACC_PUBLIC,
            vec![member(MemberKind::Field, "f", "I", 0x0002)],
        )];
        let message = apply(
            &mut classes,
            "accessWidener v1 named\naccessible class m/Gone\n",
        )
        .unwrap_err();
        assert!(message.contains("m/Gone"), "{message}");
        let message = apply(
            &mut classes,
            "accessWidener v1 named\naccessible field m/Tile g I\n",
        )
        .unwrap_err();
        assert!(message.contains("m/Tile#g I"), "{message}");
        // Nothing was widened by the failed entries.
        assert_eq!(classes[0].declared[0].access, 0x0002);
    }

    #[test]
    fn widener_is_idempotent_and_keeps_file_order() {
        let text = "\
accessWidener v1 named
accessible class b/B
extendable class a/A
accessible class c/C
";
        let widener = parse_access_widener(text).unwrap();
        assert_eq!(
            widener
                .entries
                .iter()
                .map(|entry| entry.owner.as_str())
                .collect::<Vec<_>>(),
            ["b/B", "a/A", "c/C"]
        );
        let mut classes = [
            widened("a/A", 0x0010, vec![]),
            widened("b/B", 0x0002, vec![]),
            widened("c/C", 0x0004, vec![]),
        ];
        assert_eq!(apply_access_widener(&mut classes, &widener), Ok(3));
        assert_eq!(apply_access_widener(&mut classes, &widener), Ok(0));
        assert_eq!(
            classes
                .iter()
                .map(|class| class.name.as_str())
                .collect::<Vec<_>>(),
            ["a/A", "b/B", "c/C"],
            "classes keep the order they were read in"
        );
    }

    fn apply_transformer(classes: &mut [Class], text: &str) -> Result<usize, String> {
        apply_access_transformer(classes, &parse_access_transformer(text)?)
    }

    #[test]
    fn parses_access_transformer_lines_comments_and_blanks() {
        let text = "\
# a mod's transformer
public net.minecraft.world.level.Level field_1
protected net.minecraft.world.level.Level method_1 ()V  # an overload
private-f net.minecraft.world.level.Level method_2(I)V
public net.minecraft.world.Block f I
private net.minecraft.Shape
public-f net.minecraft.Alpha
protected-f net.minecraft.Beta
";
        let transformer = parse_access_transformer(text).expect("the fixture is a transformer");
        assert_eq!(
            transformer.entries[0],
            AccessTransformerEntry {
                access: AccessTransformerAccess::Public,
                owner: "net/minecraft/world/level/Level".to_owned(),
                name: "field_1".to_owned(),
                descriptor: None,
            }
        );
        assert_eq!(transformer.entries[1].descriptor.as_deref(), Some("()V"));
        assert_eq!(transformer.entries[2].descriptor.as_deref(), Some("(I)V"));
        // A bare descriptor is a field's; a class entry names no member.
        assert_eq!(transformer.entries[3].descriptor.as_deref(), Some("I"));
        assert_eq!(transformer.entries[4].owner, "net/minecraft/Shape");
        assert!(transformer.entries[4].name.is_empty());
        // All six access forms, in file order.
        assert_eq!(
            transformer
                .entries
                .iter()
                .map(|entry| entry.access)
                .collect::<Vec<_>>(),
            [
                AccessTransformerAccess::Public,
                AccessTransformerAccess::Protected,
                AccessTransformerAccess::PrivateNoFinal,
                AccessTransformerAccess::Public,
                AccessTransformerAccess::Private,
                AccessTransformerAccess::PublicNoFinal,
                AccessTransformerAccess::ProtectedNoFinal,
            ]
        );
    }

    #[test]
    fn applies_access_transformer_classes_and_members() {
        let mut classes = [widened(
            "net/minecraft/world/level/Level",
            ACC_PUBLIC | ACC_FINAL,
            vec![
                member(MemberKind::Field, "field_1", "I", ACC_PRIVATE | ACC_FINAL),
                member(MemberKind::Method, "method_1", "()V", ACC_PROTECTED),
            ],
        )];
        let text = "\
public-f net.minecraft.world.level.Level
private-f net.minecraft.world.level.Level field_1
public-f net.minecraft.world.level.Level method_1 ()V
";
        assert_eq!(apply_transformer(&mut classes, text), Ok(3));
        assert_eq!(classes[0].access, ACC_PUBLIC);
        // `-f` clears the field's `final`; it is private now.
        assert_eq!(classes[0].declared[0].access, ACC_PRIVATE);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
        assert_eq!(apply_transformer(&mut classes, text), Ok(0), "idempotent");
    }

    #[test]
    fn access_transformer_descriptors_pick_one_overload_by_shape() {
        let mut classes = [widened(
            "a/B",
            ACC_PUBLIC,
            vec![
                member(MemberKind::Method, "get", "()V", ACC_PRIVATE),
                member(MemberKind::Method, "get", "(I)V", ACC_PRIVATE),
                member(MemberKind::Field, "get", "I", ACC_PRIVATE),
                member(MemberKind::Method, "<init>", "(I)V", ACC_PRIVATE),
                member(MemberKind::Field, "other", "J", ACC_PRIVATE),
            ],
        )];
        // A method descriptor picks one overload; a field descriptor one field.
        assert_eq!(
            apply_transformer(&mut classes, "public a.B get (I)V\n"),
            Ok(1)
        );
        assert_eq!(classes[0].declared[0].access, ACC_PRIVATE);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
        assert_eq!(
            apply_transformer(&mut classes, "protected a.B get I\n"),
            Ok(1)
        );
        assert_eq!(classes[0].declared[2].access, ACC_PROTECTED);
        // No descriptor matches every member of the name, both kinds.
        assert_eq!(apply_transformer(&mut classes, "public a.B get\n"), Ok(2));
        assert_eq!(classes[0].declared[0].access, ACC_PUBLIC);
        assert_eq!(classes[0].declared[1].access, ACC_PUBLIC);
        assert_eq!(classes[0].declared[2].access, ACC_PUBLIC);
        assert_eq!(classes[0].declared[4].access, ACC_PRIVATE);
        // A constructor stays declared and is matched by its descriptor.
        assert_eq!(
            apply_transformer(&mut classes, "public a.B <init> (I)V\n"),
            Ok(1)
        );
        assert_eq!(classes[0].declared.len(), 5);
        assert_eq!(classes[0].declared[3].access, ACC_PUBLIC);
    }

    #[test]
    fn reports_missing_malformed_and_oversized_access_transformer_input() {
        let mut classes = [widened(
            "a/B",
            ACC_PUBLIC,
            vec![member(MemberKind::Field, "f", "I", ACC_PRIVATE)],
        )];
        for (text, needle) in [
            ("public a.Gone\n", "a/Gone"),
            ("public a.B g I\n", "a/B#g I"),
            // A descriptor of the wrong shape is its own miss, not a guess.
            ("public a.B f ()V\n", "a/B#f ()V"),
        ] {
            let message = apply_transformer(&mut classes, text).unwrap_err();
            assert!(message.contains(needle), "{message}");
        }
        assert_eq!(
            classes[0].declared[0].access, ACC_PRIVATE,
            "nothing applied"
        );

        let error = |text: &str| parse_access_transformer(text).unwrap_err();
        assert!(error("mutable a.B\n").contains("line 1"));
        assert!(error("public\n").contains("no class"));
        assert!(error("public ../B\n").contains("class name"));
        assert!(error("public a..B\n").contains("class name"));
        assert!(error("public a.B bad-name\n").contains("member name"));
        assert!(error("public a.B run (\n").contains("descriptor"));
        assert!(error("public a.B run ()V extra\n").contains("after the descriptor"));
        assert!(error("public a.B f !\n").contains("not a descriptor"));
        assert!(error("public a.B f [V\n").contains("not a descriptor"));
        assert!(
            parse_access_transformer("# nothing\n\n")
                .unwrap()
                .entries
                .is_empty()
        );

        let big = format!("public a.B\n#{}", "x".repeat(ACCESS_TRANSFORMER_TEXT_MAX));
        assert!(
            parse_access_transformer(&big)
                .unwrap_err()
                .contains("limit")
        );
        let mut many = String::new();
        for _ in 0..=ACCESS_TRANSFORMER_ENTRIES_MAX {
            many.push_str("public a.B\n");
        }
        assert!(
            parse_access_transformer(&many)
                .unwrap_err()
                .contains("more than 65536 entries")
        );
    }

    #[test]
    fn discovers_access_transformer_files() {
        let names: Vec<String> = [
            "META-INF/accesstransformer.cfg",
            "META-INF/accesstransformer/extra.cfg",
            "META-INF/accesstransformer/nested/deep.cfg",
            "META-INF/accesstransformer/readme.txt",
            "META-INF/MANIFEST.MF",
        ]
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
        assert_eq!(
            access_transformer_names(&names),
            [
                "META-INF/accesstransformer.cfg",
                "META-INF/accesstransformer/extra.cfg"
            ]
        );
        // The canonical path is a candidate even when the listing omits it,
        // and is not repeated when present.
        assert_eq!(
            access_transformer_names(&[]),
            ["META-INF/accesstransformer.cfg"]
        );
        assert_eq!(
            access_transformer_names(&[
                "META-INF/accesstransformer.cfg".to_owned(),
                "META-INF/accesstransformer.cfg".to_owned()
            ]),
            ["META-INF/accesstransformer.cfg"]
        );
    }

    /// One instruction that names a pool index: a call, a field access or an
    /// `ldc_w`.
    fn indexed(op: u8, index: u16) -> Vec<u8> {
        let mut out = vec![op];
        out.extend(index.to_be_bytes());
        out
    }

    fn target(kind: CallKind, owner: &str, name: &str, descriptor: &str) -> CallTarget {
        CallTarget {
            owner: owner.to_owned(),
            name: name.to_owned(),
            descriptor: descriptor.to_owned(),
            kind,
        }
    }

    /// Parses the class `a/Caller` with one method `run` whose body is `code`.
    fn scanned(pool: &mut PoolBuilder, code: &[u8]) -> Class {
        let bytes = pool.build("a/Caller", "java/lang/Object", &[("run", code)]);
        parse(&bytes).expect("the fixture is a class file")
    }

    /// The error scanning `code` as `a/Caller#run` produces, if any.
    fn failure(code: &[u8]) -> Option<String> {
        scanned(&mut PoolBuilder::default(), code).body_error
    }

    #[test]
    fn body_scan_keeps_call_kinds_and_overloads_apart() {
        let mut pool = PoolBuilder::default();
        let int_add = pool.method("a/Math", "add", "(I)I");
        let pair_add = pool.method("a/Math", "add", "(II)I");
        let clamp = pool.method("a/Math", "clamp", "(I)I");
        let init = pool.method("a/Math", "<init>", "()V");
        let size = pool.interface_method("java/util/List", "size", "()I");
        let mut code = indexed(0xb6, int_add);
        code.extend(indexed(0xb6, pair_add));
        code.extend(indexed(0xb8, clamp));
        code.extend(indexed(0xb7, init));
        // invokeinterface: opcode, index, count, zero.
        code.extend([0xb9]);
        code.extend(size.to_be_bytes());
        code.extend([2, 0]);
        code.push(0xb1);
        let class = scanned(&mut pool, &code);
        assert_eq!(class.body_error, None);
        assert_eq!(
            class.body_refs,
            [
                target(CallKind::Virtual, "a/Math", "add", "(I)I"),
                target(CallKind::Virtual, "a/Math", "add", "(II)I"),
                target(CallKind::Static, "a/Math", "clamp", "(I)I"),
                target(CallKind::Special, "a/Math", "<init>", "()V"),
                target(CallKind::Interface, "java/util/List", "size", "()I"),
            ]
        );
        // Overloads stay distinct, and the map is sorted by key, not by the
        // order the call sites appear in.
        let calls = class.calls();
        assert_eq!(
            calls.keys().cloned().collect::<Vec<_>>(),
            [
                "a/Math#<init>(()V)",
                "a/Math#add((I)I)",
                "a/Math#add((II)I)",
                "a/Math#clamp((I)I)",
                "java/util/List#size(()I)",
            ]
        );
        assert!(calls.values().all(|sites| *sites == 1), "{calls:?}");
        assert!(class.field_refs().is_empty());
    }

    #[test]
    fn body_scan_reads_fields_literals_and_handles() {
        let mut pool = PoolBuilder::default();
        let count = pool.field("a/State", "count", "I");
        let long = pool.field("a/State", "count", "J");
        let held = pool.field("a/State", "name", "Ljava/lang/String;");
        let mut code = indexed(0xb2, count); // getstatic, twice
        code.extend(indexed(0xb2, count));
        code.extend(indexed(0xb3, long)); // putstatic
        code.extend(indexed(0xb4, held)); // getfield
        code.extend(indexed(0xb5, held)); // putfield
        code.push(0xb1);
        let fields = scanned(&mut pool, &code);
        assert_eq!(fields.body_error, None);
        assert!(fields.body_refs.iter().all(CallTarget::is_field));
        assert_eq!(
            fields.field_refs(),
            BTreeMap::from([
                ("a/State#count(I)".to_owned(), 2),
                ("a/State#count(J)".to_owned(), 1),
                ("a/State#name(Ljava/lang/String;)".to_owned(), 2),
            ])
        );
        assert!(fields.calls().is_empty(), "a field access is not a call");

        // `ldc`/`ldc_w` of a class names it, of a method handle the member
        // behind it; other constants are not references.
        let item = pool.class("net/minecraft/world/item/Item");
        let array = pool.class("[I");
        let number = pool.integer(7);
        let tick = pool.method("a/Lamp", "tick", "()V");
        let handle = pool.handle(6, tick);
        let mut code = vec![0x12, item as u8];
        code.extend(indexed(0x13, item));
        code.extend([0x12, array as u8]);
        code.extend([0x12, number as u8]);
        code.extend(indexed(0x13, handle));
        code.push(0xb1);
        let class = scanned(&mut pool, &code);
        assert_eq!(class.body_error, None);
        assert_eq!(
            class.body_refs,
            [
                target(CallKind::Class, "net/minecraft/world/item/Item", "", ""),
                target(CallKind::Class, "net/minecraft/world/item/Item", "", ""),
                target(CallKind::MethodHandle, "a/Lamp", "tick", "()V"),
            ]
        );
        assert_eq!(class.body_refs[0].key(), "net/minecraft/world/item/Item");
        assert_eq!(class.body_refs[2].key(), "a/Lamp#tick(()V)");
        assert_eq!(
            class.calls(),
            BTreeMap::from([("a/Lamp#tick(()V)".to_owned(), 1)])
        );
    }

    /// `at` nops, one `tableswitch`/`lookupswitch` there, then a `getstatic`.
    fn switch_then_field(op: u8, at: usize, field: u16) -> Vec<u8> {
        let mut code = vec![0x00; at];
        code.push(op);
        code.extend(vec![0xee; (4 - (at + 1) % 4) % 4]); // padding
        code.extend([0, 0, 0, 0]); // default
        if op == 0xaa {
            code.extend(3i32.to_be_bytes()); // low
            code.extend(5i32.to_be_bytes()); // high: three targets
            code.extend(vec![0; 3 * 4]);
        } else {
            code.extend(2i32.to_be_bytes()); // two match/offset pairs
            code.extend(vec![0; 2 * 8]);
        }
        code.extend(indexed(0xb2, field));
        code.push(0xb1);
        code
    }

    #[test]
    fn body_scan_lands_after_variable_length_operands() {
        // The count byte of an `invokeinterface` is not an opcode, and the
        // fixed-length operands behind it are two to six bytes long.
        let mut pool = PoolBuilder::default();
        let size = pool.interface_method("java/util/List", "size", "()I");
        let array = pool.class("[I");
        let count = pool.field("a/State", "count", "I");
        let mut code = vec![0xb9];
        code.extend(size.to_be_bytes());
        code.extend([0x12, 0x00]); // the count reads as `ldc` if misread
        code.extend([0x84, 0x01, 0x02]); // iinc: three bytes
        code.extend([0xc5]); // multianewarray: four
        code.extend(array.to_be_bytes());
        code.push(2);
        code.extend([0xc4, 0x15, 0x01, 0x00]); // wide iload: four
        code.extend([0xc4, 0x84, 0x01, 0x00, 0x00, 0x07]); // wide iinc: six
        code.extend(indexed(0xb2, count));
        code.push(0xb1);
        let class = scanned(&mut pool, &code);
        assert_eq!(class.body_error, None);
        assert_eq!(
            class.body_refs,
            [
                target(CallKind::Interface, "java/util/List", "size", "()I"),
                target(CallKind::GetStatic, "a/State", "count", "I"),
            ]
        );

        // A switch pads to the next 4-byte boundary from the start of the
        // code array: every offset and both tables are tried.
        for op in [0xaa, 0xab] {
            for at in 0..4 {
                let mut pool = PoolBuilder::default();
                let count = pool.field("a/State", "count", "I");
                let class = scanned(&mut pool, &switch_then_field(op, at, count));
                assert_eq!(class.body_error, None, "switch {op:#04x} at {at}");
                assert_eq!(
                    class.body_refs,
                    [target(CallKind::GetStatic, "a/State", "count", "I")],
                    "switch {op:#04x} at {at}"
                );
            }
        }
    }

    #[test]
    fn body_scan_reads_invocations_dynamics_bootstrap_and_arguments() {
        let mut pool = PoolBuilder::default();
        let metafactory = pool.method(
            "java/lang/invoke/LambdaMetafactory",
            "metafactory",
            "()Ljava/lang/invoke/CallSite;",
        );
        let bootstrap = pool.handle(6, metafactory);
        let tick = pool.method("a/Lamp", "tick", "()V");
        let tick = pool.handle(5, tick);
        let indy = pool.dynamic("run", "()Ljava/lang/Runnable;", bootstrap, &[tick]);
        // invokedynamic is five bytes: opcode, index, then two zero bytes.
        let mut code = indexed(0xba, indy);
        code.extend([0, 0]);
        code.push(0xb1);
        let class = scanned(&mut pool, &code);
        assert_eq!(class.body_error, None);
        assert_eq!(
            class.body_refs,
            [
                target(
                    CallKind::MethodHandle,
                    "java/lang/invoke/LambdaMetafactory",
                    "metafactory",
                    "()Ljava/lang/invoke/CallSite;"
                ),
                target(CallKind::MethodHandle, "a/Lamp", "tick", "()V"),
            ]
        );
        assert_eq!(class.calls().len(), 2, "a lambda's body is a call site");
    }

    #[test]
    fn body_scan_keeps_earlier_refs_and_names_the_failing_offset() {
        let mut pool = PoolBuilder::default();
        let count = pool.field("a/State", "count", "I");
        let name = pool.field("a/State", "name", "Ljava/lang/String;");
        let mut broken = indexed(0xb2, count);
        broken.push(0xff); // not an opcode
        let mut later = indexed(0xb2, name);
        later.push(0xb1);
        let bytes = pool.build(
            "a/Caller",
            "java/lang/Object",
            &[("first", &broken), ("later", &later)],
        );
        let class = parse(&bytes).expect("the fixture is a class file");
        assert_eq!(
            class.body_refs,
            [target(CallKind::GetStatic, "a/State", "count", "I")],
            "the references read before the failure are kept"
        );
        assert_eq!(
            class.body_error.as_deref(),
            Some("a/Caller#first at offset 3: unknown opcode 0xff")
        );
        // The rest of the class is still read; only the body scan stopped.
        assert!(class.refs.contains("a/State"));
        assert_eq!(class.methods, ["first", "later"]);

        // An operand, a wide and both switches fail at their own offset.
        assert_eq!(
            failure(&[0x00, 0xb6, 0x00]).as_deref(),
            Some("a/Caller#run at offset 1: operand runs past the code")
        );
        assert_eq!(
            failure(&[0x00, 0xc4, 0x00, 0x00, 0x00]).as_deref(),
            Some("a/Caller#run at offset 1: wide of opcode 0x00")
        );
        let mut table = vec![0xaa, 0, 0, 0, 0, 0, 0, 0]; // padding, default
        table.extend(5i32.to_be_bytes()); // low
        table.extend(4i32.to_be_bytes()); // high, below low
        assert_eq!(
            failure(&table).as_deref(),
            Some("a/Caller#run at offset 0: tableswitch high below low")
        );
        let mut lookup = vec![0xab, 0, 0, 0, 0, 0, 0, 0]; // padding, default
        lookup.extend((-1i32).to_be_bytes()); // pairs
        assert_eq!(
            failure(&lookup).as_deref(),
            Some("a/Caller#run at offset 0: lookupswitch pairs below zero")
        );

        // A Code attribute that cannot even be walked names the class at 0.
        let mut out = Vec::new();
        for (attribute, needle) in [
            (&[0u8, 0][..], "truncated Code attribute"),
            (
                &[0, 0, 0, 1, 0, 0, 0, 10, 0x00][..],
                "code runs past the attribute",
            ),
            (&[0, 0, 0, 1, 0, 1, 0, 1][..], "the limit is 65536"),
        ] {
            let error = scan_method_code(attribute, &Pool::default(), "a/C", "run", &mut out)
                .expect_err("the attribute is malformed");
            assert!(error.starts_with("a/C#run at offset 0: "), "{error}");
            assert!(error.contains(needle), "{error}");
        }
        assert!(out.is_empty());
    }

    #[test]
    fn body_scan_caps_a_class_and_sums_references_per_site() {
        // 21845 `invokestatic`s fill the JVM's 64 KiB limit on one method;
        // five of them pass the per-class cap of 100000 references.
        let mut pool = PoolBuilder::default();
        let count = pool.method("a/State", "count", "()I");
        let mut full = Vec::with_capacity(65_535);
        while full.len() < 65_535 {
            full.extend(indexed(0xb8, count));
        }
        let slice = full.as_slice();
        let bytes = pool.build(
            "a/Big",
            "java/lang/Object",
            &[
                ("a", slice),
                ("b", slice),
                ("c", slice),
                ("d", slice),
                ("e", slice),
            ],
        );
        let class = parse(&bytes).expect("the fixture is a class file");
        assert_eq!(class.body_error, None, "the cap is not an error");
        assert_eq!(
            class.body_refs.len(),
            BODY_REFS_MAX,
            "the scan stops at the per-class cap"
        );
        assert_eq!(
            class.calls(),
            BTreeMap::from([("a/State#count(()I)".to_owned(), BODY_REFS_MAX)])
        );

        // `method_references`/`field_references` sum the same key across
        // classes, still sorted.
        let mut pool = PoolBuilder::default();
        let call = pool.method("a/Math", "add", "(I)I");
        let other = pool.method("a/Math", "clamp", "(I)I");
        let field = pool.field("a/State", "count", "I");
        let mut code = indexed(0xb2, field);
        code.extend(indexed(0xb3, field));
        code.extend(indexed(0xb6, call));
        code.extend(indexed(0xb6, call));
        code.extend(indexed(0xb8, other));
        code.push(0xb1);
        let classes = [
            parse(&pool.build("a/One", "java/lang/Object", &[("run", &code)])).unwrap(),
            parse(&pool.build("a/Two", "java/lang/Object", &[("run", &code)])).unwrap(),
        ];
        assert_eq!(
            method_references(&classes),
            BTreeMap::from([
                ("a/Math#add((I)I)".to_owned(), 4),
                ("a/Math#clamp((I)I)".to_owned(), 2),
            ])
        );
        assert_eq!(
            method_references(&classes)
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["a/Math#add((I)I)", "a/Math#clamp((I)I)"]
        );
        assert_eq!(
            field_references(&classes),
            BTreeMap::from([("a/State#count(I)".to_owned(), 4)])
        );
    }
}
