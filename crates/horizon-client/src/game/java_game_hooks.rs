//! Fires the opt-in Java game-hook tier from the game's own hook points.
//!
//! With `HORIZON_JAVA_GAME_HOOKS=1`, a Java jar's `@Inject` on a Minecraft
//! method that `horizon_native_java::GAME_HOOK_SITES` maps onto a host hook
//! point is bound at bootstrap (`java_mods.rs`) and kept here, per execution
//! role. The game's own hook points (`native_hooks.rs`, plus the two that run
//! without a `Game`: a jump and a knockback) call the `java_hook_*` methods
//! below, which describe the hook's objects to the interpreter and run every
//! bound callback in Mixin apply order.
//!
//! The rules a firing follows:
//!
//! - **Zero cost when nothing is bound.** Each role keeps a bitmask of the
//!   hooks any of its jars still runs; a hook point reads one atomic and
//!   returns before it builds a single value.
//! - **The vanilla cancel.** At a site whose contract cancels, a callback that
//!   calls `CallbackInfo.cancel()` sets the hook's own cancel flag, and the
//!   native mods' dispatch is skipped, because Mixin's cancel at `HEAD` returns
//!   before the method body that posts their event.
//! - **Failure is per jar and hook.** A callback that throws, runs past its
//!   budget or calls something the model refuses (`game_hooks.rs` in
//!   horizon-native-java) is logged once, with the jar and the cause, and that
//!   jar's callbacks at that hook stop firing. The game and every other jar
//!   keep running.
//! - **Identity.** A player is one object from login (or respawn) to logout:
//!   its marker carries a generation that a respawn's `restoreFrom` and a
//!   logout move on, and the fields a Mixin added to the old player are
//!   dropped then, as the old Java object would be.

use super::Game;
use super::execution_role::ExecutionRole;
use super::java_payloads::{HandlerRole, ResidentHost, lock_resident};
use super::native_server::NativeServerPlayer;
#[cfg(test)]
use horizon_native_java::StdHost;
use horizon_native_java::{
    ExecutionBudget, GAME_HOOK_SITES, GameHookContext, GameHookEffect, GameHookRegistrations,
    GameHookState, GameObject, NativeJar, Value,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

/// The wall-clock bound of one firing of one jar's callbacks. A callback that
/// runs this long on the server thread is broken; it is disabled like one that
/// throws.
const CALLBACK_BUDGET: Duration = Duration::from_millis(250);

/// One jar whose game-class callbacks the opt-in hook tier bound.
///
/// The jar and its host stay alive because [`NativeJar::run_game_hook_in`]
/// resolves a callback from the jar's own classes and lets it write through
/// the host's persistent class state, so a later firing sees what an earlier
/// one wrote; `state` keeps the fields its Mixin classes add to game objects.
struct JavaGameHookJar {
    /// The jar's file name, for the log.
    label: String,
    jar: Arc<NativeJar>,
    registrations: GameHookRegistrations,
    /// Shared with the jar's payload handlers and event listeners.
    host: ResidentHost,
    state: GameHookState,
    /// Hooks whose callbacks failed once and no longer fire for this jar.
    disabled: BTreeSet<String>,
    /// Successful firings per hook.
    fired: BTreeMap<String, u64>,
}

/// The game-class callbacks one execution role's batch bound, in path order.
/// Keyed by role because the window and its server thread run different jars
/// in the same process, exactly as the payload handler jars are.
static JAVA_GAME_HOOKS: Mutex<BTreeMap<HandlerRole, Vec<JavaGameHookJar>>> =
    Mutex::new(BTreeMap::new());

/// Per role, one bit per hook name (its first row in `GAME_HOOK_SITES`) that
/// any of the role's jars still fires. Read before anything else at every hook
/// point, so a game with no bound jar pays one relaxed load.
static BOUND: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// Player generations, per role: a respawn or a logout makes the next player
/// object a new one, as a new `ServerPlayer` is.
static PLAYER_GENERATIONS: Mutex<BTreeMap<(HandlerRole, u128), u32>> = Mutex::new(BTreeMap::new());

/// Keys for objects the host has no stable id for (the dropped item a pickup
/// names): each firing gets a fresh one.
static NEXT_ANONYMOUS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    /// The role of the `Game` that last ticked on this thread, for the hook
    /// points that run without one (a jump, a knockback). The window and the
    /// single player server tick on different threads.
    static THREAD_ROLE: Cell<Option<HandlerRole>> = const { Cell::new(None) };
}

/// What one firing of a host hook point did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct JavaGameHookOutcome {
    /// How many callbacks ran; a cancellation stops the ones after it.
    pub(super) ran: usize,
    /// Whether a callback cancelled the vanilla work at this point.
    pub(super) cancelled: bool,
    /// Jars whose callbacks failed at this firing (and are now disabled here).
    pub(super) failed: usize,
}

fn role_index(role: HandlerRole) -> usize {
    match role {
        HandlerRole::Client => 0,
        HandlerRole::IntegratedServer => 1,
        HandlerRole::DedicatedServer => 2,
    }
}

/// The hook's bit: the index of its first row in the site table.
fn hook_bit(hook: &str) -> Option<u32> {
    GAME_HOOK_SITES
        .iter()
        .position(|site| site.hook == hook)
        .map(|index| 1 << index)
}

fn refresh_mask(role: HandlerRole, jars: &[JavaGameHookJar]) {
    let mut mask = 0;
    for jar in jars {
        for hook in jar.registrations.hooks() {
            if !jar.disabled.contains(hook) {
                mask |= hook_bit(hook).unwrap_or(0);
            }
        }
    }
    BOUND[role_index(role)].store(mask, Ordering::Relaxed);
}

pub(super) fn handler_role_of(role: ExecutionRole) -> HandlerRole {
    match role {
        ExecutionRole::Client => HandlerRole::Client,
        ExecutionRole::IntegratedServer => HandlerRole::IntegratedServer,
        ExecutionRole::DedicatedServer => HandlerRole::DedicatedServer,
    }
}

/// Forgets the game-class callbacks of a previous bootstrap for one role.
pub(super) fn clear_java_game_hooks_as(role: HandlerRole) {
    let mut store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    store.remove(&role);
    BOUND[role_index(role)].store(0, Ordering::Relaxed);
    PLAYER_GENERATIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(r, _), _| *r != role);
}

/// Keeps one jar's bound callbacks for its role's host hook points, on a copy
/// of `host`; the batch shares the jar's one resident host with its other
/// stores through [`publish_java_game_hooks_resident`].
#[cfg(test)]
pub(super) fn publish_java_game_hooks(
    role: HandlerRole,
    path: &Path,
    jar: Arc<NativeJar>,
    registrations: GameHookRegistrations,
    host: &StdHost,
) {
    publish_java_game_hooks_resident(
        role,
        path,
        jar,
        registrations,
        &Arc::new(Mutex::new(host.clone())),
    );
}

/// [`publish_java_game_hooks`] keeping the jar's resident host itself: the
/// host its entrypoints ran on, so its statics, the objects its entrypoint
/// built and the seeded mod class path are the callbacks', shared with its
/// payload handlers and listeners. A firing runs it strict: a call the
/// bootstrap host would answer with a neutral value fails the callback
/// instead, so a callback never "succeeds" on a value the game never produced.
pub(super) fn publish_java_game_hooks_resident(
    role: HandlerRole,
    path: &Path,
    jar: Arc<NativeJar>,
    registrations: GameHookRegistrations,
    host: &ResidentHost,
) {
    if registrations.is_empty() {
        return;
    }
    let host = Arc::clone(host);
    let mut store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let jars = store.entry(role).or_default();
    jars.push(JavaGameHookJar {
        label: path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        jar,
        registrations,
        host,
        state: GameHookState::default(),
        disabled: BTreeSet::new(),
        fired: BTreeMap::new(),
    });
    refresh_mask(role, jars);
}

/// Whether any jar of `role` still fires `hook`: one atomic load.
pub(super) fn java_game_hook_bound(role: HandlerRole, hook: &str) -> bool {
    let mask = BOUND[role_index(role)].load(Ordering::Relaxed);
    mask != 0 && hook_bit(hook).is_some_and(|bit| mask & bit != 0)
}

/// Runs every callback the role bound at one host hook point, in registration
/// order (Mixin apply order).
///
/// A callback that cancels stops the ones after it, including those of later
/// jars, which is what Mixin's `CallbackInfo.cancel` does at one injection
/// point. A jar whose callbacks fail is logged once and disabled at this hook;
/// the firing goes on with the next jar.
pub(super) fn run_java_game_hook(
    role: HandlerRole,
    hook: &str,
    context: &mut GameHookContext,
    receiver: Option<Value>,
    args: &[Value],
) -> JavaGameHookOutcome {
    let mut outcome = JavaGameHookOutcome::default();
    if !java_game_hook_bound(role, hook) {
        return outcome;
    }
    let mut store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(jars) = store.get_mut(&role) else {
        return outcome;
    };
    let mut changed = false;
    for entry in jars.iter_mut().filter(|entry| {
        !entry.disabled.contains(hook) && !entry.registrations.entries(hook).is_empty()
    }) {
        let budget = ExecutionBudget::within(CALLBACK_BUDGET);
        let JavaGameHookJar {
            jar,
            registrations,
            host,
            state,
            ..
        } = entry;
        // The interpreter returns its errors; the unwind guard only keeps a
        // bug in it from taking the server thread down with the jar.
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Strict for this firing only: the same host runs the jar's
            // lenient listeners and payload handlers. What the callbacks
            // logged is rolled back, so a hook fired every tick does not
            // grow the call log.
            let mut host = lock_resident(host);
            host.set_strict_unknown_host_calls(true);
            let mark = host.call_log_mark();
            let run = jar.run_game_hook_in(
                registrations,
                state,
                context,
                hook,
                receiver.clone(),
                args,
                &mut *host,
                &budget,
            );
            host.rewind_call_log(mark);
            host.set_strict_unknown_host_calls(false);
            run
        }));
        let error = match run {
            Ok(Ok(run)) => {
                let fired = entry.fired.entry(hook.to_owned()).or_default();
                *fired += 1;
                if *fired == 1 {
                    log::info!(
                        "Horizon: Java game hook {hook} fired {} callback(s) of {} (first firing)",
                        run.ran,
                        entry.label
                    );
                }
                outcome.ran += run.ran;
                if run.cancelled {
                    outcome.cancelled = true;
                    break;
                }
                continue;
            }
            Ok(Err(error)) => error.to_string(),
            Err(_) => "panicked".to_owned(),
        };
        let error: String = error.replace(['\n', '\r'], " ").chars().take(300).collect();
        log::warn!(
            "Horizon: Java game hook {hook} of {} failed and is disabled for this jar: {error}",
            entry.label
        );
        entry.disabled.insert(hook.to_owned());
        outcome.failed += 1;
        changed = true;
    }
    if changed {
        refresh_mask(role, jars);
    }
    outcome
}

/// One line per jar and hook: how often it fired and whether it was
/// disabled, for the log a server writes when it stops.
pub(super) fn java_game_hook_summary(role: HandlerRole) -> Vec<String> {
    let store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(jars) = store.get(&role) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for jar in jars {
        for hook in jar.registrations.hooks() {
            lines.push(format!(
                "{} {hook}: fired {}{}",
                jar.label,
                jar.fired.get(hook).copied().unwrap_or(0),
                if jar.disabled.contains(hook) {
                    ", disabled after a failure"
                } else {
                    ""
                }
            ));
        }
    }
    lines
}

/// Drop what every jar of `role` kept on one game object, which no longer
/// exists.
fn forget_object(role: HandlerRole, marker: &str) {
    let mut store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(jars) = store.get_mut(&role) {
        for jar in jars {
            jar.state.forget_object(marker);
        }
    }
}

fn player_generation(role: HandlerRole, uuid: u128) -> u32 {
    PLAYER_GENERATIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(role, uuid))
        .copied()
        .unwrap_or(0)
}

/// Moves `uuid`'s player on to a new object and drops the old one's state.
fn next_player_generation(role: HandlerRole, uuid: u128) {
    let old = {
        let mut generations = PLAYER_GENERATIONS.lock().unwrap_or_else(|e| e.into_inner());
        let generation = generations.entry((role, uuid)).or_default();
        let old = *generation;
        *generation = generation.wrapping_add(1);
        old
    };
    forget_object(role, &player_marker(uuid, old));
}

fn player_marker(uuid: u128, generation: u32) -> String {
    let mut object = GameObject::player(uuid, "");
    object.key = format!("{}.{generation}", object.key);
    object.marker()
}

/// The role of the `Game` on this thread, set by its tick.
pub(super) fn set_thread_role(role: HandlerRole) {
    THREAD_ROLE.with(|cell| cell.set(Some(role)));
}

fn thread_role() -> Option<HandlerRole> {
    THREAD_ROLE.with(Cell::get)
}

/// An entity the host knows by id and type only.
fn entity_object(entity: i32, kind: &str) -> GameObject {
    // The hook only names mobs and players, both living.
    GameObject::entity(entity, kind, true)
}

/// `LivingEntity.jumpFromGround`'s TAIL (`living_jump`), from the motion code,
/// which runs without a `Game`. Both sides.
pub(crate) fn fire_living_jump(entity: u32, kind: &str) {
    let Some(role) = thread_role() else {
        return;
    };
    if !java_game_hook_bound(role, "living_jump") {
        return;
    }
    let mut context = GameHookContext::new();
    let receiver = context.insert(entity_object(entity as i32, kind));
    run_java_game_hook(role, "living_jump", &mut context, Some(receiver), &[]);
}

/// `LivingEntity.knockback`'s HEAD (`living_knockback`), from the hit, which
/// runs without a `Game`. Answers whether a callback cancelled the push.
pub(super) fn fire_living_knockback(
    entity: i32,
    kind: &str,
    damage_type: &str,
    strength: f64,
    x: f64,
    z: f64,
) -> bool {
    let Some(role) = thread_role() else {
        return false;
    };
    if !java_game_hook_bound(role, "living_knockback") {
        return false;
    }
    let mut context = GameHookContext::new();
    let receiver = context.insert(entity_object(entity, kind));
    let source = context.insert(GameObject::opaque(
        "net/minecraft/world/damagesource/DamageSource",
        &anonymous(damage_type),
    ));
    // `knockback(strength, x, z, source, damage, invert)`: the hook carries
    // neither the hit's damage nor the invert flag (a shield's push on its
    // holder), so a callback reads 0 and false for them.
    let args = [
        Value::Float(strength),
        Value::Float(x),
        Value::Float(z),
        source,
        Value::Float(0.0),
        Value::Int(0),
    ];
    run_java_game_hook(
        role,
        "living_knockback",
        &mut context,
        Some(receiver),
        &args,
    )
    .cancelled
}

/// A key no other object of this run uses.
fn anonymous(prefix: &str) -> String {
    let n = NEXT_ANONYMOUS.fetch_add(1, Ordering::Relaxed);
    format!("{}-{n}", prefix.replace([':', '/', '|', '#'], "_"))
}

impl Game<'_> {
    /// The handler role of this `Game`.
    fn java_hook_role(&self) -> HandlerRole {
        handler_role_of(self.options.execution_role)
    }

    /// This `Game`'s role when it is a server and `hook` is bound there.
    fn java_server_hook(&self, hook: &str) -> Option<HandlerRole> {
        let role = self.java_hook_role();
        (role != HandlerRole::Client && java_game_hook_bound(role, hook)).then_some(role)
    }

    /// This `Game`'s role when `hook` is bound for it, on either side.
    fn java_any_side_hook(&self, hook: &str) -> Option<HandlerRole> {
        let role = self.java_hook_role();
        java_game_hook_bound(role, hook).then_some(role)
    }

    /// Records this `Game`'s role for the hook points that run on its thread
    /// without it. Called once a tick from `native_tick`.
    pub(super) fn java_hooks_tick(&self) {
        set_thread_role(self.java_hook_role());
    }

    /// A server player as a game object: the acting player when it is
    /// swapped in, else the listed one.
    fn java_player_object(&self, uuid: u128, name: &str) -> GameObject {
        let role = self.java_hook_role();
        let known = self
            .native_acting_player()
            .filter(|p| p.uuid == uuid)
            .or_else(|| {
                self.native_server_players()
                    .into_iter()
                    .find(|p| p.uuid == uuid)
            });
        let mut object = match &known {
            Some(p) => player_object(p),
            None => GameObject::player(uuid, name),
        };
        object.key = format!("{}.{}", object.key, player_generation(role, uuid));
        if object.name.as_deref().is_none_or(str::is_empty) {
            object.name = Some(name.to_owned());
        }
        object
    }

    /// The player whose entity id is `entity`, if a server player has it.
    fn java_player_by_entity(&self, entity: i32) -> Option<GameObject> {
        let player = self
            .native_server_players()
            .into_iter()
            .find(|p| p.entity_id == entity)?;
        Some(self.java_player_object(player.uuid, &player.name))
    }

    /// An entity as a game object: the player when it is one.
    fn java_entity_object(&self, entity: i32, kind: &str) -> GameObject {
        if kind == "minecraft:player"
            && let Some(player) = self.java_player_by_entity(entity)
        {
            return player;
        }
        entity_object(entity, kind)
    }

    /// The acting player (swapped in by the hook point) as a game object.
    fn java_acting_player_object(&self) -> Option<GameObject> {
        let player = self.native_acting_player()?;
        Some(self.java_player_object(player.uuid, &player.name))
    }

    fn java_server_object(&self) -> GameObject {
        GameObject::server(self.java_hook_role() == HandlerRole::DedicatedServer)
    }

    /// Runs one firing and applies what its callbacks asked of the game.
    fn java_fire(
        &mut self,
        role: HandlerRole,
        hook: &str,
        mut context: GameHookContext,
        receiver: Option<Value>,
        args: &[Value],
    ) -> JavaGameHookOutcome {
        // A server object is always reachable through `getServer`.
        if self.java_hook_role() != HandlerRole::Client {
            context.insert(self.java_server_object());
        }
        let outcome = run_java_game_hook(role, hook, &mut context, receiver, args);
        for effect in context.take_effects() {
            match effect {
                GameHookEffect::SystemMessage { player, text } => {
                    self.native_tell(player, horizon_client::text::Text::literal(text));
                }
            }
        }
        outcome
    }

    /// `ServerPlayer.tick`'s TAIL for the swapped-in player.
    pub(super) fn java_hook_player_tick(&mut self) {
        let Some(role) = self.java_server_hook("player_tick") else {
            return;
        };
        let Some(player) = self.java_acting_player_object() else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        self.java_fire(role, "player_tick", context, Some(receiver), &[]);
    }

    /// `ItemEntity.playerTouch`'s HEAD: true when a callback cancelled it.
    pub(super) fn java_hook_item_picked_up(
        &mut self,
        player: u128,
        item: &str,
        count: u32,
    ) -> bool {
        let Some(role) = self.java_server_hook("item_picked_up") else {
            return false;
        };
        let mut context = GameHookContext::new();
        // The hook does not carry the item entity's id, so the object is new
        // at each firing: `getId` refuses and a Mixin field on it is not kept.
        let mut item_entity = GameObject::item_entity(0, item, count as i32);
        item_entity.entity_id = None;
        item_entity.key = anonymous("pickup");
        let receiver = context.insert(item_entity);
        let player = context.insert(self.java_player_object(player, ""));
        self.java_fire(role, "item_picked_up", context, Some(receiver), &[player])
            .cancelled
    }

    /// `LivingEntity.die`'s HEAD: true when a callback cancelled the death.
    pub(super) fn java_hook_living_death(
        &mut self,
        entity: u32,
        kind: &str,
        damage_type: &str,
    ) -> bool {
        let Some(role) = self.java_server_hook("living_death") else {
            return false;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_entity_object(entity as i32, kind));
        let source = context.insert(GameObject::opaque(
            "net/minecraft/world/damagesource/DamageSource",
            &anonymous(damage_type),
        ));
        self.java_fire(role, "living_death", context, Some(receiver), &[source])
            .cancelled
    }

    /// `ServerPlayer.die`'s HEAD.
    pub(super) fn java_hook_player_died(&mut self, player: u128, cause: Option<&str>) {
        let Some(role) = self.java_server_hook("player_died") else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_player_object(player, ""));
        let source = context.insert(GameObject::opaque(
            "net/minecraft/world/damagesource/DamageSource",
            &anonymous(cause.unwrap_or("unknown")),
        ));
        self.java_fire(role, "player_died", context, Some(receiver), &[source]);
    }

    /// `ServerPlayer.restoreFrom`'s TAIL. The new player is a new object: the
    /// old one is the callback's argument, and is dropped afterwards.
    pub(super) fn java_hook_player_clone(&mut self, player: u128, name: &str, was_death: bool) {
        let role = self.java_hook_role();
        if role == HandlerRole::Client {
            return;
        }
        if !java_game_hook_bound(role, "player_clone") {
            next_player_generation(role, player);
            return;
        }
        let old = self.java_player_object(player, name);
        next_player_generation(role, player);
        let new = self.java_player_object(player, name);
        let mut context = GameHookContext::new();
        let receiver = context.insert(new);
        let old = context.insert(old);
        // `restoreFrom(oldPlayer, restoreAll)`: everything is kept when the
        // player left the End, not after a death.
        let restore_all = Value::Int(i32::from(!was_death));
        self.java_fire(
            role,
            "player_clone",
            context,
            Some(receiver),
            &[old, restore_all],
        );
    }

    /// `PlayerList.placeNewPlayer`'s TAIL. The site hands the joined player
    /// as the receiver (`GAME_HOOK_SITES`' note); the connection and the
    /// cookie are identities only.
    pub(super) fn java_hook_player_logged_in(&mut self, player: u128, name: &str) {
        let Some(role) = self.java_server_hook("player_logged_in") else {
            return;
        };
        let mut context = GameHookContext::new();
        let joined = self.java_player_object(player, name);
        let receiver = context.insert(joined.clone());
        let connection = context.insert(GameObject::opaque(
            "net/minecraft/network/Connection",
            &anonymous("connection"),
        ));
        let joined = context.insert(joined);
        let cookie = context.insert(GameObject::opaque(
            "net/minecraft/server/network/CommonListenerCookie",
            &anonymous("cookie"),
        ));
        self.java_fire(
            role,
            "player_logged_in",
            context,
            Some(receiver),
            &[connection, joined, cookie],
        );
    }

    /// A player left: the next one with this UUID is a new object.
    pub(super) fn java_hook_player_logged_out(&mut self, player: u128) {
        let role = self.java_hook_role();
        if role != HandlerRole::Client && BOUND[role_index(role)].load(Ordering::Relaxed) != 0 {
            next_player_generation(role, player);
        }
    }

    /// `ServerLevel.close`'s TAIL.
    pub(super) fn java_hook_level_unload(&mut self, dimension: &str) {
        let Some(role) = self.java_server_hook("level_unload") else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(GameObject::level(dimension));
        self.java_fire(role, "level_unload", context, Some(receiver), &[]);
    }

    /// `MinecraftServer.stopServer`'s HEAD, then one log line per bound
    /// callback saying how often it fired.
    pub(super) fn java_hook_server_stopping(&mut self) {
        let role = self.java_hook_role();
        if role == HandlerRole::Client {
            return;
        }
        if java_game_hook_bound(role, "server_stopping") {
            let mut context = GameHookContext::new();
            let receiver = context.insert(self.java_server_object());
            self.java_fire(role, "server_stopping", context, Some(receiver), &[]);
        }
        for line in java_game_hook_summary(role) {
            log::info!("Horizon: Java game hook {line}");
        }
    }

    /// `Entity.removeVehicle`'s HEAD, for a dismount: true when cancelled.
    pub(super) fn java_hook_entity_dismount(
        &mut self,
        dimension: &str,
        entity: u32,
        kind: &str,
    ) -> bool {
        let Some(role) = self.java_server_hook("entity_mount") else {
            return false;
        };
        let mut context = GameHookContext::new();
        let mut rider = self.java_entity_object(entity as i32, kind);
        if rider.dimension.is_none() {
            rider.dimension = Some(dimension.to_owned());
        }
        let receiver = context.insert(rider);
        self.java_fire(role, "entity_mount", context, Some(receiver), &[])
            .cancelled
    }

    /// `LivingEntity.updatingUsingItem`'s (`living_entity_use_item_tick`) or
    /// `releaseUsingItem`'s (`living_entity_use_item_stop`) HEAD: true when
    /// cancelled. `entity` is `None` for this `Game`'s own player.
    pub(super) fn java_hook_use_item(&mut self, hook: &str, entity: Option<i32>) -> bool {
        let Some(role) = self.java_any_side_hook(hook) else {
            return false;
        };
        let user = match entity {
            Some(entity) => self
                .java_player_by_entity(entity)
                .unwrap_or_else(|| entity_object(entity, "minecraft:unknown")),
            None => match self.java_acting_player_object() {
                Some(player) => player,
                None => {
                    let Some(p) = self.player.as_ref() else {
                        return false;
                    };
                    entity_object(p.entity_id, "minecraft:player")
                }
            },
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(user);
        self.java_fire(role, hook, context, Some(receiver), &[])
            .cancelled
    }

    /// `ServerPlayerGameMode.handleBlockBreakAction`'s HEAD for a dig's start:
    /// true when cancelled. The site hands the acting player as the receiver.
    pub(super) fn java_hook_left_click_block(&mut self, pos: [i32; 3], face: [i32; 3]) -> bool {
        let Some(role) = self.java_server_hook("left_click_block") else {
            return false;
        };
        let Some(player) = self.java_acting_player_object() else {
            return false;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        let block = context.insert(GameObject::opaque(
            "net/minecraft/core/BlockPos",
            &format!("{}_{}_{}", pos[0], pos[1], pos[2]).replace('-', "m"),
        ));
        let action = context.insert(GameObject::opaque(
            "net/minecraft/network/protocol/game/ServerboundPlayerActionPacket$Action",
            "start_destroy_block",
        ));
        let direction = context.insert(GameObject::opaque(
            "net/minecraft/core/Direction",
            match face {
                [0, -1, 0] => "down",
                [0, 1, 0] => "up",
                [0, 0, -1] => "north",
                [0, 0, 1] => "south",
                [-1, 0, 0] => "west",
                _ => "east",
            },
        ));
        // `maxBuildHeight` and the packet's `sequence` are not carried by the
        // hook: a callback reads 0 for both.
        self.java_fire(
            role,
            "left_click_block",
            context,
            Some(receiver),
            &[block, action, direction, Value::Int(0), Value::Int(0)],
        )
        .cancelled
    }

    /// `ServerPlayer.openHorseInventory`'s TAIL, for a mount's inventory.
    pub(super) fn java_hook_mount_inventory_open(&mut self, player: u128, name: &str) {
        let Some(role) = self.java_server_hook("player_container_open") else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_player_object(player, name));
        // The hook names the menu, not the mount or its container.
        let mount = context.insert(GameObject::opaque(
            "net/minecraft/world/entity/animal/equine/AbstractHorse",
            &anonymous("mount"),
        ));
        let container = context.insert(GameObject::opaque(
            "net/minecraft/world/Container",
            &anonymous("container"),
        ));
        self.java_fire(
            role,
            "player_container_open",
            context,
            Some(receiver),
            &[mount, container],
        );
    }

    /// `ItemStack.hurtAndBreak(int, LivingEntity, EquipmentSlot)`'s HEAD: true
    /// when cancelled. `amount` is the wear after Unbreaking, which is what
    /// the hook carries.
    pub(super) fn java_hook_item_damaged(
        &mut self,
        player: u128,
        name: &str,
        item: &str,
        amount: i32,
    ) -> bool {
        let Some(role) = self.java_server_hook("item_damaged") else {
            return false;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(GameObject::item_stack(&anonymous("worn"), item, 1));
        let holder = context.insert(self.java_player_object(player, name));
        let slot = context.insert(GameObject::opaque(
            "net/minecraft/world/entity/EquipmentSlot",
            &anonymous("slot"),
        ));
        self.java_fire(
            role,
            "item_damaged",
            context,
            Some(receiver),
            &[Value::Int(amount), holder, slot],
        )
        .cancelled
    }

    /// `Player.attack`'s HEAD for the acting player: true when cancelled.
    pub(super) fn java_hook_attack_entity(
        &mut self,
        entity: i32,
        kind: &str,
        pos: [f64; 3],
    ) -> bool {
        let Some(role) = self.java_server_hook("attack_entity") else {
            return false;
        };
        let Some(player) = self.java_acting_player_object() else {
            return false;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        let mut target = self.java_entity_object(entity, kind);
        if target.position.is_none() {
            target.position = Some(pos);
        }
        let target = context.insert(target);
        self.java_fire(role, "attack_entity", context, Some(receiver), &[target])
            .cancelled
    }

    /// `ServerEntity.addPairing`'s TAIL. The site hands the tracked entity as
    /// the receiver.
    pub(super) fn java_hook_player_start_tracking(
        &mut self,
        player: u128,
        name: &str,
        entity: u32,
        uuid: u128,
        kind: &str,
    ) {
        let Some(role) = self.java_server_hook("player_start_tracking") else {
            return;
        };
        let mut context = GameHookContext::new();
        let mut tracked = self.java_entity_object(entity as i32, kind);
        if tracked.uuid.is_none() {
            tracked.uuid = Some(uuid);
        }
        let receiver = context.insert(tracked);
        let watcher = context.insert(self.java_player_object(player, name));
        self.java_fire(
            role,
            "player_start_tracking",
            context,
            Some(receiver),
            &[watcher],
        );
    }
}

/// The game object for a listed server player.
fn player_object(p: &NativeServerPlayer) -> GameObject {
    GameObject::player(p.uuid, &p.name)
        .with_entity_id(p.entity_id)
        .with_dimension(&p.dimension)
        .with_position(p.position)
        .with_health(p.health)
        .with_alive(p.health > 0.0)
        .with_game_mode(p.game_mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_native_java::MixinSide;
    use std::{io::Write, path::PathBuf};

    const MIXIN: &str = "package org.spongepowered.asm.mixin;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.TYPE)\npublic @interface Mixin { Class<?>[] value() default {}; String[] targets() default {}; int priority() default 1000; }\n";
    const AT: &str = "package org.spongepowered.asm.mixin.injection;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target({})\npublic @interface At { String value() default \"\"; }\n";
    const INJECT: &str = "package org.spongepowered.asm.mixin.injection;\nimport java.lang.annotation.*;\n@Retention(RetentionPolicy.RUNTIME) @Target(ElementType.METHOD)\npublic @interface Inject { String[] method(); At[] at(); boolean cancellable() default false; }\n";
    const CALLBACK_INFO: &str = "package org.spongepowered.asm.mixin.injection.callback;\npublic class CallbackInfo { public void cancel() {} public boolean isCancelled() { return false; } }\n";
    const ENTITY: &str = "package net.minecraft.world.entity;\npublic class Entity { public int getId() { return 0; } }\n";
    const LIVING: &str = "package net.minecraft.world.entity;\npublic class LivingEntity extends Entity {\n  public float getMaxHealth() { return 0; }\n  public void die(net.minecraft.world.damagesource.DamageSource source) {}\n  public void knockback(double s, double x, double z, net.minecraft.world.damagesource.DamageSource source, float damage, boolean invert) {}\n}\n";
    const DAMAGE_SOURCE: &str =
        "package net.minecraft.world.damagesource;\npublic class DamageSource {}\n";
    /// Cancels a strong push on entity 42 only: the outcome proves the
    /// callback ran with the hook's receiver and arguments.
    const KNOCKBACK_MIXIN: &str = r#"package demo;
import net.minecraft.world.damagesource.DamageSource;
import net.minecraft.world.entity.LivingEntity;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(LivingEntity.class)
public class KnockbackMixin {
    @Inject(method = "knockback", at = @At("HEAD"), cancellable = true)
    private void onKnockback(double strength, double x, double z, DamageSource source, float damage, boolean invert, CallbackInfo ci) {
        if (strength > 1.0 && ((LivingEntity) (Object) this).getId() == 42) {
            ci.cancel();
        }
    }
}
"#;
    /// Reaches a game method the hook tier does not model.
    const DEATH_MIXIN: &str = r#"package demo;
import net.minecraft.world.damagesource.DamageSource;
import net.minecraft.world.entity.LivingEntity;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(LivingEntity.class)
public class DeathMixin {
    @Inject(method = "die", at = @At("HEAD"), cancellable = true)
    private void onDie(DamageSource source, CallbackInfo ci) {
        if (((LivingEntity) (Object) this).getMaxHealth() > 100) {
            ci.cancel();
        }
    }
}
"#;

    /// Compiles the fixture into a jar under `.local/` (never the system
    /// temporary directory). `None` without a JDK.
    fn fixture_jar() -> Option<(PathBuf, PathBuf)> {
        let javac = std::env::var_os("JAVA_HOME")
            .map(|home| PathBuf::from(home).join("bin/javac"))
            .filter(|path| path.is_file())
            .or_else(|| {
                std::env::split_paths(&std::env::var_os("PATH")?)
                    .map(|dir| dir.join("javac"))
                    .find(|path| path.is_file())
            })?;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.local/java-game-hooks-test")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&root);
        let classes = root.join("classes");
        std::fs::create_dir_all(&classes).ok()?;
        let sources = [
            ("org/spongepowered/asm/mixin/Mixin.java", MIXIN),
            ("org/spongepowered/asm/mixin/injection/At.java", AT),
            ("org/spongepowered/asm/mixin/injection/Inject.java", INJECT),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
                CALLBACK_INFO,
            ),
            ("net/minecraft/world/entity/Entity.java", ENTITY),
            ("net/minecraft/world/entity/LivingEntity.java", LIVING),
            (
                "net/minecraft/world/damagesource/DamageSource.java",
                DAMAGE_SOURCE,
            ),
            ("demo/KnockbackMixin.java", KNOCKBACK_MIXIN),
            ("demo/DeathMixin.java", DEATH_MIXIN),
        ];
        let mut files = Vec::new();
        for (relative, body) in sources {
            let path = root.join("src").join(relative);
            std::fs::create_dir_all(path.parent()?).ok()?;
            std::fs::write(&path, body).ok()?;
            files.push(path);
        }
        let status = std::process::Command::new(javac)
            .args(["--release", "17", "-d"])
            .arg(&classes)
            .args(&files)
            .status()
            .ok()?;
        assert!(status.success(), "javac failed");
        let jar = root.join("hooks.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        let metadata =
            r#"{"id":"hooks","version":"1","entrypoints":{},"mixins":["demo.mixins.json"]}"#;
        let config = r#"{"package":"demo","mixins":["KnockbackMixin","DeathMixin"]}"#;
        for (name, body) in [("fabric.mod.json", metadata), ("demo.mixins.json", config)] {
            zip.start_file(name, options).ok()?;
            zip.write_all(body.as_bytes()).ok()?;
        }
        // Only the jar's own classes ship: the game and Mixin are the host's.
        for name in ["demo/KnockbackMixin.class", "demo/DeathMixin.class"] {
            zip.start_file(name, options).ok()?;
            zip.write_all(&std::fs::read(classes.join(name)).ok()?)
                .ok()?;
        }
        zip.finish().ok()?;
        Some((root, jar))
    }

    /// Binds the fixture's two callbacks for `role`, as the batch does.
    fn bind(role: HandlerRole, jar_path: &Path) {
        let jar = NativeJar::from_path(jar_path).expect("the fixture jar");
        let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
        let registrations = jar.register_game_hooks(&weave).expect("registrations");
        assert_eq!(
            registrations.hooks().collect::<Vec<_>>(),
            ["living_death", "living_knockback"],
            "{:#?}",
            weave.skipped
        );
        clear_java_game_hooks_as(role);
        publish_java_game_hooks(
            role,
            jar_path,
            Arc::new(jar),
            registrations,
            &StdHost::new(),
        );
    }

    /// The knockback hook point fires the bound callback with the entity and
    /// the push, and its `cancel()` cancels the push and keeps the native
    /// mods from hearing it; a push it does not cancel goes on.
    #[test]
    fn a_bound_knockback_callback_runs_and_cancels_the_push() {
        let _lock = super::super::java_mods::test_lock();
        let Some((root, jar)) = fixture_jar() else {
            eprintln!("skipping: no JDK");
            return;
        };
        let role = HandlerRole::IntegratedServer;
        bind(role, &jar);
        set_thread_role(role);
        let push = |entity: i32, strength: f64| {
            let (mut strength, mut x, mut z) = (strength, 0.3, -0.4);
            let mut canceled = false;
            Game::native_living_knockback(
                entity,
                "minecraft:zombie",
                "minecraft:player_attack",
                0,
                0,
                &mut strength,
                &mut x,
                &mut z,
                &mut canceled,
            );
            canceled
        };
        assert!(push(42, 2.0), "a strong push on entity 42 is cancelled");
        assert!(!push(42, 0.5), "a weak push is not");
        assert!(!push(7, 2.0), "another entity's push is not");

        // A thread whose game bound nothing fires nothing.
        set_thread_role(HandlerRole::Client);
        assert!(!java_game_hook_bound(
            HandlerRole::Client,
            "living_knockback"
        ));
        assert!(!push(42, 2.0));

        clear_java_game_hooks_as(role);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A callback that reaches an unmodelled game method fails once, is
    /// disabled for that hook only, and the other hook keeps firing.
    #[test]
    fn a_failing_callback_is_disabled_for_its_hook_only() {
        let _lock = super::super::java_mods::test_lock();
        let Some((root, jar)) = fixture_jar() else {
            eprintln!("skipping: no JDK");
            return;
        };
        let role = HandlerRole::DedicatedServer;
        bind(role, &jar);
        let die = || {
            let mut context = GameHookContext::new();
            let receiver = context.insert(GameObject::entity(5, "minecraft:zombie", true));
            let source = context.insert(GameObject::opaque(
                "net/minecraft/world/damagesource/DamageSource",
                "lava",
            ));
            run_java_game_hook(
                role,
                "living_death",
                &mut context,
                Some(receiver),
                &[source],
            )
        };
        let first = die();
        assert_eq!((first.ran, first.cancelled, first.failed), (0, false, 1));
        assert!(!java_game_hook_bound(role, "living_death"));
        assert_eq!(
            die(),
            JavaGameHookOutcome::default(),
            "disabled, not retried"
        );
        assert!(java_game_hook_bound(role, "living_knockback"));
        let summary = java_game_hook_summary(role);
        assert!(
            summary
                .iter()
                .any(|line| line == "hooks.jar living_death: fired 0, disabled after a failure"),
            "{summary:?}"
        );

        clear_java_game_hooks_as(role);
        assert!(!java_game_hook_bound(role, "living_knockback"));
        let _ = std::fs::remove_dir_all(root);
    }
}
