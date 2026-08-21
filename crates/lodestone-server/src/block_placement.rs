//! Per-block placement conventions — `Block.getStateForPlacement` for the
//! families a player actually places by hand.
//!
//! # Why this is a table and not a formula
//!
//! There is no single convention. Read off 26.2's own sources: a stair's
//! `getStateForPlacement` takes
//! `context.getHorizontalDirection()` (`StairBlock`), a furnace's or a
//! chest's takes `.getOpposite()` (`AbstractFurnaceBlock.getStateForPlacement`,
//! `ChestBlock.getStateForPlacement`), an anvil's takes `.getClockWise()`
//! (`AnvilBlock.getStateForPlacement`), a dispenser's takes
//! `getNearestLookingDirection().getOpposite()` (`DispenserBlock.getStateForPlacement`), a
//! shulker box's takes the *clicked face* (`ShulkerBoxBlock.getStateForPlacement`) and a
//! hopper's takes the clicked face's opposite folded onto `down`
//! (`HopperBlock.getStateForPlacement`). Picking any one of those and applying it
//! everywhere is wrong about half the time, which is exactly what a placed
//! chest facing the wrong way looked like.
//!
//! # How a block reaches its convention
//!
//! Family is derived from the **block-state census** ([`block_states`]) — which
//! properties a block's states actually carry — not from a name list, so a
//! block added to 26.2's data reaches the right arm without an edit here. Name
//! lists appear only where the census cannot separate two families that carry
//! the same properties (a ladder and a lectern are both "one horizontal
//! `facing`"), and each such list is small and cited.
//!
//! # How to change it
//!
//! Add an arm to [`placement`], keyed off [`Shape`] where possible. Never
//! compute a state **id** here: return a state *string* naming only the
//! properties you mean and let `v770`'s `resolve_state_id` write them over the
//! jar-marked default state. Re-deriving id arithmetic is how a past
//! regression here happened.
//!
//! Gotcha: `cursor` is block-local to the **clicked** block, and vanilla's
//! `getClickLocation().y - getClickedPos().getY()` is relative to the
//! **placement** cell. Those agree for every horizontal click (same y) and the
//! two vertical cases short-circuit before the cursor is read, which is why
//! [`upper_half`] can use `cursor.y` directly.

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_data::block_states;
use lodestone_model::{BlockFace, BlockPos, Vec3f};

use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_to_str, get_str_property};

/// Everything a placement decision can read: where it landed, how it was
/// clicked, and where the player was looking.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlaceContext {
    /// The cell the block is being written to (vanilla's `getClickedPos`).
    pub target: BlockPos,
    /// The face of the clicked block that was hit.
    pub face: BlockFace,
    /// Block-local hit position, each component `0.0`–`1.0`.
    pub cursor: Vec3f,
    /// Player yaw in degrees; `None` before the first packet carrying angles.
    pub yaw: Option<f32>,
    /// Player pitch in degrees; `None` before the first packet carrying angles.
    pub pitch: Option<f32>,
}

/// The outcome of a placement: the state for the clicked cell plus any other
/// cells the placement owns (a door's upper half, a bed's head, a chest
/// partner's re-typing).
#[derive(Debug, Clone)]
pub(crate) struct Placement {
    pub state: String,
    pub extra: Vec<(BlockPos, String)>,
}

impl Placement {
    fn just(state: String) -> Self {
        Self {
            state,
            extra: Vec::new(),
        }
    }
}

/// `Block.getStateForPlacement` for `block`, or `None` when this crate has no
/// convention for it and the caller should keep the bare default state.
///
/// `block_at` reads the world; it is called only for the families whose state
/// genuinely depends on a neighbour (stairs' `shape`, a chest's `type`, a
/// slab's `double`, an end rod's flip).
pub(crate) fn placement<F>(block: &str, ctx: &PlaceContext, block_at: F) -> Option<Placement>
where
    F: Fn(BlockPos) -> String,
{
    // A wall-attached variant is a different *block*, not a different state, so
    // the redirect runs before any family dispatch.
    if let Some(wall) = wall_variant(block, ctx.face) {
        let facing = horizontal_face(ctx.face)?;
        return Some(Placement::just(format!(
            "{wall}[facing={}]",
            direction_to_str(facing)
        )));
    }

    // `NoteBlock.getStateForPlacement` → `setInstrument`: reads the block
    // directly above first (a mob head already sitting there), falling back to
    // the block below. Carries no `Shape` flag of its own, so it is dispatched
    // by name before the census-driven arms below.
    //
    // Not modelled here: vanilla's `NoteBlock.updateShape` re-runs the same
    // computation whenever the block directly above or below changes *after*
    // placement (e.g. breaking the block a note block sits on). This crate has
    // no generic "notify this block when a vertical neighbour changes" seam to
    // hook that into yet, so an already-placed note block's `instrument` goes
    // stale in that case — a real, currently-unclosed gap, not a silent one.
    if base_name(block) == crate::redstone_note_block::NOTE_BLOCK {
        let above = block_at(Direction::Up.relative(ctx.target));
        let below = block_at(Direction::Down.relative(ctx.target));
        let instrument = crate::redstone_note_block::instrument_for_note_block(&above, &below);
        return Some(Placement::just(format!(
            "{block}[instrument={}]",
            instrument.state_name()
        )));
    }

    let shape = shape_of(block)?;

    if shape.pillar_axis {
        let axis = match ctx.face {
            BlockFace::Up | BlockFace::Down => "y",
            BlockFace::North | BlockFace::South => "z",
            BlockFace::East | BlockFace::West => "x",
        };
        return Some(Placement::just(format!("{block}[axis={axis}]")));
    }

    if shape.slab_type {
        return Some(Placement::just(slab_state(block, ctx, &block_at)));
    }

    if shape.stairs_shape {
        return Some(Placement::just(stair_state(block, ctx, &block_at)?));
    }

    if shape.hinge {
        return door_state(block, ctx, &block_at);
    }

    if shape.trapdoor {
        return Some(Placement::just(trapdoor_state(block, ctx)?));
    }

    if shape.chest_type {
        return Some(chest_state(block, ctx, &block_at)?);
    }

    if shape.bed_part {
        let facing = horizontal_look(ctx)?;
        return Some(Placement {
            state: format!("{block}[facing={},part=foot]", direction_to_str(facing)),
            extra: vec![(
                facing.relative(ctx.target),
                format!("{block}[facing={},part=head]", direction_to_str(facing)),
            )],
        });
    }

    if shape.attach_face {
        // `FaceAttachedHorizontalDirectionalBlock.getStateForPlacement`
        // walks `getNearestLookingDirections()` and keeps the first
        // that `canSurvive`. The clicked face already *is* that direction for
        // every reachable click — a button on a block's top face attaches to
        // the floor — so the clicked face stands in for the walk plus the
        // survival filter this crate does not model.
        let (face, facing) = match ctx.face {
            BlockFace::Up => ("floor", horizontal_look(ctx)?),
            BlockFace::Down => ("ceiling", horizontal_look(ctx)?),
            other => ("wall", horizontal_face(other)?),
        };
        return Some(Placement::just(format!(
            "{block}[face={face},facing={}]",
            direction_to_str(facing)
        )));
    }

    if shape.bell_attachment {
        // `BellBlock.getStateForPlacement`.
        let (attachment, facing) = match ctx.face {
            BlockFace::Up => ("floor", horizontal_look(ctx)?),
            BlockFace::Down => ("ceiling", horizontal_look(ctx)?),
            other => ("single_wall", horizontal_face(other)?.opposite()),
        };
        return Some(Placement::just(format!(
            "{block}[attachment={attachment},facing={}]",
            direction_to_str(facing)
        )));
    }

    if shape.rotation16 {
        // A standing sign/banner/skull. `StandingSignBlock.getStateForPlacement` and
        // `BannerBlock.getStateForPlacement` offset the yaw by 180°, `SkullBlock.getStateForPlacement`
        // does not.
        let yaw = ctx.yaw?;
        let offset = if base_name(block).ends_with("_skull") || base_name(block).ends_with("_head") {
            0.0
        } else {
            180.0
        };
        return Some(Placement::just(format!(
            "{block}[rotation={}]",
            rotation_segment(yaw + offset)
        )));
    }

    if shape.rail_shape {
        // `BaseRailBlock.getStateForPlacement`.
        let facing = horizontal_look(ctx)?;
        let axis = match facing {
            Direction::East | Direction::West => "east_west",
            _ => "north_south",
        };
        return Some(Placement::just(format!("{block}[shape={axis}]")));
    }

    if shape.facing_vertical {
        return Some(Placement::just(six_way_state(block, ctx, &block_at)?));
    }

    if shape.facing_horizontal {
        let facing = horizontal_facing(block, ctx)?;
        return Some(Placement::just(format!(
            "{block}[facing={}]",
            direction_to_str(facing)
        )));
    }

    None
}

/// Blocks whose horizontal `facing` is the player's look direction rather than
/// its opposite — `context.getHorizontalDirection()` with no `.getOpposite()`.
///
/// The census cannot tell these apart from the `.getOpposite()` majority (both
/// are "one horizontal `facing`"), so this is the one place a name list is
/// unavoidable. Every entry is a `setValue(FACING, context.getHorizontalDirection())`
/// call site in 26.2's block sources; the majority convention lives in
/// [`horizontal_facing`]'s fallback.
const FACING_IS_LOOK: &[&str] = &[
    "minecraft:campfire",
    "minecraft:soul_campfire",
    "minecraft:calibrated_sculk_sensor",
    "minecraft:decorated_pot",
];

/// Blocks whose horizontal `facing` is the **clicked face** — a wall
/// attachment placed against the block it hangs on. `LadderBlock.getStateForPlacement`
/// and `TripWireHookBlock.getStateForPlacement` both walk `getNearestLookingDirections()`
/// filtered by `canSurvive`, which for any reachable click resolves to the
/// clicked face.
const FACING_IS_CLICKED_FACE: &[&str] = &["minecraft:ladder", "minecraft:tripwire_hook"];

/// The horizontal `facing` for a one-`facing` block.
fn horizontal_facing(block: &str, ctx: &PlaceContext) -> Option<Direction> {
    if FACING_IS_CLICKED_FACE.contains(&block) {
        return horizontal_face(ctx.face);
    }
    if block == "minecraft:anvil" || block.ends_with("_anvil") {
        // `AnvilBlock.getStateForPlacement` — the only `.getClockWise()` in the game.
        return Some(horizontal_look(ctx)?.clockwise());
    }
    if FACING_IS_LOOK.contains(&block) {
        return horizontal_look(ctx);
    }
    // The majority: furnace, blast furnace, smoker, ender chest, lectern,
    // loom, stonecutter, beehive, carved pumpkin, glazed terracotta, chiseled
    // bookshelf, end portal frame, vault, shelf, repeater, comparator, …
    Some(horizontal_look(ctx)?.opposite())
}

/// Blocks with a six-value `facing` that take the **clicked face** verbatim:
/// `ShulkerBoxBlock.getStateForPlacement`, `AmethystClusterBlock.getStateForPlacement`,
/// `LightningRodBlock.getStateForPlacement`.
fn facing_is_clicked_face(block: &str) -> bool {
    block.ends_with("shulker_box")
        || block == "minecraft:amethyst_cluster"
        || block.ends_with("_amethyst_bud")
        || block == "minecraft:lightning_rod"
}

/// `DirectionalBlock`-family placement.
fn six_way_state<F>(block: &str, ctx: &PlaceContext, block_at: &F) -> Option<String>
where
    F: Fn(BlockPos) -> String,
{
    let clicked = face_direction(ctx.face);
    let facing = if block == "minecraft:hopper" {
        // `HopperBlock.getStateForPlacement`: the clicked face's opposite, with either
        // vertical answer folded onto `down` (a hopper has no `up`).
        match clicked.opposite() {
            Direction::Up | Direction::Down => Direction::Down,
            horizontal => horizontal,
        }
    } else if block == "minecraft:end_rod" {
        // `EndRodBlock.getStateForPlacement`: normally the clicked face, but flipped
        // when the block behind is an end rod already pointing this way, so a
        // chain of rods alternates instead of stacking.
        let behind = block_at(clicked.opposite().relative(ctx.target));
        let chained = base_name(&behind) == "minecraft:end_rod"
            && get_str_property(&behind, "facing") == Some(direction_to_str(clicked));
        if chained { clicked.opposite() } else { clicked }
    } else if facing_is_clicked_face(block) {
        clicked
    } else if block == "minecraft:observer" {
        // `ObserverBlock.getStateForPlacement` double-negates, so an observer watches the
        // direction the player is looking.
        nearest_look(ctx)?
    } else {
        // Dispenser, dropper, barrel, piston, command blocks:
        // `getNearestLookingDirection().getOpposite()`.
        nearest_look(ctx)?.opposite()
    };
    Some(format!("{block}[facing={}]", direction_to_str(facing)))
}

/// `SlabBlock.getStateForPlacement`. The `double` arm fires when the
/// cell already holds a matching half-slab, which is why the caller must offer
/// a slab cell as replaceable.
fn slab_state<F>(block: &str, ctx: &PlaceContext, block_at: &F) -> String
where
    F: Fn(BlockPos) -> String,
{
    let existing = block_at(ctx.target);
    if base_name(&existing) == block && get_str_property(&existing, "type") != Some("double") {
        return format!("{block}[type=double]");
    }
    let kind = if upper_half(ctx) { "top" } else { "bottom" };
    format!("{block}[type={kind}]")
}

/// `StairBlock.getStateForPlacement` — facing from the look
/// direction, `half` from the click, `shape` from the two neighbours on the
/// facing axis.
fn stair_state<F>(block: &str, ctx: &PlaceContext, block_at: &F) -> Option<String>
where
    F: Fn(BlockPos) -> String,
{
    let facing = horizontal_look(ctx)?;
    let half = if upper_half(ctx) { "top" } else { "bottom" };
    let shape = stair_shape(ctx.target, facing, half, block_at);
    Some(format!(
        "{block}[facing={},half={half},shape={shape}]",
        direction_to_str(facing)
    ))
}

/// `StairBlock.getStairsShape`, including the `canTakeShape`
/// guard that stops a run of parallel stairs from cornering.
fn stair_shape<F>(pos: BlockPos, facing: Direction, half: &str, block_at: &F) -> &'static str
where
    F: Fn(BlockPos) -> String,
{
    let stair_facing = |p: BlockPos| -> Option<Direction> {
        let state = block_at(p);
        if !base_name(&state).ends_with("_stairs") || get_str_property(&state, "half") != Some(half) {
            return None;
        }
        get_str_property(&state, "facing").map(crate::redstone::direction_from_str)
    };
    // `canTakeShape`: the cell one step towards `neighbour` must not be a
    // stair with the same facing *and* half.
    let can_take = |neighbour: Direction| -> bool {
        let state = block_at(neighbour.relative(pos));
        !base_name(&state).ends_with("_stairs")
            || get_str_property(&state, "facing") != Some(direction_to_str(facing))
            || get_str_property(&state, "half") != Some(half)
    };
    let axis_differs = |other: Direction| -> bool {
        matches!(
            (facing, other),
            (Direction::North | Direction::South, Direction::East | Direction::West)
                | (Direction::East | Direction::West, Direction::North | Direction::South)
        )
    };

    if let Some(behind) = stair_facing(facing.relative(pos)) {
        if axis_differs(behind) && can_take(behind.opposite()) {
            return if behind == facing.counterclockwise() {
                "outer_left"
            } else {
                "outer_right"
            };
        }
    }
    if let Some(front) = stair_facing(facing.opposite().relative(pos)) {
        if axis_differs(front) && can_take(front) {
            return if front == facing.counterclockwise() {
                "inner_left"
            } else {
                "inner_right"
            };
        }
    }
    "straight"
}

/// `TrapDoorBlock.getStateForPlacement`.
fn trapdoor_state(block: &str, ctx: &PlaceContext) -> Option<String> {
    let (facing, half) = match horizontal_face(ctx.face) {
        // Clicked a side: the trapdoor hangs on that side, hinged at whichever
        // half of the block the cursor landed in.
        Some(side) => (side, if ctx.cursor.y > 0.5 { "top" } else { "bottom" }),
        // Clicked top or bottom: facing away from the player, hinged opposite
        // the clicked face.
        None => (
            horizontal_look(ctx)?.opposite(),
            if matches!(ctx.face, BlockFace::Up) { "bottom" } else { "top" },
        ),
    };
    Some(format!(
        "{block}[facing={},half={half}]",
        direction_to_str(facing)
    ))
}

/// `DoorBlock.getStateForPlacement` plus `DoorBlock.setPlacedBy`,
/// which is what puts the upper half in the cell above.
fn door_state<F>(block: &str, ctx: &PlaceContext, block_at: &F) -> Option<Placement>
where
    F: Fn(BlockPos) -> String,
{
    let facing = horizontal_look(ctx)?;
    let hinge = door_hinge(ctx, facing, block_at);
    let facing = direction_to_str(facing);
    Some(Placement {
        state: format!("{block}[facing={facing},half=lower,hinge={hinge}]"),
        extra: vec![(
            Direction::Up.relative(ctx.target),
            format!("{block}[facing={facing},half=upper,hinge={hinge}]"),
        )],
    })
}

/// `DoorBlock.getHinge`: a door pairs with an adjacent door, then
/// falls back to whichever side has more solid blocks, then to the half of the
/// block the cursor landed in.
fn door_hinge<F>(ctx: &PlaceContext, facing: Direction, block_at: &F) -> &'static str
where
    F: Fn(BlockPos) -> String,
{
    let pos = ctx.target;
    let above = Direction::Up.relative(pos);
    let left = facing.counterclockwise();
    let right = facing.clockwise();
    let solid = |p: BlockPos| -> i32 {
        i32::from(crate::redstone::is_redstone_conductor(&block_at(p)))
    };
    let lower_door = |p: BlockPos| -> bool {
        let state = block_at(p);
        base_name(&state).ends_with("_door") && get_str_property(&state, "half") == Some("lower")
    };
    let balance = -solid(left.relative(pos)) - solid(left.relative(above)) + solid(right.relative(pos))
        + solid(right.relative(above));
    let door_left = lower_door(left.relative(pos));
    let door_right = lower_door(right.relative(pos));
    if (door_left && !door_right) || balance > 0 {
        return "right";
    }
    if (door_right && !door_left) || balance < 0 {
        return "left";
    }
    // The tie-break, verbatim from `DoorBlock.getHinge`: which side of the doorway's
    // own axis the cursor landed on.
    let (step_x, step_z) = match facing {
        Direction::North => (0.0, -1.0),
        Direction::South => (0.0, 1.0),
        Direction::West => (-1.0, 0.0),
        _ => (1.0, 0.0),
    };
    let (cx, cz) = (f64::from(ctx.cursor.x), f64::from(ctx.cursor.z));
    let keeps_left = (step_x >= 0.0 || cz >= 0.5)
        && (step_x <= 0.0 || cz <= 0.5)
        && (step_z >= 0.0 || cx <= 0.5)
        && (step_z <= 0.0 || cx >= 0.5);
    if keeps_left { "left" } else { "right" }
}

/// `ChestBlock.getStateForPlacement` minus the sneak-placement
/// branch (this crate does not carry the client's sneak state), plus the
/// partner's own re-typing that vanilla performs through `updateShape`.
fn chest_state<F>(block: &str, ctx: &PlaceContext, block_at: &F) -> Option<Placement>
where
    F: Fn(BlockPos) -> String,
{
    let facing = horizontal_look(ctx)?.opposite();
    // `candidatePartnerFacing`: a same-block, still-single chest one step over.
    let partner = |side: Direction| -> Option<(BlockPos, Direction)> {
        let p = side.relative(ctx.target);
        let state = block_at(p);
        if base_name(&state) != block || get_str_property(&state, "type") != Some("single") {
            return None;
        }
        let f = crate::redstone::direction_from_str(get_str_property(&state, "facing")?);
        (f == facing).then_some((p, side))
    };
    let facing_str = direction_to_str(facing);
    let (kind, partner_kind, partner_pos) = match partner(facing.clockwise()) {
        Some((p, _)) => ("left", "right", Some(p)),
        None => match partner(facing.counterclockwise()) {
            Some((p, _)) => ("right", "left", Some(p)),
            None => ("single", "single", None),
        },
    };
    let mut extra = Vec::new();
    if let Some(p) = partner_pos {
        extra.push((p, format!("{block}[facing={facing_str},type={partner_kind}]")));
    }
    Some(Placement {
        state: format!("{block}[facing={facing_str},type={kind}]"),
        extra,
    })
}

/// The upper/lower-half decision every `Half`-bearing block shares —
/// `StairBlock.getStateForPlacement` and `SlabBlock.getStateForPlacement`, which are the same
/// expression.
fn upper_half(ctx: &PlaceContext) -> bool {
    match ctx.face {
        BlockFace::Down => true,
        BlockFace::Up => false,
        _ => ctx.cursor.y > 0.5,
    }
}

/// `RotationSegment.convertToSegment(float)` — `SegmentedAnglePrecision(4)`'s
/// `fromDegrees`, i.e. `round(deg * 16 / 360) & 15`.
fn rotation_segment(degrees: f32) -> i32 {
    ((degrees * 16.0 / 360.0).round() as i32) & 15
}

/// `Direction.fromYRot` restricted to the four horizontal directions: the
/// direction the player is looking. `south=0`, increasing clockwise.
fn horizontal_look(ctx: &PlaceContext) -> Option<Direction> {
    let yaw = ctx.yaw?;
    Some(match yaw.rem_euclid(360.0) {
        y if (45.0..135.0).contains(&y) => Direction::West,
        y if (135.0..225.0).contains(&y) => Direction::North,
        y if (225.0..315.0).contains(&y) => Direction::East,
        _ => Direction::South,
    })
}

/// `Direction.getNearest(player.getViewVector(1.0))` — the axis-aligned
/// direction closest to where the player is looking, pitch included. `None`
/// before the first packet carrying angles.
fn nearest_look(ctx: &PlaceContext) -> Option<Direction> {
    let (yaw, pitch) = (ctx.yaw?.to_radians(), ctx.pitch?.to_radians());
    // `Entity.calculateViewVector`.
    let x = -yaw.sin() * pitch.cos();
    let y = -pitch.sin();
    let z = yaw.cos() * pitch.cos();
    Some(if x.abs() > y.abs() && x.abs() > z.abs() {
        if x > 0.0 { Direction::East } else { Direction::West }
    } else if y.abs() > z.abs() {
        if y > 0.0 { Direction::Up } else { Direction::Down }
    } else if z > 0.0 {
        Direction::South
    } else {
        Direction::North
    })
}

/// The clicked face as a [`Direction`].
fn face_direction(face: BlockFace) -> Direction {
    match face {
        BlockFace::Down => Direction::Down,
        BlockFace::Up => Direction::Up,
        BlockFace::North => Direction::North,
        BlockFace::South => Direction::South,
        BlockFace::West => Direction::West,
        BlockFace::East => Direction::East,
    }
}

/// The clicked face, or `None` if it was the top or bottom.
fn horizontal_face(face: BlockFace) -> Option<Direction> {
    match face {
        BlockFace::Up | BlockFace::Down => None,
        other => Some(face_direction(other)),
    }
}

/// The wall-mounted sibling of a `StandingAndWallBlockItem`'s block, when the
/// click was against a vertical face: `torch` → `wall_torch`, `oak_sign` →
/// `oak_wall_sign`, `oak_hanging_sign` → `oak_wall_hanging_sign`,
/// `white_banner` → `white_wall_banner`, `skeleton_skull` →
/// `skeleton_wall_skull`, `zombie_head` → `zombie_wall_head`,
/// `tube_coral_fan` → `tube_coral_wall_fan`.
///
/// Derived by rewriting the name and checking the census for the result, so a
/// suffix that happens to match but has no wall sibling (there is no
/// `minecraft:dragon_wall_egg`) simply yields `None` rather than a state the
/// client cannot resolve. `hanging_sign` is tested before `sign` because both
/// suffixes match a hanging sign and only the longer one is right.
fn wall_variant(block: &str, face: BlockFace) -> Option<&'static str> {
    if matches!(face, BlockFace::Up | BlockFace::Down) {
        return None;
    }
    const SUFFIXES: &[&str] = &[
        "hanging_sign",
        "sign",
        "torch",
        "banner",
        "skull",
        "head",
        "fan",
    ];
    let (namespace, name) = block.split_once(':')?;
    for suffix in SUFFIXES {
        let Some(prefix) = name.strip_suffix(suffix) else {
            continue;
        };
        if name.contains("wall_") {
            return None;
        }
        let candidate = format!("{namespace}:{prefix}wall_{suffix}");
        if let Some(interned) = interned_block(&candidate) {
            return Some(interned);
        }
    }
    None
}

/// Which placement family a block belongs to, read off the block-state census.
#[derive(Debug, Clone, Copy, Default)]
struct Shape {
    /// `axis` over all three coordinate axes — a `RotatedPillarBlock`.
    pillar_axis: bool,
    /// `type` over `top`/`bottom`/`double` — a `SlabBlock`.
    slab_type: bool,
    /// `type` over `single`/`left`/`right` — a `ChestBlock`.
    chest_type: bool,
    /// `shape` over the five stair shapes.
    stairs_shape: bool,
    /// `hinge` — a `DoorBlock`.
    hinge: bool,
    /// `facing` + `half=top|bottom` + `open` and no stair `shape`.
    trapdoor: bool,
    /// `part` over `head`/`foot` — a `BedBlock`.
    bed_part: bool,
    /// `face` over `floor`/`wall`/`ceiling`.
    attach_face: bool,
    /// `attachment` — a `BellBlock`.
    bell_attachment: bool,
    /// `rotation` over the 16 segments.
    rotation16: bool,
    /// `shape` over the rail shapes.
    rail_shape: bool,
    /// `facing` including a vertical value.
    facing_vertical: bool,
    /// `facing` restricted to the four horizontal values.
    facing_horizontal: bool,
}

fn shape_of(block: &str) -> Option<Shape> {
    census().shapes.get(block).copied()
}

/// The interned census name equal to `name`, so a rewritten name can be handed
/// back as `&'static str`.
fn interned_block(name: &str) -> Option<&'static str> {
    census().shapes.get_key_value(name).map(|(k, _)| *k)
}

struct Census {
    shapes: HashMap<&'static str, Shape>,
}

/// One pass over the 32k-state census, building the block → [`Shape`] map.
///
/// Cached: the scan is `O(STATE_COUNT)` and every placement would otherwise
/// repeat it, which is what the old `pillar_axis_block` did per right-click.
fn census() -> &'static Census {
    static CENSUS: OnceLock<Census> = OnceLock::new();
    CENSUS.get_or_init(|| {
        // Per block, the set of `(key, value)` pairs seen anywhere in its states.
        let mut seen: HashMap<&'static str, Vec<(&'static str, &'static str)>> = HashMap::new();
        for id in 0..block_states::STATE_COUNT {
            let Some(name) = block_states::block_name(id) else {
                continue;
            };
            let entry = seen.entry(name).or_default();
            for &pair in block_states::properties(id).unwrap_or(&[]) {
                if !entry.contains(&pair) {
                    entry.push(pair);
                }
            }
        }
        let shapes = seen
            .into_iter()
            .map(|(name, pairs)| {
                let has = |key: &str, value: &str| pairs.contains(&(key, value));
                let key = |k: &str| pairs.iter().any(|&(pk, _)| pk == k);
                let stairs_shape = has("shape", "inner_left");
                let mut shape = Shape {
                    pillar_axis: has("axis", "x") && has("axis", "y") && has("axis", "z"),
                    slab_type: has("type", "double") && has("type", "top"),
                    chest_type: has("type", "single") && has("type", "left"),
                    stairs_shape,
                    hinge: key("hinge"),
                    trapdoor: key("facing") && has("half", "top") && key("open") && !stairs_shape,
                    bed_part: has("part", "head") && has("part", "foot"),
                    attach_face: has("face", "wall") && has("face", "floor"),
                    bell_attachment: key("attachment"),
                    rotation16: has("rotation", "15"),
                    rail_shape: has("shape", "north_south") && has("shape", "ascending_east"),
                    facing_vertical: has("facing", "up") || has("facing", "down"),
                    facing_horizontal: false,
                };
                shape.facing_horizontal = key("facing") && !shape.facing_vertical;
                (name, shape)
            })
            .collect();
        Census { shapes }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(face: BlockFace, cursor_y: f32, yaw: f32) -> PlaceContext {
        PlaceContext {
            target: BlockPos::new(0, 64, 0),
            face,
            cursor: Vec3f {
                x: 0.5,
                y: cursor_y,
                z: 0.5,
            },
            yaw: Some(yaw),
            pitch: Some(0.0),
        }
    }

    fn air(_: BlockPos) -> String {
        "minecraft:air".to_string()
    }

    fn state_of(block: &str, face: BlockFace, cursor_y: f32, yaw: f32) -> String {
        placement(block, &ctx(face, cursor_y, yaw), air)
            .unwrap_or_else(|| panic!("no convention for {block}"))
            .state
    }

    /// The three conventions that differ from each other, at one yaw: looking
    /// **north** (yaw 180) a stair faces north, a chest/furnace faces south,
    /// and an anvil faces east.
    #[test]
    fn the_three_horizontal_conventions_disagree_as_the_jar_does() {
        assert_eq!(
            state_of("minecraft:oak_stairs", BlockFace::Up, 0.0, 180.0),
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight]"
        );
        assert_eq!(
            state_of("minecraft:chest", BlockFace::Up, 0.0, 180.0),
            "minecraft:chest[facing=south,type=single]"
        );
        assert_eq!(
            state_of("minecraft:furnace", BlockFace::Up, 0.0, 180.0),
            "minecraft:furnace[facing=south]"
        );
        assert_eq!(
            state_of("minecraft:anvil", BlockFace::Up, 0.0, 180.0),
            "minecraft:anvil[facing=east]"
        );
    }

    /// `half`/`type` come from the click, not from the yaw: the bottom face
    /// always gives the upper half, the top face always the lower, and a
    /// horizontal click splits on the cursor.
    #[test]
    fn the_half_bearing_families_read_the_click() {
        assert_eq!(
            state_of("minecraft:oak_slab", BlockFace::Up, 0.0, 0.0),
            "minecraft:oak_slab[type=bottom]"
        );
        assert_eq!(
            state_of("minecraft:oak_slab", BlockFace::Down, 0.0, 0.0),
            "minecraft:oak_slab[type=top]"
        );
        assert_eq!(
            state_of("minecraft:oak_slab", BlockFace::North, 0.9, 0.0),
            "minecraft:oak_slab[type=top]"
        );
        assert_eq!(
            state_of("minecraft:oak_slab", BlockFace::North, 0.1, 0.0),
            "minecraft:oak_slab[type=bottom]"
        );
        assert!(
            state_of("minecraft:oak_stairs", BlockFace::North, 0.9, 0.0).contains("half=top"),
            "a stair clicked high on a side face is an upper stair"
        );
    }

    /// A slab clicked onto a matching half-slab doubles.
    #[test]
    fn a_slab_on_a_slab_doubles() {
        let existing = |_: BlockPos| "minecraft:oak_slab[type=bottom]".to_string();
        let placed = placement("minecraft:oak_slab", &ctx(BlockFace::Up, 0.0, 0.0), existing).unwrap();
        assert_eq!(placed.state, "minecraft:oak_slab[type=double]");
    }

    /// Vertical-`facing` blocks split three ways, and only one of them reads
    /// the look vector.
    #[test]
    fn the_six_way_family_splits_three_ways() {
        // Looking straight down (pitch 90): a dispenser points back up.
        let mut down = ctx(BlockFace::Up, 0.0, 0.0);
        down.pitch = Some(90.0);
        assert_eq!(
            placement("minecraft:dispenser", &down, air).unwrap().state,
            "minecraft:dispenser[facing=up]"
        );
        // An observer watches where the player looks.
        assert_eq!(
            placement("minecraft:observer", &down, air).unwrap().state,
            "minecraft:observer[facing=down]"
        );
        // A shulker box takes the clicked face regardless.
        assert_eq!(
            placement("minecraft:shulker_box", &down, air).unwrap().state,
            "minecraft:shulker_box[facing=up]"
        );
        // A hopper takes the clicked face's opposite, folded onto `down`.
        assert_eq!(
            placement("minecraft:hopper", &down, air).unwrap().state,
            "minecraft:hopper[facing=down]"
        );
    }

    /// A torch against a wall becomes a different *block*, not a rotated one.
    #[test]
    fn a_wall_click_redirects_to_the_wall_block() {
        assert_eq!(
            state_of("minecraft:torch", BlockFace::North, 0.5, 0.0),
            "minecraft:wall_torch[facing=north]"
        );
        assert_eq!(
            state_of("minecraft:soul_torch", BlockFace::East, 0.5, 0.0),
            "minecraft:soul_wall_torch[facing=east]"
        );
        assert_eq!(
            state_of("minecraft:oak_sign", BlockFace::South, 0.5, 0.0),
            "minecraft:oak_wall_sign[facing=south]"
        );
        assert_eq!(
            state_of("minecraft:oak_hanging_sign", BlockFace::South, 0.5, 0.0),
            "minecraft:oak_wall_hanging_sign[facing=south]"
        );
        assert_eq!(
            state_of("minecraft:skeleton_skull", BlockFace::West, 0.5, 0.0),
            "minecraft:skeleton_wall_skull[facing=west]"
        );
        // A floor click keeps the standing block. A torch has no properties at
        // all, so there is nothing to orient and the caller keeps the bare name.
        assert!(placement("minecraft:torch", &ctx(BlockFace::Up, 0.0, 0.0), air).is_none());
        // A standing sign does have one: the 16-segment yaw. Yaw 0 faces south,
        // and `StandingSignBlock.getStateForPlacement` offsets by 180°.
        assert_eq!(
            state_of("minecraft:oak_sign", BlockFace::Up, 0.0, 0.0),
            "minecraft:oak_sign[rotation=8]"
        );
    }

    /// A door writes both halves, and a bed both ends.
    #[test]
    fn the_two_cell_families_write_their_second_cell() {
        let door = placement("minecraft:oak_door", &ctx(BlockFace::Up, 0.0, 180.0), air).unwrap();
        assert!(door.state.contains("half=lower"), "{}", door.state);
        assert_eq!(door.extra.len(), 1);
        assert_eq!(door.extra[0].0, BlockPos::new(0, 65, 0));
        assert!(door.extra[0].1.contains("half=upper"), "{}", door.extra[0].1);

        // Looking north, the bed's head is the cell to the north.
        let bed = placement("minecraft:red_bed", &ctx(BlockFace::Up, 0.0, 180.0), air).unwrap();
        assert_eq!(bed.state, "minecraft:red_bed[facing=north,part=foot]");
        assert_eq!(
            bed.extra,
            vec![(
                BlockPos::new(0, 64, -1),
                "minecraft:red_bed[facing=north,part=head]".to_string()
            )]
        );
    }

    /// A chest placed beside a single chest of the same facing pairs, and the
    /// partner is re-typed too.
    #[test]
    fn a_chest_pairs_with_its_neighbour() {
        // Looking north → facing south. `facing.clockwise()` is west, so a
        // single south-facing chest to the west makes this one the left half.
        let west = BlockPos::new(-1, 64, 0);
        let neighbour = move |p: BlockPos| {
            if p == west {
                "minecraft:chest[facing=south,type=single]".to_string()
            } else {
                "minecraft:air".to_string()
            }
        };
        let placed = placement("minecraft:chest", &ctx(BlockFace::Up, 0.0, 180.0), neighbour).unwrap();
        assert_eq!(placed.state, "minecraft:chest[facing=south,type=left]");
        assert_eq!(
            placed.extra,
            vec![(west, "minecraft:chest[facing=south,type=right]".to_string())]
        );
    }

    /// Pillars still take their axis from the clicked face, with no yaw at all.
    #[test]
    fn pillars_take_the_clicked_axis() {
        let mut no_angles = ctx(BlockFace::North, 0.5, 0.0);
        no_angles.yaw = None;
        no_angles.pitch = None;
        assert_eq!(
            placement("minecraft:oak_log", &no_angles, air).unwrap().state,
            "minecraft:oak_log[axis=z]"
        );
    }

    /// A block with no orientation at all has no convention, so the caller
    /// keeps the census's bare name.
    #[test]
    fn an_unoriented_block_has_no_convention() {
        assert!(placement("minecraft:dirt", &ctx(BlockFace::Up, 0.0, 0.0), air).is_none());
    }

    /// `NoteBlock.getStateForPlacement` reads the block below when placed over
    /// nothing but air above — the ordinary case, and the one every prior
    /// build got wrong by leaving `instrument` at its bare default (`harp`)
    /// regardless of what was underneath.
    #[test]
    fn a_note_block_placed_on_gold_reads_bell() {
        let below_gold = |pos: BlockPos| {
            if pos.y == 63 {
                "minecraft:gold_block".to_string()
            } else {
                "minecraft:air".to_string()
            }
        };
        let placed = placement("minecraft:note_block", &ctx(BlockFace::Up, 0.0, 0.0), below_gold).unwrap();
        assert_eq!(placed.state, "minecraft:note_block[instrument=bell]");
    }

    /// A mob head already sitting on top wins over the block underneath —
    /// `setInstrument`'s `above.worksAboveNoteBlock()` check runs first.
    #[test]
    fn a_note_block_placed_under_a_skull_reads_the_skull_not_the_floor() {
        let skull_above_gold = |pos: BlockPos| {
            if pos.y == 65 {
                "minecraft:skeleton_skull".to_string()
            } else if pos.y == 63 {
                "minecraft:gold_block".to_string()
            } else {
                "minecraft:air".to_string()
            }
        };
        let placed = placement(
            "minecraft:note_block",
            &ctx(BlockFace::Up, 0.0, 0.0),
            skull_above_gold,
        )
        .unwrap();
        assert_eq!(placed.state, "minecraft:note_block[instrument=skeleton]");
    }

    /// Every state this module can emit must resolve to a real state id — the
    /// one thing no coverage tool can see, because a fully-connected wire
    /// carrying a bogus state string still looks connected. The expected value
    /// is the jar-derived census, not our own resolver.
    #[test]
    fn every_emitted_state_names_real_properties() {
        let blocks = [
            "minecraft:oak_stairs",
            "minecraft:oak_slab",
            "minecraft:chest",
            "minecraft:furnace",
            "minecraft:barrel",
            "minecraft:dispenser",
            "minecraft:anvil",
            "minecraft:oak_door",
            "minecraft:oak_trapdoor",
            "minecraft:oak_log",
            "minecraft:ladder",
            "minecraft:torch",
            "minecraft:red_bed",
            "minecraft:lever",
            "minecraft:stone_button",
            "minecraft:hopper",
            "minecraft:observer",
            "minecraft:end_rod",
            "minecraft:bell",
            "minecraft:rail",
            "minecraft:white_banner",
            "minecraft:note_block",
        ];
        let faces = [
            BlockFace::Up,
            BlockFace::Down,
            BlockFace::North,
            BlockFace::South,
            BlockFace::East,
            BlockFace::West,
        ];
        for block in blocks {
            for face in faces {
                for yaw in [0.0, 90.0, 180.0, -90.0] {
                    let Some(placed) = placement(block, &ctx(face, 0.75, yaw), air) else {
                        continue;
                    };
                    for state in std::iter::once(&placed.state).chain(placed.extra.iter().map(|(_, s)| s)) {
                        assert_state_exists(state);
                    }
                }
            }
        }
    }

    /// Asserts a `block[k=v,…]` string names a block in the 26.2 census and,
    /// for every property it names, a value that block really has.
    fn assert_state_exists(state: &str) {
        let block = base_name(state);
        for id in 0..block_states::STATE_COUNT {
            if block_states::block_name(id) != Some(block) {
                continue;
            }
            let props = block_states::properties(id).unwrap_or(&[]);
            let Some((_, rest)) = state.split_once('[') else {
                return;
            };
            for kv in rest.trim_end_matches(']').split(',') {
                let (k, v) = kv.split_once('=').expect("malformed property");
                assert!(
                    props.iter().any(|&(pk, _)| pk == k),
                    "{block} has no property `{k}` (from `{state}`)"
                );
                assert!(
                    (0..block_states::STATE_COUNT).any(|other| {
                        block_states::block_name(other) == Some(block)
                            && block_states::properties(other)
                                .unwrap_or(&[])
                                .contains(&(k, v))
                    }),
                    "{block}'s `{k}` has no value `{v}` (from `{state}`)"
                );
            }
            return;
        }
        panic!("`{block}` is not a block in the 26.2 census");
    }
}
