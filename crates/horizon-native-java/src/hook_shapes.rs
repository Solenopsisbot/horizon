//! Value-returning shapes and the Java-tier-only sites of the game-hook tier.
//!
//! [`GAME_HOOK_SITES`] maps a Mixin `@Inject` onto a hook point of the public
//! native-mod API (`native-hooks.json`), so every row there must agree with
//! the census, and every row is a `void` method fired at one position. Two
//! kinds of plan need more than that table can say:
//!
//! - **A target that returns a value.** Mixin hands such a callback a
//!   `CallbackInfoReturnable`. At `HEAD` a cancellable callback can
//!   short-circuit the method with `setReturnValue` (or a bare `cancel()`,
//!   which returns the type's default); at `RETURN`/`TAIL` the callback reads
//!   the method's own result with `getReturnValue` and a cancellable one can
//!   replace it. The host hands the result in and takes the override back
//!   ([`NativeJar::run_game_hook_returning`], [`GameHookRun::returned`]).
//! - **A position the public API has no hook for.** A Java callback may want
//!   a point Horizon's game code reaches but that no native mod has asked
//!   for (`PlayerList.remove`'s `HEAD`, `MinecraftServer.stopServer`'s
//!   `TAIL`). Adding a native hook would change the public API, which is
//!   stamped and versioned; the Java tier does not need that.
//!
//! [`JAVA_TIER_SITES`] holds both. It is **internal**: it is not part of
//! `native-hooks.json`, carries no API version a native mod can depend on,
//! and its hook names start with `java_` so a census check can tell them
//! apart. Each row is still hand-checked the same way: the vanilla method it
//! stands for, the Horizon function that is that point, what the host hands
//! the callback, and what a cancel or an override does there (the row's
//! `note` says it in one sentence, the game side's firing helper in
//! `crates/horizon-client/src/game/java_game_hooks.rs` says the rest).
//!
//! A row fires at exactly one position: `HEAD`, or `RETURN`/`TAIL` (which are
//! the same point for a method with one return). A method the tier serves at
//! both ends has one row per end, and [`game_hook_site_at`] picks the row by
//! the plan's anchor.

use super::*;

/// Hook names of [`JAVA_TIER_SITES`] start with this, so they can never be
/// mistaken for a `native-hooks.json` hook point.
pub const JAVA_TIER_HOOK_PREFIX: &str = "java_";

/// The internal hook points the Java game-hook tier fires besides the public
/// [`GAME_HOOK_SITES`]. See the module documentation for why they are kept
/// apart, and the coverage rule on [`GAME_HOOK_SITES`], which applies to these
/// rows unchanged except that a target may return a value.
///
/// Each row names the vanilla method (checked against the 26.3 bytecode with
/// `tools/rea_vanilla.py method`), and the game fires it from the Horizon
/// function that runs that code:
///
/// | Hook | Vanilla | Horizon point |
/// |---|---|---|
/// | `java_player_list_remove` | `PlayerList#remove` `HEAD` | `native_player_logged_out`, before the next player object |
/// | `java_server_stopped` | `MinecraftServer#stopServer` `TAIL` | after the final save and `native_lifecycle_world_closed` |
/// | `java_player_tick_head` | `ServerPlayer#tick` `HEAD` | `tick_guests`, where a living guest's tick starts |
/// | `java_base_player_tick_head` | `Player#tick` `HEAD` | the same point, right after `java_player_tick_head` |
/// | `java_base_player_tick_tail` | `Player#tick` `TAIL` | `native_player_tick`, right before `player_tick` |
/// | `java_destroy_speed` | `Player#getDestroySpeed` `RETURN` | the mining speed `tick_mining` and `destroy_progress` compute |
/// | `java_grow_crop` | `BoneMealItem#growCrop` `HEAD` | `use_bone_meal` and `dispense_bone_meal`, before `grow_crop` |
/// | `java_server_tick_head` | `MinecraftServer#tickServer` `HEAD` | `native_server_tick`'s `ServerTickStart` |
/// | `java_server_tick_tail` | `MinecraftServer#tickServer` `TAIL` | `native_server_tick`'s `ServerTickEnd` |
/// | `java_level_tick_head` | `ServerLevel#tick` `HEAD` | `native_level_tick_pre` |
/// | `java_level_tick_tail` | `ServerLevel#tick` `TAIL` | `native_level_tick_post` |
/// | `java_server_constructed` | `MinecraftServer#<init>` `TAIL` | before the configs and `native_server_about_to_start` |
/// | `java_dedicated_server_constructed` | `DedicatedServer#<init>` `TAIL` | right after `java_server_constructed`, dedicated only |
/// | `java_bootstrap_head` | `Bootstrap#bootStrap` `HEAD` | each jar's bootstrap, before its entrypoints |
/// | `java_bootstrap_tail` | `Bootstrap#bootStrap` `TAIL` | right after `java_bootstrap_head` |
/// | `java_living_hurt` | `LivingEntity#hurtServer` `HEAD` | `hurt_server` (`native_living_incoming_damage`), past the guards |
/// | `java_block_player_destroy` | `Block#playerDestroy` `HEAD` | `break_block_as_player`, before the mined stat and drops |
/// | `java_block_player_destroyed` | `Block#playerDestroy` `TAIL` | the same, after the drops and experience |
/// | `java_explosion_explode` | `ServerExplosion#explode` `HEAD` | `server_explode_as`, before the blast is heard |
/// | `java_drop_death_loot_head` | `LivingEntity#dropAllDeathLoot` `HEAD` | `mob_died`, before `drop_death_loot` |
/// | `java_drop_death_loot_tail` | `LivingEntity#dropAllDeathLoot` `TAIL` | `mob_died`, after the loot and equipment |
/// | `java_living_died` | `LivingEntity#die` `TAIL` | `mob_died`, after the loot and the wither rose |
pub const JAVA_TIER_SITES: &[GameHookSite] = &[
    GameHookSite {
        hook: "java_player_list_remove",
        version: 1,
        target_owner: "net/minecraft/server/players/PlayerList",
        target_name: "remove",
        target_descriptor: "(Lnet/minecraft/server/level/ServerPlayer;)V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("player"),
        note: "when a player leaves by any path (a lost connection, a kick, the server stopping), before its last save; Horizon has already unlisted it, and the host hands the leaving player, on the authoritative server",
    },
    GameHookSite {
        hook: "java_server_stopped",
        version: 1,
        target_owner: "net/minecraft/server/MinecraftServer",
        target_name: "stopServer",
        target_descriptor: "()V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("server"),
        note: "at the end of MinecraftServer.stopServer, after the players and worlds were saved and every level closed, on the authoritative server",
    },
    GameHookSite {
        hook: "java_player_tick_head",
        version: 1,
        target_owner: "net/minecraft/server/level/ServerPlayer",
        target_name: "tick",
        target_descriptor: "()V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("player"),
        note: "at the start of ServerPlayer.tick, before digging and item use, for a living player (the population of player_tick), on the authoritative server",
    },
    GameHookSite {
        hook: "java_base_player_tick_head",
        version: 1,
        target_owner: "net/minecraft/world/entity/player/Player",
        target_name: "tick",
        target_descriptor: "()V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("player"),
        note: "at the start of a server player's Player.tick; Horizon runs ServerPlayer.tick and doTick's Player.tick as one step, so it fires right after ServerPlayer.tick's HEAD, for a living player, on the authoritative server only",
    },
    GameHookSite {
        hook: "java_base_player_tick_tail",
        version: 1,
        target_owner: "net/minecraft/world/entity/player/Player",
        target_name: "tick",
        target_descriptor: "()V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("player"),
        note: "at the end of a server player's Player.tick; Horizon runs ServerPlayer.tick and doTick's Player.tick as one step, so it fires right before ServerPlayer.tick's TAIL, for a living player, on the authoritative server only",
    },
    GameHookSite {
        hook: "java_destroy_speed",
        version: 1,
        target_owner: "net/minecraft/world/entity/player/Player",
        target_name: "getDestroySpeed",
        target_descriptor: "(Lnet/minecraft/world/level/block/state/BlockState;)F",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Cancel { out: "speed" },
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("player"),
        note: "when the server computes a player's mining speed for a block (each mining tick and each dig check), after the tool, efficiency, haste, fatigue, break-speed, water and airborne factors; an override is the speed the progress divides by hardness, on the authoritative server",
    },
    GameHookSite {
        hook: "java_grow_crop",
        version: 1,
        target_owner: "net/minecraft/world/item/BoneMealItem",
        target_name: "growCrop",
        target_descriptor: "(Lnet/minecraft/world/item/ItemStack;Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;)Z",
        anchors: &["HEAD"],
        contract: GameHookContract::Cancel { out: "grown" },
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::None,
        note: "at the start of BoneMealItem.growCrop for a player's bone meal and a dispenser's (not a farmer villager's, which Horizon decides inside its brain); a cancel returns its value without growing or using up the meal, on the authoritative server",
    },
    GameHookSite {
        hook: "java_server_tick_head",
        version: 1,
        target_owner: "net/minecraft/server/MinecraftServer",
        target_name: "tickServer",
        target_descriptor: "(Ljava/util/function/BooleanSupplier;)V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("server"),
        note: "at the start of each server tick, before any level ticks (a paused single player server ticks nothing, as IntegratedServer skips tickServer); the host hands null for haveTime, on the authoritative server",
    },
    GameHookSite {
        hook: "java_server_tick_tail",
        version: 1,
        target_owner: "net/minecraft/server/MinecraftServer",
        target_name: "tickServer",
        target_descriptor: "(Ljava/util/function/BooleanSupplier;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("server"),
        note: "at the end of each server tick, after the last level ticked; the host hands null for haveTime, on the authoritative server",
    },
    GameHookSite {
        hook: "java_level_tick_head",
        version: 1,
        target_owner: "net/minecraft/server/level/ServerLevel",
        target_name: "tick",
        target_descriptor: "(Ljava/util/function/BooleanSupplier;)V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("level"),
        note: "at the start of each server level's tick, once per dimension per server tick; the host hands null for haveTime, on the authoritative server",
    },
    GameHookSite {
        hook: "java_level_tick_tail",
        version: 1,
        target_owner: "net/minecraft/server/level/ServerLevel",
        target_name: "tick",
        target_descriptor: "(Ljava/util/function/BooleanSupplier;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("level"),
        note: "at the end of a server level's successful tick; the host hands null for haveTime, on the authoritative server",
    },
    GameHookSite {
        hook: "java_server_constructed",
        version: 1,
        target_owner: "net/minecraft/server/MinecraftServer",
        target_name: "<init>",
        target_descriptor: "(Ljava/lang/Thread;Lnet/minecraft/world/level/storage/LevelStorageSource$LevelStorageAccess;Lnet/minecraft/server/packs/repository/PackRepository;Lnet/minecraft/server/WorldStem;Ljava/util/Optional;Ljava/net/Proxy;Lcom/mojang/datafixers/DataFixer;Lnet/minecraft/server/Services;Lnet/minecraft/server/level/progress/LevelLoadListener;ZLnet/minecraft/server/notifications/NotificationManager;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("server"),
        note: "once per server start, after the Java mods' entrypoints and before any server lifecycle event; the game objects among the arguments are identities, the JDK ones (thread, game rules, proxy) are null, on the authoritative server",
    },
    GameHookSite {
        hook: "java_dedicated_server_constructed",
        version: 1,
        target_owner: "net/minecraft/server/dedicated/DedicatedServer",
        target_name: "<init>",
        target_descriptor: "(Ljava/lang/Thread;Lnet/minecraft/world/level/storage/LevelStorageSource$LevelStorageAccess;Lnet/minecraft/server/packs/repository/PackRepository;Lnet/minecraft/server/WorldStem;Ljava/util/Optional;Lnet/minecraft/server/dedicated/DedicatedServerSettings;Lcom/mojang/datafixers/DataFixer;Lnet/minecraft/server/Services;Lnet/minecraft/server/jsonrpc/ManagementServer;Lnet/minecraft/server/notifications/NotificationManager;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("server"),
        note: "once per dedicated server start, right after MinecraftServer's constructor tail; the game objects among the arguments are identities, the JDK ones are null, on a dedicated server only",
    },
    GameHookSite {
        hook: "java_bootstrap_head",
        version: 1,
        target_owner: "net/minecraft/server/Bootstrap",
        target_name: "bootStrap",
        target_descriptor: "()V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::None,
        note: "once per jar as the server bootstraps it, before its entrypoints (vanilla's Main bootstraps before Fabric's server entrypoints run); each jar hears its own, not before every other jar's entrypoints",
    },
    GameHookSite {
        hook: "java_bootstrap_tail",
        version: 1,
        target_owner: "net/minecraft/server/Bootstrap",
        target_name: "bootStrap",
        target_descriptor: "()V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::None,
        note: "once per jar right after java_bootstrap_head, with the vanilla registries already built, before the jar's entrypoints",
    },
    GameHookSite {
        hook: "java_living_hurt",
        version: 1,
        target_owner: "net/minecraft/world/entity/LivingEntity",
        target_name: "hurtServer",
        target_descriptor: "(Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/world/damagesource/DamageSource;F)Z",
        anchors: &["HEAD"],
        contract: GameHookContract::Cancel { out: "hurt" },
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("entity"),
        note: "when a player or mob is about to take a hit, past hurtServer's invulnerable, dead and fire-resistance guards (a hit those refuse fires nothing); a cancel skips the whole hurt, and the caller acts on a refused hit whatever value was set, on the authoritative server",
    },
    GameHookSite {
        hook: "java_block_player_destroy",
        version: 1,
        target_owner: "net/minecraft/world/level/block/Block",
        target_name: "playerDestroy",
        target_descriptor: "(Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;Lnet/minecraft/world/item/ItemStack;)V",
        anchors: &["HEAD"],
        contract: GameHookContract::Cancel { out: "canceled" },
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("block"),
        note: "when a survival player's break of a block it can harvest is about to award the mined statistic, the exhaustion and the drops; a cancel skips those (experience and an infested block's silverfish included), not a beehive's, turtle egg's or ice's own part, on the authoritative server",
    },
    GameHookSite {
        hook: "java_block_player_destroyed",
        version: 1,
        target_owner: "net/minecraft/world/level/block/Block",
        target_name: "playerDestroy",
        target_descriptor: "(Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/server/level/ServerPlayer;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/block/entity/BlockEntity;Lnet/minecraft/world/item/ItemStack;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("block"),
        note: "after Block.playerDestroy's statistic, exhaustion, drops and experience for a survival player's harvest, unless its HEAD was cancelled, on the authoritative server",
    },
    GameHookSite {
        hook: "java_explosion_explode",
        version: 1,
        target_owner: "net/minecraft/world/level/ServerExplosion",
        target_name: "explode",
        target_descriptor: "()I",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("explosion"),
        note: "when a server explosion starts, before it is heard and its blocks are picked; the explosion is an identity only, on the authoritative server",
    },
    GameHookSite {
        hook: "java_drop_death_loot_head",
        version: 1,
        target_owner: "net/minecraft/world/entity/LivingEntity",
        target_name: "dropAllDeathLoot",
        target_descriptor: "(Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/world/damagesource/DamageSource;)V",
        anchors: &["HEAD"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("entity"),
        note: "when a dying mob is about to drop its loot, equipment and experience; a player's death drops do not fire it yet, on the authoritative server",
    },
    GameHookSite {
        hook: "java_drop_death_loot_tail",
        version: 1,
        target_owner: "net/minecraft/world/entity/LivingEntity",
        target_name: "dropAllDeathLoot",
        target_descriptor: "(Lnet/minecraft/server/level/ServerLevel;Lnet/minecraft/world/damagesource/DamageSource;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("entity"),
        note: "after a dying mob dropped its loot, equipment and experience; a player's death drops do not fire it yet, on the authoritative server",
    },
    GameHookSite {
        hook: "java_living_died",
        version: 1,
        target_owner: "net/minecraft/world/entity/LivingEntity",
        target_name: "die",
        target_descriptor: "(Lnet/minecraft/world/damagesource/DamageSource;)V",
        anchors: &["RETURN", "TAIL"],
        contract: GameHookContract::Listen,
        side: Some(MixinSide::Server),
        receiver: GameHookReceiver::Named("entity"),
        note: "at the end of a mob's death, after its loot and wither rose, unless living_death cancelled it (ServerPlayer.die does not call it, so a player never fires it), on the authoritative server",
    },
];

/// The internal Java-tier sites, in table order.
pub fn java_tier_sites() -> &'static [GameHookSite] {
    JAVA_TIER_SITES
}

/// Every site the tier knows: the public table, then the internal one.
pub fn every_game_hook_site() -> impl Iterator<Item = &'static GameHookSite> + Clone {
    GAME_HOOK_SITES.iter().chain(JAVA_TIER_SITES)
}

/// The site a registration names, by hook name and contract version.
pub fn game_hook_site_named(hook: &str, version: u32) -> Option<&'static GameHookSite> {
    every_game_hook_site().find(|site| site.hook == hook && site.version == version)
}

/// Whether `site` stands for the method `target` names. An empty plan
/// descriptor (real Mixin's `method = "tick"`) matches the site's own.
fn site_names(site: &GameHookSite, target: &MethodRef) -> bool {
    site.target_owner == target.owner
        && site.target_name == target.name
        && (target.descriptor.is_empty() || target.descriptor == site.target_descriptor)
}

/// The site for one plan: the row for `target` whose position answers
/// `anchor` (an empty `@At` reads as `HEAD`), else the first row for
/// `target`, so a plan at a position no row fires still gets the site's own
/// mismatch sentence rather than `target-outside-jar`.
pub fn game_hook_site_at(target: &MethodRef, anchor: &str) -> Option<&'static GameHookSite> {
    let anchor = if anchor.is_empty() { "HEAD" } else { anchor };
    let mut rows = every_game_hook_site().filter(|site| site_names(site, target));
    let first = rows.clone().next()?;
    Some(
        rows.find(|site| site.anchors.contains(&anchor))
            .unwrap_or(first),
    )
}

impl GameHookSite {
    /// Whether the vanilla method returns a value, so its callbacks take a
    /// `CallbackInfoReturnable`.
    pub fn returns_value(&self) -> bool {
        return_desc(self.target_descriptor) != "V"
    }

    /// Whether the row fires at the method's `HEAD` (otherwise at its
    /// `RETURN`/`TAIL`).
    pub fn fires_at_head(&self) -> bool {
        self.anchors.contains(&"HEAD")
    }

    /// Whether the row is one of the internal [`JAVA_TIER_SITES`].
    pub fn is_java_tier(&self) -> bool {
        self.hook.starts_with(JAVA_TIER_HOOK_PREFIX)
    }

    /// The callback-info type Mixin hands this site's callbacks.
    pub(crate) fn callback_info_descriptor(&self) -> &'static str {
        if self.returns_value() {
            CALLBACK_INFO_RETURNABLE_DESC
        } else {
            CALLBACK_INFO_DESC
        }
    }
}

/// What a callback left on the shared callback-info object of one firing:
/// its cancel flag and its `setReturnValue` override.
pub(crate) fn callback_info_snapshot(
    infos: &CallbackInfos,
    marker: &str,
) -> Result<(bool, Option<Value>), NativeError> {
    let state = infos.state(marker)?;
    Ok((state.cancelled, state.returned.clone()))
}

/// The value one firing makes the target return, or `None` when the target's
/// own value stands.
///
/// At a `HEAD` site a cancellation returns before the body: the callback's
/// `setReturnValue` value, else the descriptor's default (Mixin's
/// `CallbackInfoReturnable` answers `null`, `0` or `false` when nothing was
/// set). At a `RETURN`/`TAIL` site only an override changes the result; a
/// bare `cancel()` there returns what the method already computed. A `void`
/// site never returns a value.
pub(crate) fn game_hook_returned(
    site: &GameHookSite,
    infos: &CallbackInfos,
    marker: &str,
    cancelled: bool,
) -> Result<Option<Value>, NativeError> {
    if !site.returns_value() {
        return Ok(None);
    }
    let overridden = infos.take_override(marker, site.target_descriptor)?;
    if site.fires_at_head() && cancelled {
        return Ok(Some(
            overridden.unwrap_or_else(|| Value::default_for(site.target_descriptor)),
        ));
    }
    Ok(overridden)
}

/// Check what the host passes for the target's own result: a `RETURN`/`TAIL`
/// value site needs it (it is what `getReturnValue` answers), and every other
/// site must not get one.
pub(crate) fn check_actual_result(
    site: &GameHookSite,
    actual: Option<&Value>,
) -> Result<(), NativeError> {
    let wants = site.returns_value() && !site.fires_at_head();
    match (wants, actual) {
        (true, None) => Err(NativeError::Metadata(format!(
            "game hook point {} fires at the end of {}#{}{} and needs the method's result",
            site.hook, site.target_owner, site.target_name, site.target_descriptor
        ))),
        (false, Some(_)) => Err(NativeError::Metadata(format!(
            "game hook point {} has no result to hand its callbacks",
            site.hook
        ))),
        _ => Ok(()),
    }
}

/// The config whose lists decide one Mixin class on `side`: the first that
/// names it there and is not limited to the other side, as
/// [`mixin_config_gate`] picks it.
fn governing_config<'a>(
    configs: &'a [MixinConfig],
    config_sides: &BTreeMap<String, MixinSide>,
    class: &str,
    side: MixinSide,
) -> Option<&'a MixinConfig> {
    let binary = class.replace('/', ".");
    configs.iter().find(|config| {
        let here = match side {
            MixinSide::Client => &config.client,
            MixinSide::Server => &config.server,
        };
        (config.mixins.contains(&binary) || here.contains(&binary))
            && config_sides
                .get(&config.path)
                .is_none_or(|only| *only == side)
    })
}

impl NativeJar {
    /// The config gate for one game-hook plan.
    ///
    /// A config that names an `IMixinConfigPlugin` decides each of its Mixins
    /// in the plugin's own code, so the plain gate refuses every plan it
    /// governs (`config-plugin`). The game-hook tier asks the plugin instead:
    /// `plugins` holds one session per config, run once per weave the way
    /// Mixin runs a plugin (`mixin_plugins.rs`: a persistent instance,
    /// `onLoad`, `getRefMapperConfig`, `shouldApplyMixin` for every listed
    /// Mixin and target, `acceptTargets`, `getMixins`, then `preApply` and
    /// `postApply` with stand-in nodes). The plan passes only when that
    /// session ran and the plugin answered `true` for this Mixin at this
    /// target; a refused session, a failed question or a `false` keeps
    /// `config-plugin`. A companion Mixin a plugin's `getMixins()` added is
    /// listed by no config, and passes as Mixin applies it: without asking.
    ///
    /// The plugin is asked without the pack's mod list: a plugin that applies
    /// a compat Mixin only when another mod is loaded answers for this jar
    /// alone, so a pack can lose a compat callback, never gain one the plugin
    /// only allows alone (unless its answer is "apply when X is absent",
    /// which this documents).
    pub(crate) fn game_hook_config_gate(
        &self,
        configs: &[MixinConfig],
        config_sides: &BTreeMap<String, MixinSide>,
        plugins: &ConfigPlugins,
        mixin: &str,
        target: &str,
        side: MixinSide,
    ) -> Result<(), MixinSkipReason> {
        match mixin_config_gate(configs, config_sides, mixin, side) {
            Err(MixinSkipReason::ConfigPlugin) => {}
            Err(MixinSkipReason::NotInConfig) if plugins.is_companion(mixin) => return Ok(()),
            other => return other,
        }
        let config = governing_config(configs, config_sides, mixin, side)
            .ok_or(MixinSkipReason::ConfigPlugin)?;
        plugins.decide(&config.path, mixin, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The internal table can never shadow or impersonate a public hook: its
    /// names carry the prefix, are absent from the public table, fire at one
    /// real position each, and no two rows answer one target at one anchor.
    #[test]
    fn java_tier_sites_are_internal_and_answer_one_position_each() {
        let mut seen = BTreeMap::new();
        for site in java_tier_sites() {
            assert!(site.is_java_tier(), "{} lacks the java_ prefix", site.hook);
            assert!(
                GAME_HOOK_SITES
                    .iter()
                    .all(|public| public.hook != site.hook),
                "{} is also a public hook point",
                site.hook
            );
            assert!(
                site.anchors == ["HEAD"]
                    || site.anchors.iter().all(|a| matches!(*a, "RETURN" | "TAIL")),
                "{} mixes HEAD with RETURN/TAIL",
                site.hook
            );
            for anchor in site.anchors {
                let key = (
                    site.target_owner,
                    site.target_name,
                    site.target_descriptor,
                    *anchor,
                );
                if let Some(previous) = seen.insert(key, site.hook) {
                    panic!("{previous} and {} both answer {key:?}", site.hook);
                }
            }
        }
        for site in GAME_HOOK_SITES {
            assert!(
                !site.is_java_tier(),
                "{} uses the internal prefix",
                site.hook
            );
        }
    }

    /// A plan picks the row whose position it names, and a position no row
    /// fires falls back to the first row, so the mismatch names the site.
    #[test]
    fn a_plan_finds_the_row_of_its_anchor() {
        let tick = MethodRef {
            owner: "net/minecraft/server/level/ServerPlayer".into(),
            name: "tick".into(),
            descriptor: String::new(),
        };
        assert_eq!(
            game_hook_site_at(&tick, "TAIL").map(|s| s.hook),
            Some("player_tick")
        );
        assert_eq!(
            game_hook_site_at(&tick, "").map(|s| s.hook),
            Some("java_player_tick_head")
        );
        let base = MethodRef {
            owner: "net/minecraft/world/entity/player/Player".into(),
            ..tick.clone()
        };
        assert_eq!(
            game_hook_site_at(&base, "RETURN").map(|s| s.hook),
            Some("java_base_player_tick_tail")
        );
        assert_eq!(
            game_hook_site_at(&base, "INVOKE").map(|s| s.hook),
            Some("java_base_player_tick_head")
        );
        let unknown = MethodRef {
            name: "aiStep".into(),
            ..base
        };
        assert!(game_hook_site_at(&unknown, "HEAD").is_none());
    }
}
