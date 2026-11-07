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
//! # Order
//!
//! Listeners run in the loaders' own order ([`order`]): Fabric phases
//! (`register(phase, listener)`, `addPhaseOrdering`), NeoForge/Forge
//! `EventPriority`, then registration order, across every jar as one event.

pub mod order;

use std::{collections::BTreeMap, sync::Arc};

pub use order::{ListenerOrder, PhaseOrdering, dispatch_order};

use crate::{
    ExecutionBudget, HostInvocation, MethodRef, NativeError, NativeJar, StdHost, Value, arg_count,
    component_value, expect_ref, facade_server, parse_callback_marker, return_desc, stable_hash,
    uuid_value,
};

pub use crate::facade_server::{GameRuleValue, ServerFacts};

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
    /// A player starts digging (left-clicks) a block; result-returning
    /// (Fabric `AttackBlockCallback`, NeoForge `PlayerInteractEvent.LeftClickBlock`).
    AttackBlock,
    /// A player uses (right-clicks) an entity; result-returning (Fabric
    /// `UseEntityCallback`, NeoForge `PlayerInteractEvent.EntityInteract`).
    UseEntity,
    /// A [`GameEvent::BlockBreakBefore`] listener cancelled the break
    /// (Fabric `PlayerBlockBreakEvents.CANCELED`).
    BlockBreakCanceled,
    /// An entity left a level (Fabric `ENTITY_UNLOAD`, NeoForge
    /// `EntityLeaveLevelEvent`).
    EntityUnload,
    /// A living entity is about to be hurt; cancellable (Fabric
    /// `ALLOW_DAMAGE`, NeoForge `LivingIncomingDamageEvent`).
    LivingAllowDamage,
    /// A living entity was hurt (Fabric `AFTER_DAMAGE`, NeoForge
    /// `LivingDamageEvent.Post`).
    LivingAfterDamage,
    /// A player respawned or came back from the End (Fabric `AFTER_RESPAWN`,
    /// NeoForge `PlayerRespawnEvent`).
    PlayerRespawn,
    /// The respawned player copies what it keeps from the old one (Fabric
    /// `COPY_FROM`, NeoForge `PlayerEvent.Clone`).
    PlayerCopy,
    /// A player moved to another dimension (Fabric
    /// `AFTER_PLAYER_CHANGE_LEVEL`, NeoForge `PlayerChangedDimensionEvent`).
    PlayerChangeLevel,
    /// The server builds its command tree (Fabric `CommandRegistrationCallback`,
    /// NeoForge/Forge `RegisterCommandsEvent`): the listener registers on the
    /// Brigadier façade (`brigadier.rs`).
    RegisterCommands,
    // Client events: the window's own points fire them, never a server
    // (`client_surface` holds the façade they see).
    /// The client finished starting, before its first tick (Fabric
    /// `CLIENT_STARTED`).
    ClientStarted,
    /// The client is about to stop (Fabric `CLIENT_STOPPING`).
    ClientStopping,
    /// Each client tick's start (Fabric `START_CLIENT_TICK`, NeoForge
    /// `ClientTickEvent.Pre`), in a world or not.
    ClientTickStart,
    /// Each client tick's end (Fabric `END_CLIENT_TICK`, NeoForge
    /// `ClientTickEvent.Post`).
    ClientTickEnd,
    /// The client level's tick starts (Fabric `ClientTickEvents.START_LEVEL_TICK`).
    ClientLevelTickStart,
    /// The client level's tick ends.
    ClientLevelTickEnd,
    /// The client joined a world and play is ready (Fabric
    /// `ClientPlayConnectionEvents.JOIN`, NeoForge `ClientPlayerNetworkEvent.LoggingIn`).
    ClientJoin,
    /// The client left its world (Fabric `ClientPlayConnectionEvents.DISCONNECT`,
    /// NeoForge `ClientPlayerNetworkEvent.LoggingOut`).
    ClientDisconnect,
    /// NeoForge's mod-bus `RegisterKeyMappingsEvent`, fired once after the
    /// client entrypoints ran.
    ClientRegisterKeyMappings,
    /// NeoForge's mod-bus `RegisterGuiLayersEvent`, fired once after the
    /// client entrypoints ran.
    ClientRegisterGuiLayers,
}

impl GameEvent {
    /// Every event, in declaration order (the bit order of [`Self::bit`]).
    pub const ALL: [GameEvent; 40] = [
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
        GameEvent::AttackBlock,
        GameEvent::UseEntity,
        GameEvent::BlockBreakCanceled,
        GameEvent::EntityUnload,
        GameEvent::LivingAllowDamage,
        GameEvent::LivingAfterDamage,
        GameEvent::PlayerRespawn,
        GameEvent::PlayerCopy,
        GameEvent::PlayerChangeLevel,
        GameEvent::RegisterCommands,
        GameEvent::ClientStarted,
        GameEvent::ClientStopping,
        GameEvent::ClientTickStart,
        GameEvent::ClientTickEnd,
        GameEvent::ClientLevelTickStart,
        GameEvent::ClientLevelTickEnd,
        GameEvent::ClientJoin,
        GameEvent::ClientDisconnect,
        GameEvent::ClientRegisterKeyMappings,
        GameEvent::ClientRegisterGuiLayers,
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
            GameEvent::AttackBlock => "attack_block",
            GameEvent::UseEntity => "use_entity",
            GameEvent::BlockBreakCanceled => "block_break_canceled",
            GameEvent::EntityUnload => "entity_unload",
            GameEvent::LivingAllowDamage => "living_allow_damage",
            GameEvent::LivingAfterDamage => "living_after_damage",
            GameEvent::PlayerRespawn => "player_respawn",
            GameEvent::PlayerCopy => "player_copy",
            GameEvent::PlayerChangeLevel => "player_change_level",
            GameEvent::RegisterCommands => "register_commands",
            GameEvent::ClientStarted => "client_started",
            GameEvent::ClientStopping => "client_stopping",
            GameEvent::ClientTickStart => "client_tick_start",
            GameEvent::ClientTickEnd => "client_tick_end",
            GameEvent::ClientLevelTickStart => "client_level_tick_start",
            GameEvent::ClientLevelTickEnd => "client_level_tick_end",
            GameEvent::ClientJoin => "client_join",
            GameEvent::ClientDisconnect => "client_disconnect",
            GameEvent::ClientRegisterKeyMappings => "client_register_key_mappings",
            GameEvent::ClientRegisterGuiLayers => "client_register_gui_layers",
        }
    }

    /// One bit per event, for a caller's "does anything listen" mask (the
    /// top bits stay free for a caller's own flags).
    pub fn bit(self) -> u64 {
        1 << (self as u64)
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
                | GameEvent::AttackBlock
                | GameEvent::UseEntity
                | GameEvent::LivingAllowDamage
                // NeoForge's `EntityJoinLevelEvent` keeps the entity out.
                | GameEvent::EntityLoad
        )
    }

    /// Whether only a client (the window) fires it; a server keeps none of
    /// these listeners.
    pub fn is_client(self) -> bool {
        matches!(
            self,
            GameEvent::ClientStarted
                | GameEvent::ClientStopping
                | GameEvent::ClientTickStart
                | GameEvent::ClientTickEnd
                | GameEvent::ClientLevelTickStart
                | GameEvent::ClientLevelTickEnd
                | GameEvent::ClientJoin
                | GameEvent::ClientDisconnect
                | GameEvent::ClientRegisterKeyMappings
                | GameEvent::ClientRegisterGuiLayers
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
    /// `(Player, Level, InteractionHand, Entity, EntityHitResult)`: attacking
    /// or using an entity.
    AttackEntity,
    /// `(Player, Level, InteractionHand, BlockPos, Direction)`
    AttackBlock,
    /// `(LivingEntity, DamageSource, float baseTaken, float taken, boolean blocked)`
    AfterDamage,
    /// `(ServerPlayer oldPlayer, ServerPlayer newPlayer, boolean alive)`
    PlayerPair,
    /// `(ServerPlayer, ServerLevel origin, ServerLevel destination)`
    ChangeLevel,
    /// `(CommandDispatcher, CommandBuildContext, Commands.CommandSelection)`
    Commands,
    /// `(Minecraft)`
    Minecraft,
    /// `(ClientLevel)`
    ClientLevel,
    /// `(ClientPacketListener, PacketSender, Minecraft)`
    ClientJoin,
    /// `(ClientPacketListener, Minecraft)`
    ClientDisconnect,
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

/// A class of Fabric API's client lifecycle-events package.
macro_rules! client_lifecycle {
    ($class:literal) => {
        concat!("net/fabricmc/fabric/api/client/event/lifecycle/v1/", $class)
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
    // The rest of the server's player and entity events (Fabric API 26.3's
    // `fabric-events-interaction-v0`, `fabric-entity-events-v1`,
    // `fabric-lifecycle-events-v1`), read with `javap`.
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/AttackBlockCallback",
        field: "EVENT",
        method: "interact",
        descriptor: "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/core/BlockPos;Lnet/minecraft/core/Direction;)Lnet/minecraft/world/InteractionResult;",
        event: GameEvent::AttackBlock,
        shape: Shape::AttackBlock,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/UseEntityCallback",
        field: "EVENT",
        method: "interact",
        descriptor: "(Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/level/Level;Lnet/minecraft/world/InteractionHand;Lnet/minecraft/world/entity/Entity;Lnet/minecraft/world/phys/EntityHitResult;)Lnet/minecraft/world/InteractionResult;",
        event: GameEvent::UseEntity,
        shape: Shape::AttackEntity,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/event/player/PlayerBlockBreakEvents",
        field: "CANCELED",
        method: "onBlockBreakCanceled",
        descriptor: "(Lnet/minecraft/world/level/Level;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;)V",
        event: GameEvent::BlockBreakCanceled,
        shape: Shape::BlockBreak,
    },
    fabric(
        lifecycle!("ServerEntityEvents"),
        "ENTITY_UNLOAD",
        "onUnload",
        "(Lnet/minecraft/world/entity/Entity;Lnet/minecraft/server/level/ServerLevel;)V",
        GameEvent::EntityUnload,
        Shape::EntityLevel,
    ),
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerLivingEntityEvents",
        field: "ALLOW_DAMAGE",
        method: "allowDamage",
        descriptor: "(Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/damagesource/DamageSource;F)Z",
        event: GameEvent::LivingAllowDamage,
        shape: Shape::EntityDamageAmount,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerLivingEntityEvents",
        field: "AFTER_DAMAGE",
        method: "afterDamage",
        descriptor: "(Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/damagesource/DamageSource;FFZ)V",
        event: GameEvent::LivingAfterDamage,
        shape: Shape::AfterDamage,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerPlayerEvents",
        field: "AFTER_RESPAWN",
        method: "afterRespawn",
        descriptor: "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/server/level/ServerPlayer;Z)V",
        event: GameEvent::PlayerRespawn,
        shape: Shape::PlayerPair,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerPlayerEvents",
        field: "COPY_FROM",
        method: "copyFromPlayer",
        descriptor: "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/server/level/ServerPlayer;Z)V",
        event: GameEvent::PlayerCopy,
        shape: Shape::PlayerPair,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/entity/event/v1/ServerEntityLevelChangeEvents",
        field: "AFTER_PLAYER_CHANGE_LEVEL",
        method: "afterChangeLevel",
        descriptor: "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/server/level/ServerLevel;)V",
        event: GameEvent::PlayerChangeLevel,
        shape: Shape::ChangeLevel,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/command/v2/CommandRegistrationCallback",
        field: "EVENT",
        method: "register",
        descriptor: "(Lcom/mojang/brigadier/CommandDispatcher;Lnet/minecraft/commands/CommandBuildContext;Lnet/minecraft/commands/Commands$CommandSelection;)V",
        event: GameEvent::RegisterCommands,
        shape: Shape::Commands,
    },
    // Client events (`fabric-lifecycle-events-v1`, `fabric-networking-api-v1`
    // of the same Fabric API), fired by the window only.
    fabric(
        client_lifecycle!("ClientTickEvents"),
        "END_CLIENT_TICK",
        "onEndTick",
        "(Lnet/minecraft/client/Minecraft;)V",
        GameEvent::ClientTickEnd,
        Shape::Minecraft,
    ),
    fabric(
        client_lifecycle!("ClientTickEvents"),
        "START_CLIENT_TICK",
        "onStartTick",
        "(Lnet/minecraft/client/Minecraft;)V",
        GameEvent::ClientTickStart,
        Shape::Minecraft,
    ),
    fabric(
        client_lifecycle!("ClientTickEvents"),
        "END_LEVEL_TICK",
        "onEndTick",
        "(Lnet/minecraft/client/multiplayer/ClientLevel;)V",
        GameEvent::ClientLevelTickEnd,
        Shape::ClientLevel,
    ),
    fabric(
        client_lifecycle!("ClientTickEvents"),
        "START_LEVEL_TICK",
        "onStartTick",
        "(Lnet/minecraft/client/multiplayer/ClientLevel;)V",
        GameEvent::ClientLevelTickStart,
        Shape::ClientLevel,
    ),
    fabric(
        client_lifecycle!("ClientLifecycleEvents"),
        "CLIENT_STARTED",
        "onClientStarted",
        "(Lnet/minecraft/client/Minecraft;)V",
        GameEvent::ClientStarted,
        Shape::Minecraft,
    ),
    fabric(
        client_lifecycle!("ClientLifecycleEvents"),
        "CLIENT_STOPPING",
        "onClientStopping",
        "(Lnet/minecraft/client/Minecraft;)V",
        GameEvent::ClientStopping,
        Shape::Minecraft,
    ),
    FabricField {
        owner: "net/fabricmc/fabric/api/client/networking/v1/ClientPlayConnectionEvents",
        field: "JOIN",
        method: "onPlayReady",
        descriptor: "(Lnet/minecraft/client/multiplayer/ClientPacketListener;Lnet/fabricmc/fabric/api/networking/v1/PacketSender;Lnet/minecraft/client/Minecraft;)V",
        event: GameEvent::ClientJoin,
        shape: Shape::ClientJoin,
    },
    FabricField {
        owner: "net/fabricmc/fabric/api/client/networking/v1/ClientPlayConnectionEvents",
        field: "DISCONNECT",
        method: "onPlayDisconnect",
        descriptor: "(Lnet/minecraft/client/multiplayer/ClientPacketListener;Lnet/minecraft/client/Minecraft;)V",
        event: GameEvent::ClientDisconnect,
        shape: Shape::ClientDisconnect,
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
    (
        "entity/player/PlayerInteractEvent$LeftClickBlock",
        GameEvent::AttackBlock,
    ),
    (
        "entity/player/PlayerInteractEvent$EntityInteract",
        GameEvent::UseEntity,
    ),
    ("entity/EntityLeaveLevelEvent", GameEvent::EntityUnload),
    (
        "entity/living/LivingIncomingDamageEvent",
        GameEvent::LivingAllowDamage,
    ),
    (
        "entity/living/LivingDamageEvent$Post",
        GameEvent::LivingAfterDamage,
    ),
    (
        "entity/player/PlayerEvent$PlayerRespawnEvent",
        GameEvent::PlayerRespawn,
    ),
    ("entity/player/PlayerEvent$Clone", GameEvent::PlayerCopy),
    (
        "entity/player/PlayerEvent$PlayerChangedDimensionEvent",
        GameEvent::PlayerChangeLevel,
    ),
    ("RegisterCommandsEvent", GameEvent::RegisterCommands),
    ("TickEvent$ClientTickEvent$Pre", GameEvent::ClientTickStart),
    ("TickEvent$ClientTickEvent$Post", GameEvent::ClientTickEnd),
];

/// NeoForge (`net/neoforged/neoforge/client/event/`) and Forge
/// (`net/minecraftforge/client/event/`) client event classes, by the path
/// under that root (NeoForge 26.3.0.101's names). Forge's client tick lives in
/// `TickEvent` under the common root, in [`BUS_EVENTS`].
const CLIENT_BUS_EVENTS: &[(&str, GameEvent)] = &[
    ("ClientTickEvent$Pre", GameEvent::ClientTickStart),
    ("ClientTickEvent$Post", GameEvent::ClientTickEnd),
    ("ClientPlayerNetworkEvent$LoggingIn", GameEvent::ClientJoin),
    (
        "ClientPlayerNetworkEvent$LoggingOut",
        GameEvent::ClientDisconnect,
    ),
    (
        "RegisterKeyMappingsEvent",
        GameEvent::ClientRegisterKeyMappings,
    ),
    ("RegisterGuiLayersEvent", GameEvent::ClientRegisterGuiLayers),
];
const NEOFORGE_CLIENT_EVENT_ROOT: &str = "net/neoforged/neoforge/client/event/";
const FORGE_CLIENT_EVENT_ROOT: &str = "net/minecraftforge/client/event/";

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
    let client = class
        .strip_prefix(NEOFORGE_CLIENT_EVENT_ROOT)
        .map(|rest| (rest, EventLoader::NeoForge))
        .or_else(|| {
            class
                .strip_prefix(FORGE_CLIENT_EVENT_ROOT)
                .map(|rest| (rest, EventLoader::Forge))
        });
    if let Some((rest, loader)) = client {
        return CLIENT_BUS_EVENTS
            .iter()
            .find(|(path, _)| *path == rest)
            .map(|(_, event)| (*event, loader));
    }
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
    /// Its Fabric phase or NeoForge/Forge priority ([`dispatch_order`]).
    pub order: ListenerOrder,
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
    /// The `addPhaseOrdering` calls made on the Fabric fields the game fires.
    pub phase_orderings: Vec<PhaseOrdering>,
}

impl GameListeners {
    /// Bitmask of the events with at least one listener ([`GameEvent::bit`]).
    pub fn mask(&self) -> u64 {
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
    /// Server-wide facts (world folder, game rules, registries, the channels
    /// each client takes), shared between firings: the game rebuilds it only
    /// when one of them changes.
    pub server: Arc<ServerFacts>,
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
    /// The damage amount of an `ALLOW_DEATH`, `ALLOW_DAMAGE` or `AFTER_DAMAGE`.
    pub amount: f32,
    /// The block a use, attack or break is about.
    pub pos: Option<[i32; 3]>,
    /// The face clicked (`[0, 1, 0]` is the top).
    pub face: Option<[i32; 3]>,
    /// The off hand acts (the main hand otherwise).
    pub off_hand: bool,
    /// `AFTER_RESPAWN`/`COPY_FROM`'s `alive` (back from the End, not a
    /// death); `AFTER_DAMAGE`'s `blocked`.
    pub flag: bool,
    /// The level a player left (`AFTER_PLAYER_CHANGE_LEVEL`'s origin);
    /// `level` is the destination.
    pub origin: Option<String>,
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
        // `LevelResource` and `GameRules` constants: values, the same with or
        // without a firing.
        if let Some(value) = facade_server::static_field(r) {
            return Some(value);
        }
        if r.descriptor == "Lnet/fabricmc/fabric/api/event/Event;"
            && r.owner.starts_with("net/fabricmc/fabric/api/")
        {
            return Some(marker(format!("fabric-event/{}.{}", r.owner, r.name)));
        }
        // `Event.DEFAULT_PHASE` and the `EventPriority` constants: what a
        // listener's order is registered with (`order.rs`).
        if r.owner == FABRIC_EVENT && r.name == "DEFAULT_PHASE" {
            return Some(marker(format!("identifier/{}", order::DEFAULT_PHASE)));
        }
        if order::PRIORITY_CLASSES.contains(&r.owner.as_str())
            && order::PRIORITIES.contains(&r.name.as_str())
        {
            return Some(marker(format!("enum/{}/{}", r.owner, r.name)));
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

    /// Answer a static call the façade models during a firing, or `None`:
    /// `ServerPlayNetworking.canSend(player, type)`, from the channels the
    /// game says that player's client takes. Outside a firing, or for a
    /// player argument that is not a façade player, it is not answered here.
    pub(crate) fn game_event_invoke(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let firing = self.game_event.as_deref()?;
        let player = args
            .first()
            .and_then(expect_ref)
            .and_then(|text| match facade(text) {
                Some(Facade::Player(uuid) | Facade::Connection(uuid)) => Some(uuid),
                _ => None,
            });
        let channel = args.get(1).and_then(|value| {
            facade_server::channel_of(value, |marker| self.packet_types.get(marker).cloned())
        });
        facade_server::can_send(&firing.facade.server, r, player, channel.as_deref())
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
        // `new LevelResource("data/x.json")`: keep its id for a later
        // `getWorldPath`.
        if let Some(id) = facade_server::level_resource_init(r, args) {
            self.level_resources.insert(text.clone(), id);
            return Some(Ok(Value::Void));
        }
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
        // The server-wide markers (rules, registries, a rule, a level
        // resource) answer from the firing's server facts; outside a firing
        // there are none, and the call refuses.
        if facade_server::owns(text) {
            return Some(match self.game_event.as_deref() {
                Some(firing) => facade_server::answer(&firing.facade.server, text, r, args),
                None => Err(NativeError::UnknownHostCall(r.clone())),
            });
        }
        let kind = facade(text)?;
        if matches!(
            kind,
            Facade::Server | Facade::Level(_) | Facade::Player(_) | Facade::Entity(_)
        ) && let Some(firing) = self.game_event.as_deref()
            && let Some(answer) = facade_server::server_member(
                &firing.facade.server,
                r,
                &facade_server::with_level_resources(args, |m| {
                    self.level_resources.get(m).cloned()
                }),
            )
        {
            return Some(answer);
        }
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
        if server {
            // A server never fires a client event: a common entrypoint's
            // client registrations count as events it does not fire.
            let (client, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut out.listeners)
                .into_iter()
                .partition(|listener| listener.event.is_client());
            out.listeners = kept;
            for listener in client {
                count_unsupported(&mut out, listener.source);
            }
        }
        out
    }

    /// How many host calls the log holds, to roll a listener's calls back
    /// once it ran: a listener fired every tick must not grow the log for the
    /// life of the server.
    pub(crate) fn call_log_marks(&self) -> (usize, usize, usize) {
        (
            self.calls.len(),
            self.invocations.len(),
            self.posted_events.len(),
        )
    }

    pub(crate) fn truncate_call_log(
        &mut self,
        (calls, invocations, posted): (usize, usize, usize),
    ) {
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
pub(crate) fn indy_argument(args: &[Value]) -> Option<&str> {
    args.iter().rev().find_map(|value| match value {
        Value::Ref(Some(text)) if text.starts_with("@indy:") => Some(text.as_str()),
        _ => None,
    })
}

/// An object argument whose class the jar declares: `cls#N`.
pub(crate) fn jar_object<'a>(jar: &NativeJar, value: &'a Value) -> Option<(&'a str, &'a Value)> {
    let text = expect_ref(value)?;
    let (class, _) = text.split_once('#')?;
    jar.classes.contains_key(class).then_some((class, value))
}

/// A method `name`+`descriptor` declared by `class` or a jar superclass.
pub(crate) fn resolve_method(
    jar: &NativeJar,
    class: &str,
    name: &str,
    descriptor: &str,
) -> Option<MethodRef> {
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

/// A Fabric field as listeners name it (`UseBlockCallback.EVENT`).
fn fabric_source(row: &FabricField) -> String {
    format!(
        "{}.{}",
        row.owner.rsplit('/').next().unwrap_or(row.owner),
        row.field
    )
}

/// The phase id an `Identifier` argument names (`identifier/mymod:late` →
/// `mymod:late`). Anything else keeps its text, so two registrations with
/// the same unmodelled value still share a phase.
fn phase_id(value: &Value) -> String {
    match expect_ref(value) {
        Some(text) => text.strip_prefix("identifier/").unwrap_or(text).to_owned(),
        None => order::DEFAULT_PHASE.to_owned(),
    }
}

/// The `EventPriority` a bus registration passes, `NORMAL` without one.
fn bus_priority(args: &[Value]) -> u8 {
    args.iter()
        .find_map(|value| expect_ref(value).and_then(|text| order::priority_of(text)))
        .unwrap_or(2)
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
    // Fabric: `<Field>.addPhaseOrdering(first, second)`, kept for the order.
    if owner == FABRIC_EVENT && name == "addPhaseOrdering" {
        if let Some(row) = receiver.and_then(|r| fabric_field(r))
            && let [first, second] = invocation.args.as_slice()
        {
            out.phase_orderings.push(PhaseOrdering {
                source: fabric_source(row),
                first: phase_id(first),
                second: phase_id(second),
            });
        }
        return;
    }
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
        let source = fabric_source(row);
        // `register(Identifier phase, listener)`: the phase is the first
        // argument; plain `register(listener)` is the default phase.
        let order = if invocation
            .method
            .descriptor
            .starts_with("(Lnet/minecraft/resources/Identifier;")
            && let Some(phase) = invocation.args.first()
        {
            ListenerOrder::Phase(phase_id(phase))
        } else {
            ListenerOrder::default_phase()
        };
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
            order,
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
        push_bus_listener(class, invocation, jar, out, false, bus_priority(&invocation.args));
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
                Some(class) => push_bus_listener(
                    &class,
                    invocation,
                    jar,
                    out,
                    receives,
                    bus_priority(&invocation.args),
                ),
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
    priority: u8,
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
        order: ListenerOrder::Priority(priority),
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
        // `@SubscribeEvent(priority = EventPriority.HIGH)`.
        let priority = match annotation.values.get("priority") {
            Some(crate::AnnotationValue::String(text)) => order::priority_of(text).unwrap_or(2),
            _ => 2,
        };
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
            order: ListenerOrder::Priority(priority),
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
    let hand = || {
        let hand = if context.off_hand {
            "OFF_HAND"
        } else {
            "MAIN_HAND"
        };
        marker(format!("enum/net/minecraft/world/InteractionHand/{hand}"))
    };
    let opaque = |what: &str| marker(format!("game/{what}/0"));
    // `block-pos/x,y,z`, the host's block position value (`getX`...).
    let pos = || {
        context
            .pos
            .map(|[x, y, z]| marker(format!("block-pos/{x},{y},{z}")))
            .ok_or_else(|| missing("block position"))
    };
    let direction = || {
        let name = match context.face {
            Some([0, -1, 0]) => "DOWN",
            Some([0, 0, -1]) => "NORTH",
            Some([0, 0, 1]) => "SOUTH",
            Some([-1, 0, 0]) => "WEST",
            Some([1, 0, 0]) => "EAST",
            _ => "UP",
        };
        marker(format!("enum/net/minecraft/core/Direction/{name}"))
    };
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
            pos()?,
            opaque("block-state"),
            Value::Ref(None),
        ],
        Shape::UseBlock => {
            // The hit result names its block: `game/block-hit/x,y,z`.
            let [x, y, z] = context.pos.ok_or_else(|| missing("block position"))?;
            vec![
                player_marker(player()?),
                level()?,
                hand(),
                marker(format!("game/block-hit/{x},{y},{z}")),
            ]
        }
        Shape::AttackBlock => vec![
            player_marker(player()?),
            level()?,
            hand(),
            pos()?,
            direction(),
        ],
        Shape::AfterDamage => vec![
            subject()?,
            damage(),
            Value::Float(f64::from(context.amount)),
            Value::Float(f64::from(context.amount)),
            Value::Int(i32::from(context.flag)),
        ],
        // Horizon keeps one player across a respawn: the old and the new
        // player are the same marker.
        Shape::PlayerPair => {
            let player = player_marker(player()?);
            vec![player.clone(), player, Value::Int(i32::from(context.flag))]
        }
        Shape::ChangeLevel => vec![
            player_marker(player()?),
            context
                .origin
                .as_deref()
                .map(level_marker)
                .ok_or_else(|| missing("origin level"))?,
            level()?,
        ],
        Shape::UseItem => vec![player_marker(player()?), level()?, hand()],
        Shape::AttackEntity => vec![
            player_marker(player()?),
            level()?,
            hand(),
            subject()?,
            opaque("entity-hit"),
        ],
        Shape::Commands => {
            let selection = if firing.facade.dedicated {
                "DEDICATED"
            } else {
                "INTEGRATED"
            };
            vec![
                marker(crate::brigadier::DISPATCHER.into()),
                marker(crate::brigadier::BUILD_CONTEXT.into()),
                marker(format!(
                    "enum/net/minecraft/commands/Commands$CommandSelection/{selection}"
                )),
            ]
        }
        Shape::Minecraft => vec![marker(crate::client_surface::MINECRAFT.into())],
        Shape::ClientLevel => vec![marker(crate::client_surface::LEVEL.into())],
        Shape::ClientJoin => vec![
            marker(crate::client_surface::CONNECTION.into()),
            marker(crate::client_surface::PACKET_SENDER.into()),
            marker(crate::client_surface::MINECRAFT.into()),
        ],
        Shape::ClientDisconnect => vec![
            marker(crate::client_surface::CONNECTION.into()),
            marker(crate::client_surface::MINECRAFT.into()),
        ],
    })
}

impl NativeJar {
    /// Follow a method reference to a functional interface's own method on a
    /// captured lambda (`handler::accept`, the shape Architectury-style event
    /// wrappers register) to the jar method that lambda names. Bounded: eight
    /// links, and only while the target is not a class of this jar.
    fn unwrap_indy(
        &self,
        mut target: MethodRef,
        mut captures: Vec<Value>,
    ) -> (MethodRef, Vec<Value>) {
        for _ in 0..8 {
            if self.classes.contains_key(&target.owner) {
                break;
            }
            let Some(Value::Ref(Some(inner))) = captures.first() else {
                break;
            };
            let Ok(inner) = parse_callback_marker(inner) else {
                break;
            };
            let mut next = inner.captures;
            next.extend(captures.drain(1..));
            (target, captures) = (inner.target, next);
        }
        (target, captures)
    }

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
        let marks = host.call_log_marks();
        host.game_event = Some(Box::new(firing.clone()));
        let result = self.invoke_listener_callback(&listener.callback, event_args, host, budget);
        if let Some(after) = host.game_event.take() {
            firing.cancelled = after.cancelled;
        }
        host.truncate_call_log(marks);
        let (target, result) = result?;
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

    /// Run one listener callback with the event's arguments: a lambda's
    /// captures (its bound receiver first) go before them, an object's
    /// method gets its receiver. Returns the jar method that ran and its
    /// value. The caller owns the host's firing state and call-log rollback.
    pub(crate) fn invoke_listener_callback(
        &self,
        callback: &ListenerCallback,
        event_args: Vec<Value>,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<(MethodRef, Value), NativeError> {
        let (target, receiver, args) = match callback {
            ListenerCallback::Indy { target, captures } => {
                let (target, captures) = self.unwrap_indy(target.clone(), captures.clone());
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
                let mut captures = captures;
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
                (method.clone(), Some(receiver.clone()), event_args)
            }
            ListenerCallback::Static { method } => (method.clone(), None, event_args),
        };
        let target = &target;
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
        let value = self.invoke_persistent(target, receiver, &args, host, budget)?;
        Ok((target.clone(), value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listener's server, level and player markers answer the server-wide
    /// members from the firing's facts, and refuse them outside a firing.
    #[test]
    fn listener_markers_answer_server_wide_members_from_the_facts() {
        let call = |owner: &str, name: &str, descriptor: &str| MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        };
        let alex = 0x0123_4567_89ab_cdef_0011_2233_4455_6677_u128;
        let facade = EventFacade {
            players: vec![FacadePlayer {
                uuid: alex,
                name: "Alex".into(),
                level: "minecraft:overworld".into(),
                entity_id: 7,
            }],
            server: Arc::new(ServerFacts {
                world_dir: Some("/horizon/game/world".into()),
                game_rules: [("keep_inventory".to_owned(), GameRuleValue::Bool(false))].into(),
                client_channels: [(alex, ["demo:sync".to_owned()].into())].into(),
                ..ServerFacts::default()
            }),
            ..EventFacade::default()
        };
        let mut host = StdHost::new();
        let root = host
            .game_event_static(&call(
                "net/minecraft/world/level/storage/LevelResource",
                "ROOT",
                "Lnet/minecraft/world/level/storage/LevelResource;",
            ))
            .unwrap();
        let world_path = call(
            "net/minecraft/server/MinecraftServer",
            "getWorldPath",
            "(Lnet/minecraft/world/level/storage/LevelResource;)Ljava/nio/file/Path;",
        );
        let server = marker(SERVER.into());
        // No firing: no facts to answer from.
        assert!(matches!(
            host.game_event_call(&world_path, &server, std::slice::from_ref(&root)),
            Some(Err(NativeError::UnknownHostCall(_)))
        ));
        host.game_event = Some(Box::new(EventFiring::new(
            GameEvent::ServerStarted,
            Arc::new(facade),
            EventContext::default(),
        )));
        assert_eq!(
            host.game_event_call(&world_path, &server, std::slice::from_ref(&root))
                .unwrap()
                .unwrap(),
            Value::Ref(Some("path:/horizon/game/world/.".into()))
        );
        let rules = host
            .game_event_call(
                &call(
                    "net/minecraft/server/level/ServerLevel",
                    "getGameRules",
                    "()Lnet/minecraft/world/level/gamerules/GameRules;",
                ),
                &level_marker("minecraft:overworld"),
                &[],
            )
            .unwrap()
            .unwrap();
        let keep = host
            .game_event_static(&call(
                "net/minecraft/world/level/gamerules/GameRules",
                "KEEP_INVENTORY",
                "Lnet/minecraft/world/level/gamerules/GameRule;",
            ))
            .unwrap();
        let get = call(
            "net/minecraft/world/level/gamerules/GameRules",
            "get",
            "(Lnet/minecraft/world/level/gamerules/GameRule;)Ljava/lang/Object;",
        );
        assert_eq!(
            host.game_event_call(&get, &rules, &[keep])
                .unwrap()
                .unwrap(),
            Value::Ref(Some("boolean:false".into()))
        );
        // `registryAccess` with no registries listed refuses by name.
        assert!(matches!(
            host.game_event_call(
                &call(
                    "net/minecraft/server/level/ServerPlayer",
                    "registryAccess",
                    "()Lnet/minecraft/core/RegistryAccess;",
                ),
                &player_marker(alex),
                &[],
            ),
            Some(Err(NativeError::UnknownHostCall(_)))
        ));
        let can_send = call(
            "net/fabricmc/fabric/api/networking/v1/ServerPlayNetworking",
            "canSend",
            "(Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/resources/Identifier;)Z",
        );
        let ask = |host: &mut StdHost, channel: &str| {
            host.game_event_invoke(
                &can_send,
                &[
                    player_marker(alex),
                    Value::Ref(Some(format!("identifier/{channel}"))),
                ],
            )
            .unwrap()
            .unwrap()
        };
        assert_eq!(ask(&mut host, "demo:sync"), Value::Int(1));
        assert_eq!(ask(&mut host, "demo:other"), Value::Int(0));
    }

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
        // Both loaders' command registration is the command-tree build.
        assert_eq!(
            bus_event("net/neoforged/neoforge/event/RegisterCommandsEvent"),
            Some((GameEvent::RegisterCommands, EventLoader::NeoForge))
        );
        assert_eq!(
            bus_event("net/minecraftforge/event/RegisterCommandsEvent"),
            Some((GameEvent::RegisterCommands, EventLoader::Forge))
        );
    }

    #[test]
    fn client_events_name_their_fabric_fields_and_bus_classes() {
        let row = fabric_field(
            "fabric-event/net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents.END_CLIENT_TICK",
        )
        .unwrap();
        assert_eq!(
            (row.event, row.method, row.shape),
            (GameEvent::ClientTickEnd, "onEndTick", Shape::Minecraft)
        );
        let join = fabric_field(
            "fabric-event/net/fabricmc/fabric/api/client/networking/v1/ClientPlayConnectionEvents.JOIN",
        )
        .unwrap();
        assert_eq!(join.event, GameEvent::ClientJoin);
        // The server's own `JOIN` stays the server's.
        let server_join = fabric_field(
            "fabric-event/net/fabricmc/fabric/api/networking/v1/ServerPlayConnectionEvents.JOIN",
        )
        .unwrap();
        assert_eq!(server_join.event, GameEvent::PlayerJoin);
        assert_eq!(
            bus_event("net/neoforged/neoforge/client/event/ClientTickEvent$Post"),
            Some((GameEvent::ClientTickEnd, EventLoader::NeoForge))
        );
        assert_eq!(
            bus_event("net/neoforged/neoforge/client/event/RegisterKeyMappingsEvent"),
            Some((GameEvent::ClientRegisterKeyMappings, EventLoader::NeoForge))
        );
        assert_eq!(
            bus_event("net/minecraftforge/event/TickEvent$ClientTickEvent$Pre"),
            Some((GameEvent::ClientTickStart, EventLoader::Forge))
        );
        // A client class the game does not fire stays unnamed.
        assert_eq!(
            bus_event("net/neoforged/neoforge/client/event/ScreenEvent$Init$Post"),
            None
        );
        assert!(GameEvent::ClientTickEnd.is_client());
        assert!(!GameEvent::ServerTickEnd.is_client());
    }

    #[test]
    fn event_bits_are_distinct() {
        let mask = GameEvent::ALL.iter().fold(0u64, |mask, event| {
            assert_eq!(mask & event.bit(), 0, "{event:?}");
            mask | event.bit()
        });
        assert_eq!(mask.count_ones() as usize, GameEvent::ALL.len());
    }
}
