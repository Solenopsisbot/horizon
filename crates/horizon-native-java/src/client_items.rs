//! The local player's items and effects as the client surface shows them:
//! `ItemStack`, `Item`, `Inventory`, `MobEffectInstance` and their holders.
//!
//! Every object is a marker the façade can answer from the frame's
//! [`ClientSnapshot`]:
//!
//! - a stack is `client/stack/<slot>`: `mainhand`, `offhand`, `feet`, `legs`,
//!   `chest`, `head`, an inventory index `0`..`35`, `tooltip` (the stack a
//!   tooltip callback describes), `empty` (`ItemStack.EMPTY`), or
//!   `made/<n>`, one a callback built itself (`new ItemStack(Items.CHEST)`).
//!   A slot marker reads the slot's stack at the time of the call, so a mod
//!   that keeps one across frames sees the slot's current stack;
//! - an item is `client/item/<id>` (`Items.DIAMOND` in a firing, `getItem()`),
//!   so `stack.getItem() == Items.ELYTRA` compares two equal markers as the JVM
//!   compares two references to the one registered item;
//! - a block is `client/block/<id>` (`Blocks.CHEST`), an `ItemLike` of the item
//!   of the same id;
//! - an effect instance is `client/effect/<i>` (the player's `i`th active
//!   effect), its holder `client/effect-holder/<id>`, the effect itself
//!   `client/mob-effect/<id>`.
//!
//! Item facts that are not on the stack (the stack size and durability a
//! built stack starts with) come from [`ClientSnapshot::items`]. A member the
//! façade does not list refuses the callback, as everywhere on the client
//! surface.

use std::collections::BTreeMap;

use crate::{MethodRef, StdHost, Value, collection_value, component_value};

use super::{ClientSnapshot, flag, int_arg, marker};

pub(super) const STACK: &str = "client/stack/";
pub(super) const ITEM: &str = "client/item/";
pub(super) const BLOCK: &str = "client/block/";
pub(super) const INVENTORY: &str = "client/inventory";
pub(super) const EFFECT: &str = "client/effect/";
pub(super) const EFFECT_HOLDER: &str = "client/effect-holder/";
pub(super) const MOB_EFFECT: &str = "client/mob-effect/";
pub(super) const FOOD: &str = "client/food-data";
const EQUIPMENT_SLOT: &str = "enum/net/minecraft/world/entity/EquipmentSlot/";
const HAND: &str = "enum/net/minecraft/world/InteractionHand/";
const CATEGORY: &str = "enum/net/minecraft/world/effect/MobEffectCategory/";
/// Stacks one firing may build (`new ItemStack(...)`); one more refuses.
const MAX_MADE_STACKS: usize = 256;

/// One item stack as the façade shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientStack {
    /// Item id (`minecraft:diamond_sword`); `minecraft:air` for an empty slot.
    pub item: String,
    pub count: i32,
    /// `getDamageValue`.
    pub damage: i32,
    /// `getMaxDamage`: 0 for an item that does not wear out.
    pub max_damage: i32,
    /// `getMaxStackSize`.
    pub max_stack: i32,
    /// `getHoverName`: the custom name, else the item's name, as the game
    /// shows it.
    pub name: String,
    /// `hasFoil`: drawn with the enchantment glint.
    pub foil: bool,
    /// `isEnchanted`: carries enchantments.
    pub enchanted: bool,
    /// The item places a block (`BlockItem`).
    pub block: bool,
}

impl ClientStack {
    /// The stack of an empty slot (`ItemStack.EMPTY`).
    pub fn empty() -> Self {
        Self {
            item: "minecraft:air".into(),
            count: 0,
            damage: 0,
            max_damage: 0,
            max_stack: 64,
            name: "Air".into(),
            foil: false,
            enchanted: false,
            block: false,
        }
    }

    /// `ItemStack.isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.item == "minecraft:air" || self.count <= 0
    }

    /// `isDamageableItem`: a maximum damage (unbreakable items have none
    /// here: the game leaves it 0 for them).
    fn damageable(&self) -> bool {
        self.max_damage > 0
    }
}

impl Default for ClientStack {
    fn default() -> Self {
        Self::empty()
    }
}

/// One item's facts, for a stack a callback builds itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientItem {
    pub max_stack: i32,
    pub max_damage: i32,
    pub name: String,
    pub block: bool,
}

/// One active effect as the façade shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientEffect {
    /// Effect id (`minecraft:speed`).
    pub id: String,
    pub amplifier: i32,
    /// Remaining ticks; -1 is infinite.
    pub duration: i32,
    pub ambient: bool,
    pub visible: bool,
    pub show_icon: bool,
    /// `MobEffectCategory`: `BENEFICIAL`, `HARMFUL` or `NEUTRAL`.
    pub category: String,
    /// `MobEffect.getColor`, RGB.
    pub color: i32,
}

/// Where a stack marker reads its stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StackSlot {
    /// Inventory index 0..35 (0..8 the hotbar).
    Inventory(usize),
    /// `FEET`, `LEGS`, `CHEST`, `HEAD`: armour index 0..3 in that order.
    Armor(usize),
    MainHand,
    OffHand,
    /// The stack a tooltip callback describes.
    Tooltip,
    Empty,
    /// The `n`th stack the frame's callbacks built.
    Made(usize),
}

impl StackSlot {
    fn marker(&self) -> Value {
        marker(format!(
            "{STACK}{}",
            match self {
                StackSlot::Inventory(i) => i.to_string(),
                StackSlot::Armor(i) => ["feet", "legs", "chest", "head"][*i].to_owned(),
                StackSlot::MainHand => "mainhand".into(),
                StackSlot::OffHand => "offhand".into(),
                StackSlot::Tooltip => "tooltip".into(),
                StackSlot::Empty => "empty".into(),
                StackSlot::Made(n) => format!("made/{n}"),
            }
        ))
    }

    /// The slot of a stack marker's text after [`STACK`].
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "mainhand" => StackSlot::MainHand,
            "offhand" => StackSlot::OffHand,
            "feet" => StackSlot::Armor(0),
            "legs" => StackSlot::Armor(1),
            "chest" => StackSlot::Armor(2),
            "head" => StackSlot::Armor(3),
            "tooltip" => StackSlot::Tooltip,
            "empty" => StackSlot::Empty,
            other => match other.strip_prefix("made/") {
                Some(n) => StackSlot::Made(n.parse().ok()?),
                None => StackSlot::Inventory(other.parse().ok().filter(|i| *i < 36)?),
            },
        })
    }

    /// `EquipmentSlot` constant → slot.
    fn of_equipment(name: &str) -> Option<Self> {
        Some(match name {
            "MAINHAND" => StackSlot::MainHand,
            "OFFHAND" => StackSlot::OffHand,
            "FEET" => StackSlot::Armor(0),
            "LEGS" => StackSlot::Armor(1),
            "CHEST" => StackSlot::Armor(2),
            "HEAD" => StackSlot::Armor(3),
            // A player has no body armour or saddle: vanilla's empty stack.
            "BODY" | "SADDLE" => StackSlot::Empty,
            _ => return None,
        })
    }

    /// `Inventory.getItem(index)`: 0..35 the main inventory, 36..39 the
    /// armour (feet first), 40 the off hand, anything else empty.
    fn of_container_index(index: i32) -> Self {
        match index {
            0..=35 => StackSlot::Inventory(index as usize),
            36..=39 => StackSlot::Armor(index as usize - 36),
            40 => StackSlot::OffHand,
            _ => StackSlot::Empty,
        }
    }
}

/// The stacks a frame's callbacks built, kept only while it runs.
#[derive(Clone, Debug, Default)]
pub(crate) struct MadeStacks {
    stacks: Vec<ClientStack>,
}

impl MadeStacks {
    pub(super) fn clear(&mut self) {
        self.stacks.clear();
    }
}

/// The id of an `ItemLike` marker (`client/item/<id>`, `client/block/<id>`).
fn item_like(value: &Value) -> Option<&str> {
    let text = crate::expect_ref(value)?;
    text.strip_prefix(ITEM).or_else(|| text.strip_prefix(BLOCK))
}

/// `Items.DIAMOND_SWORD` → `minecraft:diamond_sword`: every vanilla item and
/// block constant is its id's path in upper case.
pub(super) fn constant_id(field: &str) -> String {
    format!("minecraft:{}", field.to_ascii_lowercase())
}

/// `MobEffects.SPEED` → `minecraft:speed`; the constants whose 26.3 id is
/// not their name in lower case are listed.
pub(super) fn effect_constant_id(field: &str) -> String {
    let path = match field {
        "MOVEMENT_SPEED" => "speed",
        "MOVEMENT_SLOWDOWN" => "slowness",
        "DIG_SPEED" => "haste",
        "DIG_SLOWDOWN" => "mining_fatigue",
        "DAMAGE_BOOST" => "strength",
        "HEAL" => "instant_health",
        "HARM" => "instant_damage",
        "JUMP" => "jump_boost",
        "CONFUSION" => "nausea",
        "DAMAGE_RESISTANCE" => "resistance",
        other => return format!("minecraft:{}", other.to_ascii_lowercase()),
    };
    format!("minecraft:{path}")
}

/// `Mth.hsvToRgb(h, 1, 1)` as `Item.getBarColor` calls it: the hue's sector
/// with full saturation and value.
fn hsv_full(hue: f32) -> i32 {
    let h = ((hue * 6.0) as i32).rem_euclid(6);
    let f = hue * 6.0 - (hue * 6.0).floor();
    let (v, q, t) = (1.0f32, 1.0 - f, f);
    let (r, g, b) = match h {
        0 => (v, t, 0.0),
        1 => (q, v, 0.0),
        2 => (0.0, v, t),
        3 => (0.0, q, v),
        4 => (t, 0.0, v),
        _ => (v, 0.0, q),
    };
    let byte = |c: f32| (c * 255.0).clamp(0.0, 255.0) as i32;
    (byte(r) << 16) | (byte(g) << 8) | byte(b)
}

impl StdHost {
    /// The stack a slot holds in this frame; `None` outside a world (no
    /// player), where a slot marker cannot have been handed out.
    pub(super) fn client_stack(&self, frame: &ClientSnapshot, slot: &StackSlot) -> ClientStack {
        let player = frame.player.as_ref();
        let stack = match slot {
            StackSlot::Inventory(i) => player.and_then(|p| p.inventory.get(*i)),
            StackSlot::Armor(i) => player.and_then(|p| p.armor.get(*i)),
            StackSlot::MainHand => player.and_then(|p| p.inventory.get(p.selected)),
            StackSlot::OffHand => player.map(|p| &p.offhand),
            StackSlot::Tooltip => frame.tooltip_stack.as_ref(),
            StackSlot::Empty => None,
            StackSlot::Made(n) => self.client.made.stacks.get(*n),
        };
        stack.cloned().unwrap_or_default()
    }

    /// `new ItemStack(item)` / `new ItemStack(item, count)` in a firing: the
    /// stack the item's facts start it as.
    pub(super) fn client_make_stack(
        &mut self,
        frame: &ClientSnapshot,
        receiver: &str,
        args: &[Value],
    ) -> Option<()> {
        let id = item_like(args.first()?)?.to_owned();
        let count = int_arg(args, 1).unwrap_or(1);
        if self.client.made.stacks.len() >= MAX_MADE_STACKS {
            return None;
        }
        let facts = frame.items.get(&id).cloned().unwrap_or_default();
        let stack = ClientStack {
            count,
            max_damage: facts.max_damage,
            max_stack: if facts.max_stack > 0 {
                facts.max_stack
            } else {
                64
            },
            name: if facts.name.is_empty() {
                id.clone()
            } else {
                facts.name
            },
            block: facts.block,
            item: id,
            ..ClientStack::empty()
        };
        let n = self.client.made.stacks.len();
        self.client.made.stacks.push(stack);
        // The jar's `new` made the object; its fields stay the jar's, and
        // every later call on it resolves through this alias.
        self.client
            .made_aliases
            .insert(receiver.to_owned(), StackSlot::Made(n));
        Some(())
    }

    /// The slot behind a stack value: a slot marker, or a stack the frame
    /// built (`new ItemStack`).
    pub(super) fn client_stack_slot(&self, value: &Value) -> Option<StackSlot> {
        let text = crate::expect_ref(value)?;
        if let Some(slot) = text.strip_prefix(STACK) {
            return StackSlot::parse(slot);
        }
        self.client.made_aliases.get(text).cloned()
    }

    /// The local player's members about items and effects, or `None` (the
    /// caller refuses).
    pub(super) fn client_player_items(
        &mut self,
        frame: &ClientSnapshot,
        name: &str,
        returns: &str,
        args: &[Value],
    ) -> Option<Value> {
        let p = frame.player.as_ref()?;
        Some(match (name, returns) {
            ("getMainHandItem", _) => StackSlot::MainHand.marker(),
            ("getOffhandItem", _) => StackSlot::OffHand.marker(),
            ("getItemBySlot", _) => {
                let slot = crate::expect_ref(args.first()?)?.strip_prefix(EQUIPMENT_SLOT)?;
                StackSlot::of_equipment(slot)?.marker()
            }
            ("getItemInHand", _) => match crate::expect_ref(args.first()?)?.strip_prefix(HAND)? {
                "MAIN_HAND" => StackSlot::MainHand.marker(),
                "OFF_HAND" => StackSlot::OffHand.marker(),
                _ => return None,
            },
            ("getInventory", _) => marker(INVENTORY),
            ("getArmorValue", "I") => Value::Int(p.armor_value),
            ("getAbsorptionAmount", "F") => Value::Float(f64::from(p.absorption)),
            ("getAirSupply", "I") => Value::Int(p.air),
            ("getMaxAirSupply", "I") => Value::Int(p.max_air),
            ("getFoodData", _) => marker(FOOD),
            ("getActiveEffects", _) => {
                let effects: Vec<Value> = (0..p.effects.len())
                    .map(|i| marker(format!("{EFFECT}{i}")))
                    .collect();
                collection_value("list", &effects)
            }
            ("hasEffect", "Z") => {
                let id = crate::expect_ref(args.first()?)?.strip_prefix(EFFECT_HOLDER)?;
                flag(p.effects.iter().any(|e| e.id == id))
            }
            ("getEffect", _) => {
                let id = crate::expect_ref(args.first()?)?.strip_prefix(EFFECT_HOLDER)?;
                p.effects
                    .iter()
                    .position(|e| e.id == id)
                    .map_or(Value::Ref(None), |i| marker(format!("{EFFECT}{i}")))
            }
            _ => return None,
        })
    }

    /// The player's instance fields (`player.experienceLevel`).
    pub(super) fn client_player_field(frame: &ClientSnapshot, name: &str) -> Option<Value> {
        let p = frame.player.as_ref()?;
        Some(match name {
            "experienceLevel" => Value::Int(p.xp_level),
            "experienceProgress" => Value::Float(f64::from(p.xp_progress)),
            "totalExperience" => Value::Int(p.xp_total),
            _ => return None,
        })
    }

    /// Members of the item and effect markers, or `None` when `object` is
    /// not one of them. `Some(None)` refuses.
    pub(super) fn client_item_answer(
        &mut self,
        object: &str,
        frame: &ClientSnapshot,
        r: &MethodRef,
        args: &[Value],
    ) -> Option<Option<Value>> {
        let name = r.name.as_str();
        let returns = crate::return_desc(&r.descriptor);
        if let Some(slot) = self.client_stack_slot(&marker(object)) {
            return Some(self.stack_member(frame, &slot, name, returns, args));
        }
        if let Some(id) = object
            .strip_prefix(ITEM)
            .or_else(|| object.strip_prefix(BLOCK))
        {
            let block = object.starts_with(BLOCK);
            return Some(item_member(frame, id, block, name, returns, args));
        }
        if object == INVENTORY {
            return Some(self.inventory_member(frame, name, returns, args));
        }
        if object == FOOD {
            let p = frame.player.as_ref()?;
            return Some(match (name, returns) {
                ("getFoodLevel", "I") => Some(Value::Int(p.food)),
                ("getSaturationLevel", "F") => Some(Value::Float(f64::from(p.saturation))),
                ("needsFood", "Z") => Some(flag(p.food < 20)),
                _ => None,
            });
        }
        if let Some(index) = object.strip_prefix(EFFECT) {
            let effect = frame
                .player
                .as_ref()
                .and_then(|p| p.effects.get(index.parse::<usize>().ok()?));
            return Some(effect.and_then(|e| effect_instance_member(e, name, returns, args)));
        }
        if let Some(id) = object.strip_prefix(EFFECT_HOLDER) {
            return Some(match (name, returns) {
                ("value", _) => Some(marker(format!("{MOB_EFFECT}{id}"))),
                ("is", "Z") => {
                    Some(flag(args.first().is_some_and(|a| {
                        crate::expect_ref(a).is_some_and(|a| a == object)
                    })))
                }
                ("getRegisteredName", _) => Some(marker(id.to_owned())),
                _ => None,
            });
        }
        if let Some(id) = object.strip_prefix(MOB_EFFECT) {
            let known = frame
                .player
                .as_ref()
                .and_then(|p| p.effects.iter().find(|e| e.id == id));
            let key = format!("effect.{}", id.replacen(':', ".", 1));
            return Some(match (name, returns) {
                ("getDescriptionId", _) => Some(marker(key)),
                ("getDisplayName", _) => Some(component_value("translatable", &key)),
                // Only an effect the player has carries its category and
                // colour in the frame.
                ("isBeneficial", "Z") => known.map(|e| flag(e.category == "BENEFICIAL")),
                ("getCategory", _) => known.map(|e| marker(format!("{CATEGORY}{}", e.category))),
                ("getColor", "I") => known.map(|e| Value::Int(e.color)),
                _ => None,
            });
        }
        None
    }

    fn stack_member(
        &mut self,
        frame: &ClientSnapshot,
        slot: &StackSlot,
        name: &str,
        returns: &str,
        args: &[Value],
    ) -> Option<Value> {
        let stack = self.client_stack(frame, slot);
        let empty = stack.is_empty();
        Some(match (name, returns) {
            ("isEmpty", "Z") => flag(empty),
            ("getCount", "I") => Value::Int(if empty { 0 } else { stack.count }),
            ("getItem", _) => marker(format!(
                "{ITEM}{}",
                if empty { "minecraft:air" } else { &stack.item }
            )),
            ("getMaxDamage", "I") => Value::Int(stack.max_damage),
            ("getDamageValue", "I") => Value::Int(stack.damage.clamp(0, stack.max_damage.max(0))),
            ("isDamageableItem", "Z") => flag(stack.damageable()),
            ("isDamaged", "Z") => flag(stack.damageable() && stack.damage > 0),
            ("getMaxStackSize", "I") => Value::Int(stack.max_stack),
            ("isStackable", "Z") => {
                flag(stack.max_stack > 1 && !(stack.damageable() && stack.damage > 0))
            }
            ("hasFoil", "Z") => flag(stack.foil),
            ("isEnchanted", "Z") => flag(stack.enchanted),
            // Item.isBarVisible's default: a damaged item shows its bar.
            ("isBarVisible", "Z") => flag(stack.damageable() && stack.damage > 0),
            ("getBarWidth", "I") if stack.damageable() => {
                let width = (13.0 - stack.damage as f32 * 13.0 / stack.max_damage as f32).round();
                Value::Int((width as i32).clamp(0, 13))
            }
            ("getBarColor", "I") if stack.damageable() => {
                let max = stack.max_damage as f32;
                let health = ((max - stack.damage as f32) / max).max(0.0);
                Value::Int(hsv_full(health / 3.0))
            }
            ("getHoverName" | "getStyledHoverName" | "getItemName", _) => {
                component_value("literal", &stack.name)
            }
            ("getDisplayName", _) => component_value("literal", &format!("[{}]", stack.name)),
            ("is", "Z") => {
                let id = item_like(args.first()?)?;
                flag(!empty && stack.item == id || empty && id == "minecraft:air")
            }
            // A copy reads the same slot: a callback that copies to measure
            // never writes one back.
            ("copy", _) => slot.marker(),
            ("toString", _) => marker(format!(
                "{} {}",
                if empty { 0 } else { stack.count },
                if empty { "minecraft:air" } else { &stack.item }
            )),
            ("hashCode", "I") => Value::Int(crate::stable_hash(&stack.item)),
            ("equals", "Z") => {
                let other = self.client_stack_slot(args.first()?);
                flag(other.as_ref() == Some(slot))
            }
            _ => return None,
        })
    }

    fn inventory_member(
        &mut self,
        frame: &ClientSnapshot,
        name: &str,
        returns: &str,
        args: &[Value],
    ) -> Option<Value> {
        let p = frame.player.as_ref()?;
        Some(match (name, returns) {
            ("getContainerSize", "I") => Value::Int(41),
            ("getItem", _) => StackSlot::of_container_index(int_arg(args, 0)?).marker(),
            ("getSelectedItem", _) => StackSlot::MainHand.marker(),
            ("getSelectedSlot", "I") => Value::Int(p.selected as i32),
            ("getNonEquipmentItems", _) => {
                let stacks: Vec<Value> =
                    (0..36).map(|i| StackSlot::Inventory(i).marker()).collect();
                collection_value("list", &stacks)
            }
            ("getFreeSlot", "I") => Value::Int(
                p.inventory
                    .iter()
                    .position(ClientStack::is_empty)
                    .map_or(-1, |i| i as i32),
            ),
            ("isEmpty", "Z") => flag(
                p.inventory.iter().all(ClientStack::is_empty)
                    && p.armor.iter().all(ClientStack::is_empty)
                    && p.offhand.is_empty(),
            ),
            ("countItem", "I") => {
                let id = item_like(args.first()?)?;
                Value::Int(
                    p.inventory
                        .iter()
                        .chain(&p.armor)
                        .chain(std::iter::once(&p.offhand))
                        .filter(|s| !s.is_empty() && s.item == id)
                        .map(|s| s.count)
                        .sum(),
                )
            }
            ("contains", "Z") => {
                let slot = self.client_stack_slot(args.first()?)?;
                let wanted = self.client_stack(frame, &slot);
                flag(
                    !wanted.is_empty()
                        && p.inventory
                            .iter()
                            .chain(&p.armor)
                            .chain(std::iter::once(&p.offhand))
                            .any(|s| !s.is_empty() && s.item == wanted.item),
                )
            }
            _ => return None,
        })
    }
}

fn item_member(
    frame: &ClientSnapshot,
    id: &str,
    block: bool,
    name: &str,
    returns: &str,
    args: &[Value],
) -> Option<Value> {
    let facts = frame.items.get(id);
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let item = marker(format!("{ITEM}{id}"));
    Some(match (name, returns) {
        // Item.toString: its registered name.
        ("toString", _) => marker(id.to_owned()),
        ("asItem", _) => item,
        ("getDescriptionId", _) => marker(format!(
            "{}.{namespace}.{path}",
            if block || facts.is_some_and(|f| f.block) {
                "block"
            } else {
                "item"
            }
        )),
        ("getName", _) if block => {
            component_value("translatable", &format!("block.{namespace}.{path}"))
        }
        ("getName" | "getDescription", _) => match facts {
            Some(f) if !f.name.is_empty() => component_value("literal", &f.name),
            _ => component_value("translatable", &format!("item.{namespace}.{path}")),
        },
        ("getDefaultMaxStackSize", "I") => Value::Int(facts.map_or(64, |f| f.max_stack)),
        ("hashCode", "I") => Value::Int(crate::stable_hash(id)),
        ("equals", "Z") => flag(args.first().and_then(item_like) == Some(id)),
        _ => return None,
    })
}

fn effect_instance_member(
    effect: &ClientEffect,
    name: &str,
    returns: &str,
    args: &[Value],
) -> Option<Value> {
    Some(match (name, returns) {
        ("getEffect", _) => marker(format!("{EFFECT_HOLDER}{}", effect.id)),
        ("getDuration", "I") => Value::Int(effect.duration),
        ("getAmplifier", "I") => Value::Int(effect.amplifier),
        ("isAmbient", "Z") => flag(effect.ambient),
        ("isVisible", "Z") => flag(effect.visible),
        ("showIcon", "Z") => flag(effect.show_icon),
        ("isInfiniteDuration", "Z") => flag(effect.duration == -1),
        // MobEffectInstance.endsWithin: not infinite and at most that long.
        ("endsWithin", "Z") => flag(effect.duration != -1 && effect.duration <= int_arg(args, 0)?),
        ("is", "Z") => {
            let holder = crate::expect_ref(args.first()?)?.strip_prefix(EFFECT_HOLDER)?;
            flag(holder == effect.id)
        }
        ("getDescriptionId", _) => marker(format!("effect.{}", effect.id.replacen(':', ".", 1))),
        _ => return None,
    })
}

/// Item facts keyed by id, as the game fills [`ClientSnapshot::items`].
pub type ItemTable = BTreeMap<String, ClientItem>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_colour_follows_item_get_bar_color() {
        // Item.getBarColor at full health: hsvToRgb(1/3, 1, 1) is pure green;
        // at no health it is red.
        assert_eq!(hsv_full(1.0 / 3.0), 0x00ff00);
        assert_eq!(hsv_full(0.0), 0xff0000);
    }

    #[test]
    fn container_indexes_map_like_inventory_get_item() {
        assert_eq!(StackSlot::of_container_index(8), StackSlot::Inventory(8));
        assert_eq!(StackSlot::of_container_index(36), StackSlot::Armor(0));
        assert_eq!(StackSlot::of_container_index(39), StackSlot::Armor(3));
        assert_eq!(StackSlot::of_container_index(40), StackSlot::OffHand);
        assert_eq!(StackSlot::of_container_index(41), StackSlot::Empty);
    }
}
