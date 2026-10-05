//! The version-free appearance inputs of a mob that are not a registry-holder
//! variant: ordinals, flag bytes and counters the renderer picks a texture, a
//! layer or a model part from.

/// Declares [`MobAppearance`] and its merge from one field list, so a field can
/// never be added to the struct and forgotten in [`MobAppearance::merge`].
macro_rules! appearance_fields {
    ($( $(#[$doc:meta])* $name:ident : $ty:ty ),* $(,)?) => {
        /// Appearance inputs decoded off an entity's metadata, one `Option` per
        /// field: `None` means "this packet did not mention it", which a consumer
        /// reads as the field's vanilla default (a field equal to its default is
        /// never put on the wire).
        ///
        /// The same shape serves as the per-packet update
        /// ([`EntityMetadataUpdate::appearance`](crate::EntityMetadataUpdate::appearance))
        /// and as the accumulated state; [`merge`](Self::merge) folds one into the other.
        /// Each field is raised only for the entity types that own it: the metadata
        /// indices these come from are reused across unrelated classes, so the
        /// version adapter gates every one on the concrete entity class.
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct MobAppearance {
            $( $(#[$doc])* pub $name: Option<$ty>, )*
        }

        impl MobAppearance {
            /// Overwrites every field `update` reports and leaves the rest alone.
            pub fn merge(&mut self, update: &Self) {
                $( if update.$name.is_some() { self.$name = update.$name; } )*
            }

            /// Whether no field is reported.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                true $( && self.$name.is_none() )*
            }
        }
    };
}

appearance_fields! {
    /// A rabbit's coat id: brown `0`, white `1`, black `2`, white splotched `3`,
    /// gold `4`, salt `5`, killer `99`.
    rabbit_type: i32,
    /// A parrot's plumage ordinal `0..=4` (red-blue, blue, green, yellow-blue, grey).
    parrot_variant: i32,
    /// A llama's or trader llama's coat ordinal `0..=3` (creamy, white, brown, grey).
    llama_variant: i32,
    /// Whether a donkey, mule or llama carries a chest.
    chested: bool,
    /// A panda's main gene ordinal (normal, aggressive, lazy, worried, playful,
    /// weak, brown).
    panda_main_gene: u8,
    /// A panda's hidden gene ordinal, same table as the main gene.
    panda_hidden_gene: u8,
    /// A tropical fish's packed variant: pattern, base colour and pattern colour.
    tropical_fish_variant: i32,
    /// A salmon's size ordinal (small, medium, large).
    salmon_variant: i32,
    /// A mooshroom's mushroom ordinal: red `0`, brown `1`.
    mooshroom_type: i32,
    /// A shulker's colour byte: a dye ordinal `0..=15`, or `16` for the undyed purple.
    shulker_color: u8,
    /// Whether a goat is the screaming variant.
    goat_screaming: bool,
    /// Whether a goat still has its left horn.
    goat_left_horn: bool,
    /// Whether a goat still has its right horn.
    goat_right_horn: bool,
    /// A bee's flag byte: has nectar `8`, has stung `4`, roll `2`.
    bee_flags: u8,
    /// The game time at which a bee's anger ends; angry while it is ahead of the clock.
    bee_anger_end_time: i64,
    /// A snow golem's flag byte; `0x10` is "wearing a pumpkin".
    snow_golem_flags: u8,
    /// Whether an enderman has its mouth open (screaming / provoked).
    enderman_creepy: bool,
    /// Whether a ghast is charging a fireball.
    ghast_charging: bool,
    /// A vex's flag byte; `0x01` is "charging".
    vex_flags: u8,
    /// The size of a slime, magma cube or sulfur cube.
    cube_size: i32,
    /// A phantom's size.
    phantom_size: i32,
    /// A pufferfish's puff state `0..=2`.
    puff_state: i32,
    /// Whether a strider is out of lava and shivering.
    strider_suffocating: bool,
    /// A fox's flag byte: sitting `1`, crouching `4`, interested `8`, pouncing
    /// `16`, sleeping `32`, face-planted `64`.
    fox_flags: u8,
    /// Whether a cat is lying down.
    cat_lying: bool,
    /// Whether a cat is in its first relaxed state (head dropped, beside its owner's bed).
    cat_relaxed: bool,
    /// The game time at which a wolf's anger ends; angry while it is ahead of the clock.
    wolf_anger_end_time: i64,
    /// An armadillo's state ordinal (idle, rolling, scared, unrolling).
    armadillo_state: u8,
    /// A bat's flag byte; bit `0x01` is hanging from a ceiling.
    bat_flags: u8,
    /// Whether a camel is mid-dash.
    camel_dash: bool,
    /// A camel's pose-change stamp: negative while sitting, and the magnitude is the game time of the last change.
    camel_last_pose_change_tick: i64,
    /// A sniffer's state ordinal (idling, feeling happy, scenting, sniffing, searching, digging, rising).
    sniffer_state: u8,
    /// A copper golem's weathering ordinal (unaffected, exposed, weathered, oxidized).
    copper_golem_weather: u8,
    /// A wither's invulnerable-ticks countdown; positive during the spawn charge-up.
    wither_invulnerable_ticks: i32,
    /// Whether a wither skull is the blue, charged variant.
    wither_skull_dangerous: bool,
    /// Whether a bogged has been sheared of its mushrooms.
    bogged_sheared: bool,
    /// Whether a turtle is carrying an egg.
    turtle_has_egg: bool,
    /// Whether a creaking is active (its eyes glow).
    creaking_active: bool,
    /// An arrow's effect colour; `-1` for an untipped arrow.
    arrow_effect_color: i32,
}
