//! Vanilla 26.2's `minecraft:damage_type` registry and its 35 damage-type tags.
//!
//! # What it is
//!
//! The authoritative per-damage-type table every damage-producing subsystem
//! reads instead of hand-deriving flags: `message_id`, `exhaustion`, `scaling`,
//! `effects`, `death_message_type`, and — the load-bearing part — **resolved
//! tag membership**. Behaviour keys off tags, not off the type: whether a hit
//! skips armour, ignores i-frames, sets you on fire or applies no knockback is
//! a damage-type-tags query in vanilla, so it is a
//! [`DamageType::is_in`] query here.
//!
//! # Provenance
//!
//! Unlike hardness and collision shapes — which have *no* datapack
//! representation and so need a registry-walking JVM oracle — damage types
//! **are** shipped as plain data files inside the server jar's embedded vanilla
//! datapack. `tests/support/damage_types_jar.txt` is those files, verbatim, so
//! the anchor is the game's own data rather than a program's reading of it.
//!
//! Two traps found while building this, both recorded because they cost time:
//!
//! * The **outer** `.cache/mc/26.2/server.jar` is a *bundler*: it contains none
//!   of these paths and searching it returns zero hits. The real jar is
//!   `.cache/mc/26.2/versions/26.2/server-26.2.jar`.
//! * **Seven of the 34 tag files reference other tags** (`"#minecraft:is_explosion"`),
//!   so membership is a **transitive closure**, not a flat read. `bypasses_shield`
//!   pulls in all 19 of `bypasses_armor`; a flat reader would report 11 members
//!   instead of 30 and every shield check downstream would be wrong. The closure
//!   is resolved once, at table-generation time, so a consumer's `is_in` is a
//!   single bit test.
//!
//! # `bypasses_cooldown` is real, and empty
//!
//! Vanilla's own damage-type-tags constant declares the tag and
//! its own "hurt server" step gates the whole i-frame window on it —
//! `if (this.invulnerableTime > 10.0F && !source.is(DamageTypeTags.BYPASSES_COOLDOWN))`
//! — but **no data file for it exists in the jar**. It is a genuinely empty tag
//! in vanilla 26.2: the mechanism exists and nothing opts into it. So this table
//! carries all **35** tags (34 with data files, plus this one) and
//! `DamageTypeTag::BypassesCooldown.members()` is legitimately empty.
//!
//! That emptiness is *asserted*, not assumed — `tests/damage_types.rs` fails if
//! a future version ships the file, rather than silently continuing to report
//! "nothing bypasses the cooldown".
//!
//! # These indices are NOT network ids
//!
//! `minecraft:damage_type` is absent from `registries.json` entirely because it
//! is purely data-driven: it has no default protocol id, and its network id is
//! assigned per-connection by registry-sync order. [`DamageType`]'s discriminant
//! is an index into this table, ordered **alphabetically by name** for
//! determinism. Never send it on the wire; resolve through registry sync.
//! (`mob_effects.rs` carries the same warning from the other direction — that
//! registry *is* built-in and its ids *are* network ids.)
//!
//! # How to change it
//!
//! After a version bump, re-extract the dump and regenerate:
//! `just regen-damage-types`. If a version adds or removes a damage type or tag,
//! the generated counts change and the drift gate fails loudly naming the file.
//! Adding a *new* tag also needs a [`DamageTypeTag`] variant — the test asserts
//! this enum's names match the generated table's names in order, so a forgotten
//! variant fails rather than silently shifting every bit.

use crate::generated_damage_types::{
    DAMAGE_TYPE_DEATH_MESSAGE, DAMAGE_TYPE_EFFECTS, DAMAGE_TYPE_EXHAUSTION_BITS,
    DAMAGE_TYPE_MESSAGE_IDS, DAMAGE_TYPE_NAMES, DAMAGE_TYPE_SCALING, DAMAGE_TYPE_TAG_MASKS,
    DAMAGE_TYPE_TAG_NAMES,
};

pub use crate::generated_damage_types::{DAMAGE_TYPE_COUNT, DAMAGE_TYPE_TAG_COUNT};

/// How a damage amount scales with world difficulty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamageScaling {
    /// Never scaled.
    Never,
    /// Scaled only when the cause is a living non-player entity (the default in
    /// vanilla's codec, and what 47 of the 51 types use).
    WhenCausedByLivingNonPlayer,
    /// Always scaled.
    Always,
}

impl DamageScaling {
    /// The serialized name (vanilla's own "get serialized name" accessor).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::WhenCausedByLivingNonPlayer => "when_caused_by_living_non_player",
            Self::Always => "always",
        }
    }

    pub(crate) const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Never,
            1 => Self::WhenCausedByLivingNonPlayer,
            _ => Self::Always,
        }
    }
}

/// The hurt animation/sound family a type plays.
///
/// `effects` is an optional codec field defaulting to `HURT` in
/// vanilla's own damage-type direct codec, so the 39 types with no `effects` key are
/// [`DamageEffects::Hurt`] — a real value, not a missing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamageEffects {
    /// Generic hurt (`entity.player.hurt`).
    Hurt,
    /// Thorns.
    Thorns,
    /// Drowning.
    Drowning,
    /// Burning.
    Burning,
    /// Poking (sweet berry bush).
    Poking,
    /// Freezing.
    Freezing,
}

impl DamageEffects {
    /// The serialized name (vanilla's own "get serialized name" accessor).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Hurt => "hurt",
            Self::Thorns => "thorns",
            Self::Drowning => "drowning",
            Self::Burning => "burning",
            Self::Poking => "poking",
            Self::Freezing => "freezing",
        }
    }

    pub(crate) const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Hurt,
            1 => Self::Thorns,
            2 => Self::Drowning,
            3 => Self::Burning,
            4 => Self::Poking,
            _ => Self::Freezing,
        }
    }
}

/// Which death-message form a type uses.
///
/// Also an optional codec field, defaulting to [`DeathMessageType::Default`]
/// (vanilla's own damage-type codec): only `fall`/`ender_pearl`/`stalagmite`-style
/// fall variants and `bad_respawn_point` differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeathMessageType {
    /// `death.attack.<message_id>`.
    Default,
    /// Picks a variant from the recorded fall location.
    FallVariants,
    /// The bed/respawn-anchor explosion message.
    IntentionalGameDesign,
}

impl DeathMessageType {
    /// The serialized name (vanilla's own "get serialized name" accessor).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::FallVariants => "fall_variants",
            Self::IntentionalGameDesign => "intentional_game_design",
        }
    }

    pub(crate) const fn from_index(index: u8) -> Self {
        match index {
            0 => Self::Default,
            1 => Self::FallVariants,
            _ => Self::IntentionalGameDesign,
        }
    }
}

/// One of vanilla 26.2's 35 `minecraft:damage_type` tags.
///
/// Ordered alphabetically by name, matching `DAMAGE_TYPE_TAG_NAMES`; the
/// discriminant is the bit position in a type's tag mask. `tests/damage_types.rs`
/// asserts this ordering against the generated table, so a variant added in the
/// wrong place fails rather than shifting every membership bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DamageTypeTag {
    /// Hurts the ender dragon even while it is invulnerable.
    AlwaysHurtsEnderDragons = 0,
    /// Destroys an armour stand outright.
    AlwaysKillsArmorStands = 1,
    /// Always counts as the most significant fall for death-message purposes.
    AlwaysMostSignificantFall = 2,
    /// Always wakes nearby silverfish.
    AlwaysTriggersSilverfish = 3,
    /// Not reflected by a guardian's thorns.
    AvoidsGuardianThorns = 4,
    /// Burns an entity that stepped onto the block.
    BurnFromStepping = 5,
    /// Sets an armour stand on fire.
    BurnsArmorStands = 6,
    /// Skips the armour-absorb stage (vanilla's own "get damage after armor absorb" step).
    BypassesArmor = 7,
    /// Ignores the i-frame window (vanilla's own "hurt server" step).
    ///
    /// **Empty in vanilla 26.2** — a code constant with no data file. See the
    /// module docs; the emptiness is asserted by the test suite.
    BypassesCooldown = 8,
    /// Skips both Resistance and enchantment protection (vanilla's own "get damage after magic absorb" step).
    BypassesEffects = 9,
    /// Skips only enchantment protection (vanilla's own "get damage after magic absorb" step).
    BypassesEnchantments = 10,
    /// Hurts an entity flagged invulnerable (vanilla's own "is invulnerable to base" check).
    BypassesInvulnerability = 11,
    /// Skips only the Resistance effect (vanilla's own "get damage after magic absorb" step).
    BypassesResistance = 12,
    /// Cannot be blocked with a shield.
    BypassesShield = 13,
    /// Not reduced by wolf armour.
    BypassesWolfArmor = 14,
    /// May break an armour stand.
    CanBreakArmorStand = 15,
    /// Damages the victim's helmet (falling blocks, anvils).
    DamagesHelmet = 16,
    /// Ignites armour stands.
    IgnitesArmorStands = 17,
    /// Counts as drowning.
    IsDrowning = 18,
    /// Counts as an explosion.
    IsExplosion = 19,
    /// Counts as fall damage.
    IsFall = 20,
    /// Counts as fire damage.
    IsFire = 21,
    /// Counts as freezing.
    IsFreezing = 22,
    /// Counts as lightning.
    IsLightning = 23,
    /// Counts as a direct player attack.
    IsPlayerAttack = 24,
    /// Counts as projectile damage.
    IsProjectile = 25,
    /// Mace smash damage.
    MaceSmash = 26,
    /// Does not anger the victim at the attacker.
    NoAnger = 27,
    /// Produces no hurt "impact" (no knockback/particles path).
    NoImpact = 28,
    /// Applies no knockback impulse.
    NoKnockback = 29,
    /// Causes a panic reaction.
    PanicCauses = 30,
    /// Environmental subset of the panic causes.
    PanicEnvironmentalCauses = 31,
    /// A sulfur cube with a block is immune to these.
    SulfurCubeWithBlockImmuneTo = 32,
    /// A witch is resistant to these.
    WitchResistantTo = 33,
    /// A wither is immune to these.
    WitherImmuneTo = 34,
}

/// Every tag, in table order.
pub static ALL_DAMAGE_TYPE_TAGS: [DamageTypeTag; DAMAGE_TYPE_TAG_COUNT] = [
    DamageTypeTag::AlwaysHurtsEnderDragons,
    DamageTypeTag::AlwaysKillsArmorStands,
    DamageTypeTag::AlwaysMostSignificantFall,
    DamageTypeTag::AlwaysTriggersSilverfish,
    DamageTypeTag::AvoidsGuardianThorns,
    DamageTypeTag::BurnFromStepping,
    DamageTypeTag::BurnsArmorStands,
    DamageTypeTag::BypassesArmor,
    DamageTypeTag::BypassesCooldown,
    DamageTypeTag::BypassesEffects,
    DamageTypeTag::BypassesEnchantments,
    DamageTypeTag::BypassesInvulnerability,
    DamageTypeTag::BypassesResistance,
    DamageTypeTag::BypassesShield,
    DamageTypeTag::BypassesWolfArmor,
    DamageTypeTag::CanBreakArmorStand,
    DamageTypeTag::DamagesHelmet,
    DamageTypeTag::IgnitesArmorStands,
    DamageTypeTag::IsDrowning,
    DamageTypeTag::IsExplosion,
    DamageTypeTag::IsFall,
    DamageTypeTag::IsFire,
    DamageTypeTag::IsFreezing,
    DamageTypeTag::IsLightning,
    DamageTypeTag::IsPlayerAttack,
    DamageTypeTag::IsProjectile,
    DamageTypeTag::MaceSmash,
    DamageTypeTag::NoAnger,
    DamageTypeTag::NoImpact,
    DamageTypeTag::NoKnockback,
    DamageTypeTag::PanicCauses,
    DamageTypeTag::PanicEnvironmentalCauses,
    DamageTypeTag::SulfurCubeWithBlockImmuneTo,
    DamageTypeTag::WitchResistantTo,
    DamageTypeTag::WitherImmuneTo,
];

impl DamageTypeTag {
    /// The tag's path name, without the `minecraft:` namespace
    /// (e.g. `"bypasses_armor"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        DAMAGE_TYPE_TAG_NAMES[self as usize]
    }

    /// The bit this tag occupies in a damage type's mask.
    #[must_use]
    pub const fn bit(self) -> u64 {
        1u64 << (self as u8)
    }

    /// Every damage type in this tag, in table order.
    ///
    /// Membership is the resolved transitive closure, so `BypassesShield`
    /// yields all 19 `bypasses_armor` members too.
    pub fn members(self) -> impl Iterator<Item = DamageType> {
        let bit = self.bit();
        DamageType::ALL
            .into_iter()
            .filter(move |ty| DAMAGE_TYPE_TAG_MASKS[ty.index()] & bit != 0)
    }
}

/// A vanilla 26.2 damage type — an index into the generated table, **not** a
/// network registry id (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DamageType(u8);

impl DamageType {
    /// Every damage type, ordered alphabetically by name.
    pub const ALL: [Self; DAMAGE_TYPE_COUNT] = {
        let mut all = [Self(0); DAMAGE_TYPE_COUNT];
        let mut i = 0;
        while i < DAMAGE_TYPE_COUNT {
            all[i] = Self(i as u8);
            i += 1;
        }
        all
    };

    /// Resolves a damage type by name, with or without the `minecraft:`
    /// namespace (`"minecraft:fall"` and `"fall"` both work).
    ///
    /// Returns `None` for an unknown name so a datapack-added or future-version
    /// type surfaces as an explicit miss rather than a wrong default — the
    /// silent-wrong-default bug shape this crate keeps hitting.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let path = name.strip_prefix("minecraft:").unwrap_or(name);
        DAMAGE_TYPE_NAMES
            .iter()
            .position(|candidate| *candidate == path)
            .map(|index| Self(index as u8))
    }

    /// The table index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The type's path name, without the namespace (e.g. `"mob_attack"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        DAMAGE_TYPE_NAMES[self.index()]
    }

    /// The `message_id` used to build the death-message translation key
    /// (`death.attack.<message_id>`). Note it is *not* the type name:
    /// `mob_attack`'s is `"mob"` and `bad_respawn_point`'s is
    /// `"badRespawnPoint"`.
    #[must_use]
    pub fn message_id(self) -> &'static str {
        DAMAGE_TYPE_MESSAGE_IDS[self.index()]
    }

    /// Food exhaustion added to the victim when this damage lands.
    #[must_use]
    pub fn exhaustion(self) -> f32 {
        f32::from_bits(DAMAGE_TYPE_EXHAUSTION_BITS[self.index()])
    }

    /// How the amount scales with difficulty.
    #[must_use]
    pub fn scaling(self) -> DamageScaling {
        DamageScaling::from_index(DAMAGE_TYPE_SCALING[self.index()])
    }

    /// The hurt animation/sound family.
    #[must_use]
    pub fn effects(self) -> DamageEffects {
        DamageEffects::from_index(DAMAGE_TYPE_EFFECTS[self.index()])
    }

    /// Which death-message form this type uses.
    #[must_use]
    pub fn death_message_type(self) -> DeathMessageType {
        DeathMessageType::from_index(DAMAGE_TYPE_DEATH_MESSAGE[self.index()])
    }

    /// Whether this type is in `tag`, using the resolved transitive closure.
    ///
    /// This is the query behaviour keys off — the direct equivalent of
    /// vanilla's `source.is(DamageTypeTags.X)`.
    ///
    /// (Not a `const fn`: Rust forbids reading a `static` in a const context,
    /// and the table is a `static` to match every other generated table here.)
    #[must_use]
    pub fn is_in(self, tag: DamageTypeTag) -> bool {
        DAMAGE_TYPE_TAG_MASKS[self.index()] & tag.bit() != 0
    }

}
