//! Collision-shape authority check.
//!
//! `blocks.json` (Mojang's generated report) contains block *states* but **no
//! collision geometry** — shapes are code-defined in vanilla. The community
//! `vendor/minecraft-data/blockCollisionShapes.json` is the only pre-baked
//! table, but its newest entry is **1.21.11**, several releases behind 26.2, and
//! it omits ~30 blocks that exist in 26.2 (cinnabar/sulfur families, etc.), so
//! it is *not* authoritative for our target version.
//!
//! The authoritative source is the game itself: `oracle-java/ShapeOracle.java`
//! bootstraps the real 26.2 server and reads `BlockState.getCollisionShape` for
//! every one of the 32,366 states. A curated, bit-exact subset of that output is
//! checked in as `support/collision_shapes_jvm.txt`; this test parses it and
//! asserts our collision engine reproduces the geometry and the resulting
//! resting/step behaviour exactly. It fails the moment a future version changes
//! a shape we rely on, or someone regresses the collision maths — which is the
//! point: the data-source verdict is enforced, not just documented.

use lodestone_physics::collision::{CollisionView, collide};
use lodestone_physics::geometry::{Aabb, Vec3d};
use lodestone_physics::{EntityDimensions, PhysicsProfile, PlayerState};

const REFERENCE: &str = include_str!("support/collision_shapes_jvm.txt");

/// One authoritative shape: block name, an example global state id, and the
/// block-local collision boxes (each `[minX, minY, minZ, maxX, maxY, maxZ]`).
struct RefShape {
    name: String,
    state_id: u32,
    boxes: Vec<[f64; 6]>,
}

fn parse_reference() -> Vec<RefShape> {
    let mut shapes = Vec::new();
    for line in REFERENCE.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tok = line.split_whitespace();
        let name = tok.next().unwrap().to_string();
        let state_id: u32 = tok.next().unwrap().parse().unwrap();
        let nboxes: usize = tok.next().unwrap().parse().unwrap();
        let bits: Vec<f64> = tok
            .map(|h| f64::from_bits(u64::from_str_radix(h, 16).unwrap()))
            .collect();
        assert_eq!(bits.len(), nboxes * 6, "malformed shape line: {line}");
        let boxes = bits
            .chunks_exact(6)
            .map(|c| [c[0], c[1], c[2], c[3], c[4], c[5]])
            .collect();
        shapes.push(RefShape {
            name,
            state_id,
            boxes,
        });
    }
    shapes
}

fn shape(name: &str, state_id: u32) -> Vec<[f64; 6]> {
    parse_reference()
        .into_iter()
        .find(|s| s.name == name && s.state_id == state_id)
        .unwrap_or_else(|| panic!("missing authoritative shape {name}#{state_id}"))
        .boxes
}

/// A world holding a single block at `cell`, whose authoritative block-local
/// boxes are offset into world space (exactly how `lodestone-world` will map a
/// [`CollisionView`] entry).
struct ShapeWorld {
    cell: (i32, i32, i32),
    boxes: Vec<Aabb>,
    floor_y: Option<i32>,
}

impl ShapeWorld {
    fn new(cell: (i32, i32, i32), local: &[[f64; 6]]) -> Self {
        let (cx, cy, cz) = cell;
        let boxes = local
            .iter()
            .map(|b| {
                Aabb::new(
                    b[0] + f64::from(cx),
                    b[1] + f64::from(cy),
                    b[2] + f64::from(cz),
                    b[3] + f64::from(cx),
                    b[4] + f64::from(cy),
                    b[5] + f64::from(cz),
                )
            })
            .collect();
        Self {
            cell,
            boxes,
            floor_y: None,
        }
    }

    fn with_floor(mut self, y: i32) -> Self {
        self.floor_y = Some(y);
        self
    }
}

impl CollisionView for ShapeWorld {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if (x, y, z) == self.cell {
            out.extend_from_slice(&self.boxes);
        } else if self.floor_y == Some(y) {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }
}

/// Drop the player straight down onto a block placed at cell (0,0,0) and return
/// the bit-exact resting foot height.
fn drop_onto(local: &[[f64; 6]]) -> f64 {
    let profile = PhysicsProfile::mc_1_21();
    let world = ShapeWorld::new((0, 0, 0), local);
    let start = 3.0;
    let state = PlayerState::at(Vec3d::new(0.5, start, 0.5), 0.0);
    let bb = state.bounding_box(&profile);
    let resolved = collide(
        &world,
        Vec3d::new(0.0, -start, 0.0),
        bb,
        false,
        EntityDimensions::PLAYER.step_height,
    );
    start + resolved.y
}

#[test]
fn authoritative_shapes_have_expected_geometry() {
    // Full cube.
    assert_eq!(
        shape("minecraft:stone", 1),
        vec![[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]
    );

    // Bottom slab: top face at exactly 0.5 — this is the value the slab_step
    // golden hard-codes, now anchored to the real 26.2 shape rather than assumed.
    let slab_bottom = shape("minecraft:oak_slab", 13332);
    assert_eq!(slab_bottom, vec![[0.0, 0.0, 0.0, 1.0, 0.5, 1.0]]);
    assert_eq!(slab_bottom[0][4].to_bits(), 0.5f64.to_bits());

    // Top slab: sits in the upper half.
    assert_eq!(
        shape("minecraft:oak_slab", 13330),
        vec![[0.0, 0.5, 0.0, 1.0, 1.0, 1.0]]
    );

    // Soul sand is 0.875 tall — the reason you sink slightly into it.
    assert_eq!(
        shape("minecraft:soul_sand", 6998),
        vec![[0.0, 0.0, 0.0, 1.0, 0.875, 1.0]]
    );

    // Honey block is inset and 0.9375 tall.
    assert_eq!(
        shape("minecraft:honey_block", 21816),
        vec![[0.0625, 0.0, 0.0625, 0.9375, 0.9375, 0.9375]]
    );

    // Fences collide up to y = 1.5 — taller than their 1.0 visual, which is why
    // a 0.6 auto-step can't mount one. Every box in the shape reaches 1.5.
    let fence = shape("minecraft:oak_fence", 6965);
    assert!(fence.iter().all(|b| b[4].to_bits() == 1.5f64.to_bits()));

    // Non-colliding blocks yield zero boxes.
    for (name, id) in [
        ("minecraft:cobweb", 2247u32),
        ("minecraft:water", 86),
        ("minecraft:lava", 102),
    ] {
        assert!(
            shape(name, id).is_empty(),
            "{name} should have no collision"
        );
    }
}

#[test]
fn resting_height_matches_authoritative_top_face() {
    assert_eq!(drop_onto(&shape("minecraft:stone", 1)), 1.0);
    assert_eq!(drop_onto(&shape("minecraft:oak_slab", 13332)), 0.5);
    assert_eq!(drop_onto(&shape("minecraft:soul_sand", 6998)), 0.875);
    assert_eq!(drop_onto(&shape("minecraft:honey_block", 21816)), 0.9375);
    // Bit-exact, not just numerically close.
    assert_eq!(
        drop_onto(&shape("minecraft:oak_slab", 13332)).to_bits(),
        0.5f64.to_bits()
    );
}

#[test]
fn slab_is_steppable_but_fence_is_not() {
    let step = EntityDimensions::PLAYER.step_height;

    // Player standing on a floor at y=1, an obstacle in the adjacent +x cell.
    let feet = 1.0;
    let make = |local: &[[f64; 6]]| {
        let world = ShapeWorld::new((1, 1, 0), local).with_floor(0);
        let bb = Aabb::new(0.2, feet, 0.2, 0.8, feet + 1.8, 0.8);
        collide(&world, Vec3d::new(0.5, 0.0, 0.0), bb, true, step)
    };

    // A bottom slab (top 1.5, i.e. 0.5 above the feet ≤ 0.6 step) is stepped up:
    // the player keeps its full requested x-movement.
    let slab = make(&shape("minecraft:oak_slab", 13332));
    assert_eq!(
        slab.x, 0.5,
        "expected to step onto slab and keep full x-movement, got {slab:?}"
    );

    // A fence (top 2.5, i.e. 1.5 above the feet > 0.6 step) can't be stepped: the
    // player only closes the 0.2 gap to the fence face, well short of the slab.
    let fence = make(&shape("minecraft:oak_fence", 6965));
    assert!(
        fence.x < slab.x && fence.x < 0.3,
        "fence should block the step (partial move only), got {fence:?}"
    );
}

#[test]
fn reference_covers_the_curated_block_set() {
    let shapes = parse_reference();
    for name in [
        "minecraft:stone",
        "minecraft:oak_slab",
        "minecraft:oak_stairs",
        "minecraft:oak_fence",
        "minecraft:glass_pane",
        "minecraft:soul_sand",
        "minecraft:cobweb",
        "minecraft:ladder",
        "minecraft:cobblestone_wall",
        "minecraft:water",
        "minecraft:lava",
        "minecraft:slime_block",
        "minecraft:honey_block",
    ] {
        assert!(
            shapes.iter().any(|s| s.name == name),
            "authoritative reference is missing {name}"
        );
    }
}
