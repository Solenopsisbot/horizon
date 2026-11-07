//! Fabric / NeoForge / Forge event listeners of the Java jars, fired from the
//! server's own points.
//!
//! A jar's entrypoint registers listeners (`ServerTickEvents.END_SERVER_TICK
//! .register(...)`, `NeoForge.EVENT_BUS.addListener(...)`...);
//! `horizon_native_java::events` names each one's event at bootstrap. The batch
//! hands every jar that registered one to [`adopt`], which keeps the parsed jar
//! and the host its entrypoints ran on (its statics are the mod's state), per
//! execution role, the way `java_payloads` keeps payload-handler jars.
//!
//! The server's points call the `java_*` methods below: start/started/stopping/
//! stopped (`integrated.rs`, `dedicated.rs`), the server and level tick
//! (`levels.rs`), a player joining and leaving (`lan.rs`), a living entity's
//! death (`creatures.rs`, `lan_actions.rs`). Each first asks [`listens`], one
//! relaxed atomic load: with no jar listening, nothing else runs.
//!
//! A listener that fails (a façade call the bounded surface does not answer,
//! its time budget, a bytecode the interpreter refuses) is disabled and logged
//! once with the reason; the game carries on as if it had not been there.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, AtomicU64, Ordering},
    },
    time::Duration,
};

use horizon_native_java::{
    ExecutionBudget, GameRuleValue, NativeJar, ServerFacts, StdHost,
    events::{
        EventContext, EventFacade, EventFiring, FacadeEntity, FacadeLevel, FacadePlayer, GameEvent,
        GameListener,
    },
};

use super::{
    Dim, Game,
    execution_role::ExecutionRole,
    java_payloads::{HandlerRole, ResidentHost, lock_resident},
};

/// One listener as the store keeps it.
struct Slot {
    listener: GameListener,
    /// Set after its first failure; a disabled listener never runs again.
    disabled: bool,
}

/// One jar with listeners: the jar, the host its entrypoints ran on (shared
/// with the jar's payload handlers and game hooks), and its listeners in
/// registration order.
struct EventJar {
    file: String,
    jar: Arc<NativeJar>,
    host: ResidentHost,
    slots: Vec<Slot>,
}

/// The listening jars of each execution role, in the batch's path order. Keyed
/// by role because the window and its server thread run different jars in one
/// process.
static STORE: Mutex<BTreeMap<HandlerRole, Vec<EventJar>>> = Mutex::new(BTreeMap::new());

/// The events any role has a listener for ([`GameEvent::bit`]): the fast path.
static LISTENED: AtomicU64 = AtomicU64::new(0);

/// `MinecraftServer.getTickCount()` for the façade: ticks since the server
/// started, counted at the tick-start point.
static SERVER_TICKS: AtomicI32 = AtomicI32::new(0);

/// The registries a role's server facts list, built once per server start:
/// thousands of ids that do not change while the server runs.
static REGISTRIES: Mutex<BTreeMap<HandlerRole, RegistryTable>> = Mutex::new(BTreeMap::new());

type RegistryTable = BTreeMap<String, Arc<std::collections::BTreeSet<String>>>;

/// The world folder each role's server opened, set when it starts (before
/// the level loads, as vanilla's storage access is).
static WORLD_DIRS: Mutex<BTreeMap<HandlerRole, std::path::PathBuf>> = Mutex::new(BTreeMap::new());

/// The vanilla registries the command registries hold besides blocks
/// (`Registries::ids`), which the server facts list for `registryAccess()`.
const FACT_REGISTRIES: &[&str] = &[
    "minecraft:item",
    "minecraft:entity_type",
    "minecraft:mob_effect",
    "minecraft:enchantment",
    "minecraft:worldgen/structure",
    "minecraft:worldgen/biome",
    "minecraft:attribute",
    "minecraft:damage_type",
    "minecraft:sound_event",
    "minecraft:recipe",
    "minecraft:advancement",
];

/// Listener runs and refusals since the process started, for the stop summary.
static RAN: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// The time one listener may take at one firing. Lifecycle listeners (start,
/// stop) load configs and are given longer; a tick listener runs every 50 ms
/// tick, so its bound keeps a looping one from stalling the server.
fn budget(event: GameEvent) -> Duration {
    match event {
        GameEvent::ServerStarting
        | GameEvent::ServerStarted
        | GameEvent::ServerStopping
        | GameEvent::ServerStopped
        | GameEvent::LevelLoad
        | GameEvent::LevelUnload => Duration::from_secs(2),
        _ => Duration::from_millis(20),
    }
}

pub(super) fn handler_role(role: ExecutionRole) -> HandlerRole {
    match role {
        ExecutionRole::Client => HandlerRole::Client,
        ExecutionRole::IntegratedServer => HandlerRole::IntegratedServer,
        ExecutionRole::DedicatedServer => HandlerRole::DedicatedServer,
    }
}

fn store() -> std::sync::MutexGuard<'static, BTreeMap<HandlerRole, Vec<EventJar>>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn recompute_mask(store: &BTreeMap<HandlerRole, Vec<EventJar>>) {
    let mask = store
        .values()
        .flatten()
        .flat_map(|jar| &jar.slots)
        .filter(|slot| !slot.disabled)
        .fold(0, |mask, slot| mask | slot.listener.event.bit());
    LISTENED.store(mask, Ordering::Relaxed);
}

/// Whether any jar of any role listens to `event`.
pub(super) fn listens(event: GameEvent) -> bool {
    LISTENED.load(Ordering::Relaxed) & event.bit() != 0
}

/// Forgets a previous bootstrap's listeners for one role.
pub(super) fn clear_as(role: HandlerRole) {
    let mut store = store();
    store.remove(&role);
    recompute_mask(&store);
}

/// Whether a jar's bootstrap registered a listener the game fires, so its run
/// needs the host kept (and must not be replaced by a cached replay). A client
/// role fires nothing (its window never runs a server), so it caches as before.
pub(super) fn has_listeners(
    jar: Option<&NativeJar>,
    host: Option<&StdHost>,
    role: ExecutionRole,
) -> bool {
    match (jar, host) {
        (Some(jar), Some(host)) if !role.is_client() => {
            !host.game_listeners(jar, true).listeners.is_empty()
        }
        _ => false,
    }
}

/// Keeps one bootstrapped jar's listeners for its role. Returns how many it
/// kept; a jar with none keeps nothing.
///
/// A copy of `host` becomes the jar's resident host; the batch shares one
/// with its other stores through [`adopt_resident`].
#[cfg(test)]
pub(super) fn adopt(
    role: ExecutionRole,
    path: &Path,
    jar: &Arc<NativeJar>,
    host: &StdHost,
) -> usize {
    adopt_resident(role, path, jar, &Arc::new(Mutex::new(host.clone())))
}

/// [`adopt`] keeping the jar's resident host itself: the host its entrypoints
/// ran on, so the listeners see the mod's statics, the objects its entrypoint
/// built and the seeded mod list, and share them with its other callbacks.
pub(super) fn adopt_resident(
    role: ExecutionRole,
    path: &Path,
    jar: &Arc<NativeJar>,
    host: &ResidentHost,
) -> usize {
    if role.is_client() {
        return 0;
    }
    let found = lock_resident(host).game_listeners(jar, true);
    let file = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    if !found.unsupported.is_empty() {
        let names: Vec<&str> = found.unsupported.keys().map(String::as_str).collect();
        log::debug!(
            "Horizon: Java mod {file} listens to events the game does not fire: {}",
            names.join(", ")
        );
    }
    if found.listeners.is_empty() {
        return 0;
    }
    let kept = found.listeners.len();
    let summary: Vec<String> = found
        .listeners
        .iter()
        .map(|l| format!("{} ({})", l.source, l.event.name()))
        .collect();
    log::info!(
        "Horizon: Java mod {file} registers {kept} event listener(s): {}",
        summary.join(", ")
    );
    let mut store = store();
    store.entry(handler_role(role)).or_default().push(EventJar {
        file,
        jar: Arc::clone(jar),
        host: Arc::clone(host),
        slots: found
            .listeners
            .into_iter()
            .map(|listener| Slot {
                listener,
                disabled: false,
            })
            .collect(),
    });
    recompute_mask(&store);
    kept
}

/// What firing one event did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Fired {
    /// Listeners that ran.
    pub(super) ran: usize,
    /// A listener cancelled the vanilla work.
    pub(super) cancelled: bool,
}

/// Runs every enabled listener of `firing.event` for `role`, jar by jar in
/// path order, each in registration order. A Fabric `boolean`/result callback
/// that decides stops the rest; a NeoForge cancellation is carried to the next
/// jar's listeners. A listener that fails is disabled and logged once.
pub(super) fn fire(role: HandlerRole, mut firing: EventFiring) -> Fired {
    let event = firing.event;
    let mut store = store();
    let Some(jars) = store.get_mut(&role) else {
        return Fired::default();
    };
    let mut fired = Fired::default();
    let mut disabled_any = false;
    'jars: for entry in jars.iter_mut() {
        let EventJar {
            file,
            jar,
            host,
            slots,
        } = entry;
        for slot in slots
            .iter_mut()
            .filter(|slot| !slot.disabled && slot.listener.event == event)
        {
            let limit = ExecutionBudget::within(budget(event));
            // A panic in the interpreter must not take the server tick with it.
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut host = lock_resident(host);
                jar.run_game_listener(&slot.listener, &mut firing, &mut host, &limit)
            }));
            match run {
                Ok(Ok(run)) => {
                    if run.ran {
                        fired.ran += 1;
                        RAN.fetch_add(1, Ordering::Relaxed);
                    }
                    if run.stop {
                        break 'jars;
                    }
                }
                Ok(Err(error)) => {
                    refuse(file, &mut slot.disabled, &slot.listener, &error.to_string());
                    disabled_any = true;
                }
                Err(_) => {
                    refuse(file, &mut slot.disabled, &slot.listener, "panicked");
                    disabled_any = true;
                }
            }
        }
    }
    if disabled_any {
        recompute_mask(&store);
    }
    fired.cancelled = firing.cancelled;
    fired
}

/// Disables one listener and says why, once.
fn refuse(file: &str, disabled: &mut bool, listener: &GameListener, reason: &str) {
    *disabled = true;
    REFUSED.fetch_add(1, Ordering::Relaxed);
    let target = listener.target();
    let mut reason = reason.replace(['\n', '\r'], " ");
    if reason.chars().count() > 240 {
        reason = reason.chars().take(239).collect();
        reason.push('…');
    }
    log::warn!(
        "Horizon: Java mod {file} listener {}#{} on {} ({}) refused and disabled: {reason}",
        target.owner,
        target.name,
        listener.source,
        listener.event.name()
    );
}

/// Listeners kept, enabled and disabled across every role, plus process-wide
/// runs and refusals: `(kept, disabled, ran, refused)`.
pub(super) fn stats() -> (usize, usize, u64, u64) {
    let store = store();
    let slots = || store.values().flatten().flat_map(|jar| &jar.slots);
    (
        slots().count(),
        slots().filter(|slot| slot.disabled).count(),
        RAN.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
    )
}

impl Game<'_> {
    /// The role whose listeners this `Game` fires, or `None` for one that is
    /// not a server (a window, a mirror of a server).
    fn java_event_role(&self) -> Option<HandlerRole> {
        if self.remote.is_some() {
            return None;
        }
        let server = self.serving.is_some()
            || self.options.dedicated.is_some()
            || !self.options.execution_role.is_client();
        server.then(|| handler_role(self.options.execution_role))
    }

    /// The façade's snapshot of this server: its tick count, its players and
    /// its levels. Built only when a listener is about to run. `subject` is a
    /// player the event is about who is not (or no longer) in the hub's list:
    /// one leaving, or one whose turn has the hub taken out.
    pub(super) fn java_event_facade(&self, subject: Option<FacadePlayer>) -> Arc<EventFacade> {
        let (mut players, max_players): (Vec<FacadePlayer>, i32) =
            self.lan.as_ref().map_or((Vec::new(), 20), |host| {
                let players = host
                    .hub
                    .guests
                    .iter()
                    .filter(|g| g.placed)
                    .map(facade_player)
                    .collect();
                (players, host.hub.config.max_players as i32)
            });
        if let Some(subject) = subject
            && !players.iter().any(|p| p.uuid == subject.uuid)
        {
            players.push(subject);
        }
        // Every level shares the overworld's clock, as vanilla's derived
        // levels do.
        let levels = [Dim::Overworld, Dim::Nether, Dim::End]
            .into_iter()
            .map(|dim| FacadeLevel {
                id: dim.id().to_owned(),
                game_time: self.world.game_time,
                day_time: self.world.day_time,
            })
            .collect();
        Arc::new(EventFacade {
            tick_count: SERVER_TICKS.load(Ordering::Relaxed),
            dedicated: self.options.dedicated.is_some(),
            max_players,
            players,
            levels,
            server: self.java_server_facts(),
        })
    }

    /// What a Java callback may ask about the server as a whole
    /// (`horizon_native_java::ServerFacts`): the world folder in the
    /// interpreter's virtual game folder, the game rules now, the registries
    /// the server's commands see, and the payload channels each player's
    /// client takes. Built only when a callback is about to run; the
    /// registries are built once per server start.
    pub(super) fn java_server_facts(&self) -> Arc<ServerFacts> {
        let rules = &self.ui.rules;
        let game_rules = rules
            .available_rules()
            .filter_map(|rule| {
                let value = match rules.get(rule.name)? {
                    horizon_client::gamerules::Value::Bool(b) => GameRuleValue::Bool(b),
                    horizon_client::gamerules::Value::Int(i) => GameRuleValue::Int(i),
                };
                Some((rule.name.to_owned(), value))
            })
            .collect();
        Arc::new(ServerFacts {
            world_dir: WORLD_DIRS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&handler_role(self.options.execution_role))
                .and_then(|dir| virtual_world_dir(dir))
                .or_else(|| virtual_world_dir(&self.world_root)),
            game_rules,
            registries: self.java_fact_registries(),
            client_channels: self.java_client_channels(),
            game_time: Some(self.world.game_time),
            day_time: Some(self.world.day_time),
        })
    }

    /// The registries for [`Self::java_server_facts`], from the command
    /// registries the world opened with, cached per role.
    fn java_fact_registries(&self) -> RegistryTable {
        let role = handler_role(self.options.execution_role);
        let mut cache = REGISTRIES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(table) = cache.get(&role) {
            return table.clone();
        }
        let reg = &self.ui.registries;
        let mut table = RegistryTable::new();
        let add = |table: &mut RegistryTable,
                   name: &str,
                   ids: &mut dyn Iterator<Item = &String>| {
            let ids: std::collections::BTreeSet<String> = ids.map(|id| namespaced(id)).collect();
            if !ids.is_empty() {
                table
                    .entry(name.to_owned())
                    .or_insert_with(|| Arc::new(ids));
            }
        };
        add(&mut table, "minecraft:block", &mut reg.blocks.keys());
        for name in FACT_REGISTRIES
            .iter()
            .copied()
            .chain(reg.by_registry.keys().map(String::as_str))
        {
            add(&mut table, name, &mut reg.ids(name).iter());
        }
        if !table.is_empty() {
            cache.insert(role, table.clone());
            return table;
        }
        // Before the world opens (`SERVER_STARTING`) the command registries
        // are not built yet, but vanilla's registries already are: list the
        // static ones from the resources, uncached, so the full table is
        // built once the world has opened.
        let items: std::collections::BTreeSet<String> = self
            .res
            .items()
            .order
            .iter()
            .map(|id| namespaced(id))
            .collect();
        let blocks: std::collections::BTreeSet<String> = (0..self.pack.assets.registry.len())
            .filter_map(|id| {
                let id = u32::try_from(id).ok()?;
                let state = self
                    .pack
                    .assets
                    .registry
                    .state(horizon_world::registry::BlockStateId(id))?;
                Some(namespaced(&state.name))
            })
            .collect();
        let entities: std::collections::BTreeSet<String> = self
            .res
            .mob_types()
            .keys()
            .map(|id| namespaced(id))
            .chain(["minecraft:player".to_owned()])
            .collect();
        for (name, ids) in [
            ("minecraft:item", items),
            ("minecraft:block", blocks),
            ("minecraft:entity_type", entities),
        ] {
            if !ids.is_empty() {
                table.insert(name.to_owned(), Arc::new(ids));
            }
        }
        table
    }

    /// Per placed guest, the payload channels its client takes: every
    /// declared client-bound channel for a Horizon client (the transport
    /// carries them on `horizon:payload`), none for any other client, which
    /// declared no channel to this server.
    fn java_client_channels(&self) -> BTreeMap<u128, std::collections::BTreeSet<String>> {
        let Some(host) = self.lan.as_ref() else {
            return BTreeMap::new();
        };
        let horizon: Vec<u128> = host
            .hub
            .guests
            .iter()
            .filter(|g| g.placed && g.brand == "horizon")
            .map(|g| g.uuid)
            .collect();
        if horizon.is_empty() {
            return BTreeMap::new();
        }
        let channels: std::collections::BTreeSet<String> =
            super::java_payloads::client_bound_channels();
        horizon
            .into_iter()
            .map(|uuid| (uuid, channels.clone()))
            .collect()
    }

    /// Fires `event` with `context` when a jar listens to it.
    fn java_event(&mut self, event: GameEvent, context: EventContext) -> Fired {
        self.java_event_about(event, context, None)
    }

    /// [`Self::java_event`] about `subject`, a player the hub may not list.
    fn java_event_about(
        &mut self,
        event: GameEvent,
        context: EventContext,
        subject: Option<FacadePlayer>,
    ) -> Fired {
        if !listens(event) {
            return Fired::default();
        }
        let Some(role) = self.java_event_role() else {
            return Fired::default();
        };
        let firing = EventFiring::new(event, self.java_event_facade(subject), context);
        fire(role, firing)
    }

    /// `MinecraftServer.runServer`'s start, before the levels load (Fabric
    /// `SERVER_STARTING`, NeoForge `ServerAboutToStartEvent`).
    ///
    /// `world_dir` is the world folder the server is about to open: vanilla's
    /// storage access exists before the server starts, so `getWorldPath`
    /// answers from here on, before the level loads.
    pub(super) fn java_server_starting(&mut self, world_dir: &Path) {
        SERVER_TICKS.store(0, Ordering::Relaxed);
        let role = handler_role(self.options.execution_role);
        REGISTRIES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&role);
        WORLD_DIRS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(role, world_dir.to_path_buf());
        self.java_event(GameEvent::ServerStarting, EventContext::default());
    }

    /// The levels loaded and the server is ready, before its first tick.
    pub(super) fn java_server_started(&mut self) {
        self.java_event(GameEvent::ServerStarted, EventContext::default());
    }

    /// `MinecraftServer.stopServer`'s start, everyone still here.
    pub(super) fn java_server_stopping(&mut self) {
        self.java_event(GameEvent::ServerStopping, EventContext::default());
    }

    /// After the final save; logs what the listeners did this session.
    pub(super) fn java_server_stopped(&mut self) {
        self.java_event(GameEvent::ServerStopped, EventContext::default());
        let (kept, disabled, ran, refused) = stats();
        if kept > 0 {
            log::info!(
                "Horizon: Java event listeners: {kept} kept ({disabled} disabled), {ran} run(s), {refused} refusal(s)"
            );
        }
    }

    /// One server tick's start (`start`) or end. The tick counter advances at
    /// the start whether or not anything listens: one relaxed add.
    pub(super) fn java_server_tick(&mut self, start: bool) {
        if start {
            SERVER_TICKS.fetch_add(1, Ordering::Relaxed);
        }
        let event = if start {
            GameEvent::ServerTickStart
        } else {
            GameEvent::ServerTickEnd
        };
        self.java_event(event, EventContext::default());
    }

    /// One level's tick start (`start`) or end.
    pub(super) fn java_level_tick(&mut self, dimension: &str, start: bool) {
        let event = if start {
            GameEvent::LevelTickStart
        } else {
            GameEvent::LevelTickEnd
        };
        if !listens(event) {
            return;
        }
        self.java_event(
            event,
            EventContext {
                level: Some(dimension.to_owned()),
                ..EventContext::default()
            },
        );
    }

    /// A player finished joining (Fabric `ServerPlayConnectionEvents.JOIN` and
    /// `ServerPlayerEvents.JOIN`, NeoForge `PlayerLoggedInEvent`).
    pub(super) fn java_player_joined(&mut self, player: u128) {
        if !listens(GameEvent::PlayerJoin) {
            return;
        }
        let subject = self.lan.as_ref().and_then(|host| {
            host.hub
                .guests
                .iter()
                .find(|g| g.uuid == player)
                .map(facade_player)
        });
        if let Some(subject) = subject {
            let context = EventContext {
                level: Some(subject.level.clone()),
                player: Some(subject.uuid),
                ..EventContext::default()
            };
            self.java_event_about(GameEvent::PlayerJoin, context, Some(subject));
        }
    }

    /// A player left, by any path; `guest` may already be out of the hub.
    pub(super) fn java_player_left(&mut self, guest: &horizon_client::lan::guest::Guest) {
        self.java_player_event(GameEvent::PlayerLeave, guest);
    }

    fn java_player_event(&mut self, event: GameEvent, guest: &horizon_client::lan::guest::Guest) {
        if !listens(event) {
            return;
        }
        let subject = facade_player(guest);
        let context = EventContext {
            level: Some(subject.level.clone()),
            player: Some(subject.uuid),
            ..EventContext::default()
        };
        self.java_event_about(event, context, Some(subject));
    }

    /// The players still here leave with the server: once, before
    /// `native_players_leaving` marks them told.
    pub(super) fn java_players_leaving(&mut self) {
        if !listens(GameEvent::PlayerLeave) {
            return;
        }
        let Some(host) = self.lan.as_ref() else {
            return;
        };
        if host.players_told_left {
            return;
        }
        let players: Vec<FacadePlayer> = host
            .hub
            .guests
            .iter()
            .filter(|g| g.placed)
            .map(facade_player)
            .collect();
        for subject in players {
            let context = EventContext {
                level: Some(subject.level.clone()),
                player: Some(subject.uuid),
                ..EventContext::default()
            };
            self.java_event_about(GameEvent::PlayerLeave, context, Some(subject));
        }
    }

    /// A living entity is about to die (`LivingEntity.die`): Fabric
    /// `ALLOW_DEATH` and NeoForge `LivingDeathEvent` may keep it alive; if none
    /// does, Fabric's `AFTER_DEATH` hears of it. `player` is set for a player,
    /// `entity` (id, type) otherwise. Returns whether the death is cancelled.
    pub(super) fn java_living_death(
        &mut self,
        player: Option<&horizon_client::lan::guest::Guest>,
        entity: Option<(i32, &str)>,
        damage_type: &str,
    ) -> bool {
        let allow = listens(GameEvent::LivingAllowDeath);
        let after = listens(GameEvent::LivingAfterDeath);
        if !allow && !after {
            return false;
        }
        let subject = player.map(facade_player);
        let level = subject
            .as_ref()
            .map_or_else(|| self.dim.id().to_owned(), |p| p.level.clone());
        let context = EventContext {
            level: Some(level.clone()),
            player: subject.as_ref().map(|p| p.uuid),
            entity: entity.map(|(id, kind)| FacadeEntity {
                id,
                kind: kind.to_owned(),
                level,
            }),
            damage: Some(damage_type.to_owned()),
            amount: 0.0,
        };
        if allow
            && self
                .java_event_about(
                    GameEvent::LivingAllowDeath,
                    context.clone(),
                    subject.clone(),
                )
                .cancelled
        {
            return true;
        }
        if after {
            self.java_event_about(GameEvent::LivingAfterDeath, context, subject);
        }
        false
    }
}

/// A guest as the façade shows them.
/// A registry id with its namespace (`stone` → `minecraft:stone`).
fn namespaced(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// The world folder as the interpreter's virtual file system spells it: the
/// game folder (the working directory) is `/horizon/game`, the root
/// `FabricLoader.getGameDir` answers. A world outside the game folder has no
/// such path and the façade refuses `getWorldPath`.
fn virtual_world_dir(world_root: &Path) -> Option<String> {
    let relative = if world_root.is_absolute() {
        let cwd = std::env::current_dir().ok()?;
        world_root.strip_prefix(cwd).ok()?.to_path_buf()
    } else {
        world_root.to_path_buf()
    };
    let mut parts = Vec::new();
    for part in relative.components() {
        match part {
            std::path::Component::Normal(name) => parts.push(name.to_str()?.to_owned()),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!("/horizon/game/{}", parts.join("/")))
}

fn facade_player(guest: &horizon_client::lan::guest::Guest) -> FacadePlayer {
    FacadePlayer {
        uuid: guest.uuid,
        name: guest.name.clone(),
        level: guest.dimension.clone(),
        entity_id: guest.entity_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_native_java::{Host, Value};
    use std::{io::Write, path::PathBuf, process::Command};

    /// Every role forgotten, so a test starts from an empty store.
    fn clear_all() {
        for role in [
            HandlerRole::Client,
            HandlerRole::IntegratedServer,
            HandlerRole::DedicatedServer,
        ] {
            clear_as(role);
        }
    }

    /// A Fabric jar whose `onInitialize` is `body`, built with `javac` against
    /// stub API classes that stay out of the jar (the host answers them).
    /// `None` without a Java toolchain.
    fn fabric_jar(name: &str, body: &str) -> Option<PathBuf> {
        let root =
            std::env::temp_dir().join(format!("horizon-java-events-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stubs = [
            (
                "net/fabricmc/fabric/api/event/Event.java",
                "package net.fabricmc.fabric.api.event;\n\
                 public abstract class Event<T> { public void register(T listener) {} }",
            ),
            (
                "net/minecraft/server/MinecraftServer.java",
                "package net.minecraft.server;\n\
                 public class MinecraftServer { public int getTickCount() { return 0; }\n\
                 public Object getCommands() { return null; } }",
            ),
            (
                "net/fabricmc/fabric/api/event/lifecycle/v1/ServerTickEvents.java",
                "package net.fabricmc.fabric.api.event.lifecycle.v1;\n\
                 public final class ServerTickEvents {\n\
                   public static final net.fabricmc.fabric.api.event.Event<EndTick> END_SERVER_TICK = null;\n\
                   public interface EndTick { void onEndTick(net.minecraft.server.MinecraftServer s); } }",
            ),
        ];
        let mut sources = Vec::new();
        for (path, text) in stubs {
            let file = root.join("src").join(path);
            std::fs::create_dir_all(file.parent()?).ok()?;
            std::fs::write(&file, text).ok()?;
            sources.push(file);
        }
        let main = root.join("src/demo/EventMod.java");
        std::fs::create_dir_all(main.parent()?).ok()?;
        std::fs::write(
            &main,
            format!(
                "package demo;\n\
                 import net.fabricmc.fabric.api.event.lifecycle.v1.ServerTickEvents;\n\
                 public final class EventMod {{\n\
                   public static int ticks;\n\
                   public static int lastTick;\n\
                   public void onInitialize() {{ {body} }} }}"
            ),
        )
        .ok()?;
        sources.push(main);
        let javac = std::env::var_os("JAVA_HOME")
            .map(|home| PathBuf::from(home).join("bin/javac"))
            .unwrap_or_else(|| PathBuf::from("javac"));
        let classes = root.join("classes");
        let status = Command::new(javac)
            .args(["--release", "17", "-d"])
            .arg(&classes)
            .args(&sources)
            .status()
            .ok()?;
        if !status.success() {
            return None;
        }
        let jar = root.join(format!("{name}.jar"));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).ok()?;
        zip.write_all(
            format!(
                r#"{{"id":"{name}","version":"1","entrypoints":{{"main":["demo.EventMod"]}}}}"#
            )
            .as_bytes(),
        )
        .ok()?;
        for entry in std::fs::read_dir(classes.join("demo")).ok()? {
            let path = entry.ok()?.path();
            let name = format!("demo/{}", path.file_name()?.to_string_lossy());
            zip.start_file(name, options).ok()?;
            zip.write_all(&std::fs::read(&path).ok()?).ok()?;
        }
        zip.finish().ok()?;
        Some(jar)
    }

    /// Runs `path`'s entrypoint and keeps its listeners for `role`.
    fn bootstrap(path: &Path, role: ExecutionRole) -> usize {
        let jar = NativeJar::from_path(path).unwrap();
        let entry = jar.discover_entrypoint("main").unwrap();
        let mut host = StdHost::new();
        jar.execute(&entry.class, entry.method.as_deref(), "main", &mut host)
            .unwrap();
        adopt(role, path, &Arc::new(jar), &host)
    }

    /// A static of the first kept jar of `role` (`key` is name + descriptor).
    fn kept_static(role: HandlerRole, key: &str) -> Option<Value> {
        lock_resident(&store().get(&role)?.first()?.host)
            .persistent_class_state()
            .0
            .remove(&format!("demo/EventMod#{key}"))
    }

    fn tick_end(tick_count: i32) -> EventFiring {
        EventFiring::new(
            GameEvent::ServerTickEnd,
            Arc::new(EventFacade {
                tick_count,
                ..EventFacade::default()
            }),
            EventContext::default(),
        )
    }

    #[test]
    fn the_world_folder_is_spelled_inside_the_virtual_game_folder() {
        let cwd = std::env::current_dir().unwrap();
        // The dedicated server's absolute `<game>/world`, and single
        // player's relative `saves/<folder>`.
        assert_eq!(
            virtual_world_dir(&cwd.join("world")).as_deref(),
            Some("/horizon/game/world")
        );
        assert_eq!(
            virtual_world_dir(Path::new("./saves/New World")).as_deref(),
            Some("/horizon/game/saves/New World")
        );
        // Outside the game folder, or no folder at all: no path to answer.
        assert_eq!(virtual_world_dir(Path::new("/elsewhere/world")), None);
        assert_eq!(virtual_world_dir(Path::new("")), None);
        assert_eq!(virtual_world_dir(Path::new("../world")), None);
    }

    #[test]
    fn nothing_listens_until_a_jar_is_adopted() {
        let _guard = super::super::java_mods::test_lock();
        clear_all();
        assert!(GameEvent::ALL.iter().all(|event| !listens(*event)));
        assert_eq!(
            fire(HandlerRole::DedicatedServer, tick_end(1)),
            Fired::default()
        );
    }

    #[test]
    fn a_refused_listener_is_disabled_and_its_neighbour_keeps_running() {
        let _guard = super::super::java_mods::test_lock();
        clear_all();
        let Some(path) = fabric_jar(
            "refusal",
            "ServerTickEvents.END_SERVER_TICK.register(s -> s.getCommands());\n\
             ServerTickEvents.END_SERVER_TICK.register(s -> { ticks++; lastTick = s.getTickCount(); });",
        ) else {
            eprintln!("skipped: no javac");
            return;
        };
        assert_eq!(bootstrap(&path, ExecutionRole::DedicatedServer), 2);
        assert!(listens(GameEvent::ServerTickEnd));
        // A client never fires server events, so it keeps nothing.
        assert_eq!(bootstrap(&path, ExecutionRole::Client), 0);

        let (_, _, ran_before, refused_before) = stats();
        // First firing: the first listener refuses (`getCommands` is not in the
        // façade), the second still runs.
        assert_eq!(fire(HandlerRole::DedicatedServer, tick_end(41)).ran, 1);
        assert_eq!(fire(HandlerRole::DedicatedServer, tick_end(42)).ran, 1);
        let (kept, disabled, ran, refused) = stats();
        assert_eq!((kept, disabled), (2, 1));
        assert_eq!((ran - ran_before, refused - refused_before), (2, 1));
        assert_eq!(
            kept_static(HandlerRole::DedicatedServer, "ticksI"),
            Some(Value::Int(2))
        );
        assert_eq!(
            kept_static(HandlerRole::DedicatedServer, "lastTickI"),
            Some(Value::Int(42))
        );
        // Another role's server does not run this role's listeners.
        assert_eq!(fire(HandlerRole::IntegratedServer, tick_end(43)).ran, 0);
        clear_as(HandlerRole::DedicatedServer);
        assert!(!listens(GameEvent::ServerTickEnd));
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap_or(&path));
    }

    /// The real tick point: `tick_server` on a dedicated server's `Game` fires
    /// `END_SERVER_TICK` once per tick, with the server's tick count.
    #[test]
    #[ignore = "needs prepared resources in HORIZON_TEST_RESOURCES"]
    fn the_server_tick_fires_end_server_tick_once_per_tick() {
        let _guard = super::super::java_mods::test_lock();
        clear_all();
        let Some(path) = fabric_jar(
            "tick-point",
            "ServerTickEvents.END_SERVER_TICK.register(s -> { ticks++; lastTick = s.getTickCount(); });",
        ) else {
            eprintln!("skipped: no javac");
            return;
        };
        assert_eq!(bootstrap(&path, ExecutionRole::DedicatedServer), 1);
        let root = PathBuf::from(
            std::env::var_os("HORIZON_TEST_RESOURCES")
                .expect("set HORIZON_TEST_RESOURCES to prepared resources"),
        );
        let settings = super::super::Settings::default();
        let loaded = super::super::load_resources(
            root.clone(),
            None,
            &root,
            &settings,
            std::time::Instant::now(),
            false,
            true,
            &super::super::Progress::new(),
            &mut |_, _| unreachable!(),
        )
        .unwrap();
        let mut options = super::super::Options::parse(std::iter::empty())
            .unwrap()
            .unwrap();
        options.resources = root.clone();
        options.execution_role = ExecutionRole::DedicatedServer;
        let res: &'static _ = Box::leak(Box::new(loaded.resources));
        let mut game = Game::new(
            res,
            loaded.pack,
            loaded.gui.unwrap(),
            options,
            settings,
            super::super::Audio::silent(&root),
            None,
        )
        .unwrap();
        game.options.dedicated = Some(Vec::new());
        SERVER_TICKS.store(0, Ordering::Relaxed);
        for _ in 0..3 {
            game.tick_server(false).unwrap();
        }
        assert_eq!(
            kept_static(HandlerRole::DedicatedServer, "ticksI"),
            Some(Value::Int(3))
        );
        assert_eq!(
            kept_static(HandlerRole::DedicatedServer, "lastTickI"),
            Some(Value::Int(3))
        );
        // A paused pass ticks nothing, so nothing fires.
        game.tick_server(true).unwrap();
        assert_eq!(
            kept_static(HandlerRole::DedicatedServer, "ticksI"),
            Some(Value::Int(3))
        );
        clear_as(HandlerRole::DedicatedServer);
    }
}
