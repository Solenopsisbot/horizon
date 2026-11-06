//! Typed content declarations the interpreter derives from builder chains and
//! from the jar's own data files, beside the item and block inference in
//! `StdHost::inferred_declaration`.
//!
//! * **Data components.** `DataComponentType.builder()` is a fluent game
//!   object the interpreter answers itself. Each setter it sees
//!   (`persistent`, `networkSynchronized`, `cacheEncoding`) is reported to the
//!   host as a [`ContentBuilderEvent`], so when the built type is registered
//!   into `minecraft:data_component_type` the host attaches a
//!   `Custom { kind: "data_component" }` declaration naming those flags.
//! * **Attachments.** Fabric's `AttachmentRegistry` registers by itself, with no
//!   `Registry.register` call: `create`, `createPersistent`, `createDefaulted`,
//!   `create(id, Consumer<Builder>)` (the consumer runs on a builder) and
//!   `builder()...buildAndRegister(id)` each become one
//!   `fabric:attachment_type` row with a `Custom { kind: "attachment" }`
//!   declaration.
//! * **Data recipes.** A jar's `data/<ns>/recipe/<path>.json` files load like a
//!   datapack's: each becomes a `minecraft:recipe` row with a
//!   [`RegistryDeclaration::Recipe`], appended to the jar's metadata
//!   declarations ([`with_data_recipes`]).
//!
//! Codecs are Java objects outside the bounded model. A codec argument that is
//! a non-null marker is kept as a label (for an unmodelled codec, its declared
//! type); a null one only sets the flag. NeoForge's
//! `DeferredRegister.DataComponents` and `AttachmentType` register through
//! suppliers the deferred register never runs, so they stay opaque rows.
use super::*;

/// Which builder a [`ContentBuilderEvent`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentBuilderKind {
    DataComponent,
    Attachment,
}

/// One builder step the interpreter answered itself, for the host's
/// declaration table. A host that keeps none ignores it.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBuilderEvent {
    /// `builder` set `flag` (a declaration property name); `codec` is the
    /// codec argument's label when it had one.
    Flag {
        kind: ContentBuilderKind,
        builder: String,
        flag: &'static str,
        codec: Option<String>,
    },
    /// A data-component builder built the type `built`.
    Built { builder: String, built: String },
    /// An attachment type was registered under `id`, with the flags of
    /// `builder` (if one was used) and `preset` on top.
    RegisterAttachment {
        builder: Option<String>,
        id: String,
        value: Value,
        preset: Vec<&'static str>,
        codec: Option<String>,
    },
}

/// The flags one builder collected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct BuilderFlags {
    flags: BTreeSet<&'static str>,
    codec: Option<String>,
}

impl BuilderFlags {
    /// The declaration properties: each flag `true`, and the codec label.
    fn properties(&self) -> BTreeMap<String, String> {
        let mut out: BTreeMap<String, String> = self
            .flags
            .iter()
            .map(|flag| ((*flag).to_owned(), "true".to_owned()))
            .collect();
        if let Some(codec) = &self.codec {
            out.insert("codec".into(), codec.clone());
        }
        out
    }
}

/// The most builders and built types one host remembers. A bootstrap builds
/// a few dozen; past the bound a builder is simply not typed.
const MAX_CONTENT_BUILDERS: usize = 4096;
/// A codec label longer than this is not kept.
const MAX_CODEC_LABEL: usize = 256;
/// The data recipes one jar contributes, and the bytes one may have.
pub(crate) const MAX_DATA_RECIPES: usize = 4096;
const MAX_DATA_RECIPE_BYTES: u64 = 64 * 1024;

/// The host's view of the content builders it was told about.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContentBuilders {
    builders: BTreeMap<String, BuilderFlags>,
    components: BTreeMap<String, BuilderFlags>,
}

impl ContentBuilders {
    /// Apply one event; an attachment registration answers the row to record.
    pub(crate) fn note(&mut self, event: ContentBuilderEvent) -> Option<RegistryEntry> {
        match event {
            ContentBuilderEvent::Flag {
                builder,
                flag,
                codec,
                ..
            } => {
                if self.builders.len() >= MAX_CONTENT_BUILDERS
                    && !self.builders.contains_key(&builder)
                {
                    return None;
                }
                let flags = self.builders.entry(builder).or_default();
                flags.flags.insert(flag);
                if codec.is_some() {
                    flags.codec = codec;
                }
                None
            }
            ContentBuilderEvent::Built { builder, built } => {
                if self.components.len() < MAX_CONTENT_BUILDERS {
                    let flags = self.builders.get(&builder).cloned().unwrap_or_default();
                    self.components.insert(built, flags);
                }
                None
            }
            ContentBuilderEvent::RegisterAttachment {
                builder,
                id,
                value,
                preset,
                codec,
            } => {
                let mut flags = builder
                    .and_then(|builder| self.builders.get(&builder).cloned())
                    .unwrap_or_default();
                flags.flags.extend(preset);
                if codec.is_some() {
                    flags.codec = codec;
                }
                Some(RegistryEntry {
                    registry: ATTACHMENT_REGISTRY.into(),
                    id,
                    value,
                    declaration: Some(RegistryDeclaration::Custom {
                        kind: "attachment".into(),
                        properties: flags.properties(),
                    }),
                })
            }
        }
    }

    /// The declaration a registered data component type carries, when the
    /// interpreter built it.
    pub(crate) fn component_declaration(&self, built: &str) -> Option<RegistryDeclaration> {
        self.components
            .get(built)
            .map(|flags| RegistryDeclaration::Custom {
                kind: "data_component".into(),
                properties: flags.properties(),
            })
    }
}

/// The registry name attachment rows are recorded under. Fabric keeps its
/// attachment types in a registry of its own, not a vanilla one.
pub const ATTACHMENT_REGISTRY: &str = "fabric:attachment_type";

const COMPONENT_TYPE: &str = "net/minecraft/core/component/DataComponentType";
const COMPONENT_BUILDER: &str = "net/minecraft/core/component/DataComponentType$Builder";
const ATTACHMENT_REGISTRY_CLASS: &str = "net/fabricmc/fabric/api/attachment/v1/AttachmentRegistry";
const ATTACHMENT_BUILDER: &str = "net/fabricmc/fabric/api/attachment/v1/AttachmentRegistry$Builder";
const ATTACHMENT_TYPE: &str = "net/fabricmc/fabric/api/attachment/v1/AttachmentType";

/// A codec argument's label: a non-null marker made of printable characters.
/// An unmodelled codec answers the opaque `<type>#codec` marker, which names
/// only the declared codec type, so the label is that type
/// (`com/mojang/serialization/Codec`).
fn codec_label(value: Option<&Value>) -> Option<String> {
    let label = value.and_then(expect_ref)?;
    let label = label.strip_suffix("#codec").unwrap_or(label);
    (!label.is_empty()
        && label.len() <= MAX_CODEC_LABEL
        && label.bytes().all(|b| b.is_ascii_graphic()))
    .then(|| label.to_owned())
}

/// The `namespace:path` an `Identifier` argument names.
fn identifier_id(value: &Value) -> Option<String> {
    expect_ref(value)?;
    let (namespace, path) = identifier_parts(value);
    (!namespace.is_empty() && !path.is_empty()).then(|| format!("{namespace}:{path}"))
}

impl<H: Host> Runtime<'_, H> {
    /// Answer the data-component and attachment builder surface, reporting
    /// each step to the host. Every other call answers `None`.
    pub(crate) fn content_builder_call(
        &mut self,
        r: &MethodRef,
        receiver: Option<&Value>,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let ret = return_desc(&r.descriptor);
        match r.owner.as_str() {
            COMPONENT_TYPE if r.name == "builder" && ret == format!("L{COMPONENT_BUILDER};") => {
                Ok(Some(self.new_object(COMPONENT_BUILDER)))
            }
            COMPONENT_BUILDER => {
                let Some(builder) = receiver.and_then(expect_ref).cloned() else {
                    return Ok(None);
                };
                if r.name == "build" && ret == format!("L{COMPONENT_TYPE};") {
                    let built = self.new_object(COMPONENT_TYPE);
                    if let Some(marker) = expect_ref(&built) {
                        self.host.note_content_builder(ContentBuilderEvent::Built {
                            builder,
                            built: marker.clone(),
                        });
                    }
                    return Ok(Some(built));
                }
                if ret != format!("L{COMPONENT_BUILDER};") {
                    return Ok(None);
                }
                let flag = match r.name.as_str() {
                    "persistent" => Some("persistent"),
                    "networkSynchronized" => Some("network"),
                    "cacheEncoding" => Some("cache_encoding"),
                    _ => None,
                };
                if let Some(flag) = flag {
                    let codec = (flag == "persistent")
                        .then(|| codec_label(args.first()))
                        .flatten();
                    self.host.note_content_builder(ContentBuilderEvent::Flag {
                        kind: ContentBuilderKind::DataComponent,
                        builder: builder.clone(),
                        flag,
                        codec,
                    });
                }
                Ok(Some(Value::Ref(Some(builder))))
            }
            ATTACHMENT_REGISTRY_CLASS => self.attachment_registry_call(r, args),
            ATTACHMENT_BUILDER => {
                let Some(builder) = receiver.and_then(expect_ref).cloned() else {
                    return Ok(None);
                };
                if r.name == "buildAndRegister" {
                    let Some(id) = args.first().and_then(identifier_id) else {
                        return Ok(None);
                    };
                    return Ok(Some(self.register_attachment(
                        Some(builder),
                        id,
                        Vec::new(),
                        None,
                    )));
                }
                if ret != format!("L{ATTACHMENT_BUILDER};") {
                    return Ok(None);
                }
                let flag = match r.name.as_str() {
                    "persistent" => Some("persistent"),
                    "copyOnDeath" => Some("copy_on_death"),
                    "initializer" => Some("initializer"),
                    "syncWith" => Some("synced"),
                    _ => None,
                };
                if let Some(flag) = flag {
                    let codec = (flag == "persistent")
                        .then(|| codec_label(args.first()))
                        .flatten();
                    self.host.note_content_builder(ContentBuilderEvent::Flag {
                        kind: ContentBuilderKind::Attachment,
                        builder: builder.clone(),
                        flag,
                        codec,
                    });
                }
                Ok(Some(Value::Ref(Some(builder))))
            }
            _ => Ok(None),
        }
    }

    /// `AttachmentRegistry`'s static surface.
    fn attachment_registry_call(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Option<Value>, NativeError> {
        let ret = return_desc(&r.descriptor);
        if r.name == "builder" && ret == format!("L{ATTACHMENT_BUILDER};") {
            return Ok(Some(self.new_object(ATTACHMENT_BUILDER)));
        }
        if ret != format!("L{ATTACHMENT_TYPE};") {
            return Ok(None);
        }
        let Some(id) = args.first().and_then(identifier_id) else {
            return Ok(None);
        };
        let consumer = r.descriptor.contains(";Ljava/util/function/Consumer;)");
        let value = match r.name.as_str() {
            "create" if consumer => {
                // The mod configures a builder in a lambda: run it on one, so
                // its setters reach the host as a chain's would.
                let builder = self.new_object(ATTACHMENT_BUILDER);
                if let Some(callback) = args.get(1) {
                    self.invoke_callback(callback, std::slice::from_ref(&builder))?;
                }
                self.register_attachment(expect_ref(&builder).cloned(), id, Vec::new(), None)
            }
            "create" => self.register_attachment(None, id, Vec::new(), None),
            "createPersistent" => {
                let codec = codec_label(args.get(1));
                self.register_attachment(None, id, vec!["persistent"], codec)
            }
            "createDefaulted" => self.register_attachment(None, id, vec!["initializer"], None),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    fn register_attachment(
        &mut self,
        builder: Option<String>,
        id: String,
        preset: Vec<&'static str>,
        codec: Option<String>,
    ) -> Value {
        let value = self.new_object(ATTACHMENT_TYPE);
        self.host
            .note_content_builder(ContentBuilderEvent::RegisterAttachment {
                builder,
                id,
                value: value.clone(),
                preset,
                codec,
            });
        value
    }
}

/// `declarations` with the jar's data recipes appended: every
/// `data/<ns>/recipe/<path>.json` outside the `minecraft` namespace (a mod adds
/// recipes, it does not replace vanilla's) that is a JSON object of at most
/// 64 KiB, as a `minecraft:recipe` row, sorted by id and capped at
/// [`MAX_DATA_RECIPES`]. An id the metadata already declares keeps the
/// metadata row, and a file that does not parse is skipped, as vanilla's
/// recipe manager logs and goes on.
pub(crate) fn with_data_recipes<R: std::io::Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    mut declarations: Vec<RegistryEntry>,
) -> Vec<RegistryEntry> {
    let declared: BTreeSet<String> = declarations.iter().map(RegistryEntry::key).collect();
    let mut names: Vec<(String, String)> = zip
        .file_names()
        .filter_map(|name| Some((recipe_id(name)?, name.to_owned())))
        .filter(|(id, _)| !declared.contains(&format!("minecraft:recipe/{id}")))
        .collect();
    names.sort();
    names.dedup_by(|a, b| a.0 == b.0);
    let mut added = 0;
    for (id, name) in names {
        if added >= MAX_DATA_RECIPES {
            break;
        }
        let Ok(mut file) = zip.by_name(&name) else {
            continue;
        };
        if file.size() > MAX_DATA_RECIPE_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(
            &mut std::io::Read::take(&mut file, MAX_DATA_RECIPE_BYTES + 1),
            &mut bytes,
        )
        .is_err()
            || bytes.len() as u64 > MAX_DATA_RECIPE_BYTES
        {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if !value.is_object() {
            continue;
        }
        let Ok(json) = serde_json::to_string(&value) else {
            continue;
        };
        declarations.push(RegistryEntry::typed(
            "minecraft:recipe",
            id,
            RegistryDeclaration::Recipe { json },
        ));
        added += 1;
    }
    declarations
}

/// The recipe id a jar entry is, `ns:path` for `data/<ns>/recipe/<path>.json`,
/// when both parts are valid resource-location text and `ns` is not vanilla's.
fn recipe_id(name: &str) -> Option<String> {
    let rest = name.strip_prefix("data/")?;
    let (namespace, rest) = rest.split_once('/')?;
    let path = rest.strip_prefix("recipe/")?.strip_suffix(".json")?;
    let namespace_ok = !namespace.is_empty()
        && namespace
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b));
    let path_ok = !path.is_empty()
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && path
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_./-".contains(&b));
    (namespace_ok && path_ok && namespace != "minecraft").then(|| format!("{namespace}:{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mod_namespace_recipe_files_are_recipe_ids() {
        assert_eq!(
            recipe_id("data/ruby/recipe/tools/pick.json").as_deref(),
            Some("ruby:tools/pick")
        );
        for name in [
            "data/minecraft/recipe/stick.json",
            "data/ruby/recipes/pick.json",
            "data/ruby/recipe/Pick.json",
            "data/ruby/recipe/pick.txt",
            "data/ruby/recipe/../pick.json",
            "data/ruby/advancement/recipe/pick.json",
            "assets/ruby/recipe/pick.json",
        ] {
            assert_eq!(recipe_id(name), None, "{name}");
        }
    }

    #[test]
    fn an_attachment_row_joins_builder_flags_and_its_preset() {
        let mut builders = ContentBuilders::default();
        builders.note(ContentBuilderEvent::Flag {
            kind: ContentBuilderKind::Attachment,
            builder: "b#1".into(),
            flag: "copy_on_death",
            codec: None,
        });
        let row = builders
            .note(ContentBuilderEvent::RegisterAttachment {
                builder: Some("b#1".into()),
                id: "ruby:mana".into(),
                value: Value::Ref(Some("t#2".into())),
                preset: vec!["persistent"],
                codec: Some("codec#3".into()),
            })
            .unwrap();
        assert_eq!(row.registry, ATTACHMENT_REGISTRY);
        let Some(RegistryDeclaration::Custom { kind, properties }) = row.declaration else {
            panic!("an attachment row is typed");
        };
        assert_eq!(kind, "attachment");
        let keys: Vec<(&str, &str)> = properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("codec", "codec#3"),
                ("copy_on_death", "true"),
                ("persistent", "true")
            ]
        );
    }
}
