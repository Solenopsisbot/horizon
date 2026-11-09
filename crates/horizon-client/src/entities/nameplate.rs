//! The per-entity nameplate distances (`NAME_TAG_DISTANCE` and
//! `BELOW_NAME_DISTANCE`) armour stands carry.
use super::ArmorStand;
use crate::items::ItemStack;

/// The two distances `LivingEntity` carries (None: the vanilla default).
#[derive(Clone, Debug, Default)]
pub struct NameplateDistances {
    /// `minecraft:name_tag_distance` from the server's attribute updates.
    pub name_tag: Option<f64>,
    /// `minecraft:below_name_distance`.
    pub below_name: Option<f64>,
}

impl ArmorStand {
    /// `NameTagItem.interactLivingEntity`: a custom name handles the use;
    /// only a living target changes. Stack consumption belongs to the caller.
    pub fn name_with_tag(&mut self, stack: &ItemStack) -> bool {
        if &*stack.item != "minecraft:name_tag" {
            return false;
        }
        let Some(name) = stack.data().and_then(|data| data.custom_name.as_ref()) else {
            return false;
        };
        if self.health > 0.0 {
            self.custom_name = Some(crate::component::name_text(name));
        }
        true
    }

    /// `LivingEntity.getAttributeValue(Attributes.NAME_TAG_DISTANCE)`.
    pub fn name_tag_distance(&self) -> f64 {
        self.nameplate
            .name_tag
            .filter(|v| v.is_finite())
            .unwrap_or(64.0)
            .clamp(0.0, 512.0)
    }
    /// `LivingEntity.getAttributeValue(Attributes.BELOW_NAME_DISTANCE)`.
    pub fn below_name_distance(&self) -> f64 {
        self.nameplate
            .below_name
            .filter(|v| v.is_finite())
            .unwrap_or(10.0)
            .clamp(0.0, 512.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_core::geometry::Vec3;

    #[test]
    fn a_stand_uses_its_name_tag_distances_with_vanilla_defaults() {
        let mut stand = ArmorStand::new(1, Vec3::new(0.5, 64.0, 0.5), 0.0);
        assert_eq!(stand.name_tag_distance(), 64.0);
        assert_eq!(stand.below_name_distance(), 10.0);
        stand.nameplate.name_tag = Some(5.0);
        stand.nameplate.below_name = Some(2.0);
        assert_eq!(
            (stand.name_tag_distance(), stand.below_name_distance()),
            (5.0, 2.0)
        );
        stand.nameplate.name_tag = Some(600.0);
        assert_eq!(stand.name_tag_distance(), 512.0);
    }
}
