//! **Mineshafts** — the piece generator and all four piece kinds,
//! and the first structure in this engine whose pieces are generated *eagerly*.
//!
//! # What it is
//!
//! An implementation of the reference piece-tree generator and the four piece
//! kinds (room, corridor, crossing,
//! stairs), producing the same [`StructurePiece`] list the rest of the
//! structure engine consumes. Two structures ride on it: `minecraft:mineshaft` and
//! `minecraft:mineshaft_mesa`, which differ only in three block states
//! (the mineshaft's wood/material type).
//!
//! # How it works, and why it is a second engine rather than a third generator
//!
//! Every other coded structure so far is a single-piece structure: one box, one
//! block-writing walk, and a generation-point search that draws nothing. A mineshaft is the
//! opposite of that on both axes, and the two facts compound.
//!
//! **The pieces come first.** The generation-point search returns
//! the whole finished builder — the piece tree is grown *before* the biome filter runs
//! and the start's own Y is the answer of moving the whole tree below sea level, which is a function of the
//! tree's total height. So the structure's generation point cannot be computed
//! without generating pieces, which is the exact inversion of the lazy-stub rule the
//! rest of [`super`] is built on. [`generate`] therefore does the whole job and
//! [`super::Stub::Mineshaft`] carries the finished list across the biome check.
//!
//! **The tree is grown in two passes, boxes then blocks.** The child-placement walk recurses to
//! depth 8, each candidate box tested against every piece placed so far
//! (a collision-piece search), and *then* the whole set is shifted vertically by
//! the move-below-sea-level step. Blocks cannot be resolved during the first pass because
//! every one of them would move. [`Shaft`] is that first pass and
//! [`Shaft::into_pieces`] is the second.
//!
//! ```text
//! generate(cx, cz, seed, ctx, mesa, blocking) :
//!     random.next_double()                     <- discarded; shifts the stream
//!     Shaft::room(...)  +  add_children()      <- boxes only, depth <= 8
//!     move_below_sea_level / mesa's surface pick
//!     into_pieces(ctx, &mut random)             <- block-writing walk, in list order
//! ```
//!
//! # How to change it
//!
//! * **The RNG order is the specification and the two passes share one stream.**
//!   The child-placement walk's draws interleave with the random-shaft-piece pick's
//!   draw bounded by 100, with the corridor-size search's draw bounded by 3 and with the per-child
//!   recursion, so a reordering that looks like a tidy-up builds a different
//!   mineshaft. The block-writing walk then continues from the same stream — see the
//!   deviation note below.
//! * **A block read sees what earlier pieces wrote.** [`Shaft::into_pieces`] holds
//!   one [`View`] for the whole start, so a corridor's double lower/upper support placement
//!   sees the planks the same corridor laid two statements earlier, and a crossing's
//!   support-pillar placement sees the terrain above it. Generating pieces into
//!   independent block lists would break both.
//! * **The replaceability test is overridden for every mineshaft piece** and is the reason a
//!   corridor's `cave_air` sweep does not erase the supports of the corridor it
//!   crosses. [`View::can_be_replaced`] is that override; dropping it produces a
//!   mineshaft with no woodwork wherever two pieces touch.
//!
//! # Deviations, all three of them the same shape
//!
//! A faithful implementation runs its block-writing walk once **per decorating chunk**, with that chunk's own
//! feature random, and clips every write to that chunk's own box. A corridor spanning two
//! chunks therefore draws its cobwebs twice, from two unrelated streams, and keeps
//! whichever half landed. There is no single deterministic answer to reproduce, exactly as
//! `swamp_hut`'s average ground height had none. Resolved eagerly here, once:
//!
//! | reference behaviour | here | ledger row |
//! |---|---|---|
//! | the block-writing walk's random is the decorating chunk's | the structure's own stream, continuing after piece layout | `coded:region_random` |
//! | the invalid-location check clamps its shell walk to the decorating chunk's box, so a piece can be invalid in one chunk and valid in another | the walk covers the whole inflated box, once | `mineshaft:invalid_location_scope` |
//! | the sturdy-neighbours check / block placement skip positions outside the decorating chunk's box | no chunk gate; `structure_place_stage` clips instead | as above |
//!
//! # Dependencies
//!
//! [`StartContext`] for column heights and the four-way
//! [`BlockKind`](crate::aquifer::BlockKind), and
//! [`super::template::BlockState`] for the mirror/rotate transform block placement
//! applies.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;

use crate::aquifer::BlockKind;

use super::coded::Facing;
use super::template::{BlockState, Mirror, Rotation};
use super::{
    BoundingBox, CodedBlock, HeightmapKind, StartContext, StructurePiece, free_height,
};

/// The deepest a child piece may recurse.
const MAX_DEPTH: i32 = 8;
/// The absolute-distance-from-the-start-box-min-X bound a new piece's foot must
/// stay within.
const MAX_SPREAD: i32 = 80;
/// Every box is laid out at this Y and then
/// moved.
const MAGIC_START_Y: i32 = 50;
/// The tallest a support pillar may rise.
const MAX_PILLAR_HEIGHT: i32 = 20;
/// The tallest a support chain may rise.
const MAX_CHAIN_HEIGHT: i32 = 50;
/// The abandoned-mineshaft loot table id.
const MINESHAFT_LOOT: &str = "minecraft:chests/abandoned_mineshaft";

/// A mineshaft's wood/material type — the three states, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wood {
    /// Oak.
    Normal,
    /// Dark oak, and the type whose vertical placement samples the
    /// surface instead of moving below sea level.
    Mesa,
}

impl Wood {
    fn log(self) -> &'static str {
        match self {
            Self::Normal => "minecraft:oak_log[axis=y]",
            Self::Mesa => "minecraft:dark_oak_log[axis=y]",
        }
    }

    fn planks(self) -> &'static str {
        match self {
            Self::Normal => "minecraft:oak_planks",
            Self::Mesa => "minecraft:dark_oak_planks",
        }
    }

    /// The fence's *block name* only: support placement sets `west`/`east` on it, so
    /// the caller spells the properties.
    fn fence_name(self) -> &'static str {
        match self {
            Self::Normal => "minecraft:oak_fence",
            Self::Mesa => "minecraft:dark_oak_fence",
        }
    }

    fn fence(self) -> String {
        format!(
            "{}[east=false,north=false,south=false,waterlogged=false,west=false]",
            self.fence_name()
        )
    }
}

/// Which piece a slot in [`Shaft::pieces`] holds, plus the per-kind facts its
/// block-writing walk reads.
#[derive(Debug, Clone)]
enum Kind {
    /// A room. Carries its child entrance boxes, which the vertical-move step shifts along
    /// with the box — the one piece here with mutable state beyond its box.
    Room { entrances: Vec<BoundingBox> },
    /// A corridor.
    Corridor {
        has_rails: bool,
        spider_corridor: bool,
        sections: i32,
    },
    /// A crossing. `direction` is a *field*, not the piece's orientation
    /// — a crossing never sets an orientation, so its block-writing walk
    /// works in absolute coordinates.
    Crossing { direction: Facing, two_floored: bool },
    /// A stairs piece.
    Stairs,
}

/// One node of the tree, before blocks.
#[derive(Debug, Clone)]
struct Node {
    box_: BoundingBox,
    /// The piece's fixed orientation, or `None` for the room and the crossing — both
    /// of which leave `orientation` null and therefore address blocks absolutely.
    orientation: Option<Facing>,
    gen_depth: i32,
    kind: Kind,
}

impl Node {
    /// The local-to-world position transform, including the null-orientation identity both the room and
    /// the crossing depend on.
    fn world_pos(&self, x: i32, y: i32, z: i32) -> [i32; 3] {
        let Some(orientation) = self.orientation else {
            return [x, y, z];
        };
        let wx = match orientation {
            Facing::North | Facing::South => self.box_.min[0] + x,
            Facing::West => self.box_.max[0] - z,
            Facing::East => self.box_.min[0] + z,
        };
        let wz = match orientation {
            Facing::North => self.box_.max[2] - z,
            Facing::South => self.box_.min[2] + z,
            Facing::West | Facing::East => self.box_.min[2] + x,
        };
        [wx, y + self.box_.min[1], wz]
    }

    /// The orientation's `(mirror, rotation)` table, or the null-orientation
    /// identity.
    fn transform(&self) -> (Mirror, Rotation) {
        match self.orientation {
            None => (Mirror::None, Rotation::None),
            Some(Facing::South) => (Mirror::LeftRight, Rotation::None),
            Some(Facing::West) => (Mirror::LeftRight, Rotation::Cw90),
            Some(Facing::East) => (Mirror::None, Rotation::Cw90),
            Some(Facing::North) => (Mirror::None, Rotation::None),
        }
    }

    fn piece_id(&self) -> &'static str {
        match self.kind {
            Kind::Room { .. } => "minecraft:msroom",
            Kind::Corridor { .. } => "minecraft:mscorridor",
            Kind::Crossing { .. } => "minecraft:mscrossing",
            Kind::Stairs => "minecraft:msstairs",
        }
    }
}

/// The piece tree under construction — a piece-builder, narrowed to what
/// a mineshaft asks of it.
#[derive(Debug)]
pub struct Shaft {
    pieces: Vec<Node>,
    wood: Wood,
}

impl Shaft {
    /// `findCollisionPiece(box)` — the first piece whose box intersects, or none.
    ///
    /// Linear over every piece placed so far, exactly as vanilla's is. The order is
    /// load-bearing only in that it decides *which* piece is reported, and nothing
    /// here reads the identity, but the **count** of pieces walked is not: a
    /// candidate is rejected on the first intersection, so a set built in a
    /// different order can accept a different candidate.
    fn collides(&self, candidate: BoundingBox) -> bool {
        self.pieces.iter().any(|p| p.box_.intersects(candidate))
    }

    fn add(&mut self, node: Node) -> usize {
        self.pieces.push(node);
        self.pieces.len() - 1
    }

    /// `getBoundingBox()` — the union. Never empty: the room is added first.
    fn bounding_box(&self) -> BoundingBox {
        self.pieces
            .iter()
            .map(|p| p.box_)
            .reduce(BoundingBox::encapsulate)
            .unwrap_or(BoundingBox {
                min: [0, MAGIC_START_Y, 0],
                max: [0, MAGIC_START_Y, 0],
            })
    }

    /// Shifts every piece's box by `dy`, including the room's own override that
    /// shifts the entrance boxes too.
    fn offset_vertically(&mut self, dy: i32) {
        for piece in &mut self.pieces {
            piece.box_.min[1] += dy;
            piece.box_.max[1] += dy;
            if let Kind::Room { entrances } = &mut piece.kind {
                for entrance in entrances {
                    entrance.min[1] += dy;
                    entrance.max[1] += dy;
                }
            }
        }
    }

    /// Moves the whole shaft's box below sea level, offset by `offset`.
    ///
    /// The random draw is conditional on the shaft fitting under `max_y`, so a
    /// tall shaft consumes no draw here — which is why this cannot be hoisted out
    /// of the branch.
    fn move_below_sea_level<R: RandomSource>(
        &mut self,
        sea_level: i32,
        min_y: i32,
        random: &mut R,
        offset: i32,
    ) -> i32 {
        let max_y = sea_level - offset;
        let box_ = self.bounding_box();
        // The box's Y span is `maxY - minY + 1`.
        let mut y1 = (box_.max[1] - box_.min[1] + 1) + min_y + 1;
        if y1 < max_y {
            y1 += random.next_int_bounded(max_y - y1);
        }
        let dy = y1 - box_.max[1];
        self.offset_vertically(dy);
        dy
    }
}

/// A mineshaft's generation-point search plus piece generation, whole.
///
/// Returns the finished piece list and the start position a faithful stub reports
/// — the chunk's middle block X, `50 + dy`, the chunk's min block Z, which is *not* the chunk middle in Z and
/// is not the room's own corner either.
#[must_use]
pub fn generate<R: RandomSource>(
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    wood: Wood,
    blocking_biomes: &std::collections::HashSet<String>,
    random: &mut R,
) -> (Vec<StructurePiece>, [i32; 3]) {
    // One discarded double draw — the canonical
    // stream-shifting trap. Without it every draw below lands one value early.
    let _ = random.next_double();
    let mut shaft = Shaft {
        pieces: Vec::new(),
        wood,
    };
    // The starting room's corner is at local (2, 2) of the chunk, and its three
    // spans are three draws in this order: maxX, maxY, maxZ.
    let west = cx * 16 + 2;
    let north = cz * 16 + 2;
    let max_x = west + 7 + random.next_int_bounded(6);
    let max_y = 54 + random.next_int_bounded(6);
    let max_z = north + 7 + random.next_int_bounded(6);
    let root = shaft.add(Node {
        box_: BoundingBox {
            min: [west, MAGIC_START_Y, north],
            max: [max_x, max_y, max_z],
        },
        orientation: None,
        gen_depth: 0,
        kind: Kind::Room {
            entrances: Vec::new(),
        },
    });
    let start_box = shaft.pieces[root].box_;
    add_children(&mut shaft, root, start_box, random);

    let sea_level = ctx.sea_level();
    let dy = if wood == Wood::Mesa {
        // The box centre is `min + (max - min + 1) / 2` per axis, the same asymmetric
        // rounding `jigsaw::reference_position` documents.
        let box_ = shaft.bounding_box();
        let centre = [
            box_.min[0] + (box_.max[0] - box_.min[0] + 1) / 2,
            box_.min[1] + (box_.max[1] - box_.min[1] + 1) / 2,
            box_.min[2] + (box_.max[2] - box_.min[2] + 1) / 2,
        ];
        // The free-height convention: one above the topmost occupied cell.
        let surface = free_height(ctx, centre[0], centre[2], HeightmapKind::WorldSurfaceWg);
        let target = if surface <= sea_level {
            sea_level
        } else {
            // An inclusive-both-ends draw between sea level and the surface.
            random.next_int_bounded(surface - sea_level + 1) + sea_level
        };
        let dy = target - centre[1];
        shaft.offset_vertically(dy);
        dy
    } else {
        shaft.move_below_sea_level(sea_level, ctx.min_y(), random, 10)
    };

    let pieces = into_pieces(shaft, ctx, blocking_biomes, random);
    (
        pieces,
        [cx * 16 + 8, MAGIC_START_Y + dy, cz * 16],
    )
}

/// The spread bound, then
/// a random shaft piece pick, then recursion.
fn generate_and_add<R: RandomSource>(
    shaft: &mut Shaft,
    start_box: BoundingBox,
    random: &mut R,
    foot: [i32; 3],
    direction: Facing,
    depth: i32,
) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    if (foot[0] - start_box.min[0]).abs() > MAX_SPREAD
        || (foot[2] - start_box.min[2]).abs() > MAX_SPREAD
    {
        return None;
    }
    let index = create_random_shaft_piece(shaft, random, foot, direction, depth + 1)?;
    add_children(shaft, index, start_box, random);
    Some(index)
}

/// Picks and builds a random shaft piece kind at `foot`.
///
/// One draw bounded by 100 decides the family, and only the chosen family's own
/// candidate search draws anything — so a rejected candidate still costs the family's own draws.
fn create_random_shaft_piece<R: RandomSource>(
    shaft: &mut Shaft,
    random: &mut R,
    foot: [i32; 3],
    direction: Facing,
    gen_depth: i32,
) -> Option<usize> {
    let selection = random.next_int_bounded(100);
    if selection >= 80 {
        let box_ = find_crossing(shaft, random, foot, direction)?;
        let two_floored = box_.max[1] - box_.min[1] + 1 > 3;
        Some(shaft.add(Node {
            box_,
            orientation: None,
            gen_depth,
            kind: Kind::Crossing {
                direction,
                two_floored,
            },
        }))
    } else if selection >= 70 {
        let box_ = find_stairs(shaft, foot, direction)?;
        Some(shaft.add(Node {
            box_,
            orientation: Some(direction),
            gen_depth,
            kind: Kind::Stairs,
        }))
    } else {
        let box_ = find_corridor_size(shaft, random, foot, direction)?;
        // The corridor's own has-rails/is-spider draws happen *after* the
        // corridor-size search, and
        // both come out of the same stream: two draws for an accepted corridor,
        // none for a rejected one.
        let has_rails = random.next_int_bounded(3) == 0;
        let spider_corridor = !has_rails && random.next_int_bounded(23) == 0;
        let sections = if direction.is_z_axis() {
            (box_.max[2] - box_.min[2] + 1) / 5
        } else {
            (box_.max[0] - box_.min[0] + 1) / 5
        };
        Some(shaft.add(Node {
            box_,
            orientation: Some(direction),
            gen_depth,
            kind: Kind::Corridor {
                has_rails,
                spider_corridor,
                sections,
            },
        }))
    }
}

fn moved(min: [i32; 3], max: [i32; 3], foot: [i32; 3]) -> BoundingBox {
    BoundingBox {
        min: [min[0] + foot[0], min[1] + foot[1], min[2] + foot[2]],
        max: [max[0] + foot[0], max[1] + foot[1], max[2] + foot[2]],
    }
}

/// A corridor's candidate-length search — up to three candidate lengths, longest
/// first, one draw bounded by 3 total.
fn find_corridor_size<R: RandomSource>(
    shaft: &Shaft,
    random: &mut R,
    foot: [i32; 3],
    direction: Facing,
) -> Option<BoundingBox> {
    let mut length = random.next_int_bounded(3) + 2;
    while length > 0 {
        let blocks = length * 5;
        let candidate = match direction {
            Facing::South => moved([0, 0, 0], [2, 2, blocks - 1], foot),
            Facing::West => moved([-(blocks - 1), 0, 0], [0, 2, 2], foot),
            Facing::East => moved([0, 0, 0], [blocks - 1, 2, 2], foot),
            Facing::North => moved([0, 0, -(blocks - 1)], [2, 2, 0], foot),
        };
        if !shaft.collides(candidate) {
            return Some(candidate);
        }
        length -= 1;
    }
    None
}

/// A crossing's candidate search — the draw bounded by 4 that decides a two-floored
/// crossing is drawn **before** the collision test, so it is spent either way.
fn find_crossing<R: RandomSource>(
    shaft: &Shaft,
    random: &mut R,
    foot: [i32; 3],
    direction: Facing,
) -> Option<BoundingBox> {
    let y1 = if random.next_int_bounded(4) == 0 { 6 } else { 2 };
    let candidate = match direction {
        Facing::South => moved([-1, 0, 0], [3, y1, 4], foot),
        Facing::West => moved([-4, 0, -1], [0, y1, 3], foot),
        Facing::East => moved([0, 0, -1], [4, y1, 3], foot),
        Facing::North => moved([-1, 0, -4], [3, y1, 0], foot),
    };
    if shaft.collides(candidate) {
        None
    } else {
        Some(candidate)
    }
}

/// A stairs piece's candidate search — no RNG at all.
fn find_stairs(shaft: &Shaft, foot: [i32; 3], direction: Facing) -> Option<BoundingBox> {
    let candidate = match direction {
        Facing::South => moved([0, -5, 0], [2, 2, 8], foot),
        Facing::West => moved([-8, -5, 0], [0, 2, 2], foot),
        Facing::East => moved([0, -5, 0], [8, 2, 2], foot),
        Facing::North => moved([0, -5, -8], [2, 2, 0], foot),
    };
    if shaft.collides(candidate) {
        None
    } else {
        Some(candidate)
    }
}

/// Grows a piece's children, dispatched on the piece kind.
fn add_children<R: RandomSource>(
    shaft: &mut Shaft,
    index: usize,
    start_box: BoundingBox,
    random: &mut R,
) {
    let node = shaft.pieces[index].clone();
    match node.kind {
        Kind::Room { .. } => room_children(shaft, index, node.box_, node.gen_depth, start_box, random),
        Kind::Corridor { .. } => {
            corridor_children(shaft, node.box_, node.orientation, node.gen_depth, start_box, random);
        }
        Kind::Crossing {
            direction,
            two_floored,
        } => crossing_children(
            shaft,
            node.box_,
            direction,
            two_floored,
            node.gen_depth,
            start_box,
            random,
        ),
        Kind::Stairs => {
            stairs_children(shaft, node.box_, node.orientation, node.gen_depth, start_box, random);
        }
    }
}

/// A corridor's own children growth.
fn corridor_children<R: RandomSource>(
    shaft: &mut Shaft,
    box_: BoundingBox,
    orientation: Option<Facing>,
    depth: i32,
    start_box: BoundingBox,
    random: &mut R,
) {
    let Some(orientation) = orientation else {
        return;
    };
    // Drawn unconditionally, before the orientation switch.
    let end = random.next_int_bounded(4);
    // The `minY - 1 + nextInt(3)` in every arm is one draw and it is inside the
    // chosen arm, so exactly one of the three happens.
    let jitter = |random: &mut R| box_.min[1] - 1 + random.next_int_bounded(3);
    match orientation {
        Facing::North => {
            if end <= 1 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0], y, box_.min[2] - 1], Facing::North, depth);
            } else if end == 2 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0] - 1, y, box_.min[2]], Facing::West, depth);
            } else {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.max[0] + 1, y, box_.min[2]], Facing::East, depth);
            }
        }
        Facing::South => {
            if end <= 1 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0], y, box_.max[2] + 1], Facing::South, depth);
            } else if end == 2 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0] - 1, y, box_.max[2] - 3], Facing::West, depth);
            } else {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.max[0] + 1, y, box_.max[2] - 3], Facing::East, depth);
            }
        }
        Facing::West => {
            if end <= 1 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0] - 1, y, box_.min[2]], Facing::West, depth);
            } else if end == 2 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0], y, box_.min[2] - 1], Facing::North, depth);
            } else {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.min[0], y, box_.max[2] + 1], Facing::South, depth);
            }
        }
        Facing::East => {
            if end <= 1 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.max[0] + 1, y, box_.min[2]], Facing::East, depth);
            } else if end == 2 {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.max[0] - 3, y, box_.min[2] - 1], Facing::North, depth);
            } else {
                let y = jitter(random);
                generate_and_add(shaft, start_box, random, [box_.max[0] - 3, y, box_.max[2] + 1], Facing::South, depth);
            }
        }
    }

    if depth < MAX_DEPTH {
        // The side branches: one `nextInt(5)` per 5-block section, and only two of
        // its five values place anything — so three fifths of these draws exist
        // solely to advance the stream.
        if orientation.is_z_axis() {
            let mut z = box_.min[2] + 3;
            while z + 3 <= box_.max[2] {
                let selection = random.next_int_bounded(5);
                if selection == 0 {
                    generate_and_add(shaft, start_box, random, [box_.min[0] - 1, box_.min[1], z], Facing::West, depth + 1);
                } else if selection == 1 {
                    generate_and_add(shaft, start_box, random, [box_.max[0] + 1, box_.min[1], z], Facing::East, depth + 1);
                }
                z += 5;
            }
        } else {
            let mut x = box_.min[0] + 3;
            while x + 3 <= box_.max[0] {
                let selection = random.next_int_bounded(5);
                if selection == 0 {
                    generate_and_add(shaft, start_box, random, [x, box_.min[1], box_.min[2] - 1], Facing::North, depth + 1);
                } else if selection == 1 {
                    generate_and_add(shaft, start_box, random, [x, box_.min[1], box_.max[2] + 1], Facing::South, depth + 1);
                }
                x += 5;
            }
        }
    }
}

/// A crossing's own children growth — three arms always, plus up to four more for a
/// two-floored crossing, each behind its own boolean draw.
fn crossing_children<R: RandomSource>(
    shaft: &mut Shaft,
    box_: BoundingBox,
    direction: Facing,
    two_floored: bool,
    depth: i32,
    start_box: BoundingBox,
    random: &mut R,
) {
    match direction {
        Facing::North => {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.min[2] - 1], Facing::North, depth);
            generate_and_add(shaft, start_box, random, [box_.min[0] - 1, box_.min[1], box_.min[2] + 1], Facing::West, depth);
            generate_and_add(shaft, start_box, random, [box_.max[0] + 1, box_.min[1], box_.min[2] + 1], Facing::East, depth);
        }
        Facing::South => {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.max[2] + 1], Facing::South, depth);
            generate_and_add(shaft, start_box, random, [box_.min[0] - 1, box_.min[1], box_.min[2] + 1], Facing::West, depth);
            generate_and_add(shaft, start_box, random, [box_.max[0] + 1, box_.min[1], box_.min[2] + 1], Facing::East, depth);
        }
        Facing::West => {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.min[2] - 1], Facing::North, depth);
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.max[2] + 1], Facing::South, depth);
            generate_and_add(shaft, start_box, random, [box_.min[0] - 1, box_.min[1], box_.min[2] + 1], Facing::West, depth);
        }
        Facing::East => {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.min[2] - 1], Facing::North, depth);
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, box_.min[1], box_.max[2] + 1], Facing::South, depth);
            generate_and_add(shaft, start_box, random, [box_.max[0] + 1, box_.min[1], box_.min[2] + 1], Facing::East, depth);
        }
    }
    if two_floored {
        // `minY + 3 + 1`, the upper floor's foot. Four independent boolean draws,
        // all four drawn in this fixed order regardless of whether the piece lands.
        let upper = box_.min[1] + 3 + 1;
        if random.next_bool() {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, upper, box_.min[2] - 1], Facing::North, depth);
        }
        if random.next_bool() {
            generate_and_add(shaft, start_box, random, [box_.min[0] - 1, upper, box_.min[2] + 1], Facing::West, depth);
        }
        if random.next_bool() {
            generate_and_add(shaft, start_box, random, [box_.max[0] + 1, upper, box_.min[2] + 1], Facing::East, depth);
        }
        if random.next_bool() {
            generate_and_add(shaft, start_box, random, [box_.min[0] + 1, upper, box_.max[2] + 1], Facing::South, depth);
        }
    }
}

/// A stairs piece's own children growth — one child, straight on.
fn stairs_children<R: RandomSource>(
    shaft: &mut Shaft,
    box_: BoundingBox,
    orientation: Option<Facing>,
    depth: i32,
    start_box: BoundingBox,
    random: &mut R,
) {
    let Some(orientation) = orientation else {
        return;
    };
    let foot = match orientation {
        Facing::North => [box_.min[0], box_.min[1], box_.min[2] - 1],
        Facing::South => [box_.min[0], box_.min[1], box_.max[2] + 1],
        Facing::West => [box_.min[0] - 1, box_.min[1], box_.min[2]],
        Facing::East => [box_.max[0] + 1, box_.min[1], box_.min[2]],
    };
    generate_and_add(shaft, start_box, random, foot, orientation, depth);
}

/// A room's own children growth — four walls, each a `while` loop whose *step* is a
/// draw, so the number of draws depends on their own values.
fn room_children<R: RandomSource>(
    shaft: &mut Shaft,
    index: usize,
    box_: BoundingBox,
    depth: i32,
    start_box: BoundingBox,
    random: &mut R,
) {
    let x_span = box_.max[0] - box_.min[0] + 1;
    let z_span = box_.max[2] - box_.min[2] + 1;
    let mut height_space = (box_.max[1] - box_.min[1] + 1) - 3 - 1;
    if height_space <= 0 {
        height_space = 1;
    }

    // The four walls in source order, and the *span* each one steps over differs
    // between the X pair and the Z pair.
    let walls: [(i32, u8); 4] = [(x_span, 0), (x_span, 1), (z_span, 2), (z_span, 3)];
    for (span, wall) in walls {
        let mut pos = 0;
        while pos < span {
            pos += random.next_int_bounded(span);
            if pos + 3 > span {
                break;
            }
            let y = box_.min[1] + random.next_int_bounded(height_space) + 1;
            let (foot, direction) = match wall {
                0 => ([box_.min[0] + pos, y, box_.min[2] - 1], Facing::North),
                1 => ([box_.min[0] + pos, y, box_.max[2] + 1], Facing::South),
                2 => ([box_.min[0] - 1, y, box_.min[2] + pos], Facing::West),
                _ => ([box_.max[0] + 1, y, box_.min[2] + pos], Facing::East),
            };
            if let Some(child) = generate_and_add(shaft, start_box, random, foot, direction, depth) {
                let child_box = shaft.pieces[child].box_;
                // The entrance box is the child's own X/Y (or Z/Y) extent narrowed
                // to a two-block-thick slab against the room's own wall.
                let entrance = match wall {
                    0 => BoundingBox {
                        min: [child_box.min[0], child_box.min[1], box_.min[2]],
                        max: [child_box.max[0], child_box.max[1], box_.min[2] + 1],
                    },
                    1 => BoundingBox {
                        min: [child_box.min[0], child_box.min[1], box_.max[2] - 1],
                        max: [child_box.max[0], child_box.max[1], box_.max[2]],
                    },
                    2 => BoundingBox {
                        min: [box_.min[0], child_box.min[1], child_box.min[2]],
                        max: [box_.min[0] + 1, child_box.max[1], child_box.max[2]],
                    },
                    _ => BoundingBox {
                        min: [box_.max[0] - 1, child_box.min[1], child_box.min[2]],
                        max: [box_.max[0], child_box.max[1], child_box.max[2]],
                    },
                };
                if let Kind::Room { entrances } = &mut shaft.pieces[index].kind {
                    entrances.push(entrance);
                }
            }
            pos += 4;
        }
    }
}

/// The world one `postProcess` pass reads and writes — pre-surface terrain with
/// every block this start has already written laid over it.
///
/// # Why this is not simply a block list
///
/// Six of the mineshaft's helpers read the world and *branch* on what they find:
/// the replaceability test, the supporting-box check, support-pillar placement, plank-block assignment,
/// double lower/upper support placement and the downward pillar/chain probe. A faithful implementation
/// reads the level, which by then holds both the terrain and whatever earlier pieces
/// of the same start wrote. So the overlay is not an optimisation — without it a
/// corridor's pillars would be decided against bare stone and would never appear.
struct View<'a> {
    ctx: &'a dyn StartContext,
    overlay: HashMap<[i32; 3], Arc<str>>,
    /// The current piece's own writes, in order. Drained at each piece boundary.
    emitted: Vec<CodedBlock>,
}

/// The four terrain kinds plus "something a piece wrote", which is all the
/// resolution any mineshaft predicate needs.
enum Sample<'a> {
    Terrain(BlockKind),
    Written(&'a str),
}

impl<'a> View<'a> {
    fn sample(&self, pos: [i32; 3]) -> Sample<'_> {
        match self.overlay.get(&pos) {
            Some(state) => Sample::Written(state),
            None => Sample::Terrain(self.ctx.block_kind_at(pos[0], pos[1], pos[2])),
        }
    }

    /// Whether the block is air. `cave_air` is air — which matters, because the whole
    /// interior of a mineshaft is `cave_air` and the supporting-box check asks exactly this
    /// question about the block above a support.
    fn is_air(&self, pos: [i32; 3]) -> bool {
        match self.sample(pos) {
            Sample::Terrain(kind) => kind == BlockKind::Air,
            Sample::Written(state) => {
                state.starts_with("minecraft:air") || state.starts_with("minecraft:cave_air")
            }
        }
    }

    /// Whether the block is a fluid. No mineshaft piece writes a fluid, so a written block is
    /// never liquid — stated rather than assumed, because the invalid-location check
    /// walks a box that other pieces have written into.
    fn is_liquid(&self, pos: [i32; 3]) -> bool {
        match self.sample(pos) {
            Sample::Terrain(kind) => matches!(kind, BlockKind::Water | BlockKind::Lava),
            Sample::Written(_) => false,
        }
    }

    fn is_lava(&self, pos: [i32; 3]) -> bool {
        matches!(self.sample(pos), Sample::Terrain(BlockKind::Lava))
    }

    /// Whether a structure may freely replace the block — air or fluid. (Glow
    /// lichen and seagrass are in the reference set and cannot exist pre-surface.)
    fn is_replaceable(&self, pos: [i32; 3]) -> bool {
        self.is_air(pos) || self.is_liquid(pos)
    }

    /// Whether the block's up face is sturdy — the face-sturdiness, solid-render
    /// and can-support-centre tests the three column walks all reduce to.
    ///
    /// A table over the blocks that can actually appear here, not a general
    /// solidity model: pre-surface terrain is one solid kind, and the only written
    /// candidates are the eight states a mineshaft places. The fence is the
    /// interesting row — a fence post's top face is not full, so it does **not**
    /// support a pillar, which is what stops a support column growing out of its
    /// own fence.
    fn is_sturdy_up(&self, pos: [i32; 3]) -> bool {
        match self.sample(pos) {
            Sample::Terrain(kind) => kind == BlockKind::Stone,
            Sample::Written(state) => {
                let name = state.split('[').next().unwrap_or(state);
                matches!(
                    name,
                    "minecraft:oak_planks"
                        | "minecraft:dark_oak_planks"
                        | "minecraft:oak_log"
                        | "minecraft:dark_oak_log"
                        | "minecraft:spawner"
                )
            }
        }
    }

    /// True when the written block at `pos` is exactly `name` —
    /// used only for the four wood/chain tests the replaceability check and
    /// double lower/upper support placement make. Terrain is never one of these.
    fn is_block(&self, pos: [i32; 3], name: &str) -> bool {
        match self.sample(pos) {
            Sample::Terrain(_) => false,
            Sample::Written(state) => state.split('[').next().unwrap_or(state) == name,
        }
    }

    /// A mineshaft piece's replaceability override that protects a mineshaft's own
    /// woodwork from a neighbouring piece's `cave_air` sweep.
    fn can_be_replaced(&self, pos: [i32; 3], wood: Wood) -> bool {
        !self.is_block(pos, wood.planks())
            && !self.is_block(pos, wood.log().split('[').next().unwrap_or(""))
            && !self.is_block(pos, wood.fence_name())
            && !self.is_block(pos, "minecraft:iron_chain")
    }

    /// The raw write, with no replaceability check and
    /// no transform. Four helpers use it directly.
    fn set(&mut self, pos: [i32; 3], state: &str) {
        let shared: Arc<str> = Arc::from(state);
        self.overlay.insert(pos, Arc::clone(&shared));
        self.emitted.push(CodedBlock {
            pos,
            state: state.to_string(),
        });
    }

    fn take(&mut self) -> Vec<CodedBlock> {
        std::mem::take(&mut self.emitted)
    }
}

/// One piece's block-writing walk, bound to its node and the shared [`View`].
struct Place<'a, 'v> {
    node: &'a Node,
    view: &'a mut View<'v>,
    wood: Wood,
    mirror: Mirror,
    rotation: Rotation,
}

impl Place<'_, '_> {
    /// Places one block — the replaceability check, then mirror, then rotate, then write.
    fn place(&mut self, state: &BlockState, x: i32, y: i32, z: i32) {
        let pos = self.node.world_pos(x, y, z);
        if !self.view.can_be_replaced(pos, self.wood) {
            return;
        }
        let transformed = state.mirror(self.mirror).rotate(self.rotation);
        self.view.set(pos, &transformed.canonical());
    }

    /// `getBlock(x, y, z, chunkBB)`, minus the chunk gate.
    fn air_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.view.is_air(self.node.world_pos(x, y, z))
    }

    /// `isInterior(x, y, z, chunkBB)` — the `(y + 1)` position sits below the
    /// `OCEAN_FLOOR_WG` height, i.e. the piece is underground here.
    fn is_interior(&self, x: i32, y: i32, z: i32) -> bool {
        let pos = self.node.world_pos(x, y + 1, z);
        pos[1] < free_height(self.view.ctx, pos[0], pos[2], HeightmapKind::OceanFloorWg)
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_box(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: &BlockState,
        fill: &BlockState,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    let state = if interior { fill } else { edge };
                    self.place(state, x, y, z);
                }
            }
        }
    }

    /// A probabilistic hollow box — **one float draw per position, unconditionally**, and
    /// it is the leftmost operand of the `&&` chain, so it is spent before either
    /// world test runs.
    #[allow(clippy::too_many_arguments)]
    fn generate_maybe_box<R: RandomSource>(
        &mut self,
        random: &mut R,
        probability: f32,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: &BlockState,
        fill: &BlockState,
        has_to_be_inside: bool,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if random.next_float() > probability {
                        continue;
                    }
                    if has_to_be_inside && !self.is_interior(x, y, z) {
                        continue;
                    }
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    let state = if interior { fill } else { edge };
                    self.place(state, x, y, z);
                }
            }
        }
    }

    /// Places one block with probability `probability` — always one draw.
    fn maybe_generate_block<R: RandomSource>(
        &mut self,
        random: &mut R,
        probability: f32,
        x: i32,
        y: i32,
        z: i32,
        state: &BlockState,
    ) {
        if random.next_float() < probability {
            self.place(state, x, y, z);
        }
    }

    /// The room's dome ceiling. Float arithmetic in `f32`
    /// deliberately, and the `1.05` threshold sits close enough to 1 that
    /// a promotion to `f64` moves the boundary cells.
    #[allow(clippy::too_many_arguments)]
    fn generate_upper_half_sphere(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        fill: &BlockState,
    ) {
        let diag_x = (x1 - x0 + 1) as f32;
        let diag_y = (y1 - y0 + 1) as f32;
        let diag_z = (z1 - z0 + 1) as f32;
        let cx = x0 as f32 + diag_x / 2.0;
        let cz = z0 as f32 + diag_z / 2.0;
        for y in y0..=y1 {
            let ny = (y - y0) as f32 / diag_y;
            for x in x0..=x1 {
                let nx = (x as f32 - cx) / (diag_x * 0.5);
                for z in z0..=z1 {
                    let nz = (z as f32 - cz) / (diag_z * 0.5);
                    if nx * nx + ny * ny + nz * nz <= 1.05 {
                        self.place(fill, x, y, z);
                    }
                }
            }
        }
    }

    /// A mineshaft piece's invalid-location check — the biome veto, then a liquid walk
    /// over the six faces of the box inflated by one.
    ///
    /// The inflation is `±1` clamped to the decorating chunk's own box in a
    /// faithful implementation; here it is clamped to
    /// the dimension instead, which is the deviation this module's doc names.
    fn is_in_invalid_location(&self, blocking_biomes: &std::collections::HashSet<String>) -> bool {
        let box_ = self.node.box_;
        let min_y = self.view.ctx.min_y();
        let max_y = min_y + self.view.ctx.dimension_height() - 1;
        let (x0, z0) = (box_.min[0] - 1, box_.min[2] - 1);
        let (x1, z1) = (box_.max[0] + 1, box_.max[2] + 1);
        let y0 = (box_.min[1] - 1).max(min_y);
        let y1 = (box_.max[1] + 1).min(max_y);
        let centre = [(x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2];
        if !blocking_biomes.is_empty() {
            let biome = self
                .view
                .ctx
                .biome_at_quart(centre[0] >> 2, centre[1] >> 2, centre[2] >> 2);
            if blocking_biomes.contains(&biome) {
                return true;
            }
        }
        // Three double loops, one per axis pair — the top/bottom faces, then the
        // north/south faces, then the west/east faces. Transcribed as three rather
        // than folded into one shell walk, because the folded version visits the
        // edges a different number of times and this function short-circuits.
        for x in x0..=x1 {
            for z in z0..=z1 {
                if self.view.is_liquid([x, y0, z]) || self.view.is_liquid([x, y1, z]) {
                    return true;
                }
            }
        }
        for x in x0..=x1 {
            for y in y0..=y1 {
                if self.view.is_liquid([x, y, z0]) || self.view.is_liquid([x, y, z1]) {
                    return true;
                }
            }
        }
        for z in z0..=z1 {
            for y in y0..=y1 {
                if self.view.is_liquid([x0, y, z]) || self.view.is_liquid([x1, y, z]) {
                    return true;
                }
            }
        }
        false
    }

    /// A floor plank wherever the piece is underground and the
    /// existing block cannot be stood on. The raw write directly, so no
    /// replaceability check and no transform.
    fn set_planks_block(&mut self, planks: &str, x: i32, y: i32, z: i32) {
        if !self.is_interior(x, y, z) {
            return;
        }
        let pos = self.node.world_pos(x, y, z);
        if self.view.is_sturdy_up(pos) {
            return;
        }
        self.view.set(pos, planks);
    }

    /// Whether every block above the span is non-air.
    fn is_supporting_box(&self, x0: i32, x1: i32, y1: i32, z0: i32) -> bool {
        (x0..=x1).all(|x| !self.air_at(x, y1 + 1, z0))
    }
}

/// A corridor's own support placement.
fn place_support<R: RandomSource>(
    p: &mut Place<'_, '_>,
    random: &mut R,
    x0: i32,
    y0: i32,
    z: i32,
    y1: i32,
    x1: i32,
) {
    if !p.is_supporting_box(x0, x1, y1, z) {
        return;
    }
    let wood = p.wood;
    let planks = BlockState::parse(wood.planks());
    let cave_air = BlockState::of("minecraft:cave_air");
    let fence_west = BlockState::parse(&format!(
        "{}[east=false,north=false,south=false,waterlogged=false,west=true]",
        wood.fence_name()
    ));
    let fence_east = BlockState::parse(&format!(
        "{}[east=true,north=false,south=false,waterlogged=false,west=false]",
        wood.fence_name()
    ));
    p.generate_box(x0, y0, z, x0, y1 - 1, z, &fence_west, &cave_air);
    p.generate_box(x1, y0, z, x1, y1 - 1, z, &fence_east, &cave_air);
    if random.next_int_bounded(4) == 0 {
        p.generate_box(x0, y1, z, x0, y1, z, &planks, &cave_air);
        p.generate_box(x1, y1, z, x1, y1, z, &planks, &cave_air);
    } else {
        p.generate_box(x0, y1, z, x1, y1, z, &planks, &cave_air);
        // The two torch draws happen only on this branch, so a corridor whose
        // supports are all the two-plank variant makes 3/4 of the draws.
        let south = BlockState::parse("minecraft:wall_torch[facing=south]");
        let north = BlockState::parse("minecraft:wall_torch[facing=north]");
        p.maybe_generate_block(random, 0.05, x0 + 1, y1, z - 1, &south);
        p.maybe_generate_block(random, 0.05, x0 + 1, y1, z + 1, &north);
    }
}

/// A corridor's probabilistic cobweb placement — the interior check gates the **draw**, so this
/// cannot be reordered into "draw, then test".
fn maybe_place_cobweb<R: RandomSource>(
    p: &mut Place<'_, '_>,
    random: &mut R,
    probability: f32,
    x: i32,
    y: i32,
    z: i32,
) {
    if !p.is_interior(x, y, z) {
        return;
    }
    if !(random.next_float() < probability) {
        return;
    }
    if !has_sturdy_neighbours(p, x, y, z, 2) {
        return;
    }
    let cobweb = BlockState::of("minecraft:cobweb");
    p.place(&cobweb, x, y, z);
}

/// Whether at least `count` of the six neighbours present a sturdy
/// face toward this position. The direction order is down, up, north, south,
/// west, east; only the *count* is read, so the order is inert, but it is kept
/// because a future predicate that short-circuits differently would not be.
fn has_sturdy_neighbours(p: &Place<'_, '_>, x: i32, y: i32, z: i32, count: i32) -> bool {
    let pos = p.node.world_pos(x, y, z);
    const STEPS: [[i32; 3]; 6] = [
        [0, -1, 0],
        [0, 1, 0],
        [0, 0, -1],
        [0, 0, 1],
        [-1, 0, 0],
        [1, 0, 0],
    ];
    let mut sturdy = 0;
    for step in STEPS {
        let neighbour = [pos[0] + step[0], pos[1] + step[1], pos[2] + step[2]];
        if p.view.is_sturdy_up(neighbour) {
            sturdy += 1;
            if sturdy >= count {
                return true;
            }
        }
    }
    false
}

/// A corridor's downward pillar/chain probe — a wood pillar down to the first
/// surface that can carry it, or a fence-and-chain hang up to the first ceiling
/// that can hold one, whichever is found first at equal distance.
///
/// The two searches advance **together**, one step each per iteration, and the
/// downward one is checked first — so a pillar wins a tie. Splitting them into two
/// loops would flip that.
fn fill_pillar_down_or_chain_up(p: &mut Place<'_, '_>, x: i32, y: i32, z: i32) {
    let pos = p.node.world_pos(x, y, z);
    let world_y = pos[1];
    let min_y = p.view.ctx.min_y();
    let max_y = min_y + p.view.ctx.dimension_height() - 1;
    let pillar = p.wood.log().to_string();
    let fence = p.wood.fence();
    let mut distance = 1;
    let mut check_below = true;
    let mut check_above = true;
    while check_below || check_above {
        if check_below {
            let probe = [pos[0], world_y - distance, pos[2]];
            let empty_below = p.view.is_replaceable(probe) && !p.view.is_lava(probe);
            if !empty_below && p.view.is_sturdy_up(probe) {
                for py in (world_y - distance + 1)..world_y {
                    p.view.set([pos[0], py, pos[2]], &pillar);
                }
                return;
            }
            check_below = distance <= MAX_PILLAR_HEIGHT && empty_below && probe[1] > min_y + 1;
        }
        if check_above {
            let probe = [pos[0], world_y + distance, pos[2]];
            let empty_above = p.view.is_replaceable(probe);
            if !empty_above && p.view.is_sturdy_up(probe) {
                p.view.set([pos[0], world_y + 1, pos[2]], &fence);
                for py in (world_y + 2)..(world_y + distance) {
                    p.view
                        .set([pos[0], py, pos[2]], "minecraft:iron_chain[axis=y,waterlogged=false]");
                }
                return;
            }
            check_above = distance <= MAX_CHAIN_HEIGHT && empty_above && probe[1] < max_y;
        }
        distance += 1;
    }
}

/// A corridor's double lower/upper support placement — the two outer floor columns,
/// each only if the floor plank the plank-assignment sweep just laid is actually
/// there.
fn place_double_support(p: &mut Place<'_, '_>, x: i32, y: i32, z: i32) {
    let planks = p.wood.planks().to_string();
    if p.view.is_block(p.node.world_pos(x, y, z), &planks) {
        fill_pillar_down_or_chain_up(p, x, y, z);
    }
    if p.view.is_block(p.node.world_pos(x + 2, y, z), &planks) {
        fill_pillar_down_or_chain_up(p, x + 2, y, z);
    }
}

/// Second pass: every piece's `postProcess`, in list order, against one shared
/// [`View`].
fn into_pieces<R: RandomSource>(
    shaft: Shaft,
    ctx: &dyn StartContext,
    blocking_biomes: &std::collections::HashSet<String>,
    random: &mut R,
) -> Vec<StructurePiece> {
    let wood = shaft.wood;
    let mut view = View {
        ctx,
        overlay: HashMap::new(),
        emitted: Vec::new(),
    };
    let mut out = Vec::with_capacity(shaft.pieces.len());
    for node in &shaft.pieces {
        let (mirror, rotation) = node.transform();
        let mut loot = Vec::new();
        {
            let mut place = Place {
                node,
                view: &mut view,
                wood,
                mirror,
                rotation,
            };
            post_process(&mut place, random, blocking_biomes, &mut loot);
        }
        out.push(StructurePiece {
            id: node.piece_id().to_string(),
            bounding_box: node.box_,
            orientation: node.orientation.map(Facing::data_2d),
            gen_depth: node.gen_depth,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: Some(Arc::new(view.take())),
            loot,
            // `terrain_adaptation` is `none` for both mineshaft structures, so the
            // rigid-box `else` branch of `Beardifier` is inert here.
            beard: None,
            refine: None,
        });
    }
    out
}

/// The `postProcess` of whichever piece `place` is bound to.
fn post_process<R: RandomSource>(
    p: &mut Place<'_, '_>,
    random: &mut R,
    blocking_biomes: &std::collections::HashSet<String>,
    loot: &mut Vec<super::CodedLoot>,
) {
    if p.is_in_invalid_location(blocking_biomes) {
        return;
    }
    match p.node.kind.clone() {
        Kind::Corridor {
            has_rails,
            spider_corridor,
            sections,
        } => corridor_post(p, random, loot, has_rails, spider_corridor, sections),
        Kind::Crossing { two_floored, .. } => crossing_post(p, two_floored),
        Kind::Stairs => stairs_post(p),
        Kind::Room { entrances } => room_post(p, &entrances),
    }
}

/// A corridor's block-writing walk.
fn corridor_post<R: RandomSource>(
    p: &mut Place<'_, '_>,
    random: &mut R,
    loot: &mut Vec<super::CodedLoot>,
    has_rails: bool,
    spider_corridor: bool,
    sections: i32,
) {
    let cave_air = BlockState::of("minecraft:cave_air");
    let cobweb = BlockState::of("minecraft:cobweb");
    let planks = p.wood.planks().to_string();
    let length = sections * 5 - 1;

    p.generate_box(0, 0, 0, 2, 1, length, &cave_air, &cave_air);
    p.generate_maybe_box(random, 0.8, 0, 2, 0, 2, 2, length, &cave_air, &cave_air, false);
    if spider_corridor {
        p.generate_maybe_box(random, 0.6, 0, 0, 0, 2, 1, length, &cobweb, &cave_air, true);
    }

    let mut placed_spider = false;
    for section in 0..sections {
        let z = 2 + section * 5;
        place_support(p, random, 0, 0, z, 2, 2);
        maybe_place_cobweb(p, random, 0.1, 0, 2, z - 1);
        maybe_place_cobweb(p, random, 0.1, 2, 2, z - 1);
        maybe_place_cobweb(p, random, 0.1, 0, 2, z + 1);
        maybe_place_cobweb(p, random, 0.1, 2, 2, z + 1);
        maybe_place_cobweb(p, random, 0.05, 0, 2, z - 2);
        maybe_place_cobweb(p, random, 0.05, 2, 2, z - 2);
        maybe_place_cobweb(p, random, 0.05, 0, 2, z + 2);
        maybe_place_cobweb(p, random, 0.05, 2, 2, z + 2);
        if random.next_int_bounded(100) == 0 {
            create_chest_minecart(p, random, loot, 2, 0, z - 1);
        }
        if random.next_int_bounded(100) == 0 {
            create_chest_minecart(p, random, loot, 0, 0, z + 1);
        }
        if spider_corridor && !placed_spider {
            // The draw bounded by 3 happens before the interior check runs, unlike
            // the cobweb placement's own draw-then-test order — the asymmetry
            // is deliberate.
            let spider_z = z - 1 + random.next_int_bounded(3);
            if p.is_interior(1, 0, spider_z) {
                placed_spider = true;
                let pos = p.node.world_pos(1, 0, spider_z);
                p.view.set(pos, "minecraft:spawner");
                // Assigning the spawner's entity type draws nothing from the
                // random source — the spawn-entry constructor takes the random
                // only to seed an already-resolved weighted list.
            }
        }
    }

    for x in 0..=2 {
        for z in 0..=length {
            p.set_planks_block(&planks, x, -1, z);
        }
    }

    place_double_support(p, 0, -1, 2);
    if sections > 1 {
        place_double_support(p, 0, -1, length - 2);
    }

    if has_rails {
        // The rail's north-south shape, which the piece's own rotation then turns into
        // its east-west shape for an east/west corridor — the reason `BlockState::rotate`
        // grew a rail table with this unit.
        let rail = BlockState::parse("minecraft:rail[shape=north_south,waterlogged=false]");
        for z in 0..=length {
            // Not-air and solid-render: two tests, and for the
            // blocks that can be here the second implies the first.
            let floor = p.node.world_pos(1, -1, z);
            if p.view.is_air(floor) || !p.view.is_sturdy_up(floor) {
                continue;
            }
            let probability = if p.is_interior(1, 0, z) { 0.7 } else { 0.9 };
            p.maybe_generate_block(random, probability, 1, 0, z, &rail);
        }
    }
}

/// A corridor's own container placement — an override that places a **rail** and a chest
/// *minecart*, not a chest.
///
/// Two draws, in this order: a boolean draw for the rail shape, then
/// a 64-bit draw for the loot seed, and both only when the position is air over a
/// non-air block. The minecart itself is an entity, so the container is on the
/// ledger (`coded:worldgen_entities`) while the rail is real.
fn create_chest_minecart<R: RandomSource>(
    p: &mut Place<'_, '_>,
    random: &mut R,
    loot: &mut Vec<super::CodedLoot>,
    x: i32,
    y: i32,
    z: i32,
) {
    let pos = p.node.world_pos(x, y, z);
    let below = [pos[0], pos[1] - 1, pos[2]];
    if !p.view.is_air(pos) || p.view.is_air(below) {
        return;
    }
    let shape = if random.next_bool() {
        "north_south"
    } else {
        "east_west"
    };
    let rail = BlockState::parse(&format!("minecraft:rail[shape={shape},waterlogged=false]"));
    p.place(&rail, x, y, z);
    let seed = random.next_long();
    loot.push(super::CodedLoot {
        pos,
        table: MINESHAFT_LOOT.to_string(),
        seed,
    });
}

/// A crossing's block-writing walk — absolute coordinates throughout, because the
/// crossing has no orientation.
fn crossing_post(p: &mut Place<'_, '_>, two_floored: bool) {
    let cave_air = BlockState::of("minecraft:cave_air");
    let box_ = p.node.box_;
    let planks = p.wood.planks().to_string();
    if two_floored {
        p.generate_box(box_.min[0] + 1, box_.min[1], box_.min[2], box_.max[0] - 1, box_.min[1] + 2, box_.max[2], &cave_air, &cave_air);
        p.generate_box(box_.min[0], box_.min[1], box_.min[2] + 1, box_.max[0], box_.min[1] + 2, box_.max[2] - 1, &cave_air, &cave_air);
        p.generate_box(box_.min[0] + 1, box_.max[1] - 2, box_.min[2], box_.max[0] - 1, box_.max[1], box_.max[2], &cave_air, &cave_air);
        p.generate_box(box_.min[0], box_.max[1] - 2, box_.min[2] + 1, box_.max[0], box_.max[1], box_.max[2] - 1, &cave_air, &cave_air);
        p.generate_box(box_.min[0] + 1, box_.min[1] + 3, box_.min[2] + 1, box_.max[0] - 1, box_.min[1] + 3, box_.max[2] - 1, &cave_air, &cave_air);
    } else {
        p.generate_box(box_.min[0] + 1, box_.min[1], box_.min[2], box_.max[0] - 1, box_.max[1], box_.max[2], &cave_air, &cave_air);
        p.generate_box(box_.min[0], box_.min[1], box_.min[2] + 1, box_.max[0], box_.max[1], box_.max[2] - 1, &cave_air, &cave_air);
    }
    place_support_pillar(p, box_.min[0] + 1, box_.min[1], box_.min[2] + 1, box_.max[1]);
    place_support_pillar(p, box_.min[0] + 1, box_.min[1], box_.max[2] - 1, box_.max[1]);
    place_support_pillar(p, box_.max[0] - 1, box_.min[1], box_.min[2] + 1, box_.max[1]);
    place_support_pillar(p, box_.max[0] - 1, box_.min[1], box_.max[2] - 1, box_.max[1]);
    let y = box_.min[1] - 1;
    for x in box_.min[0]..=box_.max[0] {
        for z in box_.min[2]..=box_.max[2] {
            p.set_planks_block(&planks, x, y, z);
        }
    }
}

/// A crossing's support-pillar placement — only where the crossing's ceiling has
/// something above it to hold up.
fn place_support_pillar(p: &mut Place<'_, '_>, x: i32, y0: i32, z: i32, y1: i32) {
    if p.air_at(x, y1 + 1, z) {
        return;
    }
    let planks = BlockState::parse(p.wood.planks());
    let cave_air = BlockState::of("minecraft:cave_air");
    p.generate_box(x, y0, z, x, y1, z, &planks, &cave_air);
}

/// A stairs piece's block-writing walk — five stepped boxes plus the two landings, and
/// the only piece here with no RNG and no world read.
fn stairs_post(p: &mut Place<'_, '_>) {
    let cave_air = BlockState::of("minecraft:cave_air");
    p.generate_box(0, 5, 0, 2, 7, 1, &cave_air, &cave_air);
    p.generate_box(0, 0, 7, 2, 2, 8, &cave_air, &cave_air);
    for i in 0..5 {
        // The `(i < 4 ? 1 : 0)` makes the last step one block shallower, which is
        // what lands the staircase flush with the lower landing.
        let drop = if i < 4 { 1 } else { 0 };
        p.generate_box(0, 5 - i - drop, 2 + i, 2, 7 - i, 2 + i, &cave_air, &cave_air);
    }
}

/// A room's block-writing walk — the floor slab, each entrance's lintel, then the
/// domed ceiling.
fn room_post(p: &mut Place<'_, '_>, entrances: &[BoundingBox]) {
    let cave_air = BlockState::of("minecraft:cave_air");
    let box_ = p.node.box_;
    p.generate_box(
        box_.min[0],
        box_.min[1] + 1,
        box_.min[2],
        box_.max[0],
        (box_.min[1] + 3).min(box_.max[1]),
        box_.max[2],
        &cave_air,
        &cave_air,
    );
    for entrance in entrances {
        p.generate_box(
            entrance.min[0],
            entrance.max[1] - 2,
            entrance.min[2],
            entrance.max[0],
            entrance.max[1],
            entrance.max[2],
            &cave_air,
            &cave_air,
        );
    }
    p.generate_upper_half_sphere(
        box_.min[0],
        box_.min[1] + 4,
        box_.min[2],
        box_.max[0],
        box_.max[1],
        box_.max[2],
        &cave_air,
    );
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};

    use super::*;

    /// A world that is stone below `surface` and air above, with no fluid anywhere —
    /// enough for every mineshaft predicate and deliberately *not* enough to stand
    /// in for the oracle world, which the integration gate uses instead.
    struct Solid {
        surface: i32,
    }

    impl StartContext for Solid {
        fn first_occupied_height(&self, _x: i32, _z: i32, _h: HeightmapKind) -> i32 {
            self.surface
        }
        fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
            "minecraft:plains".to_string()
        }
        fn sea_level(&self) -> i32 {
            63
        }
        fn block_kind_at(&self, _x: i32, y: i32, _z: i32) -> BlockKind {
            if y <= self.surface {
                BlockKind::Stone
            } else {
                BlockKind::Air
            }
        }
    }

    fn random(seed: i64) -> WorldgenRandom<LegacyRandomSource> {
        let mut r = WorldgenRandom::new(LegacyRandomSource::new(0));
        r.set_large_feature_seed(seed, 0, 0);
        r
    }


    /// A grown shaft, with **counted** rather than eyeballed results: the tree at
    /// this seed is exactly 101 pieces and 14,344 blocks, and both numbers are the
    /// specification (RNG draw order decides them). A change to any draw moves them.
    #[test]
    fn a_shaft_grows_a_tree_of_all_four_piece_kinds() {
        let ctx = Solid { surface: 70 };
        let mut r = random(99);
        let (pieces, start) = generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r);
        assert_eq!(pieces.len(), 101);
        assert_eq!(pieces[0].id, "minecraft:msroom", "the room is added first");
        let total: usize = pieces
            .iter()
            .filter_map(|p| p.blocks.as_ref())
            .map(|b| b.len())
            .sum();
        assert_eq!(total, 14_344);
        for kind in [
            "minecraft:msroom",
            "minecraft:mscorridor",
            "minecraft:mscrossing",
            "minecraft:msstairs",
        ] {
            assert!(
                pieces.iter().any(|p| p.id == kind),
                "no {kind} in a 101-piece shaft"
            );
        }
        // `(middleBlockX, 50 + dy, minBlockZ)` — the Z is the chunk *minimum*, which
        // is the detail a "chunk centre" reading gets wrong by 8.
        assert_eq!(start[0], 8);
        assert_eq!(start[2], 0);
    }

    /// The room's four wall walks each step by `nextInt(xSpan)` and break as soon as
    /// `pos + 3 > span`, so a small room can legitimately reach **no** children.
    /// Kept as its own case because a generator that could not produce a lone room
    /// would look healthier and be wrong.
    #[test]
    fn a_room_whose_wall_walks_all_break_first_step_is_a_lone_room() {
        let ctx = Solid { surface: 70 };
        let mut r = random(1_234_567);
        let (pieces, _) = generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r);
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].id, "minecraft:msroom");
        let total = pieces[0].blocks.as_ref().map_or(0, |b| b.len());
        assert_eq!(total, 435, "a lone room still hollows itself out");
    }

    /// The whole engine's premise: two independent runs at one seed agree exactly.
    #[test]
    fn two_runs_at_one_seed_are_byte_identical() {
        let ctx = Solid { surface: 70 };
        let render = |seed: i64| {
            let mut r = random(seed);
            let (pieces, start) = generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r);
            let blocks: Vec<String> = pieces
                .iter()
                .filter_map(|p| p.blocks.as_ref())
                .flat_map(|b| b.iter())
                .map(|b| format!("{:?}{}", b.pos, b.state))
                .collect();
            (start, blocks)
        };
        assert_eq!(render(99), render(99));
        assert_ne!(
            render(99).1,
            render(100).1,
            "two seeds must not agree, or the first assertion is vacuous"
        );
    }

    /// The discarded leading `nextDouble` is the whole reason mineshaft's stream is
    /// offset. Removing it must change the answer — the control that proves the
    /// draw is load-bearing rather than decorative.
    #[test]
    fn the_discarded_leading_next_double_shifts_the_stream() {
        let ctx = Solid { surface: 70 };
        let with = {
            let mut r = random(7);
            generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r).0.len()
        };
        let without = {
            let mut r = random(7);
            // Pre-consume nothing, then call the same generator: it draws its own
            // `nextDouble`, so the only way to see the shift is to consume an extra
            // one first and require a different tree.
            let _ = r.next_double();
            generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r).0.len()
        };
        assert_ne!(
            with, without,
            "a one-draw stream shift must change the piece tree"
        );
    }

    /// `moveBelowSeaLevel` puts the whole shaft under `seaLevel - 10`, and the mesa
    /// variant does not — a *predicted* pair of inequalities rather than a
    /// direction, since the two branches are the two hypotheses a wrong port lands
    /// on.
    #[test]
    fn normal_shafts_sit_below_sea_level_and_mesa_shafts_track_the_surface() {
        let ctx = Solid { surface: 100 };
        let mut r = random(4_242);
        let (normal, _) = generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r);
        let top = normal.iter().map(|p| p.bounding_box.max[1]).max().unwrap();
        assert!(
            top <= ctx.sea_level() - 10,
            "moveBelowSeaLevel's maxY is seaLevel - 10; got {top}"
        );
        let mut r = random(4_242);
        let (mesa, _) = generate(0, 0, &ctx, Wood::Mesa, &HashSet::new(), &mut r);
        let mesa_top = mesa.iter().map(|p| p.bounding_box.max[1]).max().unwrap();
        assert!(
            mesa_top > ctx.sea_level() - 10,
            "a mesa shaft at surface 100 is lifted toward the surface; got {mesa_top}"
        );
    }

    /// A blocking biome vetoes every piece, so the start exists and places nothing.
    /// The control is the same seed with an empty blocking set.
    #[test]
    fn a_blocking_biome_places_no_blocks_at_all() {
        let ctx = Solid { surface: 70 };
        let mut r = random(55);
        let (allowed, _) = generate(0, 0, &ctx, Wood::Normal, &HashSet::new(), &mut r);
        let allowed_blocks: usize = allowed
            .iter()
            .filter_map(|p| p.blocks.as_ref())
            .map(|b| b.len())
            .sum();
        let mut blocking = HashSet::new();
        blocking.insert("minecraft:plains".to_string());
        let mut r = random(55);
        let (vetoed, _) = generate(0, 0, &ctx, Wood::Normal, &blocking, &mut r);
        let vetoed_blocks: usize = vetoed
            .iter()
            .filter_map(|p| p.blocks.as_ref())
            .map(|b| b.len())
            .sum();
        assert!(allowed_blocks > 0, "the control must place something");
        assert_eq!(vetoed_blocks, 0, "a vetoed shaft places nothing");
        assert_eq!(
            allowed.len(),
            vetoed.len(),
            "the veto is at postProcess time, so the piece tree is unchanged"
        );
    }

    /// An east/west corridor's rails must come out `east_west`, which is the whole
    /// point of the rail table added with this unit. The wrong hypothesis —
    /// `shape` left alone — is `north_south`, and it is what shipped before.
    #[test]
    fn an_east_west_corridor_rotates_its_rail_shape() {
        let rail = BlockState::parse("minecraft:rail[shape=north_south,waterlogged=false]");
        assert_eq!(
            rail.rotate(Rotation::Cw90).canonical(),
            "minecraft:rail[shape=east_west,waterlogged=false]"
        );
        assert_eq!(
            rail.rotate(Rotation::None).canonical(),
            "minecraft:rail[shape=north_south,waterlogged=false]"
        );
        // A diagonal, where the two directions rotate independently.
        let diagonal = BlockState::parse("minecraft:rail[shape=north_east,waterlogged=false]");
        assert_eq!(
            diagonal.rotate(Rotation::Cw90).canonical(),
            "minecraft:rail[shape=south_east,waterlogged=false]"
        );
        assert_eq!(
            diagonal.mirror(Mirror::LeftRight).canonical(),
            "minecraft:rail[shape=south_east,waterlogged=false]"
        );
        // A stair's `shape` must be untouched by the rail table.
        let stair = BlockState::parse(
            "minecraft:oak_stairs[facing=north,half=bottom,shape=outer_left,waterlogged=false]",
        );
        assert_eq!(
            stair.rotate(Rotation::Cw90).canonical(),
            "minecraft:oak_stairs[facing=east,half=bottom,shape=outer_left,waterlogged=false]"
        );
    }

    /// The replaceability check is what stops a second piece's `cave_air` sweep erasing the
    /// first piece's woodwork. Asserted through the [`View`] rather than through a
    /// whole shaft, because the failure is one predicate.
    #[test]
    fn woodwork_survives_a_neighbouring_pieces_air_sweep() {
        let ctx = Solid { surface: 70 };
        let mut view = View {
            ctx: &ctx,
            overlay: HashMap::new(),
            emitted: Vec::new(),
        };
        view.set([0, 0, 0], "minecraft:oak_planks");
        view.set([1, 0, 0], "minecraft:iron_chain[axis=y,waterlogged=false]");
        view.set([2, 0, 0], "minecraft:cave_air");
        assert!(!view.can_be_replaced([0, 0, 0], Wood::Normal));
        assert!(!view.can_be_replaced([1, 0, 0], Wood::Normal));
        assert!(view.can_be_replaced([2, 0, 0], Wood::Normal));
        // A *mesa* piece does not protect oak, which is vanilla's own behaviour —
        // the override tests `this.type`'s three blocks, not all woods.
        assert!(view.can_be_replaced([0, 0, 0], Wood::Mesa));
    }
}
