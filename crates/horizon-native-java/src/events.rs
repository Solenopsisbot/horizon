//! Game events a jar subscribes to, named at registration and run later.
//!
//! A Fabric, NeoForge or Forge mod hooks the server by registering a listener
//! during its entrypoint: `ServerTickEvents.END_SERVER_TICK.register(...)`,
//! `NeoForge.EVENT_BUS.addListener(this::onTick)`, an `@EventBusSubscriber`
//! class, or Forge's per-event `ServerTickEvent.Post.BUS.addListener(...)`.
//! The interpreter records those calls as host invocations; this module turns
//! them into [`GameListener`]s with a stable identity ([`GameEvent`], the
//! loader, and the Fabric field or event class it came from) and runs one
//! listener when the game fires its event ([`NativeJar::run_game_listener`]).
//!
//! # The façade a listener sees
//!
//! A callback receives bounded markers, never live game objects: the server is
//! `game/server`, a level `game/level/<dimension>`, a player
//! `game/player/<uuid>`, a NeoForge/Forge event object `game-event/<event>`.
//! The game hands a snapshot ([`EventFacade`]) and the firing's context
//! ([`EventContext`]) to each firing, and the markers answer the few methods
//! callbacks call first (`getTickCount`, `getPlayerList().getPlayers()`,
//! `level.dimension()`, `player.getUUID()`, `event.getServer()`,
//! `setCanceled`...). Any other instance call on a façade marker fails the
//! listener with [`NativeError::UnknownHostCall`] naming that call: the caller
//! disables the listener and logs the reason once. A neutral `null`/`0` would
//! let the callback carry on with made-up state, which is worse than not
//! running it.
//!
//! # Results and cancellation
//!
//! [`EventFiring`] carries the firing's outcome across listeners (and across
//! jars, since each jar has its own host):
//!
//! - Fabric `boolean` callbacks (`ALLOW_DEATH`, `PlayerBlockBreakEvents.BEFORE`)
//!   stop at the first `false` and cancel, as Fabric's array-backed invoker does.
//! - Fabric `InteractionResult` callbacks (`UseBlockCallback`...) stop at the
//!   first result that is not `PASS`, which then replaces vanilla's handling.
//! - NeoForge/Forge listeners cancel with `setCanceled(true)` (or a Forge
//!   `Predicate` listener returning `true`); a later listener that did not ask
//!   for cancelled events (`receiveCanceled`) is skipped.
//!
//! Priorities are not modelled: listeners run in registration order, jar by jar.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    ExecutionBudget, HostInvocation, MethodRef, NativeError, NativeJar, StdHost, Value, arg_count,
    component_value, expect_ref, parse_callback_marker, return_desc, stable_hash, uuid_value,
};

/// One game event the game can fire, whatever loader named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GameEvent {
    /// Before the levels load (Fabric `SERVER_STARTING`, NeoForge/Forge
    /// `ServerAboutToStartEvent`).
    ServerStarting,
    /// After the levels load and the server is ready (Fabric `SERVER_STARTED`;
    /// NeoForge/Forge `ServerStartingEvent` then `ServerStartedEvent`).
    ServerStarted,
    /// The stop begins, everyone still connected.
    ServerStopping,
    /// After the final save.
    ServerStopped,
    /// Each server tick, before any level ticks.
    ServerTickStart,
    /// Each server tick, after every level ticked.
    ServerTickEnd,
    /// One level's tick starts.
    LevelTickStart,
    /// One level's tick ends.
    LevelTickEnd,
    /// A level loaded.
    LevelLoad,
    /// A level unloaded.
    LevelUnload,
    /// A player finished joining.
    PlayerJoin,
    /// A player left.
    PlayerLeave,
    /// An entity was added to a level.
    EntityLoad,
    /// A living entity is about to die; cancellable.
    LivingAllowDeath,
    /// A living entity died.
    LivingAfterDeath,
    /// A player uses (right-clicks) a block; result-returning.
    UseBlock,
    /// A player uses the held item; result-returning.
    UseItem,
    /// A player attacks an entity; result-returning.
    AttackEntity,
    /// A player is about to break a block; cancellable.
    BlockBreakBefore,
    /// A player broke a block.
    BlockBreakAfter,
}

impl GameEvent {
    /// Every event, in declaration order (the bit order of [`Self::bit`]).
    pub const ALL: [GameEvent; 20] = [
        GameEvent::ServerStarting,
        GameEvent::ServerStarted,
        GameEvent::ServerStopping,
        GameEvent::ServerStopped,
        GameEvent::ServerTickStart,
        GameEvent::ServerTickEnd,
        GameEvent::LevelTickStart,
        GameEvent::LevelTickEnd,
        GameEvent::LevelLoad,
        GameEvent::LevelUnload,
        GameEvent::PlayerJoin,
        GameEvent::PlayerLeave,
        GameEvent::EntityLoad,
        GameEvent::LivingAllowDeath,
        GameEvent::LivingAfterDeath,
        GameEvent::UseBlock,
        GameEvent::UseItem,
        GameEvent::AttackEntity,
        GameEvent::BlockBreakBefore,
        GameEvent::BlockBreakAfter,
    ];

    /// The stable name logs and reports use.
    pub fn name(self) -> &'static str {
        match self {
            GameEvent::ServerStarting => "server_starting",
            GameEvent::ServerStarted => "server_started",
            GameEvent::ServerStopping => "server_stopping",
            GameEvent::ServerStopped => "server_stopped",
            GameEvent::ServerTickStart => "server_tick_start",
            GameEvent::ServerTickEnd => "server_tick_end",
            GameEvent::LevelTickStart => "level_tick_start",
            GameEvent::LevelTickEnd => "level_tick_end",
            GameEvent::LevelLoad => "level_load",
            GameEvent::LevelUnload => "level_unload",
            GameEvent::PlayerJoin => "player_join",
            GameEvent::PlayerLeave => "player_leave",
            GameEvent::EntityLoad => "entity_load",
            GameEvent::LivingAllowDeath => "living_allow_death",
            GameEvent::LivingAfterDeath => "living_after_death",
            GameEvent::UseBlock => "use_block",
            GameEvent::UseItem => "use_item",
            GameEvent::AttackEntity => "attack_entity",
            GameEvent::BlockBreakBefore => "block_break_before",
            GameEvent::BlockBreakAfter => "block_break_after",
        }
    }

    /// One bit per event, for a caller's "does anything listen" mask.
    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// Whether a NeoForge/Forge listener's `setCanceled(true)` means anything.
    pub fn cancellable(self) -> bool {
        matches!(
            self,
            GameEvent::LivingAllowDeath
                | GameEvent::UseBlock
                | GameEvent::UseItem
                | GameEvent::AttackEntity
                | GameEvent::BlockBreakBefore
        )
    }
}

/// Which loader API a listener was registered through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventLoader {
    Fabric,
    NeoForge,
    Forge,
}

/// The argument list a Fabric callback interface declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `(MinecraftServer)`
    Server,
    /// `(ServerLevel)`
    Level,
    /// `(MinecraftServer, ServerLevel)`
    ServerLevel,
    /// `(ServerGamePacketListenerImpl, PacketSender, MinecraftServer)`
    Join,
    /// `(ServerGamePacketListenerImpl, MinecraftServer)`
    Disconnect,
    /// `(ServerPlayer)`
    Player,
    /// `(Entity, ServerLevel)`
    EntityLevel,
    /// `(LivingEntity, DamageSource, float)`
    EntityDamageAmount,
    /// `(LivingEntity, DamageSource)`
    EntityDamage,
    /// `(Level, Player, BlockPos, BlockState, BlockEntity)`
    BlockBreak,
    /// `(Player, Level, InteractionHand, BlockHitResult)`
    UseBlock,
    /// `(Player, Level, InteractionHand)`
    UseItem,
    /// `(Player, Level, InteractionHand, Entity, EntityHitResult)`
    AttackEntity,
}

/// How a Fabric callback's return value combines across listeners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combine {
    Void,
    /// `boolean`: the first `false` stops and cancels.
    Allow,
    /// `InteractionResult`: the first non-`PASS` stops and becomes the result.
    Interaction,
}

/// One Fabric API `Event` field the game dispatches, with its callback
/// interface method (the one an object listener implements).
struct FabricField {
    owner: &'static str,
    field: &'static str,
    method: &'static str,
    descriptor: &'static str,
    event: GameEvent,
    shape: Shape,
}

/// A class of Fabric API's lifecycle-events module, where most fields live.
macro_rules! lifecycle {
    ($class:literal) => {
        concat!("net/fabricmc/fabric/api/event/lifecycle/v1/", $class)
    };
}

/// The Fabric API 26.3 (`fabric-api-0.165.0+26.3`) fields, read from its
/// class files with `javap`. Ordered by how many corpus jars use them.
const FABRIC_FIELDS: &[FabricField] = &[
    fabric(
        lifecycle!("ServerLifecycleEvents"),
        "SERVER_STARTED",
        "onServerStarted",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerStarted,
        Shape::Server,
    ),
    fabric(
        lifecycle!("ServerLifecycleEvents"),
        "SERVER_STARTING",
        "onServerStarting",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerStarting,
        Shape::Server,
    ),
    fabric(
        lifecycle!("ServerLifecycleEvents"),
        "SERVER_STOPPED",
        "onServerStopped",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerStopped,
        Shape::Server,
    ),
    fabric(
        lifecycle!("ServerLifecycleEvents"),
        "SERVER_STOPPING",
        "onServerStopping",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerStopping,
        Shape::Server,
    ),
    FabricField {
        owner: "net/fabricmc/fabric/api/networking/v1/ServerPlayConnectionEvents",
        field: "JOIN",
        method: "onPlayReady",
        descriptor: "(Lnet/minecraft/server/network/ServerGamePacketListenerImpl;Lnet/fabricmc/fabric/api/networking/v1/PacketSender;Lnet/minecraft/server/MinecraftServer;)V",
        event: GameEvent::PlayerJoin,
        shape: Shape::Join,
    },
    fabric(
        lifecycle!("ServerTickEvents"),
        "END_SERVER_TICK",
        "onEndTick",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerTickEnd,
        Shape::Server,
    ),
    fabric(
        lifecycle!("ServerLevelEvents"),
        "LOAD",
        "onLevelLoad",
        "(Lnet/minecraft/server/MinecraftServer;Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::LevelLoad,
        Shape::ServerLevel,
    ),
    fabric(
        lifecycle!("ServerTickEvents"),
        "START_LEVEL_TICK",
        "onStartTick",
        "(Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::LevelTickStart,
        Shape::Level,
    ),
    FabricField {
        owner: "net/fabricmc/fabric/api/networking/v1/ServerPlayConnectionEvents",
        field: "DISCONNECT",
        method: "onPlayDisconnect",
        descriptor: "(Lnet/minecraft/server/network/ServerGamePacketListenerImpl;Lnet/minecraft/server/MinecraftServer;)V",
        event: GameEvent::PlayerLeave,
        shape: Shape::Disconnect,
    },
    fabric(
        lifecycle!("ServerTickEvents"),
        "START_SERVER_TICK",
        "onStartTick",
        "(Lnet/minecraft/server/MinecraftServer;)V",
        GameEvent::ServerTickStart,
        Shape::Server,
    ),
    fabric(
        lifecycle!("ServerTickEvents"),
        "END_LEVEL_TICK",
        "onEndTick",
        "(Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::LevelTickEnd,
        Shape::Level,
    ),
    fabric(
        lifecycle!("ServerLevelEvents"),
        "UNLOAD",
        "onLevelUnload",
        "(Lnet/minecraft/server/MinecraftServer;Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::LevelUnload,
        Shape::ServerLevel,
    ),
    fabric(
        lifecycle!("ServerEntityEvents"),
        "ENTITY_LOAD",
        "onLoad",
        "(Lnet/minecraft/world/entity/Entity;Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::EntityLoad,
        Shape::EntityLevel,
    ),
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerPlayerEvents",
        field: "JOIN",
        method: "onJoin",
        descriptor: "(Lnet/minecraft/server/level/ServerPlayer;)V",
        event: GameEvent::PlayerJoin,
        shape: Shape::Player,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerPlayerEvents",
        field: "LEAVE",
        method: "onLeave",
        descriptor: "(Lnet/minecraft/server/level/ServerPlayer;)V",
        event: GameEvent::PlayerLeave,
        shape: Shape::Player,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerLivingEntityEvents",
        field: "ALLOW_DEATH",
        method: "allowDeath",
        descriptor: "(Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/damagesource/DamageSource;F)Z",
        event: GameEvent::LivingAllowDeath,
        shape: Shape::EntityDamageAmount,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerLivingEntityEvents",
        field: "AFTER_DEATH",
        method: "afterDeath",
        descriptor: "(Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/damagesource/DamageSource;)V",
        event: GameEvent::LivingAfterDeath,
        shape: Shape::EntityDamage,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/UseBlockCallback",
        field: "EVENT",
        method: "interact",
        descriptor: "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;",
        event: GameEvent::UseBlock,
        shape: Shape::UseBlock,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/UseItemCallback",
        field: "EVENT",
        method: "interact",
        descriptor: "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;)Lnet/minecraft/world/InteractionResult;",
        event: GameEvent::UseItem,
        shape: Shape::UseItem,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/AttackEntityCallback",
        field: "EVENT",
        method: "interact",
        descriptor: "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/entity/Entity;Lnet/minecraft/world/phys/EntityHitResult;)Lnet/minecraft/world/InteractionResult;",
        event: GameEvent::AttackEntity,
        shape: Shape::AttackEntity,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents",
        field: "BEFORE",
        method: "beforeBlockBreak",
        descriptor: "(Lnet/minecraft/world/level/Level;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;)Z",
        event: GameEvent::BlockBreakBefore,
        shape: Shape::BlockBreak,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents",
        field: "AFTER",
        method: "afterBlockBreak",
        descriptor: "(Lnet/minecraft/world/level/Level;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;)V",
        event: GameEvent::BlockBreakAfter,
        shape: Shape::BlockBreak,
    },
];

/// One table row, positionally.
const fn fabric(
    owner: &'static str,
    field: &'static str,
    method: &'static str,
    descriptor: &'static str,
    event: GameEvent,
    shape: Shape,
) -> FabricField {
    FabricField {
        owner,
        field,
        method,
        descriptor,
        event,
        shape,
    }
}

/// NeoForge (`net/neoforged/neoforge/event/`) and Forge
/// (`net/minecraftforge/event/`) event classes, by the path under that root.
/// The two loaders share most names; Forge keeps its tick events nested in
/// `TickEvent`.
const BUS_EVENTS: &[(&str, GameEvent)] = &[
    ("server/ServerAboutToStartEvent", GameEvent::ServerStarting),
    ("server/ServerStartingEvent", GameEvent::ServerStarted),
    ("server/ServerStartedEvent", GameEvent::ServerStarted),
    ("server/ServerStoppingEvent", GameEvent::ServerStopping),
    ("server/ServerStoppedEvent", GameEvent::ServerStopped),
    ("tick/ServerTickEvent$Pre", GameEvent::ServerTickStart),
    ("tick/ServerTickEvent$Post", GameEvent::ServerTickEnd),
    ("tick/LevelTickEvent$Pre", GameEvent::LevelTickStart),
    ("tick/LevelTickEvent$Post", GameEvent::LevelTickEnd),
    ("TickEvent$ServerTickEvent$Pre", GameEvent::ServerTickStart),
    ("TickEvent$ServerTickEvent$Post", GameEvent::ServerTickEnd),
    ("TickEvent$LevelTickEvent$Pre", GameEvent::LevelTickStart),
    ("TickEvent$LevelTickEvent$Post", GameEvent::LevelTickEnd),
    ("level/LevelEvent$Load", GameEvent::LevelLoad),
    ("level/LevelEvent$Unload", GameEvent::LevelUnload),
    (
        "entity/player/PlayerEvent$PlayerLoggedInEvent",
        GameEvent::PlayerJoin,
    ),
    (
        "entity/player/PlayerEvent$PlayerLoggedOutEvent",
        GameEvent::PlayerLeave,
    ),
    ("entity/EntityJoinLevelEvent", GameEvent::EntityLoad),
    (
        "entity/living/LivingDeathEvent",
        GameEvent::LivingAllowDeath,
    ),
    (
        "entity/player/PlayerInteractEvent$RightClickBlock",
        GameEvent::UseBlock,
    ),
    (
        "entity/player/PlayerInteractEvent$RightClickItem",
        GameEvent::UseItem,
    ),
    ("entity/player/AttackEntityEvent", GameEvent::AttackEntity),
    ("level/BlockEvent$BreakEvent", GameEvent::BlockBreakBefore),
    ("level/block/BreakBlockEvent", GameEvent::BlockBreakBefore),
];

const NEOFORGE_EVENT_ROOT: &str = "net/neoforged/neoforge/event/";
const FORGE_EVENT_ROOT: &str = "net/minecraftforge/event/";
const FABRIC_EVENT: &str = "net/fabricmc/fabric/api/event/Event";
/// Forge EventBus 7's per-event bus types (`ServerTickEvent.Post.BUS`).
const FORGE_BUSES: &[&str] = &[
    "net/minecraftforge/eventbus/api/bus/EventBus",
    "net/minecraftforge/eventbus/api/bus/CancellableEventBus",
];
const SUBSCRIBE_EVENT: &[&str] = &[
    "Lnet/neoforged/bus/api/SubscribeEvent;",
    "Lnet/minecraftforge/eventbus/api/SubscribeEvent;",
];
const EVENT_BUS_SUBSCRIBER: &[&str] = &[
    "Lnet/neoforged/fml/common/EventBusSubscriber;",
    "Lnet/neoforged/fml/common/Mod$EventBusSubscriber;",
];

/// The game event and loader an event class names, if the game fires it.
fn bus_event(class: &str) -> Option<(GameEvent, EventLoader)> {
    let (rest, loader) = match class.strip_prefix(NEOFORGE_EVENT_ROOT) {
        Some(rest) => (rest, EventLoader::NeoForge),
        None => (class.strip_prefix(FORGE_EVENT_ROOT)?, EventLoader::Forge),
    };
    BUS_EVENTS
        .iter()
        .find(|(path, _)| *path == rest)
        .map(|(_, event)| (*event, loader))
}

fn fabric_field(marker: &str) -> Option<&'static FabricField> {
    let (owner, field) = marker.strip_prefix("fabric-event/")?.rsplit_once('.')?;
    FABRIC_FIELDS
        .iter()
        .find(|row| row.owner == owner && row.field == field)
}

/// How a listener's callback is reached when its event fires.
#[derive(Clone, Debug, PartialEq)]
pub enum ListenerCallback {
    /// A lambda or method reference: the jar method it names, with the values
    /// it captured (the bound receiver first, for an instance target).
    Indy {
        target: MethodRef,
        captures: Vec<Value>,
    },
    /// An object of a jar class that implements the callback interface (or
    /// carries a `@SubscribeEvent` instance method).
    Object { receiver: Value, method: MethodRef },
    /// A static `@SubscribeEvent` method.
    Static { method: MethodRef },
}

/// One registered listener with its event identity.
#[derive(Clone, Debug, PartialEq)]
pub struct GameListener {
    pub event: GameEvent,
    pub loader: EventLoader,
    /// The Fabric field (`ServerTickEvents.END_SERVER_TICK`) or event class
    /// (`net/neoforged/neoforge/event/tick/ServerTickEvent$Post`) it was
    /// registered on, for diagnostics.
    pub source: String,
    pub callback: ListenerCallback,
    /// NeoForge/Forge `receiveCanceled`: it still runs after a cancellation.
    pub receives_cancelled: bool,
    shape: Option<Shape>,
    combine: Combine,
}

impl GameListener {
    /// The jar method this listener runs, for diagnostics.
    pub fn target(&self) -> &MethodRef {
        match &self.callback {
            ListenerCallback::Indy { target, .. } => target,
            ListenerCallback::Object { method, .. } | ListenerCallback::Static { method } => method,
        }
    }
}

/// What one jar's host registered: the listeners the game dispatches, and the
/// events (by field or class) it registered on that the game does not fire.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GameListeners {
    pub listeners: Vec<GameListener>,
    /// Source → listener count, for registrations the game never fires
    /// (`CommandRegistrationCallback.EVENT`, a client tick...).
    pub unsupported: BTreeMap<String, usize>,
}

impl GameListeners {
    /// Bitmask of the events with at least one listener ([`GameEvent::bit`]).
    pub fn mask(&self) -> u32 {
        self.listeners
            .iter()
            .fold(0, |mask, listener| mask | listener.event.bit())
    }
}

/// A player as the façade shows them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FacadePlayer {
    pub uuid: u128,
    pub name: String,
    /// Dimension id (`minecraft:overworld`).
    pub level: String,
    pub entity_id: i32,
}

/// A level as the façade shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FacadeLevel {
    /// Dimension id (`minecraft:overworld`).
    pub id: String,
    pub game_time: i64,
    pub day_time: i64,
}

/// A non-player entity as the façade shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FacadeEntity {
    pub id: i32,
    /// Entity type id (`minecraft:zombie`).
    pub kind: String,
    pub level: String,
}

/// The server state every firing exposes, snapshotted by the game.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventFacade {
    /// `MinecraftServer.getTickCount()`.
    pub tick_count: i32,
    pub dedicated: bool,
    pub max_players: i32,
    pub players: Vec<FacadePlayer>,
    pub levels: Vec<FacadeLevel>,
}

/// What this one firing is about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventContext {
    /// The level the event happens in (a level tick, an entity's level).
    pub level: Option<String>,
    /// The player the event is about.
    pub player: Option<u128>,
    /// The non-player entity the event is about (`player` covers players).
    pub entity: Option<FacadeEntity>,
    /// The damage type id of a death (`minecraft:player_attack`).
    pub damage: Option<String>,
    /// The damage amount of an `ALLOW_DEATH`.
    pub amount: f32,
}

/// One firing of one event, carried across every listener of every jar.
#[derive(Clone, Debug, PartialEq)]
pub struct EventFiring {
    pub event: GameEvent,
    pub facade: Arc<EventFacade>,
    pub context: EventContext,
    /// A listener cancelled the vanilla work (Fabric `false`/non-`PASS`,
    /// NeoForge/Forge `setCanceled(true)`).
    pub cancelled: bool,
    /// The first non-`PASS` `InteractionResult` a Fabric listener returned
    /// (`SUCCESS`, `FAIL`...).
    pub interaction: Option<String>,
}

impl EventFiring {
    pub fn new(event: GameEvent, facade: Arc<EventFacade>, context: EventContext) -> Self {
        Self {
            event,
            facade,
            context,
            cancelled: false,
            interaction: None,
        }
    }
}

/// What running one listener did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenerRun {
    /// The callback ran (it is skipped after a cancellation it does not receive).
    pub ran: bool,
    /// Later listeners must not run: a Fabric `boolean`/`InteractionResult`
    /// invoker stops at the first deciding answer.
    pub stop: bool,
}

const SERVER: &str = "game/server";
const PLAYER_LIST: &str = "game/player-list";

fn uuid_text(uuid: u128) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (uuid >> 96) as u32,
        (uuid >> 80) as u16,
        (uuid >> 64) as u16,
        (uuid >> 48) as u16,
        uuid & 0xffff_ffff_ffff
    )
}

fn marker(text: String) -> Value {
    Value::Ref(Some(text))
}

fn level_marker(id: &str) -> Value {
    marker(format!("game/level/{id}"))
}

fn player_marker(uuid: u128) -> Value {
    marker(format!("game/player/{uuid:032x}"))
}

fn dimension_key(id: &str) -> Value {
    marker(format!("resource-key/dimension/{id}"))
}

fn event_marker(event: GameEvent) -> Value {
    marker(format!("game-event/{}", event.name()))
}

/// The façade object one marker stands for.
enum Facade<'a> {
    Server,
    PlayerList,
    Level(&'a str),
    Player(u128),
    Connection(u128),
    Entity(i32),
    Event,
    /// Markers that only exist to be passed along (damage source, packet
    /// sender, hand, hit result, block position): every call on them refuses.
    Opaque,
}

fn facade(marker: &str) -> Option<Facade<'_>> {
    if marker == SERVER {
        return Some(Facade::Server);
    }
    if marker == PLAYER_LIST {
        return Some(Facade::PlayerList);
    }
    if marker.starts_with("game-event/") {
        return Some(Facade::Event);
    }
    let rest = marker.strip_prefix("game/")?;
    let (kind, id) = rest.split_once('/')?;
    let hex = || u128::from_str_radix(id, 16).ok();
    Some(match kind {
        "level" => Facade::Level(id),
        "player" => Facade::Player(hex()?),
        "connection" => Facade::Connection(hex()?),
        "entity" => Facade::Entity(id.parse().ok()?),
        _ => Facade::Opaque,
    })
}

impl StdHost {
    /// The statics a listener's registration and body read through the host:
    /// a Fabric `Event` field keeps its name (`fabric-event/<owner>.<FIELD>`),
    /// a Forge event's `BUS` names its event, and the façade answers
    /// `InteractionResult` and dimension constants plus the field read of a
    /// join handler's `player`.
    pub(crate) fn game_event_static(&self, r: &MethodRef) -> Option<Value> {
        if r.descriptor == "Lnet/fabricmc/fabric/api/event/Event;"
            && r.owner.starts_with("net/fabricmc/fabric/api/")
        {
            return Some(marker(format!("fabric-event/{}.{}", r.owner, r.name)));
        }
        if r.name == "BUS"
            && r.owner.starts_with(FORGE_EVENT_ROOT)
            && r.descriptor
                .starts_with("Lnet/minecraftforge/eventbus/api/bus/")
        {
            return Some(marker(format!("forge-bus/{}", r.owner)));
        }
        match (r.owner.as_str(), r.name.as_str()) {
            ("net/minecraft/world/InteractionResult", name)
                if r.descriptor
                    .starts_with("Lnet/minecraft/world/InteractionResult") =>
            {
                Some(marker(format!("interaction-result/{name}")))
            }
            ("net/minecraft/world/level/Level", "OVERWORLD") => {
                Some(dimension_key("minecraft:overworld"))
            }
            ("net/minecraft/world/level/Level", "NETHER") => {
                Some(dimension_key("minecraft:the_nether"))
            }
            ("net/minecraft/world/level/Level", "END") => Some(dimension_key("minecraft:the_end")),
            // `handler.player`: an instance field of a host object reaches the
            // host as a static read without its receiver, and only the join
            // and disconnect firings hand a handler out.
            ("net/minecraft/server/network/ServerGamePacketListenerImpl", "player") => self
                .game_event
                .as_ref()
                .and_then(|firing| firing.context.player)
                .map(player_marker),
            _ => None,
        }
    }

    /// Answer one instance call whose receiver is a façade marker, or `None`
    /// when the receiver is not one (the host's usual surface answers it).
    /// A façade call the surface does not model is an error, so the caller
    /// can refuse the listener instead of running it on invented state.
    pub(crate) fn game_event_call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let text = expect_ref(receiver)?;
        if let Some(name) = text.strip_prefix("interaction-result/") {
            // `consumesAction` is the one query mods make of a result.
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("consumesAction", "()Z") => Some(Ok(Value::Int(i32::from(matches!(
                    name,
                    "SUCCESS" | "SUCCESS_SERVER" | "CONSUME"
                ))))),
                _ => None,
            };
        }
        let kind = facade(text)?;
        if r.owner == "java/lang/Object" || r.owner == "java/lang/Record" {
            return match (r.name.as_str(), r.descriptor.as_str()) {
                ("toString", "()Ljava/lang/String;") => Some(Ok(marker(text.clone()))),
                ("hashCode", "()I") => Some(Ok(Value::Int(stable_hash(text)))),
                ("equals", "(Ljava/lang/Object;)Z") => {
                    Some(Ok(Value::Int(i32::from(args.first() == Some(receiver)))))
                }
                _ => None,
            };
        }
        let answer = self.facade_answer(&kind, r, args);
        Some(answer.ok_or_else(|| NativeError::UnknownHostCall(r.clone())))
    }

    fn facade_answer(&mut self, kind: &Facade<'_>, r: &MethodRef, args: &[Value]) -> Option<Value> {
        let firing = self.game_event.as_deref()?;
        let facade = Arc::clone(&firing.facade);
        let context = firing.context.clone();
        let (event, cancelled) = (firing.event, firing.cancelled);
        let returns = return_desc(&r.descriptor);
        let name = r.name.as_str();
        let int = |value: i32| Some(Value::Int(value));
        let flag = |value: bool| Some(Value::Int(i32::from(value)));
        let player = |uuid: u128| facade.players.iter().find(|p| p.uuid == uuid);
        match kind {
            Facade::Server => match (name, returns) {
                ("getTickCount", "I") => int(facade.tick_count),
                ("isDedicatedServer", "Z") => flag(facade.dedicated),
                ("isSingleplayer", "Z") => flag(!facade.dedicated),
                ("isRunning" | "isSameThread", "Z") => flag(true),
                ("getPlayerCount", "I") => int(facade.players.len() as i32),
                ("getMaxPlayers", "I") => int(facade.max_players),
                ("getServerVersion", "Ljava/lang/String;") => Some(marker("26.3".into())),
                ("getPlayerList", _) => Some(marker(PLAYER_LIST.into())),
                ("overworld", _) => Some(level_marker("minecraft:overworld")),
                ("getAllLevels", _) => {
                    let levels = facade.levels.iter().map(|l| level_marker(&l.id)).collect();
                    Some(self.facade_list("levels", levels))
                }
                _ => None,
            },
            Facade::PlayerList => match (name, returns) {
                ("getPlayers", _) => {
                    let players = facade
                        .players
                        .iter()
                        .map(|p| player_marker(p.uuid))
                        .collect();
                    Some(self.facade_list("players", players))
                }
                ("getPlayerCount", "I") => int(facade.players.len() as i32),
                ("getMaxPlayers", "I") => int(facade.max_players),
                ("getServer", _) => Some(marker(SERVER.into())),
                ("getPlayer", _) if r.descriptor.starts_with("(Ljava/util/UUID;)") => {
                    let wanted = args.first().and_then(expect_ref)?.strip_prefix("uuid/")?;
                    Some(
                        facade
                            .players
                            .iter()
                            .find(|p| uuid_text(p.uuid) == wanted)
                            .map_or(Value::Ref(None), |p| player_marker(p.uuid)),
                    )
                }
                ("getPlayerByName", _) => {
                    let wanted = args.first().and_then(expect_ref)?;
                    Some(
                        facade
                            .players
                            .iter()
                            .find(|p| p.name.eq_ignore_ascii_case(wanted))
                            .map_or(Value::Ref(None), |p| player_marker(p.uuid)),
                    )
                }
                _ => None,
            },
            Facade::Level(id) => {
                let level = facade.levels.iter().find(|l| l.id == *id);
                match (name, returns) {
                    ("getServer", _) => Some(marker(SERVER.into())),
                    ("getGameTime", "J") => Some(Value::Long(level?.game_time)),
                    ("getDayTime", "J") => Some(Value::Long(level?.day_time)),
                    ("isClientSide", "Z") => flag(false),
                    ("dimension", _) => Some(dimension_key(id)),
                    ("players", _) => {
                        let players = facade
                            .players
                            .iter()
                            .filter(|p| p.level == *id)
                            .map(|p| player_marker(p.uuid))
                            .collect();
                        Some(self.facade_list("level-players", players))
                    }
                    _ => None,
                }
            }
            Facade::Player(uuid) => {
                let p = player(*uuid)?;
                match (name, returns) {
                    ("getUUID", _) => uuid_value(&uuid_text(*uuid)).ok(),
                    ("getStringUUID", _) => Some(marker(uuid_text(*uuid))),
                    ("getScoreboardName" | "getPlainTextName", _) => Some(marker(p.name.clone())),
                    ("getName" | "getDisplayName", _) => Some(component_value("literal", &p.name)),
                    ("getId", "I") => int(p.entity_id),
                    ("isAlive", "Z") => flag(true),
                    ("level" | "serverLevel", _) => Some(level_marker(&p.level)),
                    ("getServer", _) => Some(marker(SERVER.into())),
                    _ => None,
                }
            }
            Facade::Connection(uuid) => match (name, returns) {
                ("getPlayer", _) => Some(player_marker(*uuid)),
                _ => None,
            },
            Facade::Entity(id) => {
                let entity = context.entity.as_ref().filter(|e| e.id == *id)?;
                match (name, returns) {
                    ("getId", "I") => int(entity.id),
                    ("isAlive", "Z") => flag(true),
                    ("level", _) => Some(level_marker(&entity.level)),
                    _ => None,
                }
            }
            Facade::Event => match (name, returns) {
                ("getServer" | "server", _) => Some(marker(SERVER.into())),
                ("getLevel" | "level", _) => context.level.as_deref().map(level_marker),
                ("getEntity" | "getPlayer" | "player" | "entity", _) => context_subject(&context),
                ("getSource" | "source", _) => context
                    .damage
                    .as_ref()
                    .map(|damage| marker(format!("game/damage/{damage}"))),
                ("hasTime", "Z") => flag(true),
                ("isCanceled", "Z") => flag(cancelled),
                ("setCanceled", "V") => {
                    let cancel = matches!(args.first(), Some(Value::Int(v)) if *v != 0);
                    if !event.cancellable() {
                        // NeoForge throws for a non-cancellable event; refuse.
                        return None;
                    }
                    if let Some(firing) = self.game_event.as_deref_mut() {
                        firing.cancelled = cancel;
                    }
                    Some(Value::Void)
                }
                _ => None,
            },
            Facade::Opaque => None,
        }
    }

    /// A fresh list marker holding `values`, through the host's own
    /// collection table so `size`/`get`/`iterator` work on it.
    fn facade_list(&mut self, what: &str, values: Vec<Value>) -> Value {
        let marker = format!("java/util/ArrayList#game-{what}");
        self.mutable_collections.insert(marker.clone(), values);
        Value::Ref(Some(marker))
    }

    /// Every listener this host recorded, with its event identity, plus the
    /// static `@EventBusSubscriber` methods `jar` declares. `server` drops
    /// subscribers marked `Dist.CLIENT`.
    pub fn game_listeners(&self, jar: &NativeJar, server: bool) -> GameListeners {
        let mut out = GameListeners::default();
        for invocation in &self.invocations {
            listeners_from(invocation, jar, &mut out);
        }
        subscriber_classes(jar, server, &mut out);
        out
    }

    /// How many host calls the log holds, to roll a listener's calls back
    /// once it ran: a listener fired every tick must not grow the log for the
    /// life of the server.
    fn call_log_marks(&self) -> (usize, usize, usize) {
        (
            self.calls.len(),
            self.invocations.len(),
            self.posted_events.len(),
        )
    }

    fn truncate_call_log(&mut self, (calls, invocations, posted): (usize, usize, usize)) {
        self.calls.truncate(calls);
        self.invocations.truncate(invocations);
        self.posted_events.truncate(posted);
    }
}

/// The player or entity marker a firing is about.
fn context_subject(context: &EventContext) -> Option<Value> {
    if let Some(uuid) = context.player {
        return Some(player_marker(uuid));
    }
    context
        .entity
        .as_ref()
        .map(|entity| marker(format!("game/entity/{}", entity.id)))
}

/// The last `@indy:` argument of a registration call: its listener.
fn indy_argument(args: &[Value]) -> Option<&str> {
    args.iter().rev().find_map(|value| match value {
        Value::Ref(Some(text)) if text.starts_with("@indy:") => Some(text.as_str()),
        _ => None,
    })
}

/// An object argument whose class the jar declares: `cls#N`.
fn jar_object<'a>(jar: &NativeJar, value: &'a Value) -> Option<(&'a str, &'a Value)> {
    let text = expect_ref(value)?;
    let (class, _) = text.split_once('#')?;
    jar.classes.contains_key(class).then_some((class, value))
}

/// A method `name`+`descriptor` declared by `class` or a jar superclass.
fn resolve_method(jar: &NativeJar, class: &str, name: &str, descriptor: &str) -> Option<MethodRef> {
    let mut current = class.to_owned();
    for _ in 0..32 {
        let file = jar.classes.get(&current)?;
        if file.method(name, descriptor).is_some() {
            return Some(MethodRef {
                owner: current,
                name: name.into(),
                descriptor: descriptor.into(),
            });
        }
        current = file.super_name.clone();
    }
    None
}

/// The single event-class parameter of a listener method descriptor, after
/// `skip` captured values.
fn event_parameter(descriptor: &str, skip: usize) -> Option<String> {
    let arguments = crate::descriptor_arguments(descriptor);
    let rest = arguments.get(skip..)?;
    match rest {
        [only] => only
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
            .map(str::to_owned),
        _ => None,
    }
}

fn count_unsupported(out: &mut GameListeners, source: String) {
    // Bounded: a jar naming thousands of distinct events keeps the first 64.
    if out.unsupported.len() < 64 || out.unsupported.contains_key(&source) {
        *out.unsupported.entry(source).or_default() += 1;
    }
}

/// Turn one recorded host invocation into listeners, if it registered any.
fn listeners_from(invocation: &HostInvocation, jar: &NativeJar, out: &mut GameListeners) {
    let owner = invocation.method.owner.as_str();
    let name = invocation.method.name.as_str();
    let receiver = invocation.receiver.as_ref().and_then(expect_ref);
    // Fabric: `<Field>.register([phase,] listener)` on a named Event field.
    if owner == FABRIC_EVENT && name == "register" {
        let Some(receiver) = receiver else {
            return;
        };
        let Some(source) = receiver.strip_prefix("fabric-event/") else {
            return;
        };
        let Some(row) = fabric_field(receiver) else {
            count_unsupported(out, source.to_owned());
            return;
        };
        let source = format!(
            "{}.{}",
            row.owner.rsplit('/').next().unwrap_or(row.owner),
            row.field
        );
        let callback = if let Some(indy) = indy_argument(&invocation.args) {
            match parse_callback_marker(indy) {
                Ok(marker) => ListenerCallback::Indy {
                    target: marker.target,
                    captures: marker.captures,
                },
                Err(_) => return count_unsupported(out, source),
            }
        } else if let Some((class, value)) = invocation.args.last().and_then(|v| jar_object(jar, v))
        {
            match resolve_method(jar, class, row.method, row.descriptor) {
                Some(method) => ListenerCallback::Object {
                    receiver: value.clone(),
                    method,
                },
                None => return count_unsupported(out, source),
            }
        } else {
            return count_unsupported(out, source);
        };
        let combine = match return_desc(row.descriptor) {
            "Z" => Combine::Allow,
            "V" => Combine::Void,
            _ => Combine::Interaction,
        };
        out.listeners.push(GameListener {
            event: row.event,
            loader: EventLoader::Fabric,
            source,
            callback,
            receives_cancelled: false,
            shape: Some(row.shape),
            combine,
        });
        return;
    }
    // Forge EventBus 7: `<Event>.BUS.addListener([priority,] [alwaysCancelling,] listener)`.
    if FORGE_BUSES.contains(&owner) && name == "addListener" {
        let Some(class) = receiver.and_then(|r| r.strip_prefix("forge-bus/")) else {
            return;
        };
        push_bus_listener(class, invocation, jar, out, false);
        return;
    }
    if !crate::is_event_bus_owner(owner) {
        return;
    }
    match name {
        // `addListener([priority, receiveCanceled,] [EventClass,] Consumer)`.
        "addListener" => {
            let class = invocation.args.iter().find_map(|value| {
                expect_ref(value)
                    .filter(|text| bus_event(text).is_some())
                    .cloned()
            });
            // The `boolean receiveCanceled` parameter, where the overload has one.
            let receives = crate::descriptor_arguments(&invocation.method.descriptor)
                .iter()
                .position(|argument| argument == "Z")
                .and_then(|at| invocation.args.get(at))
                == Some(&Value::Int(1));
            // Without a class argument the event is the `Consumer`'s one
            // parameter: the target's last (a bound receiver or captured
            // values come before it).
            let class = class.or_else(|| {
                let marker = parse_callback_marker(indy_argument(&invocation.args)?).ok()?;
                let arguments = crate::descriptor_arguments(&marker.target.descriptor);
                let last = arguments.last()?;
                Some(last.strip_prefix('L')?.strip_suffix(';')?.to_owned())
            });
            match class {
                Some(class) => push_bus_listener(&class, invocation, jar, out, receives),
                None => count_unsupported(out, "IEventBus.addListener".into()),
            }
        }
        // `register(Object)`: a class literal's static, or an object's
        // instance, `@SubscribeEvent` methods.
        "register" => {
            let Some(value) = invocation.args.first() else {
                return;
            };
            if let Some((class, object)) = jar_object(jar, value) {
                subscribe_methods(jar, class, Some(object), out);
            } else if let Some(class) = expect_ref(value).filter(|c| jar.classes.contains_key(*c)) {
                subscribe_methods(jar, class, None, out);
            }
        }
        _ => {}
    }
}

/// A NeoForge/Forge listener on `class` whose callback is the registration's
/// lambda or method reference.
fn push_bus_listener(
    class: &str,
    invocation: &HostInvocation,
    jar: &NativeJar,
    out: &mut GameListeners,
    receives_cancelled: bool,
) {
    let Some((event, loader)) = bus_event(class) else {
        count_unsupported(out, class.to_owned());
        return;
    };
    let callback = match indy_argument(&invocation.args).map(parse_callback_marker) {
        Some(Ok(marker)) => ListenerCallback::Indy {
            target: marker.target,
            captures: marker.captures,
        },
        _ => match invocation.args.last().and_then(|v| jar_object(jar, v)) {
            // A `Consumer` object of a jar class.
            Some((owner, value)) => {
                match resolve_method(jar, owner, "accept", "(Ljava/lang/Object;)V") {
                    Some(method) => ListenerCallback::Object {
                        receiver: value.clone(),
                        method,
                    },
                    None => return count_unsupported(out, class.to_owned()),
                }
            }
            None => return count_unsupported(out, class.to_owned()),
        },
    };
    out.listeners.push(GameListener {
        event,
        loader,
        source: class.to_owned(),
        callback,
        receives_cancelled,
        shape: None,
        combine: Combine::Void,
    });
}

/// The `@SubscribeEvent` methods of `class`: static ones without a receiver,
/// instance ones on `object`.
fn subscribe_methods(
    jar: &NativeJar,
    class: &str,
    object: Option<&Value>,
    out: &mut GameListeners,
) {
    let Some(file) = jar.classes.get(class) else {
        return;
    };
    for method in &file.methods {
        let Some(annotation) = method
            .annotations
            .iter()
            .find(|a| SUBSCRIBE_EVENT.contains(&a.descriptor.as_str()))
        else {
            continue;
        };
        // `register(Class)` takes the static methods, `register(object)` the
        // instance ones, as NeoForge's bus does.
        if method.is_static() == object.is_some() {
            continue;
        }
        let Some(event_class) = event_parameter(&method.descriptor, 0) else {
            continue;
        };
        let Some((event, loader)) = bus_event(&event_class) else {
            count_unsupported(out, event_class);
            continue;
        };
        let receives_cancelled = matches!(
            annotation.values.get("receiveCanceled"),
            Some(crate::AnnotationValue::Bool(true))
        );
        let target = MethodRef {
            owner: class.to_owned(),
            name: method.name.clone(),
            descriptor: method.descriptor.clone(),
        };
        out.listeners.push(GameListener {
            event,
            loader,
            source: event_class,
            callback: match object {
                Some(receiver) => ListenerCallback::Object {
                    receiver: receiver.clone(),
                    method: target,
                },
                None => ListenerCallback::Static { method: target },
            },
            receives_cancelled,
            shape: None,
            combine: Combine::Void,
        });
    }
}

/// NeoForge's automatic subscribers: every `@EventBusSubscriber` class's
/// static `@SubscribeEvent` methods, without a `register` call.
fn subscriber_classes(jar: &NativeJar, server: bool, out: &mut GameListeners) {
    for (name, file) in &jar.classes {
        let Some(annotation) = file
            .annotations
            .iter()
            .find(|a| EVENT_BUS_SUBSCRIBER.contains(&a.descriptor.as_str()))
        else {
            continue;
        };
        if server && subscriber_client_only(annotation) {
            continue;
        }
        subscribe_methods(jar, name, None, out);
    }
}

/// `@EventBusSubscriber(value = Dist.CLIENT)`: the class only loads on a client.
fn subscriber_client_only(annotation: &crate::Annotation) -> bool {
    fn names(value: &crate::AnnotationValue, out: &mut Vec<String>) {
        match value {
            crate::AnnotationValue::String(text) => out.push(text.clone()),
            crate::AnnotationValue::Array(values) => {
                values.iter().for_each(|value| names(value, out))
            }
            _ => {}
        }
    }
    let mut dists = Vec::new();
    if let Some(value) = annotation.values.get("value") {
        names(value, &mut dists);
    }
    !dists.is_empty() && dists.iter().all(|dist| dist.ends_with(".CLIENT"))
}

/// The Fabric argument list of `shape` for this firing.
fn fabric_arguments(shape: Shape, firing: &EventFiring) -> Result<Vec<Value>, NativeError> {
    let context = &firing.context;
    let missing = |what: &str| {
        NativeError::Metadata(format!("{} fired without its {what}", firing.event.name()))
    };
    let server = || marker(SERVER.into());
    let level = || {
        context
            .level
            .as_deref()
            .map(level_marker)
            .ok_or_else(|| missing("level"))
    };
    let player = || context.player.ok_or_else(|| missing("player"));
    let subject = || context_subject(context).ok_or_else(|| missing("entity"));
    let damage = || {
        marker(format!(
            "game/damage/{}",
            context.damage.as_deref().unwrap_or("minecraft:generic")
        ))
    };
    let hand = || marker("enum/net/minecraft/world/InteractionHand/MAIN_HAND".into());
    let opaque = |what: &str| marker(format!("game/{what}/0"));
    Ok(match shape {
        Shape::Server => vec![server()],
        Shape::Level => vec![level()?],
        Shape::ServerLevel => vec![server(), level()?],
        Shape::Join => {
            let uuid = player()?;
            vec![
                marker(format!("game/connection/{uuid:032x}")),
                marker(format!("game/packet-sender/{uuid:032x}")),
                server(),
            ]
        }
        Shape::Disconnect => vec![
            marker(format!("game/connection/{:032x}", player()?)),
            server(),
        ],
        Shape::Player => vec![player_marker(player()?)],
        Shape::EntityLevel => vec![subject()?, level()?],
        Shape::EntityDamageAmount => vec![
            subject()?,
            damage(),
            Value::Float(f64::from(context.amount)),
        ],
        Shape::EntityDamage => vec![subject()?, damage()],
        Shape::BlockBreak => vec![
            level()?,
            player_marker(player()?),
            opaque("block-pos"),
            opaque("block-state"),
            Value::Ref(None),
        ],
        Shape::UseBlock => vec![
            player_marker(player()?),
            level()?,
            hand(),
            opaque("block-hit"),
        ],
        Shape::UseItem => vec![player_marker(player()?), level()?, hand()],
        Shape::AttackEntity => vec![
            player_marker(player()?),
            level()?,
            hand(),
            subject()?,
            opaque("entity-hit"),
        ],
    })
}

impl NativeJar {
    /// Run one listener this jar registered for `firing`, through `host` (the
    /// host its entrypoints ran on, so its statics are the jar's).
    ///
    /// `firing` carries the cancellation and result from earlier listeners and
    /// receives this one's. The host's call log is rolled back afterwards. An
    /// error (a façade call the surface does not model, the budget running
    /// out, a bytecode the interpreter refuses) is returned for the caller to
    /// disable the listener; what the callback wrote to its statics before
    /// failing is kept, as a JVM would keep it.
    pub fn run_game_listener(
        &self,
        listener: &GameListener,
        firing: &mut EventFiring,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<ListenerRun, NativeError> {
        if firing.cancelled
            && listener.loader != EventLoader::Fabric
            && !listener.receives_cancelled
        {
            return Ok(ListenerRun::default());
        }
        let event_args = match listener.shape {
            Some(shape) => fabric_arguments(shape, firing)?,
            None => vec![event_marker(firing.event)],
        };
        let (target, receiver, args) = match &listener.callback {
            ListenerCallback::Indy { target, captures } => {
                let is_static = self
                    .classes
                    .get(&target.owner)
                    .and_then(|class| class.method(&target.name, &target.descriptor))
                    .map(|method| method.is_static())
                    .ok_or_else(|| {
                        NativeError::MissingMethod(format!(
                            "{}#{}{}",
                            target.owner, target.name, target.descriptor
                        ))
                    })?;
                let mut captures = captures.clone();
                let receiver = if is_static {
                    None
                } else if captures.is_empty() {
                    return Err(NativeError::Metadata(
                        "instance listener has no captured receiver".into(),
                    ));
                } else {
                    Some(captures.remove(0))
                };
                captures.extend(event_args);
                (target, receiver, captures)
            }
            ListenerCallback::Object { receiver, method } => {
                (method, Some(receiver.clone()), event_args)
            }
            ListenerCallback::Static { method } => (method, None, event_args),
        };
        let expected = arg_count(&target.descriptor);
        if args.len() != expected {
            return Err(NativeError::Metadata(format!(
                "listener {}#{}{} takes {expected} arguments, the event passes {}",
                target.owner,
                target.name,
                target.descriptor,
                args.len()
            )));
        }
        let marks = host.call_log_marks();
        host.game_event = Some(Box::new(firing.clone()));
        let result = self.invoke_persistent(target, receiver, &args, host, budget);
        if let Some(after) = host.game_event.take() {
            firing.cancelled = after.cancelled;
        }
        host.truncate_call_log(marks);
        let result = result?;
        let mut run = ListenerRun {
            ran: true,
            stop: false,
        };
        match (listener.loader, listener.combine) {
            (EventLoader::Fabric, Combine::Allow) => {
                if result == Value::Int(0) {
                    firing.cancelled = true;
                    run.stop = true;
                }
            }
            (EventLoader::Fabric, Combine::Interaction) => {
                let name = expect_ref(&result)
                    .and_then(|text| text.strip_prefix("interaction-result/"))
                    .unwrap_or("PASS");
                if name != "PASS" {
                    firing.interaction = Some(name.to_owned());
                    firing.cancelled = true;
                    run.stop = true;
                }
            }
            // A Forge `Predicate` listener answers whether it cancels.
            (EventLoader::Forge, _)
                if firing.event.cancellable()
                    && return_desc(&target.descriptor) == "Z"
                    && result == Value::Int(1) =>
            {
                firing.cancelled = true;
            }
            _ => {}
        }
        Ok(run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_text_is_the_canonical_java_form() {
        // java.util.UUID(0x0123456789abcdef, 0xfedcba9876543210).toString()
        assert_eq!(
            uuid_text(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210),
            "01234567-89ab-cdef-fedc-ba9876543210"
        );
    }

    #[test]
    fn bus_events_name_both_loaders() {
        assert_eq!(
            bus_event("net/neoforged/neoforge/event/tick/ServerTickEvent$Post"),
            Some((GameEvent::ServerTickEnd, EventLoader::NeoForge))
        );
        assert_eq!(
            bus_event("net/minecraftforge/event/TickEvent$ServerTickEvent$Pre"),
            Some((GameEvent::ServerTickStart, EventLoader::Forge))
        );
        assert_eq!(
            bus_event("net/neoforged/neoforge/event/RegisterCommandsEvent"),
            None
        );
    }

    #[test]
    fn event_bits_are_distinct() {
        let mask = GameEvent::ALL.iter().fold(0u32, |mask, event| {
            assert_eq!(mask & event.bit(), 0, "{event:?}");
            mask | event.bit()
        });
        assert_eq!(mask.count_ones() as usize, GameEvent::ALL.len());
    }
}
