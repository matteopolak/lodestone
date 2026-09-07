//! The generator's block-entity layer.
//!
//! ## What it is
//!
//! A list of block entities a generated column carries alongside its block field,
//! so a decoration feature can produce a block *plus* the state that block needs to
//! be more than scenery. The beehive decorator and underground dungeon feature
//! are the current producers.
//!
//! ## How it works
//!
//! [`crate::feature::vegetation::VegGrid`] collects them during decoration, exactly
//! as it collects block writes; the unified FEATURES dispatcher drains the
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
//! produce keeps that question in the type system. Each variant carries the data
//! needed to build the save and packet forms at the
//! server boundary. Adding another generated block entity means adding a variant
//! here, and the consumer's `match` then fails to compile until it handles it —
//! which is the property a blob would throw away.
//!
//! ## Dependencies
//!
//! None. The server boundary consumes this typed list when it adopts a generated
//! column and turns each entry into the appropriate block-entity record.

/// One block entity a generated column carries, with its **absolute** world
/// position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedBlockEntity {
    /// `BeehiveBlockEntity` for a freshly generated `bee_nest`.
    ///
    /// Field names and shapes come from vanilla's own beehive-block-entity
    /// occupant record —
    /// `{entity_data, ticks_in_hive, min_ticks_in_hive}` under a `bees` list — not
    /// from memory. `entity_data` is not modelled per-occupant because
    /// vanilla's own occupant-create call builds it from an empty `CompoundTag` plus
    /// the bee entity type, so every generated occupant's is the same value: the
    /// consumer writes `{id: "minecraft:bee"}`.
    Beehive {
        x: i32,
        y: i32,
        z: i32,
        /// One entry per bee, in the order the decorator drew them.
        bees: Vec<BeeOccupant>,
    },
    /// A dungeon chest with deferred loot generation.
    ///
    /// `loot_table_seed` is the feature's own random draw, retained so the
    /// server can reproduce the deferred table selection after the chunk is
    /// sent or saved. The block state's facing is carried separately because
    /// the chest's block entity does not own block-state properties.
    DungeonChest {
        x: i32,
        y: i32,
        z: i32,
        /// The canonical horizontal facing (`north`, `south`, `east`, or `west`).
        facing: String,
        /// The resource id of the deferred loot table.
        loot_table: String,
        /// The random seed attached to that deferred table.
        loot_table_seed: i64,
    },
    /// A dungeon monster spawner with the selected initial entity type.
    DungeonSpawner {
        x: i32,
        y: i32,
        z: i32,
        /// The resource id selected by the feature's final bounded draw.
        entity_type: String,
    },
}

impl GeneratedBlockEntity {
    /// Absolute world position.
    #[must_use]
    pub fn position(&self) -> (i32, i32, i32) {
        match self {
            GeneratedBlockEntity::Beehive { x, y, z, .. } => (*x, *y, *z),
            GeneratedBlockEntity::DungeonChest { x, y, z, .. }
            | GeneratedBlockEntity::DungeonSpawner { x, y, z, .. } => (*x, *y, *z),
        }
    }

    /// The block-entity registry id, for the wire array's type field.
    #[must_use]
    pub fn type_id(&self) -> &'static str {
        match self {
            GeneratedBlockEntity::Beehive { .. } => "minecraft:beehive",
            GeneratedBlockEntity::DungeonChest { .. } => "minecraft:chest",
            GeneratedBlockEntity::DungeonSpawner { .. } => "minecraft:mob_spawner",
        }
    }
}

/// Vanilla's own beehive-block-entity occupant, minus the `entity_data` every generated bee
/// shares. See [`GeneratedBlockEntity::Beehive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BeeOccupant {
    /// Vanilla's own bee-occupant constructor's `random.nextInt(599)` argument.
    pub ticks_in_hive: i32,
    /// Always 600 for a generated bee — vanilla's own bee-occupant constructor's constant. Carried
    /// explicitly rather than implied, because the *other* constructor
    /// (its own "of nectar" constructor) uses 2400 for a bee with nectar and a future producer may
    /// need it.
    pub min_ticks_in_hive: i32,
}
