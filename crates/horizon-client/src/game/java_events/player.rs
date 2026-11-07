//! The player and entity events of the Java listeners, fired from the
//! server's own points: a player using or attacking a block or an entity,
//! breaking a block, an entity being hurt, a player respawning, being copied
//! or changing level, a level loading or unloading.
//!
//! The result-returning ones answer whether vanilla's handling stops: a
//! Fabric `InteractionResult` callback that answers anything but `PASS`, a
//! Fabric `boolean` callback that answers `false`, or a NeoForge/Forge
//! listener that cancels. The site then does what the loader's own hook does
//! at that point (Fabric's `ServerPlayerGameModeMixin`,
//! `ServerGamePacketListenerImplMixin`, NeoForge's `CommonHooks`): the use,
//! dig, attack or break does not happen, and a `SUCCESS_SERVER` answer swings
//! the hand (`ServerGamePacketListenerImpl.handleUseItemOn` swings for a
//! result whose swing source is the server).
//!
//! Each site is a one-line call; every helper first asks [`listens`], one
//! relaxed atomic load, so a server without such a listener does nothing
//! else. The player an event is about is the acting one (`as_guest` swaps
//! their state in), named to the façade as a subject because the hub has
//! them out for their turn.

use horizon_native_java::events::{
    EventContext, EventFacade, EventFiring, FacadeEntity, FacadePlayer, GameEvent,
};
use std::sync::{Arc, atomic::Ordering};

use super::{Fired, SERVER_TICKS, fire, listens};
use crate::game::{Game, GuestUse, Hand};

/// Fabric's `InteractionResult.SUCCESS_SERVER`: the server swings the hand.
fn swings(fired: &Fired) -> bool {
    fired.interaction.as_deref() == Some("SUCCESS_SERVER")
}

impl Game<'_> {
    /// The acting player as the façade shows them: the one whose packet the
    /// server is handling (`as_guest`).
    fn java_acting_subject(&self) -> Option<FacadePlayer> {
        Some(FacadePlayer {
            uuid: self.lan_acting_uuid?,
            name: self.lan_acting_name.clone()?,
            level: self.dim.id().to_owned(),
            entity_id: self.lan_acting_entity.map_or(0, |id| id as i32),
        })
    }

    /// A player the hub lists, or the acting one, by uuid.
    fn java_player_subject(&self, player: u128) -> Option<FacadePlayer> {
        if let Some(acting) = self.java_acting_subject().filter(|p| p.uuid == player) {
            return Some(acting);
        }
        let guest = self
            .lan
            .as_ref()?
            .hub
            .guests
            .iter()
            .find(|g| g.uuid == player)?;
        Some(FacadePlayer {
            uuid: guest.uuid,
            name: guest.name.clone(),
            level: guest.dimension.clone(),
            entity_id: guest.entity_id,
        })
    }

    /// Fires `event` about the acting player, with the block, face and hand
    /// of `context` (`level` and `player` are filled in). Nothing fires
    /// without an acting player.
    fn java_acting_event(&mut self, event: GameEvent, context: EventContext) -> Fired {
        if !listens(event) {
            return Fired::default();
        }
        let Some(subject) = self.java_acting_subject() else {
            return Fired::default();
        };
        let context = EventContext {
            level: Some(subject.level.clone()),
            player: Some(subject.uuid),
            off_hand: self.hand == Hand::Off,
            ..context
        };
        self.java_event_about(event, context, Some(subject))
    }

    /// Fabric `UseBlockCallback`, NeoForge `PlayerInteractEvent.RightClickBlock`:
    /// where `ServerPlayerGameMode.useItemOn` starts, before the block's own
    /// use and the item's. True when a listener decided the click: neither
    /// the block nor the item acts.
    pub(in crate::game) fn java_use_block(&mut self, pos: [i32; 3], face: [i32; 3]) -> bool {
        let fired = self.java_acting_event(
            GameEvent::UseBlock,
            EventContext {
                pos: Some(pos),
                face: Some(face),
                ..EventContext::default()
            },
        );
        self.java_decided_use(&fired)
    }

    /// Fabric `UseItemCallback`, NeoForge `PlayerInteractEvent.RightClickItem`:
    /// where `ServerPlayerGameMode.useItem` starts (a use in the air, not a
    /// use on a block, which only reaches `useOn`). True when a listener
    /// decided the use: the item's own use does not run.
    pub(in crate::game) fn java_use_item(&mut self) -> bool {
        if matches!(self.guest_use, Some(GuestUse::OnBlock(_))) {
            return false;
        }
        let fired = self.java_acting_event(GameEvent::UseItem, EventContext::default());
        self.java_decided_use(&fired)
    }

    /// Fabric `UseEntityCallback`, NeoForge `PlayerInteractEvent.EntityInteract`:
    /// where `Player.interactOn` starts for `entity`. True when a listener
    /// decided the click: neither the entity nor the item acts.
    pub(in crate::game) fn java_use_entity(&mut self, entity: i32, kind: &str) -> bool {
        let fired =
            self.java_acting_event(GameEvent::UseEntity, self.java_entity_context(entity, kind));
        self.java_decided_use(&fired)
    }

    /// Fabric `AttackEntityCallback`, NeoForge `AttackEntityEvent`: where
    /// `Player.attack` starts for `entity`. True when a listener decided the
    /// attack: it does not happen.
    pub(in crate::game) fn java_attack_entity(&mut self, entity: i32, kind: &str) -> bool {
        self.java_acting_event(
            GameEvent::AttackEntity,
            self.java_entity_context(entity, kind),
        )
        .cancelled
    }

    /// Fabric `AttackBlockCallback`, NeoForge `PlayerInteractEvent.LeftClickBlock`:
    /// where `ServerPlayerGameMode.handleBlockBreakAction` takes a dig's
    /// start. True when a listener decided the click: the block is not
    /// attacked, dug or broken, and the player's client gets it back.
    pub(in crate::game) fn java_attack_block(&mut self, pos: [i32; 3], face: [i32; 3]) -> bool {
        self.java_acting_event(
            GameEvent::AttackBlock,
            EventContext {
                pos: Some(pos),
                face: Some(face),
                ..EventContext::default()
            },
        )
        .cancelled
    }

    /// Fabric `PlayerBlockBreakEvents.BEFORE`, NeoForge `BlockEvent.BreakEvent`:
    /// where `ServerPlayerGameMode.destroyBlock` is about to break the block
    /// at `pos`. True when a listener refused the break; Fabric's `CANCELED`
    /// listeners then hear of it, as its hook tells them.
    pub(in crate::game) fn java_break_block(&mut self, pos: [i32; 3]) -> bool {
        if !listens(GameEvent::BlockBreakBefore) {
            return false;
        }
        let context = EventContext {
            pos: Some(pos),
            ..EventContext::default()
        };
        let canceled = self
            .java_acting_event(GameEvent::BlockBreakBefore, context.clone())
            .cancelled;
        if canceled {
            self.java_acting_event(GameEvent::BlockBreakCanceled, context);
        }
        canceled
    }

    /// Fabric `PlayerBlockBreakEvents.AFTER`: the block at `pos` was broken
    /// by the acting player (`destroyBlock` removed it).
    pub(in crate::game) fn java_broke_block(&mut self, pos: [i32; 3]) {
        self.java_acting_event(
            GameEvent::BlockBreakAfter,
            EventContext {
                pos: Some(pos),
                ..EventContext::default()
            },
        );
    }

    /// Fabric `ServerPlayerEvents.AFTER_RESPAWN`, NeoForge `PlayerRespawnEvent`:
    /// `player` respawned after a death (`alive` false) or came back from the
    /// End (`alive` true), placed in their new level.
    pub(in crate::game) fn java_player_respawned(&mut self, player: u128, alive: bool) {
        self.java_player_pair(GameEvent::PlayerRespawn, player, alive);
    }

    /// Fabric `ServerPlayerEvents.COPY_FROM`, NeoForge `PlayerEvent.Clone`:
    /// what `player` keeps is copied to their new player (`alive` true for
    /// the End's exit, false after a death).
    pub(in crate::game) fn java_player_copied(&mut self, player: u128, alive: bool) {
        self.java_player_pair(GameEvent::PlayerCopy, player, alive);
    }

    fn java_player_pair(&mut self, event: GameEvent, player: u128, alive: bool) {
        if !listens(event) {
            return;
        }
        let Some(subject) = self.java_player_subject(player) else {
            return;
        };
        let context = EventContext {
            level: Some(subject.level.clone()),
            player: Some(player),
            flag: alive,
            ..EventContext::default()
        };
        self.java_event_about(event, context, Some(subject));
    }

    /// Fabric `ServerEntityLevelChangeEvents.AFTER_PLAYER_CHANGE_LEVEL`,
    /// NeoForge `PlayerChangedDimensionEvent`: `player` ended a move from the
    /// level `from` to `to`.
    pub(in crate::game) fn java_player_changed_level(
        &mut self,
        player: u128,
        from: &str,
        to: &str,
    ) {
        if !listens(GameEvent::PlayerChangeLevel) {
            return;
        }
        let Some(mut subject) = self.java_player_subject(player) else {
            return;
        };
        subject.level = to.to_owned();
        let context = EventContext {
            level: Some(to.to_owned()),
            origin: Some(from.to_owned()),
            player: Some(player),
            ..EventContext::default()
        };
        self.java_event_about(GameEvent::PlayerChangeLevel, context, Some(subject));
    }

    /// Fabric `ServerLevelEvents.LOAD`/`UNLOAD`, NeoForge `LevelEvent.Load`/
    /// `Unload`: `dimension`'s level opened or closed on the server.
    pub(in crate::game) fn java_level_loaded(&mut self, dimension: &str, load: bool) {
        let event = if load {
            GameEvent::LevelLoad
        } else {
            GameEvent::LevelUnload
        };
        if !listens(event) {
            return;
        }
        let context = EventContext {
            level: Some(dimension.to_owned()),
            ..EventContext::default()
        };
        self.java_event_about(event, context, None);
    }

    /// Fabric `ServerEntityEvents.ENTITY_LOAD`, NeoForge `EntityJoinLevelEvent`:
    /// `entity` of type `kind` joined `dimension`'s level. True when a
    /// NeoForge listener cancelled the join: the entity is taken back out.
    pub(in crate::game) fn java_entity_loaded(
        &mut self,
        dimension: &str,
        entity: u32,
        kind: &str,
    ) -> bool {
        self.java_entity_event(GameEvent::EntityLoad, dimension, entity, kind)
            .cancelled
    }

    /// Fabric `ServerEntityEvents.ENTITY_UNLOAD`, NeoForge
    /// `EntityLeaveLevelEvent`: `entity` left `dimension`'s level.
    pub(in crate::game) fn java_entity_unloaded(
        &mut self,
        dimension: &str,
        entity: u32,
        kind: &str,
    ) {
        self.java_entity_event(GameEvent::EntityUnload, dimension, entity, kind);
    }

    fn java_entity_event(
        &mut self,
        event: GameEvent,
        dimension: &str,
        entity: u32,
        kind: &str,
    ) -> Fired {
        if !listens(event) {
            return Fired::default();
        }
        let context = EventContext {
            level: Some(dimension.to_owned()),
            entity: Some(FacadeEntity {
                id: entity as i32,
                kind: kind.to_owned(),
                level: dimension.to_owned(),
            }),
            ..EventContext::default()
        };
        self.java_event_about(event, context, None)
    }

    /// The entity a use or attack is about: a player the hub lists is a
    /// player to the façade, anything else an entity of `kind`.
    fn java_entity_context(&self, entity: i32, kind: &str) -> EventContext {
        let level = self.dim.id().to_owned();
        EventContext {
            entity: Some(FacadeEntity {
                id: entity,
                kind: kind.to_owned(),
                level,
            }),
            ..EventContext::default()
        }
    }

    /// What a decided use does besides stopping: a `SUCCESS_SERVER` swings
    /// the hand, a refusal counts as a failed use.
    fn java_decided_use(&mut self, fired: &Fired) -> bool {
        if !fired.cancelled {
            return false;
        }
        if swings(fired) {
            self.swing_arm();
        } else {
            self.use_failed = true;
        }
        true
    }
}

/// Fabric `ServerLivingEntityEvents.ALLOW_DAMAGE`, NeoForge
/// `LivingIncomingDamageEvent`: `LivingEntity.hurtServer` is about to hurt
/// `entity` (of type `kind`) by `amount` of `damage_type`. Called from the
/// damage code, which runs without a `Game`. Only a server role keeps
/// listeners and a process runs one server, so the role is the one whose
/// jars listen (the integrated server's if both somehow did). True when a
/// listener refused the hit.
pub(in crate::game) fn fire_allow_damage(
    entity: i32,
    kind: &str,
    damage_type: &str,
    amount: f32,
) -> bool {
    if !listens(GameEvent::LivingAllowDamage) {
        return false;
    }
    let Some(role) = super::listening_role(GameEvent::LivingAllowDamage) else {
        return false;
    };
    // No `Game` here: the façade has the tick count and no players.
    let facade = Arc::new(EventFacade {
        tick_count: SERVER_TICKS.load(Ordering::Relaxed),
        ..EventFacade::default()
    });
    let context = EventContext {
        entity: Some(FacadeEntity {
            id: entity,
            kind: kind.to_owned(),
            level: String::new(),
        }),
        damage: Some(damage_type.to_owned()),
        amount,
        ..EventContext::default()
    };
    fire(
        role,
        EventFiring::new(GameEvent::LivingAllowDamage, facade, context),
    )
    .cancelled
}
