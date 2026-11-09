//! `ClientPacketListener.handleUpdateAttributes` for the attributes a nameplate
//! uses, split from the rest of the remote listener so `remote.rs` stays a
//! module, not a catalog.
use super::snapshot_value;
use crate::game::Game;
use horizon_protocol::packets::play;

impl Game<'_> {
    /// `ClientPacketListener.handleUpdateAttributes` for a mob, each value from
    /// `AttributeInstance.calculateValue`: its maximum health (the hearts drawn
    /// over its mount), and its `minecraft:scale`, which sizes its box
    /// (`LivingEntity.getDimensions`) and model (`LivingEntityRenderState.scale`)
    /// as the local player's does: NPC plugins make their fake players bigger
    /// with it, datapacks scale any mob.
    pub(super) fn remote_mob_attributes(&mut self, id: i32, values: &[play::AttributeSnapshot]) {
        let catalog = &self.motion.attributes;
        let value = |name: &str| {
            let attribute = catalog.id(name)?;
            let s = values
                .iter()
                .find(|s| s.attribute == attribute.registry_id())?;
            Some(
                catalog
                    .attribute(attribute)
                    .definition
                    .sanitize(snapshot_value(s)),
            )
        };
        let (max_health, scale) = (value("minecraft:max_health"), value("minecraft:scale"));
        let (speed, jump) = (
            value("minecraft:movement_speed"),
            value("minecraft:jump_strength"),
        );
        let name_tag = value("minecraft:name_tag_distance");
        let below_name = value("minecraft:below_name_distance");
        let Some(m) = self.mob_index.get_mut(&mut self.mobs, id as u32) else {
            if let Some(a) = self.armor_stands.iter_mut().find(|a| a.id == id as u32) {
                if let Some(name_tag) = name_tag {
                    a.nameplate.name_tag = Some(name_tag);
                }
                if let Some(below_name) = below_name {
                    a.nameplate.below_name = Some(below_name);
                }
            }
            return;
        };
        // A horse its rider's client moves goes at its own speed and jumps
        // its own height (`getSpeed`, `getCustomJump`).
        if horizon_client::mobs::equine::moved_by_rider(m.species()) {
            if let Some(speed) = speed {
                horizon_client::mobs::finalize::set_base(
                    m,
                    horizon_client::mobs::finalize::Attr::MovementSpeed,
                    speed,
                );
            }
            if let Some(jump) = jump {
                m.extra.vector[0] = jump;
            }
        }
        if let Some(max_health) = max_health {
            horizon_client::mobs::finalize::set_base(
                m,
                horizon_client::mobs::finalize::Attr::MaxHealth,
                max_health,
            );
        }
        if let Some(scale) = scale {
            m.extra.model_scale = scale as f32;
        }
        if let Some(name_tag) = name_tag {
            m.extra.nameplate.name_tag = Some(name_tag);
        }
        if let Some(below_name) = below_name {
            m.extra.nameplate.below_name = Some(below_name);
        }
    }
}
