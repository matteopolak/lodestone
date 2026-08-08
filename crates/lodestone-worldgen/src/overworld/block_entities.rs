//! Issue #520: the generator's block-entity layer.
//!
//! ## What it is
//!
//! A list of block entities a generated column carries alongside its block field,
//! so a decoration feature can produce a block *plus* the state that block needs to
//! be more than scenery. Today there is exactly one producer — the beehive
//! decorator, whose nests reached the client empty because the draw for
//! "2 or 3 bees" was consumed and thrown away
//! ([`crate::feature::vegetation::place`]).
//!
//! ## How it works
//!
//! [`crate::feature::vegetation::VegGrid`] collects them during decoration, exactly
//! as it collects block writes; `OverworldGenerator::vegetation_stage` drains the
//! list and keeps only what landed inside the served 16×16, which is the same
//! discard rule the grid's own `dirty_cells` fold-back already applies to spilled
//! blocks. A nest that spilled into a neighbour belongs to *that* chunk's own
//! generation pass, not to this one.
//!
//! ## How to change it: why this is a typed enum and not an NBT blob
//!
//! The issue asks for "position + type + NBT". A generic NBT value would mean this
//! crate either taking an NBT dependency or inventing its own tag type, and it
//! would move the "did I spell the field names right" question from compile time to
//! a wire gate. One variant per block-entity kind the generator can actually
//! produce keeps that question in the type system, and the list is short: adding
//! chests, spawners and decorated pots for the structure engine means adding
//! variants here, and the consumer's `match` then fails to compile until it handles
//! them — which is the property a blob would throw away.
//!
//! ## Dependencies
//!
//! None. The **consumer** side is not in this crate: `ChunkColumn` has no
//! block-entity field and the chunk-data packet writes a hardcoded `var_i32(0)`,
//! both outside `lodestone-worldgen`. See #520's own comment for that patch.

/// One block entity a generated column carries, with its **absolute** world
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedBlockEntity {
    /// `BeehiveBlockEntity` for a freshly generated `bee_nest`.
    ///
    /// Field names and shapes come from `BeehiveBlockEntity.Occupant`
    /// (`.cache/mc/26.2/.../BeehiveBlockEntity.java:366`) —
    /// `{entity_data, ticks_in_hive, min_ticks_in_hive}` under a `bees` list — not
    /// from memory. `entity_data` is not modelled per-occupant because
    /// `Occupant.create` builds it from an empty `CompoundTag` plus
    /// `EntityTypes.BEE`, so every generated occupant's is the same value: the
    /// consumer writes `{id: "minecraft:bee"}`.
    Beehive {
        x: i32,
        y: i32,
        z: i32,
        /// One entry per bee, in the order the decorator drew them.
        bees: Vec<BeeOccupant>,
    },
}

impl GeneratedBlockEntity {
    /// Absolute world position.
    #[must_use]
    pub fn position(&self) -> (i32, i32, i32) {
        match self {
            GeneratedBlockEntity::Beehive { x, y, z, .. } => (*x, *y, *z),
        }
    }

    /// The block-entity registry id, for the wire array's type field.
    #[must_use]
    pub fn type_id(&self) -> &'static str {
        match self {
            GeneratedBlockEntity::Beehive { .. } => "minecraft:beehive",
        }
    }
}

/// `BeehiveBlockEntity.Occupant`, minus the `entity_data` every generated bee
/// shares. See [`GeneratedBlockEntity::Beehive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeeOccupant {
    /// `Occupant.create(random.nextInt(599))`'s argument.
    pub ticks_in_hive: i32,
    /// Always 600 for a generated bee — `Occupant.create`'s constant. Carried
    /// explicitly rather than implied, because the *other* constructor
    /// (`Occupant.of`) uses 2400 for a bee with nectar and a future producer may
    /// need it.
    pub min_ticks_in_hive: i32,
}
