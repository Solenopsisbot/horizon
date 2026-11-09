//! `DeathProtection.TOTEM_EFFECTS`: the effects a totem runs when it saves its holder.
/// `DeathProtection.TOTEM_EFFECTS`: clear, then regeneration II, absorption II
/// and fire resistance.
pub(super) fn totem_death_effects() -> Vec<crate::items::ConsumeEffect> {
    use crate::items::{ConsumeEffect, ConsumeEffectSpec};
    let spec = |id: &str, duration, amplifier| ConsumeEffectSpec {
        id: id.to_owned(),
        duration,
        amplifier,
        ambient: false,
        show_particles: true,
        show_icon: true,
    };
    vec![
        ConsumeEffect::ClearAllEffects,
        ConsumeEffect::ApplyEffects {
            effects: vec![
                spec("minecraft:regeneration", 900, 1),
                spec("minecraft:absorption", 100, 1),
                spec("minecraft:fire_resistance", 800, 0),
            ],
            probability: 1.0,
        },
    ]
}
