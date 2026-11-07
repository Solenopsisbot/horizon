//! Game objects for the game-class hook tier.
//!
//! A `@Inject` callback the hook tier binds runs where a host hook point fires,
//! not inside a woven Minecraft class, so the object Mixin would hand it as
//! `this` (a `ServerPlayer`, a `LivingEntity`, the `MinecraftServer`) does not
//! exist in the interpreter. This module gives that object an identity and a
//! small, honest surface:
//!
//! - **Identity.** The host describes each object it passes as a
//!   [`GameObject`]: its vanilla class, entity id, UUID, name, position and the
//!   few other facts it already knows at the hook point. The interpreter sees a
//!   stable marker (`game/player/<uuid>`, `game/entity/<id>`, `game/server`),
//!   so the same entity is the same object at every firing.
//! - **Members.** A call on a marker answers from that description when the
//!   member is one this module models (`getUUID`, `getId`, `getX`,
//!   `getScoreboardName`, `level`, `getServer`...). Any other member, any
//!   `@Shadow` field of a Mixin class, and any other game-class call refuses
//!   with an error that names it. The callback stops there and the host decides
//!   what to do; nothing answers a neutral value the game never produced.
//! - **Mixin state.** Fields a Mixin class adds to its target (`shogi$cooldowns`)
//!   live in a [`GameHookState`] the host keeps per jar, keyed by the object's
//!   marker, so a value a callback stores on a player is there at the player's
//!   next tick. The Mixin class's no-argument constructor runs once per object,
//!   the first time a callback of that class sees it, which is where `javac`
//!   puts the field initialisers Mixin would merge into the target.
//!
//! The surface is deliberately small: it is what the bound callbacks of the
//! measured corpus reach first. Widening it is adding a row to
//! [`GameHookHost::answer`], never relaxing the refusal.

use super::*;

/// Where every game object marker starts. Markers never contain `#`, which the
/// interpreter reads as a jar object's `class#n`.
pub const GAME_OBJECT_PREFIX: &str = "game/";

/// Jar objects a hook callback builds are numbered from here, so they cannot
/// collide with the `class#n` markers an entrypoint run handed out before.
const GAME_HOOK_OBJECT_BASE: usize = 1 << 40;

/// Instance fields and arrays one jar's hook callbacks may keep alive between
/// firings. A callback that would exceed either fails instead of growing
/// without bound.
const MAX_GAME_HOOK_FIELDS: usize = 65_536;
const MAX_GAME_HOOK_ARRAYS: usize = 8_192;

/// Mixin's `@Shadow`: the member stands for one the target class declares.
const MIXIN_SHADOW_DESCRIPTOR: &str = "Lorg/spongepowered/asm/mixin/Shadow;";

/// What kind of game object a marker names; it decides the vanilla class
/// hierarchy the object answers `instanceof` for and the members it models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameObjectKind {
    /// A server player (`ServerPlayer`).
    Player,
    /// A living entity that is not a player (`LivingEntity`).
    Living,
    /// Any other entity (`Entity`).
    Entity,
    /// A dropped item (`ItemEntity`).
    ItemEntity,
    /// An item stack (`ItemStack`).
    ItemStack,
    /// A server level (`ServerLevel`).
    Level,
    /// The server (`MinecraftServer`).
    Server,
    /// A player's profile (`GameProfile`): their UUID and name.
    Profile,
    /// An item type (`Item`), keyed by its registry id.
    Item,
    /// An argument the host passes for its identity (a `DamageSource`, a
    /// `Connection`, a login cookie): every member refuses except the few the
    /// host gave a fact for (a connection's remote address, a cookie's
    /// profile).
    Opaque,
}

/// One game object a host hook point hands a callback, as the host knows it.
///
/// Every fact is optional: a member whose fact the host left out refuses
/// rather than answering a default. The constructors fill the identity; the
/// `with_*` methods add what the hook point knows.
#[derive(Debug, Clone, PartialEq)]
pub struct GameObject {
    pub kind: GameObjectKind,
    /// The most specific vanilla class the object is, as an internal name.
    pub class: String,
    /// The marker's last segment; unique per kind.
    pub key: String,
    pub entity_id: Option<i32>,
    pub uuid: Option<u128>,
    pub name: Option<String>,
    /// The entity or item type id (`minecraft:zombie`, `minecraft:diamond`).
    pub type_id: Option<String>,
    /// The level id the object is in (`minecraft:overworld`).
    pub dimension: Option<String>,
    pub position: Option<[f64; 3]>,
    pub health: Option<f32>,
    pub alive: Option<bool>,
    /// A player's game mode: 0 survival, 1 creative, 2 adventure, 3 spectator.
    pub game_mode: Option<i32>,
    /// A stack's (or a dropped item's stack's) count.
    pub count: Option<i32>,
    /// The server: whether it is a dedicated one.
    pub dedicated: Option<bool>,
    /// A connection's remote address as `SocketAddress.toString` prints it
    /// (`/127.0.0.1:52144`).
    pub address: Option<String>,
    /// A living entity's main-hand and off-hand stacks as (item id, count);
    /// an empty hand is `("minecraft:air", 0)`, as vanilla's `ItemStack.EMPTY`.
    pub main_hand: Option<(String, i32)>,
    pub off_hand: Option<(String, i32)>,
}

impl GameObject {
    fn new(kind: GameObjectKind, class: &str, key: String) -> Self {
        Self {
            kind,
            class: class.to_owned(),
            key: key.replace('#', "_"),
            entity_id: None,
            uuid: None,
            name: None,
            type_id: None,
            dimension: None,
            position: None,
            health: None,
            alive: None,
            game_mode: None,
            count: None,
            dedicated: None,
            address: None,
            main_hand: None,
            off_hand: None,
        }
    }

    /// A server player, keyed by UUID so it is the same object after a
    /// respawn, as the player's data is.
    pub fn player(uuid: u128, name: &str) -> Self {
        let mut object = Self::new(
            GameObjectKind::Player,
            "net/minecraft/server/level/ServerPlayer",
            uuid_text_of(uuid),
        );
        object.uuid = Some(uuid);
        object.name = Some(name.to_owned());
        object.type_id = Some("minecraft:player".to_owned());
        object
    }

    /// An entity by its id; `living` picks `LivingEntity` over `Entity`.
    pub fn entity(entity_id: i32, type_id: &str, living: bool) -> Self {
        let (kind, class) = if living {
            (
                GameObjectKind::Living,
                "net/minecraft/world/entity/LivingEntity",
            )
        } else {
            (GameObjectKind::Entity, "net/minecraft/world/entity/Entity")
        };
        let mut object = Self::new(kind, class, entity_id.to_string());
        object.entity_id = Some(entity_id);
        object.type_id = Some(type_id.to_owned());
        object
    }

    /// A dropped item entity holding `count` of `item`.
    pub fn item_entity(entity_id: i32, item: &str, count: i32) -> Self {
        let mut object = Self::new(
            GameObjectKind::ItemEntity,
            "net/minecraft/world/entity/item/ItemEntity",
            entity_id.to_string(),
        );
        object.entity_id = Some(entity_id);
        object.type_id = Some(item.to_owned());
        object.count = Some(count);
        object
    }

    /// An item stack; `key` tells two stacks of one firing apart.
    pub fn item_stack(key: &str, item: &str, count: i32) -> Self {
        let mut object = Self::new(
            GameObjectKind::ItemStack,
            "net/minecraft/world/item/ItemStack",
            key.to_owned(),
        );
        object.type_id = Some(item.to_owned());
        object.count = Some(count);
        object
    }

    /// The server level of `dimension`.
    pub fn level(dimension: &str) -> Self {
        let mut object = Self::new(
            GameObjectKind::Level,
            "net/minecraft/server/level/ServerLevel",
            dimension.to_owned(),
        );
        object.dimension = Some(dimension.to_owned());
        object
    }

    /// The server.
    pub fn server(dedicated: bool) -> Self {
        let class = if dedicated {
            "net/minecraft/server/dedicated/DedicatedServer"
        } else {
            "net/minecraft/server/MinecraftServer"
        };
        let mut object = Self::new(GameObjectKind::Server, class, "main".to_owned());
        object.dedicated = Some(dedicated);
        object
    }

    /// A player's `GameProfile`.
    pub fn profile(uuid: u128, name: &str) -> Self {
        let mut object = Self::new(
            GameObjectKind::Profile,
            "com/mojang/authlib/GameProfile",
            uuid_text_of(uuid),
        );
        object.uuid = Some(uuid);
        object.name = Some(name.to_owned());
        object
    }

    /// An item type, by registry id (`minecraft:diamond_sword`).
    pub fn item(id: &str) -> Self {
        let mut object = Self::new(
            GameObjectKind::Item,
            "net/minecraft/world/item/Item",
            id.replace([':', '/'], "_"),
        );
        object.type_id = Some(id.to_owned());
        object
    }

    /// An object passed for its identity only, of vanilla class `class`.
    pub fn opaque(class: &str, key: &str) -> Self {
        let simple = class.rsplit('/').next().unwrap_or(class);
        Self::new(
            GameObjectKind::Opaque,
            class,
            format!("{}/{key}", simple.to_ascii_lowercase()),
        )
    }

    pub fn with_entity_id(mut self, id: i32) -> Self {
        self.entity_id = Some(id);
        self
    }
    pub fn with_uuid(mut self, uuid: u128) -> Self {
        self.uuid = Some(uuid);
        self
    }
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = Some(name.to_owned());
        self
    }
    pub fn with_dimension(mut self, dimension: &str) -> Self {
        self.dimension = Some(dimension.to_owned());
        self
    }
    pub fn with_position(mut self, position: [f64; 3]) -> Self {
        self.position = Some(position);
        self
    }
    pub fn with_health(mut self, health: f32) -> Self {
        self.health = Some(health);
        self
    }
    pub fn with_alive(mut self, alive: bool) -> Self {
        self.alive = Some(alive);
        self
    }
    pub fn with_game_mode(mut self, mode: i32) -> Self {
        self.game_mode = Some(mode);
        self
    }
    pub fn with_address(mut self, address: &str) -> Self {
        self.address = Some(address.to_owned());
        self
    }
    /// The stacks in the main and off hand, as (item id, count).
    pub fn with_hands(mut self, main: (&str, i32), off: (&str, i32)) -> Self {
        self.main_hand = Some((main.0.to_owned(), main.1));
        self.off_hand = Some((off.0.to_owned(), off.1));
        self
    }

    /// The marker the interpreter sees for this object.
    pub fn marker(&self) -> String {
        let kind = match self.kind {
            GameObjectKind::Player => "player",
            GameObjectKind::Living | GameObjectKind::Entity => "entity",
            GameObjectKind::ItemEntity => "item",
            GameObjectKind::ItemStack => "stack",
            GameObjectKind::Level => "level",
            GameObjectKind::Server => "server",
            GameObjectKind::Profile => "profile",
            GameObjectKind::Item => "itemtype",
            GameObjectKind::Opaque => "opaque",
        };
        format!("{GAME_OBJECT_PREFIX}{kind}/{}", self.key)
    }

    /// The vanilla classes this object is an instance of, most specific
    /// first. Interfaces of vanilla classes are not listed: a callback that
    /// tests one gets `false`, which a refusal on its first call then names.
    fn hierarchy(&self) -> Vec<&str> {
        let mut classes = vec![self.class.as_str()];
        let rest: &[&str] = match self.kind {
            GameObjectKind::Player => &[
                "net/minecraft/server/level/ServerPlayer",
                "net/minecraft/world/entity/player/Player",
                "net/minecraft/world/entity/Avatar",
                "net/minecraft/world/entity/LivingEntity",
                "net/minecraft/world/entity/Entity",
            ],
            GameObjectKind::Living => &[
                "net/minecraft/world/entity/LivingEntity",
                "net/minecraft/world/entity/Entity",
            ],
            GameObjectKind::Entity => &["net/minecraft/world/entity/Entity"],
            GameObjectKind::ItemEntity => &[
                "net/minecraft/world/entity/item/ItemEntity",
                "net/minecraft/world/entity/Entity",
            ],
            GameObjectKind::ItemStack => &["net/minecraft/world/item/ItemStack"],
            GameObjectKind::Level => &[
                "net/minecraft/server/level/ServerLevel",
                "net/minecraft/world/level/Level",
            ],
            GameObjectKind::Server => &["net/minecraft/server/MinecraftServer"],
            GameObjectKind::Item => &["net/minecraft/world/item/Item"],
            GameObjectKind::Profile | GameObjectKind::Opaque => &[],
        };
        for class in rest {
            if !classes.contains(class) {
                classes.push(class);
            }
        }
        classes
    }
}

/// A change a callback asked the game to make, which the host applies after
/// the firing.
#[derive(Debug, Clone, PartialEq)]
pub enum GameHookEffect {
    /// `ServerPlayer#sendSystemMessage(Component)`: a system line in the
    /// player's chat.
    SystemMessage { player: u128, text: String },
}

/// The game objects of one firing, and what the callbacks asked of them.
///
/// The host builds one per firing: an object that is not in it (a player a
/// callback kept in a static field from an earlier firing who has since left)
/// refuses every member, because the host can no longer answer for it.
#[derive(Debug, Clone, Default)]
pub struct GameHookContext {
    objects: BTreeMap<String, GameObject>,
    effects: Vec<GameHookEffect>,
    /// Server-wide facts (world folder, game rules, registries, the channels
    /// each client takes): `facade_server.rs`. Empty unless the host set them,
    /// and then every member that needs one refuses.
    server: Arc<ServerFacts>,
}

impl GameHookContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// The server-wide facts this firing's members answer from.
    pub fn set_server_facts(&mut self, facts: Arc<ServerFacts>) {
        self.server = facts;
    }

    /// Add `object` and answer the value a callback receives for it.
    pub fn insert(&mut self, object: GameObject) -> Value {
        let marker = object.marker();
        self.objects.insert(marker.clone(), object);
        Value::Ref(Some(marker))
    }

    /// The object a marker names in this firing.
    pub fn object(&self, marker: &str) -> Option<&GameObject> {
        self.objects.get(marker)
    }

    /// What the callbacks asked the game to do, in call order.
    pub fn effects(&self) -> &[GameHookEffect] {
        &self.effects
    }

    pub fn take_effects(&mut self) -> Vec<GameHookEffect> {
        std::mem::take(&mut self.effects)
    }
}

/// What one jar's hook callbacks keep between firings: the instance fields
/// and arrays they wrote on game objects (and on the jar objects those hold),
/// which objects each Mixin class has initialised, and the Mixin model of the
/// jar. A host keeps one per jar and passes it to every firing.
#[derive(Debug, Clone, Default)]
pub struct GameHookState {
    fields: BTreeMap<String, Value>,
    arrays: BTreeMap<String, Vec<Value>>,
    next_object: usize,
    next_array: usize,
    /// `mixin class|marker` for every object a Mixin class's constructor ran on.
    initialized: BTreeSet<String>,
    model: Option<Arc<MixinModel>>,
}

impl GameHookState {
    /// How many instance fields the callbacks keep alive.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }

    /// Drop everything kept on one game object that no longer exists (a
    /// player who respawned as a new object, or left): its fields, and the
    /// record that its Mixin constructors ran, so a later object with the same
    /// marker starts fresh as a new Java object would.
    pub fn forget_object(&mut self, marker: &str) {
        let prefix = format!("{marker}|");
        self.fields.retain(|key, _| !key.starts_with(&prefix));
        let suffix = format!("|{marker}");
        self.initialized.retain(|key| !key.ends_with(&suffix));
    }
}

/// How the interpreter runs a jar method a host redirects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JarCallRedirect {
    /// Ask the host, as a call to this member (the game method a Mixin
    /// `@Shadow` stands for).
    Host(MethodRef),
    /// Run this jar method instead (the Mixin class's implementation of an
    /// interface it adds to the game object).
    Jar(MethodRef),
}

/// The Mixin classes of one jar that target game classes, read once.
#[derive(Debug, Default)]
struct MixinModel {
    /// Mixin class → the game classes it targets.
    targets: BTreeMap<String, Vec<String>>,
    /// Game class → the Mixin classes that target it.
    by_target: BTreeMap<String, Vec<String>>,
    /// Mixin class → the interfaces it adds to its targets.
    interfaces: BTreeMap<String, Vec<String>>,
    /// `class|name|descriptor` of every member that stands for a game method:
    /// `@Shadow`, abstract, `@Accessor` and `@Invoker` members of a Mixin class.
    shadow_methods: BTreeSet<String>,
    /// `class|name|descriptor` of every method with a body a Mixin class
    /// declares, which a redirect may run.
    concrete_methods: BTreeSet<String>,
    /// `class#name descriptor` (the interpreter's field-key suffix) of every
    /// `@Shadow` field of a Mixin class.
    shadow_fields: BTreeSet<String>,
}

impl MixinModel {
    fn build(classes: &BTreeMap<String, ClassFile>) -> Self {
        let mut model = Self::default();
        for (name, class) in classes {
            let Some(mixin) = class
                .annotations
                .iter()
                .find(|annotation| annotation.descriptor == MIXIN_ANNOTATION_DESCRIPTOR)
            else {
                continue;
            };
            let targets: Vec<String> = mixin_target_owners(mixin)
                .into_iter()
                .filter(|target| !classes.contains_key(target))
                .collect();
            if targets.is_empty() {
                continue;
            }
            for target in &targets {
                model
                    .by_target
                    .entry(target.clone())
                    .or_default()
                    .push(name.clone());
            }
            model.targets.insert(name.clone(), targets);
            model
                .interfaces
                .insert(name.clone(), class.interface_names());
            let (fields, methods) = member_annotations(class).unwrap_or_default();
            for (member, descriptors) in fields {
                if descriptors.iter().any(|d| d == MIXIN_SHADOW_DESCRIPTOR) {
                    let (field, descriptor) = member.split_once('|').unwrap_or((&member, ""));
                    model
                        .shadow_fields
                        .insert(format!("{name}#{field}{descriptor}"));
                }
            }
            for method in &class.methods {
                if method.name.starts_with('<') {
                    continue;
                }
                let key = format!("{}|{}", method.name, method.descriptor);
                let annotated = methods.get(&key).is_some_and(|descriptors| {
                    descriptors.iter().any(|d| {
                        d == MIXIN_SHADOW_DESCRIPTOR
                            || d == MIXIN_ACCESSOR_DESCRIPTOR
                            || d == MIXIN_INVOKER_DESCRIPTOR
                    })
                }) || method.annotations.iter().any(|a| {
                    a.descriptor == MIXIN_ACCESSOR_DESCRIPTOR
                        || a.descriptor == MIXIN_INVOKER_DESCRIPTOR
                });
                if annotated || method.access & 0x0400 != 0 {
                    model.shadow_methods.insert(format!("{name}|{key}"));
                } else if method.code.is_some() {
                    model.concrete_methods.insert(format!("{name}|{key}"));
                }
            }
        }
        model
    }

    /// The Mixin classes applied to an object of these vanilla classes.
    fn mixins_for<'a>(&'a self, hierarchy: &[&str]) -> impl Iterator<Item = &'a String> {
        let lists: Vec<&'a Vec<String>> = hierarchy
            .iter()
            .filter_map(|class| self.by_target.get(*class))
            .collect();
        lists.into_iter().flatten()
    }
}

/// Every field and method annotation descriptor of `class`, visible and
/// invisible, keyed `name|descriptor`. The parser keeps neither the invisible
/// method table nor any field annotation, and Mixin's `@Shadow` is
/// `CLASS`-retained, so the table is read back from the class bytes.
type MemberAnnotations = BTreeMap<String, Vec<String>>;

fn member_annotations(
    class: &ClassFile,
) -> Result<(MemberAnnotations, MemberAnnotations), NativeError> {
    let mut r = Reader::new(&class.raw);
    if r.u4()? != 0xCAFE_BABE {
        return Err(NativeError::InvalidClass("bad magic"));
    }
    r.skip(4)?;
    let pool = read_pool(&mut r)?;
    r.skip(6)?; // access, this, super
    let interfaces = r.u2()? as usize;
    r.skip(interfaces * 2)?;
    let fields = read_member_annotations(&mut r, &pool)?;
    let methods = read_member_annotations(&mut r, &pool)?;
    Ok((fields, methods))
}

fn read_member_annotations(
    r: &mut Reader<'_>,
    pool: &[Cp],
) -> Result<MemberAnnotations, NativeError> {
    let count = r.u2()? as usize;
    let mut out = BTreeMap::new();
    for _ in 0..count {
        r.u2()?; // access
        let name = utf8(pool, r.u2()?)?;
        let descriptor = utf8(pool, r.u2()?)?;
        let attributes = r.u2()? as usize;
        let mut descriptors = Vec::new();
        for _ in 0..attributes {
            let attribute = utf8(pool, r.u2()?)?;
            let length = r.u4()? as usize;
            let bytes = r.bytes(length)?;
            if attribute == "RuntimeInvisibleAnnotations"
                || attribute == "RuntimeVisibleAnnotations"
            {
                // A malformed table loses this member's annotations only.
                if let Ok(annotations) = read_annotations(&mut Reader::new(bytes), pool) {
                    descriptors.extend(annotations.into_iter().map(|a| a.descriptor));
                }
            }
        }
        if !descriptors.is_empty() {
            out.insert(format!("{name}|{descriptor}"), descriptors);
        }
    }
    Ok(out)
}

/// A game class the hook tier answers for itself rather than letting the
/// bootstrap host's neutral façade answer.
fn is_game_class(owner: &str) -> bool {
    owner.starts_with("net/minecraft/") || owner.starts_with("com/mojang/")
}

/// Static game-class calls that only build a value a callback then hands to a
/// modelled member (`Component.literal("hi")` for `sendSystemMessage`). The
/// bootstrap host answers them with the same markers the model reads.
/// Guava's no-argument collection factories, whose whole contract is a new,
/// empty, mutable collection: the kind the host builds for each. Callbacks
/// build their per-object state with them (`Maps.newHashMap()` in a Mixin's
/// field initialiser). A factory that takes elements or an expected size is
/// not listed and stays an unknown call.
fn empty_collection_factory(r: &MethodRef) -> Option<&'static str> {
    if !r.descriptor.starts_with("()") {
        return None;
    }
    match (r.owner.as_str(), r.name.as_str()) {
        (
            "com/google/common/collect/Maps",
            "newHashMap" | "newLinkedHashMap" | "newConcurrentMap",
        ) => Some("map"),
        (
            "com/google/common/collect/Lists",
            "newArrayList" | "newLinkedList" | "newCopyOnWriteArrayList",
        ) => Some("list"),
        (
            "com/google/common/collect/Sets",
            "newHashSet" | "newLinkedHashSet" | "newConcurrentHashSet",
        ) => Some("set"),
        _ => None,
    }
}

fn is_value_constructor(r: &MethodRef) -> bool {
    matches!(
        (r.owner.as_str(), r.name.as_str()),
        (
            "net/minecraft/network/chat/Component",
            "literal" | "translatable" | "empty" | "nullToEmpty"
        ) | (
            "net/minecraft/resources/Identifier",
            "fromNamespaceAndPath" | "withDefaultNamespace" | "parse" | "tryParse" | "tryBuild"
        ) | ("net/minecraft/resources/ResourceKey", "create")
    )
}

/// A game class whose instances are values the bootstrap host builds and
/// answers itself: a payload type is its channel id
/// (`new CustomPacketPayload.Type<>(id)` in a payload's static initialiser).
fn is_host_value_class(owner: &str) -> bool {
    owner == "net/minecraft/network/protocol/common/custom/CustomPacketPayload$Type"
}

/// A value marker the bootstrap host built and models the members of.
fn is_host_value_marker(marker: &str) -> bool {
    marker.starts_with("component/") || marker.starts_with("uuid/")
}

fn uuid_text_of(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A connection's remote address value: the rest is its `toString`.
const SOCKET_ADDRESS: &str = "socket-address/";

/// The refusal for a member the model does not answer.
fn unmodelled(marker: &str, r: &MethodRef) -> NativeError {
    NativeError::Metadata(format!(
        "game hook object {marker} does not model {}#{}{}",
        r.owner, r.name, r.descriptor
    ))
}

/// The host a hook callback runs against: the game objects of the firing in
/// front of the jar's own bootstrap host.
struct GameHookHost<'a, H> {
    inner: &'a mut H,
    context: &'a mut GameHookContext,
    model: &'a MixinModel,
}

impl<H: Host> GameHookHost<'_, H> {
    fn object(&self, marker: &str) -> Result<GameObject, NativeError> {
        self.context.object(marker).cloned().ok_or_else(|| {
            NativeError::Metadata(format!(
                "game hook object {marker} is not live at this hook point"
            ))
        })
    }

    /// Answer one member on a game object, or refuse it.
    fn answer(
        &mut self,
        marker: &str,
        r: &MethodRef,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        // `Object`'s own members hold for every object, live or not.
        match (r.name.as_str(), r.descriptor.as_str()) {
            ("<init>", "()V") if r.owner == "java/lang/Object" => return Ok(Value::Void),
            ("hashCode", "()I") => return Ok(Value::Int(stable_hash(marker))),
            ("equals", "(Ljava/lang/Object;)Z") => {
                let other = args.first().and_then(expect_ref);
                return Ok(Value::Int(i32::from(other.is_some_and(|o| o == marker))));
            }
            _ => {}
        }
        let object = self.object(marker)?;
        let fact = |value: Option<Value>| value.ok_or_else(|| unmodelled(marker, r));
        let entity = matches!(
            object.kind,
            GameObjectKind::Player
                | GameObjectKind::Living
                | GameObjectKind::Entity
                | GameObjectKind::ItemEntity
        );
        let living = matches!(object.kind, GameObjectKind::Player | GameObjectKind::Living);
        let player = object.kind == GameObjectKind::Player;
        let string = |text: &str| Value::Ref(Some(text.to_owned()));
        match (r.name.as_str(), r.descriptor.as_str()) {
            ("toString", "()Ljava/lang/String;") => Ok(string(&format!(
                "{}[{}]",
                object.class.rsplit('/').next().unwrap_or_default(),
                object.name.as_deref().unwrap_or(&object.key)
            ))),
            ("getId", "()I") if entity => fact(object.entity_id.map(Value::Int)),
            ("getUUID", "()Ljava/util/UUID;") if entity => fact(
                object
                    .uuid
                    .map(|uuid| uuid_value(&uuid_text_of(uuid)))
                    .transpose()?,
            ),
            ("getStringUUID", "()Ljava/lang/String;") if entity => {
                fact(object.uuid.map(|uuid| string(&uuid_text_of(uuid))))
            }
            ("getX", "()D") if entity => fact(object.position.map(|p| Value::Float(p[0]))),
            ("getY", "()D") if entity => fact(object.position.map(|p| Value::Float(p[1]))),
            ("getZ", "()D") if entity => fact(object.position.map(|p| Value::Float(p[2]))),
            ("isAlive", "()Z") if entity => fact(object.alive.map(|a| Value::Int(i32::from(a)))),
            ("isRemoved", "()Z") if entity => fact(object.alive.map(|a| Value::Int(i32::from(!a)))),
            ("getHealth", "()F") if living => {
                fact(object.health.map(|h| Value::Float(f64::from(h))))
            }
            ("isSpectator", "()Z") if player => {
                fact(object.game_mode.map(|m| Value::Int(i32::from(m == 3))))
            }
            ("isCreative", "()Z") if player => {
                fact(object.game_mode.map(|m| Value::Int(i32::from(m == 1))))
            }
            ("getScoreboardName", "()Ljava/lang/String;") if player => {
                fact(object.name.as_deref().map(string))
            }
            ("getName" | "getDisplayName", "()Lnet/minecraft/network/chat/Component;")
                if entity =>
            {
                fact(
                    object
                        .name
                        .as_deref()
                        .map(|name| Value::Ref(Some(format!("component/literal/{name}")))),
                )
            }
            ("sendSystemMessage", "(Lnet/minecraft/network/chat/Component;)V") if player => {
                let uuid = object.uuid.ok_or_else(|| unmodelled(marker, r))?;
                let text = args.first().map(component_text).unwrap_or_default();
                self.context
                    .effects
                    .push(GameHookEffect::SystemMessage { player: uuid, text });
                Ok(Value::Void)
            }
            ("level", "()Lnet/minecraft/world/level/Level;")
            | ("level", "()Lnet/minecraft/server/level/ServerLevel;")
            | ("getLevel", "()Lnet/minecraft/server/level/ServerLevel;")
            | ("serverLevel", "()Lnet/minecraft/server/level/ServerLevel;")
                if entity =>
            {
                let dimension = object
                    .dimension
                    .as_deref()
                    .ok_or_else(|| unmodelled(marker, r))?;
                Ok(self.context.insert(GameObject::level(dimension)))
            }
            ("getServer", "()Lnet/minecraft/server/MinecraftServer;")
                if entity || object.kind == GameObjectKind::Level =>
            {
                let server = self
                    .context
                    .objects
                    .values()
                    .find(|o| o.kind == GameObjectKind::Server)
                    .map(GameObject::marker)
                    .ok_or_else(|| unmodelled(marker, r))?;
                Ok(Value::Ref(Some(server)))
            }
            ("isClientSide", "()Z") if object.kind == GameObjectKind::Level => Ok(Value::Int(0)),
            ("isDedicatedServer", "()Z") if object.kind == GameObjectKind::Server => {
                fact(object.dedicated.map(|d| Value::Int(i32::from(d))))
            }
            ("getItem", "()Lnet/minecraft/world/item/ItemStack;")
                if object.kind == GameObjectKind::ItemEntity =>
            {
                let item = object
                    .type_id
                    .clone()
                    .ok_or_else(|| unmodelled(marker, r))?;
                let count = object.count.ok_or_else(|| unmodelled(marker, r))?;
                Ok(self
                    .context
                    .insert(GameObject::item_stack(&object.key, &item, count)))
            }
            ("getCount", "()I") if object.kind == GameObjectKind::ItemStack => {
                fact(object.count.map(Value::Int))
            }
            ("isEmpty", "()Z") if object.kind == GameObjectKind::ItemStack => {
                fact(object.count.map(|c| Value::Int(i32::from(c <= 0))))
            }
            ("getItem", "()Lnet/minecraft/world/item/Item;")
                if object.kind == GameObjectKind::ItemStack =>
            {
                let item = object
                    .type_id
                    .as_deref()
                    .ok_or_else(|| unmodelled(marker, r))?;
                Ok(self.context.insert(GameObject::item(item)))
            }
            ("is", "(Lnet/minecraft/world/item/Item;)Z")
                if object.kind == GameObjectKind::ItemStack =>
            {
                let other = args
                    .first()
                    .and_then(Self::is_game_object)
                    .and_then(|other| self.context.object(other))
                    .filter(|other| other.kind == GameObjectKind::Item)
                    .and_then(|other| other.type_id.clone())
                    .ok_or_else(|| unmodelled(marker, r))?;
                Ok(Value::Int(i32::from(object.type_id.as_deref() == Some(&other))))
            }
            ("getMainHandItem" | "getOffhandItem", "()Lnet/minecraft/world/item/ItemStack;")
                if living =>
            {
                let (hand, stack) = if r.name == "getMainHandItem" {
                    ("mainhand", &object.main_hand)
                } else {
                    ("offhand", &object.off_hand)
                };
                let (item, count) = stack.clone().ok_or_else(|| unmodelled(marker, r))?;
                Ok(self.context.insert(GameObject::item_stack(
                    &format!("{}-{hand}", object.key),
                    &item,
                    count,
                )))
            }
            ("getGameTime" | "getDayTime", "()J") if object.kind == GameObjectKind::Level => {
                let time = if r.name == "getGameTime" {
                    self.context.server.game_time
                } else {
                    self.context.server.day_time
                };
                fact(time.map(Value::Long))
            }
            // A player's profile, and a login cookie's (the host gives the
            // cookie the joining player's UUID and name).
            ("getGameProfile", "()Lcom/mojang/authlib/GameProfile;") if player => {
                self.profile_of(marker, r, &object)
            }
            ("gameProfile", "()Lcom/mojang/authlib/GameProfile;")
                if object.kind == GameObjectKind::Opaque
                    && object.class == "net/minecraft/server/network/CommonListenerCookie" =>
            {
                self.profile_of(marker, r, &object)
            }
            ("id" | "getId", "()Ljava/util/UUID;") if object.kind == GameObjectKind::Profile => {
                fact(
                    object
                        .uuid
                        .map(|uuid| uuid_value(&uuid_text_of(uuid)))
                        .transpose()?,
                )
            }
            ("name" | "getName", "()Ljava/lang/String;")
                if object.kind == GameObjectKind::Profile =>
            {
                fact(object.name.as_deref().map(string))
            }
            ("getRemoteAddress", "()Ljava/net/SocketAddress;")
                if object.kind == GameObjectKind::Opaque
                    && object.class == "net/minecraft/network/Connection" =>
            {
                fact(
                    object
                        .address
                        .as_deref()
                        .map(|address| string(&format!("{SOCKET_ADDRESS}{address}"))),
                )
            }
            _ => {
                // Server-wide members (world path, game rules, registry
                // access) on the server, a level or an entity.
                let server_wide = entity
                    || matches!(object.kind, GameObjectKind::Server | GameObjectKind::Level);
                match server_wide
                    .then(|| facade_server::server_member(&self.context.server, r, args))
                    .flatten()
                {
                    Some(answer) => answer.map_err(|_| unmodelled(marker, r)),
                    None => Err(unmodelled(marker, r)),
                }
            }
        }
    }

    /// The `GameProfile` of an object that knows a player's UUID and name.
    fn profile_of(
        &mut self,
        marker: &str,
        r: &MethodRef,
        object: &GameObject,
    ) -> Result<Value, NativeError> {
        match (object.uuid, object.name.as_deref()) {
            (Some(uuid), Some(name)) => Ok(self.context.insert(GameObject::profile(uuid, name))),
            _ => Err(unmodelled(marker, r)),
        }
    }

    /// A call on a value marker the hook tier hands out itself: a server-wide
    /// marker (`facade_server.rs`) or a connection's remote address.
    fn answer_value(
        &mut self,
        text: &str,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        if facade_server::owns(text) {
            return Some(
                facade_server::answer(&self.context.server, text, r, args).map_err(|_| {
                    NativeError::Metadata(format!(
                        "game hook value {text} does not model {}#{}{}",
                        r.owner, r.name, r.descriptor
                    ))
                }),
            );
        }
        // An `Identifier` value: its text and parts, as vanilla prints them.
        if let Some(id) = text.strip_prefix("identifier/")
            && let Some((namespace, path)) = id.split_once(':')
        {
            let answer = match (r.name.as_str(), r.descriptor.as_str()) {
                ("toString", "()Ljava/lang/String;") => Value::Ref(Some(id.to_owned())),
                ("getNamespace", "()Ljava/lang/String;") => Value::Ref(Some(namespace.to_owned())),
                ("getPath", "()Ljava/lang/String;") => Value::Ref(Some(path.to_owned())),
                // `Identifier.hashCode`: `31 * namespace.hashCode() + path.hashCode()`.
                ("hashCode", "()I") => Value::Int(
                    stable_hash(namespace)
                        .wrapping_mul(31)
                        .wrapping_add(stable_hash(path)),
                ),
                ("equals", "(Ljava/lang/Object;)Z") => Value::Int(i32::from(
                    args.first().and_then(expect_ref).is_some_and(|o| o == text),
                )),
                _ => return None,
            };
            return Some(Ok(answer));
        }
        // `BuiltInRegistries.ITEM.getKey(item)` for an item the firing names.
        if text == "registry/minecraft:item"
            && r.name == "getKey"
            && r.descriptor == "(Ljava/lang/Object;)Lnet/minecraft/resources/Identifier;"
        {
            let id = args
                .first()
                .and_then(Self::is_game_object)
                .and_then(|item| self.context.object(item))
                .filter(|item| item.kind == GameObjectKind::Item)
                .and_then(|item| item.type_id.clone())?;
            return Some(Ok(identifier_parse(&id)));
        }
        let address = text.strip_prefix(SOCKET_ADDRESS)?;
        Some(match (r.name.as_str(), r.descriptor.as_str()) {
            ("toString", "()Ljava/lang/String;") => Ok(Value::Ref(Some(address.to_owned()))),
            ("hashCode", "()I") => Ok(Value::Int(stable_hash(address))),
            ("equals", "(Ljava/lang/Object;)Z") => Ok(Value::Int(i32::from(
                args.first().and_then(expect_ref).is_some_and(|o| o == text),
            ))),
            _ => Err(NativeError::Metadata(format!(
                "game hook value {text} does not model {}#{}{}",
                r.owner, r.name, r.descriptor
            ))),
        })
    }

    fn is_game_object(value: &Value) -> Option<&str> {
        expect_ref(value)
            .map(String::as_str)
            .filter(|marker| marker.starts_with(GAME_OBJECT_PREFIX))
    }
}

impl<H: Host> Host for GameHookHost<'_, H> {
    fn get_static(&mut self, r: &MethodRef) -> Result<Value, NativeError> {
        // An instance-field read the callbacks never wrote reaches the host
        // too. A Mixin `@Shadow` field is the game object's own field, which
        // the model does not hold: refuse instead of answering 0/null.
        if self
            .model
            .shadow_fields
            .contains(&format!("{}#{}{}", r.owner, r.name, r.descriptor))
        {
            return Err(NativeError::Metadata(format!(
                "game hook callback reads the game's field {}#{} through @Shadow, which is not modelled",
                r.owner, r.name
            )));
        }
        self.inner.get_static(r)
    }

    fn invoke(&mut self, r: &MethodRef, args: &[Value]) -> Result<Value, NativeError> {
        if let Some(kind) = empty_collection_factory(r) {
            return self.inner.empty_mutable_collection(kind).ok_or_else(|| {
                NativeError::Metadata(format!(
                    "game hook callback calls {}#{}{}, and the host models no {kind}",
                    r.owner, r.name, r.descriptor
                ))
            });
        }
        // `ServerPlayNetworking.canSend(player, type)`: whether that player's
        // client takes the channel, from the server facts.
        let player = args
            .first()
            .and_then(Self::is_game_object)
            .and_then(|marker| self.context.object(marker))
            .filter(|object| object.kind == GameObjectKind::Player)
            .and_then(|object| object.uuid);
        let channel = args.get(1).and_then(|value| {
            facade_server::channel_of(value, |marker| self.inner.payload_type_id(marker))
        });
        if let Some(answer) =
            facade_server::can_send(&self.context.server, r, player, channel.as_deref())
        {
            return answer.map_err(|_| {
                NativeError::Metadata(format!(
                    "game hook callback calls {}#{}{} for a player or channel the firing does not name",
                    r.owner, r.name, r.descriptor
                ))
            });
        }
        if is_game_class(&r.owner) && !is_value_constructor(r) {
            return Err(NativeError::Metadata(format!(
                "game hook callback calls {}#{}{}, which the hook tier does not model",
                r.owner, r.name, r.descriptor
            )));
        }
        self.inner.invoke(r, args)
    }

    fn invoke_instance(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value, NativeError> {
        if let Some(text) = expect_ref(receiver).cloned()
            && let Some(answer) = self.answer_value(&text, r, args)
        {
            return answer;
        }
        if let Some(marker) = Self::is_game_object(receiver) {
            let marker = marker.to_owned();
            return self.answer(&marker, r, args);
        }
        if is_game_class(&r.owner)
            && !is_host_value_class(&r.owner)
            && !expect_ref(receiver).is_some_and(|marker| is_host_value_marker(marker))
        {
            return Err(NativeError::Metadata(format!(
                "game hook callback calls {}#{}{} on {}, which the hook tier does not model",
                r.owner,
                r.name,
                r.descriptor,
                expect_ref(receiver).map_or("null", String::as_str)
            )));
        }
        self.inner.invoke_instance(r, receiver, args)
    }

    fn mutable_collection_values(&self, value: &Value) -> Option<Vec<String>> {
        self.inner.mutable_collection_values(value)
    }

    fn service_providers(&self, service: &str) -> Vec<String> {
        self.inner.service_providers(service)
    }

    fn resource_stream_text(&mut self, marker: &str) -> Option<String> {
        self.inner.resource_stream_text(marker)
    }

    fn linked_class_table(&self) -> Option<&Arc<LinkedClassTable>> {
        self.inner.linked_class_table()
    }

    fn models_class(&self, name: &str) -> bool {
        self.inner.models_class(name)
    }

    fn throwable_message(&self, marker: &str) -> Option<String> {
        self.inner.throwable_message(marker)
    }

    fn record_throwable(&mut self, marker: &str, message: Option<String>) {
        self.inner.record_throwable(marker, message);
    }

    fn persistent_class_state(&self) -> (BTreeMap<String, Value>, BTreeSet<String>) {
        self.inner.persistent_class_state()
    }

    fn store_persistent_class_state(
        &mut self,
        fields: BTreeMap<String, Value>,
        initialized: BTreeSet<String>,
    ) -> Result<(), NativeError> {
        self.inner.store_persistent_class_state(fields, initialized)
    }

    fn take_persistent_heap(&mut self) -> Option<JavaHeap> {
        self.inner.take_persistent_heap()
    }

    fn store_persistent_heap(&mut self, heap: JavaHeap) -> Result<(), NativeError> {
        self.inner.store_persistent_heap(heap)
    }

    fn empty_mutable_collection(&mut self, kind: &str) -> Option<Value> {
        self.inner.empty_mutable_collection(kind)
    }

    fn payload_type_id(&self, marker: &str) -> Option<String> {
        self.inner.payload_type_id(marker)
    }

    fn redirect_jar_call(
        &self,
        class: &str,
        method: &Method,
        receiver: Option<&Value>,
    ) -> Option<JarCallRedirect> {
        let shadow = self.model.targets.get(class).and_then(|targets| {
            self.model
                .shadow_methods
                .contains(&format!("{class}|{}|{}", method.name, method.descriptor))
                .then(|| targets.first().cloned())
                .flatten()
        });
        if let Some(target) = shadow {
            return Some(JarCallRedirect::Host(MethodRef {
                owner: target,
                name: method.name.clone(),
                descriptor: method.descriptor.clone(),
            }));
        }
        if method.code.is_some() {
            return None;
        }
        // An abstract member on a game object: a Mixin class applied to the
        // object may implement it (an interface the Mixin adds). Otherwise the
        // host is asked, which refuses it, instead of the interpreter's
        // neutral answer for a body it cannot find.
        let marker = receiver.and_then(Self::is_game_object)?;
        let object = self.context.object(marker)?;
        let hierarchy = object.hierarchy();
        for mixin in self.model.mixins_for(&hierarchy) {
            let implements = self
                .model
                .interfaces
                .get(mixin)
                .is_some_and(|interfaces| interfaces.iter().any(|i| i == class));
            if implements
                && self
                    .model
                    .concrete_methods
                    .contains(&format!("{mixin}|{}|{}", method.name, method.descriptor))
            {
                return Some(JarCallRedirect::Jar(MethodRef {
                    owner: mixin.clone(),
                    name: method.name.clone(),
                    descriptor: method.descriptor.clone(),
                }));
            }
        }
        Some(JarCallRedirect::Host(MethodRef {
            owner: class.to_owned(),
            name: method.name.clone(),
            descriptor: method.descriptor.clone(),
        }))
    }

    fn host_instance_of(&self, marker: &str, wanted: &str) -> bool {
        let Some(object) = self.context.object(marker) else {
            return false;
        };
        let hierarchy = object.hierarchy();
        if hierarchy.contains(&wanted) {
            return true;
        }
        self.model.mixins_for(&hierarchy).any(|mixin| {
            mixin == wanted
                || self
                    .model
                    .interfaces
                    .get(mixin)
                    .is_some_and(|interfaces| interfaces.iter().any(|i| i == wanted))
        })
    }
}

impl NativeJar {
    /// Run the callbacks a jar registered at one host hook point, against the
    /// game objects of this firing and the state earlier firings left.
    ///
    /// This is [`Self::run_game_hook`] for a host that fires the point from
    /// the game: `context` holds the objects the receiver and the arguments
    /// name (see [`GameHookContext::insert`]), `state` is the jar's own and
    /// persists between firings, and `budget` bounds the firing's wall-clock
    /// time. A call the model does not answer fails the firing with an error
    /// naming it; the callbacks that ran before it keep their effects, and the
    /// host decides whether to fire the jar again.
    ///
    /// On a resident host (`StdHost`) the callbacks run on the jar's own
    /// objects (`heap.rs`): what its entrypoint built and earlier firings
    /// changed, through statics or lambda captures. Their writes to those
    /// objects and to statics stay even when the firing fails, as a JVM's
    /// would; only the fields a failed firing wrote on game objects are
    /// dropped. Fields on game objects always stay in `state`, so
    /// [`GameHookState::forget_object`] still forgets them per object.
    #[allow(clippy::too_many_arguments)]
    pub fn run_game_hook_in<H: Host>(
        &self,
        registrations: &GameHookRegistrations,
        state: &mut GameHookState,
        context: &mut GameHookContext,
        hook: &str,
        receiver: Option<Value>,
        args: &[Value],
        host: &mut H,
        budget: &ExecutionBudget,
    ) -> Result<GameHookRun, NativeError> {
        self.run_game_hook_returning(
            registrations,
            state,
            context,
            hook,
            receiver,
            args,
            None,
            host,
            budget,
        )
    }

    /// [`Self::run_game_hook_in`] for a site whose vanilla method returns a
    /// value (`hook_shapes.rs`).
    ///
    /// `actual` is the method's own result at a `RETURN`/`TAIL` site, which
    /// the callbacks read with `CallbackInfoReturnable.getReturnValue`; every
    /// other site passes `None`, and a mismatch is an error before any
    /// callback runs. [`GameHookRun::returned`] is what the host must make the
    /// method return: at a `HEAD` site, set exactly when a cancellable
    /// callback cancelled (its `setReturnValue` value, else the type's
    /// default), so the host skips its own work; at a `RETURN`/`TAIL` site,
    /// a cancellable callback's override of `actual`.
    ///
    /// Mixin's rule for a callback that is not `cancellable` holds here too:
    /// its `cancel()` or `setReturnValue()` throws `CancellationException`, so
    /// the firing fails with that error rather than dropping the call.
    #[allow(clippy::too_many_arguments)]
    pub fn run_game_hook_returning<H: Host>(
        &self,
        registrations: &GameHookRegistrations,
        state: &mut GameHookState,
        context: &mut GameHookContext,
        hook: &str,
        receiver: Option<Value>,
        args: &[Value],
        actual: Option<Value>,
        host: &mut H,
        budget: &ExecutionBudget,
    ) -> Result<GameHookRun, NativeError> {
        let entries = registrations.entries(hook);
        let Some(first) = entries.first() else {
            return Err(NativeError::Metadata(format!(
                "no native game hook callback is registered at {hook}"
            )));
        };
        let first_site = game_hook_site_named(&first.hook, first.version).ok_or_else(|| {
            NativeError::Metadata(format!(
                "game hook registration names {} version {}, which this runtime does not know",
                first.hook, first.version
            ))
        })?;
        hook_shapes::check_actual_result(first_site, actual.as_ref())?;
        let expected = arg_count(&first.target.descriptor);
        if args.len() != expected {
            return Err(NativeError::Metadata(format!(
                "game hook point {hook} passes {} argument(s) to {}#{}{}, got {}",
                expected,
                first.target.owner,
                first.target.name,
                first.target.descriptor,
                args.len()
            )));
        }
        let mut checked = Vec::with_capacity(entries.len());
        for entry in entries {
            let site = game_hook_site_named(&entry.hook, entry.version).ok_or_else(|| {
                NativeError::Metadata(format!(
                    "game hook registration names {} version {}, which this runtime does not know",
                    entry.hook, entry.version
                ))
            })?;
            game_hook_callback(&self.classes, &entry.callback, site)
                .map_err(|reason| game_hook_error(&reason))?;
            checked.push(self.game_hook_callback_values(entry, args)?);
        }
        let model = Arc::clone(
            state
                .model
                .get_or_insert_with(|| Arc::new(MixinModel::build(&self.classes))),
        );
        let (persistent_static_fields, persistent_initialized_classes) =
            host.persistent_class_state();
        let mut wrapped = GameHookHost {
            inner: host,
            context,
            model: &model,
        };
        let linked = wrapped.linked_class_table().cloned().unwrap_or_default();
        let mut callback_infos = CallbackInfos::new();
        let mut results = Vec::with_capacity(entries.len());
        let mut cancelled = false;
        let mut initialized = state.initialized.clone();
        let mut runtime = Runtime {
            classes: &self.classes,
            linked,
            host: &mut wrapped,
            calls: 0,
            fields: state.fields.clone(),
            static_fields: persistent_static_fields,
            initialized_classes: persistent_initialized_classes,
            arrays: state.arrays.clone(),
            next_array: state.next_array.max(GAME_HOOK_OBJECT_BASE),
            next_object: state.next_object.max(GAME_HOOK_OBJECT_BASE),
            streams: BTreeMap::new(),
            next_stream: 0,
            enum_constants: BTreeMap::new(),
            string_builders: BTreeMap::new(),
            string_readers: BTreeMap::new(),
            io_writers: BTreeMap::new(),
            io_paths: BTreeMap::new(),
            io_files: BTreeMap::new(),
            annotations: BTreeMap::new(),
            next_annotation: 0,
            callback_infos: &mut callback_infos,
            invoke_hooks: None,
            anchor_hooks: None,
            modify_hooks: None,
            pending_skipped_new: Vec::new(),
            depth: 0,
            budget,
        };
        // `game_hook_callback_values` answers whether the callback is an
        // instance method: only those receive the game object as `this`.
        // The jar's own objects: what its entrypoint built and earlier
        // firings changed. Fields on game objects come from `state`.
        let resident = runtime.adopt_resident_heap();
        let outcome = (|| -> Result<(), NativeError> {
            for (entry, (is_instance, values)) in entries.iter().zip(checked) {
                let marker = callback_info_marker(&entry.target);
                // Every callback at one hook point shares the object Mixin would
                // hand the injection point, so a `cancel()` in one is what the
                // next one observes.
                runtime.open_callback_info(&marker)?;
                if let Some(actual) = &actual {
                    runtime.callback_infos.set_actual(&marker, actual.clone())?;
                }
                let before = hook_shapes::callback_info_snapshot(runtime.callback_infos, &marker)?;
                let callback_receiver = is_instance.then(|| {
                    receiver
                        .clone()
                        .unwrap_or_else(|| Value::Ref(Some(entry.callback.owner.clone())))
                });
                // The first callback of a Mixin class on a game object runs the
                // class's constructor on it: `javac` put the Mixin's field
                // initialisers there, which real Mixin merges into the target's
                // constructors. A Mixin that extends a game class has no
                // no-argument constructor to run, and its fields start at Java's
                // defaults.
                if let Some(Value::Ref(Some(object))) = &callback_receiver
                    && object.starts_with(GAME_OBJECT_PREFIX)
                    && initialized.insert(format!("{}|{object}", entry.callback.owner))
                    && self
                        .classes
                        .get(&entry.callback.owner)
                        .is_some_and(|class| {
                            class.super_name == "java/lang/Object"
                                && class.method("<init>", "()V").is_some()
                        })
                {
                    runtime.invoke(
                        &entry.callback.owner,
                        "<init>",
                        "()V",
                        callback_receiver.clone(),
                        &[],
                    )?;
                }
                let result = runtime.invoke(
                    &entry.callback.owner,
                    &entry.callback.name,
                    &entry.callback.descriptor,
                    callback_receiver,
                    &values,
                )?;
                results.push(result);
                let after = hook_shapes::callback_info_snapshot(runtime.callback_infos, &marker)?;
                if !entry.cancellable {
                    // Mixin's `CallbackInfo.cancel()` (and the `cancel()` a
                    // `setReturnValue` makes) throws on a callback that was not
                    // declared cancellable.
                    if after != before {
                        return Err(NativeError::Metadata(format!(
                            "uncaught exception java/util/concurrent/CancellationException: game hook callback {}#{}{} cancelled or set the result of {}#{}, which its @Inject does not declare cancellable",
                            entry.callback.owner,
                            entry.callback.name,
                            entry.callback.descriptor,
                            entry.target.owner,
                            entry.target.name
                        )));
                    }
                    continue;
                }
                // `CallbackInfoReturnable.setReturnValue` cancels as it sets.
                if after.0 || after.1.is_some() {
                    cancelled = true;
                    break;
                }
            }
            // A write to a `@Shadow` field is a write to the game object's own
            // field, which the host never sees: that callback did not do what
            // it meant to, so the firing fails instead of keeping the write.
            if let Some(key) = runtime.fields.keys().find(|key| {
                key.split_once('|').is_some_and(|(object, field)| {
                    object.starts_with(GAME_OBJECT_PREFIX) && model.shadow_fields.contains(field)
                })
            }) {
                return Err(NativeError::Metadata(format!(
                    "game hook callback writes the game's field {key} through @Shadow, which the host does not apply"
                )));
            }
            if !resident
                && (runtime.fields.len() > MAX_GAME_HOOK_FIELDS
                    || runtime.arrays.len() > MAX_GAME_HOOK_ARRAYS)
            {
                return Err(NativeError::Metadata(format!(
                    "game hook callbacks keep more than {MAX_GAME_HOOK_FIELDS} fields or {MAX_GAME_HOOK_ARRAYS} arrays"
                )));
            }
            Ok(())
        })();
        let returned = outcome.as_ref().ok().map(|()| {
            hook_shapes::game_hook_returned(
                first_site,
                runtime.callback_infos,
                &callback_info_marker(&first.target),
                cancelled,
            )
        });
        if !resident {
            // A host without a resident heap: `state` keeps every object, and
            // a failed firing keeps nothing, as before the heap.
            outcome?;
            runtime.persist_class_state()?;
            state.fields = std::mem::take(&mut runtime.fields);
            state.arrays = std::mem::take(&mut runtime.arrays);
        } else {
            // Game objects' fields stay with `state`, kept per object; the
            // jar's objects go back to the host, which keeps what the statics,
            // its registrations or those game-object fields still reach. A
            // failed firing's game-object writes are dropped.
            let (game, jar): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut runtime.fields)
                .into_iter()
                .partition(|(key, _)| key.starts_with(GAME_OBJECT_PREFIX));
            runtime.fields = jar;
            if outcome.is_ok() {
                state.fields = game;
            }
            state.arrays.clear();
            let roots = state
                .fields
                .values()
                .filter_map(|value| expect_ref(value).cloned())
                .collect();
            let persistence = runtime.persist_class_state();
            let heap = runtime.persist_resident_heap_with(roots);
            outcome?;
            persistence?;
            heap?;
        }
        state.next_object = runtime.next_object;
        state.next_array = runtime.next_array;
        state.initialized = initialized;
        Ok(GameHookRun {
            hook: hook.to_owned(),
            ran: results.len(),
            cancelled,
            returned: returned.transpose()?.flatten(),
            results,
        })
    }
}
