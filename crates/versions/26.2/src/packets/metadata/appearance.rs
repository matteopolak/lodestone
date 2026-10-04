//! The appearance table: which metadata accessor feeds which
//! [`MobAppearance`] field, per concrete entity class.
//!
//! Every row names its accessor as `Owner.FIELD`, the spelling of the committed
//! jar dump (`tests/support/entity_data_index_jvm.txt`), and
//! `every_appearance_row_matches_the_jar_dump` checks the row's index and
//! serializer against that dump. The numbers are therefore never hand-counted: a
//! row that drifts from the game's own class hierarchy fails that test.
//!
//! The class guard is not optional. Indices 16 through 23 are reused by
//! unrelated classes with the same serializer (index 18 alone is a sheep's wool
//! byte, a goat's screaming flag, a mooshroom's type and a fox's coat), so a row
//! fires only for the [`MetadataClass`] it names.

use super::{
    IDX_BABY, IDX_PIGLIN_BABY, MetadataClass, SER_BOOLEAN, SER_BYTE, SER_INT, SER_LONG, Value,
};
use lodestone_model::MobAppearance;

/// The serializer ids of the enum-ordinal accessors this table reads. Each is a
/// `VarInt` ordinal on the wire. (The sniffer's state, 35, and the copper golem's
/// action state, 37, are ordinals too and carry no appearance.)
pub(super) const SER_ARMADILLO_STATE: i32 = 36;
pub(super) const SER_WEATHERING_COPPER_STATE: i32 = 38;

/// One accessor-to-field mapping.
pub(super) struct Row {
    /// The concrete classes this accessor belongs to.
    pub classes: &'static [MetadataClass],
    /// The metadata index.
    pub index: u8,
    /// The accessor, as the jar dump spells it. Read by the dump-anchor test only.
    #[cfg(test)]
    pub accessor: &'static str,
    /// The serializer the accessor is registered with.
    pub serializer: i32,
    /// Stores the decoded scalar into its field.
    pub raise: fn(&mut MobAppearance, i64),
}

use MetadataClass as C;

/// The rows. Order is irrelevant: `(class, index, serializer)` is unique.
pub(super) const ROWS: &[Row] = &[
    Row { classes: &[C::Rabbit], index: 18, #[cfg(test)] accessor: "Rabbit.DATA_TYPE_ID", serializer: SER_INT,
        raise: |a, v| a.rabbit_type = Some(v as i32) },
    Row { classes: &[C::Parrot], index: 20, #[cfg(test)] accessor: "Parrot.DATA_VARIANT_ID", serializer: SER_INT,
        raise: |a, v| a.parrot_variant = Some(v as i32) },
    Row { classes: &[C::Horse], index: 21, #[cfg(test)] accessor: "Llama.DATA_VARIANT_ID", serializer: SER_INT,
        raise: |a, v| a.llama_variant = Some(v as i32) },
    Row { classes: &[C::Horse], index: 19, #[cfg(test)] accessor: "AbstractChestedHorse.DATA_ID_CHEST", serializer: SER_BOOLEAN,
        raise: |a, v| a.chested = Some(v != 0) },
    Row { classes: &[C::Panda], index: 21, #[cfg(test)] accessor: "Panda.MAIN_GENE_ID", serializer: SER_BYTE,
        raise: |a, v| a.panda_main_gene = Some(v as u8) },
    Row { classes: &[C::Panda], index: 22, #[cfg(test)] accessor: "Panda.HIDDEN_GENE_ID", serializer: SER_BYTE,
        raise: |a, v| a.panda_hidden_gene = Some(v as u8) },
    Row { classes: &[C::TropicalFish], index: 17, #[cfg(test)] accessor: "TropicalFish.DATA_ID_TYPE_VARIANT", serializer: SER_INT,
        raise: |a, v| a.tropical_fish_variant = Some(v as i32) },
    Row { classes: &[C::Salmon], index: 17, #[cfg(test)] accessor: "Salmon.DATA_TYPE", serializer: SER_INT,
        raise: |a, v| a.salmon_variant = Some(v as i32) },
    Row { classes: &[C::Pufferfish], index: 17, #[cfg(test)] accessor: "Pufferfish.PUFF_STATE", serializer: SER_INT,
        raise: |a, v| a.puff_state = Some(v as i32) },
    Row { classes: &[C::Mooshroom], index: 18, #[cfg(test)] accessor: "MushroomCow.DATA_TYPE", serializer: SER_INT,
        raise: |a, v| a.mooshroom_type = Some(v as i32) },
    Row { classes: &[C::Shulker], index: 18, #[cfg(test)] accessor: "Shulker.DATA_COLOR_ID", serializer: SER_BYTE,
        raise: |a, v| a.shulker_color = Some(v as u8) },
    Row { classes: &[C::Goat], index: 18, #[cfg(test)] accessor: "Goat.DATA_IS_SCREAMING_GOAT", serializer: SER_BOOLEAN,
        raise: |a, v| a.goat_screaming = Some(v != 0) },
    Row { classes: &[C::Goat], index: 19, #[cfg(test)] accessor: "Goat.DATA_HAS_LEFT_HORN", serializer: SER_BOOLEAN,
        raise: |a, v| a.goat_left_horn = Some(v != 0) },
    Row { classes: &[C::Goat], index: 20, #[cfg(test)] accessor: "Goat.DATA_HAS_RIGHT_HORN", serializer: SER_BOOLEAN,
        raise: |a, v| a.goat_right_horn = Some(v != 0) },
    Row { classes: &[C::Bee], index: 18, #[cfg(test)] accessor: "Bee.DATA_FLAGS_ID", serializer: SER_BYTE,
        raise: |a, v| a.bee_flags = Some(v as u8) },
    Row { classes: &[C::Bee], index: 19, #[cfg(test)] accessor: "Bee.DATA_ANGER_END_TIME", serializer: SER_LONG,
        raise: |a, v| a.bee_anger_end_time = Some(v) },
    Row { classes: &[C::SnowGolem], index: 16, #[cfg(test)] accessor: "SnowGolem.DATA_PUMPKIN_ID", serializer: SER_BYTE,
        raise: |a, v| a.snow_golem_flags = Some(v as u8) },
    Row { classes: &[C::Enderman], index: 17, #[cfg(test)] accessor: "EnderMan.DATA_CREEPY", serializer: SER_BOOLEAN,
        raise: |a, v| a.enderman_creepy = Some(v != 0) },
    Row { classes: &[C::Ghast], index: 16, #[cfg(test)] accessor: "Ghast.DATA_IS_CHARGING", serializer: SER_BOOLEAN,
        raise: |a, v| a.ghast_charging = Some(v != 0) },
    Row { classes: &[C::Vex], index: 16, #[cfg(test)] accessor: "Vex.DATA_FLAGS_ID", serializer: SER_BYTE,
        raise: |a, v| a.vex_flags = Some(v as u8) },
    Row { classes: &[C::Cube], index: 18, #[cfg(test)] accessor: "AbstractCubeMob.ID_SIZE", serializer: SER_INT,
        raise: |a, v| a.cube_size = Some(v as i32) },
    Row { classes: &[C::Phantom], index: 16, #[cfg(test)] accessor: "Phantom.ID_SIZE", serializer: SER_INT,
        raise: |a, v| a.phantom_size = Some(v as i32) },
    Row { classes: &[C::Strider], index: 19, #[cfg(test)] accessor: "Strider.DATA_SUFFOCATING", serializer: SER_BOOLEAN,
        raise: |a, v| a.strider_suffocating = Some(v != 0) },
    Row { classes: &[C::Fox], index: 19, #[cfg(test)] accessor: "Fox.DATA_FLAGS_ID", serializer: SER_BYTE,
        raise: |a, v| a.fox_flags = Some(v as u8) },
    Row { classes: &[C::Cat], index: 21, #[cfg(test)] accessor: "Cat.IS_LYING", serializer: SER_BOOLEAN,
        raise: |a, v| a.cat_lying = Some(v != 0) },
    Row { classes: &[C::Wolf], index: 22, #[cfg(test)] accessor: "Wolf.DATA_ANGER_END_TIME", serializer: SER_LONG,
        raise: |a, v| a.wolf_anger_end_time = Some(v) },
    Row { classes: &[C::Armadillo], index: 18, #[cfg(test)] accessor: "Armadillo.ARMADILLO_STATE", serializer: SER_ARMADILLO_STATE,
        raise: |a, v| a.armadillo_state = Some(v as u8) },
    Row { classes: &[C::CopperGolem], index: 16, #[cfg(test)] accessor: "CopperGolem.DATA_WEATHER_STATE", serializer: SER_WEATHERING_COPPER_STATE,
        raise: |a, v| a.copper_golem_weather = Some(v as u8) },
    Row { classes: &[C::WitherBoss], index: 19, #[cfg(test)] accessor: "WitherBoss.DATA_ID_INV", serializer: SER_INT,
        raise: |a, v| a.wither_invulnerable_ticks = Some(v as i32) },
    Row { classes: &[C::WitherSkull], index: 8, #[cfg(test)] accessor: "WitherSkull.DATA_DANGEROUS", serializer: SER_BOOLEAN,
        raise: |a, v| a.wither_skull_dangerous = Some(v != 0) },
    Row { classes: &[C::Bogged], index: 16, #[cfg(test)] accessor: "Bogged.DATA_SHEARED", serializer: SER_BOOLEAN,
        raise: |a, v| a.bogged_sheared = Some(v != 0) },
    Row { classes: &[C::Turtle], index: 18, #[cfg(test)] accessor: "Turtle.HAS_EGG", serializer: SER_BOOLEAN,
        raise: |a, v| a.turtle_has_egg = Some(v != 0) },
    Row { classes: &[C::Creaking], index: 17, #[cfg(test)] accessor: "Creaking.IS_ACTIVE", serializer: SER_BOOLEAN,
        raise: |a, v| a.creaking_active = Some(v != 0) },
    Row { classes: &[C::Arrow], index: 11, #[cfg(test)] accessor: "Arrow.ID_EFFECT_COLOR", serializer: SER_INT,
        raise: |a, v| a.arrow_effect_color = Some(v as i32) },
];

/// The serializer a decoded value was read with, for the shapes this table can
/// consume. `None` for a shape no row reads.
fn value_serializer(value: &Value) -> Option<(i32, i64)> {
    Some(match *value {
        Value::Byte(b) => (SER_BYTE, i64::from(b as u8)),
        Value::Int(v) => (SER_INT, i64::from(v)),
        Value::Bool(b) => (SER_BOOLEAN, i64::from(b)),
        Value::Long(v) => (SER_LONG, v),
        Value::Ordinal(serializer, v) => (serializer, i64::from(v)),
        _ => return None,
    })
}

/// Raises the appearance field a `(class, index, value)` triple names. Returns
/// whether a row consumed it; the caller moves on to its remaining arms when not.
pub(super) fn raise(
    class: Option<MetadataClass>,
    index: u8,
    value: &Value,
    out: &mut MobAppearance,
) -> bool {
    let (Some(class), Some((serializer, scalar))) = (class, value_serializer(value)) else {
        return false;
    };
    for row in ROWS {
        if row.index == index && row.serializer == serializer && row.classes.contains(&class) {
            (row.raise)(out, scalar);
            return true;
        }
    }
    false
}

/// The index of the baby flag for a class, or `None` for a class that is not
/// ageable. The ageable base class declares it at 16, the zombie family and the
/// zoglin each declare their own at 16, and the piglin declares a second at 17
/// behind its own immunity flag, so neither "the bool at 16" nor "any mob" is a
/// safe reading: fish, ghasts, guardians, raiders and piglins all carry an
/// unrelated bool there.
pub(super) fn baby_index(class: Option<MetadataClass>) -> Option<u8> {
    match class? {
        C::Piglin => Some(IDX_PIGLIN_BABY),
        c if c.is_ageable() => Some(IDX_BABY),
        _ => None,
    }
}
