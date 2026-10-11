//! Entities that structure generation spawns, as plain entity tags.
//!
//! A structure's entities are decided where its blocks are: a coded piece records them beside its
//! block list, a template carries them in its file, and a data marker names one for a structure's
//! own handler. Each is spawned by the chunk that contains its block cell, once, when that chunk
//! is generated, so the engine hands the server a list of entity tags per chunk and leaves
//! building the live entity to the server's mob simulation.

use lodestone_core::Nbt;

use super::template::{BlockNbt, Mirror, PlaceSettings, Rotation};

/// A fresh entity: the tag a saved entity of this type would have, holding at least `id`, `Pos`
/// and `Rotation` plus whatever the spawning rule set.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureEntity {
    /// The entity tag fields.
    pub nbt: BlockNbt,
}

impl StructureEntity {
    /// An entity of type `id` standing at `pos` looking along `yaw`/`pitch`.
    #[must_use]
    pub fn new(id: &str, pos: [f64; 3], yaw: f32, pitch: f32) -> Self {
        Self {
            nbt: vec![
                ("id".to_owned(), Nbt::String(id.to_owned())),
                ("Pos".to_owned(), doubles(pos)),
                ("Rotation".to_owned(), floats([yaw, pitch])),
            ],
        }
    }

    /// Sets (or replaces) one tag field.
    #[must_use]
    pub fn with(mut self, key: &str, value: Nbt) -> Self {
        self.set(key, value);
        self
    }

    /// Marks the entity as never despawning naturally.
    #[must_use]
    pub fn persistent(self) -> Self {
        self.with("PersistenceRequired", Nbt::Byte(1))
    }

    fn set(&mut self, key: &str, value: Nbt) {
        match self.nbt.iter_mut().find(|(name, _)| name == key) {
            Some((_, slot)) => *slot = value,
            None => self.nbt.push((key.to_owned(), value)),
        }
    }

    /// The tag field `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Nbt> {
        self.nbt.iter().find(|(name, _)| name == key).map(|(_, value)| value)
    }

    /// The entity type id, e.g. `minecraft:witch`.
    #[must_use]
    pub fn id(&self) -> &str {
        match self.get("id") {
            Some(Nbt::String(id)) => id,
            _ => "",
        }
    }

    /// Where the entity stands.
    #[must_use]
    pub fn pos(&self) -> [f64; 3] {
        let Some(Nbt::List { elements, .. }) = self.get("Pos") else {
            return [0.0; 3];
        };
        let at = |i: usize| match elements.get(i) {
            Some(Nbt::Double(v)) => *v,
            _ => 0.0,
        };
        [at(0), at(1), at(2)]
    }

    /// The block cell holding the entity's position, which decides the chunk that spawns it.
    #[must_use]
    pub fn block(&self) -> [i32; 3] {
        self.pos().map(|v| v.floor() as i32)
    }
}

fn doubles(v: [f64; 3]) -> Nbt {
    Nbt::List {
        element_type: lodestone_core::NbtTag::Double,
        elements: v.into_iter().map(Nbt::Double).collect(),
    }
}

fn floats(v: [f32; 2]) -> Nbt {
    Nbt::List {
        element_type: lodestone_core::NbtTag::Float,
        elements: v.into_iter().map(Nbt::Float).collect(),
    }
}

/// A mob standing on a block's floor at `block`, centred in the cell.
#[must_use]
pub fn mob_at_block(id: &str, block: [i32; 3]) -> StructureEntity {
    StructureEntity::new(id, [f64::from(block[0]) + 0.5, f64::from(block[1]), f64::from(block[2]) + 0.5], 0.0, 0.0).persistent()
}

/// A chest minecart resting on the rail cell `block`, holding an unrolled loot table.
#[must_use]
pub fn chest_minecart(block: [i32; 3], table: &str, seed: i64) -> StructureEntity {
    StructureEntity::new(
        "minecraft:chest_minecart",
        [f64::from(block[0]) + 0.5, f64::from(block[1]) + 0.5, f64::from(block[2]) + 0.5],
        0.0,
        0.0,
    )
    .with("LootTable", Nbt::String(table.to_owned()))
    .with("LootTableSeed", Nbt::Long(seed))
}

/// The yaw a template entity ends with: its stored yaw turned by `rotation`, then reflected by
/// `mirror`.
fn placed_yaw(stored: f32, rotation: Rotation, mirror: Mirror) -> f32 {
    let angle = wrap_degrees(stored);
    let rotated = match rotation {
        Rotation::Cw180 => angle + 180.0,
        Rotation::Ccw90 => angle + 270.0,
        Rotation::Cw90 => angle + 90.0,
        Rotation::None => angle,
    };
    let mirrored = match mirror {
        Mirror::FrontBack => -angle,
        Mirror::LeftRight => 180.0 - angle,
        Mirror::None => angle,
    };
    rotated + mirrored - stored
}

fn wrap_degrees(angle: f32) -> f32 {
    let mut wrapped = angle % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

/// A template-local entity position carried through `mirror` and `rotation` about `pivot`. A
/// position is a point in the cell, so a reflection is `1 - v` rather than `-v`.
fn transform_point(pos: [f64; 3], mirror: Mirror, rotation: Rotation, pivot: [i32; 3]) -> [f64; 3] {
    let [mut x, y, mut z] = pos;
    let mut mirrored = true;
    match mirror {
        Mirror::LeftRight => z = 1.0 - z,
        Mirror::FrontBack => x = 1.0 - x,
        Mirror::None => mirrored = false,
    }
    let (px, pz) = (f64::from(pivot[0]), f64::from(pivot[2]));
    match rotation {
        Rotation::Ccw90 => [px - pz + z, y, px + pz + 1.0 - x],
        Rotation::Cw90 => [px + pz + 1.0 - z, y, pz - px + x],
        Rotation::Cw180 => [px + px + 1.0 - x, y, pz + pz + 1.0 - z],
        Rotation::None if mirrored => [x, y, z],
        Rotation::None => pos,
    }
}

/// One template entity placed at `position`, or `None` when its block cell is outside `keep`.
pub(super) fn place_template_entity(
    entity: &super::template::TemplateEntity,
    position: [i32; 3],
    settings: &PlaceSettings,
    keep: &mut dyn FnMut([i32; 3]) -> bool,
) -> Option<StructureEntity> {
    let cell = super::template::transform(entity.block_pos(), settings.mirror, settings.rotation, settings.pivot);
    let cell = [cell[0] + position[0], cell[1] + position[1], cell[2] + position[2]];
    if !keep(cell) {
        return None;
    }
    let rel = transform_point(entity.pos(), settings.mirror, settings.rotation, settings.pivot);
    let pos = [rel[0] + f64::from(position[0]), rel[1] + f64::from(position[1]), rel[2] + f64::from(position[2])];
    let mut placed = StructureEntity { nbt: entity.nbt().to_vec() };
    let (stored_yaw, pitch) = match placed.get("Rotation") {
        Some(Nbt::List { elements, .. }) => {
            let at = |i: usize| match elements.get(i) {
                Some(Nbt::Float(v)) => *v,
                _ => 0.0,
            };
            (at(0), at(1))
        }
        _ => (0.0, 0.0),
    };
    placed.set("Pos", doubles(pos));
    placed.set("Rotation", floats([placed_yaw(stored_yaw, settings.rotation, settings.mirror), pitch]));
    placed.nbt.retain(|(name, _)| name != "UUID");
    if placed.get("block_pos").is_some() {
        placed.set("block_pos", Nbt::IntArray(cell.to_vec()));
    }
    Some(placed)
}

/// What a structure's data-marker handler does besides spawn: the block it leaves at the marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerBlock {
    /// Air.
    Air,
    /// Water.
    Water,
}

/// A data marker's outcome: the entities it spawns and the block it overwrites itself with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkerSpawn {
    /// Entities spawned at the marker, in spawn order.
    pub entities: Vec<StructureEntity>,
    /// The block written over the marker once something spawned.
    pub block: Option<MarkerBlock>,
}

/// The entity work a structure's own handler does for a data marker. Markers that only decorate
/// a chest, or that a structure does not interpret, spawn nothing.
///
/// `rotation` is the placing piece's rotation, `sea_level` the dimension's, and `roll` a value in
/// `0..3` standing in for the handler's group-size draw (the reference draws it from an unseeded
/// generator, so no value is the reference's).
#[must_use]
pub fn handle_marker(structure: &str, marker: &super::template::DataMarker, rotation: Rotation, sea_level: i32, roll: i32) -> MarkerSpawn {
    let [x, y, z] = marker.pos;
    match (structure, marker.metadata.as_str()) {
        ("minecraft:ocean_ruin_cold" | "minecraft:ocean_ruin_warm", "drowned") => MarkerSpawn {
            entities: vec![mob_at_block("minecraft:drowned", marker.pos)],
            block: Some(if y > sea_level { MarkerBlock::Air } else { MarkerBlock::Water }),
        },
        ("minecraft:woodland_mansion", id @ ("Mage" | "Warrior" | "Group of Allays")) => {
            let (kind, count) = match id {
                "Mage" => ("minecraft:evoker", 1),
                "Warrior" => ("minecraft:vindicator", 1),
                _ => ("minecraft:allay", roll.clamp(0, 2) + 1),
            };
            MarkerSpawn {
                entities: (0..count).map(|_| mob_at_block(kind, marker.pos)).collect(),
                block: Some(MarkerBlock::Air),
            }
        }
        ("minecraft:end_city", id) if id.starts_with("Sentry") => MarkerSpawn {
            entities: vec![StructureEntity::new(
                "minecraft:shulker",
                [f64::from(x) + 0.5, f64::from(y), f64::from(z) + 0.5],
                0.0,
                0.0,
            )],
            block: None,
        },
        ("minecraft:end_city", id) if id.starts_with("Elytra") => MarkerSpawn {
            entities: vec![elytra_frame(marker.pos, rotation)],
            block: None,
        },
        _ => MarkerSpawn::default(),
    }
}

/// An item frame on the cell `block` holding an elytra, facing south turned by `rotation`.
fn elytra_frame(block: [i32; 3], rotation: Rotation) -> StructureEntity {
    // North, south, west, east in the 3D data order after down and up.
    let facing: i8 = match rotation {
        Rotation::None => 3,
        Rotation::Cw90 => 4,
        Rotation::Cw180 => 2,
        Rotation::Ccw90 => 5,
    };
    let yaw = match facing {
        3 => 0.0,
        4 => 90.0,
        2 => 180.0,
        _ => 270.0,
    };
    StructureEntity::new(
        "minecraft:item_frame",
        [f64::from(block[0]) + 0.5, f64::from(block[1]) + 0.5, f64::from(block[2]) + 0.5],
        yaw,
        0.0,
    )
    .with("Facing", Nbt::Byte(facing))
    .with("block_pos", Nbt::IntArray(block.to_vec()))
    .with("Item", Nbt::Compound(vec![("id".to_owned(), Nbt::String("minecraft:elytra".to_owned())), ("count".to_owned(), Nbt::Int(1))]))
    .with("ItemRotation", Nbt::Byte(0))
}

/// A group-size roll in `0..3` for the marker at `pos`, seeded from the world seed and the
/// position so a regenerated chunk repeats it.
#[must_use]
pub fn marker_roll(seed: i64, pos: [i32; 3]) -> i32 {
    use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};
    LegacyRandomSource::new(seed ^ get_seed(pos[0], pos[1], pos[2])).next_int_bounded(3)
}
