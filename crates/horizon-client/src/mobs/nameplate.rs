//! The per-entity nameplate distances (`NAME_TAG_DISTANCE` and
//! `BELOW_NAME_DISTANCE`), shared by mobs, players and armour stands.
use super::Mob;

/// The two distances `LivingEntity` carries (None: the vanilla default).
#[derive(Clone, Debug, Default)]
pub struct NameplateDistances {
    /// `minecraft:name_tag_distance` from the server's attribute updates.
    pub name_tag: Option<f64>,
    /// `minecraft:below_name_distance`.
    pub below_name: Option<f64>,
}

impl Mob {
    /// `LivingEntity.getAttributeValue(Attributes.NAME_TAG_DISTANCE)`: the
    /// distance a name tag is drawn (LivingEntity's default 64, clamp 0..512).
    pub fn name_tag_distance(&self) -> f64 {
        name_tag_distance_of(self.extra.nameplate.name_tag)
    }
    /// `LivingEntity.getAttributeValue(Attributes.BELOW_NAME_DISTANCE)`: the
    /// distance a below-name scoreboard line is drawn (default 10).
    pub fn below_name_distance(&self) -> f64 {
        below_name_distance_of(self.extra.nameplate.below_name)
    }
}

/// `Attributes.NAME_TAG_DISTANCE`: the default (64) and the 512 maximum.
pub(super) fn name_tag_distance_of(value: Option<f64>) -> f64 {
    value
        .filter(|v| v.is_finite())
        .unwrap_or(64.0)
        .clamp(0.0, 512.0)
}
/// `Attributes.BELOW_NAME_DISTANCE`: the default (10) and the 512 maximum.
pub(super) fn below_name_distance_of(value: Option<f64>) -> f64 {
    value
        .filter(|v| v.is_finite())
        .unwrap_or(10.0)
        .clamp(0.0, 512.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_tag_distances_use_vanilla_defaults_and_clamp() {
        assert_eq!(name_tag_distance_of(None), 64.0);
        assert_eq!(name_tag_distance_of(Some(5.0)), 5.0);
        assert_eq!(name_tag_distance_of(Some(600.0)), 512.0);
        assert_eq!(name_tag_distance_of(Some(-4.0)), 0.0);
        assert_eq!(below_name_distance_of(None), 10.0);
        assert_eq!(below_name_distance_of(Some(2.0)), 2.0);
        assert_eq!(below_name_distance_of(Some(-1.0)), 0.0);
    }
}
