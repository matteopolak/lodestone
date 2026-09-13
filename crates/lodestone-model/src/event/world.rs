//! World, dimension, and particle payloads.

use crate::*;
use lodestone_core::Nbt;

/// A last-death location, from the optional `GlobalPos` field of
/// the respawn packet (and the game-join packet's equivalent).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeathLocation {
    /// Dimension the death occurred in.
    pub dimension: DimensionId,
    /// Block position of the death.
    pub pos: BlockPos,
}

/// The server-declared properties of the dimension type the local player is in.
///
/// # Why this is a distinct fact from [`DimensionId`]
///
/// A [`DimensionId`] names a *level* (`minecraft:the_nether`, or `mypack:mine`);
/// a dimension **type** is a registry entry that level points at, and it is
/// where the geometry and lighting rules actually live. Two levels can share one
/// type, and a data pack can give a level called `mypack:mine` the vanilla
/// overworld type — which is exactly the case that made matching on the level
/// name wrong.
///
/// Version adapters fill this in from the Configuration `registry_data` packet;
/// before that, nothing decoded that packet at all, so every field here was
/// hardcoded client-side by level-name match.
///
/// # Field selection
///
/// Only the fields a version-free consumer can act on. Vanilla's dimension-type
/// record additionally carries `infiniburn`, monster-spawn settings, a skybox
/// choice, a cardinal-light mode and a timeline set. Those remain outside this
/// version-free consumer surface; the environment-attribute map is retained
/// because visual attributes already have render consumers.
///
/// Note there is **no `bed_works`**: 26.2 moved that into the dimension type's
/// environment attributes (`minecraft:gameplay/bed_rule`), so it is not a
/// top-level dimension-type field any more and cannot be modelled as a bool.
#[derive(Debug, Clone, PartialEq)]
pub struct DimensionTypeInfo {
    /// The dimension type's own registry id, e.g. `minecraft:overworld`. This is
    /// a `dimension_type` id, **not** the level's [`DimensionId`].
    pub name: ResourceKey,
    /// Whether columns here carry sky light. `false` only in the Nether among
    /// vanilla's four types — the End has sky light exactly like the overworld.
    pub has_skylight: bool,
    /// Whether the dimension has a solid ceiling (the Nether).
    pub has_ceiling: bool,
    /// Whether the time of day is fixed here (the Nether and the End).
    pub has_fixed_time: bool,
    /// Movement scale relative to the overworld — `8.0` in the Nether.
    pub coordinate_scale: f64,
    /// Lowest world-`y` a column stores (`-64` overworld, `0` Nether/End).
    pub min_y: i32,
    /// Total column height in blocks (`384` overworld, `256` Nether/End).
    pub height: i32,
    /// Highest `y` a portal or bed may place the player at (`128` in the Nether,
    /// against a height of `256`).
    pub logical_height: i32,
    /// Baseline light every block receives regardless of sky exposure — `0.0`
    /// overworld, `0.1` Nether, `0.25` End.
    pub ambient_light: f32,
    /// The dimension's ambient-light-color attribute, packed `0xRRGGBB` — the
    /// colour the GPU lightmap seeds its accumulator with before either light
    /// half is added, so an unlit surface is not pure black. **Not** the same
    /// quantity as [`Self::ambient_light`] above (that one only ever blends a
    /// *lerp fraction*; this is the actual seed colour the terrain/entity/fluid
    /// shaders read). Grey in the overworld, warm brown in the Nether, sage in
    /// the End — see `lodestone_render::light`. `None` when the source did not
    /// resolve one; a version-free consumer should fall back to the
    /// overworld's own value rather than invent a brighter one.
    pub ambient_light_color: Option<u32>,
    /// The server's complete environment-attribute map, retained in wire
    /// order so data-pack extensions survive the version seam. Typed visual
    /// fields below are extracted from this map; unknown keys remain available
    /// to a future consumer instead of being discarded at decode time.
    pub environment_attributes: Vec<(String, Nbt)>,
    /// Dimension-level `visual/fog_color`, packed RGB, when declared.
    pub fog_color: Option<u32>,
    /// Dimension-level `visual/sky_color`, packed RGB, when declared.
    pub sky_color: Option<u32>,
    /// Dimension-level `visual/cloud_color`, packed ARGB, when declared.
    pub cloud_color: Option<u32>,
    /// Dimension-level `visual/sky_light_factor`, validated finite value.
    pub sky_light_factor: Option<f32>,
}

impl DimensionTypeInfo {
    /// Number of 16-tall block sections in a column of this dimension.
    #[must_use]
    pub fn section_count(&self) -> usize {
        usize::try_from(self.height.max(0)).unwrap_or(0) / 16
    }
}

/// A raw block-state id whose numbering source is known, but whose built-in
/// census membership has not yet been checked.
///
/// The version-free event model cannot validate a numeric state against the
/// generated 26.2 census: an older protocol family may use a different
/// numbering, and a synchronized extension may own an opaque value that no
/// built-in table can name. The adapter must therefore tag the source rather
/// than calling `lodestone_data::block_states::StateId::new` on every raw
/// value. A consumer that needs generated data validates only
/// [`Self::Canonical`] at its own boundary; it leaves [`Self::ProtocolLocal`]
/// intact until a matching version or dynamic-registry resolver is available.
///
/// This intentionally owns no `StateId`: `lodestone-model` stays independent
/// of the generated-data crate, while `StateId` remains the proof that a value
/// is one of this build's built-in states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockStateRef {
    /// A raw global state id in the canonical 26.2 numbering. It still needs
    /// generated-census validation before an indexed built-in lookup.
    Canonical(u32),
    /// A raw state id whose protocol family or synchronized extension owns the
    /// numbering. This may numerically overlap the canonical range, so it must
    /// never be range-checked as though it were a 26.2 state.
    ProtocolLocal(u32),
}

impl BlockStateRef {
    /// Tags a raw global state id emitted by the canonical 26.2 protocol.
    #[must_use]
    pub const fn canonical(raw: u32) -> Self {
        Self::Canonical(raw)
    }

    /// Tags a raw state id from a protocol-local or dynamic registry.
    #[must_use]
    pub const fn protocol_local(raw: u32) -> Self {
        Self::ProtocolLocal(raw)
    }

    /// The original numeric value, for the source-specific resolver that owns
    /// this reference's numbering.
    #[must_use]
    pub const fn raw(self) -> u32 {
        match self {
            Self::Canonical(raw) | Self::ProtocolLocal(raw) => raw,
        }
    }
}

/// A level event's payload, retaining block-state numbering provenance for the
/// one event whose payload names a block state.
///
/// Most level-event payloads are event-specific signed integers and remain
/// [`Self::Raw`]. Event `2001` carries a state id instead; adapters turn that
/// payload into [`Self::BlockState`] while they still know whether the wire
/// numbering is canonical or protocol-local. This prevents a shell consumer
/// from recovering intent by range-checking a bare integer after that source
/// information has already been lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LevelEventData {
    /// An event-specific signed payload with no block-state interpretation.
    Raw(i32),
    /// Event `2001`'s pre-destruction block state.
    BlockState(BlockStateRef),
}

impl LevelEventData {
    /// The original 32 payload bits, for callers that deliberately handle an
    /// event's protocol-specific data rather than a built-in block-state
    /// lookup.
    #[must_use]
    pub const fn raw_i32(self) -> i32 {
        match self {
            Self::Raw(raw) => raw,
            Self::BlockState(state) => state.raw() as i32,
        }
    }
}

/// A `minecraft:particle_type` registry entry's type-specific payload —
/// [`ClientEvent::Particles`]'s `options`.
///
/// Most vanilla particle types are a bare `SimpleParticleType` with no
/// payload at all ([`Self::None`], the common case); a handful carry extra
/// fields read immediately after the registry id (`DustParticleOptions`,
/// `BlockParticleOption`, `ItemParticleOption`, …). Adding a variant here
/// does not by itself decode anything — the adapter's `LEVEL_PARTICLES` arm
/// (`crates/versions/26.2/src/adapter/chunk.rs`) is what parses a payload out
/// of the wire bytes based on the resolved particle name, and only for the
/// names it recognises; every other name still resolves to [`Self::None`],
/// same as before this type existed.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ParticleOptions {
    /// No type-specific payload.
    #[default]
    None,
    /// `minecraft:dust` (`DustParticleOptions`).
    Dust {
        /// Colour, unpacked from the wire's packed RGB24 `i32` to `[0, 1]`
        /// components (`ARGB.vector3fFromRGB24`).
        color: [f32; 3],
        /// Size multiplier (`ScalableParticleOptionsBase::getScale`).
        scale: f32,
    },
    /// `minecraft:dust_color_transition` (`DustColorTransitionOptions`) — the
    /// sculk-to-redstone sibling of [`Self::Dust`] that lerps colour over its
    /// life instead of holding one fixed.
    DustColorTransition {
        /// Starting colour, same unpacking as [`Self::Dust`]'s `color`.
        from_color: [f32; 3],
        /// Ending colour.
        to_color: [f32; 3],
        /// Size multiplier.
        scale: f32,
    },
    /// `minecraft:effect` and `minecraft:instant_effect`
    /// (`SpellParticleOption`) — the potion-effect motes trailing an entity
    /// under a status effect, and a splash potion's instant burst.
    Spell {
        /// Tint, unpacked from the wire's packed RGB24 `i32` the same way
        /// [`Self::Dust`]'s `color` is. Vanilla's own spell-particle option's own accessors
        /// read only the low three bytes (its own red/green/blue channel reads), so
        /// the top byte of the wire word is not an alpha here — that is
        /// [`Self::Color`]'s field, on a different option type.
        color: [f32; 3],
        /// Velocity multiplier (`SpellParticleOption::getPower`, applied by
        /// the provider through `Particle.setPower`). Defaults to `1.0` in the
        /// data codec but is unconditional on the wire.
        power: f32,
    },
    /// `minecraft:entity_effect` (`ColorParticleOption`) — the ambient motes a
    /// mob under a status effect, or a lingering potion's cloud, gives off.
    ///
    /// Distinct from [`Self::Spell`] despite both driving the same
    /// `SpellParticle` class: this one is a **four**-component ARGB word with
    /// no power field, and the two are not interchangeable on the wire (8
    /// bytes against 4).
    Color {
        /// Tint and alpha, unpacked from the wire's packed **ARGB** `i32` —
        /// `[ARGB.red, ARGB.green, ARGB.blue, ARGB.alpha]`, each `/ 255.0`.
        /// The alpha byte is the top one and is genuinely used
        /// (vanilla's own mob-effect spell-particle provider sets alpha with it), so
        /// dropping it makes every ambient effect mote fully opaque.
        color: [f32; 4],
    },
    /// `minecraft:dragon_breath` (`PowerParticleOption`) — a bare velocity
    /// multiplier and nothing else.
    ///
    /// Its own variant rather than a reuse of [`Self::Spell`]'s `power`: this
    /// option class carries no colour at all (`DragonBreathParticle` draws its
    /// purple out of the RNG), so the wire payload is four bytes against
    /// `SpellParticleOption`'s eight and the two are not interchangeable.
    Power {
        /// Velocity multiplier (`PowerParticleOption::getPower`, applied by
        /// the provider through `Particle.setPower`).
        power: f32,
    },
    /// `minecraft:sculk_charge` (`SculkChargeParticleOptions`).
    SculkCharge {
        /// Roll about the view axis, in radians — the one thing that makes a
        /// sculk charge's motes lie along the direction the charge is
        /// spreading rather than all sharing one orientation.
        roll: f32,
    },
    /// The `BlockParticleOption` family — `minecraft:block`,
    /// `minecraft:block_marker`, `minecraft:block_crumble`,
    /// `minecraft:dust_pillar` and `minecraft:falling_dust`.
    ///
    /// One payload type shared by five registry entries whose *providers* have
    /// nothing else in common: three build a `TerrainParticle` (with different
    /// speeds and lifetimes), one builds a physics-free marker quad and one
    /// builds a sheet-textured falling mote tinted from the block. The wire
    /// payload is identical for all five, so they share this variant and the
    /// emitters differ — reading the shared payload as a shared *behaviour* is
    /// what would make a `block_marker` fall and a `falling_dust` wear the
    /// block's own texture.
    BlockState {
        /// The block state, by **block-state** network id — not a block id and
        /// not an item id. [`BlockStateRef::Canonical`] is the 26.2 numbering
        /// that a built-in renderer may validate against
        /// `lodestone_data::block_states`; [`BlockStateRef::ProtocolLocal`]
        /// stays opaque for a version-aware or dynamic-registry consumer.
        state: BlockStateRef,
    },
}
