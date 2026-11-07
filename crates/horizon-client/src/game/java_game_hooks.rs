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
    ExecutionBudget, GameHookContext, GameHookEffect, GameHookRegistrations, GameHookState,
    GameObject, NativeJar, Value, every_game_hook_site,
};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
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
    /// Firings per hook in which one of this jar's callbacks cancelled.
    cancelled: BTreeMap<String, u64>,
    /// Firings per hook in which one of this jar's callbacks changed the
    /// value the vanilla method returns.
    overridden: BTreeMap<String, u64>,
}

/// The game-class callbacks one execution role's batch bound, in path order.
/// Keyed by role because the window and its server thread run different jars
/// in the same process, exactly as the payload handler jars are.
static JAVA_GAME_HOOKS: Mutex<BTreeMap<HandlerRole, Vec<JavaGameHookJar>>> =
    Mutex::new(BTreeMap::new());

/// Per role, one bit per hook name (its first row in `GAME_HOOK_SITES`, then
/// the internal `JAVA_TIER_SITES`) that any of the role's jars still fires.
/// Read before anything else at every hook point, so a game with no bound jar
/// pays one relaxed load.
static BOUND: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];

/// Player generations, per role: a respawn or a logout makes the next player
/// object a new one, as a new `ServerPlayer` is.
static PLAYER_GENERATIONS: Mutex<BTreeMap<(HandlerRole, u128), u32>> = Mutex::new(BTreeMap::new());

/// Firings that ran before their jar was published, per role, jar label and
/// hook (`Bootstrap#bootStrap`, which runs before the jar's entrypoints):
/// [`publish_java_game_hooks_resident`] moves them into the jar's counts.
/// The flag says the hook failed there, so it is published disabled.
static EARLY_FIRINGS: Mutex<BTreeMap<(HandlerRole, String, String), (u64, bool)>> =
    Mutex::new(BTreeMap::new());

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
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct JavaGameHookOutcome {
    /// How many callbacks ran; a cancellation stops the ones after it.
    pub(super) ran: usize,
    /// Whether a callback cancelled the vanilla work at this point.
    pub(super) cancelled: bool,
    /// What a value-returning site's callbacks made the vanilla method return
    /// (`GameHookRun::returned`): at a `HEAD` site the cancelled call's
    /// result, at a `RETURN`/`TAIL` site the override of the result the game
    /// handed in. `None` leaves the game's own value.
    pub(super) returned: Option<Value>,
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

/// The hook's bit: the index of its first row over both site tables. The
/// tables hold far fewer than 64 rows (a unit test keeps it so).
fn hook_bit(hook: &str) -> Option<u64> {
    every_game_hook_site()
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
    let label = jar_label(path);
    let mut fired = BTreeMap::new();
    let mut disabled = BTreeSet::new();
    EARLY_FIRINGS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(r, l, hook), (count, failed)| {
            if *r != role || *l != label {
                return true;
            }
            fired.insert(hook.clone(), *count);
            if *failed {
                disabled.insert(hook.clone());
            }
            false
        });
    let mut store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let jars = store.entry(role).or_default();
    jars.push(JavaGameHookJar {
        label,
        jar,
        registrations,
        host,
        state: GameHookState::default(),
        disabled,
        fired,
        cancelled: BTreeMap::new(),
        overridden: BTreeMap::new(),
    });
    refresh_mask(role, jars);
}

/// A jar's file name, the label its log lines and counts carry.
fn jar_label(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
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
    run_java_game_hook_returning(role, hook, context, receiver, args, None)
}

/// [`run_java_game_hook`] for a site whose vanilla method returns a value:
/// `actual` is the game's own result at a `RETURN`/`TAIL` site (`None` at a
/// `HEAD` one), and [`JavaGameHookOutcome::returned`] is what the game must
/// use instead, if anything.
///
/// Every jar sees the game's own `actual`: an override is made by a
/// cancellable callback, and its cancel stops the callbacks after it, so no
/// later jar ever reads an earlier jar's override, as in Mixin.
pub(super) fn run_java_game_hook_returning(
    role: HandlerRole,
    hook: &str,
    context: &mut GameHookContext,
    receiver: Option<Value>,
    args: &[Value],
    actual: Option<Value>,
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
            let run = jar.run_game_hook_returning(
                registrations,
                state,
                context,
                hook,
                receiver.clone(),
                args,
                actual.clone(),
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
                if run.returned.is_some() {
                    *entry.overridden.entry(hook.to_owned()).or_default() += 1;
                    outcome.returned = run.returned;
                }
                if run.cancelled {
                    *entry.cancelled.entry(hook.to_owned()).or_default() += 1;
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

/// One line per jar and hook: how often it fired, how often a callback
/// cancelled or changed the returned value there, and whether it was
/// disabled, for the log a server writes when it stops.
pub(super) fn java_game_hook_summary(role: HandlerRole) -> Vec<String> {
    let store = JAVA_GAME_HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(jars) = store.get(&role) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for jar in jars {
        for hook in jar.registrations.hooks() {
            let count = |map: &BTreeMap<String, u64>| map.get(hook).copied().unwrap_or(0);
            let mut effects = String::new();
            if count(&jar.cancelled) > 0 {
                effects.push_str(&format!(", cancelled {}", count(&jar.cancelled)));
            }
            if count(&jar.overridden) > 0 {
                effects.push_str(&format!(", overridden {}", count(&jar.overridden)));
            }
            lines.push(format!(
                "{} {hook}: fired {}{effects}{}",
                jar.label,
                count(&jar.fired),
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

/// `LivingEntity.hurtServer`'s HEAD (`java_living_hurt`), from the damage
/// code once hurtServer's guards passed, which runs without a `Game`.
/// Answers whether a callback cancelled the hit. Horizon's callers only know
/// a refused hit, so a cancel is one whatever value the callback set (vanilla
/// would let a `setReturnValue(true)` count as landed); the level is an
/// identity without its dimension.
pub(super) fn fire_living_hurt(entity: i32, kind: &str, damage_type: &str, amount: f32) -> bool {
    let Some(role) = thread_role() else {
        return false;
    };
    if role == HandlerRole::Client || !java_game_hook_bound(role, "java_living_hurt") {
        return false;
    }
    let mut context = GameHookContext::new();
    let receiver = context.insert(entity_object(entity, kind));
    let level = context.insert(GameObject::opaque(
        "net/minecraft/server/level/ServerLevel",
        &anonymous("level"),
    ));
    let source = context.insert(GameObject::opaque(
        "net/minecraft/world/damagesource/DamageSource",
        &anonymous(damage_type),
    ));
    let args = [level, source, Value::Float(f64::from(amount))];
    run_java_game_hook(
        role,
        "java_living_hurt",
        &mut context,
        Some(receiver),
        &args,
    )
    .cancelled
}

/// `Bootstrap.bootStrap`'s HEAD, then its TAIL (`java_bootstrap_head`,
/// `java_bootstrap_tail`), for one jar as the server bootstraps it: on the
/// jar's own host, after the host is seeded and before the jar's entrypoints
/// run, which is where vanilla's `Main` bootstraps before Fabric starts the
/// server entrypoints. Each jar hears its own firing. The jar is not
/// published yet, so the counts wait in [`EARLY_FIRINGS`]; a failure is
/// logged and leaves the jar's entrypoints to run as before.
pub(super) fn fire_java_bootstrap(
    role: ExecutionRole,
    path: &Path,
    jar: &NativeJar,
    registrations: &GameHookRegistrations,
    host: &mut horizon_native_java::StdHost,
) {
    let role = handler_role_of(role);
    if role == HandlerRole::Client {
        return;
    }
    let label = jar_label(path);
    let mut state = GameHookState::default();
    for hook in ["java_bootstrap_head", "java_bootstrap_tail"] {
        if registrations.entries(hook).is_empty() {
            continue;
        }
        let budget = ExecutionBudget::within(CALLBACK_BUDGET);
        let mut context = GameHookContext::new();
        host.set_strict_unknown_host_calls(true);
        let mark = host.call_log_mark();
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jar.run_game_hook_returning(
                registrations,
                &mut state,
                &mut context,
                hook,
                None,
                &[],
                None,
                &mut *host,
                &budget,
            )
        }));
        host.rewind_call_log(mark);
        host.set_strict_unknown_host_calls(false);
        let failure = match run {
            Ok(Ok(run)) => {
                log::info!(
                    "Horizon: Java game hook {hook} fired {} callback(s) of {label} (first firing)",
                    run.ran
                );
                None
            }
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("panicked".to_owned()),
        };
        if let Some(error) = &failure {
            let error: String = error.replace(['\n', '\r'], " ").chars().take(300).collect();
            log::warn!(
                "Horizon: Java game hook {hook} of {label} failed and is disabled for this jar: {error}"
            );
        }
        let mut early = EARLY_FIRINGS.lock().unwrap_or_else(|e| e.into_inner());
        let entry = early
            .entry((role, label.clone(), hook.to_owned()))
            .or_default();
        if failure.is_some() {
            entry.1 = true;
        } else {
            entry.0 += 1;
        }
    }
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
        // The hands are known only for the player swapped in for this hook
        // point: their inventory is the `Game`'s now.
        if self
            .native_acting_player()
            .is_some_and(|acting| acting.uuid == uuid)
        {
            let stack = |s: Option<&horizon_client::items::ItemStack>| {
                s.filter(|s| s.count > 0).map_or_else(
                    || ("minecraft:air".to_owned(), 0),
                    |s| {
                        let item = if s.item.contains(':') {
                            s.item.to_string()
                        } else {
                            format!("minecraft:{}", s.item)
                        };
                        (item, i32::try_from(s.count).unwrap_or(i32::MAX))
                    },
                )
            };
            let (main, off) = (stack(self.held()), stack(self.offhand.as_ref()));
            object = object.with_hands((&main.0, main.1), (&off.0, off.1));
        }
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

    /// A guest's remote address as Java's `InetSocketAddress.toString` prints
    /// an accepted socket's (`/127.0.0.1:52144`). IPv6 is left out: Java
    /// spells it uncompressed, which `SocketAddr` does not.
    fn java_guest_address(&self, player: u128) -> Option<String> {
        let guest = self
            .lan
            .as_ref()?
            .hub
            .guests
            .iter()
            .find(|g| g.uuid == player)?;
        match guest.address {
            // Port 0 is the window's own memory connection, which vanilla
            // names a `LocalAddress`, not a socket.
            std::net::SocketAddr::V4(address) if address.port() != 0 => Some(format!("/{address}")),
            _ => None,
        }
    }

    fn java_server_object(&self) -> GameObject {
        GameObject::server(self.java_hook_role() == HandlerRole::DedicatedServer)
    }

    /// Runs one firing and applies what its callbacks asked of the game.
    fn java_fire(
        &mut self,
        role: HandlerRole,
        hook: &str,
        context: GameHookContext,
        receiver: Option<Value>,
        args: &[Value],
    ) -> JavaGameHookOutcome {
        self.java_fire_returning(role, hook, context, receiver, args, None)
    }

    /// [`Self::java_fire`] at a value-returning site, with the game's own
    /// result at a `RETURN`/`TAIL` one (`run_java_game_hook_returning`).
    fn java_fire_returning(
        &mut self,
        role: HandlerRole,
        hook: &str,
        mut context: GameHookContext,
        receiver: Option<Value>,
        args: &[Value],
        actual: Option<Value>,
    ) -> JavaGameHookOutcome {
        // A server object is always reachable through `getServer`, and the
        // server-wide members (world path, game rules, registries, which
        // channels a client takes) answer from the server's own state.
        if self.java_hook_role() != HandlerRole::Client {
            context.insert(self.java_server_object());
            context.set_server_facts(self.java_server_facts());
        }
        let outcome =
            run_java_game_hook_returning(role, hook, &mut context, receiver, args, actual);
        for effect in context.take_effects() {
            match effect {
                GameHookEffect::SystemMessage { player, text } => {
                    self.native_tell(player, horizon_client::text::Text::literal(text));
                }
            }
        }
        outcome
    }

    /// `ServerPlayer.tick`'s TAIL for the swapped-in player, after the end of
    /// its `Player.tick` (`java_base_player_tick_tail`, `hook_shapes.rs`):
    /// Horizon runs the two as one step.
    pub(super) fn java_hook_player_tick(&mut self) {
        self.java_hook_acting_player("java_base_player_tick_tail");
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
        // The connection knows its remote address (a guest's TCP peer; the
        // window's own memory connection has none), the cookie the joining
        // player's profile.
        let mut connection =
            GameObject::opaque("net/minecraft/network/Connection", &anonymous("connection"));
        if let Some(address) = self.java_guest_address(player) {
            connection = connection.with_address(&address);
        }
        let connection = context.insert(connection);
        let joined = context.insert(joined);
        let cookie = context.insert(
            GameObject::opaque(
                "net/minecraft/server/network/CommonListenerCookie",
                &anonymous("cookie"),
            )
            .with_uuid(player)
            .with_name(name),
        );
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

/// The mining speed after a `Player.getDestroySpeed` RETURN firing
/// (`java_destroy_speed`): a cancellable callback's override, else the
/// game's own `speed`. The override comes back already rounded to `float`.
fn destroy_speed_from(outcome: &JavaGameHookOutcome, speed: f32) -> f32 {
    match outcome.returned {
        Some(Value::Float(overridden)) => overridden as f32,
        _ => speed,
    }
}

/// What a `BoneMealItem.growCrop` HEAD firing (`java_grow_crop`) makes the
/// method return: `Some` when a callback cancelled (its `setReturnValue`, or
/// `false`, the type's default, for a bare `cancel()`), `None` to let it run.
fn grown_from(outcome: &JavaGameHookOutcome) -> Option<bool> {
    match (outcome.cancelled, &outcome.returned) {
        (true, Some(Value::Int(grown))) => Some(*grown != 0),
        (true, _) => Some(false),
        (false, _) => None,
    }
}

/// What bone meal does next once `java_grow_crop` answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoneMealStep {
    /// Vanilla's own `growCrop`, then `growWaterPlant` when it grew nothing.
    Vanilla,
    /// `growWaterPlant` alone: a cancelled `growCrop` answered `false`.
    WaterPlant,
}

/// A player's or a dispenser's bone meal after `BoneMealItem.growCrop`'s
/// HEAD (`java_grow_crop`, [`Game::java_hook_grow_crop`]): whether the meal
/// did something, and whether it was used up. `grow` runs one step and says
/// whether it grew anything; both steps use the meal up when they grow.
///
/// - `None`: no callback cancelled, so vanilla runs ([`BoneMealStep::Vanilla`]).
/// - `Some(true)`: the cancelled `growCrop` answered `true`, so both callers
///   stop with a success and no meal is used (vanilla's body, which shrinks
///   the stack, never ran); `grow` is not called.
/// - `Some(false)`: `growCrop` answered `false`, so both callers go on to
///   `growWaterPlant`, which uses the meal up only when it grows.
pub(super) fn bone_meal_after_grow_crop(
    java: Option<bool>,
    mut grow: impl FnMut(BoneMealStep) -> bool,
) -> (bool, bool) {
    match java {
        Some(true) => (true, false),
        Some(false) => {
            let grown = grow(BoneMealStep::WaterPlant);
            (grown, grown)
        }
        None => {
            let grown = grow(BoneMealStep::Vanilla);
            (grown, grown)
        }
    }
}

/// The Java-tier-only points (`horizon_native_java::JAVA_TIER_SITES`): the
/// positions the game reaches that the public native-mod API has no hook for,
/// and the value-returning ones. Each says the vanilla method it stands for;
/// `hook_shapes.rs` in horizon-native-java holds the table and its notes.
impl Game<'_> {
    /// A listen-only point whose receiver is the swapped-in player and whose
    /// target takes no argument.
    fn java_hook_acting_player(&mut self, hook: &'static str) {
        let Some(role) = self.java_server_hook(hook) else {
            return;
        };
        let Some(player) = self.java_acting_player_object() else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        self.java_fire(role, hook, context, Some(receiver), &[]);
    }

    /// `ServerPlayer.tick`'s HEAD, then `Player.tick`'s, for the swapped-in
    /// player: Horizon runs the two methods as one step of a living guest's
    /// tick, so both heads fire before its digging and item use.
    fn java_hook_player_tick_head(&mut self) {
        self.java_hook_acting_player("java_player_tick_head");
        self.java_hook_acting_player("java_base_player_tick_head");
    }

    /// Where `tick_guests` starts a living guest's tick: swaps the guest in
    /// only when a jar bound one of the two heads.
    pub(super) fn java_guest_tick_head(&mut self, host: &mut super::lan::LanHost, conn: u32) {
        let role = self.java_hook_role();
        if role == HandlerRole::Client
            || !(java_game_hook_bound(role, "java_player_tick_head")
                || java_game_hook_bound(role, "java_base_player_tick_head"))
        {
            return;
        }
        self.as_guest(host, conn, |g| g.java_hook_player_tick_head());
    }

    /// `PlayerList.remove`'s HEAD, from `native_player_logged_out`: the
    /// leaving player as the receiver (as at `player_logged_in`) and as the
    /// argument. Horizon has unlisted it already, so the object carries the
    /// UUID and name only. Called before the next player object starts.
    pub(super) fn java_hook_player_list_remove(&mut self, player: u128, name: &str) {
        let Some(role) = self.java_server_hook("java_player_list_remove") else {
            return;
        };
        let mut context = GameHookContext::new();
        let leaving = self.java_player_object(player, name);
        let receiver = context.insert(leaving.clone());
        let leaving = context.insert(leaving);
        self.java_fire(
            role,
            "java_player_list_remove",
            context,
            Some(receiver),
            &[leaving],
        );
    }

    /// `MinecraftServer.stopServer`'s TAIL: after the final save and the
    /// levels' close, before the server's stopped event.
    pub(super) fn java_hook_server_stopped(&mut self) {
        let Some(role) = self.java_server_hook("java_server_stopped") else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_server_object());
        self.java_fire(role, "java_server_stopped", context, Some(receiver), &[]);
    }

    /// `Player.getDestroySpeed`'s RETURN for the swapped-in player mining
    /// `state`: `speed` is the vanilla result Horizon computed, and the
    /// answer is what the progress must divide by the hardness. A firing that
    /// fails leaves `speed`.
    pub(super) fn java_hook_destroy_speed(
        &mut self,
        state: horizon_world::registry::BlockStateId,
        speed: f32,
    ) -> f32 {
        let Some(role) = self.java_server_hook("java_destroy_speed") else {
            return speed;
        };
        let Some(player) = self.java_acting_player_object() else {
            return speed;
        };
        let name = self.res.facts().get(state).name.to_string();
        let mut context = GameHookContext::new();
        let receiver = context.insert(player);
        let block = context.insert(GameObject::opaque(
            "net/minecraft/world/level/block/state/BlockState",
            &anonymous(&name),
        ));
        let outcome = self.java_fire_returning(
            role,
            "java_destroy_speed",
            context,
            Some(receiver),
            &[block],
            Some(Value::Float(f64::from(speed))),
        );
        destroy_speed_from(&outcome, speed)
    }

    /// `BoneMealItem.growCrop`'s HEAD for `stack` on `pos` of this level:
    /// `Some(grown)` when a callback cancelled, which the caller returns in
    /// place of `grow_crop` without growing or using up the meal; `None` to
    /// run it.
    pub(super) fn java_hook_grow_crop(
        &mut self,
        stack: &horizon_client::items::ItemStack,
        pos: horizon_core::position::BlockPos,
    ) -> Option<bool> {
        let role = self.java_server_hook("java_grow_crop")?;
        let mut context = GameHookContext::new();
        let meal = context.insert(GameObject::item_stack(
            &anonymous("meal"),
            &stack.item,
            stack.count as i32,
        ));
        let level = context.insert(GameObject::level(self.dim.id()));
        let pos = context.insert(GameObject::opaque(
            "net/minecraft/core/BlockPos",
            &format!("{}_{}_{}", pos.x, pos.y, pos.z).replace('-', "m"),
        ));
        let outcome = self.java_fire(role, "java_grow_crop", context, None, &[meal, level, pos]);
        grown_from(&outcome)
    }
}

/// A key for an object of which there is one per name (a `Block` is one
/// object per block type), so a Mixin field on it is kept.
fn named_key(name: &str) -> String {
    name.replace([':', '/', '|', '#'], "_")
}

/// The Java-tier server points added for the server's tick, its construction,
/// a player's harvest, an explosion and a mob's death
/// (`horizon_native_java::JAVA_TIER_SITES`, `hook_shapes.rs`).
impl Game<'_> {
    /// `MinecraftServer.tickServer`'s HEAD (`start`) or TAIL, from
    /// `native_server_tick`'s `ServerTickStart`/`ServerTickEnd`. The host has
    /// no `haveTime` supplier to hand: a callback reads null.
    pub(super) fn java_hook_server_tick(&mut self, start: bool) {
        let hook = if start {
            "java_server_tick_head"
        } else {
            "java_server_tick_tail"
        };
        let Some(role) = self.java_server_hook(hook) else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_server_object());
        self.java_fire(role, hook, context, Some(receiver), &[Value::Ref(None)]);
    }

    /// `ServerLevel.tick`'s HEAD (`start`) or TAIL for `dimension`, from
    /// `native_level_tick_pre`/`_post`; `haveTime` is null, as above.
    pub(super) fn java_hook_level_tick(&mut self, dimension: &str, start: bool) {
        let hook = if start {
            "java_level_tick_head"
        } else {
            "java_level_tick_tail"
        };
        let Some(role) = self.java_server_hook(hook) else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(GameObject::level(dimension));
        self.java_fire(role, hook, context, Some(receiver), &[Value::Ref(None)]);
    }

    /// The server's constructor tails: `MinecraftServer.<init>`'s, then (on a
    /// dedicated server) `DedicatedServer.<init>`'s, as the subclass's body
    /// runs after its `super` call. Called once per server start, after the
    /// Java batch ran and before the configs and `native_server_about_to_start`.
    /// The arguments vanilla passes are game objects the host does not model
    /// (identities only, one per server start) or JDK objects it has none of
    /// (the thread, the `Optional` game rules, the proxy: null).
    pub(super) fn java_hook_server_constructed(&mut self) {
        let role = self.java_hook_role();
        if role == HandlerRole::Client {
            return;
        }
        let dedicated = role == HandlerRole::DedicatedServer;
        for hook in [
            "java_server_constructed",
            "java_dedicated_server_constructed",
        ] {
            if hook == "java_dedicated_server_constructed" && !dedicated {
                continue;
            }
            let Some(role) = self.java_server_hook(hook) else {
                continue;
            };
            let mut context = GameHookContext::new();
            let receiver = context.insert(self.java_server_object());
            let mut object =
                |class: &str, key: &str| context.insert(GameObject::opaque(class, key));
            let storage = object(
                "net/minecraft/world/level/storage/LevelStorageSource$LevelStorageAccess",
                "server-storage",
            );
            let packs = object(
                "net/minecraft/server/packs/repository/PackRepository",
                "server-packs",
            );
            let stem = object("net/minecraft/server/WorldStem", "server-world-stem");
            let fixer = object("com/mojang/datafixers/DataFixer", "server-data-fixer");
            let services = object("net/minecraft/server/Services", "server-services");
            let notifications = object(
                "net/minecraft/server/notifications/NotificationManager",
                "server-notifications",
            );
            let null = Value::Ref(None);
            let args = if hook == "java_server_constructed" {
                let listener = object(
                    "net/minecraft/server/level/progress/LevelLoadListener",
                    "server-level-load-listener",
                );
                // `propagatesCrashes`: true for a dedicated server.
                vec![
                    null.clone(),
                    storage,
                    packs,
                    stem,
                    null.clone(),
                    null,
                    fixer,
                    services,
                    listener,
                    Value::Int(i32::from(dedicated)),
                    notifications,
                ]
            } else {
                let settings = object(
                    "net/minecraft/server/dedicated/DedicatedServerSettings",
                    "server-settings",
                );
                let management = object(
                    "net/minecraft/server/jsonrpc/ManagementServer",
                    "server-management",
                );
                vec![
                    null.clone(),
                    storage,
                    packs,
                    stem,
                    null,
                    settings,
                    fixer,
                    services,
                    management,
                    notifications,
                ]
            };
            self.java_fire(role, hook, context, Some(receiver), &args);
        }
    }

    /// `Block.playerDestroy`'s HEAD (`head`, true when a callback cancelled
    /// it) or TAIL for the acting player's harvest of `state` at `pos` with
    /// `held` (`destroyedWith`, a copy taken before the tool wore).
    /// `block_entity` says whether the block had one: vanilla passes null
    /// otherwise, and the host hands an identity when it had.
    pub(super) fn java_hook_block_player_destroy(
        &mut self,
        head: bool,
        pos: [i32; 3],
        state: horizon_world::registry::BlockStateId,
        held: Option<&horizon_client::items::ItemStack>,
        block_entity: bool,
    ) -> bool {
        let hook = if head {
            "java_block_player_destroy"
        } else {
            "java_block_player_destroyed"
        };
        let Some(role) = self.java_server_hook(hook) else {
            return false;
        };
        let Some(player) = self.java_acting_player_object() else {
            return false;
        };
        let name = self.res.facts().get(state).name.to_string();
        let mut context = GameHookContext::new();
        let receiver = context.insert(GameObject::opaque(
            "net/minecraft/world/level/block/Block",
            &named_key(&name),
        ));
        let level = context.insert(GameObject::level(self.dim.id()));
        let player = context.insert(player);
        let at = context.insert(GameObject::opaque(
            "net/minecraft/core/BlockPos",
            &format!("{}_{}_{}", pos[0], pos[1], pos[2]).replace('-', "m"),
        ));
        let block = context.insert(GameObject::opaque(
            "net/minecraft/world/level/block/state/BlockState",
            &anonymous(&name),
        ));
        let entity = if block_entity {
            context.insert(GameObject::opaque(
                "net/minecraft/world/level/block/entity/BlockEntity",
                &anonymous("block_entity"),
            ))
        } else {
            Value::Ref(None)
        };
        let (item, count) =
            held.filter(|s| s.count > 0)
                .map_or(("minecraft:air".to_owned(), 0), |s| {
                    (
                        s.item.to_string(),
                        i32::try_from(s.count).unwrap_or(i32::MAX),
                    )
                });
        let tool = context.insert(GameObject::item_stack(
            &anonymous("destroyed_with"),
            &item,
            count,
        ));
        self.java_fire(
            role,
            hook,
            context,
            Some(receiver),
            &[level, player, at, block, entity, tool],
        )
        .cancelled
    }

    /// `ServerExplosion.explode`'s HEAD, from `server_explode_as` before the
    /// blast is heard. The explosion is an identity: the host does not model
    /// its fields, so a callback reading them through `@Shadow` is refused.
    pub(super) fn java_hook_explosion(&mut self) {
        let Some(role) = self.java_server_hook("java_explosion_explode") else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(GameObject::opaque(
            "net/minecraft/world/level/ServerExplosion",
            &anonymous("explosion"),
        ));
        self.java_fire(role, "java_explosion_explode", context, Some(receiver), &[]);
    }

    /// A dying mob's `LivingEntity.dropAllDeathLoot` HEAD
    /// (`java_drop_death_loot_head`) or TAIL (`_tail`), or `LivingEntity.die`'s
    /// TAIL (`java_living_died`), from `mob_died`. `damage_type` names the
    /// source it died to.
    pub(super) fn java_hook_mob_death(
        &mut self,
        hook: &'static str,
        entity: u32,
        kind: &str,
        damage_type: &str,
    ) {
        let Some(role) = self.java_server_hook(hook) else {
            return;
        };
        let mut context = GameHookContext::new();
        let receiver = context.insert(self.java_entity_object(entity as i32, kind));
        let source = context.insert(GameObject::opaque(
            "net/minecraft/world/damagesource/DamageSource",
            &anonymous(damage_type),
        ));
        let args = if hook == "java_living_died" {
            vec![source]
        } else {
            vec![context.insert(GameObject::level(self.dim.id())), source]
        };
        self.java_fire(role, hook, context, Some(receiver), &args);
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
    const CALLBACK_INFO_RETURNABLE: &str = "package org.spongepowered.asm.mixin.injection.callback;\npublic class CallbackInfoReturnable<R> extends CallbackInfo { public R getReturnValue() { return null; } public void setReturnValue(R value) {} }\n";
    /// The value-returning Java-tier sites: a `getDestroySpeed` override that
    /// triples the speed of a slow block only, and a `growCrop` HEAD that
    /// answers `true` without growing.
    const VALUE_MIXINS: &str = r#"package demo;
import net.minecraft.core.BlockPos;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import net.minecraft.world.level.block.state.BlockState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;
@Mixin(net.minecraft.world.entity.player.Player.class)
public class SpeedMixin {
    @Inject(method = "getDestroySpeed", at = @At("RETURN"), cancellable = true)
    private static void faster(BlockState state, CallbackInfoReturnable<Float> cir) {
        if (cir.getReturnValue() < 2.0f) {
            cir.setReturnValue(cir.getReturnValue() * 3.0f);
        }
    }
}
"#;
    const GROW_MIXIN: &str = r#"package demo;
import net.minecraft.core.BlockPos;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.Level;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;
@Mixin(net.minecraft.world.item.BoneMealItem.class)
public class GrowMixin {
    @Inject(method = "growCrop", at = @At("HEAD"), cancellable = true)
    private static void claim(ItemStack stack, Level level, BlockPos pos, CallbackInfoReturnable<Boolean> cir) {
        cir.setReturnValue(true);
    }
}
"#;

    /// The value fixture's jar under `.local/`; `None` without a JDK.
    fn value_fixture_jar() -> Option<(PathBuf, PathBuf)> {
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
            .join(format!("values-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let classes = root.join("classes");
        std::fs::create_dir_all(&classes).ok()?;
        let stub =
            |package: &str, name: &str| format!("package {package};\npublic class {name} {{}}\n");
        let sources = [
            ("org/spongepowered/asm/mixin/Mixin.java", MIXIN.to_owned()),
            (
                "org/spongepowered/asm/mixin/injection/At.java",
                AT.to_owned(),
            ),
            (
                "org/spongepowered/asm/mixin/injection/Inject.java",
                INJECT.to_owned(),
            ),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
                CALLBACK_INFO.to_owned(),
            ),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable.java",
                CALLBACK_INFO_RETURNABLE.to_owned(),
            ),
            (
                "net/minecraft/core/BlockPos.java",
                stub("net.minecraft.core", "BlockPos"),
            ),
            (
                "net/minecraft/world/item/ItemStack.java",
                stub("net.minecraft.world.item", "ItemStack"),
            ),
            (
                "net/minecraft/world/item/BoneMealItem.java",
                stub("net.minecraft.world.item", "BoneMealItem"),
            ),
            (
                "net/minecraft/world/level/Level.java",
                stub("net.minecraft.world.level", "Level"),
            ),
            (
                "net/minecraft/world/level/block/state/BlockState.java",
                stub("net.minecraft.world.level.block.state", "BlockState"),
            ),
            (
                "net/minecraft/world/entity/player/Player.java",
                stub("net.minecraft.world.entity.player", "Player"),
            ),
            ("demo/SpeedMixin.java", VALUE_MIXINS.to_owned()),
            ("demo/GrowMixin.java", GROW_MIXIN.to_owned()),
        ];
        let mut files = Vec::new();
        for (relative, body) in &sources {
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
        let jar = root.join("values.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        let metadata =
            r#"{"id":"values","version":"1","entrypoints":{},"mixins":["demo.mixins.json"]}"#;
        let config = r#"{"package":"demo","mixins":["SpeedMixin","GrowMixin"]}"#;
        for (name, body) in [("fabric.mod.json", metadata), ("demo.mixins.json", config)] {
            zip.start_file(name, options).ok()?;
            zip.write_all(body.as_bytes()).ok()?;
        }
        for name in ["demo/SpeedMixin.class", "demo/GrowMixin.class"] {
            zip.start_file(name, options).ok()?;
            zip.write_all(&std::fs::read(classes.join(name)).ok()?)
                .ok()?;
        }
        zip.finish().ok()?;
        Some((root, jar))
    }

    /// Every site has its own bit in the role masks.
    #[test]
    fn every_site_fits_the_bound_mask() {
        assert!(every_game_hook_site().count() <= 64);
        assert!(hook_bit("java_destroy_speed").is_some());
        assert_ne!(hook_bit("java_destroy_speed"), hook_bit("player_tick"));
    }

    /// The game reads back what a bound callback made a value-returning
    /// vanilla method return: the mining speed it overrode (only for the
    /// slow block it picks), and a `growCrop` it answered `true` without
    /// growing. Nothing bound leaves the game's own values.
    #[test]
    fn a_bound_value_callback_overrides_what_the_game_uses() {
        let _lock = super::super::java_mods::test_lock();
        let Some((root, jar_path)) = value_fixture_jar() else {
            eprintln!("skipping: no JDK");
            return;
        };
        let role = HandlerRole::DedicatedServer;
        clear_java_game_hooks_as(role);
        let speed = |speed: f32| {
            let mut context = GameHookContext::new();
            let player = context.insert(GameObject::player(7, "Steve"));
            let block = context.insert(GameObject::opaque(
                "net/minecraft/world/level/block/state/BlockState",
                "stone",
            ));
            let outcome = run_java_game_hook_returning(
                role,
                "java_destroy_speed",
                &mut context,
                Some(player),
                &[block],
                Some(Value::Float(f64::from(speed))),
            );
            destroy_speed_from(&outcome, speed)
        };
        let grow = || {
            let mut context = GameHookContext::new();
            let args = [
                context.insert(GameObject::item_stack("meal", "minecraft:bone_meal", 3)),
                context.insert(GameObject::level("minecraft:overworld")),
                context.insert(GameObject::opaque("net/minecraft/core/BlockPos", "1_64_2")),
            ];
            grown_from(&run_java_game_hook_returning(
                role,
                "java_grow_crop",
                &mut context,
                None,
                &args,
                None,
            ))
        };
        // Nothing bound: the game's own values stand.
        assert_eq!(speed(1.5), 1.5);
        assert_eq!(grow(), None);

        let jar = NativeJar::from_path(&jar_path).expect("the fixture jar");
        let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
        let registrations = jar.register_game_hooks(&weave).expect("registrations");
        assert_eq!(
            registrations.hooks().collect::<Vec<_>>(),
            ["java_destroy_speed", "java_grow_crop"],
            "{:#?}",
            weave.skipped
        );
        publish_java_game_hooks(
            role,
            &jar_path,
            Arc::new(jar),
            registrations,
            &StdHost::new(),
        );
        assert_eq!(speed(1.5), 4.5, "the slow block's speed is tripled");
        assert_eq!(speed(6.0), 6.0, "a fast block keeps the game's speed");
        assert_eq!(grow(), Some(true));

        clear_java_game_hooks_as(role);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A bone meal use after `java_grow_crop`: an answered `true` stops with
    /// a success and no meal used, an answered `false` goes on to the water
    /// plant alone (which uses the meal only when it grows), and no answer
    /// runs vanilla. `grow` records the steps it was asked to run.
    #[test]
    fn bone_meal_falls_back_the_way_growcrop_answered() {
        let run = |java: Option<bool>, grows: bool| {
            let mut steps = Vec::new();
            let outcome = bone_meal_after_grow_crop(java, |step| {
                steps.push(step);
                grows
            });
            (outcome, steps)
        };
        // Answered true: a success, the meal kept, nothing grown here.
        assert_eq!(run(Some(true), false), ((true, false), vec![]));
        // Answered false: only growWaterPlant runs; the meal goes when it grew.
        assert_eq!(
            run(Some(false), true),
            ((true, true), vec![BoneMealStep::WaterPlant])
        );
        assert_eq!(
            run(Some(false), false),
            ((false, false), vec![BoneMealStep::WaterPlant])
        );
        // No answer: vanilla's growCrop-then-growWaterPlant.
        assert_eq!(run(None, true), ((true, true), vec![BoneMealStep::Vanilla]));
        assert_eq!(
            run(None, false),
            ((false, false), vec![BoneMealStep::Vanilla])
        );
        // What the hook's outcome means for growCrop: a cancel without a
        // value answers false, Mixin's default for a boolean.
        let cancelled = |returned| JavaGameHookOutcome {
            ran: 1,
            cancelled: true,
            returned,
            failed: 0,
        };
        assert_eq!(grown_from(&cancelled(None)), Some(false));
        assert_eq!(grown_from(&cancelled(Some(Value::Int(1)))), Some(true));
        assert_eq!(grown_from(&JavaGameHookOutcome::default()), None);
    }

    /// The wave-3 server sites: callbacks at `Bootstrap.bootStrap`'s HEAD
    /// (run before the jar is published, counted after), `ServerLevel.tick`'s
    /// HEAD in Mixin's simple form (the callback info alone),
    /// `LivingEntity.hurtServer`'s HEAD (cancelling only a heavy hit) and
    /// `Block.playerDestroy`'s HEAD (cancelling), with the stop log's
    /// per-site fire, cancel and override counts.
    const SERVER_SITE_MIXINS: &str = r#"package demo;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.damagesource.DamageSource;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfoReturnable;
@Mixin(net.minecraft.world.entity.LivingEntity.class)
public class HurtMixin {
    @Inject(method = "hurtServer", at = @At("HEAD"), cancellable = true)
    private void heavy(ServerLevel level, DamageSource source, float amount, CallbackInfoReturnable<Boolean> cir) {
        if (amount > 5.0f) {
            cir.setReturnValue(false);
        }
    }
}
"#;
    const LEVEL_MIXIN: &str = r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(net.minecraft.server.level.ServerLevel.class)
public class LevelMixin {
    static int ticks;
    @Inject(method = "tick", at = @At("HEAD"))
    private void onTick(CallbackInfo ci) {
        ticks++;
    }
}
"#;
    const BOOT_MIXIN: &str = r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(net.minecraft.server.Bootstrap.class)
public class BootMixin {
    static boolean booted;
    @Inject(method = "bootStrap", at = @At("HEAD"))
    private static void boot(CallbackInfo ci) {
        booted = true;
    }
}
"#;
    /// The second row of one hook name (`living_jump` stands for
    /// `LivingEntity` and `MagmaCube`): registering it must find its own row.
    const JUMP_MIXIN: &str = r#"package demo;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(net.minecraft.world.entity.monster.cubemob.MagmaCube.class)
public class JumpMixin {
    @Inject(method = "jumpFromGround", at = @At("TAIL"))
    private void jumped(CallbackInfo ci) {}
}
"#;
    const DESTROY_MIXIN: &str = r#"package demo;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerLevel;
import net.minecraft.server.level.ServerPlayer;
import net.minecraft.world.item.ItemStack;
import net.minecraft.world.level.block.entity.BlockEntity;
import net.minecraft.world.level.block.state.BlockState;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;
@Mixin(net.minecraft.world.level.block.Block.class)
public class DestroyMixin {
    @Inject(method = "playerDestroy", at = @At("HEAD"), cancellable = true)
    private void keep(ServerLevel level, ServerPlayer player, BlockPos pos, BlockState state, BlockEntity entity, ItemStack tool, CallbackInfo ci) {
        if (entity == null) {
            ci.cancel();
        }
    }
}
"#;

    /// The server-site fixture's jar under `.local/`; `None` without a JDK.
    fn server_sites_jar() -> Option<(PathBuf, PathBuf)> {
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
            .join(format!("server-sites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let classes = root.join("classes");
        std::fs::create_dir_all(&classes).ok()?;
        let stub = |class: &str| {
            let (package, name) = class.rsplit_once('.').unwrap();
            (
                format!("{}.java", class.replace('.', "/")),
                format!("package {package};\npublic class {name} {{}}\n"),
            )
        };
        let mut sources: Vec<(String, String)> = [
            "net.minecraft.core.BlockPos",
            "net.minecraft.server.Bootstrap",
            "net.minecraft.server.level.ServerLevel",
            "net.minecraft.server.level.ServerPlayer",
            "net.minecraft.world.damagesource.DamageSource",
            "net.minecraft.world.entity.LivingEntity",
            "net.minecraft.world.entity.monster.cubemob.MagmaCube",
            "net.minecraft.world.item.ItemStack",
            "net.minecraft.world.level.block.Block",
            "net.minecraft.world.level.block.entity.BlockEntity",
            "net.minecraft.world.level.block.state.BlockState",
        ]
        .into_iter()
        .map(stub)
        .collect();
        for (path, body) in [
            ("org/spongepowered/asm/mixin/Mixin.java", MIXIN),
            ("org/spongepowered/asm/mixin/injection/At.java", AT),
            ("org/spongepowered/asm/mixin/injection/Inject.java", INJECT),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfo.java",
                CALLBACK_INFO,
            ),
            (
                "org/spongepowered/asm/mixin/injection/callback/CallbackInfoReturnable.java",
                CALLBACK_INFO_RETURNABLE,
            ),
            ("demo/HurtMixin.java", SERVER_SITE_MIXINS),
            ("demo/LevelMixin.java", LEVEL_MIXIN),
            ("demo/BootMixin.java", BOOT_MIXIN),
            ("demo/DestroyMixin.java", DESTROY_MIXIN),
            ("demo/JumpMixin.java", JUMP_MIXIN),
        ] {
            sources.push((path.to_owned(), body.to_owned()));
        }
        let mut files = Vec::new();
        for (relative, body) in &sources {
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
        let jar = root.join("server-sites.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        let metadata =
            r#"{"id":"server_sites","version":"1","entrypoints":{},"mixins":["demo.mixins.json"]}"#;
        let config = r#"{"package":"demo","mixins":["HurtMixin","LevelMixin","BootMixin","DestroyMixin","JumpMixin"]}"#;
        for (name, body) in [("fabric.mod.json", metadata), ("demo.mixins.json", config)] {
            zip.start_file(name, options).ok()?;
            zip.write_all(body.as_bytes()).ok()?;
        }
        for name in [
            "HurtMixin",
            "LevelMixin",
            "BootMixin",
            "DestroyMixin",
            "JumpMixin",
        ] {
            let name = format!("demo/{name}.class");
            zip.start_file(&name, options).ok()?;
            zip.write_all(&std::fs::read(classes.join(&name)).ok()?)
                .ok()?;
        }
        zip.finish().ok()?;
        Some((root, jar))
    }

    #[test]
    fn the_server_sites_bind_fire_and_count_what_their_callbacks_did() {
        let _lock = super::super::java_mods::test_lock();
        let Some((root, jar_path)) = server_sites_jar() else {
            eprintln!("skipping: no JDK");
            return;
        };
        let role = HandlerRole::DedicatedServer;
        clear_java_game_hooks_as(role);
        let jar = NativeJar::from_path(&jar_path).expect("the fixture jar");
        let weave = jar.plan_load_time_weave_with_game_hooks(MixinSide::Server);
        let registrations = jar.register_game_hooks(&weave).expect("registrations");
        assert_eq!(
            registrations.hooks().collect::<Vec<_>>(),
            [
                "java_block_player_destroy",
                "java_bootstrap_head",
                "java_level_tick_head",
                "java_living_hurt",
                "living_jump",
            ],
            "{:#?}",
            weave.skipped
        );
        // The bootstrap fires on the jar's own host before it is published.
        let mut host = StdHost::new();
        fire_java_bootstrap(
            ExecutionRole::DedicatedServer,
            &jar_path,
            &jar,
            &registrations,
            &mut host,
        );
        publish_java_game_hooks(role, &jar_path, Arc::new(jar), registrations, &host);

        // hurtServer, from the damage code: only the heavy hit is refused.
        set_thread_role(role);
        assert!(fire_living_hurt(
            3,
            "minecraft:zombie",
            "minecraft:generic",
            8.0
        ));
        assert!(!fire_living_hurt(
            3,
            "minecraft:zombie",
            "minecraft:generic",
            2.0
        ));

        // ServerLevel.tick in the simple form: the callback info alone.
        let mut context = GameHookContext::new();
        let level = context.insert(GameObject::level("minecraft:overworld"));
        let tick = run_java_game_hook(
            role,
            "java_level_tick_head",
            &mut context,
            Some(level),
            &[Value::Ref(None)],
        );
        assert_eq!((tick.ran, tick.cancelled, tick.failed), (1, false, 0));

        // playerDestroy: a block without a block entity keeps its drops back.
        let destroy = |block_entity: bool| {
            let mut context = GameHookContext::new();
            let block = context.insert(GameObject::opaque(
                "net/minecraft/world/level/block/Block",
                "minecraft_stone",
            ));
            let mut args = vec![
                context.insert(GameObject::level("minecraft:overworld")),
                context.insert(GameObject::player(7, "Steve")),
                context.insert(GameObject::opaque("net/minecraft/core/BlockPos", "0_64_0")),
                context.insert(GameObject::opaque(
                    "net/minecraft/world/level/block/state/BlockState",
                    "stone",
                )),
            ];
            args.push(if block_entity {
                context.insert(GameObject::opaque(
                    "net/minecraft/world/level/block/entity/BlockEntity",
                    "chest",
                ))
            } else {
                Value::Ref(None)
            });
            args.push(context.insert(GameObject::item_stack("pick", "minecraft:iron_pickaxe", 1)));
            run_java_game_hook(
                role,
                "java_block_player_destroy",
                &mut context,
                Some(block),
                &args,
            )
            .cancelled
        };
        assert!(destroy(false));
        assert!(!destroy(true));

        let summary = java_game_hook_summary(role);
        for line in [
            "server-sites.jar java_bootstrap_head: fired 1",
            "server-sites.jar java_level_tick_head: fired 1",
            "server-sites.jar java_living_hurt: fired 2, cancelled 1, overridden 1",
            "server-sites.jar java_block_player_destroy: fired 2, cancelled 1",
        ] {
            assert!(summary.iter().any(|l| l == line), "{line} in {summary:#?}");
        }
        clear_java_game_hooks_as(role);
        let _ = std::fs::remove_dir_all(root);
    }
}
