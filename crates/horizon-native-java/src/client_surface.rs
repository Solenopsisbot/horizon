//! The client surface a Java mod's client code sees: key mappings, HUD
//! elements, and the `Minecraft` stand-in its tick and connection listeners
//! read.
//!
//! A client entrypoint registers three kinds of things the window drives:
//!
//! - **Key mappings.** `KeyMappingHelper.registerKeyMapping(new KeyMapping(
//!   name, type, key, category))` (Fabric) or `RegisterKeyMappingsEvent
//!   .register(mapping)` (NeoForge). The host records the constructor's
//!   arguments against the object marker and the registration in order; the
//!   game lists them in Controls and answers `consumeClick`/`isDown` on that
//!   marker from real input ([`KeyPress`]).
//! - **HUD elements.** `HudElementRegistry.addLast(id, element)` (Fabric) or
//!   `RegisterGuiLayersEvent.registerAboveAll(id, layer)` (NeoForge). Each
//!   becomes a [`HudLayer`] the game runs every frame
//!   ([`NativeJar::run_hud_layer`]) against a bounded `GuiGraphicsExtractor`
//!   façade that records [`DrawOp`]s: `text`, `centeredText`, `fill`,
//!   `fillGradient`, `outline`, the pose's `translate`/`scale`, and
//!   `Font.width`.
//! - **Client events** (tick, lifecycle, join/disconnect) are ordinary
//!   [`crate::events::GameListener`]s with client [`GameEvent`]s; their
//!   `Minecraft`, `ClientLevel` and `LocalPlayer` arguments are the markers
//!   below.
//!
//! # The stand-ins
//!
//! Every client object is a marker under `client/`: [`MINECRAFT`],
//! [`LEVEL`], [`PLAYER`], the options, the GUI and its chat, the font, the
//! window, the graphics and its pose, the delta tracker. During a firing the
//! game hands a [`ClientSnapshot`] (player, level, GUI size, held vanilla
//! keys, text metrics) and the [`KeyPress`] state of the jar's mappings
//! ([`StdHost::begin_client_frame`]); the markers answer from it, and what
//! the callback did (draws, chat lines, consumed clicks) comes back from
//! [`StdHost::end_client_frame`]. A member the surface does not model fails
//! the callback with [`NativeError::UnknownHostCall`] naming it, so the game
//! disables that callback instead of running it on invented state. Outside a
//! firing (an entrypoint caching `Minecraft.getInstance()`), an unmodelled
//! member falls through to the host's usual lenient answer, exactly as it did
//! before this surface existed; `player` and `level` read `null` then, which
//! is true while the client is starting.
//!
//! Chat components are text markers (`component/literal/<text>` or
//! `component/translatable/<key>`); a style (`withStyle`, `withColor`,
//! `Style`) is kept as legacy codes inside that text (`client_text.rs`).

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    ExecutionBudget, MethodRef, NativeError, NativeJar, StdHost, Value, boolean_value,
    component_value,
    events::{self, GameEvent, ListenerCallback},
    expect_ref, identifier_parts, parse_callback_marker, return_desc, stable_hash, uuid_value,
};

// The player's items and effects (`ItemStack`, `Inventory`, effects). A
// submodule so the crate root's module list stays the interpreter's.
#[path = "client_items.rs"]
mod items;
pub use items::{ClientEffect, ClientItem, ClientStack, ItemTable, StackSlot};
// Style objects (`Style`, `TextColor`, `ChatFormatting`).
#[path = "client_text.rs"]
mod text;
// Config libraries' defaults (`AutoConfig`, YACL), for the interpreter's
// library dispatch.
#[path = "client_config.rs"]
mod config;

/// `Minecraft.getInstance()`, and every client listener's `Minecraft`.
pub const MINECRAFT: &str = "client/minecraft";
/// `Minecraft.level`, a `ClientLevel`.
pub const LEVEL: &str = "client/level";
/// `Minecraft.player`, the `LocalPlayer`.
pub const PLAYER: &str = "client/player";
/// A join/disconnect listener's `ClientPacketListener`.
pub const CONNECTION: &str = "client/connection";
/// A join listener's `PacketSender`.
pub const PACKET_SENDER: &str = "client/packet-sender";
const OPTIONS: &str = "client/options";
const GUI: &str = "client/gui";
/// `gui.hud` (26.3 moved the action bar there).
const HUD: &str = "client/hud";
const CHAT: &str = "client/chat";
const FONT: &str = "client/font";
const WINDOW: &str = "client/window";
const GRAPHICS: &str = "client/gui-graphics";
const POSE: &str = "client/pose";
const DELTA: &str = "client/delta-tracker";
/// `Minecraft.gameMode`, the `MultiPlayerGameMode`.
const GAME_MODE: &str = "client/game-mode";
/// `Minecraft.getDebugOverlay()`.
const DEBUG: &str = "client/debug-overlay";
const GAME_TYPE: &str = "net/minecraft/world/level/GameType";
/// `Minecraft.gameRenderer`.
const GAME_RENDERER: &str = "client/game-renderer";
/// `gameRenderer.getMainCamera()`.
const CAMERA: &str = "client/camera";
/// `Minecraft.getTextureManager()`: a bounded stand-in that names textures
/// but never uploads one.
const TEXTURE_MANAGER: &str = "client/texture-manager";
/// `Minecraft.getSingleplayerServer()` in single player. The server runs on
/// its own thread; the window's callbacks see only that it exists.
const INTEGRATED_SERVER: &str = "client/integrated-server";
/// `Minecraft.getUser()`.
const USER: &str = "client/user";
/// `options.<name>()`: `client/option/<name>`.
const OPTION: &str = "client/option/";
/// A `Vec3` the façade handed out: `client/vec3/<x>,<y>,<z>`.
const VEC3: &str = "client/vec3/";
/// A tooltip listener's `List<Component>`: the host collection the game
/// seeds with the tooltip's lines and reads back after the firing.
const TOOLTIP_LIST: &str = "java/util/ArrayList#client-tooltip";
const TOOLTIP_CONTEXT: &str = "client/tooltip-context";
const TOOLTIP_FLAG: &str = "client/tooltip-flag";
/// Lines a tooltip listener may leave; more are cut.
const MAX_TOOLTIP_LINES: usize = 64;

/// A Fabric `ItemTooltipCallback`'s arguments: the stack, its context, the
/// flag and the lines.
pub(crate) fn tooltip_arguments() -> Vec<Value> {
    vec![
        marker(format!("{}tooltip", items::STACK)),
        marker(TOOLTIP_CONTEXT),
        marker(TOOLTIP_FLAG),
        marker(TOOLTIP_LIST),
    ]
}

/// A `java.awt.Color` constant: `client/awt-color/<aarrggbb>`.
const AWT_COLOR: &str = "client/awt-color/";
/// `java.awt.Color`'s named constants (both spellings resolve).
const AWT_COLORS: &[(&str, u32)] = &[
    ("WHITE", 0xffff_ffff),
    ("LIGHT_GRAY", 0xffc0_c0c0),
    ("GRAY", 0xff80_8080),
    ("DARK_GRAY", 0xff40_4040),
    ("BLACK", 0xff00_0000),
    ("RED", 0xffff_0000),
    ("PINK", 0xffff_afaf),
    ("ORANGE", 0xffff_c800),
    ("YELLOW", 0xffff_ff00),
    ("GREEN", 0xff00_ff00),
    ("MAGENTA", 0xffff_00ff),
    ("CYAN", 0xff00_ffff),
    ("BLUE", 0xff00_00ff),
];

/// `Runnable`s one firing may queue with `Minecraft.execute`.
const MAX_TASKS: usize = 64;
/// A vanilla mapping read through `options.keyJump`...: `client/vanilla-key/key.jump`.
const VANILLA_KEY: &str = "client/vanilla-key/";
const CATEGORY: &str = "key-category/";

const KEY_MAPPING: &str = "net/minecraft/client/KeyMapping";
const KEY_CATEGORY: &str = "net/minecraft/client/KeyMapping$Category";
const INPUT_TYPE: &str = "com/mojang/blaze3d/platform/InputConstants$Type";
const MINECRAFT_CLASS: &str = "net/minecraft/client/Minecraft";
const OPTIONS_CLASS: &str = "net/minecraft/client/Options";
const FONT_CLASS: &str = "net/minecraft/client/gui/Font";
const KEY_HELPER: &str = "net/fabricmc/fabric/api/client/keymapping/v1/KeyMappingHelper";
const HUD_REGISTRY: &str = "net/fabricmc/fabric/api/client/rendering/v1/hud/HudElementRegistry";
/// `HudElement.extractRenderState` and NeoForge's `GuiLayer.render` share it.
const LAYER_DESCRIPTOR: &str =
    "(Lnet/minecraft/client/gui/GuiGraphicsExtractor;Lnet/minecraft/client/DeltaTracker;)V";

/// Key mappings one jar may register; more are refused (a jar with hundreds
/// is generating them, and each becomes a Controls row).
pub const MAX_KEYS_PER_JAR: usize = 64;
/// HUD elements one jar may register.
pub const MAX_LAYERS_PER_JAR: usize = 32;
/// Draw operations one callback may record in one frame; one more fails it.
pub const MAX_DRAWS_PER_CALL: usize = 2048;
/// Characters of one drawn string or chat line; longer ones are cut.
const MAX_TEXT: usize = 512;
/// `pushMatrix` depth.
const MAX_POSE_DEPTH: usize = 16;
/// Chat lines one firing may add.
const MAX_MESSAGES: usize = 16;

/// `Options` key fields (`options.keyJump`) and the vanilla mapping each is.
const OPTION_KEYS: &[(&str, &str)] = &[
    ("keyUp", "key.forward"),
    ("keyLeft", "key.left"),
    ("keyDown", "key.back"),
    ("keyRight", "key.right"),
    ("keyJump", "key.jump"),
    ("keyShift", "key.sneak"),
    ("keySprint", "key.sprint"),
    ("keyInventory", "key.inventory"),
    ("keySwapOffhand", "key.swapOffhand"),
    ("keyDrop", "key.drop"),
    ("keyUse", "key.use"),
    ("keyAttack", "key.attack"),
    ("keyPickItem", "key.pickItem"),
    ("keyChat", "key.chat"),
    ("keyPlayerList", "key.playerlist"),
    ("keyCommand", "key.command"),
    ("keyScreenshot", "key.screenshot"),
    ("keyTogglePerspective", "key.togglePerspective"),
    ("keySmoothCamera", "key.smoothCamera"),
    ("keyFullscreen", "key.fullscreen"),
    ("keyAdvancements", "key.advancements"),
];

/// One key mapping a jar registered.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientKeyMapping {
    /// The `KeyMapping` object marker the mod keeps and later asks
    /// `consumeClick` of.
    pub marker: String,
    /// Its name, a translation key of the mod's own lang file
    /// (`key.examplemod.zoom`); also its `options.txt` name (`key_<name>`).
    pub name: String,
    /// Its default key as `InputConstants` names it (`key.keyboard.c`,
    /// `key.mouse.middle`), `key.keyboard.unknown` when the code has no name.
    pub default_key: String,
    /// Its category: an identifier (`minecraft:misc`, `examplemod:main`), or
    /// a pre-26.3 mod's translation key (`key.categories.examplemod`).
    pub category: String,
    /// The API it came through, for diagnostics.
    pub source: &'static str,
}

/// One HUD element a jar registered.
#[derive(Clone, Debug, PartialEq)]
pub struct HudLayer {
    /// Its identifier (`examplemod:coordinates`).
    pub id: String,
    /// The API it came through, for diagnostics.
    pub source: &'static str,
    pub callback: ListenerCallback,
}

/// The local player as the façade shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientPlayer {
    pub uuid: u128,
    pub name: String,
    pub entity_id: i32,
    pub pos: [f64; 3],
    /// Degrees, `getYRot`/`getXRot`.
    pub yaw: f32,
    pub pitch: f32,
    pub health: f32,
    pub max_health: f32,
    pub creative: bool,
    pub spectator: bool,
    pub sneaking: bool,
    pub sprinting: bool,
    pub on_ground: bool,
    /// `isFallFlying`: gliding with elytra.
    pub fall_flying: bool,
    /// Its `GameType` constant: `SURVIVAL`, `CREATIVE`, `ADVENTURE`, `SPECTATOR`.
    pub game_mode: String,
    /// The main inventory, 36 stacks (0..8 the hotbar); an empty slot is
    /// [`ClientStack::empty`].
    pub inventory: Vec<ClientStack>,
    /// The selected hotbar slot, 0..8.
    pub selected: usize,
    /// Armour: feet, legs, chest, head (`Inventory` slots 36..39).
    pub armor: [ClientStack; 4],
    pub offhand: ClientStack,
    pub effects: Vec<ClientEffect>,
    pub food: i32,
    pub saturation: f32,
    /// `getArmorValue`.
    pub armor_value: i32,
    pub absorption: f32,
    pub air: i32,
    pub max_air: i32,
    pub xp_level: i32,
    /// Progress to the next level, 0..1.
    pub xp_progress: f32,
    pub xp_total: i32,
}

/// The camera as the façade shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientCamera {
    pub pos: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub detached: bool,
}

/// The client level as the façade shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientLevel {
    /// Dimension id (`minecraft:overworld`).
    pub dimension: String,
    pub game_time: i64,
    pub day_time: i64,
    /// `getMinY` and `getHeight`: the dimension's build limits.
    pub min_y: i32,
    pub height: i32,
}

/// What `Font.width` needs: the game font's advances and the translations a
/// translatable component resolves to. Built once by the game and shared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextMetrics {
    /// Advance of each char below `advances.len()`, in GUI pixels.
    pub advances: Vec<f32>,
    /// The advance of a char past the table.
    pub fallback: f32,
    /// Translation key → text, for translatable components.
    pub translations: BTreeMap<String, String>,
}

impl TextMetrics {
    /// `Font.width` of plain text: the advances' sum, rounded up; a `§`
    /// formatting code and the char after it take no width.
    pub fn width(&self, text: &str) -> i32 {
        let mut total = 0.0f32;
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '§' {
                chars.next();
                continue;
            }
            total += self
                .advances
                .get(c as usize)
                .copied()
                .unwrap_or(self.fallback);
        }
        total.ceil() as i32
    }

    /// The text a [`ClientText`] reads as: a translatable one's translation,
    /// else its key (vanilla shows the key of a missing translation).
    pub fn resolve<'a>(&'a self, text: &'a ClientText) -> std::borrow::Cow<'a, str> {
        match text {
            ClientText::Literal(text) => text.as_str().into(),
            ClientText::Translatable(key) => self
                .translations
                .get(key)
                .map_or(key.as_str(), String::as_str)
                .into(),
            ClientText::Json(json) => {
                let mut out = String::new();
                if let Ok(json) = serde_json::from_str(json) {
                    self.flatten(&json, &mut out);
                }
                out.into()
            }
        }
    }

    /// A component JSON's text in order, translations resolved (their
    /// arguments are not substituted for the width).
    fn flatten(&self, json: &serde_json::Value, out: &mut String) {
        use serde_json::Value as Json;
        match json {
            Json::String(text) => out.push_str(text),
            Json::Object(object) => {
                if let Some(Json::String(text)) = object.get("text") {
                    out.push_str(text);
                } else if let Some(Json::String(key)) = object.get("translate") {
                    out.push_str(self.translations.get(key).map_or(key, String::as_str));
                }
                if let Some(Json::Array(extra)) = object.get("extra") {
                    extra.iter().for_each(|part| self.flatten(part, out));
                }
            }
            _ => {}
        }
    }
}

/// The client state one firing sees, snapshotted by the game.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientSnapshot {
    /// `Minecraft.player`; `None` outside a world.
    pub player: Option<ClientPlayer>,
    /// `Minecraft.level`; `None` outside a world.
    pub level: Option<ClientLevel>,
    pub paused: bool,
    /// The world is this client's own (single player).
    pub single_player: bool,
    /// `options.hideGui` (F1).
    pub hide_gui: bool,
    /// `Hud.getGuiTicks`: the HUD's tick counter.
    pub gui_ticks: i32,
    /// `options.renderDistance()`, in chunks.
    pub render_distance: i32,
    /// `options.mainHand()`: the player's main arm is the left one.
    pub left_handed: bool,
    /// The single-player world is open to LAN (`IntegratedServer.isPublished`).
    pub lan_open: bool,
    /// The camera: eye position, rotation (degrees) and whether it is
    /// detached (third person).
    pub camera: Option<ClientCamera>,
    /// A screen is open (`Minecraft.screen` / `gui.screen()` is not null):
    /// the title, a menu, chat, an inventory. The façade has no screen
    /// object, so a callback that reads it then refuses.
    pub screen_open: bool,
    /// The debug screen (F3) shows.
    pub debug_screen: bool,
    /// `Window.getGuiScaledWidth/Height`, `GuiGraphicsExtractor.guiWidth/Height`.
    pub gui_size: [i32; 2],
    /// `Window.getWidth/Height`, in pixels.
    pub window_size: [i32; 2],
    pub gui_scale: f64,
    /// `DeltaTracker.getGameTimeDeltaPartialTick`.
    pub partial_tick: f32,
    pub fps: i32,
    /// The vanilla mappings held right now (`key.jump`...).
    pub vanilla_keys_down: Vec<String>,
    pub metrics: Arc<TextMetrics>,
    /// Item facts by id, for stacks a callback builds itself and for
    /// `Item` members; shared, built once by the game.
    pub items: Arc<ItemTable>,
    /// The stack a tooltip callback describes (`client/stack/tooltip`).
    pub tooltip_stack: Option<ClientStack>,
    /// The tooltip's lines so far, as plain text, which a tooltip listener's
    /// list starts with.
    pub tooltip_lines: Vec<String>,
    /// `TooltipFlag.isAdvanced` (F3+H).
    pub advanced_tooltips: bool,
}

/// One registered mapping's input state, in [`StdHost::client_key_mappings`]
/// order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyPress {
    /// `isDown`.
    pub down: bool,
    /// Clicks `consumeClick` has not taken yet.
    pub clicks: u32,
}

/// Text a mod drew or sent.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientText {
    /// A string (its `§` codes are formatting) or a literal component.
    Literal(String),
    /// A translatable component's key.
    Translatable(String),
    /// A styled component as vanilla's component JSON (`facade_text.rs`).
    Json(String),
}

/// One recorded `GuiGraphicsExtractor` call, in GUI pixels after the pose.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    /// `text`/`centeredText`: `at` is where the text starts on screen and
    /// `scale` the pose's scale, which the game applies to the glyphs.
    Text {
        text: ClientText,
        at: [f32; 2],
        argb: u32,
        shadow: bool,
        scale: [f32; 2],
    },
    /// `fill`: `[x, y, w, h]`.
    Fill { rect: [f32; 4], argb: u32 },
    /// `fillGradient`: `top` to `bottom`, down the rectangle.
    Gradient {
        rect: [f32; 4],
        top: u32,
        bottom: u32,
    },
    /// `item`/`fakeItem` (`model`) or `itemDecorations` (`decorations`) of a
    /// stack, its top left at `at`, 16 GUI pixels square times `scale`.
    /// `slot` names where the stack came from so the game can draw the real
    /// stack (its components, glint and trims); the façade's copy is the
    /// fallback for a built one.
    Item {
        stack: ClientStack,
        slot: Option<StackSlot>,
        at: [f32; 2],
        scale: [f32; 2],
        model: bool,
        /// `Some(count text)` draws the decorations; an inner `None` is the
        /// stack's own count (`itemDecorations(font, stack, x, y)`).
        decorations: Option<Option<String>>,
    },
    /// `blitSprite` of a GUI sprite (`hud/heart/full`, a mod's
    /// `examplemod:gui/meter`) stretched into `rect`, tinted `argb`.
    Sprite {
        sprite: String,
        rect: [f32; 4],
        argb: u32,
    },
}

/// A chat line or action-bar message a mod sent.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    /// `gui.getChat().addMessage`, `displayClientMessage(_, false)`,
    /// `sendSystemMessage`.
    Chat(ClientText),
    /// `displayClientMessage(_, true)`, `gui.setOverlayMessage`.
    Overlay(ClientText),
}

/// What one firing left for the game.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientFrameOutput {
    pub draws: Vec<DrawOp>,
    pub messages: Vec<ClientMessage>,
    /// The mappings' state after the callback (clicks it consumed are gone).
    pub keys: Vec<KeyPress>,
    /// `Minecraft.execute(runnable)`s queued, in order: the game runs them
    /// after the firing ([`NativeJar::run_client_task`]), as vanilla runs its
    /// queued tasks later in the same tick.
    pub tasks: Vec<Value>,
    /// A tooltip firing's lines after its listeners, when the frame had a
    /// tooltip; a line that is not text (null, another object) is dropped.
    pub tooltip: Option<Vec<ClientText>>,
}

/// How a `KeyMapping` was constructed.
#[derive(Clone, Debug, PartialEq)]
struct KeyInit {
    name: String,
    mouse: bool,
    code: i32,
    category: Option<String>,
}

/// One affine pose without rotation: `[sx, sy, tx, ty]`.
type Pose = [f32; 4];
const IDENTITY: Pose = [1.0, 1.0, 0.0, 0.0];

/// The client part of a [`StdHost`]: registrations (kept for the host's
/// life) and the current firing (only while one runs).
#[derive(Clone, Debug, Default)]
pub(crate) struct ClientState {
    /// `KeyMapping` object marker → its constructor's arguments.
    key_inits: BTreeMap<String, KeyInit>,
    /// `KeyMapping.Category` object marker → its identifier.
    categories: BTreeMap<String, String>,
    keys: Vec<ClientKeyMapping>,
    /// Registrations that could not become a mapping or layer, with why.
    refused: Vec<String>,
    /// HUD registrations as recorded: (id, element value, source).
    layers: Vec<(String, Value, &'static str)>,
    frame: Option<Arc<ClientSnapshot>>,
    presses: Vec<KeyPress>,
    draws: Vec<DrawOp>,
    messages: Vec<ClientMessage>,
    poses: Vec<Pose>,
    /// Stacks the firing's callbacks built (`new ItemStack`).
    made: items::MadeStacks,
    /// A built stack's object marker → its slot.
    made_aliases: BTreeMap<String, StackSlot>,
    tasks: Vec<Value>,
}

fn marker(text: impl Into<String>) -> Value {
    Value::Ref(Some(text.into()))
}

fn flag(value: bool) -> Value {
    Value::Int(i32::from(value))
}

fn int_arg(args: &[Value], at: usize) -> Option<i32> {
    match args.get(at)? {
        Value::Int(value) => Some(*value),
        _ => None,
    }
}

fn float_arg(args: &[Value], at: usize) -> Option<f32> {
    match args.get(at)? {
        Value::Float(value) => Some(*value as f32),
        Value::Int(value) => Some(*value as f32),
        _ => None,
    }
}

fn cut(text: &str) -> String {
    text.chars().take(MAX_TEXT).collect()
}

/// A drawn or sent text argument: a `String` or a component marker (a
/// `FormattedCharSequence` is the component it came from). `None` for null.
fn text_arg(value: &Value) -> Option<ClientText> {
    let text = expect_ref(value)?;
    if let Some(literal) = text.strip_prefix("component/literal/") {
        return Some(ClientText::Literal(cut(literal)));
    }
    if let Some(key) = text.strip_prefix("component/translatable/") {
        return Some(ClientText::Translatable(cut(key)));
    }
    if let Some(json) = text.strip_prefix(crate::facade_text::JSON_PREFIX) {
        // Bounded by the component model itself (64 KiB).
        return Some(ClientText::Json(json.to_owned()));
    }
    Some(ClientText::Literal(cut(text)))
}

/// The `InputConstants` name of a key code: an SDL scancode for a keyboard
/// key (`InputConstants.KEY_C` is 6 in 26.3), a button for a mouse one.
pub fn input_key_name(mouse: bool, code: i32) -> String {
    if mouse {
        return match code {
            0 => "key.mouse.left".into(),
            1 => "key.mouse.right".into(),
            2 => "key.mouse.middle".into(),
            n if n > 2 => format!("key.mouse.{}", n + 1),
            _ => "key.keyboard.unknown".into(),
        };
    }
    KEYBOARD_NAMES
        .iter()
        .find(|(scancode, _)| *scancode == code)
        .map_or_else(|| "key.keyboard.unknown".into(), |(_, name)| (*name).into())
}

/// `InputConstants` keyboard names by SDL scancode (26.3): the subset a key
/// mapping's default uses. The full table is `horizon_client_input::key_names`,
/// which this crate cannot depend on; the game re-checks every default there.
const KEYBOARD_NAMES: &[(i32, &str)] = &[
    (4, "key.keyboard.a"),
    (5, "key.keyboard.b"),
    (6, "key.keyboard.c"),
    (7, "key.keyboard.d"),
    (8, "key.keyboard.e"),
    (9, "key.keyboard.f"),
    (10, "key.keyboard.g"),
    (11, "key.keyboard.h"),
    (12, "key.keyboard.i"),
    (13, "key.keyboard.j"),
    (14, "key.keyboard.k"),
    (15, "key.keyboard.l"),
    (16, "key.keyboard.m"),
    (17, "key.keyboard.n"),
    (18, "key.keyboard.o"),
    (19, "key.keyboard.p"),
    (20, "key.keyboard.q"),
    (21, "key.keyboard.r"),
    (22, "key.keyboard.s"),
    (23, "key.keyboard.t"),
    (24, "key.keyboard.u"),
    (25, "key.keyboard.v"),
    (26, "key.keyboard.w"),
    (27, "key.keyboard.x"),
    (28, "key.keyboard.y"),
    (29, "key.keyboard.z"),
    (30, "key.keyboard.1"),
    (31, "key.keyboard.2"),
    (32, "key.keyboard.3"),
    (33, "key.keyboard.4"),
    (34, "key.keyboard.5"),
    (35, "key.keyboard.6"),
    (36, "key.keyboard.7"),
    (37, "key.keyboard.8"),
    (38, "key.keyboard.9"),
    (39, "key.keyboard.0"),
    (40, "key.keyboard.enter"),
    (41, "key.keyboard.escape"),
    (42, "key.keyboard.backspace"),
    (43, "key.keyboard.tab"),
    (44, "key.keyboard.space"),
    (45, "key.keyboard.minus"),
    (46, "key.keyboard.equal"),
    (47, "key.keyboard.left.bracket"),
    (48, "key.keyboard.right.bracket"),
    (49, "key.keyboard.backslash"),
    (51, "key.keyboard.semicolon"),
    (52, "key.keyboard.apostrophe"),
    (53, "key.keyboard.grave.accent"),
    (54, "key.keyboard.comma"),
    (55, "key.keyboard.period"),
    (56, "key.keyboard.slash"),
    (57, "key.keyboard.caps.lock"),
    (58, "key.keyboard.f1"),
    (59, "key.keyboard.f2"),
    (60, "key.keyboard.f3"),
    (61, "key.keyboard.f4"),
    (62, "key.keyboard.f5"),
    (63, "key.keyboard.f6"),
    (64, "key.keyboard.f7"),
    (65, "key.keyboard.f8"),
    (66, "key.keyboard.f9"),
    (67, "key.keyboard.f10"),
    (68, "key.keyboard.f11"),
    (69, "key.keyboard.f12"),
    (70, "key.keyboard.print.screen"),
    (71, "key.keyboard.scroll.lock"),
    (72, "key.keyboard.pause"),
    (73, "key.keyboard.insert"),
    (74, "key.keyboard.home"),
    (75, "key.keyboard.page.up"),
    (76, "key.keyboard.delete"),
    (77, "key.keyboard.end"),
    (78, "key.keyboard.page.down"),
    (79, "key.keyboard.right"),
    (80, "key.keyboard.left"),
    (81, "key.keyboard.down"),
    (82, "key.keyboard.up"),
    (83, "key.keyboard.num.lock"),
    (84, "key.keyboard.keypad.divide"),
    (85, "key.keyboard.keypad.multiply"),
    (86, "key.keyboard.keypad.subtract"),
    (87, "key.keyboard.keypad.add"),
    (88, "key.keyboard.keypad.enter"),
    (89, "key.keyboard.keypad.1"),
    (90, "key.keyboard.keypad.2"),
    (91, "key.keyboard.keypad.3"),
    (92, "key.keyboard.keypad.4"),
    (93, "key.keyboard.keypad.5"),
    (94, "key.keyboard.keypad.6"),
    (95, "key.keyboard.keypad.7"),
    (96, "key.keyboard.keypad.8"),
    (97, "key.keyboard.keypad.9"),
    (98, "key.keyboard.keypad.0"),
    (99, "key.keyboard.keypad.decimal"),
    (224, "key.keyboard.left.control"),
    (225, "key.keyboard.left.shift"),
    (226, "key.keyboard.left.alt"),
    (227, "key.keyboard.left.win"),
    (228, "key.keyboard.right.control"),
    (229, "key.keyboard.right.shift"),
    (230, "key.keyboard.right.alt"),
    (231, "key.keyboard.right.win"),
];

/// A category marker's identifier: `key-category/<namespace>:<path>`.
fn category_id(value: &Value, categories: &BTreeMap<String, String>) -> Option<String> {
    let text = expect_ref(value)?;
    if let Some(id) = text.strip_prefix(CATEGORY) {
        return Some(id.to_owned());
    }
    // A pre-26.3 mod's `String` category (`key.categories.examplemod`).
    if text.starts_with("key.") {
        return Some(text.clone());
    }
    categories.get(text).cloned()
}

/// `KeyMapping.<init>`'s arguments, for every 26.3 overload (and the
/// `String`-category ones of mods built before 1.21.9).
fn key_init(
    descriptor: &str,
    args: &[Value],
    categories: &BTreeMap<String, String>,
) -> Option<KeyInit> {
    let name = expect_ref(args.first()?)?.clone();
    let typed = descriptor
        .starts_with("(Ljava/lang/String;Lcom/mojang/blaze3d/platform/InputConstants$Type;I");
    let (mouse, code, category) = if typed {
        let mouse = expect_ref(args.get(1)?).is_some_and(|t| t.ends_with("/MOUSE"));
        (mouse, int_arg(args, 2)?, args.get(3)?)
    } else if descriptor.starts_with("(Ljava/lang/String;I") {
        (false, int_arg(args, 1)?, args.get(2)?)
    } else {
        return None;
    };
    Some(KeyInit {
        name,
        mouse,
        code,
        category: category_id(category, categories),
    })
}

impl ClientState {
    fn in_frame(&self) -> bool {
        self.frame.is_some()
    }

    fn pose(&self) -> Pose {
        self.poses.last().copied().unwrap_or(IDENTITY)
    }

    fn register_key(&mut self, value: &Value, source: &'static str) {
        let Some(marker) = expect_ref(value) else {
            self.refused.push(format!("{source}: null key mapping"));
            return;
        };
        if self.keys.iter().any(|key| key.marker == *marker) {
            return;
        }
        let Some(init) = self.key_inits.get(marker) else {
            self.refused.push(format!(
                "{source}: {marker} was not built by a KeyMapping constructor"
            ));
            return;
        };
        if self.keys.len() >= MAX_KEYS_PER_JAR {
            self.refused.push(format!(
                "{source}: more than {MAX_KEYS_PER_JAR} key mappings"
            ));
            return;
        }
        self.keys.push(ClientKeyMapping {
            marker: marker.clone(),
            name: init.name.clone(),
            default_key: input_key_name(init.mouse, init.code),
            category: init
                .category
                .clone()
                .unwrap_or_else(|| "minecraft:misc".into()),
            source,
        });
    }

    fn register_layer(&mut self, id: &Value, element: &Value, source: &'static str) {
        if self.layers.len() >= MAX_LAYERS_PER_JAR {
            self.refused.push(format!(
                "{source}: more than {MAX_LAYERS_PER_JAR} HUD elements"
            ));
            return;
        }
        let (namespace, path) = identifier_parts(id);
        self.layers
            .push((format!("{namespace}:{path}"), element.clone(), source));
    }

    fn draw(&mut self, op: DrawOp) -> Result<(), ()> {
        if self.draws.len() >= MAX_DRAWS_PER_CALL {
            return Err(());
        }
        self.draws.push(op);
        Ok(())
    }

    fn message(&mut self, message: ClientMessage) {
        if self.messages.len() < MAX_MESSAGES {
            self.messages.push(message);
        }
    }

    /// `fill(x1, y1, x2, y2, color)` after the pose, corners in any order.
    fn fill(&mut self, corners: [i32; 4], argb: u32) -> Result<(), ()> {
        if argb >> 24 == 0 {
            return Ok(());
        }
        let rect = self.posed_rect(corners);
        self.draw(DrawOp::Fill { rect, argb })
    }

    fn posed_rect(&self, [x1, y1, x2, y2]: [i32; 4]) -> [f32; 4] {
        let [sx, sy, tx, ty] = self.pose();
        let (ax, bx) = (x1 as f32 * sx + tx, x2 as f32 * sx + tx);
        let (ay, by) = (y1 as f32 * sy + ty, y2 as f32 * sy + ty);
        [ax.min(bx), ay.min(by), (ax - bx).abs(), (ay - by).abs()]
    }

    fn text(
        &mut self,
        text: ClientText,
        x: i32,
        y: i32,
        argb: u32,
        shadow: bool,
    ) -> Result<(), ()> {
        // GuiGraphicsExtractor.text: a colour with no alpha draws nothing.
        if argb >> 24 == 0 {
            return Ok(());
        }
        let [sx, sy, tx, ty] = self.pose();
        self.draw(DrawOp::Text {
            text,
            at: [x as f32 * sx + tx, y as f32 * sy + ty],
            argb,
            shadow,
            scale: [sx, sy],
        })
    }
}

impl StdHost {
    /// A server's host (`with_environment_type("SERVER")`) has no client:
    /// there, none of this surface answers, and the host behaves exactly as
    /// it did before the surface existed.
    fn client_environment(&self) -> bool {
        self.environment_type != "SERVER"
    }

    /// The key mappings this host's jar registered, in order.
    pub fn client_key_mappings(&self) -> &[ClientKeyMapping] {
        &self.client.keys
    }

    /// Registrations that did not become a key mapping or HUD element, with
    /// why (a mapping built some other way, a jar over a bound).
    pub fn client_refused_registrations(&self) -> &[String] {
        &self.client.refused
    }

    /// The HUD elements this host's jar registered, resolved against `jar`:
    /// a lambda or an object of a jar class implementing the element's
    /// method. An element that resolves to neither is listed in the second
    /// value with why.
    pub fn client_hud_layers(&self, jar: &NativeJar) -> (Vec<HudLayer>, Vec<String>) {
        let mut layers = Vec::new();
        let mut refused = Vec::new();
        for (id, element, source) in &self.client.layers {
            let callback = match expect_ref(element) {
                Some(text) if text.starts_with("@indy:") => match parse_callback_marker(text) {
                    Ok(marker) => Some(ListenerCallback::Indy {
                        target: marker.target,
                        captures: marker.captures,
                    }),
                    Err(_) => None,
                },
                _ => events::jar_object(jar, element).and_then(|(class, value)| {
                    ["extractRenderState", "render"]
                        .iter()
                        .find_map(|name| events::resolve_method(jar, class, name, LAYER_DESCRIPTOR))
                        .map(|method| ListenerCallback::Object {
                            receiver: value.clone(),
                            method,
                        })
                }),
            };
            match callback {
                Some(callback) => layers.push(HudLayer {
                    id: id.clone(),
                    source,
                    callback,
                }),
                None => refused.push(format!("{source} {id}: the element is not a jar callback")),
            }
        }
        (layers, refused)
    }

    /// Start a client firing: the markers answer from `snapshot`, and the
    /// jar's mappings (in [`Self::client_key_mappings`] order) have `keys`.
    pub fn begin_client_frame(&mut self, snapshot: Arc<ClientSnapshot>, keys: &[KeyPress]) {
        let state = &mut self.client;
        state.frame = Some(snapshot);
        state.presses = keys.to_vec();
        state.presses.resize(state.keys.len(), KeyPress::default());
        state.draws.clear();
        state.messages.clear();
        state.poses.clear();
        state.made.clear();
        state.made_aliases.clear();
        state.tasks.clear();
        let lines = state.frame.as_ref().and_then(|f| {
            f.tooltip_stack.as_ref()?;
            Some(
                f.tooltip_lines
                    .iter()
                    .map(|line| component_value("literal", line))
                    .collect::<Vec<_>>(),
            )
        });
        match lines {
            Some(lines) => {
                self.mutable_collections.insert(TOOLTIP_LIST.into(), lines);
            }
            None => {
                self.mutable_collections.remove(TOOLTIP_LIST);
            }
        }
    }

    /// End the firing [`Self::begin_client_frame`] started: what the
    /// callbacks drew and sent, and the mappings' state after them.
    pub fn end_client_frame(&mut self) -> ClientFrameOutput {
        let state = &mut self.client;
        state.frame = None;
        state.poses.clear();
        state.made.clear();
        state.made_aliases.clear();
        ClientFrameOutput {
            draws: std::mem::take(&mut state.draws),
            messages: std::mem::take(&mut state.messages),
            keys: std::mem::take(&mut state.presses),
            tasks: std::mem::take(&mut state.tasks),
            tooltip: self.mutable_collections.remove(TOOLTIP_LIST).map(|lines| {
                lines
                    .iter()
                    .filter_map(text_arg)
                    .take(MAX_TOOLTIP_LINES)
                    .collect()
            }),
        }
    }

    /// A static field the client surface answers: input types, key
    /// categories, and the fields of the `Minecraft` and `Options` stand-ins
    /// (an instance field of a host object reaches the host as a static read
    /// without its receiver).
    pub(crate) fn client_static(&self, r: &MethodRef) -> Option<Result<Value, NativeError>> {
        if !self.client_environment() {
            return None;
        }
        let state = &self.client;
        let frame = state.frame.as_deref();
        let refuse = || Some(Err(NativeError::UnknownHostCall(r.clone())));
        if let Some(value) = text::text_static(r) {
            return Some(Ok(value));
        }
        match (r.owner.as_str(), r.name.as_str()) {
            (INPUT_TYPE, name @ ("KEYBOARD" | "MOUSE")) => {
                Some(Ok(marker(format!("enum/{INPUT_TYPE}/{name}"))))
            }
            (KEY_CATEGORY, name) if r.descriptor == format!("L{KEY_CATEGORY};") => Some(Ok(
                marker(format!("{CATEGORY}minecraft:{}", name.to_ascii_lowercase())),
            )),
            (MINECRAFT_CLASS, name) => Some(Ok(match name {
                "player" => frame
                    .and_then(|f| f.player.as_ref())
                    .map_or(Value::Ref(None), |_| marker(PLAYER)),
                "level" => frame
                    .and_then(|f| f.level.as_ref())
                    .map_or(Value::Ref(None), |_| marker(LEVEL)),
                // No screen: null. An open one has no stand-in, and refuses.
                "screen" if frame.is_some_and(|f| f.screen_open) => return refuse(),
                "screen" => Value::Ref(None),
                "options" => marker(OPTIONS),
                "gui" => marker(GUI),
                "font" => marker(FONT),
                "gameMode" => frame
                    .and_then(|f| f.player.as_ref())
                    .map_or(Value::Ref(None), |_| marker(GAME_MODE)),
                "gameRenderer" => marker(GAME_RENDERER),
                "debugEntries" => marker(DEBUG),
                _ if frame.is_some() => return refuse(),
                _ => return None,
            })),
            (OPTIONS_CLASS, name) => {
                if let Some((_, mapping)) = OPTION_KEYS.iter().find(|(field, _)| *field == name) {
                    return Some(Ok(marker(format!("{VANILLA_KEY}{mapping}"))));
                }
                match (name, frame) {
                    ("hideGui", Some(frame)) => Some(Ok(flag(frame.hide_gui))),
                    ("keyDebugModifier", _) => {
                        Some(Ok(marker(format!("{VANILLA_KEY}key.debug.modifier"))))
                    }
                    (_, Some(_)) => refuse(),
                    (_, None) => None,
                }
            }
            (FONT_CLASS, "lineHeight") => Some(Ok(Value::Int(9))),
            // `java.awt.Color`'s constants, which HUD mods turn into ARGB
            // ints with `getRGB()` (durabilityviewer's indicator colours).
            ("java/awt/Color", name) => AWT_COLORS
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, argb)| Ok(marker(format!("{AWT_COLOR}{argb:08x}")))),
            // Item, block and effect constants answer only in a firing: an
            // entrypoint reading `Items.DIAMOND` keeps the answer it always got.
            ("net/minecraft/world/item/Items", name) if frame.is_some() => Some(Ok(marker(
                format!("{}{}", items::ITEM, items::constant_id(name)),
            ))),
            ("net/minecraft/world/level/block/Blocks", name) if frame.is_some() => Some(Ok(
                marker(format!("{}{}", items::BLOCK, items::constant_id(name))),
            )),
            ("net/minecraft/world/effect/MobEffects", name) if frame.is_some() => {
                Some(Ok(marker(format!(
                    "{}{}",
                    items::EFFECT_HOLDER,
                    items::effect_constant_id(name)
                ))))
            }
            // `EquipmentSlot.VALUES`: every slot in declaration order.
            ("net/minecraft/world/entity/EquipmentSlot", "VALUES") => {
                let slots: Vec<Value> = [
                    "MAINHAND", "OFFHAND", "FEET", "LEGS", "CHEST", "HEAD", "BODY", "SADDLE",
                ]
                .iter()
                .map(|name| {
                    marker(format!(
                        "enum/net/minecraft/world/entity/EquipmentSlot/{name}"
                    ))
                })
                .collect();
                Some(Ok(crate::collection_value("list", &slots)))
            }
            ("net/minecraft/world/item/ItemStack", "EMPTY") if frame.is_some() => {
                Some(Ok(marker(format!("{}empty", items::STACK))))
            }
            (
                "net/minecraft/world/entity/player/Player"
                | "net/minecraft/client/player/LocalPlayer"
                | "net/minecraft/client/player/AbstractClientPlayer",
                name,
            ) if frame.is_some() => {
                match frame.and_then(|frame| Self::client_player_field(frame, name)) {
                    Some(value) => Some(Ok(value)),
                    None => refuse(),
                }
            }
            ("net/minecraft/client/gui/Gui", "hud") => Some(Ok(marker(HUD))),
            _ => None,
        }
    }

    /// A static call the client surface answers: `Minecraft.getInstance`,
    /// key-mapping and HUD registration, category registration.
    pub(crate) fn client_invoke(
        &mut self,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        if !self.client_environment() {
            return None;
        }
        if let Some(value) = text::text_invoke(r, args) {
            return Some(Ok(value));
        }
        let state = &mut self.client;
        match (r.owner.as_str(), r.name.as_str()) {
            (MINECRAFT_CLASS, "getInstance") => Some(Ok(marker(MINECRAFT))),
            (KEY_HELPER, "registerKeyMapping") => {
                let mapping = args.first().cloned().unwrap_or(Value::Ref(None));
                state.register_key(&mapping, "KeyMappingHelper.registerKeyMapping");
                Some(Ok(mapping))
            }
            (HUD_REGISTRY, "addFirst" | "addLast") if args.len() == 2 => {
                state.register_layer(&args[0], &args[1], "HudElementRegistry.addLast");
                Some(Ok(Value::Void))
            }
            (HUD_REGISTRY, "attachElementBefore" | "attachElementAfter") if args.len() == 3 => {
                state.register_layer(&args[1], &args[2], "HudElementRegistry.attachElement");
                Some(Ok(Value::Void))
            }
            (KEY_CATEGORY, "register") => {
                let (namespace, path) = identifier_parts(args.first()?);
                Some(Ok(marker(format!("{CATEGORY}{namespace}:{path}"))))
            }
            _ => None,
        }
    }

    /// An instance call the client surface answers, or `None` when the
    /// receiver is not one of its objects (the host's usual surface answers
    /// it). During a firing, an unmodelled member of a client object refuses.
    pub(crate) fn client_call(
        &mut self,
        r: &MethodRef,
        receiver: &Value,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        if !self.client_environment() {
            return None;
        }
        let text = expect_ref(receiver)?.clone();
        let refuse = || Err(NativeError::UnknownHostCall(r.clone()));
        // `NonNullList` is a `List`: the inventory's lists answer as one.
        if r.owner == "net/minecraft/core/NonNullList" && text.starts_with("list") {
            let as_list = MethodRef {
                owner: "java/util/List".into(),
                ..r.clone()
            };
            return Some(crate::Host::invoke_instance(self, &as_list, receiver, args));
        }
        // Constructors record their arguments against the new object.
        if r.name == "<init>" {
            if r.owner == KEY_MAPPING {
                if let Some(init) = key_init(&r.descriptor, args, &self.client.categories) {
                    self.client.key_inits.insert(text, init);
                }
                return Some(Ok(Value::Void));
            }
            if r.owner == KEY_CATEGORY {
                let (namespace, path) = identifier_parts(args.first()?);
                self.client
                    .categories
                    .insert(text, format!("{namespace}:{path}"));
                return Some(Ok(Value::Void));
            }
            // `new ItemStack(Items.CHEST)` in a firing: a stack of the frame.
            if r.owner == "net/minecraft/world/item/ItemStack"
                && let Some(frame) = self.client.frame.clone()
            {
                return Some(
                    self.client_make_stack(&frame, &text, args)
                        .map(|()| Value::Void)
                        .ok_or_else(|| NativeError::UnknownHostCall(r.clone())),
                );
            }
            return None;
        }
        // `BuiltInRegistries.ITEM.getKey(item)`: the item's identifier.
        if r.name == "getKey"
            && let Some(id) = args.first().and_then(expect_ref).and_then(|a| {
                a.strip_prefix(items::ITEM)
                    .or_else(|| a.strip_prefix(items::BLOCK))
            })
        {
            return Some(Ok(marker(format!("identifier/{id}"))));
        }
        // A stack the frame built answers as its slot marker does.
        if !text.starts_with("client/")
            && let Some(frame) = self.client.frame.clone()
            && let Some(slot) = self.client.made_aliases.get(&text).cloned()
        {
            let object = match slot {
                StackSlot::Made(n) => format!("{}made/{n}", items::STACK),
                _ => return None,
            };
            let answer = self.client_item_answer(&object, &frame, r, args).flatten();
            return Some(answer.map_or_else(refuse, Ok));
        }
        // `GameType` predicates on the enum constant `getPlayerMode` hands out.
        if let Some(mode) = text.strip_prefix(&format!("enum/{GAME_TYPE}/")) {
            let answer = match (r.name.as_str(), r.descriptor.as_str()) {
                ("isSurvival", "()Z") => matches!(mode, "SURVIVAL" | "ADVENTURE"),
                ("isCreative", "()Z") => mode == "CREATIVE",
                ("isBlockPlacingRestricted", "()Z") => matches!(mode, "ADVENTURE" | "SPECTATOR"),
                _ => return None,
            };
            return Some(Ok(flag(answer)));
        }
        // Components, styles, text colours and formatting constants
        // (`client_text.rs`); an unlisted component member keeps the host's
        // own component model.
        if text.starts_with("component/")
            || text.starts_with("client/style/")
            || text.starts_with("client/text-color/")
            || text.starts_with("enum/net/minecraft/ChatFormatting/")
        {
            if let Some(value) = text::text_call(&text, r, args) {
                return Some(Ok(value));
            }
            if text.starts_with("component/")
                || text.starts_with("enum/")
                || self.client.frame.is_none()
            {
                return None;
            }
            return Some(refuse());
        }
        if let Some(event) = text.strip_prefix("game-event/") {
            return self.client_event_call(event, r, args);
        }
        if self.client.key_inits.contains_key(&text) {
            return self.key_mapping_call(&text, r, args);
        }
        if let Some(id) = text.strip_prefix(CATEGORY) {
            return Some(match (r.name.as_str(), return_desc(&r.descriptor)) {
                ("id", _) => {
                    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
                    Ok(marker(format!("identifier/{namespace}:{path}")))
                }
                ("label", _) => Ok(component_value(
                    "translatable",
                    &format!("key.category.{}", id.replacen(':', ".", 1)),
                )),
                ("hashCode", "I") => Ok(Value::Int(stable_hash(&text))),
                ("equals", "Z") => Ok(flag(args.first() == Some(receiver))),
                ("toString", _) => Ok(marker(text.clone())),
                _ => refuse(),
            });
        }
        if !text.starts_with("client/") {
            return None;
        }
        match (r.name.as_str(), r.descriptor.as_str()) {
            ("hashCode", "()I") => return Some(Ok(Value::Int(stable_hash(&text)))),
            ("equals", "(Ljava/lang/Object;)Z") => {
                return Some(Ok(flag(args.first() == Some(receiver))));
            }
            ("toString", "()Ljava/lang/String;") => return Some(Ok(marker(text))),
            _ => {}
        }
        if let Some(hex) = text.strip_prefix(AWT_COLOR) {
            let argb = u32::from_str_radix(hex, 16).ok()?;
            let channel = |shift: u32| Value::Int(((argb >> shift) & 0xff) as i32);
            return Some(match r.name.as_str() {
                "getRGB" => Ok(Value::Int(argb as i32)),
                "getAlpha" => Ok(channel(24)),
                "getRed" => Ok(channel(16)),
                "getGreen" => Ok(channel(8)),
                "getBlue" => Ok(channel(0)),
                _ => refuse(),
            });
        }
        let Some(frame) = self.client.frame.clone() else {
            // Outside a firing: what the host answered before this surface.
            return None;
        };
        let answer = self.client_answer(&text, &frame, r, args);
        Some(answer.map_or_else(refuse, Ok))
    }

    /// A NeoForge client event object's members: the registration events'
    /// `register` calls, and the player of a login/logout event. Anything
    /// else goes to the game-event façade.
    fn client_event_call(
        &mut self,
        event: &str,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let state = &mut self.client;
        if event == GameEvent::ClientRegisterKeyMappings.name() {
            return match r.name.as_str() {
                "register" => {
                    let mapping = args.first().cloned().unwrap_or(Value::Ref(None));
                    state.register_key(&mapping, "RegisterKeyMappingsEvent.register");
                    Some(Ok(Value::Void))
                }
                // A category needs no registration of its own here: a mapping
                // names its category.
                "registerCategory" => Some(Ok(Value::Void)),
                _ => None,
            };
        }
        if event == GameEvent::ClientRegisterGuiLayers.name() {
            let source = "RegisterGuiLayersEvent";
            return match (r.name.as_str(), args.len()) {
                ("registerBelowAll" | "registerAboveAll", 2) => {
                    state.register_layer(&args[0], &args[1], source);
                    Some(Ok(Value::Void))
                }
                ("registerBelow" | "registerAbove", 3) => {
                    state.register_layer(&args[1], &args[2], source);
                    Some(Ok(Value::Void))
                }
                // Replacing or wrapping a vanilla layer needs the layer.
                _ => Some(Err(NativeError::UnknownHostCall(r.clone()))),
            };
        }
        if event == GameEvent::ClientItemTooltip.name() {
            let frame = state.frame.as_deref();
            return Some(match r.name.as_str() {
                "getToolTip" => Ok(marker(TOOLTIP_LIST)),
                "getItemStack" => Ok(marker(format!("{}tooltip", items::STACK))),
                "getFlags" => Ok(marker(TOOLTIP_FLAG)),
                "getContext" => Ok(marker(TOOLTIP_CONTEXT)),
                "getEntity" => Ok(frame
                    .and_then(|f| f.player.as_ref())
                    .map_or(Value::Ref(None), |_| marker(PLAYER))),
                _ => Err(NativeError::UnknownHostCall(r.clone())),
            });
        }
        let client_event = matches!(
            event,
            "client_join" | "client_disconnect" | "client_tick_start" | "client_tick_end"
        );
        if client_event && r.name == "getPlayer" {
            let frame = state.frame.as_deref();
            return Some(Ok(frame
                .and_then(|f| f.player.as_ref())
                .map_or(Value::Ref(None), |_| marker(PLAYER))));
        }
        None
    }

    /// `KeyMapping` members on a mapping the jar constructed.
    fn key_mapping_call(
        &mut self,
        object: &str,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Result<Value, NativeError>> {
        let state = &mut self.client;
        let index = state.keys.iter().position(|key| key.marker == object);
        let init = state.key_inits.get(object)?.clone();
        let in_frame = state.in_frame();
        let press = index.and_then(|i| state.presses.get_mut(i));
        Some(Ok(match (r.name.as_str(), r.descriptor.as_str()) {
            ("consumeClick", "()Z") => {
                // Outside a firing nothing was pressed: the client is starting.
                let clicked = match press {
                    Some(press) if in_frame && press.clicks > 0 => {
                        press.clicks -= 1;
                        true
                    }
                    _ => false,
                };
                flag(clicked)
            }
            ("isDown", "()Z") => flag(in_frame && press.is_some_and(|p| p.down)),
            ("setDown", "(Z)V") => {
                if let Some(press) = press {
                    press.down = int_arg(args, 0).is_some_and(|v| v != 0);
                }
                Value::Void
            }
            ("getName", "()Ljava/lang/String;") => marker(init.name),
            ("getCategory", _) => marker(format!(
                "{CATEGORY}{}",
                init.category.as_deref().unwrap_or("minecraft:misc")
            )),
            ("hashCode", "()I") => Value::Int(stable_hash(object)),
            ("equals", "(Ljava/lang/Object;)Z") | ("same", _) => flag(
                args.first()
                    .and_then(expect_ref)
                    .is_some_and(|o| o == object),
            ),
            _ if in_frame => return Some(Err(NativeError::UnknownHostCall(r.clone()))),
            _ => return None,
        }))
    }

    /// A client stand-in's member during a firing, or `None` (the caller
    /// refuses).
    fn client_answer(
        &mut self,
        object: &str,
        frame: &ClientSnapshot,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Value> {
        let name = r.name.as_str();
        let returns = return_desc(&r.descriptor);
        let int = |value: i32| Some(Value::Int(value));
        let double = |value: f64| Some(Value::Float(value));
        // A float return is rounded to f32 precision, as the interpreter's `F`.
        let float = |value: f32| Some(Value::Float(f64::from(value)));
        if let Some(coords) = object.strip_prefix(VEC3) {
            let v: Vec<f64> = coords.split(',').filter_map(|c| c.parse().ok()).collect();
            let [x, y, z] = <[f64; 3]>::try_from(v).ok()?;
            return match (name, returns) {
                ("x", "D") => double(x),
                ("y", "D") => double(y),
                ("z", "D") => double(z),
                ("distanceTo", "D") | ("distanceToSqr", "D") => {
                    let o = expect_ref(args.first()?)?.strip_prefix(VEC3)?;
                    let o: Vec<f64> = o.split(',').filter_map(|c| c.parse().ok()).collect();
                    let d2 = (x - o.first()?).powi(2)
                        + (y - o.get(1)?).powi(2)
                        + (z - o.get(2)?).powi(2);
                    double(if name == "distanceTo" { d2.sqrt() } else { d2 })
                }
                ("toString", _) => Some(marker(format!("({x}, {y}, {z})"))),
                _ => None,
            };
        }
        if let Some(option) = object.strip_prefix(OPTION) {
            return match (option, name) {
                ("renderDistance", "get") => {
                    Some(marker(format!("integer:{}", frame.render_distance)))
                }
                ("mainHand", "get") => Some(marker(format!(
                    "enum/net/minecraft/world/entity/HumanoidArm/{}",
                    if frame.left_handed { "LEFT" } else { "RIGHT" }
                ))),
                ("hideGui", "get") => Some(boolean_value(frame.hide_gui)),
                _ => None,
            };
        }
        if let Some(mapping) = object.strip_prefix(VANILLA_KEY) {
            return match (name, returns) {
                ("isDown", "Z") => Some(flag(frame.vanilla_keys_down.iter().any(|k| k == mapping))),
                ("getName", _) => Some(marker(mapping)),
                _ => None,
            };
        }
        match object {
            MINECRAFT => match (name, returns) {
                ("getWindow", _) => Some(marker(WINDOW)),
                ("isPaused", "Z") => Some(flag(frame.paused)),
                ("isWindowActive", "Z") => Some(flag(true)),
                ("hasSingleplayerServer" | "isLocalServer" | "isSingleplayer", "Z") => {
                    Some(flag(frame.single_player))
                }
                ("getFps", "I") => int(frame.fps),
                ("getDeltaTracker", _) => Some(marker(DELTA)),
                ("getDebugOverlay", _) => Some(marker(DEBUG)),
                ("getConnection", _) => Some(if frame.level.is_some() {
                    marker(CONNECTION)
                } else {
                    Value::Ref(None)
                }),
                ("getCameraEntity", _) => Some(if frame.player.is_some() {
                    marker(PLAYER)
                } else {
                    Value::Ref(None)
                }),
                ("isSameThread", "Z") => Some(flag(true)),
                ("getSingleplayerServer", _) => Some(if frame.single_player {
                    marker(INTEGRATED_SERVER)
                } else {
                    Value::Ref(None)
                }),
                ("getTextureManager", _) => Some(marker(TEXTURE_MANAGER)),
                ("getUser", _) if frame.player.is_some() => Some(marker(USER)),
                ("execute", "V") | ("schedule" | "submit", _) => {
                    let task = args.first()?.clone();
                    if self.client.tasks.len() >= MAX_TASKS {
                        return None;
                    }
                    self.client.tasks.push(task);
                    // `submit` answers a future; nothing waits on it here.
                    Some(if returns == "V" {
                        Value::Void
                    } else {
                        Value::Ref(None)
                    })
                }
                // `getCurrentServer` is null in single player and out of a
                // world; a server's address and name are not in the façade.
                ("getCurrentServer", _) if frame.single_player || frame.level.is_none() => {
                    Some(Value::Ref(None))
                }
                _ => None,
            },
            OPTIONS => match name {
                "renderDistance" | "mainHand" => Some(marker(format!("{OPTION}{name}"))),
                _ => None,
            },
            WINDOW => match (name, returns) {
                ("getGuiScaledWidth", "I") => int(frame.gui_size[0]),
                ("getGuiScaledHeight", "I") => int(frame.gui_size[1]),
                ("getWidth" | "getScreenWidth", "I") => int(frame.window_size[0]),
                ("getHeight" | "getScreenHeight", "I") => int(frame.window_size[1]),
                ("getGuiScale", "D") => double(frame.gui_scale),
                _ => None,
            },
            GUI | HUD => match (name, returns) {
                ("getChat", _) => Some(marker(CHAT)),
                ("getGuiTicks", "I") => int(frame.gui_ticks),
                ("isHidden", "Z") => Some(flag(frame.hide_gui)),
                // 26.3 reads the open screen through the GUI.
                ("screen", _) if !frame.screen_open => Some(Value::Ref(None)),
                ("setOverlayMessage", "V") => {
                    let text = text_arg(args.first()?)?;
                    self.client.message(ClientMessage::Overlay(text));
                    Some(Value::Void)
                }
                _ => None,
            },
            CONNECTION => match (name, returns) {
                ("getLevel", _) if frame.level.is_some() => Some(marker(LEVEL)),
                ("getServerData", _) if frame.single_player => Some(Value::Ref(None)),
                _ => None,
            },
            CHAT => match (name, r.descriptor.as_str()) {
                ("addMessage", "(Lnet/minecraft/network/chat/Component;)V") => {
                    let text = text_arg(args.first()?)?;
                    self.client.message(ClientMessage::Chat(text));
                    Some(Value::Void)
                }
                _ => None,
            },
            PLAYER => {
                let p = frame.player.as_ref()?;
                if let Some(value) = self.client_player_items(frame, name, returns, args) {
                    return Some(value);
                }
                match (name, returns) {
                    ("displayClientMessage", "V") => {
                        let text = text_arg(args.first()?)?;
                        let overlay = int_arg(args, 1).is_some_and(|v| v != 0);
                        self.client.message(if overlay {
                            ClientMessage::Overlay(text)
                        } else {
                            ClientMessage::Chat(text)
                        });
                        Some(Value::Void)
                    }
                    ("sendSystemMessage", "V") => {
                        let text = text_arg(args.first()?)?;
                        self.client.message(ClientMessage::Chat(text));
                        Some(Value::Void)
                    }
                    ("position", _) => Some(vec3(p.pos)),
                    ("getEyeY", "D") => double(p.pos[1] + if p.sneaking { 1.27 } else { 1.62 }),
                    ("getX", "D") => double(p.pos[0]),
                    ("getY", "D") => double(p.pos[1]),
                    ("getZ", "D") => double(p.pos[2]),
                    ("getYRot", "F") => float(p.yaw),
                    ("getXRot", "F") => float(p.pitch),
                    ("getHealth", "F") => float(p.health),
                    ("getMaxHealth", "F") => float(p.max_health),
                    ("isCreative", "Z") => Some(flag(p.creative)),
                    ("isSpectator", "Z") => Some(flag(p.spectator)),
                    ("isShiftKeyDown" | "isCrouching", "Z") => Some(flag(p.sneaking)),
                    ("isSprinting", "Z") => Some(flag(p.sprinting)),
                    ("onGround", "Z") => Some(flag(p.on_ground)),
                    ("isFallFlying", "Z") => Some(flag(p.fall_flying)),
                    ("isAlive", "Z") => Some(flag(p.health > 0.0)),
                    ("getId", "I") => int(p.entity_id),
                    ("getName" | "getDisplayName", _) => Some(component_value("literal", &p.name)),
                    ("getScoreboardName" | "getPlainTextName", _) => Some(marker(p.name.clone())),
                    ("getUUID", _) => uuid_value(&uuid_text(p.uuid)).ok(),
                    ("getStringUUID", _) => Some(marker(uuid_text(p.uuid))),
                    ("level" | "clientLevel", _) if frame.level.is_some() => Some(marker(LEVEL)),
                    _ => None,
                }
            }
            LEVEL => {
                let level = frame.level.as_ref()?;
                match (name, returns) {
                    ("getGameTime", "J") => Some(Value::Long(level.game_time)),
                    // 26.3 names the day clock after the overworld.
                    ("getDayTime" | "getOverworldClockTime", "J") => {
                        Some(Value::Long(level.day_time))
                    }
                    ("dimension", _) => Some(marker(format!(
                        "resource-key/dimension/{}",
                        level.dimension
                    ))),
                    ("isClientSide", "Z") => Some(flag(true)),
                    ("getMinY", "I") => int(level.min_y),
                    ("getHeight", "I") => int(level.height),
                    ("getMaxY", "I") => int(level.min_y + level.height - 1),
                    _ => None,
                }
            }
            TOOLTIP_FLAG => match (name, returns) {
                ("isAdvanced", "Z") => Some(flag(frame.advanced_tooltips)),
                ("isCreative", "Z") => Some(flag(false)),
                _ => None,
            },
            GAME_RENDERER => match (name, returns) {
                ("getMainCamera" | "mainCamera", _) if frame.camera.is_some() => {
                    Some(marker(CAMERA))
                }
                _ => None,
            },
            CAMERA => {
                let camera = frame.camera.as_ref()?;
                match (name, returns) {
                    ("position" | "getPosition", _) => Some(vec3(camera.pos)),
                    ("xRot" | "getXRot", "F") => float(camera.pitch),
                    ("yRot" | "getYRot" | "yaw", "F") => float(camera.yaw),
                    ("isDetached", "Z") => Some(flag(camera.detached)),
                    ("isInitialized", "Z") => Some(flag(true)),
                    ("entity" | "getEntity", _) => Some(if frame.player.is_some() {
                        marker(PLAYER)
                    } else {
                        Value::Ref(None)
                    }),
                    _ => None,
                }
            }
            // Naming a texture is free; registering or uploading one is not
            // modelled, and refuses.
            TEXTURE_MANAGER => match (name, returns) {
                ("getTexture", _) => {
                    let (namespace, path) = identifier_parts(args.first()?);
                    Some(marker(format!("client/texture/{namespace}:{path}")))
                }
                ("release", "V") => Some(Value::Void),
                _ => None,
            },
            INTEGRATED_SERVER => match (name, returns) {
                ("isDedicatedServer", "Z") => Some(flag(false)),
                ("isSingleplayer", "Z") => Some(flag(true)),
                ("isPublished", "Z") => Some(flag(frame.lan_open)),
                _ => None,
            },
            USER => {
                let p = frame.player.as_ref()?;
                match (name, returns) {
                    ("getName", _) => Some(marker(p.name.clone())),
                    ("getProfileId", _) => uuid_value(&uuid_text(p.uuid)).ok(),
                    _ => None,
                }
            }
            GAME_MODE => match (name, returns) {
                ("getPlayerMode", _) => {
                    let mode = &frame.player.as_ref()?.game_mode;
                    Some(marker(format!("enum/{GAME_TYPE}/{mode}")))
                }
                _ => None,
            },
            DEBUG => match (name, returns) {
                // `getDebugOverlay().showDebugScreen()` and 26.3's
                // `debugEntries.isOverlayVisible()`.
                ("showDebugScreen" | "isOverlayVisible", "Z") => Some(flag(frame.debug_screen)),
                _ => None,
            },
            FONT => match (name, returns) {
                ("width", "I") => {
                    let text = text_arg(args.first()?)?;
                    int(frame.metrics.width(&frame.metrics.resolve(&text)))
                }
                _ => None,
            },
            DELTA => match (name, returns) {
                ("getGameTimeDeltaPartialTick", "F") => float(frame.partial_tick),
                ("getGameTimeDeltaTicks" | "getRealtimeDeltaTicks", "F") => float(1.0),
                _ => None,
            },
            GRAPHICS => self.graphics_call(frame, r, args),
            POSE => self.pose_call(r, args),
            _ => self.client_item_answer(object, frame, r, args).flatten(),
        }
    }

    /// `GuiGraphicsExtractor` members: drawing records a [`DrawOp`]; one past
    /// [`MAX_DRAWS_PER_CALL`] refuses the callback.
    fn graphics_call(
        &mut self,
        frame: &ClientSnapshot,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Value> {
        let state = &mut self.client;
        let ints = |from: usize, n: usize| -> Option<Vec<i32>> {
            (from..from + n).map(|at| int_arg(args, at)).collect()
        };
        let drawn = match (r.name.as_str(), r.descriptor.as_str()) {
            ("guiWidth", "()I") => return Some(Value::Int(frame.gui_size[0])),
            ("guiHeight", "()I") => return Some(Value::Int(frame.gui_size[1])),
            ("pose", _) => return Some(marker(POSE)),
            ("fill", "(IIIII)V") => {
                let v = ints(0, 5)?;
                state.fill([v[0], v[1], v[2], v[3]], v[4] as u32)
            }
            ("fillGradient", "(IIIIII)V") => {
                let v = ints(0, 6)?;
                let rect = state.posed_rect([v[0], v[1], v[2], v[3]]);
                state.draw(DrawOp::Gradient {
                    rect,
                    top: v[4] as u32,
                    bottom: v[5] as u32,
                })
            }
            // GuiGraphicsExtractor.outline: four one-pixel fills.
            ("outline", "(IIIII)V") => {
                let v = ints(0, 5)?;
                let (x, y, w, h, c) = (v[0], v[1], v[2], v[3], v[4] as u32);
                state
                    .fill([x, y, x + w, y + 1], c)
                    .and_then(|()| state.fill([x, y + h - 1, x + w, y + h], c))
                    .and_then(|()| state.fill([x, y + 1, x + 1, y + h - 1], c))
                    .and_then(|()| state.fill([x + w - 1, y + 1, x + w, y + h - 1], c))
            }
            ("text", descriptor) if descriptor.starts_with("(Lnet/minecraft/client/gui/Font;") => {
                // A null string draws nothing (`text` returns early).
                let Some(text) = text_arg(args.get(1)?) else {
                    return Some(Value::Void);
                };
                let v = ints(2, 3)?;
                // The five-argument overloads draw with a shadow.
                let shadow = int_arg(args, 5).is_none_or(|v| v != 0);
                state.text(text, v[0], v[1], v[2] as u32, shadow)
            }
            ("horizontalLine", "(IIII)V") => {
                let v = ints(0, 4)?;
                let (x0, x1) = (v[0].min(v[1]), v[0].max(v[1]));
                state.fill([x0, v[2], x1 + 1, v[2] + 1], v[3] as u32)
            }
            ("verticalLine", "(IIII)V") => {
                let v = ints(0, 4)?;
                let (y0, y1) = (v[1].min(v[2]), v[1].max(v[2]));
                state.fill([v[0], y0 + 1, v[0] + 1, y1], v[3] as u32)
            }
            // A fill through an explicit pipeline (`RenderPipelines.GUI`).
            ("fill", "(Lcom/mojang/blaze3d/pipeline/RenderPipeline;IIIII)V") => {
                let v = ints(1, 5)?;
                state.fill([v[0], v[1], v[2], v[3]], v[4] as u32)
            }
            ("item" | "fakeItem", descriptor)
                if descriptor.starts_with("(Lnet/minecraft/world/item/ItemStack;II") =>
            {
                let slot = self.client_stack_slot(args.first()?)?;
                let v = ints(1, 2)?;
                return self
                    .client_item_draw(frame, slot, [v[0], v[1]], true, None)
                    .map(|()| Value::Void);
            }
            // `item(owner, stack, x, y, seed)`: the owner only seeds animation.
            ("item", "(Lnet/minecraft/world/entity/LivingEntity;Lnet/minecraft/world/item/ItemStack;III)V") => {
                let slot = self.client_stack_slot(args.get(1)?)?;
                let v = ints(2, 2)?;
                return self
                    .client_item_draw(frame, slot, [v[0], v[1]], true, None)
                    .map(|()| Value::Void);
            }
            ("itemDecorations", descriptor)
                if descriptor.starts_with("(Lnet/minecraft/client/gui/Font;Lnet/minecraft/world/item/ItemStack;II") =>
            {
                let slot = self.client_stack_slot(args.get(1)?)?;
                let v = ints(2, 2)?;
                let count = args.get(4).map(|text| {
                    text_arg(text).map(|t| match t {
                        ClientText::Literal(s) | ClientText::Translatable(s) => s,
                        ClientText::Json(json) => serde_json::from_str(&json)
                            .map(|json| crate::facade_text::plain(&json))
                            .unwrap_or_default(),
                    })
                });
                return self
                    .client_item_draw(frame, slot, [v[0], v[1]], false, Some(count.flatten()))
                    .map(|()| Value::Void);
            }
            // `blitSprite(pipeline, sprite, x, y, w, h[, colour | alpha])`.
            ("blitSprite", descriptor)
                if descriptor.starts_with(
                    "(Lcom/mojang/blaze3d/pipeline/RenderPipeline;Lnet/minecraft/resources/Identifier;IIII",
                ) =>
            {
                let (namespace, path) = identifier_parts(args.get(1)?);
                let v = ints(2, 4)?;
                let argb = match (args.get(6), descriptor.contains("IIIIF)")) {
                    (Some(alpha), true) => {
                        let alpha = float_arg(std::slice::from_ref(alpha), 0)?;
                        ((alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24 | 0x00ff_ffff
                    }
                    (Some(Value::Int(colour)), false) => *colour as u32,
                    _ => u32::MAX,
                };
                // A GUI sprite is named by its path under `textures/gui/sprites`;
                // a mod's keeps its namespace.
                let sprite = if namespace == "minecraft" {
                    path.to_owned()
                } else {
                    format!("{namespace}:{path}")
                };
                let rect = state.posed_rect([v[0], v[1], v[0] + v[2], v[1] + v[3]]);
                state.draw(DrawOp::Sprite { sprite, rect, argb })
            }
            ("centeredText", descriptor)
                if descriptor.starts_with("(Lnet/minecraft/client/gui/Font;") =>
            {
                let Some(text) = text_arg(args.get(1)?) else {
                    return Some(Value::Void);
                };
                let v = ints(2, 3)?;
                // `x - font.width(text) / 2`, Java's integer division.
                let width = frame.metrics.width(&frame.metrics.resolve(&text));
                state.text(text, v[0] - width / 2, v[1], v[2] as u32, true)
            }
            _ => return None,
        };
        drawn.ok().map(|()| Value::Void)
    }

    /// Records one item draw; `None` past [`MAX_DRAWS_PER_CALL`].
    fn client_item_draw(
        &mut self,
        frame: &ClientSnapshot,
        slot: StackSlot,
        [x, y]: [i32; 2],
        model: bool,
        decorations: Option<Option<String>>,
    ) -> Option<()> {
        let stack = self.client_stack(frame, &slot);
        // GuiGraphicsExtractor.item and itemDecorations draw nothing for an
        // empty stack.
        if stack.is_empty() {
            return Some(());
        }
        let [sx, sy, tx, ty] = self.client.pose();
        let made = matches!(slot, StackSlot::Made(_) | StackSlot::Empty);
        self.client
            .draw(DrawOp::Item {
                stack,
                slot: (!made).then_some(slot),
                at: [x as f32 * sx + tx, y as f32 * sy + ty],
                scale: [sx, sy],
                model,
                decorations,
            })
            .ok()
    }

    /// `Matrix3x2fStack` members the pose supports: push, pop, translate and
    /// scale. A rotation (or anything else) refuses.
    fn pose_call(&mut self, r: &MethodRef, args: &[Value]) -> Option<Value> {
        let state = &mut self.client;
        let [sx, sy, tx, ty] = state.pose();
        let returns = return_desc(&r.descriptor);
        let this = || {
            if returns == "V" {
                Value::Void
            } else {
                marker(POSE)
            }
        };
        match (r.name.as_str(), r.descriptor.split(')').next()?) {
            ("pushMatrix", "(") => {
                if state.poses.len() >= MAX_POSE_DEPTH {
                    return None;
                }
                state.poses.push([sx, sy, tx, ty]);
                Some(this())
            }
            ("popMatrix", "(") => {
                state.poses.pop();
                Some(this())
            }
            // JOML's `translate(x, y, dest)` writing into the stack itself is
            // the two-argument form; into another matrix it is not modelled.
            ("translate" | "scale", "(FFLorg/joml/Matrix3x2f;")
                if args.get(2) != Some(&marker(POSE)) =>
            {
                None
            }
            ("translate", "(FF" | "(FFLorg/joml/Matrix3x2f;") => {
                let (dx, dy) = (float_arg(args, 0)?, float_arg(args, 1)?);
                state.set_pose([sx, sy, tx + sx * dx, ty + sy * dy]);
                Some(this())
            }
            ("scale", "(FF" | "(FFLorg/joml/Matrix3x2f;") => {
                let (a, b) = (float_arg(args, 0)?, float_arg(args, 1)?);
                state.set_pose([sx * a, sy * b, tx, ty]);
                Some(this())
            }
            ("scale", "(F") => {
                let a = float_arg(args, 0)?;
                state.set_pose([sx * a, sy * a, tx, ty]);
                Some(this())
            }
            _ => None,
        }
    }
}

impl ClientState {
    /// Replace the current pose (the stack's top, or the base one).
    fn set_pose(&mut self, pose: Pose) {
        match self.poses.last_mut() {
            Some(top) => *top = pose,
            None => self.poses.push(pose),
        }
    }
}

/// A position the façade hands out as a `Vec3`.
fn vec3([x, y, z]: [f64; 3]) -> Value {
    marker(format!("{VEC3}{x},{y},{z}"))
}

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

impl NativeJar {
    /// Run one HUD element for this frame, through `host` (the jar's own,
    /// inside [`StdHost::begin_client_frame`]): its callback gets the
    /// graphics and delta tracker stand-ins. The host's call log is rolled
    /// back afterwards; the draws stay in the frame. An error (an unmodelled
    /// member, the budget, a refused bytecode) is the caller's to disable the
    /// element with.
    pub fn run_hud_layer(
        &self,
        layer: &HudLayer,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<(), NativeError> {
        let marks = host.call_log_marks();
        host.client.poses.clear();
        let args = vec![marker(GRAPHICS), marker(DELTA)];
        let result = self.invoke_listener_callback(&layer.callback, args, host, budget);
        host.truncate_call_log(marks);
        result.map(|_| ())
    }

    /// Run one `Runnable` a callback queued with `Minecraft.execute`, through
    /// `host` inside a client frame: a lambda of this jar, or an object of a
    /// jar class with a `run()` method. Anything else (a JDK or game
    /// runnable) is not a jar callback and refuses.
    pub fn run_client_task(
        &self,
        task: &Value,
        host: &mut StdHost,
        budget: &ExecutionBudget,
    ) -> Result<(), NativeError> {
        let callback = match expect_ref(task) {
            Some(text) if text.starts_with("@indy:") => {
                parse_callback_marker(text)
                    .ok()
                    .map(|marker| ListenerCallback::Indy {
                        target: marker.target,
                        captures: marker.captures,
                    })
            }
            _ => events::jar_object(self, task).and_then(|(class, value)| {
                events::resolve_method(self, class, "run", "()V").map(|method| {
                    ListenerCallback::Object {
                        receiver: value.clone(),
                        method,
                    }
                })
            }),
        };
        let Some(callback) = callback else {
            return Err(NativeError::Metadata(
                "Minecraft.execute: the task is not a jar callback".into(),
            ));
        };
        let marks = host.call_log_marks();
        let result = self.invoke_listener_callback(&callback, Vec::new(), host, budget);
        host.truncate_call_log(marks);
        result.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(owner: &str, name: &str, descriptor: &str) -> MethodRef {
        MethodRef {
            owner: owner.into(),
            name: name.into(),
            descriptor: descriptor.into(),
        }
    }

    fn snapshot() -> Arc<ClientSnapshot> {
        Arc::new(ClientSnapshot {
            gui_size: [320, 240],
            metrics: Arc::new(TextMetrics {
                advances: vec![6.0; 128],
                fallback: 6.0,
                translations: BTreeMap::from([("hud.example.title".into(), "Title".into())]),
            }),
            ..ClientSnapshot::default()
        })
    }

    #[test]
    fn key_codes_name_their_input_constants() {
        // InputConstants.KEY_C, KEY_LSHIFT, KEY_F1 in 26.3 (SDL scancodes).
        assert_eq!(input_key_name(false, 6), "key.keyboard.c");
        assert_eq!(input_key_name(false, 225), "key.keyboard.left.shift");
        assert_eq!(input_key_name(false, 58), "key.keyboard.f1");
        assert_eq!(input_key_name(false, -1), "key.keyboard.unknown");
        assert_eq!(input_key_name(true, 2), "key.mouse.middle");
        assert_eq!(input_key_name(true, 4), "key.mouse.5");
    }

    #[test]
    fn font_width_skips_formatting_codes_and_rounds_up() {
        let metrics = TextMetrics {
            advances: vec![5.5; 128],
            fallback: 8.0,
            translations: BTreeMap::new(),
        };
        assert_eq!(metrics.width("ab"), 11);
        assert_eq!(metrics.width("§cab"), 11);
        assert_eq!(metrics.width("a\u{4e2d}"), 14);
    }

    /// The façade's draw calls land as ops in GUI pixels: the pose moves and
    /// scales them, a colour without alpha draws nothing, `centeredText`
    /// centres on the measured width.
    #[test]
    fn graphics_calls_record_draw_ops_through_the_pose() {
        let mut host = StdHost::new();
        host.begin_client_frame(snapshot(), &[]);
        let graphics = marker(GRAPHICS);
        let text = method(
            "net/minecraft/client/gui/GuiGraphicsExtractor",
            "text",
            "(Lnet/minecraft/client/gui/Font;Ljava/lang/String;IIIZ)V",
        );
        let font = marker(FONT);
        let call = |host: &mut StdHost, r: &MethodRef, receiver: &Value, args: &[Value]| {
            host.client_call(r, receiver, args).unwrap().unwrap()
        };
        call(
            &mut host,
            &text,
            &graphics,
            &[
                font.clone(),
                marker("hi"),
                Value::Int(2),
                Value::Int(3),
                Value::Int(-1),
                Value::Int(0),
            ],
        );
        // Alpha zero: vanilla draws nothing.
        call(
            &mut host,
            &text,
            &graphics,
            &[
                font.clone(),
                marker("gone"),
                Value::Int(0),
                Value::Int(0),
                Value::Int(0xFFFFFF),
                Value::Int(1),
            ],
        );
        let pose = call(
            &mut host,
            &method(
                "net/minecraft/client/gui/GuiGraphicsExtractor",
                "pose",
                "()Lorg/joml/Matrix3x2fStack;",
            ),
            &graphics,
            &[],
        );
        call(
            &mut host,
            &method(
                "org/joml/Matrix3x2fStack",
                "pushMatrix",
                "()Lorg/joml/Matrix3x2fStack;",
            ),
            &pose,
            &[],
        );
        call(
            &mut host,
            &method(
                "org/joml/Matrix3x2fStack",
                "translate",
                "(FF)Lorg/joml/Matrix3x2f;",
            ),
            &pose,
            &[Value::Float(10.0), Value::Float(20.0)],
        );
        call(
            &mut host,
            &method(
                "org/joml/Matrix3x2fStack",
                "scale",
                "(F)Lorg/joml/Matrix3x2f;",
            ),
            &pose,
            &[Value::Float(2.0)],
        );
        call(
            &mut host,
            &method(
                "net/minecraft/client/gui/GuiGraphicsExtractor",
                "fill",
                "(IIIII)V",
            ),
            &graphics,
            &[
                Value::Int(5),
                Value::Int(4),
                Value::Int(1),
                Value::Int(1),
                Value::Int(0x8000_0000u32 as i32),
            ],
        );
        call(
            &mut host,
            &method(
                "net/minecraft/client/gui/GuiGraphicsExtractor",
                "centeredText",
                "(Lnet/minecraft/client/gui/Font;Lnet/minecraft/network/chat/Component;III)V",
            ),
            &graphics,
            &[
                font.clone(),
                component_value("translatable", "hud.example.title"),
                Value::Int(0),
                Value::Int(0),
                Value::Int(-1),
            ],
        );
        call(
            &mut host,
            &method(
                "org/joml/Matrix3x2fStack",
                "popMatrix",
                "()Lorg/joml/Matrix3x2fStack;",
            ),
            &pose,
            &[],
        );
        let width = call(
            &mut host,
            &method(
                "net/minecraft/client/gui/Font",
                "width",
                "(Ljava/lang/String;)I",
            ),
            &font,
            &[marker("abc")],
        );
        assert_eq!(width, Value::Int(18));
        let out = host.end_client_frame();
        assert_eq!(
            out.draws,
            vec![
                DrawOp::Text {
                    text: ClientText::Literal("hi".into()),
                    at: [2.0, 3.0],
                    argb: 0xFFFF_FFFF,
                    shadow: false,
                    scale: [1.0, 1.0],
                },
                // (1,1)-(5,4) scaled by 2 and moved by (10, 20).
                DrawOp::Fill {
                    rect: [12.0, 22.0, 8.0, 6.0],
                    argb: 0x8000_0000,
                },
                // "Title" is 30 wide: x = 0 - 15, scaled by 2, then moved.
                DrawOp::Text {
                    text: ClientText::Translatable("hud.example.title".into()),
                    at: [-20.0, 20.0],
                    argb: 0xFFFF_FFFF,
                    shadow: true,
                    scale: [2.0, 2.0],
                },
            ]
        );
    }

    /// The player's equipment, inventory and effects answer from the frame:
    /// a damaged sword in the selected slot reads its durability, an armour
    /// slot's item is the same marker `Items.<NAME>` reads, a built stack
    /// starts from the item's facts, and an unlisted member still refuses.
    #[test]
    fn stacks_and_effects_answer_from_the_frame() {
        let sword = ClientStack {
            item: "minecraft:diamond_sword".into(),
            count: 1,
            damage: 400,
            max_damage: 1561,
            max_stack: 1,
            name: "Diamond Sword".into(),
            ..ClientStack::empty()
        };
        let helmet = ClientStack {
            item: "minecraft:iron_helmet".into(),
            count: 1,
            max_damage: 165,
            max_stack: 1,
            ..ClientStack::empty()
        };
        let mut inventory = vec![ClientStack::empty(); 36];
        inventory[2] = sword;
        let frame = Arc::new(ClientSnapshot {
            player: Some(ClientPlayer {
                inventory,
                selected: 2,
                armor: [
                    ClientStack::empty(),
                    ClientStack::empty(),
                    ClientStack::empty(),
                    helmet,
                ],
                effects: vec![ClientEffect {
                    id: "minecraft:speed".into(),
                    amplifier: 1,
                    duration: 600,
                    ambient: false,
                    visible: true,
                    show_icon: true,
                    category: "BENEFICIAL".into(),
                    color: 0x33ebff,
                }],
                ..ClientPlayer::default()
            }),
            items: Arc::new(ItemTable::from([(
                "minecraft:chest".into(),
                ClientItem {
                    max_stack: 64,
                    max_damage: 0,
                    name: "Chest".into(),
                    block: true,
                },
            )])),
            ..(*snapshot()).clone()
        });
        let mut host = StdHost::new();
        host.begin_client_frame(frame, &[]);
        let player = marker(PLAYER);
        let call = |host: &mut StdHost,
                    owner: &str,
                    name: &str,
                    desc: &str,
                    receiver: &Value,
                    args: &[Value]| {
            host.client_call(&method(owner, name, desc), receiver, args)
                .unwrap()
                .unwrap()
        };
        const PLAYER_CLASS: &str = "net/minecraft/world/entity/player/Player";
        const STACK_CLASS: &str = "net/minecraft/world/item/ItemStack";
        let held = call(
            &mut host,
            PLAYER_CLASS,
            "getMainHandItem",
            "()Lnet/minecraft/world/item/ItemStack;",
            &player,
            &[],
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "getMaxDamage", "()I", &held, &[]),
            Value::Int(1561)
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "getDamageValue", "()I", &held, &[]),
            Value::Int(400)
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "isDamaged", "()Z", &held, &[]),
            Value::Int(1)
        );
        // Item.getBarWidth: round(13 - 400 * 13 / 1561) = 10.
        assert_eq!(
            call(&mut host, STACK_CLASS, "getBarWidth", "()I", &held, &[]),
            Value::Int(10)
        );
        let head = call(
            &mut host,
            PLAYER_CLASS,
            "getItemBySlot",
            "(Lnet/minecraft/world/entity/EquipmentSlot;)Lnet/minecraft/world/item/ItemStack;",
            &player,
            &[marker("enum/net/minecraft/world/entity/EquipmentSlot/HEAD")],
        );
        let item = call(
            &mut host,
            STACK_CLASS,
            "getItem",
            "()Lnet/minecraft/world/item/Item;",
            &head,
            &[],
        );
        let constant = host
            .client_static(&method(
                "net/minecraft/world/item/Items",
                "IRON_HELMET",
                "Lnet/minecraft/world/item/Item;",
            ))
            .unwrap()
            .unwrap();
        assert_eq!(item, constant);
        // The feet slot is empty: vanilla's empty stack counts 0.
        let feet = call(
            &mut host,
            PLAYER_CLASS,
            "getItemBySlot",
            "(Lnet/minecraft/world/entity/EquipmentSlot;)Lnet/minecraft/world/item/ItemStack;",
            &player,
            &[marker("enum/net/minecraft/world/entity/EquipmentSlot/FEET")],
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "isEmpty", "()Z", &feet, &[]),
            Value::Int(1)
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "getCount", "()I", &feet, &[]),
            Value::Int(0)
        );
        // Inventory.getItem(39) is the head slot.
        let inventory = call(
            &mut host,
            PLAYER_CLASS,
            "getInventory",
            "()Lnet/minecraft/world/entity/player/Inventory;",
            &player,
            &[],
        );
        let slot39 = call(
            &mut host,
            "net/minecraft/world/entity/player/Inventory",
            "getItem",
            "(I)Lnet/minecraft/world/item/ItemStack;",
            &inventory,
            &[Value::Int(39)],
        );
        assert_eq!(slot39, head);
        // A built stack starts from the item's facts.
        let chest = host
            .client_static(&method(
                "net/minecraft/world/level/block/Blocks",
                "CHEST",
                "Lnet/minecraft/world/level/block/Block;",
            ))
            .unwrap()
            .unwrap();
        let built = marker("net/minecraft/world/item/ItemStack#7");
        call(
            &mut host,
            STACK_CLASS,
            "<init>",
            "(Lnet/minecraft/world/level/ItemLike;)V",
            &built,
            &[chest],
        );
        assert_eq!(
            call(
                &mut host,
                STACK_CLASS,
                "getMaxStackSize",
                "()I",
                &built,
                &[]
            ),
            Value::Int(64)
        );
        assert_eq!(
            call(&mut host, STACK_CLASS, "getCount", "()I", &built, &[]),
            Value::Int(1)
        );
        // Effects: the active one reads its duration through its holder.
        let speed = host
            .client_static(&method(
                "net/minecraft/world/effect/MobEffects",
                "SPEED",
                "Lnet/minecraft/core/Holder;",
            ))
            .unwrap()
            .unwrap();
        let effect = call(
            &mut host,
            PLAYER_CLASS,
            "getEffect",
            "(Lnet/minecraft/core/Holder;)Lnet/minecraft/world/effect/MobEffectInstance;",
            &player,
            &[speed],
        );
        assert_eq!(
            call(
                &mut host,
                "net/minecraft/world/effect/MobEffectInstance",
                "getDuration",
                "()I",
                &effect,
                &[]
            ),
            Value::Int(600)
        );
        // An unlisted stack member refuses.
        assert!(matches!(
            host.client_call(
                &method(STACK_CLASS, "getTooltipLines", "()Ljava/util/List;"),
                &held,
                &[]
            ),
            Some(Err(NativeError::UnknownHostCall(_)))
        ));
    }

    /// `java.awt.Color.GREEN.getRGB()` is opaque green, as on the JVM, through
    /// the host's own static and instance paths.
    #[test]
    fn awt_colour_constants_answer_their_argb() {
        let mut host = StdHost::new();
        let green = crate::Host::get_static(
            &mut host,
            &method("java/awt/Color", "GREEN", "Ljava/awt/Color;"),
        )
        .unwrap();
        let rgb = crate::Host::invoke_instance(
            &mut host,
            &method("java/awt/Color", "getRGB", "()I"),
            &green,
            &[],
        )
        .unwrap();
        assert_eq!(rgb, Value::Int(0xff00_ff00_u32 as i32));
    }

    #[test]
    fn an_unmodelled_member_refuses_only_inside_a_firing() {
        let mut host = StdHost::new();
        let r = method(
            "net/minecraft/client/Minecraft",
            "setScreen",
            "(Lnet/minecraft/client/gui/screens/Screen;)V",
        );
        // Bootstrap: the host's usual lenient path answers.
        assert!(
            host.client_call(&r, &marker(MINECRAFT), &[Value::Ref(None)])
                .is_none()
        );
        host.begin_client_frame(snapshot(), &[]);
        assert!(matches!(
            host.client_call(&r, &marker(MINECRAFT), &[Value::Ref(None)]),
            Some(Err(NativeError::UnknownHostCall(_)))
        ));
        let rotate = method(
            "org/joml/Matrix3x2fStack",
            "rotate",
            "(F)Lorg/joml/Matrix3x2f;",
        );
        assert!(matches!(
            host.client_call(&rotate, &marker(POSE), &[Value::Float(1.0)]),
            Some(Err(NativeError::UnknownHostCall(_)))
        ));
    }

    #[test]
    fn a_server_host_has_no_client_surface() {
        let get_instance = method(
            MINECRAFT_CLASS,
            "getInstance",
            "()Lnet/minecraft/client/Minecraft;",
        );
        let mut client = StdHost::new();
        assert_eq!(
            client.client_invoke(&get_instance, &[]).unwrap().unwrap(),
            marker(MINECRAFT)
        );
        // A server never has a `Minecraft`: the call keeps the host's old answer.
        let mut server = StdHost::new().with_environment_type("SERVER");
        assert!(server.client_invoke(&get_instance, &[]).is_none());
    }

    #[test]
    fn a_registered_mapping_answers_clicks_from_its_key_state() {
        let mut host = StdHost::new();
        let object = marker("net/minecraft/client/KeyMapping#0");
        let category = host
            .client_static(&method(
                KEY_CATEGORY,
                "MISC",
                "Lnet/minecraft/client/KeyMapping$Category;",
            ))
            .unwrap()
            .unwrap();
        host.client_call(
            &method(
                KEY_MAPPING,
                "<init>",
                "(Ljava/lang/String;ILnet/minecraft/client/KeyMapping$Category;)V",
            ),
            &object,
            &[marker("key.example.zoom"), Value::Int(6), category],
        );
        let registered = host
            .client_invoke(
                &method(
                    KEY_HELPER,
                    "registerKeyMapping",
                    "(Lnet/minecraft/client/KeyMapping;)Lnet/minecraft/client/KeyMapping;",
                ),
                std::slice::from_ref(&object),
            )
            .unwrap()
            .unwrap();
        // Fabric hands the same mapping back for the mod to keep.
        assert_eq!(registered, object);
        assert_eq!(
            host.client_key_mappings(),
            &[ClientKeyMapping {
                marker: "net/minecraft/client/KeyMapping#0".into(),
                name: "key.example.zoom".into(),
                default_key: "key.keyboard.c".into(),
                category: "minecraft:misc".into(),
                source: "KeyMappingHelper.registerKeyMapping",
            }]
        );
        let consume = method(KEY_MAPPING, "consumeClick", "()Z");
        host.begin_client_frame(
            snapshot(),
            &[KeyPress {
                down: true,
                clicks: 2,
            }],
        );
        let mut clicks = 0;
        while host.client_call(&consume, &object, &[]).unwrap().unwrap() == Value::Int(1) {
            clicks += 1;
        }
        assert_eq!(clicks, 2);
        let down = host.client_call(&method(KEY_MAPPING, "isDown", "()Z"), &object, &[]);
        assert_eq!(down.unwrap().unwrap(), Value::Int(1));
        assert_eq!(
            host.end_client_frame().keys,
            vec![KeyPress {
                down: true,
                clicks: 0
            }]
        );
    }
}
