//! The one gate that matters for portals: a **round trip**, from a spawn nowhere
//! near the origin, at coordinates whose eighth is not an integer.
//!
//! # Why the round trip and not the outbound leg
//!
//! A one-way trip is the failure mode that reads as success in a screenshot. Worse,
//! the two legs share one expression (`crate::dimension::teleport_scale` is
//! `from / to`), so the interesting failure is not a missing leg but an *inverted
//! ratio* — and at `x = 0` multiply-by-8, divide-by-8 and doing nothing at all are
//! byte-identical. The spawn here is `x = 1720.5, z = -523.25`, whose eighths
//! (215.0625 and −65.40625) are both non-integers, so a truncation bug cannot hide
//! either.
//!
//! # What this drives, and what it does not
//!
//! It calls `lodestone_server::portal::resolve_destination`, which is the *same*
//! function `crate::server`'s travel path calls — not a re-implementation of it. So
//! this measures production's destination search, its 8:1 scaling and its portal
//! index.
//!
//! It does **not** drive the packet sequence (the `forget_chunk` sweep, the
//! dimension-change respawn, the re-stream). That needs a live connection walking
//! into a portal for 81 ticks; `docs/nether-portals.md` records it as untested.
//!
//! The overworld and Nether here are block-map test worlds rather than real
//! generators, deliberately: what is under test is the *scale and the search*, and a
//! real generator would make the assertion depend on where its terrain happens to
//! leave a gap. `the_nether_source_serves_the_dimensions_full_window` below is the
//! arm that pins the real generator's shape.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_server::dimension::{Dimension, DimensionalSource, teleport_scale};
use lodestone_server::portal::{self, Axis, PortalIndex};
use lodestone_server::{ChunkColumn, ChunkSource};

/// The spawn. **Not the origin, and not a multiple of eight** — see the module doc.
const SPAWN_X: f64 = 1720.5;
const SPAWN_Y: f64 = 96.0;
const SPAWN_Z: f64 = -523.25;

/// A block-map world with a solid floor, so the portal site search has somewhere to
/// stand a frame.
struct TestWorld {
    dimension: Dimension,
    floor_top: i32,
    filler: &'static str,
    blocks: Mutex<HashMap<(i32, i32, i32), String>>,
}

impl TestWorld {
    fn new(dimension: Dimension, floor_top: i32, filler: &'static str) -> Self {
        Self {
            dimension,
            floor_top,
            filler,
            blocks: Mutex::new(HashMap::new()),
        }
    }

    fn put(&self, x: i32, y: i32, z: i32, state: &str) {
        self.blocks
            .lock()
            .unwrap()
            .insert((x, y, z), state.to_owned());
    }

    /// An obsidian frame with a `width × height` interior whose lower-left interior
    /// cell is `(x, y, z)`, in the plane of `axis`.
    fn frame(&self, x: i32, y: i32, z: i32, axis: Axis, width: i32, height: i32) {
        let (ax, az) = match axis {
            Axis::X => (1, 0),
            Axis::Z => (0, 1),
        };
        for across in -1..=width {
            for up in -1..=height {
                if across == -1 || across == width || up == -1 || up == height {
                    self.put(x + ax * across, y + up, z + az * across, "minecraft:obsidian");
                }
            }
        }
    }
}

/// A shared handle to one [`TestWorld`], so both the linked wrapper and the test
/// body can read the same block map. `ChunkSource` is implemented on the `Arc`
/// rather than the world being cloned, because the whole point is that a portal
/// written through one handle is visible through the other.
impl ChunkSource for SharedWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.0.column(cx, cz)
    }
    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.0.block_state(x, y, z)
    }
    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.0.set_block(x, y, z, name);
    }
    fn dimension(&self) -> Option<Dimension> {
        self.0.dimension()
    }
}

#[derive(Clone)]
struct SharedWorld(Arc<TestWorld>);

impl std::ops::Deref for SharedWorld {
    type Target = TestWorld;
    fn deref(&self) -> &TestWorld {
        &self.0
    }
}

impl ChunkSource for TestWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(self.dimension.min_y(), self.dimension.height())
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        if let Some(state) = self.blocks.lock().unwrap().get(&(x, y, z)) {
            return state.clone();
        }
        if y <= self.floor_top && y >= self.dimension.min_y() {
            return self.filler.to_owned();
        }
        "minecraft:air".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.put(x, y, z, name);
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(self.dimension)
    }
}

/// Builds the production seam: an overworld source that can reach a Nether one,
/// sharing one portal index, wired through `DimensionalSource` exactly as
/// `crate::integrated`'s `with_nether` does.
fn linked_worlds() -> (DimensionalSource<SharedWorld>, SharedWorld, PortalIndex) {
    let overworld = SharedWorld(Arc::new(TestWorld::new(
        Dimension::Overworld,
        63,
        "minecraft:stone",
    )));
    let nether = SharedWorld(Arc::new(TestWorld::new(
        Dimension::Nether,
        31,
        "minecraft:netherrack",
    )));
    let portals = PortalIndex::new();

    let nether_for_factory = nether.clone();
    let portals_for_factory = portals.clone();
    let factory: lodestone_server::dimension::SiblingFactory =
        Arc::new(move |dimension| match dimension {
            Dimension::Nether => Some(Arc::new(DimensionalSource::alone(
                nether_for_factory.clone(),
                Dimension::Nether,
                portals_for_factory.clone(),
            )) as Arc<dyn ChunkSource>),
            Dimension::Overworld | Dimension::End => None,
        });

    let linked = DimensionalSource::with_siblings(
        overworld,
        Dimension::Overworld,
        factory,
        portals.clone(),
    );
    (linked, nether, portals)
}

/// Overworld → Nether → overworld, landing back in the portal we left.
///
/// Four claims, and each one fails under a different bug:
///
/// * the Nether arrival is the overworld position **divided** by 8 (within the site
///   search's own 16-block reach), which fails under an inverted ratio;
/// * the return arrival is the overworld position again — *exactly*, because the
///   index and the 128-block overworld search find the portal we lit rather than
///   building a second one. This is the claim a one-way implementation cannot make;
/// * the return trip **creates nothing**, which is the difference between "the search
///   works" and "a new portal every trip";
/// * the created Nether portal is below the bedrock roof at 127.
#[test]
fn a_portal_round_trips_from_a_spawn_far_from_the_origin() {
    let (linked, nether, portals) = linked_worlds();

    // Light a portal at the spawn. `ignite` is production's, so this is also the arm
    // that proves a frame built by hand is one the frame search accepts.
    let feet = (
        SPAWN_X.floor() as i32,
        SPAWN_Y.floor() as i32,
        SPAWN_Z.floor() as i32,
    );
    let overworld_frame_bottom = feet;
    linked.primary().frame(
        overworld_frame_bottom.0,
        overworld_frame_bottom.1,
        overworld_frame_bottom.2,
        Axis::X,
        2,
        3,
    );
    let lit = portal::ignite(
        linked.primary(),
        Dimension::Overworld,
        lodestone_model::BlockPos::new(feet.0, feet.1, feet.2),
    )
    .expect("the hand-built frame must be a valid portal");
    for (pos, state) in &lit {
        linked.primary().set_block(pos.x, pos.y, pos.z, state);
    }
    portals.extend(Dimension::Overworld, lit.iter().map(|(pos, _)| *pos));

    // ---- outbound ----
    let destination = linked
        .sibling(Dimension::Nether)
        .expect("the linked world reaches the Nether");
    let outbound = portal::resolve_destination(
        &*destination,
        Dimension::Overworld,
        Dimension::Nether,
        Some(&portals),
        (SPAWN_X, SPAWN_Y, SPAWN_Z),
        Axis::X,
    )
    .expect("the Nether has a placeable band");
    let created = outbound
        .created
        .as_ref()
        .expect("an empty Nether must have a portal built for it");
    for (pos, block) in &created.blocks {
        nether.set_block(pos.x, pos.y, pos.z, block);
    }
    portals.extend(Dimension::Nether, created.portal_cells.iter().copied());

    // The scaled target, computed here from outside constants rather than read back
    // out of the code under test.
    let expected_nether_x = (SPAWN_X / 8.0).floor();
    let expected_nether_z = (SPAWN_Z / 8.0).floor();
    assert_eq!(expected_nether_x, 215.0, "1720.5 / 8 floors to 215");
    assert_eq!(expected_nether_z, -66.0, "-523.25 / 8 floors to -66");
    // `create_portal` may walk up to 16 columns from the scaled point looking for a
    // site, so the bound is the search radius, not zero.
    assert!(
        (outbound.position.x - expected_nether_x).abs() <= 17.0,
        "Nether arrival x {} is not near {expected_nether_x} — an inverted scale \
         would put it at {}",
        outbound.position.x,
        SPAWN_X * 8.0
    );
    assert!(
        (outbound.position.z - expected_nether_z).abs() <= 17.0,
        "Nether arrival z {} is not near {expected_nether_z}",
        outbound.position.z
    );
    assert!(
        outbound.position.y + 3.0 <= f64::from(Dimension::Nether.max_placeable_y()),
        "the created portal's top at {} is at or above the Nether's ceiling {}",
        outbound.position.y + 3.0,
        Dimension::Nether.max_placeable_y()
    );

    // ---- return ----
    let return_trip = portal::resolve_destination(
        linked.primary(),
        Dimension::Nether,
        Dimension::Overworld,
        Some(&portals),
        (
            outbound.position.x,
            outbound.position.y,
            outbound.position.z,
        ),
        created.axis,
    )
    .expect("the overworld has a placeable band");

    assert!(
        return_trip.created.is_none(),
        "the return trip must find the portal we lit, not build a second one beside it"
    );
    // The portal we lit has its lower-left interior cell at `overworld_frame_bottom`,
    // and an arrival lands at the bottom-centre of that rectangle.
    assert_eq!(
        (return_trip.position.x, return_trip.position.z),
        (
            f64::from(overworld_frame_bottom.0) + 0.5,
            f64::from(overworld_frame_bottom.2) + 0.5,
        ),
        "the round trip must land back in the portal it left"
    );
    assert_eq!(
        return_trip.position.y,
        f64::from(overworld_frame_bottom.1),
        "and at its floor"
    );
}

/// The control for the gate above: at the origin, every scale hypothesis agrees.
///
/// This is not a redundant test — it is the evidence that the coordinates chosen
/// above are load-bearing. Without it, "we spawned far from the origin" is an
/// assertion in a doc comment; with it, the *reason* is executable.
#[test]
fn at_the_origin_the_scale_cannot_be_measured_at_all() {
    let identity = 0.0_f64;
    let out = teleport_scale(Dimension::Overworld, Dimension::Nether);
    let back = teleport_scale(Dimension::Nether, Dimension::Overworld);
    assert_ne!(out, back, "the two directions are genuinely different ratios");
    assert_eq!(identity * out, identity * back);
    assert_eq!(identity * out, identity, "and both agree with doing nothing");

    // The chosen spawn separates all three.
    assert_ne!(SPAWN_X * out, SPAWN_X * back);
    assert_ne!(SPAWN_X * out, SPAWN_X);
    assert!(
        (SPAWN_X * out).fract() != 0.0,
        "the spawn's eighth must not be an integer, or a truncation bug hides"
    );
}

/// The real Nether generator's columns are the **dimension's** 256 rows, not the
/// generator's 128 — the property that decides whether a Nether chunk decodes on a
/// client at all.
///
/// `#[ignore]`d: it builds a real `NetherGenerator`, which parses the whole
/// `noise_settings/nether` document tree.
///
/// ```text
/// cargo test -p lodestone-server --test nether_portal_round_trip -- --ignored --nocapture
/// ```
#[test]
#[ignore = "builds a real Nether generator"]
fn the_nether_source_serves_the_dimensions_full_window() {
    let source = lodestone_server::NetherChunkSource::new(
        lodestone_server::nether_generator(-195_764_831),
    );
    assert_eq!(source.min_y(), Dimension::Nether.min_y());
    assert_eq!(
        source.height(),
        Dimension::Nether.height(),
        "the served window is the dimension type's height, not the generator's"
    );

    let column = source.column(4, -7);
    assert_eq!(column.min_y, 0);
    assert_eq!(column.height, 256);
    assert_eq!(column.section_count(), 16, "16 sections, as the client expects");

    // Real terrain below 128, and nothing but air above it. Both halves matter: the
    // first says the generator ran, the second says the padding is padding.
    let solid_below = (0..128)
        .filter(|&y| column.block_state(8, y, 8) != "minecraft:air")
        .count();
    assert!(
        solid_below > 0,
        "a Nether column with no non-air block below 128 means the generator did not run"
    );
    let solid_above = (128..256)
        .filter(|&y| column.block_state(8, y, 8) != "minecraft:air")
        .count();
    assert_eq!(
        solid_above, 0,
        "the rows above the generator's 128 must be air, not a repeated top row"
    );
}
