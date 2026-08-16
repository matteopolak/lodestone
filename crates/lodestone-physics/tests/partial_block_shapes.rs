//! **What a world made of unit cubes costs, measured.**
//!
//! `lodestone-physics` is bit-exact against two independent oracles across 26
//! zero-tolerance golden traces, so the integrator has never been the suspect.
//! The shell's live [`CollisionView`], however, spent its whole existence
//! emitting *one unit cube per occluding block and nothing else* — so in live
//! play there were **no slabs, stairs, fences, walls, ice, ladders, cobwebs or
//! soul sand in collision at all**. A perfect integrator fed a world where slabs
//! do not exist produces perfectly wrong movement.
//!
//! These are the **controls** for that fix. Each one runs the real `tick` twice
//! over the same terrain: once through a view serving the true block-local
//! shapes (`Shaped`), and once through `CubesOnly`, which reproduces exactly what
//! the shell used to do — any block with collision becomes a full cube. The
//! assertion pair is what makes this evidence rather than description: the
//! `Shaped` run must land on vanilla's number, and the `CubesOnly` run must
//! **fail that same assertion**, by a stated amount.
//!
//! # Why the amounts matter, and are not cosmetic
//!
//! 26.2's server replays our movement delta through `move(MoverType.PLAYER, …)`
//! and rubber-bands whenever horizontal disagreement exceeds **0.25 blocks in a
//! single packet, with no accumulator** (`docs/baritone-port.md` §3.2). The
//! errors measured below are `0.5` blocks on a slab and `1.0` on a fence — 2× and
//! 4× that bar — reached on the *first* tick of contact, not accumulated.
//!
//! Deliberately hermetic: the shapes are hand-written from vanilla's own
//! geometry (a bottom slab is `8/16` tall, a fence post `1.5`), so nothing here
//! depends on the shell, an atlas, a pack, a server or a GPU. The companion gate
//! that checks the shell resolves *the real census* to these same numbers lives
//! in `lodestone-shell`'s `collision::tests`.

use std::collections::HashMap;

use lodestone_physics::{
    Aabb, CollisionView, MovementInput, PhysicsProfile, PlayerState, Vec3d, tick,
};

/// One block-local box, `[min_x, min_y, min_z, max_x, max_y, max_z]`, in the same
/// `0..1`-per-axis space vanilla's `VoxelShape.toAabbs()` uses — except `max_y`,
/// which is uncapped (a fence is `1.5`).
type LocalBox = [f64; 6];

/// A full block.
const CUBE: &[LocalBox] = &[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]];
/// `SlabBlock` bottom half: `Block.box(0, 0, 0, 16, 8, 16)`.
const BOTTOM_SLAB: &[LocalBox] = &[[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]];
/// `SoulSandBlock.SHAPE`: `Block.box(0, 0, 0, 16, 14, 16)` — 14/16 = 0.875, the
/// reason you sink slightly into soul sand.
const SOUL_SAND: &[LocalBox] = &[[0.0, 0.0, 0.0, 1.0, 0.875, 1.0]];
/// A free-standing fence: the post only, `Block.box(6, 0, 6, 10, 24, 10)`. The
/// `24` is the point — **1.5 blocks tall**, which is why a fence cannot be
/// step-mounted even though it looks one block high.
const FENCE_POST: &[LocalBox] = &[[0.375, 0.0, 0.375, 0.625, 1.5, 0.625]];

/// Which shape a cell holds. `Air` is the absence of a cell entirely.
#[derive(Clone, Copy)]
struct Cell(&'static [LocalBox]);

/// How a view turns a cell's shape into collision boxes.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fidelity {
    /// The block's real shape — what the fixed adapter does.
    Shaped,
    /// **The defect.** Any cell with collision becomes a unit cube; anything with
    /// an empty shape disappears. This is precisely the old
    /// `if is_solid(cell) { push(unit cube) }`.
    CubesOnly,
}

struct World {
    cells: HashMap<(i32, i32, i32), Cell>,
    fidelity: Fidelity,
}

impl World {
    fn new(fidelity: Fidelity) -> Self {
        Self {
            cells: HashMap::new(),
            fidelity,
        }
    }

    /// A 21×21 floor of full blocks at `y`, so nothing in these tests ever falls
    /// out of the world or off an edge.
    fn with_floor(mut self, y: i32) -> Self {
        for x in -10..=10 {
            for z in -10..=10 {
                self.cells.insert((x, y, z), Cell(CUBE));
            }
        }
        self
    }

    fn put(mut self, pos: (i32, i32, i32), shape: &'static [LocalBox]) -> Self {
        self.cells.insert(pos, Cell(shape));
        self
    }

    /// Fill a horizontal strip at one `y`, for a wall the player cannot sidestep.
    fn put_row_x(mut self, x: i32, y: i32, shape: &'static [LocalBox]) -> Self {
        for z in -10..=10 {
            self.cells.insert((x, y, z), Cell(shape));
        }
        self
    }
}

impl CollisionView for World {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let Some(cell) = self.cells.get(&(x, y, z)) else {
            return;
        };
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        let shape: &[LocalBox] = match self.fidelity {
            Fidelity::Shaped => cell.0,
            Fidelity::CubesOnly if cell.0.is_empty() => &[],
            Fidelity::CubesOnly => CUBE,
        };
        for b in shape {
            out.push(Aabb::new(
                bx + b[0],
                by + b[1],
                bz + b[2],
                bx + b[3],
                by + b[4],
                bz + b[5],
            ));
        }
    }
}

const STILL: MovementInput = MovementInput {
    forward: 0.0,
    strafe: 0.0,
    jump: false,
    sneak: false,
    sprint: false,
    using_item: None,
};

const WALK_FORWARD: MovementInput = MovementInput {
    forward: 1.0,
    strafe: 0.0,
    jump: false,
    sneak: false,
    sprint: false,
    using_item: None,
};

/// Drop a player from `start` and return where their feet come to rest.
fn settle(view: &World, start: Vec3d, ticks: usize) -> PlayerState {
    let profile = PhysicsProfile::mc_1_21();
    let mut state = PlayerState::at(start, 0.0);
    for _ in 0..ticks {
        tick(&mut state, STILL, view, &profile);
    }
    assert!(
        state.on_ground,
        "fixture error: the player never landed (y = {})",
        state.position.y
    );
    state
}

/// **The headline case.** A player standing on a bottom slab rests at `y + 0.5`.
/// Cube-only collision puts them at `y + 1.0` — half a block in the air, twice
/// the server's 0.25-block rubber-band threshold, on the very first contact.
#[test]
fn a_player_rests_on_top_of_a_bottom_slab_not_on_top_of_a_phantom_cube() {
    let slab_y = 4;
    let start = Vec3d::new(0.5, f64::from(slab_y) + 6.0, 0.5);

    let shaped = World::new(Fidelity::Shaped)
        .with_floor(0)
        .put((0, slab_y, 0), BOTTOM_SLAB);
    let rest = settle(&shaped, start, 60).position.y;
    let expected = f64::from(slab_y) + 0.5;
    assert!(
        (rest - expected).abs() < 1e-9,
        "feet should rest on the slab's 8/16 surface: got {rest}, want {expected}"
    );

    // The control: the same fixture through the old cube-only view. If this
    // *passed* the assertion above, the assertion would prove nothing.
    let cubes = World::new(Fidelity::CubesOnly)
        .with_floor(0)
        .put((0, slab_y, 0), BOTTOM_SLAB);
    let wrong = settle(&cubes, start, 60).position.y;
    assert!(
        (wrong - expected).abs() > 0.25,
        "control did not fire: cube-only collision must NOT land on 0.5 (got {wrong})"
    );
    assert!(
        (wrong - (f64::from(slab_y) + 1.0)).abs() < 1e-9,
        "cube-only collision should stand a full block up, i.e. exactly 0.5 blocks \
         too high: got {wrong}"
    );
}

/// Soul sand's `14/16` shape, the same measurement one notch finer: `0.125`
/// blocks of error is *below* the rubber-band bar per packet, but it is a
/// permanent standing-height offset — the eye, the reach ray and every
/// subsequent step inherit it.
#[test]
fn a_player_sinks_into_soul_sand_by_two_sixteenths() {
    let y = 4;
    let start = Vec3d::new(0.5, f64::from(y) + 6.0, 0.5);

    let shaped = World::new(Fidelity::Shaped)
        .with_floor(0)
        .put((0, y, 0), SOUL_SAND);
    let rest = settle(&shaped, start, 60).position.y;
    assert!(
        (rest - (f64::from(y) + 0.875)).abs() < 1e-9,
        "feet should rest at 14/16: got {rest}"
    );

    let cubes = World::new(Fidelity::CubesOnly)
        .with_floor(0)
        .put((0, y, 0), SOUL_SAND);
    let wrong = settle(&cubes, start, 60).position.y;
    assert!(
        (wrong - rest).abs() > 0.1,
        "control did not fire: cube-only must differ from 14/16 (got {wrong})"
    );
}

/// **The fence.** `collision_top` is contracted *uncapped* precisely so a fence
/// reads as 1.5 blocks tall; the 0.6-block auto-step therefore cannot mount it.
/// A cube-only view reports 1.0, which a step *can* clear — so the client walks
/// over fences the server has it walking into.
#[test]
fn a_fence_is_one_and_a_half_blocks_tall_and_cannot_be_stepped_over() {
    let floor_y = 3;
    let feet = f64::from(floor_y) + 1.0;

    // Uncapped top, straight off the view.
    let shaped =
        World::new(Fidelity::Shaped)
            .with_floor(floor_y)
            .put_row_x(2, floor_y + 1, FENCE_POST);
    assert!(
        (shaped.collision_top(2, floor_y + 1, 0) - 1.5).abs() < 1e-9,
        "a fence's collision_top is 1.5, NOT clamped to 1.0"
    );

    let cubes = World::new(Fidelity::CubesOnly)
        .with_floor(floor_y)
        .put_row_x(2, floor_y + 1, FENCE_POST);
    assert!(
        (cubes.collision_top(2, floor_y + 1, 0) - 1.0).abs() < 1e-9,
        "control: the cube-only view reports a 1-block-tall fence"
    );

    // Walk into the fence line at x = 2 (yaw -90 makes +X forward) and compare
    // where the player is stopped. A fence *post* is 4/16 wide and centred, so
    // its west face is at x = 2.375; a phantom cube's is at x = 2.0. The player's
    // hitbox is 0.6 wide, so the two resting positions are 0.375 blocks apart —
    // and neither view lets the player climb, because both shapes are at least a
    // block tall and the auto-step is 0.6.
    let profile = PhysicsProfile::mc_1_21();
    let mut rest_x = [0.0_f64; 2];
    // Auto-jump off for this measurement: the test isolates *collision-stop
    // fidelity* (the 0.6 auto-step), and a 1.0-tall cube is a height auto-jump
    // legitimately clears — so with it on, the cube-only control would simply
    // jump the phantom and walk on, confounding the very stop the two views are
    // being compared on. Auto-jump's own behaviour is asserted in `golden.rs`.
    for (i, view) in [&shaped, &cubes].into_iter().enumerate() {
        let mut state = PlayerState::at(Vec3d::new(0.5, feet, 0.5), -90.0).with_auto_jump(false);
        for _ in 0..60 {
            tick(&mut state, WALK_FORWARD, view, &profile);
        }
        assert!(
            state.position.y < feet + 0.25,
            "neither shape is step-able: the player must not end up on top of the \
             fence (y = {})",
            state.position.y
        );
        rest_x[i] = state.position.x;
    }

    assert!(
        (rest_x[0] - (2.375 - 0.3)).abs() < 1e-6,
        "with the real post the player stops against x = 2.375 minus half a \
         hitbox: got {}",
        rest_x[0]
    );
    // The control: the cube-only view stops the player 0.375 blocks short —
    // 1.5x the server's per-packet rubber-band threshold, in the horizontal axis
    // the threshold is actually measured on.
    assert!(
        (rest_x[1] - (2.0 - 0.3)).abs() < 1e-6,
        "control: the cube-only view stops the player against x = 2.0: got {}",
        rest_x[1]
    );
    assert!(
        (rest_x[0] - rest_x[1]) > 0.25,
        "control did not fire: the two views must disagree by more than the \
         0.25-block rubber-band bar (got {})",
        rest_x[0] - rest_x[1]
    );
}

/// The *other* direction of the same defect: a shape that is **empty** must not
/// collide at all. Cobweb, kelp and every fluid have no collision boxes, and an
/// adapter that cubed "anything that is not air" would stand the player on top of
/// a cobweb instead of letting them sink into it.
///
/// The control here is the floor: the player must still land on *something*, or a
/// view that simply reported nothing anywhere would satisfy the assertion.
#[test]
fn an_empty_shape_does_not_collide_but_the_floor_still_does() {
    let floor_y = 3;
    // Cobweb occupies the two cells above the floor; its collision shape is empty.
    let view = World::new(Fidelity::Shaped)
        .with_floor(floor_y)
        .put((0, floor_y + 1, 0), &[])
        .put((0, floor_y + 2, 0), &[]);

    let rest = settle(&view, Vec3d::new(0.5, f64::from(floor_y) + 8.0, 0.5), 60)
        .position
        .y;
    assert!(
        (rest - (f64::from(floor_y) + 1.0)).abs() < 1e-9,
        "the player falls through the empty-shaped cells and lands on the floor: \
         got {rest}"
    );
    assert!(
        view.collision_top(0, floor_y + 1, 0) == 0.0,
        "an empty shape has no top"
    );
    assert!(
        view.collision_top(0, floor_y, 0) == 1.0,
        "control: the floor cell underneath does have one"
    );
}

/// `collision_top` is contracted to be derivable from `collision_boxes` — the
/// trait's default does exactly that — so an implementer that overrides it (as
/// the shell now does, to avoid the default's per-call `Vec`) must agree with the
/// default. Pin the equivalence here, where both are cheap to compare.
#[test]
fn collision_top_agrees_with_the_boxes_it_is_derived_from() {
    let y = 4;
    for shape in [CUBE, BOTTOM_SLAB, SOUL_SAND, FENCE_POST, &[]] {
        let view = World::new(Fidelity::Shaped).put((0, y, 0), shape);
        let mut boxes = Vec::new();
        view.collision_boxes(0, y, 0, &mut boxes);
        let from_boxes = boxes
            .iter()
            .map(|b| b.max_y - f64::from(y))
            .fold(0.0_f64, f64::max);
        assert!(
            (view.collision_top(0, y, 0) - from_boxes).abs() < 1e-9,
            "collision_top must equal max(box.max_y) - y for {shape:?}"
        );
    }
}
