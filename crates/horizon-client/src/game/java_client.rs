//! The Java mods' client surface, driven from the window: client events, key
//! mappings and HUD elements.
//!
//! The client batch (`java_mods`, client role) hands each bootstrapped jar to
//! [`adopt`]. A jar that registered a client listener (`ClientTickEvents`,
//! `ClientLifecycleEvents`, `ClientPlayConnectionEvents`, NeoForge's
//! `ClientTickEvent`), a key mapping or a HUD element is kept here with the
//! host its entrypoints ran on. NeoForge's registration events
//! (`RegisterKeyMappingsEvent`, `RegisterGuiLayersEvent`) fire once, at
//! adoption, as FML fires them after the mod constructors.
//!
//! The window's points call the `java_client_*` methods below:
//!
//! - the client tick, each 50 ms step in a world or not (game.rs's step
//!   loop): `START_CLIENT_TICK`/`END_CLIENT_TICK`, and once `CLIENT_STARTED`
//!   first; `JOIN` the first tick the client is in a world with its player
//!   (Horizon makes the player at the first teleport, after the login vanilla
//!   fires `JOIN` at, so a listener reading `client.player` finds one);
//! - the client level's tick (`tick_remote`);
//! - leaving a world (`remote.rs`'s disconnect, and the tick when the world is
//!   gone by another path) and stopping (`save_for_exit`);
//! - a key or mouse button (`native_key_event`): a press while playing clicks
//!   every Java mapping bound to it, a release lets go;
//! - the HUD (`draw_extras`, hidden under F1): every HUD element, drawn
//!   through `GuiGraphicsExtractor`'s bounded façade.
//!
//! Each first checks [`ACTIVE`], one relaxed atomic load: with no Java client
//! surface, nothing else runs.
//!
//! Mappings join Horizon's Controls through `horizon_client_input::mod_mappings`
//! (named by the mod's own lang keys, saved as `key_<name>` in `options.txt`).
//!
//! A listener or element that fails (an unmodelled member, its time budget, a
//! refused bytecode, a panic) is disabled and logged once with the reason.

use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use horizon_client::{
    chat::ChatSource,
    hud::{HudBatch, rgba},
    items::ItemStack,
    text::Text,
};
use horizon_client_input::mod_mappings;
use horizon_native_java::{
    ExecutionBudget, NativeJar, StdHost,
    client_surface::{
        ClientCamera, ClientEffect, ClientItem, ClientLevel, ClientMessage, ClientPlayer,
        ClientSnapshot, ClientStack, ClientText, DrawOp, HudLayer, ItemTable, KeyPress, StackSlot,
        TextMetrics,
    },
    events::{EventContext, EventFacade, EventFiring, GameEvent, GameListener},
};

use super::{
    Game, Screen,
    execution_role::ExecutionRole,
    java_payloads::{ResidentHost, lock_resident},
};

/// One listener as the store keeps it.
struct Slot {
    listener: GameListener,
    disabled: bool,
    /// Why it was disabled, for diagnostics.
    refusal: Option<String>,
}

/// One HUD element as the store keeps it.
struct LayerSlot {
    layer: HudLayer,
    disabled: bool,
    refusal: Option<String>,
    /// Frames it has drawn, for its warm-up budget.
    frames: u32,
    /// Frames in a row it ran out of time in.
    overruns: u32,
}

/// One registered mapping: its Controls name and input state.
struct JavaKey {
    name: &'static str,
    press: KeyPress,
}

/// One jar with a client surface.
struct ClientJar {
    file: String,
    jar: Arc<NativeJar>,
    /// The jar's resident host (`java_mods::adopt_run`): the one its
    /// entrypoints ran on, shared with its payload handlers, so a HUD element
    /// draws what a received frame stored.
    host: ResidentHost,
    slots: Vec<Slot>,
    /// In the host's `client_key_mappings` order; a mapping Controls could
    /// not take keeps an empty name and never clicks.
    keys: Vec<JavaKey>,
    layers: Vec<LayerSlot>,
    /// The jar's own lang lines (current language over `en_us`), for
    /// `Font.width` of its translatable text.
    translations: BTreeMap<String, String>,
    metrics: Option<Arc<TextMetrics>>,
}

static STORE: Mutex<Vec<ClientJar>> = Mutex::new(Vec::new());

/// [`GameEvent::bit`]s with a live listener, plus [`HUD_BIT`] and
/// [`KEYS_BIT`]: the fast path.
static ACTIVE: AtomicU64 = AtomicU64::new(0);
/// Bit 31: an enabled HUD element. Client events stay below it (30 events).
const HUD_BIT: u64 = 1 << 63;
/// Bit 30's place is an event's, so mappings are flagged separately.
static HAS_KEYS: AtomicBool = AtomicBool::new(false);
/// New mappings joined Controls: the window copies the player's keys for
/// them out of `options.txt` at its next tick.
static KEYS_PENDING: AtomicBool = AtomicBool::new(false);
/// The player's max health (high half, `f32` bits) and armour value (low
/// half) the last snapshot read, for a `&self` snapshot (a tooltip).
static LAST_ATTRIBUTES: AtomicU64 = AtomicU64::new(0x41a0_0000_u64 << 32);
/// `CLIENT_STARTED` fired.
static STARTED: AtomicBool = AtomicBool::new(false);
/// `JOIN` fired and `DISCONNECT` has not.
static JOINED: AtomicBool = AtomicBool::new(false);

static RAN: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);

/// The time one listener may take at one firing: a tick listener runs every
/// 50 ms in the frame loop, so its bound keeps a looping one from stalling
/// the window; lifecycle and registration listeners load configs.
fn budget(event: GameEvent) -> Duration {
    match event {
        GameEvent::ClientStarted
        | GameEvent::ClientStopping
        | GameEvent::ClientRegisterKeyMappings
        | GameEvent::ClientRegisterGuiLayers => Duration::from_secs(2),
        _ => Duration::from_millis(10),
    }
}

/// The time one HUD element may take in one frame. A frame at 60 fps has
/// 16 ms for everything; an element over this is disabled, not drawn late.
const HUD_BUDGET: Duration = Duration::from_millis(4);
/// Frames in a row an element may run out of time in before it is disabled.
const HUD_OVERRUNS: u32 = 8;
/// An element's first frames, which run its one-time class initialisation.
const HUD_WARMUP_FRAMES: u32 = 3;
/// The time one of those frames may take.
const HUD_WARMUP_BUDGET: Duration = Duration::from_millis(100);

fn store() -> std::sync::MutexGuard<'static, Vec<ClientJar>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn recompute(store: &[ClientJar]) {
    let mut mask = 0;
    for jar in store {
        for slot in jar.slots.iter().filter(|s| !s.disabled) {
            mask |= slot.listener.event.bit();
        }
        if jar.layers.iter().any(|l| !l.disabled) {
            mask |= HUD_BIT;
        }
    }
    ACTIVE.store(mask, Ordering::Relaxed);
    HAS_KEYS.store(
        store
            .iter()
            .any(|jar| jar.keys.iter().any(|k| !k.name.is_empty())),
        Ordering::Relaxed,
    );
}

fn listens(event: GameEvent) -> bool {
    ACTIVE.load(Ordering::Relaxed) & event.bit() != 0
}

/// Forgets the previous client batch's jars (the mappings stay in Controls:
/// the next batch registers the same names again).
pub(super) fn clear() {
    let mut store = store();
    store.clear();
    recompute(&store);
    STARTED.store(false, Ordering::Relaxed);
    JOINED.store(false, Ordering::Relaxed);
}

/// The client listeners of a host, in registration order.
fn client_listeners(jar: &NativeJar, host: &StdHost) -> Vec<GameListener> {
    host.game_listeners(jar, false)
        .listeners
        .into_iter()
        .filter(|l| l.event.is_client())
        .collect()
}

/// Whether a client-role bootstrap left something the window drives, so its
/// run needs the host kept (and must not be replaced by a cached replay).
pub(super) fn has_surface(
    jar: Option<&NativeJar>,
    host: Option<&StdHost>,
    role: ExecutionRole,
) -> bool {
    match (jar, host) {
        (Some(jar), Some(host)) if role.is_client() => {
            !host.client_key_mappings().is_empty()
                || !host.client_hud_layers(jar).0.is_empty()
                || !client_listeners(jar, host).is_empty()
        }
        _ => false,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Keeps one client-role jar's surface. Returns how many listeners,
/// mappings and elements it kept; a jar with none keeps nothing.
pub(super) fn adopt(
    role: ExecutionRole,
    path: &Path,
    jar: &Arc<NativeJar>,
    resident: &ResidentHost,
) -> usize {
    if !role.is_client() {
        return 0;
    }
    let mut host = lock_resident(resident);
    let listeners = client_listeners(jar, &host);
    let registers = |l: &GameListener| {
        matches!(
            l.event,
            GameEvent::ClientRegisterKeyMappings | GameEvent::ClientRegisterGuiLayers
        )
    };
    if listeners.is_empty()
        && host.client_key_mappings().is_empty()
        && host.client_hud_layers(jar).0.is_empty()
    {
        return 0;
    }
    let file = file_name(path);
    let mut slots: Vec<Slot> = listeners
        .into_iter()
        .map(|listener| Slot {
            listener,
            disabled: false,
            refusal: None,
        })
        .collect();
    // FML fires the registration events once the mod constructors ran.
    for event in [
        GameEvent::ClientRegisterKeyMappings,
        GameEvent::ClientRegisterGuiLayers,
    ] {
        let firing = EventFiring::new(
            event,
            Arc::new(EventFacade::default()),
            EventContext::default(),
        );
        run_listeners(
            &file,
            jar,
            &mut host,
            &mut slots,
            firing,
            Arc::default(),
            &mut [],
        );
    }
    slots.retain(|slot| !registers(&slot.listener));
    let translations = jar_translations(path);
    let mut keys = Vec::new();
    for mapping in host.client_key_mappings() {
        let name = mod_mappings::register(&mapping.name, &mapping.default_key, &mapping.category);
        if name.is_none() {
            log::warn!(
                "Horizon: Java mod {file} key mapping {} not added to Controls (a vanilla name, or more than {} mod mappings)",
                mapping.name,
                mod_mappings::MAX
            );
        }
        keys.push(JavaKey {
            name: name.unwrap_or(""),
            press: KeyPress::default(),
        });
    }
    let (layers, refused) = host.client_hud_layers(jar);
    for reason in host.client_refused_registrations().iter().chain(&refused) {
        log::warn!("Horizon: Java mod {file} client registration refused: {reason}");
    }
    let kept = slots.len() + keys.len() + layers.len();
    if kept == 0 {
        return 0;
    }
    let events: Vec<String> = slots
        .iter()
        .map(|s| format!("{} ({})", s.listener.source, s.listener.event.name()))
        .collect();
    log::info!(
        "Horizon: Java mod {file} client surface: {} listener(s) [{}], {} key mapping(s), {} HUD element(s)",
        slots.len(),
        events.join(", "),
        keys.len(),
        layers.len()
    );
    if !keys.is_empty() {
        KEYS_PENDING.store(true, Ordering::Relaxed);
    }
    drop(host);
    let mut store = store();
    store.push(ClientJar {
        file,
        jar: Arc::clone(jar),
        host: Arc::clone(resident),
        slots,
        keys,
        layers: layers
            .into_iter()
            .map(|layer| LayerSlot {
                layer,
                disabled: false,
                refusal: None,
                frames: 0,
                overruns: 0,
            })
            .collect(),
        translations,
        metrics: None,
    });
    recompute(&store);
    kept
}

/// The jar's `assets/<ns>/lang/{en_us,<current>}.json` lines, handed to
/// Controls' fallback table and kept for the jar's text widths. Bounded: a
/// lang file over 1 MiB is skipped.
fn jar_translations(path: &Path) -> BTreeMap<String, String> {
    let mut merged = BTreeMap::new();
    let Ok(file) = std::fs::File::open(path) else {
        return merged;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return merged;
    };
    let current = horizon_client::lang::code();
    let mut codes = vec![horizon_client::lang::DEFAULT.to_owned()];
    if current != codes[0] {
        codes.push(current);
    }
    let names: Vec<String> = zip
        .file_names()
        .filter_map(|name| name.ok().map(|name| name.into_owned()))
        .collect();
    for code in codes {
        let suffix = format!("/lang/{code}.json");
        for name in names
            .iter()
            .filter(|n| n.starts_with("assets/") && n.ends_with(&suffix))
        {
            let Ok(mut entry) = zip.by_name(name) else {
                continue;
            };
            if entry.size() > 1 << 20 {
                continue;
            }
            let mut text = String::new();
            if entry.read_to_string(&mut text).is_err() {
                continue;
            }
            let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&text) else {
                continue;
            };
            let lines: Vec<(String, String)> = map
                .into_iter()
                .filter_map(|(k, v)| Some((k, v.as_str()?.to_owned())))
                .collect();
            mod_mappings::set_translations(&code, lines.iter().cloned());
            merged.extend(lines);
        }
    }
    merged
}

/// Runs every enabled listener of `firing.event` in `slots` inside one
/// client frame, disabling one that fails. Returns what the frame left.
fn run_listeners(
    file: &str,
    jar: &NativeJar,
    host: &mut StdHost,
    slots: &mut [Slot],
    mut firing: EventFiring,
    snapshot: Arc<ClientSnapshot>,
    keys: &mut [JavaKey],
) -> (Vec<ClientMessage>, bool) {
    let event = firing.event;
    let presses: Vec<KeyPress> = keys.iter().map(|k| k.press).collect();
    let firing_snapshot = Arc::clone(&snapshot);
    host.begin_client_frame(snapshot, &presses);
    let mut disabled_any = false;
    for slot in slots
        .iter_mut()
        .filter(|s| !s.disabled && s.listener.event == event)
    {
        let limit = ExecutionBudget::within(budget(event));
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            jar.run_game_listener(&slot.listener, &mut firing, host, &limit)
        }));
        let reason = match run {
            Ok(Ok(run)) => {
                if run.ran {
                    RAN.fetch_add(1, Ordering::Relaxed);
                }
                if run.stop {
                    break;
                }
                continue;
            }
            Ok(Err(error)) => error.to_string(),
            Err(_) => "panicked".to_owned(),
        };
        let target = slot.listener.target();
        refuse(
            file,
            &format!(
                "listener {}#{} on {}",
                target.owner, target.name, slot.listener.source
            ),
            &reason,
        );
        slot.disabled = true;
        slot.refusal = Some(reason);
        disabled_any = true;
    }
    let mut out = host.end_client_frame();
    for (key, press) in keys.iter_mut().zip(std::mem::take(&mut out.keys)) {
        key.press = press;
    }
    let mut messages = out.messages;
    messages.extend(run_tasks(file, jar, host, out.tasks, &firing_snapshot));
    (messages, disabled_any)
}

/// Runs the `Minecraft.execute` tasks a firing queued, each in its own
/// frame, as vanilla's `runAllTasks` runs them later in the same tick. A
/// task may queue more; the rounds are bounded. A task that fails is
/// logged and dropped (it ran once; there is nothing to disable). Returns
/// the chat lines they sent.
fn run_tasks(
    file: &str,
    jar: &NativeJar,
    host: &mut StdHost,
    mut tasks: Vec<horizon_native_java::Value>,
    snapshot: &Arc<ClientSnapshot>,
) -> Vec<ClientMessage> {
    let mut messages = Vec::new();
    for _ in 0..4 {
        if tasks.is_empty() {
            break;
        }
        let mut queued = Vec::new();
        for task in tasks {
            host.begin_client_frame(Arc::clone(snapshot), &[]);
            let limit = ExecutionBudget::within(Duration::from_millis(10));
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                jar.run_client_task(&task, host, &limit)
            }));
            let out = host.end_client_frame();
            match run {
                Ok(Ok(())) => {
                    RAN.fetch_add(1, Ordering::Relaxed);
                    messages.extend(out.messages);
                    queued.extend(out.tasks);
                }
                Ok(Err(error)) => refuse(file, "Minecraft.execute task", &error.to_string()),
                Err(_) => refuse(file, "Minecraft.execute task", "panicked"),
            }
        }
        tasks = queued;
    }
    messages
}

/// Logs one disabled callback, once.
fn refuse(file: &str, what: &str, reason: &str) {
    REFUSED.fetch_add(1, Ordering::Relaxed);
    let mut reason = reason.replace(['\n', '\r'], " ");
    if reason.chars().count() > 240 {
        reason = reason.chars().take(239).collect();
        reason.push('…');
    }
    log::warn!("Horizon: Java mod {file} client {what} refused and disabled: {reason}");
}

/// Kept callbacks across every jar, disabled ones, runs and refusals:
/// `(kept, disabled, ran, refused)`.
pub(super) fn stats() -> (usize, usize, u64, u64) {
    let store = store();
    let kept = store.iter().map(|j| j.slots.len() + j.layers.len()).sum();
    let disabled = store
        .iter()
        .map(|j| {
            j.slots.iter().filter(|s| s.disabled).count()
                + j.layers.iter().filter(|l| l.disabled).count()
        })
        .sum();
    (
        kept,
        disabled,
        RAN.load(Ordering::Relaxed),
        REFUSED.load(Ordering::Relaxed),
    )
}

/// The game font's advances for chars below this, for `Font.width`.
const METRIC_CHARS: u32 = 0x500;

impl Game<'_> {
    /// Only the window fires client events: never a server thread or a
    /// dedicated server.
    fn java_client_window(&self) -> bool {
        self.serving.is_none()
            && self.options.dedicated.is_none()
            && self.options.execution_role.is_client()
    }

    /// The client state a firing shows. `gui` is the frame's GUI size, when
    /// drawing.
    fn java_client_snapshot(&mut self, gui: Option<[f32; 2]>) -> ClientSnapshot {
        let max_health = self.player.as_mut().map_or(20.0, |p| {
            p.attribute("minecraft:max_health").unwrap_or(20.0) as f32
        });
        let armor_value = self.player.as_mut().map_or(0, |p| {
            p.attribute("minecraft:armor")
                .or_else(|_| p.attribute("armor"))
                .unwrap_or(0.0) as i32
        });
        LAST_ATTRIBUTES.store(
            u64::from(max_health.to_bits()) << 32 | armor_value as u32 as u64,
            Ordering::Relaxed,
        );
        // Only the vanilla mappings `options.keyX` names are ever asked for.
        let vanilla = [
            "key.forward",
            "key.left",
            "key.back",
            "key.right",
            "key.jump",
            "key.sneak",
            "key.sprint",
            "key.attack",
            "key.use",
        ];
        let vanilla_keys_down: Vec<String> = vanilla
            .into_iter()
            .filter(|name| self.native_mapping(name).is_some_and(|(down, _)| down))
            .map(str::to_owned)
            .collect();
        ClientSnapshot {
            vanilla_keys_down,
            ..self.java_client_snapshot_with(gui, max_health, armor_value)
        }
    }

    /// [`Self::java_client_snapshot`] with the player's attributes given: a
    /// `&self` caller (a tooltip) passes the last ones read.
    fn java_client_snapshot_with(
        &self,
        gui: Option<[f32; 2]>,
        max_health: f32,
        armor_value: i32,
    ) -> ClientSnapshot {
        let window = self
            .renderer
            .as_ref()
            .map_or([1280, 720], |r| [r.config.width, r.config.height]);
        let batch = HudBatch::new(window);
        let gui = gui.unwrap_or([batch.width, batch.height]);
        let in_world = self.remote.as_ref().is_some_and(|r| r.has_level());
        let player = if in_world {
            self.player.as_ref().map(|p| ClientPlayer {
                uuid: self.remote.as_ref().map_or(0, |r| r.profile()),
                name: self
                    .remote
                    .as_ref()
                    .map_or_else(String::new, |r| r.profile_name.clone()),
                entity_id: self.remote.as_ref().map_or(0, |r| r.entity_id),
                pos: [p.position.x, p.position.y, p.position.z],
                yaw: self.yaw,
                pitch: self.pitch,
                health: p.health.health(),
                max_health,
                creative: self.creative,
                spectator: p.spectator,
                sneaking: p.pose == horizon_client::pose::Pose::Crouching,
                sprinting: p.sprinting,
                on_ground: p.ground.on_ground,
                fall_flying: p.fall_flying,
                game_mode: match self.mode {
                    horizon_client::command::args::GameMode::Survival => "SURVIVAL",
                    horizon_client::command::args::GameMode::Creative => "CREATIVE",
                    horizon_client::command::args::GameMode::Adventure => "ADVENTURE",
                    horizon_client::command::args::GameMode::Spectator => "SPECTATOR",
                }
                .to_owned(),
                inventory: (0..36)
                    .map(|i| self.java_client_stack(self.inventory.get(i).and_then(Option::as_ref)))
                    .collect(),
                selected: self.selected.min(8),
                // `armor` is head first; the façade keeps `Inventory`'s feet-first order.
                armor: [3, 2, 1, 0].map(|i| self.java_client_stack(self.armor[i].as_ref())),
                offhand: self.java_client_stack(self.offhand.as_ref()),
                effects: self.java_client_effects(),
                food: p.health.food.food,
                saturation: p.health.food.saturation,
                armor_value,
                absorption: p.health.absorption(),
                air: p.air,
                max_air: 300,
                xp_level: self.experience.level,
                xp_progress: self.experience.progress,
                xp_total: self.experience.total,
            })
        } else {
            None
        };
        let level = (in_world && player.is_some()).then(|| ClientLevel {
            dimension: self.dim.id().to_owned(),
            game_time: self.world.game_time,
            day_time: self.world.day_time,
            min_y: self.world.shape.min_y,
            height: self.world.shape.height,
        });
        ClientSnapshot {
            player,
            level,
            paused: self.pause,
            single_player: self.integrated.is_some(),
            hide_gui: self.ui.hide_gui,
            gui_ticks: self.visual_ticks() as i32,
            render_distance: self.settings.render_distance,
            left_handed: self.settings.main_hand_left,
            lan_open: self.lan.is_some(),
            camera: in_world
                .then(|| self.eye_position())
                .flatten()
                .map(|pos| ClientCamera {
                    pos,
                    yaw: self.yaw,
                    pitch: self.pitch,
                    detached: !self.camera_type.first_person(),
                }),
            screen_open: self.title.is_some() || self.screen != Screen::Playing,
            debug_screen: self.debug,
            gui_size: [gui[0] as i32, gui[1] as i32],
            window_size: [window[0] as i32, window[1] as i32],
            gui_scale: f64::from(batch.scale),
            partial_tick: self.clock.fraction() as f32,
            fps: self.frame_reading().0 as i32,
            // Read by the `&mut self` caller ([`Self::java_client_snapshot`]).
            vanilla_keys_down: Vec::new(),
            metrics: Arc::default(),
            items: self.java_client_items(),
            tooltip_stack: None,
            tooltip_lines: Vec::new(),
            advanced_tooltips: self.settings.file.bool("advancedItemTooltips"),
        }
    }

    /// `ItemTooltipCallback` / `ItemTooltipEvent`: every Java tooltip
    /// listener edits `lines` (the name line first, as vanilla hands them),
    /// jar after jar. A line a listener kept is the game's own line with its
    /// style; a line it added is the mod's component. A listener that fails
    /// is disabled and leaves the lines as they were.
    pub(super) fn java_client_tooltip(&self, stack: &ItemStack, lines: &mut Vec<(Text, u32)>) {
        if !listens(GameEvent::ClientItemTooltip) || !self.java_client_window() {
            return;
        }
        let packed = LAST_ATTRIBUTES.load(Ordering::Relaxed);
        let mut base = self.java_client_snapshot_with(
            None,
            f32::from_bits((packed >> 32) as u32),
            packed as u32 as i32,
        );
        base.tooltip_stack = Some(self.java_client_stack(Some(stack)));
        let mut store = store();
        let mut disabled = false;
        for entry in store.iter_mut() {
            if !entry
                .slots
                .iter()
                .any(|s| !s.disabled && s.listener.event == GameEvent::ClientItemTooltip)
            {
                continue;
            }
            let originals: Vec<String> = lines.iter().map(|(text, _)| text.plain()).collect();
            let snapshot = Arc::new(ClientSnapshot {
                tooltip_lines: originals.clone(),
                ..base.clone()
            });
            let mut host = lock_resident(&entry.host);
            let mut firing = EventFiring::new(
                GameEvent::ClientItemTooltip,
                Arc::new(EventFacade::default()),
                EventContext::default(),
            );
            host.begin_client_frame(snapshot, &[]);
            let mut failed = false;
            for slot in entry
                .slots
                .iter_mut()
                .filter(|s| !s.disabled && s.listener.event == GameEvent::ClientItemTooltip)
            {
                let limit = ExecutionBudget::within(budget(GameEvent::ClientItemTooltip));
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    entry
                        .jar
                        .run_game_listener(&slot.listener, &mut firing, &mut host, &limit)
                }));
                let reason = match run {
                    Ok(Ok(_)) => {
                        RAN.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "panicked".to_owned(),
                };
                let target = slot.listener.target();
                refuse(
                    &entry.file,
                    &format!("tooltip listener {}#{}", target.owner, target.name),
                    &reason,
                );
                slot.disabled = true;
                slot.refusal = Some(reason);
                disabled = true;
                failed = true;
                break;
            }
            let out = host.end_client_frame();
            if failed {
                continue;
            }
            let Some(edited) = out.tooltip else {
                continue;
            };
            // A line the mod kept (the same text) is the game's own line with
            // its style, each matched once; a line it added is its component;
            // a line it removed is gone, as in vanilla.
            let mut kept: std::collections::VecDeque<((Text, u32), String)> =
                lines.drain(..).zip(originals).collect();
            let mut merged = Vec::with_capacity(edited.len());
            for text in edited {
                let found = match &text {
                    ClientText::Literal(s) => kept.iter().position(|(_, o)| o == s),
                    _ => None,
                };
                match found.and_then(|i| kept.remove(i)) {
                    Some((line, _)) => merged.push(line),
                    None => merged.push((java_text(&text), u32::MAX)),
                }
            }
            *lines = merged;
        }
        if disabled {
            recompute(&store);
        }
    }

    /// A stack as the façade shows it; `None` is an empty slot.
    fn java_client_stack(&self, stack: Option<&ItemStack>) -> ClientStack {
        let Some(stack) = stack.filter(|s| &*s.item != "minecraft:air" && s.count > 0) else {
            return ClientStack::empty();
        };
        let info = self.res.items().get(&stack.item);
        ClientStack {
            item: stack.item.to_string(),
            count: stack.count as i32,
            damage: stack.damage as i32,
            max_damage: info.map_or(0, |i| i.max_damage as i32),
            max_stack: info.map_or(64, |i| i.max_stack as i32),
            name: stack
                .custom_name()
                .unwrap_or_else(|| self.pack.names.display_item(&stack.item)),
            foil: stack.has_foil(false),
            enchanted: stack.has_any_enchantments(),
            block: info.is_some_and(|i| i.block.is_some()),
        }
    }

    /// The player's active effects, in the order the game keeps them.
    fn java_client_effects(&self) -> Vec<ClientEffect> {
        use horizon_client_core::potions::{Category, effect};
        self.potions
            .effects
            .list
            .iter()
            .map(|e| {
                let def = effect(e.id);
                ClientEffect {
                    id: horizon_client_core::potions::namespaced(e.id).to_owned(),
                    amplifier: e.amplifier,
                    duration: e.duration,
                    ambient: e.ambient,
                    visible: e.visible,
                    // Horizon keeps no showIcon flag: every effect shows its icon.
                    show_icon: true,
                    category: match def.map(|d| d.category) {
                        Some(Category::Beneficial) => "BENEFICIAL",
                        Some(Category::Harmful) => "HARMFUL",
                        _ => "NEUTRAL",
                    }
                    .to_owned(),
                    color: def.map_or(0, |d| d.color as i32),
                }
            })
            .collect()
    }

    /// Every item's facts, built once (names in the language of the first
    /// firing) and shared by every snapshot.
    fn java_client_items(&self) -> Arc<ItemTable> {
        static TABLE: Mutex<Option<Arc<ItemTable>>> = Mutex::new(None);
        let mut table = TABLE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(table) = &*table {
            return Arc::clone(table);
        }
        let items = self.res.items();
        let built: ItemTable = items
            .items
            .values()
            .map(|info| {
                (
                    info.id.to_string(),
                    ClientItem {
                        max_stack: info.max_stack as i32,
                        max_damage: info.max_damage as i32,
                        name: self.pack.names.display_item(&info.id),
                        block: info.block.is_some(),
                    },
                )
            })
            .collect();
        let built = Arc::new(built);
        *table = Some(Arc::clone(&built));
        built
    }

    /// Fires `event` at every jar's listeners, then shows what they sent.
    fn java_client_event(&mut self, event: GameEvent) {
        if !listens(event) || !self.java_client_window() {
            return;
        }
        let snapshot = Arc::new(self.java_client_snapshot(None));
        let mut messages = Vec::new();
        let mut store = store();
        let mut disabled = false;
        for entry in store.iter_mut() {
            if !entry
                .slots
                .iter()
                .any(|s| !s.disabled && s.listener.event == event)
            {
                continue;
            }
            let firing = EventFiring::new(
                event,
                Arc::new(EventFacade::default()),
                EventContext::default(),
            );
            let mut host = lock_resident(&entry.host);
            let (sent, refused) = run_listeners(
                &entry.file,
                &entry.jar,
                &mut host,
                &mut entry.slots,
                firing,
                Arc::clone(&snapshot),
                &mut entry.keys,
            );
            messages.extend(sent);
            disabled |= refused;
        }
        if disabled {
            recompute(&store);
        }
        drop(store);
        self.java_client_messages(messages);
    }

    /// Chat lines and action-bar messages the mods sent.
    fn java_client_messages(&mut self, messages: Vec<ClientMessage>) {
        for message in messages {
            match message {
                ClientMessage::Chat(text) => {
                    let line = java_text(&text);
                    let focused = self.screen == Screen::Chat;
                    let ticks = self.visual_ticks();
                    self.ui.chat.add(
                        &self.gui.atlas,
                        line,
                        None,
                        ChatSource::Client,
                        ticks,
                        focused,
                    );
                }
                ClientMessage::Overlay(text) => self.titles.set_overlay(java_text(&text), false),
            }
        }
    }

    /// One client tick's start (`start`) or end: `CLIENT_STARTED` before the
    /// first, the player's keys for new mappings, `JOIN` once in a world.
    pub(super) fn java_client_tick(&mut self, start: bool) {
        let active = ACTIVE.load(Ordering::Relaxed) != 0 || HAS_KEYS.load(Ordering::Relaxed);
        if !active || !self.java_client_window() {
            return;
        }
        if start {
            if KEYS_PENDING.swap(false, Ordering::Relaxed) {
                self.java_client_sync_keys();
            }
            if !STARTED.swap(true, Ordering::Relaxed) {
                self.java_client_event(GameEvent::ClientStarted);
            }
            let in_world = self.remote.as_ref().is_some_and(|r| r.joined) && self.player.is_some();
            let joined = JOINED.load(Ordering::Relaxed);
            if in_world && !joined {
                JOINED.store(true, Ordering::Relaxed);
                self.java_client_event(GameEvent::ClientJoin);
            } else if joined && self.remote.is_none() {
                // Left by a path that skipped `java_client_disconnected`.
                self.java_client_disconnected();
            }
            self.java_client_event(GameEvent::ClientTickStart);
        } else {
            self.java_client_event(GameEvent::ClientTickEnd);
        }
    }

    /// The client level's tick start (`start`) or end.
    pub(super) fn java_client_level_tick(&mut self, start: bool) {
        if self.remote.as_ref().is_some_and(|r| r.has_level()) {
            self.java_client_event(if start {
                GameEvent::ClientLevelTickStart
            } else {
                GameEvent::ClientLevelTickEnd
            });
        }
    }

    /// The client is leaving its world (`Minecraft.disconnect`).
    pub(super) fn java_client_disconnected(&mut self) {
        if JOINED.swap(false, Ordering::Relaxed) {
            self.java_client_event(GameEvent::ClientDisconnect);
        }
        // Keys held when the world goes are let go (`KeyMapping.releaseAll`).
        for jar in store().iter_mut() {
            for key in &mut jar.keys {
                key.press = KeyPress::default();
            }
        }
    }

    /// The client is stopping; logs what the surface did this session.
    pub(super) fn java_client_stopping(&mut self) {
        self.java_client_event(GameEvent::ClientStopping);
        let (kept, disabled, ran, refused) = stats();
        if kept > 0 {
            log::info!(
                "Horizon: Java client callbacks: {kept} kept ({disabled} disabled), {ran} run(s), {refused} refusal(s)"
            );
        }
    }

    /// The player's keys for mappings that joined Controls after the
    /// settings were read: `options.txt`'s `key_<name>` line, if any.
    fn java_client_sync_keys(&mut self) {
        for (name, _, _) in mod_mappings::registered() {
            let key = self.settings.file.key_binding(name);
            if !key.is_empty() && self.settings.keys.get(name) != key {
                self.settings.keys.set(name, &key);
            }
        }
    }

    /// A physical key or mouse button (`key.keyboard.c`, `key.mouse.left`)
    /// went down or up. A press while playing (no screen open, as
    /// `KeyMapping.click` needs) clicks every Java mapping bound to it.
    pub(super) fn java_client_key(&mut self, input: &str, pressed: bool) {
        if !HAS_KEYS.load(Ordering::Relaxed) || !self.java_client_window() {
            return;
        }
        let playing = self.screen == Screen::Playing;
        let keys = &self.settings.keys;
        for jar in store().iter_mut() {
            for key in jar.keys.iter_mut().filter(|k| !k.name.is_empty()) {
                if keys.get(key.name) != input {
                    continue;
                }
                if pressed && playing {
                    key.press.down = true;
                    key.press.clicks = key.press.clicks.saturating_add(1);
                } else if !pressed {
                    key.press.down = false;
                }
            }
        }
    }

    /// The font's advances, shared by every jar's metrics.
    fn java_client_advances(&self) -> Vec<f32> {
        let font = &self.gui.atlas.font;
        let style = horizon_client::text::Style::default();
        (0..METRIC_CHARS)
            .map(|c| char::from_u32(c).map_or(0.0, |c| font.advance(c, &style)))
            .collect()
    }

    /// Every enabled HUD element, drawn into the HUD. An element that fails
    /// is disabled and draws nothing (its partial draws are dropped).
    pub(super) fn java_client_hud(&mut self, batch: &mut HudBatch) {
        if ACTIVE.load(Ordering::Relaxed) & HUD_BIT == 0 || !self.java_client_window() {
            return;
        }
        let base = self.java_client_snapshot(Some([batch.width, batch.height]));
        let mut advances: Option<Vec<f32>> = None;
        let mut draws = Vec::new();
        let mut messages = Vec::new();
        let mut store = store();
        let mut disabled = false;
        for entry in store.iter_mut() {
            if !entry.layers.iter().any(|l| !l.disabled) {
                continue;
            }
            let metrics = match &entry.metrics {
                Some(metrics) => Arc::clone(metrics),
                None => {
                    let advances = advances.get_or_insert_with(|| self.java_client_advances());
                    let metrics = Arc::new(TextMetrics {
                        advances: advances.clone(),
                        fallback: 8.0,
                        translations: entry.translations.clone(),
                    });
                    entry.metrics = Some(Arc::clone(&metrics));
                    metrics
                }
            };
            let snapshot = Arc::new(ClientSnapshot {
                metrics,
                ..base.clone()
            });
            let mut host = lock_resident(&entry.host);
            for slot in entry.layers.iter_mut().filter(|l| !l.disabled) {
                let presses: Vec<KeyPress> = entry.keys.iter().map(|k| k.press).collect();
                host.begin_client_frame(Arc::clone(&snapshot), &presses);
                // The first frames initialise the element's classes and
                // config (`<clinit>`, the JVM's one-time work); they get the
                // warm-up budget, every later frame the per-frame one.
                let limit = ExecutionBudget::within(if slot.frames < HUD_WARMUP_FRAMES {
                    HUD_WARMUP_BUDGET
                } else {
                    HUD_BUDGET
                });
                slot.frames = slot.frames.saturating_add(1);
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    entry.jar.run_hud_layer(&slot.layer, &mut host, &limit)
                }));
                let out = host.end_client_frame();
                let reason = match run {
                    Ok(Ok(())) => {
                        RAN.fetch_add(1, Ordering::Relaxed);
                        slot.overruns = 0;
                        draws.extend(out.draws);
                        for (key, press) in entry.keys.iter_mut().zip(out.keys) {
                            key.press = press;
                        }
                        messages.extend(out.messages);
                        messages.extend(run_tasks(
                            &entry.file,
                            &entry.jar,
                            &mut host,
                            out.tasks,
                            &snapshot,
                        ));
                        continue;
                    }
                    // Over its time in one frame: that frame draws nothing.
                    // A loaded machine can push any element over once, so
                    // only a run of overruns disables it.
                    Ok(Err(horizon_native_java::NativeError::ExecutionBudgetExceeded {
                        ..
                    })) if slot.overruns + 1 < HUD_OVERRUNS => {
                        slot.overruns += 1;
                        continue;
                    }
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "panicked".to_owned(),
                };
                refuse(
                    &entry.file,
                    &format!("HUD element {} ({})", slot.layer.id, slot.layer.source),
                    &reason,
                );
                slot.disabled = true;
                slot.refusal = Some(reason);
                disabled = true;
            }
        }
        if disabled {
            recompute(&store);
        }
        drop(store);
        self.java_client_draw(batch, draws);
        self.java_client_messages(messages);
    }

    /// Draws the recorded ops in order, through the HUD's own fill and text.
    fn java_client_draw(&self, batch: &mut HudBatch, draws: Vec<DrawOp>) {
        let atlas = &self.gui.atlas;
        let color = |argb: u32| {
            let [a, r, g, b] = argb.to_be_bytes();
            rgba(r, g, b, a)
        };
        for op in draws {
            match op {
                DrawOp::Fill { rect, argb } => self.native_fill(batch, rect, argb),
                DrawOp::Gradient { rect, top, bottom } => batch.fill_gradient(
                    atlas,
                    (rect[0], rect[1], rect[2], rect[3]),
                    color(top),
                    color(bottom),
                ),
                DrawOp::Text {
                    text,
                    at,
                    argb,
                    shadow,
                    scale,
                } => {
                    let text = java_text(&text);
                    let [sx, sy] = scale;
                    if sx == 1.0 && sy == 1.0 {
                        batch.text_styled(atlas, &text, at[0], at[1], color(argb), shadow);
                    } else if sx != 0.0 && sy != 0.0 {
                        // Drawn at the unscaled spot, then scaled about the
                        // origin: lands at `at` with the pose's glyph size.
                        let start = batch.vertices.len();
                        batch.text_styled(
                            atlas,
                            &text,
                            at[0] / sx,
                            at[1] / sy,
                            color(argb),
                            shadow,
                        );
                        batch.scale_since(start, 0.0, 0.0, sx, sy);
                    }
                }
                DrawOp::Sprite { sprite, rect, argb } => {
                    // A sprite the GUI atlas lacks draws nothing (vanilla
                    // would show its missing texture).
                    let Some(r) = atlas.sprites.get(sprite.as_str()).copied() else {
                        continue;
                    };
                    batch.rect_uv(rect[0], rect[1], rect[2], rect[3], r, color(argb));
                }
                DrawOp::Item {
                    stack,
                    slot,
                    at,
                    scale,
                    model,
                    decorations,
                } => {
                    let real = slot.and_then(|slot| self.java_client_slot_stack(&slot));
                    let stack = real.unwrap_or_else(|| {
                        let mut built =
                            ItemStack::new(stack.item.as_str(), stack.count.max(0) as u32);
                        built.damage = stack.damage.max(0) as u32;
                        built
                    });
                    if self.res.items().get(&stack.item).is_none()
                        || scale[0] == 0.0
                        || scale[1] == 0.0
                    {
                        continue;
                    }
                    // Laid out at the unscaled spot, then scaled about the
                    // origin, as text is.
                    let start = batch.vertices.len();
                    let (x, y) = (at[0] / scale[0], at[1] / scale[1]);
                    if model {
                        self.draw_item_model(batch, &stack, x, y);
                    }
                    if let Some(count) = decorations {
                        self.draw_item_decorations(batch, &stack, x, y, count);
                    }
                    batch.scale_since(start, 0.0, 0.0, scale[0], scale[1]);
                }
            }
        }
    }

    /// The game's own stack in a façade slot, so a mod's item draws with its
    /// components (glint, trims, dye).
    fn java_client_slot_stack(&self, slot: &StackSlot) -> Option<ItemStack> {
        match slot {
            StackSlot::Inventory(i) => self.inventory.get(*i).cloned().flatten(),
            StackSlot::MainHand => self.inventory.get(self.selected).cloned().flatten(),
            StackSlot::OffHand => self.offhand.clone(),
            // `armor` is head first; the façade's armour index is feet first.
            StackSlot::Armor(i) => self.armor.get(3usize.checked_sub(*i)?).cloned().flatten(),
            StackSlot::Tooltip | StackSlot::Empty | StackSlot::Made(_) => None,
        }
    }
}

/// A mod's text as the HUD and chat draw it: a string's `§` codes format it
/// (`§#rrggbb` is the client surface's RGB colour); a translatable key reads
/// through the game's language, then the mod's own lang lines, else shows
/// the key (vanilla's missing translation); a styled component is vanilla's
/// component JSON.
fn java_text(text: &ClientText) -> Text {
    match text {
        ClientText::Literal(s) => styled_text(s),
        ClientText::Translatable(key) => {
            Text::literal(mod_mappings::translation(key).unwrap_or_else(|| key.clone()))
        }
        // The same decoder a server's chat component goes through; a key
        // the game's language lacks reads from the mod's own lang lines.
        ClientText::Json(json) => match serde_json::from_str::<serde_json::Value>(json) {
            Ok(mut json) => {
                with_mod_fallbacks(&mut json);
                horizon_client::component::to_text(&json, &mut |_| Text::empty())
            }
            Err(_) => Text::literal(json.clone()),
        },
    }
}

/// Gives every translatable part of a component the mod's own line as its
/// `fallback` (vanilla's `translatableWithFallback`), so a key only the
/// mod's lang file knows reads as that line, as it does with the mod's
/// resources loaded.
fn with_mod_fallbacks(json: &mut serde_json::Value) {
    match json {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(key)) = object.get("translate")
                && !object.contains_key("fallback")
                && let Some(line) = mod_mappings::translation(key)
            {
                object.insert("fallback".into(), serde_json::Value::String(line));
            }
            for value in object.values_mut() {
                with_mod_fallbacks(value);
            }
        }
        serde_json::Value::Array(list) => list.iter_mut().for_each(with_mod_fallbacks),
        _ => {}
    }
}

/// Legacy-coded text with the client surface's `§#rrggbb` colour code.
fn styled_text(s: &str) -> Text {
    use horizon_client::text::{Span, Style, legacy_format};
    if !s.contains('§') {
        return Text::literal(s.to_owned());
    }
    let mut out = Text::default();
    let mut style = Style::default();
    let mut current = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '§' {
            current.push(c);
            continue;
        }
        let Some(code) = chars.next() else { break };
        if !current.is_empty() {
            out.spans.push(Span {
                text: std::mem::take(&mut current),
                style: style.clone(),
            });
        }
        if code == '#' {
            let hex: String = chars.by_ref().take(6).collect();
            if let Ok(rgb) = u32::from_str_radix(&hex, 16) {
                // A colour clears the decorations, as a legacy colour does.
                style = Style::color(rgb);
            }
            continue;
        }
        style = legacy_format(&style, code, &Style::default());
    }
    if !current.is_empty() {
        out.spans.push(Span {
            text: current,
            style,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, path::PathBuf, process::Command};

    /// A Fabric client jar whose `onInitializeClient` registers one key
    /// mapping (`key.java_client_test.ping` on C) and an `END_CLIENT_TICK`
    /// listener counting the clicks it consumes. `None` without `javac`.
    fn client_jar() -> Option<PathBuf> {
        let root = std::env::temp_dir().join(format!("horizon-java-client-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stubs = [
            (
                "net/fabricmc/fabric/api/event/Event.java",
                "package net.fabricmc.fabric.api.event;\n\
                 public abstract class Event<T> { public void register(T listener) {} }",
            ),
            (
                "net/minecraft/client/Minecraft.java",
                "package net.minecraft.client;\npublic class Minecraft {}",
            ),
            (
                "net/minecraft/client/KeyMapping.java",
                "package net.minecraft.client;\n\
                 public class KeyMapping { public KeyMapping(String n, int k, Category c) {}\n\
                   public boolean consumeClick() { return false; }\n\
                   public record Category(Object id) { public static final Category MISC = null; } }",
            ),
            (
                "net/fabricmc/fabric/api/client/keymapping/v1/KeyMappingHelper.java",
                "package net.fabricmc.fabric.api.client.keymapping.v1;\n\
                 public final class KeyMappingHelper {\n\
                   public static net.minecraft.client.KeyMapping registerKeyMapping(net.minecraft.client.KeyMapping k) { return k; } }",
            ),
            (
                "net/fabricmc/fabric/api/client/event/lifecycle/v1/ClientTickEvents.java",
                "package net.fabricmc.fabric.api.client.event.lifecycle.v1;\n\
                 public final class ClientTickEvents {\n\
                   public static final net.fabricmc.fabric.api.event.Event<EndTick> END_CLIENT_TICK = null;\n\
                   public interface EndTick { void onEndTick(net.minecraft.client.Minecraft c); } }",
            ),
            (
                "demo/Ping.java",
                "package demo;\n\
                 import net.minecraft.client.KeyMapping;\n\
                 public final class Ping {\n\
                   public static KeyMapping KEY; public static int clicks;\n\
                   public void onInitializeClient() {\n\
                     KEY = net.fabricmc.fabric.api.client.keymapping.v1.KeyMappingHelper.registerKeyMapping(\n\
                         new KeyMapping(\"key.java_client_test.ping\", 6, KeyMapping.Category.MISC));\n\
                     net.fabricmc.fabric.api.client.event.lifecycle.v1.ClientTickEvents.END_CLIENT_TICK\n\
                         .register(c -> { while (KEY.consumeClick()) clicks++; }); } }",
            ),
        ];
        let mut sources = Vec::new();
        for (path, text) in stubs {
            let file = root.join("src").join(path);
            std::fs::create_dir_all(file.parent()?).ok()?;
            std::fs::write(&file, text).ok()?;
            sources.push(file);
        }
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
        let jar = root.join("ping.jar");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&jar).ok()?);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("fabric.mod.json", options).ok()?;
        zip.write_all(
            br#"{"id":"java_client_test","version":"1","environment":"client","entrypoints":{"client":["demo.Ping"]}}"#,
        )
        .ok()?;
        zip.start_file("assets/java_client_test/lang/en_us.json", options)
            .ok()?;
        zip.write_all(br#"{"key.java_client_test.ping":"Ping"}"#)
            .ok()?;
        zip.start_file("demo/Ping.class", options).ok()?;
        zip.write_all(&std::fs::read(classes.join("demo/Ping.class")).ok()?)
            .ok()?;
        zip.finish().ok()?;
        Some(jar)
    }

    /// A registered mapping becomes a Controls row named from the mod's lang
    /// file, its key is kept by the player's settings, and a press of that key
    /// is the click the mod's `consumeClick` takes in its tick listener.
    #[test]
    fn a_java_key_mapping_joins_controls_and_its_press_reaches_consume_click() {
        let _guard = super::super::java_mods::test_lock();
        clear();
        // A save before the batch registers its mappings keeps the player's
        // line for one (the game can save first).
        let early = horizon_client::settings::Settings::parse(
            "version:4000\nkey_key.java_client_test.later:key.keyboard.z\n",
        );
        assert!(
            early
                .serialize()
                .contains("\nkey_key.java_client_test.later:key.keyboard.z\n")
        );
        let Some(path) = client_jar() else {
            eprintln!("skipped: no javac");
            return;
        };
        let jar = NativeJar::from_path(&path).unwrap();
        let entry = jar.discover_entrypoint("client").unwrap();
        let mut host = StdHost::new();
        jar.execute(&entry.class, entry.method.as_deref(), "client", &mut host)
            .unwrap();
        let jar = Arc::new(jar);
        assert!(has_surface(Some(&jar), Some(&host), ExecutionRole::Client));
        let resident: ResidentHost = Arc::new(Mutex::new(host));
        // A server role keeps nothing of a client surface.
        assert_eq!(
            adopt(ExecutionRole::DedicatedServer, &path, &jar, &resident),
            0
        );
        assert_eq!(adopt(ExecutionRole::Client, &path, &jar, &resident), 2);

        // Controls lists it, under vanilla's Miscellaneous, on C, named "Ping".
        let row = mod_mappings::find("key.java_client_test.ping").unwrap();
        assert_eq!(row.1, "key.keyboard.c");
        assert_eq!(row.2, "misc");
        assert_eq!(mod_mappings::label(row.0).plain(), "Ping");
        let mut settings = horizon_client::settings::Settings::parse("");
        assert_eq!(settings.keys.get(row.0), "key.keyboard.c");
        // Rebound to V, it is saved as vanilla saves a mapping.
        settings.keys.set(row.0, "key.keyboard.v");
        assert!(
            settings
                .serialize()
                .contains("\nkey_key.java_client_test.ping:key.keyboard.v\n")
        );
        let reread = horizon_client::settings::Settings::parse(&settings.serialize());
        assert_eq!(reread.keys.get(row.0), "key.keyboard.v");

        // Two presses of C (the default), as the window's key events give
        // them; the tick listener consumes both.
        let (keys, clicks) = {
            let mut store = store();
            let entry = store.last_mut().unwrap();
            for _ in 0..2 {
                entry.keys[0].press.down = true;
                entry.keys[0].press.clicks += 1;
            }
            let firing = EventFiring::new(
                GameEvent::ClientTickEnd,
                Arc::new(EventFacade::default()),
                EventContext::default(),
            );
            let ClientJar {
                file,
                jar,
                host,
                slots,
                keys,
                ..
            } = entry;
            let mut host = lock_resident(host);
            run_listeners(file, jar, &mut host, slots, firing, Arc::default(), keys);
            let clicks = horizon_native_java::Host::persistent_class_state(&*host)
                .0
                .remove("demo/Ping#clicksI");
            (keys[0].press, clicks)
        };
        assert_eq!(clicks, Some(horizon_native_java::Value::Int(2)));
        assert_eq!(
            keys,
            KeyPress {
                down: true,
                clicks: 0
            }
        );
        clear();
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// What the client surface does with a corpus: every jar in the
    /// directories of `HORIZON_CLIENT_CORPUS` (`:`-separated) bootstraps in
    /// the client role through the real batch, 32 at a time; each kept jar's
    /// client listeners then fire (started, join, three ticks, level ticks)
    /// and its HUD elements draw three frames, in a synthetic world frame: a
    /// player at (0.5, 64, 0.5) holding a damaged pickaxe, a 427x240 GUI.
    /// Tooltip listeners describe a damaged diamond sword. The counts, each
    /// HUD element's slowest frame and the tooltip lines added print at the
    /// end; `HORIZON_CLIENT_DUMP_DRAWS=1` also prints every frame's draw ops.
    ///
    ///   HORIZON_JAVA_CACHE=0 HORIZON_CLIENT_CORPUS=~/horizon-corpus/corpus-xl:~/horizon-corpus/mega \
    ///   cargo test -p horizon-client --bin horizon-client -- --ignored client_surface_corpus --nocapture
    #[test]
    #[ignore = "needs a jar corpus in HORIZON_CLIENT_CORPUS"]
    fn client_surface_corpus_measure() {
        let _guard = super::super::java_mods::test_lock();
        let Some(dirs) = std::env::var_os("HORIZON_CLIENT_CORPUS") else {
            eprintln!("skipped: set HORIZON_CLIENT_CORPUS");
            return;
        };
        let mut paths: Vec<PathBuf> = std::env::split_paths(&dirs)
            .filter_map(|dir| std::fs::read_dir(dir).ok())
            .flatten()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.extension().is_some_and(|e| e == "jar"))
            .collect();
        paths.sort();
        let snapshot = Arc::new(ClientSnapshot {
            player: Some(ClientPlayer {
                name: "Alex".into(),
                pos: [0.5, 64.0, 0.5],
                health: 20.0,
                max_health: 20.0,
                on_ground: true,
                game_mode: "SURVIVAL".into(),
                // A damaged pickaxe in the selected slot, so item HUDs draw.
                inventory: std::iter::once(horizon_native_java::client_surface::ClientStack {
                    item: "minecraft:diamond_pickaxe".into(),
                    count: 1,
                    damage: 700,
                    max_damage: 1561,
                    max_stack: 1,
                    name: "Diamond Pickaxe".into(),
                    ..horizon_native_java::client_surface::ClientStack::empty()
                })
                .chain(std::iter::repeat_n(
                    horizon_native_java::client_surface::ClientStack::empty(),
                    35,
                ))
                .collect(),
                food: 20,
                saturation: 5.0,
                air: 300,
                max_air: 300,
                ..ClientPlayer::default()
            }),
            level: Some(ClientLevel {
                dimension: "minecraft:overworld".into(),
                game_time: 1000,
                day_time: 1000,
                min_y: -64,
                height: 384,
            }),
            gui_size: [427, 240],
            window_size: [854, 480],
            render_distance: 12,
            camera: Some(ClientCamera {
                pos: [0.5, 65.62, 0.5],
                ..ClientCamera::default()
            }),
            gui_scale: 2.0,
            fps: 60,
            // Single player, the common case: no server data to ask for.
            single_player: true,
            metrics: Arc::new(TextMetrics {
                advances: vec![6.0; 0x500],
                fallback: 8.0,
                translations: BTreeMap::new(),
            }),
            ..ClientSnapshot::default()
        });
        let mut kept_jars = 0;
        let mut by_event: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
        let (mut key_jars, mut keys) = (0, 0);
        let (mut hud_jars, mut huds) = (0, 0);
        let mut runs: BTreeMap<&'static str, [usize; 3]> = BTreeMap::new();
        let mut causes: BTreeMap<String, usize> = BTreeMap::new();
        let mut running_jars = 0;
        // Draw ops each jar's HUD elements recorded over the three frames.
        let mut drew: BTreeMap<String, usize> = BTreeMap::new();
        // Tooltip lines each jar's tooltip listeners added to a damaged
        // diamond sword's one-line tooltip.
        let mut tooltip_lines: BTreeMap<String, usize> = BTreeMap::new();
        // The slowest of each HUD element's three frames (the first runs
        // its class initialisation).
        let mut hud_time: BTreeMap<String, Duration> = BTreeMap::new();
        let tooltip = Arc::new(ClientSnapshot {
            tooltip_stack: Some(horizon_native_java::client_surface::ClientStack {
                item: "minecraft:diamond_sword".into(),
                count: 1,
                damage: 100,
                max_damage: 1561,
                max_stack: 1,
                name: "Diamond Sword".into(),
                ..horizon_native_java::client_surface::ClientStack::empty()
            }),
            tooltip_lines: vec!["Diamond Sword".into()],
            ..(*snapshot).clone()
        });
        let mut started = 0;
        for chunk in paths.chunks(32) {
            let Ok(batch) =
                super::super::java_mods::run_java_mods_batch(chunk, ExecutionRole::Client)
            else {
                continue;
            };
            started += batch.reports.len();
            let mut store = store();
            for entry in store.iter_mut() {
                kept_jars += 1;
                let mut seen = std::collections::BTreeSet::new();
                for slot in &entry.slots {
                    let row = by_event.entry(slot.listener.event.name()).or_default();
                    row.1 += 1;
                    if seen.insert(slot.listener.event) {
                        row.0 += 1;
                    }
                }
                if !entry.keys.is_empty() {
                    key_jars += 1;
                    keys += entry.keys.len();
                }
                if !entry.layers.is_empty() {
                    hud_jars += 1;
                    huds += entry.layers.len();
                }
                let mut host = lock_resident(&entry.host);
                for event in [
                    GameEvent::ClientStarted,
                    GameEvent::ClientJoin,
                    GameEvent::ClientTickStart,
                    GameEvent::ClientTickEnd,
                    GameEvent::ClientLevelTickStart,
                    GameEvent::ClientLevelTickEnd,
                    GameEvent::ClientTickStart,
                    GameEvent::ClientTickEnd,
                    GameEvent::ClientTickStart,
                    GameEvent::ClientTickEnd,
                ] {
                    let firing = EventFiring::new(
                        event,
                        Arc::new(EventFacade::default()),
                        EventContext::default(),
                    );
                    run_listeners(
                        &entry.file,
                        &entry.jar,
                        &mut host,
                        &mut entry.slots,
                        firing,
                        Arc::clone(&snapshot),
                        &mut entry.keys,
                    );
                }
                let tooltip_slots =
                    |s: &&mut Slot| !s.disabled && s.listener.event == GameEvent::ClientItemTooltip;
                if entry.slots.iter_mut().any(|s| tooltip_slots(&s)) {
                    host.begin_client_frame(Arc::clone(&tooltip), &[]);
                    let mut firing = EventFiring::new(
                        GameEvent::ClientItemTooltip,
                        Arc::new(EventFacade::default()),
                        EventContext::default(),
                    );
                    for slot in entry.slots.iter_mut().filter(tooltip_slots) {
                        let limit = ExecutionBudget::within(Duration::from_millis(250));
                        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            entry.jar.run_game_listener(
                                &slot.listener,
                                &mut firing,
                                &mut host,
                                &limit,
                            )
                        }));
                        let reason = match run {
                            Ok(Ok(_)) => continue,
                            Ok(Err(error)) => error.to_string(),
                            Err(_) => "panicked".into(),
                        };
                        slot.disabled = true;
                        slot.refusal = Some(reason);
                    }
                    if let Some(lines) = host.end_client_frame().tooltip
                        && lines.len() > 1
                    {
                        tooltip_lines.insert(entry.file.clone(), lines.len() - 1);
                    }
                }
                for _ in 0..3 {
                    for slot in entry.layers.iter_mut().filter(|l| !l.disabled) {
                        host.begin_client_frame(Arc::clone(&snapshot), &[]);
                        let limit = ExecutionBudget::within(Duration::from_millis(250));
                        let started = std::time::Instant::now();
                        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            entry.jar.run_hud_layer(&slot.layer, &mut host, &limit)
                        }));
                        let took = started.elapsed();
                        let slowest = hud_time.entry(slot.layer.id.clone()).or_default();
                        *slowest = (*slowest).max(took);
                        let frame_out = host.end_client_frame();
                        if std::env::var_os("HORIZON_CLIENT_DUMP_DRAWS").is_some() {
                            eprintln!("DRAWS {}: {:?}", slot.layer.id, frame_out.draws);
                        }
                        let drawn = frame_out.draws.len();
                        if drawn > 0 {
                            *drew.entry(entry.file.clone()).or_default() += drawn;
                        }
                        match run {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                slot.disabled = true;
                                slot.refusal = Some(error.to_string());
                            }
                            Err(_) => {
                                slot.disabled = true;
                                slot.refusal = Some("panicked".into());
                            }
                        }
                    }
                }
                let mut running = false;
                for slot in &entry.slots {
                    let row = runs.entry(slot.listener.event.name()).or_default();
                    row[0] += 1;
                    match &slot.refusal {
                        Some(reason) => {
                            row[2] += 1;
                            *causes.entry(cause_head(reason)).or_default() += 1;
                        }
                        None => {
                            row[1] += 1;
                            running = true;
                        }
                    }
                }
                for slot in &entry.layers {
                    let row = runs.entry("hud_element").or_default();
                    row[0] += 1;
                    match &slot.refusal {
                        Some(reason) => {
                            row[2] += 1;
                            *causes.entry(cause_head(reason)).or_default() += 1;
                        }
                        None => {
                            row[1] += 1;
                            running = true;
                        }
                    }
                }
                running_jars += usize::from(running);
                let refused: Vec<&str> = entry
                    .slots
                    .iter()
                    .filter_map(|s| s.refusal.as_deref())
                    .chain(entry.layers.iter().filter_map(|l| l.refusal.as_deref()))
                    .collect();
                println!(
                    "jar {}: {} listener(s), {} key(s) [{}], {} HUD element(s), {} refused{}",
                    entry.file,
                    entry.slots.len(),
                    entry.keys.len(),
                    entry
                        .keys
                        .iter()
                        .map(|k| k.name)
                        .collect::<Vec<_>>()
                        .join(" "),
                    entry.layers.len(),
                    refused.len(),
                    refused
                        .first()
                        .map_or_else(String::new, |r| format!(": {}", cause_head(r)))
                );
            }
            drop(store);
            clear();
        }
        println!(
            "client surface corpus: {} jars, {started} started in the client role",
            paths.len()
        );
        println!(
            "kept (a client listener, key mapping or HUD element): {kept_jars} jars, {running_jars} with one still running after the frames"
        );
        println!(
            "key mappings: {keys} in {key_jars} jars; HUD elements: {huds} in {hud_jars} jars"
        );
        for (event, (jars, listeners)) in &by_event {
            println!("  {event}: {listeners} listener(s) in {jars} jar(s)");
        }
        for (what, [kept, ran, refused]) in &runs {
            println!("  ran {what}: {kept} kept, {ran} still running, {refused} refused");
        }
        println!("HUD elements that drew: {} jar(s) {drew:?}", drew.len());
        println!("HUD element slowest frame: {hud_time:?}");
        println!(
            "tooltip listeners that added lines: {} jar(s) {tooltip_lines:?}",
            tooltip_lines.len()
        );
        let mut causes: Vec<(String, usize)> = causes.into_iter().collect();
        causes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (cause, count) in causes.iter().take(25) {
            println!("  refused {count}x: {cause}");
        }
    }

    /// A refusal's cause without the jar-specific tail: an unmodelled member
    /// names its owner and method.
    fn cause_head(reason: &str) -> String {
        let reason = reason.replace(['\n', '\r'], " ");
        reason.chars().take(150).collect()
    }
}
