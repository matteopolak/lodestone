//! Respawn anchor: charging, setting a spawn, the bad-dimension blast, and the
//! stand-up search a respawn uses.
//!
//! # What it is
//!
//! A respawn anchor holds 0 to 4 charges (the `charges` block property). A
//! glowstone block adds one. Using a charged anchor where the dimension allows
//! it (the Nether) makes it the player's respawn point; using one anywhere else
//! destroys it in a power-5 fiery blast. Dying with a charged anchor as the
//! respawn point spends one charge and places the player on a safe cell beside
//! it.
//!
//! # How it works
//!
//! * [`decide_use`] is the pure decision for a right-click: it names which of
//!   [`AnchorUse`]'s outcomes applies from the clicked state, the dimension, the
//!   held items and the existing respawn point. `crate::server` performs the
//!   writes, packets and sounds for each outcome.
//! * [`find_stand_up`] walks 25 cells around the anchor in a fixed order (eight
//!   horizontal neighbours, the same eight one below, one above, then the cell
//!   straight above) and takes the first where a standing player fits. It runs
//!   twice, first rejecting hazardous cells (fire, lava, magma, lit campfires,
//!   cacti, berry bushes, wither roses, powder snow) and then accepting them.
//! * [`respawn_at`] is the respawn-time read: the block must still be an anchor
//!   with a charge, the dimension must still allow it, and a stand-up cell must
//!   exist. Only then is a charge spent.
//!
//! # How to change it
//!
//! Which dimensions allow an anchor is [`works_in`]; add a dimension there. The
//! blast is queued by `MobSim::queue_blast` (`crate::mobs`), and its fire pass
//! lives with the other block effects of a blast in `crate::block_drops`.
//!
//! # Not modelled
//!
//! * A respawn into a different dimension than the one the player died in: the
//!   anchor is honoured only when the player dies in its own dimension, because
//!   nothing here moves a connection to a dimension it is not already in.
//! * The world-border test of a stand-up cell, the water-resistance override of
//!   an anchor blasted next to water, and the dispenser's glowstone behaviour.

use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, Vec3};

use crate::chunk::ChunkSource;
use crate::dimension::Dimension;

/// The most charges an anchor holds.
pub(crate) const MAX_CHARGES: u8 = 4;

/// Power of the blast an anchor makes when used where it does not work.
pub(crate) const BLAST_POWER: f32 = 5.0;

/// Whether `state` is a respawn anchor.
#[must_use]
pub(crate) fn is_anchor(state: StateId) -> bool {
    state.block() == Block::RespawnAnchor
}

/// The `charges` value of an anchor state, `0` for anything else.
#[must_use]
pub(crate) fn charges(state: StateId) -> u8 {
    if !is_anchor(state) {
        return 0;
    }
    state
        .properties()
        .iter()
        .find_map(|(key, value)| (*key == "charges").then(|| value.parse::<u8>().ok()).flatten())
        .unwrap_or(0)
}

/// The anchor state holding `charges` (clamped to [`MAX_CHARGES`]).
#[must_use]
pub(crate) fn with_charges(charges: u8) -> StateId {
    let charges = charges.min(MAX_CHARGES);
    StateId::from_state_str(&format!("minecraft:respawn_anchor[charges={charges}]"))
        .expect("every respawn anchor charge level is a generated state")
}

/// Whether an anchor can set a respawn point in `dimension`.
#[must_use]
pub(crate) fn works_in(dimension: Dimension) -> bool {
    dimension.respawn_anchor_works()
}

/// Whether water would fill the cell at `pos` once the anchor there is gone:
/// water directly above, or a source or spreading flow beside it. A blast that
/// goes off in such a cell is smothered and leaves the terrain alone.
#[must_use]
pub(crate) fn smothered_by_water<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> bool {
    use crate::fluid::{FluidKind, fluid_state_of_id};
    let water = |x: i32, y: i32, z: i32| {
        fluid_state_of_id(source.block_state_id(x, y, z)).filter(|fluid| fluid.kind == FluidKind::Water)
    };
    if water(pos.x, pos.y + 1, pos.z).is_some() {
        return true;
    }
    [(1, 0), (-1, 0), (0, 1), (0, -1)].into_iter().any(|(dx, dz)| {
        water(pos.x + dx, pos.y, pos.z + dz)
            .is_some_and(|fluid| fluid.is_source() || fluid.falling || fluid.amount >= 2)
    })
}

/// What a right-click on an anchor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchorUse {
    /// The click is not this block's: carry on with the ordinary placement and
    /// use rules (an empty anchor with a non-glowstone item, or a sneaking
    /// player holding something).
    FallThrough,
    /// Handled with no effect: the main hand defers to a glowstone block in the
    /// off hand, whose own click does the charging.
    Defer,
    /// Add one charge and consume one glowstone block.
    Charge,
    /// The anchor does not work here: remove it and blast.
    Explode,
    /// Make it the respawn point.
    SetSpawn,
    /// It already is the respawn point: handled, silent.
    AlreadySet,
}

/// The inputs [`decide_use`] reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct UseContext {
    /// The clicked anchor's charges.
    pub charges: u8,
    /// Whether the dimension allows an anchor ([`works_in`]).
    pub works_here: bool,
    /// Whether the hand that clicked holds a glowstone block.
    pub clicking_hand_glowstone: bool,
    /// Whether the clicking hand is the main hand.
    pub main_hand: bool,
    /// Whether the off hand holds a glowstone block.
    pub off_hand_glowstone: bool,
    /// Whether the player is sneaking while holding anything in either hand,
    /// which skips block use entirely.
    pub sneaking_with_item: bool,
    /// Whether the player's respawn point is already this anchor.
    pub already_this_point: bool,
}

/// Decides what a click on an anchor does. Order matters: fuel is tried
/// first, then the empty-hand rule.
#[must_use]
pub(crate) fn decide_use(cx: UseContext) -> AnchorUse {
    if cx.sneaking_with_item {
        return AnchorUse::FallThrough;
    }
    let can_charge = cx.charges < MAX_CHARGES;
    if cx.clicking_hand_glowstone && can_charge {
        return AnchorUse::Charge;
    }
    if cx.main_hand && cx.off_hand_glowstone && can_charge {
        return AnchorUse::Defer;
    }
    if cx.charges == 0 {
        return AnchorUse::FallThrough;
    }
    if !cx.works_here {
        return AnchorUse::Explode;
    }
    if cx.already_this_point {
        AnchorUse::AlreadySet
    } else {
        AnchorUse::SetSpawn
    }
}

/// The 25 cells a stand-up search visits, as offsets from the anchor, in order.
const HORIZONTAL: [(i32, i32); 8] = [
    (0, -1),
    (-1, 0),
    (0, 1),
    (1, 0),
    (-1, -1),
    (1, -1),
    (-1, 1),
    (1, 1),
];

fn stand_up_offsets() -> impl Iterator<Item = (i32, i32, i32)> {
    let level = HORIZONTAL.into_iter().map(|(dx, dz)| (dx, 0, dz));
    let below = HORIZONTAL.into_iter().map(|(dx, dz)| (dx, -1, dz));
    let above = HORIZONTAL.into_iter().map(|(dx, dz)| (dx, 1, dz));
    level.chain(below).chain(above).chain(std::iter::once((0, 1, 0)))
}

/// A cell a player is hurt by standing in or beside.
fn is_hazard(state: StateId) -> bool {
    match state.block() {
        Block::Fire
        | Block::SoulFire
        | Block::Lava
        | Block::MagmaBlock
        | Block::LavaCauldron
        | Block::WitherRose
        | Block::SweetBerryBush
        | Block::Cactus
        | Block::PowderSnow => true,
        Block::Campfire | Block::SoulCampfire => state
            .properties()
            .iter()
            .any(|(key, value)| *key == "lit" && *value == "true"),
        _ => false,
    }
}

/// Cells whose shape does not count as a floor or an obstacle: ladders and
/// vines a player climbs, and open trapdoors.
fn is_non_climbable_exempt(state: StateId) -> bool {
    let climbable = matches!(
        state.block(),
        Block::Ladder
            | Block::Vine
            | Block::Scaffolding
            | Block::WeepingVines
            | Block::WeepingVinesPlant
            | Block::TwistingVines
            | Block::TwistingVinesPlant
            | Block::CaveVines
            | Block::CaveVinesPlant
    );
    let open_trapdoor = state.block().name().ends_with("_trapdoor")
        && state
            .properties()
            .iter()
            .any(|(key, value)| *key == "open" && *value == "true");
    climbable || open_trapdoor
}

/// The collision boxes a dismounting body sees in a cell.
fn shape(state: StateId) -> &'static [lodestone_data::collision_shapes::Aabb] {
    if is_non_climbable_exempt(state) {
        &[]
    } else {
        lodestone_data::collision_shapes::collision_boxes(state)
    }
}

fn top_of(boxes: &[lodestone_data::collision_shapes::Aabb]) -> Option<f64> {
    boxes
        .iter()
        .map(|b| f64::from(b.max[1]))
        .max_by(f64::total_cmp)
}

/// The standing height inside `pos`: the cell's own top when it has a shape, or
/// one below the top of the cell underneath when that reaches the floor line.
/// `None` when there is nothing to stand on or the floor is a full block's top
/// or higher.
fn floor_height<S: ChunkSource + ?Sized>(source: &S, pos: BlockPos) -> Option<f64> {
    let own = shape(source.block_state_id(pos.x, pos.y, pos.z));
    let height = match top_of(own) {
        Some(top) => top,
        None => {
            let below = top_of(shape(source.block_state_id(pos.x, pos.y - 1, pos.z)))?;
            if below >= 1.0 {
                below - 1.0
            } else {
                return None;
            }
        }
    };
    (height < 1.0).then_some(height)
}

/// Whether a standing player at `feet` overlaps any collision shape. Fluids are
/// not obstacles here, only shapes are.
fn body_collides<S: ChunkSource + ?Sized>(source: &S, feet: Vec3) -> bool {
    const HALF_WIDTH: f64 = 0.3;
    const HEIGHT: f64 = 1.8;
    let (min_x, max_x) = (feet.x - HALF_WIDTH, feet.x + HALF_WIDTH);
    let (min_y, max_y) = (feet.y, feet.y + HEIGHT);
    let (min_z, max_z) = (feet.z - HALF_WIDTH, feet.z + HALF_WIDTH);
    for x in min_x.floor() as i32..max_x.ceil() as i32 {
        for y in min_y.floor() as i32..max_y.ceil() as i32 {
            for z in min_z.floor() as i32..max_z.ceil() as i32 {
                for b in lodestone_data::collision_shapes::collision_boxes(
                    source.block_state_id(x, y, z),
                ) {
                    let overlaps = min_x < f64::from(x) + f64::from(b.max[0])
                        && max_x > f64::from(x) + f64::from(b.min[0])
                        && min_y < f64::from(y) + f64::from(b.max[1])
                        && max_y > f64::from(y) + f64::from(b.min[1])
                        && min_z < f64::from(z) + f64::from(b.max[2])
                        && max_z > f64::from(z) + f64::from(b.min[2]);
                    if overlaps {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// The feet position of a safe standing spot in the cell `pos`, or `None`.
fn safe_spot<S: ChunkSource + ?Sized>(
    source: &S,
    pos: BlockPos,
    avoid_hazards: bool,
) -> Option<Vec3> {
    let here = source.block_state_id(pos.x, pos.y, pos.z);
    if avoid_hazards && is_hazard(here) {
        return None;
    }
    let floor = floor_height(source, pos)?;
    if avoid_hazards
        && floor <= 0.0
        && is_hazard(source.block_state_id(pos.x, pos.y - 1, pos.z))
    {
        return None;
    }
    let feet = Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + floor, f64::from(pos.z) + 0.5);
    if body_collides(source, feet) {
        return None;
    }
    let portal_like = |p: BlockPos| {
        matches!(
            source.block_state_id(p.x, p.y, p.z).block(),
            Block::EndPortal | Block::EndGateway
        )
    };
    if portal_like(pos) || portal_like(BlockPos::new(pos.x, pos.y + 1, pos.z)) {
        return None;
    }
    Some(feet)
}

/// The first safe standing spot around the anchor at `anchor`, preferring cells
/// with no hazard.
#[must_use]
pub(crate) fn find_stand_up<S: ChunkSource + ?Sized>(source: &S, anchor: BlockPos) -> Option<Vec3> {
    for avoid_hazards in [true, false] {
        for (dx, dy, dz) in stand_up_offsets() {
            let cell = BlockPos::new(anchor.x + dx, anchor.y + dy, anchor.z + dz);
            if let Some(feet) = safe_spot(source, cell, avoid_hazards) {
                return Some(feet);
            }
        }
    }
    None
}

/// A respawn resolved at an anchor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AnchorRespawn {
    /// Where the player's feet go.
    pub feet: Vec3,
    /// The anchor cell, whose charge was spent.
    pub anchor: BlockPos,
    /// The anchor's state before the spend.
    pub before: StateId,
    /// The anchor's state after the spend.
    pub after: StateId,
}

/// Stands a player beside the anchor at `pos`, spending one charge when
/// `consume` is set. The caller has already decided the anchor may be used (it
/// has a charge or the point is forced, and the dimension allows it). `None`
/// when there is nowhere to stand, in which case nothing is spent.
pub(crate) fn respawn_at<S: ChunkSource + ?Sized>(
    source: &S,
    pos: BlockPos,
    consume: bool,
) -> Option<AnchorRespawn> {
    let before = source.block_state_id(pos.x, pos.y, pos.z);
    let feet = find_stand_up(source, pos)?;
    let after = if consume { with_charges(charges(before).saturating_sub(1)) } else { before };
    if consume {
        source.set_block(pos.x, pos.y, pos.z, after);
    }
    Some(AnchorRespawn { feet, anchor: pos, before, after })
}

/// The system-chat line shown when an anchor becomes the respawn point.
pub(crate) const RESPAWN_SET_MESSAGE: &str = "Respawn point set";

/// The depletion sound a respawning player hears, positioned at the anchor
/// block's own corner coordinates rather than its centre.
#[must_use]
pub(crate) fn deplete_sound(pos: BlockPos) -> crate::effects::WorldEffect {
    let crate::effects::WorldEffect::Sound { sound, category, volume, pitch, seed, .. } =
        block_sound("deplete", pos)
    else {
        unreachable!("block_sound builds a sound")
    };
    crate::effects::WorldEffect::Sound {
        sound,
        category,
        pos: Vec3::new(f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)),
        volume,
        pitch,
        seed,
    }
}

/// A block sound at the centre of `pos`, in the block category, at full volume
/// and pitch.
#[must_use]
pub(crate) fn block_sound(name: &str, pos: BlockPos) -> crate::effects::WorldEffect {
    crate::effects::WorldEffect::Sound {
        sound: format!("minecraft:block.respawn_anchor.{name}"),
        category: lodestone_model::SoundCategory::Block,
        pos: Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.5, f64::from(pos.z) + 0.5),
        volume: 1.0,
        pitch: 1.0,
        seed: i64::from(pos.x) ^ (i64::from(pos.y) << 20) ^ (i64::from(pos.z) << 40),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A sparse block world: air everywhere but the cells inserted.
    struct Cells {
        cells: Mutex<HashMap<(i32, i32, i32), StateId>>,
        dimension: Dimension,
    }

    impl Cells {
        fn new(dimension: Dimension) -> Self {
            Self { cells: Mutex::new(HashMap::new()), dimension }
        }

        fn put(&self, x: i32, y: i32, z: i32, name: &str) {
            let state = StateId::from_state_str(name).expect("state");
            self.cells.lock().unwrap().insert((x, y, z), state);
        }

        /// A stone slab of floor at y = 63 under a 9x9 area, and an anchor at
        /// (0, 64, 0) with `charges`.
        fn floor_with_anchor(dimension: Dimension, charges: u8) -> Self {
            let world = Self::new(dimension);
            for x in -4..=4 {
                for z in -4..=4 {
                    world.put(x, 63, z, "minecraft:stone");
                }
            }
            world.put(0, 64, 0, &format!("minecraft:respawn_anchor[charges={charges}]"));
            world
        }
    }

    impl ChunkSource for Cells {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            crate::chunk::ChunkColumn::new(0, 256)
        }
        fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
            self.cells.lock().unwrap().get(&(x, y, z)).copied().unwrap_or_else(crate::chunk::air_state)
        }
        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_owned()
        }
        fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
            self.cells.lock().unwrap().insert((x, y, z), state);
        }
        fn dimension(&self) -> Option<Dimension> {
            Some(self.dimension)
        }
    }

    fn cx() -> UseContext {
        UseContext {
            charges: 2,
            works_here: true,
            clicking_hand_glowstone: false,
            main_hand: true,
            off_hand_glowstone: false,
            sneaking_with_item: false,
            already_this_point: false,
        }
    }

    #[test]
    fn charge_levels_round_trip_through_the_block_property() {
        for level in 0..=MAX_CHARGES {
            assert_eq!(charges(with_charges(level)), level);
        }
        assert_eq!(charges(with_charges(9)), MAX_CHARGES, "clamped");
        assert_eq!(charges(crate::chunk::air_state()), 0);
    }

    #[test]
    fn glowstone_charges_until_full_then_the_empty_hand_rules_apply() {
        let fuel = UseContext { clicking_hand_glowstone: true, ..cx() };
        assert_eq!(decide_use(UseContext { charges: 0, ..fuel }), AnchorUse::Charge);
        assert_eq!(decide_use(UseContext { charges: 3, ..fuel }), AnchorUse::Charge);
        // Full: glowstone no longer charges, so it is an ordinary use.
        assert_eq!(decide_use(UseContext { charges: 4, ..fuel }), AnchorUse::SetSpawn);
    }

    #[test]
    fn the_main_hand_defers_to_fuel_in_the_off_hand() {
        let base = UseContext { off_hand_glowstone: true, ..cx() };
        assert_eq!(decide_use(base), AnchorUse::Defer);
        assert_eq!(decide_use(UseContext { main_hand: false, clicking_hand_glowstone: true, ..base }), AnchorUse::Charge);
        assert_eq!(decide_use(UseContext { charges: 4, ..base }), AnchorUse::SetSpawn, "full anchors do not defer");
    }

    #[test]
    fn using_a_charged_anchor_sets_the_spawn_in_the_nether_and_blasts_elsewhere() {
        assert_eq!(decide_use(cx()), AnchorUse::SetSpawn);
        assert_eq!(decide_use(UseContext { already_this_point: true, ..cx() }), AnchorUse::AlreadySet);
        assert_eq!(decide_use(UseContext { works_here: false, ..cx() }), AnchorUse::Explode);
        // An empty anchor never blasts or sets: the click falls through.
        assert_eq!(decide_use(UseContext { charges: 0, works_here: false, ..cx() }), AnchorUse::FallThrough);
        assert_eq!(decide_use(UseContext { charges: 0, ..cx() }), AnchorUse::FallThrough);
    }

    #[test]
    fn sneaking_with_an_item_skips_the_block() {
        let fuel = UseContext { clicking_hand_glowstone: true, sneaking_with_item: true, ..cx() };
        assert_eq!(decide_use(fuel), AnchorUse::FallThrough);
    }

    #[test]
    fn works_only_in_the_nether() {
        assert!(works_in(Dimension::Nether));
        assert!(!works_in(Dimension::Overworld));
        assert!(!works_in(Dimension::End));
    }

    /// The first cell tried is the one straight north of the anchor; with a
    /// clear floor it is chosen, standing on the floor at the cell's centre.
    #[test]
    fn the_stand_up_search_starts_north_of_the_anchor() {
        let world = Cells::floor_with_anchor(Dimension::Nether, 2);
        let feet = find_stand_up(&world, BlockPos::new(0, 64, 0)).expect("a clear spot");
        assert_eq!(feet, Vec3::new(0.5, 64.0, -0.5));
    }

    /// Blocking the north and west cells with stone moves the choice to the
    /// third cell in order, south; each block is a control for the one before.
    #[test]
    fn blocked_cells_are_skipped_in_order() {
        let world = Cells::floor_with_anchor(Dimension::Nether, 2);
        world.put(0, 64, -1, "minecraft:stone");
        let after_north = find_stand_up(&world, BlockPos::new(0, 64, 0)).expect("spot");
        assert_eq!(after_north, Vec3::new(-0.5, 64.0, 0.5), "west is second in order");
        world.put(-1, 64, 0, "minecraft:stone");
        let after_west = find_stand_up(&world, BlockPos::new(0, 64, 0)).expect("spot");
        assert_eq!(after_west, Vec3::new(0.5, 64.0, 1.5), "south is third in order");
    }

    /// A lava cell is skipped by the safe pass even though it is first in
    /// order; boxed in on every other side, the hazardous pass takes it.
    #[test]
    fn hazards_are_avoided_until_nothing_else_fits() {
        let world = Cells::floor_with_anchor(Dimension::Nether, 2);
        world.put(0, 64, -1, "minecraft:fire");
        let feet = find_stand_up(&world, BlockPos::new(0, 64, 0)).expect("spot");
        assert_eq!(feet, Vec3::new(-0.5, 64.0, 0.5), "fire at north pushes the choice west");
        for (dx, dz) in HORIZONTAL {
            world.put(dx, 64, dz, "minecraft:stone");
        }
        for (dx, dz) in HORIZONTAL {
            world.put(dx, 63, dz, "minecraft:stone");
            world.put(dx, 65, dz, "minecraft:stone");
        }
        world.put(0, 65, 0, "minecraft:stone");
        world.put(0, 64, -1, "minecraft:fire");
        // Everything else is solid, so nothing fits at all: the anchor is walled in.
        assert_eq!(find_stand_up(&world, BlockPos::new(0, 64, 0)), None);
    }

    #[test]
    fn a_respawn_spends_one_charge_and_reports_the_spot() {
        let world = Cells::floor_with_anchor(Dimension::Nether, 3);
        let respawn = respawn_at(&world, BlockPos::new(0, 64, 0), true).expect("charged anchor");
        assert_eq!(respawn.feet, Vec3::new(0.5, 64.0, -0.5));
        assert_eq!(charges(respawn.before), 3);
        assert_eq!(charges(respawn.after), 2);
        assert_eq!(charges(world.block_state_id(0, 64, 0)), 2, "the world was written");
    }

    /// Walled in on every side, the anchor yields no respawn and keeps its charge.
    #[test]
    fn a_walled_in_anchor_keeps_its_charge() {
        let world = Cells::floor_with_anchor(Dimension::Nether, 2);
        for (dx, dy, dz) in stand_up_offsets() {
            world.put(dx, 64 + dy, dz, "minecraft:stone");
        }
        assert!(respawn_at(&world, BlockPos::new(0, 64, 0), true).is_none());
        assert_eq!(charges(world.block_state_id(0, 64, 0)), 2);
    }

    #[test]
    fn the_deplete_sound_is_the_registered_anchor_sound_at_the_block_centre() {
        let crate::effects::WorldEffect::Sound { sound, pos, .. } =
            deplete_sound(BlockPos::new(4, 70, -3))
        else {
            panic!("a sound");
        };
        assert_eq!(sound, "minecraft:block.respawn_anchor.deplete");
        assert_eq!(pos, Vec3::new(4.0, 70.0, -3.0), "the block's corner, not its centre");
    }
}
