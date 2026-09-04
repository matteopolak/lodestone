//! Local placement prediction, split out of `sim.rs` into its
//! own module for the same reason `sim/tests.rs` was: this is the most
//! self-contained ~600-line block in the file — every item here is a pure
//! function or a plain data type, none of it touches `Sim` state — so it is
//! the least risky semantic move available. See
//! [`docs/block-placement-prediction.md`](../../../../docs/block-placement-prediction.md).
//!
//! Re-exported into `sim`'s own namespace (`pub(crate) use placement::{...}`
//! in `sim.rs`) so every existing call site elsewhere in that file, and in
//! `sim/tests.rs`'s `use super::*;`, keeps compiling unqualified.
//! [`predicted_placement_state`]/[`write_predicted_block`] are re-exported
//! with `pub use` specifically: both are referenced by their original
//! `crate::sim::`/`lodestone::sim::` path from `block_entities.rs` and from
//! `tests/placed_chest_block_entity_pixels.rs` (an external integration
//! test), and only a `pub use` preserves that path through the move.
//!
//! **Two more items joined later, for `PlaceIntent` (`docs/plugin-api.md`):**
//! [`placement_facts`] and [`block_intersects_player`] used to be `Sim`
//! methods in `sim/actions.rs`, reading `self.net`/`self.player()` directly.
//! `crate::interact::drive_placement` is a `GameTick` **system** — a free
//! function over `&mut World`, not a `Sim` method — so both became free
//! functions parameterised over the two reads (a block-state lookup, a
//! player-intersection test) instead. `Sim::use_item_live` was re-pointed at
//! them rather than kept on a separate, parallel path.

use lodestone_client::BlockPos;
use lodestone_game::placement::{Axis, Half, OrientationKind, PlacedState, PlacementWorld};
use lodestone_model::BlockFace;
use lodestone_physics::Aabb;
use lodestone_world::{BlockEntitySync, WorldSink};

// ---------------------------------------------------------------------------
// Local placement prediction
//
// `use_item_live` used to send `use_item_on` and wait: `Placement` is a
// *decision* machine and nothing wrote the world, so a placed block — a chest
// especially, since that fix made a state write create its block entity — was a hole
// for one server round trip. Everything below is what turns that decision into a
// local write. See `docs/block-placement-prediction.md`.
// ---------------------------------------------------------------------------

/// The world facts [`lodestone_game::placement::Placement::use_on`] asks for, **read once, before the
/// decision runs** rather than from inside it.
///
/// [`PlacementWorld`] is queried re-entrantly by `use_on`, and every answer needs
/// the chunk store's read lock — while `use_on` itself needs the ECS write guard
/// (it mutates the [`crate::interact::PlacementPredictor`] resource). Answering live would nest
/// those two guards, which is the `chunks → World` order `EcsHandle`'s rule 3
/// exists to forbid. Precomputing keeps the guards disjoint *and* makes the whole
/// decision hermetically testable, with no `Sim` and no server.
///
/// `use_on` asks exactly four questions over two positions:
/// `is_replaceable(clicked)` (which picks the target), then
/// `is_replaceable(target)` / `is_obstructed(target)` (legality) and
/// `is_interactable(clicked)`. Any other position answers conservatively — not
/// replaceable, not interactable — which can only make the shell predict *less*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlacementFacts {
    /// The block the ray hit.
    pub(crate) clicked: BlockPos,
    /// Where a placement would land: `clicked` itself when it is replaceable,
    /// otherwise the cell across the hit face. Same rule as
    /// [`lodestone_game::placement::resolve_target`], evaluated here because it
    /// needs the same world read.
    pub(crate) target: BlockPos,
    pub(crate) clicked_replaceable: bool,
    pub(crate) clicked_interactable: bool,
    pub(crate) target_replaceable: bool,
    pub(crate) target_obstructed: bool,
}

impl PlacementWorld for PlacementFacts {
    fn is_replaceable(&self, pos: BlockPos) -> bool {
        if pos == self.clicked {
            self.clicked_replaceable
        } else if pos == self.target {
            self.target_replaceable
        } else {
            false
        }
    }

    fn is_interactable(&self, pos: BlockPos) -> bool {
        pos == self.clicked && self.clicked_interactable
    }

    fn is_obstructed(&self, pos: BlockPos) -> bool {
        pos == self.target && self.target_obstructed
    }
}

/// Build [`PlacementFacts`] for one right-click, parameterised over the
/// block-state lookup and the player-intersection test rather than reading
/// `Sim` directly.
///
/// Moved out of `Sim::placement_facts` (`sim/actions.rs`) for
/// `crate::interact::drive_placement`: a `GameTick` **system** has a
/// `NetHandle` resource and a `&PhysicsState` component, not a `Sim`, so the
/// two reads this needs — a block-state-by-position lookup and "does this
/// cell overlap the player" — are taken as closures instead. `Sim::use_item_live`
/// (the human path) now calls this too, with closures that read `self.net`/
/// `self.player()` — a pure move, not a second implementation: this is the
/// *same* four-question resolution [`PlacementFacts`]'s own docs describe,
/// unchanged.
pub(crate) fn placement_facts(
    clicked: BlockPos,
    face: BlockFace,
    state_at: impl Fn(BlockPos) -> Option<u32>,
    intersects_player: impl Fn(BlockPos) -> bool,
) -> PlacementFacts {
    let clicked_state = state_at(clicked);
    let clicked_replaceable = clicked_state.is_some_and(is_air_state);
    // `resolve_target`'s rule, evaluated here because it is the same read: a
    // replaceable clicked cell is replaced in place, otherwise the placement
    // goes to the cell across the hit face.
    let target = if clicked_replaceable {
        clicked
    } else {
        lodestone_game::placement::offset(clicked, face)
    };
    PlacementFacts {
        clicked,
        target,
        clicked_replaceable,
        clicked_interactable: clicked_state.is_some_and(is_interactable_state),
        // An unloaded column reads `None` and therefore "not replaceable",
        // which declines the prediction — the same conservative direction as
        // every other unknown here.
        target_replaceable: state_at(target).is_some_and(is_air_state),
        target_obstructed: intersects_player(target),
    }
}

/// Whether `block` overlaps the player's own bounding box — vanilla's
/// placement-legality rule (a `BlockItem` cannot place a block that would
/// intersect the placer). A pure function of the box and the cell, moved out
/// of `Sim::block_intersects_player` (`sim/actions.rs`) for the same reason as
/// [`placement_facts`] above: `crate::interact::drive_placement` has a
/// `Profile` resource and a `&PhysicsState` component to build `bb` from, but
/// no `Sim`.
#[must_use]
pub(crate) fn block_intersects_player(bb: &Aabb, block: [i32; 3]) -> bool {
    let (x0, y0, z0) = (
        f64::from(block[0]),
        f64::from(block[1]),
        f64::from(block[2]),
    );
    bb.max_x > x0
        && bb.min_x < x0 + 1.0
        && bb.max_y > y0
        && bb.min_y < y0 + 1.0
        && bb.max_z > z0
        && bb.min_z < z0 + 1.0
}

/// Whether a block state is one the client may place *into*.
///
/// Deliberately only the three air blocks, not vanilla's full
/// `BlockState.canBeReplaced` set (water, lava, tall grass, snow layers, …):
/// that set is per-block-state registry data no census in this tree carries, and
/// guessing it would make the shell predict placements the server then refuses.
/// Narrowing it costs nothing but a *missing* prediction — i.e. today's
/// behaviour, a one-round-trip wait — for the cases it excludes, and it is what
/// makes the `waterlogged = false` rule in [`state_for_placement`] exact rather
/// than assumed.
pub(crate) fn is_air_state(state: u32) -> bool {
    matches!(
        lodestone_data::block_states::block_name(state),
        Some("minecraft:air" | "minecraft:cave_air" | "minecraft:void_air")
    )
}

/// Name fragments of blocks whose right-click **actuates** them, for the
/// place-vs-interact question `use_on` asks first.
///
/// This is an over-approximation on purpose, and the asymmetry is the whole
/// design: calling an inert block interactable only *suppresses* a prediction
/// (the shell falls back to sending and waiting, exactly today's behaviour),
/// while calling an interactable block inert makes the shell predict a block into
/// the cell next to the chest you meant to open. So the list errs long, and every
/// block that owns a block entity is treated as interactable regardless of
/// whether it appears here — which covers every container in the game through
/// [`lodestone_data::block_entity_types`]' census rather than through this list.
///
/// Vanilla asks `BlockState.useItemOn`/`useWithoutItem` — real per-block
/// behaviour with no census anywhere in this tree. A mislabelled block costs one
/// round trip either way, because the server re-sends the block state at *both*
/// candidate positions after every `use_item_on` (see [`super::Sim::use_item_live`]).
const INTERACTABLE_FRAGMENTS: &[&str] = &[
    "_door",
    "_trapdoor",
    "_fence_gate",
    "_button",
    "_bed",
    "_sign",
    "_shelf",
    "_head",
    "_skull",
    "candle",
    "cauldron",
    "anvil",
    "_pot",
    "note_block",
    "lever",
    "_table",
    "grindstone",
    "loom",
    "stonecutter",
    "repeater",
    "comparator",
    "daylight_detector",
    "cake",
    "composter",
    "respawn_anchor",
    "dragon_egg",
    "tnt",
    "lightning_rod",
    "bell",
    "beehive",
    "bee_nest",
    "campfire",
    "redstone",
    "copper_bulb",
    "berries",
    "berry_bush",
    "cave_vines",
    "sculk_",
    "shulker_box",
];

/// Whether right-clicking this block state actuates it instead of placing.
pub(crate) fn is_interactable_state(state: u32) -> bool {
    if lodestone_data::block_entity_types::block_entity_type(state).is_some() {
        return true;
    }
    let Some(name) = lodestone_data::block_states::block_name(state) else {
        return false;
    };
    INTERACTABLE_FRAGMENTS
        .iter()
        .any(|fragment| name.contains(fragment))
}

/// Blocks whose `facing` is `getHorizontalDirection().getOpposite()` — vanilla's
/// own horizontal-directional block family, i.e. "faces the player".
///
/// A hand-written list, and the reason it is a list rather than a derivation:
/// nothing in the block-state census distinguishes a 4-way `facing` that points
/// *toward* the player (stairs, ladders, beds, doors,
/// and the face-attached horizontal-directional family) from one that points *away*
/// (chests, furnaces, the carved pumpkin, …) — the two
/// have identical property signatures and differ only in vanilla's own
/// per-block placement logic. There are 293
/// blocks with a 4-value `facing` in 26.2; a block that is not named here (and is
/// not a stair) simply does not predict.
///
/// Sourced by grepping `getStateForPlacement` for
/// `getHorizontalDirection().getOpposite()` across the decompiled 26.2
/// client's own block classes, then restricted to the
/// single-cell blocks whose remaining properties [`state_for_placement`] can also
/// resolve. Namespace-stripped paths.
const FACING_HORIZONTAL_OPPOSITE: &[&str] = &[
    // The chest family (regular and ender chests).
    "chest",
    "trapped_chest",
    "ender_chest",
    "copper_chest",
    "exposed_copper_chest",
    "weathered_copper_chest",
    "oxidized_copper_chest",
    "waxed_copper_chest",
    "waxed_exposed_copper_chest",
    "waxed_weathered_copper_chest",
    "waxed_oxidized_copper_chest",
    // The furnace family.
    "furnace",
    "blast_furnace",
    "smoker",
    // The carved-pumpkin family.
    "carved_pumpkin",
    "jack_o_lantern",
    // The beehive family.
    "beehive",
    "bee_nest",
    // One-off horizontal-directional blocks.
    "end_portal_frame",
    "chiseled_bookshelf",
    "lectern",
    "loom",
    "stonecutter",
    "vault",
    "repeater",
    // The glazed-terracotta family.
    "white_glazed_terracotta",
    "orange_glazed_terracotta",
    "magenta_glazed_terracotta",
    "light_blue_glazed_terracotta",
    "yellow_glazed_terracotta",
    "lime_glazed_terracotta",
    "pink_glazed_terracotta",
    "gray_glazed_terracotta",
    "light_gray_glazed_terracotta",
    "cyan_glazed_terracotta",
    "purple_glazed_terracotta",
    "blue_glazed_terracotta",
    "brown_glazed_terracotta",
    "green_glazed_terracotta",
    "red_glazed_terracotta",
    "black_glazed_terracotta",
];

/// Blocks whose 6-way `facing` is `getNearestLookingDirection().getOpposite()` —
/// vanilla's `DirectionalBlock` family.
///
/// Same reasoning as [`FACING_HORIZONTAL_OPPOSITE`], and likewise a list rather
/// than "every block with a 6-value `facing`": 41 blocks have one in 26.2, and
/// several derive it from the *clicked face* instead (`amethyst_cluster`,
/// `end_rod`, `shulker_box`'s successors), which is a different rule with the same
/// property signature.
const FACING_ALL: &[&str] = &[
    "dispenser",
    "dropper",
    "observer",
    "piston",
    "sticky_piston",
    "barrel",
];

/// The value vanilla's `getStateForPlacement` leaves each **non-geometric**
/// property at, for every property whose registered default is the *same across
/// every block that has it*.
///
/// # Provenance, and why this is a measurement rather than a guess
///
/// Derived from the generated block registry report at
/// `.cache/mc/26.2/generated/reports/blocks.json` by taking each block's
/// `"default": true` state and collecting, per property name, the set of values
/// it holds there.
/// 93 property names appear; **60 of them take one value across all 1,196
/// blocks** and are listed below. The 17 that do not (`facing`, `axis`, `half`,
/// `type`, `shape`, `lit`, `waterlogged`, `level`, `mode`, `rotation`, `up`,
/// `down`, `north`, `south`, `east`, `west`, `bottom`) are either resolved from
/// geometry by [`OrientationKind`], handled by an explicit rule in
/// [`state_for_placement`], or a reason to decline the prediction outright.
///
/// A further 16 unambiguous names are **deliberately left out** because vanilla
/// computes them at placement time from geometry or neighbours, so their
/// registered default is the wrong answer for a *placed* block: `attachment`
/// (`BellBlock`), `face` (`FaceAttachedHorizontalDirectionalBlock`),
/// `orientation` (`CrafterBlock`, `JigsawBlock`), `hinge` (`DoorBlock`), `part`
/// (`BedBlock`), `vertical_direction`/`thickness` (`PointedDripstoneBlock`),
/// `hanging` (`LanternBlock`), `distance`/`persistent`/`leaves`
/// (`LeavesBlock` — note `persistent` is set **true** for a player-placed leaf,
/// so its `false` default would be actively wrong), `instrument`
/// (`NoteBlock`, read from the block below), `side_chain`, `tip`, `tilt`, `drag`.
/// Omitting a name makes every block carrying it decline, which is the safe
/// direction.
///
/// Measured coverage of the whole scheme: **721 of 1,196 blocks** resolve to a
/// state, and every one of those 721 matches the block's own registered default
/// once the geometry properties are put back — except the 22 aquatic blocks
/// (corals, coral fans, `sea_pickle`, `conduit`) whose registered default is
/// `waterlogged = true`. Those are not a divergence in practice: vanilla sets
/// `waterlogged` from the fluid at the placement position, and
/// [`is_air_state`] means the shell only ever predicts into a cell with no fluid.
const NON_GEOMETRIC_DEFAULTS: &[(&str, &str)] = &[
    ("age", "0"),
    ("attached", "false"),
    ("berries", "false"),
    ("bites", "0"),
    ("bloom", "false"),
    ("can_summon", "false"),
    ("candles", "1"),
    ("charges", "0"),
    ("conditional", "false"),
    ("copper_golem_pose", "standing"),
    ("cracked", "false"),
    ("crafting", "false"),
    ("creaking_heart_state", "uprooted"),
    ("delay", "1"),
    ("disarmed", "false"),
    ("dusted", "0"),
    ("eggs", "1"),
    ("enabled", "true"),
    ("extended", "false"),
    ("eye", "false"),
    ("flower_amount", "1"),
    ("has_book", "false"),
    ("has_bottle_0", "false"),
    ("has_bottle_1", "false"),
    ("has_bottle_2", "false"),
    ("has_record", "false"),
    ("hatch", "0"),
    ("honey_level", "0"),
    ("hydration", "0"),
    ("in_wall", "false"),
    ("inverted", "false"),
    ("layers", "1"),
    ("locked", "false"),
    ("moisture", "0"),
    ("natural", "false"),
    ("note", "0"),
    ("occupied", "false"),
    ("ominous", "false"),
    ("open", "false"),
    ("pickles", "1"),
    ("potent_sulfur_state", "dry"),
    ("power", "0"),
    ("powered", "false"),
    ("sculk_sensor_phase", "inactive"),
    ("segment_amount", "1"),
    ("short", "false"),
    ("shrieking", "false"),
    ("signal_fire", "false"),
    ("slot_0_occupied", "false"),
    ("slot_1_occupied", "false"),
    ("slot_2_occupied", "false"),
    ("slot_3_occupied", "false"),
    ("slot_4_occupied", "false"),
    ("slot_5_occupied", "false"),
    ("snowy", "false"),
    ("stage", "0"),
    ("trial_spawner_state", "inactive"),
    ("triggered", "false"),
    ("unstable", "false"),
    ("vault_state", "inactive"),
];

/// Per-block values for a property whose default is *not* consistent across
/// blocks, so it cannot live in [`NON_GEOMETRIC_DEFAULTS`].
///
/// `lit` splits 48 `false` / 4 `true` over the blocks that have it — a furnace
/// places unlit, a `redstone_torch` places lit. Rather than pick one and be wrong
/// for the other, only the named blocks get an answer; everything else with a
/// `lit` property declines. `(block path, property, value)`, from the same
/// `blocks.json` default states.
const BLOCK_PROPERTY_OVERRIDES: &[(&str, &str, &str)] = &[
    ("furnace", "lit", "false"),
    ("blast_furnace", "lit", "false"),
    ("smoker", "lit", "false"),
];

/// Every 26.2 state of one block, plus the value domain of each of its
/// properties.
///
/// Built by one linear pass over the 32,366-entry state table. That is only
/// ever run on a right-click, and it is what lets the two functions below work
/// from the real census instead of a second, hand-maintained table keyed by
/// block name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BlockStates {
    /// This block's state ids, ascending.
    ids: Vec<u32>,
    /// `(property name, distinct values)`. Sorted by name, because
    /// [`lodestone_data::block_states::properties`] hands back sorted pairs.
    domains: Vec<(&'static str, Vec<&'static str>)>,
}

impl BlockStates {
    fn domain(&self, name: &str) -> Option<&[&'static str]> {
        self.domains
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, values)| values.as_slice())
    }
}

/// Collect [`BlockStates`] for `block` (a full identifier, e.g.
/// `minecraft:chest`), or `None` if no such block exists — which is how a
/// non-block item (a sword, bread) is recognised: vanilla's `BlockItem` shares
/// its block's registry name, so "is this item placeable?" is "is there a block
/// with this name?".
pub(crate) fn block_states_of(block: &str) -> Option<BlockStates> {
    let mut ids = Vec::new();
    let mut domains: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
    for id in 0..lodestone_data::block_states::STATE_COUNT {
        if lodestone_data::block_states::block_name(id) != Some(block) {
            continue;
        }
        ids.push(id);
        for &(name, value) in lodestone_data::block_states::properties(id).unwrap_or(&[]) {
            match domains.iter_mut().find(|(candidate, _)| *candidate == name) {
                Some((_, values)) => {
                    if !values.contains(&value) {
                        values.push(value);
                    }
                }
                None => domains.push((name, vec![value])),
            }
        }
    }
    (!ids.is_empty()).then_some(BlockStates { ids, domains })
}

/// Classify how `block` derives its orientation from placement geometry, or
/// `None` when the census cannot say — in which case the shell does not predict
/// this item at all.
///
/// Everything decidable from the property signature is decided from it; the two
/// facing families that are *not* (see [`FACING_HORIZONTAL_OPPOSITE`]) come from
/// a named list. Declining is always safe: it reproduces the pre-fix behaviour
/// of sending `use_item_on` and waiting.
pub(crate) fn orientation_for_placement(block: &str, states: &BlockStates) -> Option<OrientationKind> {
    let path = block.strip_prefix("minecraft:").unwrap_or(block);
    // A pillar's axis is the clicked face's axis (`RotatedPillarBlock`). A
    // 2-value `axis` is `nether_portal`, which is not placed by an item.
    if let Some(axis) = states.domain("axis") {
        return (axis.len() == 3).then_some(OrientationKind::Pillar);
    }
    // `SlabBlock`'s `type` is `top`/`bottom`/`double`; a chest's is
    // `single`/`left`/`right`, which is not geometry and is handled as a
    // non-geometric default instead.
    if states.domain("type").is_some_and(|d| d.contains(&"double")) {
        return Some(OrientationKind::Slab);
    }
    match states.domain("facing").map(<[&str]>::len) {
        Some(4) => {
            if states.domain("half").is_some_and(|d| d.contains(&"bottom"))
                && states.domain("shape").is_some()
            {
                return Some(OrientationKind::Stairs);
            }
            FACING_HORIZONTAL_OPPOSITE
                .contains(&path)
                .then_some(OrientationKind::FacingHorizontalOpposite)
        }
        Some(6) => FACING_ALL
            .contains(&path)
            .then_some(OrientationKind::FacingAll),
        // A 5-value `facing` is a hopper, whose placement rule is its own.
        Some(_) => None,
        // No `facing`: orientation-free, as long as nothing else in the
        // signature says the placement reads geometry we are not modelling
        // (a rail's `shape`, a door's `half`).
        None => (states.domain("shape").is_none() && states.domain("half").is_none())
            .then_some(OrientationKind::Fixed),
    }
}

/// The block-state id a predicted placement should write, or `None` when any
/// property of the block cannot be resolved.
///
/// This is a **total** specification, not a best effort: every property the block
/// has is given a value — from `placed` when [`OrientationKind`] defines it, from
/// [`BLOCK_PROPERTY_OVERRIDES`] / [`NON_GEOMETRIC_DEFAULTS`] / the two explicit
/// rules otherwise — and the matching state id is then the *unique* state whose
/// property set equals it. A partial specification would need the block's
/// registered default state to fill the rest, and no census in this tree carries
/// one (`blocks.json`'s `"default": true` flag is not in
/// [`lodestone_data::block_states`]). That absence is exactly why this function
/// declines instead of guessing.
pub(crate) fn state_for_placement(
    block: &str,
    states: &BlockStates,
    orientation: OrientationKind,
    placed: &PlacedState,
) -> Option<u32> {
    let path = block.strip_prefix("minecraft:").unwrap_or(block);
    let mut wanted: Vec<(&'static str, &'static str)> = Vec::with_capacity(states.domains.len());
    for (name, domain) in &states.domains {
        let value = match *name {
            "facing"
                if matches!(
                    orientation,
                    OrientationKind::FacingAll
                        | OrientationKind::FacingHorizontal
                        | OrientationKind::FacingHorizontalOpposite
                        | OrientationKind::Stairs
                ) =>
            {
                face_property(placed.facing?)
            }
            "axis" if orientation == OrientationKind::Pillar => axis_property(placed.axis?),
            "type" if orientation == OrientationKind::Slab => half_property(placed.half?),
            "half" if orientation == OrientationKind::Stairs => half_property(placed.half?),
            // `StairBlock.getStateForPlacement` computes `shape` from the
            // neighbouring stairs; `straight` is the no-neighbour answer and is
            // what every one of the 64 stair blocks defaults to. The server
            // corrects a corner with its own block update.
            "shape" if orientation == OrientationKind::Stairs => "straight",
            // Vanilla reads this from the fluid at the placement position
            // (`SimpleWaterloggedBlock`'s `copyWaterloggedFrom`). We only predict
            // into air (see `is_air_state`), so `false` is the answer rather than
            // a default.
            "waterlogged" => "false",
            // `ChestBlock.getStateForPlacement` scans for an adjacent chest to
            // make a double; `single` is the no-neighbour answer, and the server
            // re-sends the state when a neighbour makes it a double. Keyed on the
            // value rather than the property name because `type` is also a slab's
            // (`top`/`bottom`/`double`) and a piston head's (`normal`/`sticky`) —
            // only the ten chest blocks have a `single`, measured across the 26.2
            // census.
            "type" if domain.contains(&"single") => "single",
            _ => BLOCK_PROPERTY_OVERRIDES
                .iter()
                .find(|(candidate, property, _)| *candidate == path && property == name)
                .map(|&(_, _, value)| value)
                .or_else(|| {
                    NON_GEOMETRIC_DEFAULTS
                        .iter()
                        .find(|(property, _)| property == name)
                        .map(|&(_, value)| value)
                })?,
        };
        wanted.push((name, value));
    }
    // `domains` is in the census's own sorted-by-name order, so `wanted` is too
    // and this is a slice comparison rather than a per-property search.
    states
        .ids
        .iter()
        .copied()
        .find(|&id| lodestone_data::block_states::properties(id) == Some(wanted.as_slice()))
}

/// The block-state id a right-click on `block` predicts, given the
/// geometry-derived [`PlacedState`] [`lodestone_game::placement::Placement::use_on`] resolved — or `None`
/// when the shell declines to predict this block at all.
///
/// The whole resolution behind [`super::Sim::use_item_live`]'s local write, in one
/// callable place: classify the orientation from the census
/// ([`orientation_for_placement`]) then specify every property
/// ([`state_for_placement`]). `pub` so a pixel gate can drive the *same*
/// resolution a click does instead of choosing a state of its own and proving
/// nothing about which one the shell would pick.
#[must_use]
pub fn predicted_placement_state(block: &str, placed: &PlacedState) -> Option<u32> {
    let states = block_states_of(block)?;
    let orientation = orientation_for_placement(block, &states)?;
    state_for_placement(block, &states, orientation, placed)
}

/// [`BlockFace`] to the `facing` property value (`Direction.getSerializedName`).
pub(crate) fn face_property(face: BlockFace) -> &'static str {
    match face {
        BlockFace::Down => "down",
        BlockFace::Up => "up",
        BlockFace::North => "north",
        BlockFace::South => "south",
        BlockFace::West => "west",
        BlockFace::East => "east",
    }
}

/// [`Axis`] to the `axis` property value.
pub(crate) fn axis_property(axis: Axis) -> &'static str {
    match axis {
        Axis::X => "x",
        Axis::Y => "y",
        Axis::Z => "z",
    }
}

/// [`Half`] to the `half` (stairs) / `type` (slab) property value — the two share
/// the `top`/`bottom` vocabulary.
pub(crate) fn half_property(half: Half) -> &'static str {
    match half {
        Half::Bottom => "bottom",
        Half::Top => "top",
    }
}

/// Write a locally predicted block state, block entity included.
///
/// **This is the local mirror of the v770 adapter's `BLOCK_UPDATE` arm**, and it
/// is deliberately the same two calls in the same order:
/// [`WorldSink::set_block`] then [`WorldSink::sync_block_entity`] with the new
/// state's `BLOCK_ENTITY_TYPE` id. Writing the state alone is that fix — a
/// chest with a state, no record, and zero pixels — and that fix is that same bug
/// reached through the *prediction* rather than through a packet.
///
/// A free function over `&mut dyn WorldSink` rather than a `Sim` method so a test
/// can drive the production write with a bare [`lodestone_world::World`], no GPU and no server.
/// The `Option<u32>` the world takes comes from [`lodestone_data`]: `lodestone-world`
/// cannot depend on it (`data → model → world` is a cycle), which is why the
/// caller resolves the type and the world only applies it.
pub fn write_predicted_block(
    world: &mut dyn WorldSink,
    block: [i32; 3],
    state: u32,
) -> BlockEntitySync {
    world.set_block(block[0], block[1], block[2], state);
    world.sync_block_entity(
        block[0],
        block[1],
        block[2],
        lodestone_data::block_entity_types::block_entity_type(state),
    )
}
