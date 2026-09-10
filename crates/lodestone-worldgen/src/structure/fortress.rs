//! Nether fortress piece-tree generation.
//!
//! # What it is
//!
//! The old-style Nether complex is a weighted recursive piece tree, with a bridge
//! family feeding a castle family.  This module creates its complete, translated
//! list of boxes; placement consumes those boxes through [`StructurePiece`].
//!
//! # How it works
//!
//! A start cross at chunk-local `(2, 64, 2)` chooses its horizontal direction,
//! then drains a randomly selected pending child at a time.  Each family owns a
//! mutable weight table, so exhausting an entry removes it only from that family.
//! Candidates collide against every existing box.  Once the queue is empty, the
//! union of the whole tree (not the start box) is translated into the inclusive
//! 48..70 height interval.
//!
//! # How to change it
//!
//! Keep random draws in their current branch and call order.  In particular, a
//! rejected weighted candidate consumes its selection draw, the five retries are
//! not a redraw loop around the whole family, and the final height draw must stay
//! after the pending queue has drained.
//!
//! # Configuration
//!
//! The two tables and the depth/spread limits below are fixed generation rules.
//!
//! # Dependencies
//!
//! [`RandomSource`] supplies the structure stream; [`BoundingBox`] and
//! [`StructurePiece`] are the structure-stage interchange format.

use std::sync::Arc;

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource};

use super::coded::Facing;
use super::template::{BlockState, Mirror, Rotation};
use super::{BoundingBox, CodedBlock, CodedLoot, StructurePiece};
use crate::dense_grid::DenseBlockGrid;

const MAX_DEPTH: i32 = 30;
const MAX_SPREAD: i32 = 112;
const LOWEST_Y: i32 = 10;
const START_Y: i32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FortressPieceKind {
    Start,
    LongBridge,
    WideJunction,
    SmallJunction,
    RisingJunction,
    SpawnerHall,
    CastleGate,
    CastleHall,
    CastleJunction,
    RightElbow,
    LeftElbow,
    Ascender,
    Balcony,
    Garden,
    EndCap,
}

impl FortressPieceKind {
    fn id(self) -> &'static str {
        match self {
            // The start is persisted with the crossing's piece type, not a
            // synthetic start-only id.
            Self::Start | Self::WideJunction => "minecraft:nebcr",
            Self::LongBridge => "minecraft:nebs",
            Self::SmallJunction => "minecraft:nerc",
            Self::RisingJunction => "minecraft:nesr",
            Self::SpawnerHall => "minecraft:nemt",
            Self::CastleGate => "minecraft:nece",
            Self::CastleHall => "minecraft:nesc",
            Self::CastleJunction => "minecraft:nescsc",
            Self::RightElbow => "minecraft:nescrt",
            Self::LeftElbow => "minecraft:nesclt",
            Self::Ascender => "minecraft:neccs",
            Self::Balcony => "minecraft:nectb",
            Self::Garden => "minecraft:necsr",
            Self::EndCap => "minecraft:nebef",
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    kind: FortressPieceKind,
    box_: BoundingBox,
    facing: Facing,
    depth: i32,
    chest: bool,
    end_seed: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct Weight {
    kind: FortressPieceKind,
    weight: i32,
    max: i32,
    placed: i32,
    repeatable: bool,
}

fn bridge_weights() -> Vec<Weight> {
    vec![
        Weight { kind: FortressPieceKind::LongBridge, weight: 30, max: 0, placed: 0, repeatable: true },
        Weight { kind: FortressPieceKind::WideJunction, weight: 10, max: 4, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::SmallJunction, weight: 10, max: 4, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::RisingJunction, weight: 10, max: 3, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::SpawnerHall, weight: 5, max: 2, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::CastleGate, weight: 5, max: 1, placed: 0, repeatable: false },
    ]
}

fn castle_weights() -> Vec<Weight> {
    vec![
        Weight { kind: FortressPieceKind::CastleHall, weight: 25, max: 0, placed: 0, repeatable: true },
        Weight { kind: FortressPieceKind::CastleJunction, weight: 15, max: 5, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::RightElbow, weight: 5, max: 10, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::LeftElbow, weight: 5, max: 10, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::Ascender, weight: 10, max: 3, placed: 0, repeatable: true },
        Weight { kind: FortressPieceKind::Balcony, weight: 7, max: 2, placed: 0, repeatable: false },
        Weight { kind: FortressPieceKind::Garden, weight: 5, max: 2, placed: 0, repeatable: false },
    ]
}

#[derive(Debug)]
struct Tree {
    pieces: Vec<Node>,
    pending: Vec<usize>,
    bridge: Vec<Weight>,
    castle: Vec<Weight>,
    previous: Option<FortressPieceKind>,
    start: BoundingBox,
}

impl Tree {
    fn collides(&self, candidate: BoundingBox) -> bool {
        self.pieces.iter().any(|piece| piece.box_.intersects(candidate))
    }

    fn add(&mut self, node: Node) -> usize {
        self.pieces.push(node);
        self.pieces.len() - 1
    }

    fn bounds(&self) -> BoundingBox {
        self.pieces.iter().skip(1).fold(self.pieces[0].box_, |all, piece| all.encapsulate(piece.box_))
    }

    fn translate_y(&mut self, delta: i32) {
        for piece in &mut self.pieces {
            piece.box_.min[1] += delta;
            piece.box_.max[1] += delta;
        }
    }
}

fn orient(foot: [i32; 3], off: [i32; 3], size: [i32; 3], facing: Facing) -> BoundingBox {
    let [x, y, z] = foot;
    let [ox, oy, oz] = off;
    let [w, h, d] = size;
    match facing {
        Facing::South => BoundingBox { min: [x + ox, y + oy, z + oz], max: [x + ox + w - 1, y + oy + h - 1, z + oz + d - 1] },
        Facing::North => BoundingBox { min: [x + ox, y + oy, z - d + 1 + oz], max: [x + ox + w - 1, y + oy + h - 1, z + oz] },
        Facing::West => BoundingBox { min: [x - d + 1 + oz, y + oy, z + ox], max: [x + oz, y + oy + h - 1, z + ox + w - 1] },
        Facing::East => BoundingBox { min: [x + oz, y + oy, z + ox], max: [x + oz + d - 1, y + oy + h - 1, z + ox + w - 1] },
    }
}

fn shape(kind: FortressPieceKind) -> ([i32; 3], [i32; 3]) {
    match kind {
        FortressPieceKind::Start | FortressPieceKind::WideJunction => ([-8, -3, 0], [19, 10, 19]),
        FortressPieceKind::LongBridge => ([-1, -3, 0], [5, 10, 19]),
        FortressPieceKind::SmallJunction => ([-2, 0, 0], [7, 9, 7]),
        FortressPieceKind::RisingJunction => ([-2, 0, 0], [7, 11, 7]),
        FortressPieceKind::SpawnerHall => ([-2, 0, 0], [7, 8, 9]),
        FortressPieceKind::CastleGate | FortressPieceKind::Garden => ([-5, -3, 0], [13, 14, 13]),
        FortressPieceKind::CastleHall | FortressPieceKind::CastleJunction | FortressPieceKind::RightElbow | FortressPieceKind::LeftElbow => ([-1, 0, 0], [5, 7, 5]),
        FortressPieceKind::Ascender => ([-1, -7, 0], [5, 14, 10]),
        FortressPieceKind::Balcony => ([-3, 0, 0], [9, 7, 9]),
        FortressPieceKind::EndCap => ([-1, -3, 0], [5, 10, 8]),
    }
}

fn candidate(tree: &Tree, kind: FortressPieceKind, foot: [i32; 3], facing: Facing, depth: i32) -> Option<Node> {
    let (off, size) = shape(kind);
    let box_ = orient(foot, off, size, facing);
    (box_.min[1] > LOWEST_Y && !tree.collides(box_)).then_some(Node {
        kind,
        box_,
        facing,
        depth,
        chest: false,
        end_seed: None,
    })
}

fn end_filler<R: RandomSource>(tree: &Tree, random: &mut R, foot: [i32; 3], facing: Facing, depth: i32) -> Option<Node> {
    let mut node = candidate(tree, FortressPieceKind::EndCap, foot, facing, depth)?;
    // Its persistent local seed is drawn at construction.  The block writer uses
    // it later; advancing here keeps following tree choices aligned.
    node.end_seed = Some(random.next_int());
    Some(node)
}

fn add_weighted<R: RandomSource>(tree: &mut Tree, random: &mut R, foot: [i32; 3], facing: Facing, parent_depth: i32, castle: bool) -> Option<usize> {
    if (foot[0] - tree.start.min[0]).abs() > MAX_SPREAD || (foot[2] - tree.start.min[2]).abs() > MAX_SPREAD {
        // An out-of-range filler is returned to the caller but never enters
        // the builder, so it cannot seed another pending-child expansion.
        let _ = end_filler(tree, random, foot, facing, parent_depth);
        return None;
    }
    let depth = parent_depth + 1;
    let (total, any_limited) = {
        let weights = if castle { &tree.castle } else { &tree.bridge };
        (
            weights.iter().map(|weight| weight.weight).sum(),
            weights.iter().any(|weight| weight.max > 0 && weight.placed < weight.max),
        )
    };
    if total > 0 && any_limited && depth <= MAX_DEPTH {
        for _ in 0..5 {
            let mut selected = random.next_int_bounded(total);
            let mut index = 0;
            while index < if castle { tree.castle.len() } else { tree.bridge.len() } {
                let weight = if castle { tree.castle[index] } else { tree.bridge[index] };
                selected -= weight.weight;
                if selected >= 0 {
                    index += 1;
                    continue;
                }
                // An unavailable selected entry ends this attempt. A selected
                // entry whose geometry cannot fit falls through the remaining
                // table entries without another random draw.
                if (weight.max != 0 && weight.placed >= weight.max)
                    || (tree.previous == Some(weight.kind) && !weight.repeatable)
                {
                    break;
                }
                if let Some(mut node) = candidate(tree, weight.kind, foot, facing, depth) {
                    if matches!(weight.kind, FortressPieceKind::RightElbow | FortressPieceKind::LeftElbow) {
                        node.chest = random.next_int_bounded(3) == 0;
                    }
                    let weights = if castle { &mut tree.castle } else { &mut tree.bridge };
                    weights[index].placed += 1;
                    tree.previous = Some(weight.kind);
                    if weights[index].max != 0 && weights[index].placed >= weights[index].max {
                        weights.remove(index);
                    }
                    let index = tree.add(node);
                    tree.pending.push(index);
                    return Some(index);
                }
                index += 1;
            }
        }
    }
    let node = end_filler(tree, random, foot, facing, depth)?;
    let index = tree.add(node);
    tree.pending.push(index);
    Some(index)
}

fn forward<R: RandomSource>(tree: &mut Tree, random: &mut R, node: Node, x_off: i32, y_off: i32, castle: bool) {
    let b = node.box_;
    let foot = match node.facing {
        Facing::North => [b.min[0] + x_off, b.min[1] + y_off, b.min[2] - 1],
        Facing::South => [b.min[0] + x_off, b.min[1] + y_off, b.max[2] + 1],
        Facing::West => [b.min[0] - 1, b.min[1] + y_off, b.min[2] + x_off],
        Facing::East => [b.max[0] + 1, b.min[1] + y_off, b.min[2] + x_off],
    };
    let _ = add_weighted(tree, random, foot, node.facing, node.depth, castle);
}

fn left<R: RandomSource>(tree: &mut Tree, random: &mut R, node: Node, y_off: i32, z_off: i32, castle: bool) {
    let b = node.box_;
    let (foot, facing) = match node.facing {
        Facing::North | Facing::South => ([b.min[0] - 1, b.min[1] + y_off, b.min[2] + z_off], Facing::West),
        Facing::West | Facing::East => ([b.min[0] + z_off, b.min[1] + y_off, b.min[2] - 1], Facing::North),
    };
    let _ = add_weighted(tree, random, foot, facing, node.depth, castle);
}

fn right<R: RandomSource>(tree: &mut Tree, random: &mut R, node: Node, y_off: i32, z_off: i32, castle: bool) {
    let b = node.box_;
    let (foot, facing) = match node.facing {
        Facing::North | Facing::South => ([b.max[0] + 1, b.min[1] + y_off, b.min[2] + z_off], Facing::East),
        Facing::West | Facing::East => ([b.min[0] + z_off, b.min[1] + y_off, b.max[2] + 1], Facing::South),
    };
    let _ = add_weighted(tree, random, foot, facing, node.depth, castle);
}

fn grow_children<R: RandomSource>(tree: &mut Tree, index: usize, random: &mut R) {
    let node = tree.pieces[index].clone();
    match node.kind {
        FortressPieceKind::Start | FortressPieceKind::WideJunction => {
            forward(tree, random, node.clone(), 8, 3, false);
            left(tree, random, node.clone(), 3, 8, false);
            right(tree, random, node, 3, 8, false);
        }
        FortressPieceKind::LongBridge => forward(tree, random, node, 1, 3, false),
        FortressPieceKind::SmallJunction => {
            forward(tree, random, node.clone(), 2, 0, false);
            left(tree, random, node.clone(), 0, 2, false);
            right(tree, random, node, 0, 2, false);
        }
        FortressPieceKind::RisingJunction => right(tree, random, node, 6, 2, false),
        FortressPieceKind::CastleGate => forward(tree, random, node, 5, 3, true),
        FortressPieceKind::CastleHall => forward(tree, random, node, 1, 0, true),
        FortressPieceKind::CastleJunction => {
            forward(tree, random, node.clone(), 1, 0, true);
            left(tree, random, node.clone(), 0, 1, true);
            right(tree, random, node, 0, 1, true);
        }
        FortressPieceKind::RightElbow => right(tree, random, node, 0, 1, true),
        FortressPieceKind::LeftElbow => left(tree, random, node, 0, 1, true),
        FortressPieceKind::Ascender => forward(tree, random, node, 1, 0, true),
        FortressPieceKind::Balcony => {
            let side = if matches!(node.facing, Facing::West | Facing::North) { 5 } else { 1 };
            let left_opens = random.next_int_bounded(8) > 0;
            left(tree, random, node.clone(), 0, side, left_opens);
            let right_opens = random.next_int_bounded(8) > 0;
            right(tree, random, node, 0, side, right_opens);
        }
        FortressPieceKind::Garden => {
            forward(tree, random, node.clone(), 5, 3, true);
            forward(tree, random, node, 5, 11, true);
        }
        FortressPieceKind::SpawnerHall | FortressPieceKind::EndCap => {}
    }
}

fn local_pos(piece: &Node, x: i32, y: i32, z: i32) -> [i32; 3] {
    let box_ = piece.box_;
    match piece.facing {
        Facing::North => [box_.min[0] + x, box_.min[1] + y, box_.max[2] - z],
        Facing::South => [box_.min[0] + x, box_.min[1] + y, box_.min[2] + z],
        Facing::West => [box_.max[0] - z, box_.min[1] + y, box_.min[2] + x],
        Facing::East => [box_.min[0] + z, box_.min[1] + y, box_.min[2] + x],
    }
}

fn write(blocks: &mut Vec<CodedBlock>, piece: &Node, state: &str, x: i32, y: i32, z: i32) {
    let (mirror, rotation) = match piece.facing {
        Facing::North => (Mirror::None, Rotation::None),
        Facing::South => (Mirror::LeftRight, Rotation::None),
        Facing::West => (Mirror::LeftRight, Rotation::Cw90),
        Facing::East => (Mirror::None, Rotation::Cw90),
    };
    let state = BlockState::parse(state).mirror(mirror).rotate(rotation).canonical();
    blocks.push(CodedBlock { pos: local_pos(piece, x, y, z), state });
}

fn fence(north: bool, east: bool, south: bool, west: bool) -> String {
    format!(
        "minecraft:nether_brick_fence[east={east},north={north},south={south},waterlogged=false,west={west}]"
    )
}

fn box_fill(blocks: &mut Vec<CodedBlock>, piece: &Node, state: &str, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
    for y in y0..=y1 {
        for z in z0..=z1 {
            for x in x0..=x1 {
                write(blocks, piece, state, x, y, z);
            }
        }
    }
}

fn emit_cross(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    box_fill(blocks, piece, BRICK, 7, 3, 0, 11, 4, 18);
    box_fill(blocks, piece, BRICK, 0, 3, 7, 18, 4, 11);
    box_fill(blocks, piece, AIR, 8, 5, 0, 10, 7, 18);
    box_fill(blocks, piece, AIR, 0, 5, 8, 18, 7, 10);
    for (x, z0, z1) in [(7, 0, 7), (7, 11, 18), (11, 0, 7), (11, 11, 18)] {
        box_fill(blocks, piece, BRICK, x, 5, z0, x, 5, z1);
    }
    for (x0, x1, z) in [(0, 7, 7), (11, 18, 7), (0, 7, 11), (11, 18, 11)] {
        box_fill(blocks, piece, BRICK, x0, 5, z, x1, 5, z);
    }
    box_fill(blocks, piece, BRICK, 7, 2, 0, 11, 2, 5);
    box_fill(blocks, piece, BRICK, 7, 2, 13, 11, 2, 18);
    box_fill(blocks, piece, BRICK, 7, 0, 0, 11, 1, 3);
    box_fill(blocks, piece, BRICK, 7, 0, 15, 11, 1, 18);
    box_fill(blocks, piece, BRICK, 0, 2, 7, 5, 2, 11);
    box_fill(blocks, piece, BRICK, 13, 2, 7, 18, 2, 11);
    box_fill(blocks, piece, BRICK, 0, 0, 7, 3, 1, 11);
    box_fill(blocks, piece, BRICK, 15, 0, 7, 18, 1, 11);
}

fn emit_room(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let we = fence(false, true, false, true);
    let ns = fence(true, false, true, false);
    box_fill(blocks, piece, BRICK, 0, 0, 0, 6, 1, 6);
    box_fill(blocks, piece, AIR, 0, 2, 0, 6, 7, 6);
    for (x0, x1, z) in [(0, 1, 0), (0, 1, 6), (5, 6, 0), (5, 6, 6)] {
        box_fill(blocks, piece, BRICK, x0, 2, z, x1, 6, z);
    }
    for (x, z0, z1) in [(0, 0, 1), (0, 5, 6), (6, 0, 1), (6, 5, 6)] {
        box_fill(blocks, piece, BRICK, x, 2, z0, x, 6, z1);
    }
    for (x0, x1, z) in [(2, 4, 0), (2, 4, 6)] {
        box_fill(blocks, piece, BRICK, x0, 6, z, x1, 6, z);
        box_fill(blocks, piece, &we, x0, 5, z, x1, 5, z);
    }
    for (x, z0, z1) in [(0, 2, 4), (6, 2, 4)] {
        box_fill(blocks, piece, BRICK, x, 6, z0, x, 6, z1);
        box_fill(blocks, piece, &ns, x, 5, z0, x, 5, z1);
    }
}

fn emit_rising_room(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let we = fence(false, true, false, true);
    let ns = fence(true, false, true, false);
    box_fill(blocks, piece, BRICK, 0, 0, 0, 6, 1, 6);
    box_fill(blocks, piece, AIR, 0, 2, 0, 6, 10, 6);
    box_fill(blocks, piece, BRICK, 0, 2, 0, 1, 8, 0);
    box_fill(blocks, piece, BRICK, 5, 2, 0, 6, 8, 0);
    box_fill(blocks, piece, BRICK, 0, 2, 1, 0, 8, 6);
    box_fill(blocks, piece, BRICK, 6, 2, 1, 6, 8, 6);
    box_fill(blocks, piece, BRICK, 1, 2, 6, 5, 8, 6);
    box_fill(blocks, piece, &ns, 0, 3, 2, 0, 5, 4);
    box_fill(blocks, piece, &ns, 6, 3, 2, 6, 5, 2);
    box_fill(blocks, piece, &ns, 6, 3, 4, 6, 5, 4);
    write(blocks, piece, BRICK, 5, 2, 5);
    box_fill(blocks, piece, BRICK, 4, 2, 5, 4, 3, 5);
    box_fill(blocks, piece, BRICK, 3, 2, 5, 3, 4, 5);
    box_fill(blocks, piece, BRICK, 2, 2, 5, 2, 5, 5);
    box_fill(blocks, piece, BRICK, 1, 2, 5, 1, 6, 5);
    box_fill(blocks, piece, BRICK, 1, 7, 1, 5, 7, 4);
    box_fill(blocks, piece, AIR, 6, 8, 2, 6, 8, 4);
    box_fill(blocks, piece, BRICK, 2, 6, 0, 4, 8, 0);
    box_fill(blocks, piece, &we, 2, 5, 0, 4, 5, 0);
}

fn is_structure_replaceable(state: &str) -> bool {
    matches!(
        state.split_once('[').map_or(state, |(name, _)| name),
        "minecraft:air"
            | "minecraft:cave_air"
            | "minecraft:void_air"
            | "minecraft:water"
            | "minecraft:lava"
    )
}

fn support_in_placing_chunk(
    piece: &Node,
    placing_cx: i32,
    placing_cz: i32,
    world: &mut DenseBlockGrid,
    x: i32,
    z: i32,
) {
    let mut pos = local_pos(piece, x, -1, z);
    let min_x = placing_cx * 16;
    let min_z = placing_cz * 16;
    while pos[1] > 1
        && (min_x..=min_x + 15).contains(&pos[0])
        && (min_z..=min_z + 15).contains(&pos[2])
        && is_structure_replaceable(world.get(pos[0], pos[1], pos[2]))
    {
        world.set(pos[0], pos[1], pos[2], "minecraft:nether_bricks");
        pos[1] -= 1;
    }
}

fn place_supports_in_chunk(
    piece: &Node,
    placing_cx: i32,
    placing_cz: i32,
    world: &mut DenseBlockGrid,
) {
    match piece.kind {
        FortressPieceKind::Start | FortressPieceKind::WideJunction => {
            for x in 7..=11 {
                for z in 0..=2 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, 18 - z);
                }
            }
            for x in 0..=2 {
                for z in 7..=11 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, 18 - x, z);
                }
            }
        }
        FortressPieceKind::SmallJunction | FortressPieceKind::RisingJunction => {
            for x in 0..=6 {
                for z in 0..=6 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        FortressPieceKind::LongBridge => {
            for x in 0..=4 {
                for z in 0..=2 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, 18 - z);
                }
            }
        }
        FortressPieceKind::CastleGate | FortressPieceKind::Garden => {
            for x in 4..=8 {
                for z in 0..=2 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, 12 - z);
                }
            }
            for x in 0..=2 {
                for z in 4..=8 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, 12 - x, z);
                }
            }
        }
        FortressPieceKind::CastleHall | FortressPieceKind::CastleJunction | FortressPieceKind::RightElbow | FortressPieceKind::LeftElbow => {
            for x in 0..=4 {
                for z in 0..=4 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        FortressPieceKind::Ascender => {
            for x in 0..=4 {
                for z in 0..=9 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        FortressPieceKind::Balcony => {
            for x in 0..=8 {
                for z in 0..=5 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        FortressPieceKind::SpawnerHall => {
            for x in 0..=6 {
                for z in 0..=6 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        _ => {}
    }
}

/// Applies only the terrain-dependent remainder of an already-generated
/// fortress piece. Its immutable block list remains owned by `StructurePiece`.
pub(crate) fn place_cached_piece(
    piece: &StructurePiece,
    kind: FortressPieceKind,
    facing: Facing,
    chest: bool,
    end_seed: Option<i32>,
    placing_cx: i32,
    placing_cz: i32,
    world: &mut DenseBlockGrid,
    placement_random: &mut impl RandomSource,
    solid_render: &dyn Fn(&str) -> bool,
) -> Option<CodedLoot> {
    let node = Node {
        kind,
        box_: piece.bounding_box,
        facing,
        depth: piece.gen_depth,
        chest,
        end_seed,
    };
    let chest_pos = chest_position(&node);
    if let Some(blocks) = &piece.blocks {
        for block in blocks.iter() {
            if chest_pos == Some(block.pos) {
                continue;
            }
            world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
        }
    }
    let loot = chest_pos.and_then(|pos| {
        (pos[0].div_euclid(16) == placing_cx
            && pos[2].div_euclid(16) == placing_cz
            && base_name(world.get(pos[0], pos[1], pos[2])) != "minecraft:chest")
            .then(|| {
                let state = chest_state(world, pos, solid_render);
                world.set(pos[0], pos[1], pos[2], &state);
                CodedLoot {
                    pos,
                    table: "minecraft:chests/nether_bridge".to_string(),
                    seed: placement_random.next_long(),
                }
            })
    });
    place_supports_in_chunk(&node, placing_cx, placing_cz, world);
    loot
}

fn emit_long_bridge(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let nse = fence(true, true, true, false);
    let nsw = fence(true, false, true, true);
    box_fill(blocks, piece, BRICK, 0, 3, 0, 4, 4, 18);
    box_fill(blocks, piece, AIR, 1, 5, 0, 3, 7, 18);
    box_fill(blocks, piece, BRICK, 0, 5, 0, 0, 5, 18);
    box_fill(blocks, piece, BRICK, 4, 5, 0, 4, 5, 18);
    box_fill(blocks, piece, BRICK, 0, 2, 0, 4, 2, 5);
    box_fill(blocks, piece, BRICK, 0, 2, 13, 4, 2, 18);
    box_fill(blocks, piece, BRICK, 0, 0, 0, 4, 1, 3);
    box_fill(blocks, piece, BRICK, 0, 0, 15, 4, 1, 18);
    for (x, fence) in [(0, &nse), (4, &nsw)] {
        box_fill(blocks, piece, fence, x, 1, 1, x, 4, 1);
        box_fill(blocks, piece, fence, x, 3, 4, x, 4, 4);
        box_fill(blocks, piece, fence, x, 3, 14, x, 4, 14);
        box_fill(blocks, piece, fence, x, 1, 17, x, 4, 17);
    }
}

fn emit_small_castle(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let ns = fence(true, false, true, false);
    let we = fence(false, true, false, true);
    box_fill(blocks, piece, BRICK, 0, 0, 0, 4, 1, 4);
    box_fill(blocks, piece, AIR, 0, 2, 0, 4, 5, 4);
    match piece.kind {
        FortressPieceKind::CastleHall => {
            box_fill(blocks, piece, BRICK, 0, 2, 0, 0, 5, 4);
            box_fill(blocks, piece, BRICK, 4, 2, 0, 4, 5, 4);
            for x in [0, 4] {
                box_fill(blocks, piece, &ns, x, 3, 1, x, 4, 1);
                box_fill(blocks, piece, &ns, x, 3, 3, x, 4, 3);
            }
        }
        FortressPieceKind::CastleJunction => {
            for (x, z) in [(0, 0), (4, 0), (0, 4), (4, 4)] {
                box_fill(blocks, piece, BRICK, x, 2, z, x, 5, z);
            }
        }
        FortressPieceKind::LeftElbow => {
            box_fill(blocks, piece, BRICK, 4, 2, 0, 4, 5, 4);
            for z in [1, 3] { box_fill(blocks, piece, &ns, 4, 3, z, 4, 4, z); }
            box_fill(blocks, piece, BRICK, 0, 2, 0, 0, 5, 0);
            box_fill(blocks, piece, BRICK, 0, 2, 4, 3, 5, 4);
            for x in [1, 3] { box_fill(blocks, piece, &we, x, 3, 4, x, 4, 4); }
        }
        FortressPieceKind::RightElbow => {
            box_fill(blocks, piece, BRICK, 0, 2, 0, 0, 5, 4);
            for z in [1, 3] { box_fill(blocks, piece, &ns, 0, 3, z, 0, 4, z); }
            box_fill(blocks, piece, BRICK, 4, 2, 0, 4, 5, 0);
            box_fill(blocks, piece, BRICK, 1, 2, 4, 4, 5, 4);
            for x in [1, 3] { box_fill(blocks, piece, &we, x, 3, 4, x, 4, 4); }
        }
        _ => unreachable!("not a small castle piece"),
    }
    box_fill(blocks, piece, BRICK, 0, 6, 0, 4, 6, 4);
}

fn emit_ascender(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    const STAIRS: &str = "minecraft:nether_brick_stairs[facing=south,half=bottom,shape=straight,waterlogged=false]";
    let ns = fence(true, false, true, false);
    for step in 0..=9 {
        let floor = (7 - step).max(1);
        let roof = (floor + 5).max(14 - step).min(13);
        box_fill(blocks, piece, BRICK, 0, 0, step, 4, floor, step);
        box_fill(blocks, piece, AIR, 1, floor + 1, step, 3, roof - 1, step);
        if step <= 6 { box_fill(blocks, piece, STAIRS, 1, floor + 1, step, 3, floor + 1, step); }
        box_fill(blocks, piece, BRICK, 0, roof, step, 4, roof, step);
        box_fill(blocks, piece, BRICK, 0, floor + 1, step, 0, roof - 1, step);
        box_fill(blocks, piece, BRICK, 4, floor + 1, step, 4, roof - 1, step);
        if step % 2 == 0 {
            box_fill(blocks, piece, &ns, 0, floor + 2, step, 0, floor + 3, step);
            box_fill(blocks, piece, &ns, 4, floor + 2, step, 4, floor + 3, step);
        }
    }
}

fn emit_balcony(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let ns = fence(true, false, true, false);
    let we = fence(false, true, false, true);
    box_fill(blocks, piece, BRICK, 0, 0, 0, 8, 1, 8);
    box_fill(blocks, piece, AIR, 0, 2, 0, 8, 5, 8);
    box_fill(blocks, piece, BRICK, 0, 6, 0, 8, 6, 5);
    box_fill(blocks, piece, BRICK, 0, 2, 0, 2, 5, 0);
    box_fill(blocks, piece, BRICK, 6, 2, 0, 8, 5, 0);
    box_fill(blocks, piece, &we, 1, 3, 0, 1, 4, 0);
    box_fill(blocks, piece, &we, 7, 3, 0, 7, 4, 0);
    box_fill(blocks, piece, BRICK, 0, 2, 4, 8, 2, 8);
    box_fill(blocks, piece, AIR, 1, 1, 4, 2, 2, 4);
    box_fill(blocks, piece, AIR, 6, 1, 4, 7, 2, 4);
    box_fill(blocks, piece, &we, 1, 3, 8, 7, 3, 8);
    write(blocks, piece, &fence(false, true, true, false), 0, 3, 8);
    write(blocks, piece, &fence(false, false, true, true), 8, 3, 8);
    box_fill(blocks, piece, &ns, 0, 3, 6, 0, 3, 7);
    box_fill(blocks, piece, &ns, 8, 3, 6, 8, 3, 7);
    box_fill(blocks, piece, BRICK, 0, 3, 4, 0, 5, 5);
    box_fill(blocks, piece, BRICK, 8, 3, 4, 8, 5, 5);
    box_fill(blocks, piece, BRICK, 1, 3, 5, 2, 5, 5);
    box_fill(blocks, piece, BRICK, 6, 3, 5, 7, 5, 5);
    box_fill(blocks, piece, &we, 1, 4, 5, 1, 5, 5);
    box_fill(blocks, piece, &we, 7, 4, 5, 7, 5, 5);
}

fn emit_large_frame(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    box_fill(blocks, piece, BRICK, 0, 3, 0, 12, 4, 12);
    box_fill(blocks, piece, AIR, 0, 5, 0, 12, 13, 12);
    box_fill(blocks, piece, BRICK, 0, 5, 0, 1, 12, 12);
    box_fill(blocks, piece, BRICK, 11, 5, 0, 12, 12, 12);
    box_fill(blocks, piece, BRICK, 2, 5, 11, 4, 12, 12);
    box_fill(blocks, piece, BRICK, 8, 5, 11, 10, 12, 12);
    box_fill(blocks, piece, BRICK, 5, 9, 11, 7, 12, 12);
    box_fill(blocks, piece, BRICK, 2, 5, 0, 4, 12, 1);
    box_fill(blocks, piece, BRICK, 8, 5, 0, 10, 12, 1);
    box_fill(blocks, piece, BRICK, 5, 9, 0, 7, 12, 1);
    box_fill(blocks, piece, BRICK, 2, 11, 2, 10, 12, 10);
}

fn emit_large_rails(blocks: &mut Vec<CodedBlock>, piece: &Node, joined: bool) {
    const BRICK: &str = "minecraft:nether_bricks";
    let we = fence(false, true, false, true);
    let ns = fence(true, false, true, false);
    for i in (1..=11).step_by(2) {
        box_fill(blocks, piece, &we, i, 10, 0, i, 11, 0);
        box_fill(blocks, piece, &we, i, 10, 12, i, 11, 12);
        box_fill(blocks, piece, &ns, 0, 10, i, 0, 11, i);
        box_fill(blocks, piece, &ns, 12, 10, i, 12, 11, i);
        write(blocks, piece, BRICK, i, 13, 0);
        write(blocks, piece, BRICK, i, 13, 12);
        write(blocks, piece, BRICK, 0, 13, i);
        write(blocks, piece, BRICK, 12, 13, i);
        if i != 11 {
            write(blocks, piece, &we, i + 1, 13, 0);
            write(blocks, piece, &we, i + 1, 13, 12);
            write(blocks, piece, &ns, 0, 13, i + 1);
            write(blocks, piece, &ns, 12, 13, i + 1);
        }
    }
    write(blocks, piece, &fence(true, true, false, false), 0, 13, 0);
    write(blocks, piece, &fence(false, true, true, false), 0, 13, 12);
    write(blocks, piece, &fence(false, false, true, true), 12, 13, 12);
    write(blocks, piece, &fence(true, false, false, true), 12, 13, 0);
    for z in (3..=9).step_by(2) {
        let left = if joined { fence(true, false, true, true) } else { fence(true, false, true, false) };
        let right = if joined { fence(true, true, true, false) } else { fence(true, false, true, false) };
        box_fill(blocks, piece, &left, 1, 7, z, 1, 8, z);
        box_fill(blocks, piece, &right, 11, 7, z, 11, 8, z);
    }
}

fn emit_large_foundation(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    box_fill(blocks, piece, BRICK, 4, 2, 0, 8, 2, 12);
    box_fill(blocks, piece, BRICK, 0, 2, 4, 12, 2, 8);
    box_fill(blocks, piece, BRICK, 4, 0, 0, 8, 1, 3);
    box_fill(blocks, piece, BRICK, 4, 0, 9, 8, 1, 12);
    box_fill(blocks, piece, BRICK, 0, 0, 4, 3, 1, 8);
    box_fill(blocks, piece, BRICK, 9, 0, 4, 12, 1, 8);
}

fn emit_castle_gate(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    emit_large_frame(blocks, piece);
    box_fill(blocks, piece, &fence(false, false, false, false), 5, 8, 0, 7, 8, 0);
    emit_large_rails(blocks, piece, true);
    emit_large_foundation(blocks, piece);
    box_fill(blocks, piece, BRICK, 5, 5, 5, 7, 5, 7);
    box_fill(blocks, piece, AIR, 6, 1, 6, 6, 4, 6);
    write(blocks, piece, BRICK, 6, 0, 6);
    write(blocks, piece, "minecraft:lava", 6, 5, 6);
}

fn emit_garden(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    const NORTH_STAIRS: &str = "minecraft:nether_brick_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]";
    const EAST_STAIRS: &str = "minecraft:nether_brick_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]";
    const WEST_STAIRS: &str = "minecraft:nether_brick_stairs[facing=west,half=bottom,shape=straight,waterlogged=false]";
    emit_large_frame(blocks, piece);
    emit_large_rails(blocks, piece, true);
    for i in 0..=6 {
        let z = i + 4;
        box_fill(blocks, piece, NORTH_STAIRS, 5, 5 + i, z, 7, 5 + i, z);
        if (5..=8).contains(&z) {
            box_fill(blocks, piece, BRICK, 5, 5, z, 7, i + 4, z);
        } else if (9..=10).contains(&z) {
            box_fill(blocks, piece, BRICK, 5, 8, z, 7, i + 4, z);
        }
        if i >= 1 { box_fill(blocks, piece, AIR, 5, 6 + i, z, 7, 9 + i, z); }
    }
    box_fill(blocks, piece, NORTH_STAIRS, 5, 12, 11, 7, 12, 11);
    box_fill(blocks, piece, &fence(true, true, true, false), 5, 6, 7, 5, 7, 7);
    box_fill(blocks, piece, &fence(true, false, true, true), 7, 6, 7, 7, 7, 7);
    box_fill(blocks, piece, AIR, 5, 13, 12, 7, 13, 12);
    box_fill(blocks, piece, BRICK, 2, 5, 2, 3, 5, 3);
    box_fill(blocks, piece, BRICK, 2, 5, 9, 3, 5, 10);
    box_fill(blocks, piece, BRICK, 2, 5, 4, 2, 5, 8);
    box_fill(blocks, piece, BRICK, 9, 5, 2, 10, 5, 3);
    box_fill(blocks, piece, BRICK, 9, 5, 9, 10, 5, 10);
    box_fill(blocks, piece, BRICK, 10, 5, 4, 10, 5, 8);
    box_fill(blocks, piece, WEST_STAIRS, 4, 5, 2, 4, 5, 3);
    box_fill(blocks, piece, WEST_STAIRS, 4, 5, 9, 4, 5, 10);
    box_fill(blocks, piece, EAST_STAIRS, 8, 5, 2, 8, 5, 3);
    box_fill(blocks, piece, EAST_STAIRS, 8, 5, 9, 8, 5, 10);
    box_fill(blocks, piece, "minecraft:soul_sand", 3, 4, 4, 4, 4, 8);
    box_fill(blocks, piece, "minecraft:soul_sand", 8, 4, 4, 9, 4, 8);
    box_fill(blocks, piece, "minecraft:nether_wart[age=0]", 3, 5, 4, 4, 5, 8);
    box_fill(blocks, piece, "minecraft:nether_wart[age=0]", 8, 5, 4, 9, 5, 8);
    emit_large_foundation(blocks, piece);
}

fn emit_spawner_hall(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    let we = fence(false, true, false, true);
    let ns = fence(true, false, true, false);
    box_fill(blocks, piece, AIR, 0, 2, 0, 6, 7, 7);
    box_fill(blocks, piece, BRICK, 1, 0, 0, 5, 1, 7);
    box_fill(blocks, piece, BRICK, 1, 2, 1, 5, 2, 7);
    box_fill(blocks, piece, BRICK, 1, 3, 2, 5, 3, 7);
    box_fill(blocks, piece, BRICK, 1, 4, 3, 5, 4, 7);
    box_fill(blocks, piece, BRICK, 1, 2, 0, 1, 4, 2);
    box_fill(blocks, piece, BRICK, 5, 2, 0, 5, 4, 2);
    box_fill(blocks, piece, BRICK, 1, 5, 2, 1, 5, 3);
    box_fill(blocks, piece, BRICK, 5, 5, 2, 5, 5, 3);
    box_fill(blocks, piece, BRICK, 0, 5, 3, 0, 5, 8);
    box_fill(blocks, piece, BRICK, 6, 5, 3, 6, 5, 8);
    box_fill(blocks, piece, BRICK, 1, 5, 8, 5, 5, 8);
    write(blocks, piece, &fence(false, false, false, true), 1, 6, 3);
    write(blocks, piece, &fence(false, true, false, false), 5, 6, 3);
    write(blocks, piece, &fence(true, true, false, false), 0, 6, 3);
    write(blocks, piece, &fence(true, false, false, true), 6, 6, 3);
    box_fill(blocks, piece, &ns, 0, 6, 4, 0, 6, 7);
    box_fill(blocks, piece, &ns, 6, 6, 4, 6, 6, 7);
    write(blocks, piece, &fence(false, true, true, false), 0, 6, 8);
    write(blocks, piece, &fence(false, false, true, true), 6, 6, 8);
    box_fill(blocks, piece, &we, 1, 6, 8, 5, 6, 8);
    write(blocks, piece, &fence(false, true, false, false), 1, 7, 8);
    box_fill(blocks, piece, &we, 2, 7, 8, 4, 7, 8);
    write(blocks, piece, &fence(false, false, false, true), 5, 7, 8);
    write(blocks, piece, &fence(false, true, false, false), 2, 8, 8);
    write(blocks, piece, &we, 3, 8, 8);
    write(blocks, piece, &fence(false, false, false, true), 4, 8, 8);
    write(blocks, piece, "minecraft:spawner", 3, 5, 5);
}

fn emit_end_cap(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    let mut random = LegacyRandomSource::new(i64::from(piece.end_seed.unwrap_or_default()));
    for x in 0..=4 {
        for y in 3..=4 {
            let z = random.next_int_bounded(8);
            box_fill(blocks, piece, BRICK, x, y, 0, x, y, z);
        }
    }
    let z = random.next_int_bounded(8);
    box_fill(blocks, piece, BRICK, 0, 5, 0, 0, 5, z);
    let z = random.next_int_bounded(8);
    box_fill(blocks, piece, BRICK, 4, 5, 0, 4, 5, z);
    for x in 0..=4 {
        let z = random.next_int_bounded(5);
        box_fill(blocks, piece, BRICK, x, 2, 0, x, 2, z);
    }
    for x in 0..=4 {
        for y in 0..=1 {
            let z = random.next_int_bounded(3);
            box_fill(blocks, piece, BRICK, x, y, 0, x, y, z);
        }
    }
}

/// Replays each piece's local block-writing sequence. Supports remain in the
/// placement pass because only that pass can see the receiving chunk's terrain.
fn emit_blocks(piece: &Node) -> Vec<CodedBlock> {
    let mut blocks = Vec::new();
    match piece.kind {
        FortressPieceKind::LongBridge => emit_long_bridge(&mut blocks, piece),
        FortressPieceKind::WideJunction | FortressPieceKind::Start => emit_cross(&mut blocks, piece),
        FortressPieceKind::SmallJunction => emit_room(&mut blocks, piece),
        FortressPieceKind::RisingJunction => emit_rising_room(&mut blocks, piece),
        FortressPieceKind::SpawnerHall => emit_spawner_hall(&mut blocks, piece),
        FortressPieceKind::CastleGate => emit_castle_gate(&mut blocks, piece),
        FortressPieceKind::CastleHall | FortressPieceKind::CastleJunction | FortressPieceKind::RightElbow | FortressPieceKind::LeftElbow => emit_small_castle(&mut blocks, piece),
        FortressPieceKind::Ascender => emit_ascender(&mut blocks, piece),
        FortressPieceKind::Balcony => emit_balcony(&mut blocks, piece),
        FortressPieceKind::Garden => emit_garden(&mut blocks, piece),
        FortressPieceKind::EndCap => emit_end_cap(&mut blocks, piece),
    }
    if piece.chest {
        let x = if matches!(piece.kind, FortressPieceKind::RightElbow) { 1 } else { 3 };
        let pos = local_pos(piece, x, 2, 3);
        // This eager block list has no receiving chunk grid. The external
        // state is wall-dependent, so keep the default state until container
        // placement is moved to that grid-aware stage.
        blocks.push(CodedBlock {
            pos,
            state: "minecraft:chest[facing=north,type=single,waterlogged=false]".to_string(),
        });
    }
    blocks
}

fn chest_position(piece: &Node) -> Option<[i32; 3]> {
    piece.chest.then(|| {
        let x = if matches!(piece.kind, FortressPieceKind::RightElbow) { 1 } else { 3 };
        local_pos(piece, x, 2, 3)
    })
}

fn base_name(state: &str) -> &str {
    state.split_once('[').map_or(state, |(base, _)| base)
}

/// Reorients one chest against the receiving chunk's already-written grid.
///
/// Kept here as the shared state rule for fortress and coded-piece placement:
/// both consumers must inspect the same four horizontal neighbours and must not
/// spend a placement-random draw while choosing the facing.
pub(crate) fn chest_state(
    world: &DenseBlockGrid,
    pos: [i32; 3],
    solid_render: &dyn Fn(&str) -> bool,
) -> String {
    const DIRECTIONS: [(&str, i32, i32, &str); 4] = [
        ("north", 0, -1, "south"),
        ("east", 1, 0, "west"),
        ("south", 0, 1, "north"),
        ("west", -1, 0, "east"),
    ];
    let mut single_solid = None;
    for (direction, dx, dz, opposite) in DIRECTIONS {
        let neighbor = world.get(pos[0] + dx, pos[1], pos[2] + dz);
        if base_name(neighbor) == "minecraft:chest" {
            return "minecraft:chest[facing=north,type=single,waterlogged=false]".to_string();
        }
        if solid_render(neighbor) {
            if single_solid.is_some() {
                single_solid = None;
                break;
            }
            single_solid = Some(opposite);
        }
        let _ = direction;
    }
    if let Some(facing) = single_solid {
        return format!("minecraft:chest[facing={facing},type=single,waterlogged=false]");
    }

    let mut facing = 0usize;
    for turn in [2usize, 1, 2] {
        let (_, dx, dz, _) = DIRECTIONS[facing];
        if !solid_render(world.get(pos[0] + dx, pos[1], pos[2] + dz)) {
            break;
        }
        facing = (facing + turn) % DIRECTIONS.len();
    }
    format!(
        "minecraft:chest[facing={},type=single,waterlogged=false]",
        DIRECTIONS[facing].0,
    )
}

fn finish(tree: Tree) -> Vec<StructurePiece> {
    tree.pieces.into_iter().map(|piece| {
        let blocks = emit_blocks(&piece);
        StructurePiece {
        id: piece.kind.id().to_string(),
        bounding_box: piece.box_,
        orientation: Some(piece.facing.data_2d()),
        gen_depth: piece.depth,
        template: None,
        placement: None,
        extra_placements: Vec::new(),
        blocks: Some(Arc::new(blocks)),
        loot: Vec::new(),
        beard: None,
        refine: Some(super::PieceRefinement::FortressPlacement {
            kind: piece.kind,
            facing: piece.facing,
            chest: piece.chest,
            end_seed: piece.end_seed,
        }),
    }}).collect()
}

fn build_tree<R: RandomSource>(cx: i32, cz: i32, random: &mut R) -> (Tree, [i32; 3]) {
    let facing = Facing::random(random);
    let start = BoundingBox {
        min: [cx * 16 + 2, START_Y, cz * 16 + 2],
        max: [cx * 16 + 20, START_Y + 9, cz * 16 + 20],
    };
    let mut tree = Tree { pieces: Vec::new(), pending: Vec::new(), bridge: bridge_weights(), castle: castle_weights(), previous: None, start };
    let root = tree.add(Node { kind: FortressPieceKind::Start, box_: start, facing, depth: 0, chest: false, end_seed: None });
    grow_children(&mut tree, root, random);
    while !tree.pending.is_empty() {
        let index = tree.pending.remove(random.next_int_bounded(tree.pending.len() as i32) as usize);
        grow_children(&mut tree, index, random);
    }
    let bounds = tree.bounds();
    let available = 70 - 48 + 1 - (bounds.max[1] - bounds.min[1] + 1);
    let target_min = if available > 1 { 48 + random.next_int_bounded(available) } else { 48 };
    let delta = target_min - bounds.min[1];
    tree.translate_y(delta);
    (tree, [cx * 16, START_Y + delta, cz * 16])
}

/// Builds all pieces for one placed start, using the caller's structure stream.
/// The returned start point is the translated root corner.
#[must_use]
pub fn generate<R: RandomSource>(cx: i32, cz: i32, random: &mut R) -> (Vec<StructurePiece>, [i32; 3]) {
    let (tree, origin) = build_tree(cx, cz, random);
    (finish(tree), origin)
}

/// Places one fortress start into its current receiving chunk, including
/// supports that need the post-carve grid to locate their solid boundary.
pub fn place_for_chunk<R: RandomSource, P: RandomSource>(
    start_cx: i32,
    start_cz: i32,
    placing_cx: i32,
    placing_cz: i32,
    world: &mut DenseBlockGrid,
    tree_random: &mut R,
    placement_random: &mut P,
    solid_render: &dyn Fn(&str) -> bool,
) -> Vec<CodedLoot> {
    let (tree, _) = build_tree(start_cx, start_cz, tree_random);
    let mut loot = Vec::new();
    for piece in tree.pieces {
        let blocks = emit_blocks(&piece);
        for block in blocks {
            if chest_position(&piece) == Some(block.pos) {
                continue;
            }
            world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
        }
        if let Some(pos) = chest_position(&piece)
            && pos[0].div_euclid(16) == placing_cx
            && pos[2].div_euclid(16) == placing_cz
            && base_name(world.get(pos[0], pos[1], pos[2])) != "minecraft:chest"
        {
            let state = chest_state(world, pos, solid_render);
            world.set(pos[0], pos[1], pos[2], &state);
            loot.push(CodedLoot {
                pos,
                table: "minecraft:chests/nether_bridge".to_string(),
                seed: placement_random.next_long(),
            });
        }
        place_supports_in_chunk(&piece, placing_cx, placing_cz, world);
    }
    loot
}

#[cfg(test)]
mod tests {
    use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom, XoroshiroRandomSource};
    use super::*;

    fn stream(seed: i64, cx: i32, cz: i32) -> WorldgenRandom<LegacyRandomSource> {
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
        random.set_large_feature_seed(seed, cx, cz);
        random
    }

    #[test]
    fn seed_42_external_control_has_full_tree_and_translated_boxes() {
        let mut random = stream(42, 0, 0);
        let (pieces, origin) = generate(0, 0, &mut random);
        assert_eq!(pieces.len(), 90);
        assert_eq!(origin, [0, 69, 0]);
        assert_eq!(pieces[0].bounding_box, BoundingBox { min: [2, 69, 2], max: [20, 78, 20] });
        assert!(pieces.iter().any(|piece| piece.bounding_box == BoundingBox { min: [-5, 72, 8], max: [1, 80, 14] }));
        assert!(pieces.iter().any(|piece| piece.bounding_box == BoundingBox { min: [-5, 72, 15], max: [1, 80, 21] }));
        assert!(pieces.iter().any(|piece| piece.bounding_box == BoundingBox { min: [-5, 72, 1], max: [1, 82, 7] }));
        let root_blocks = pieces[0].blocks.as_ref().expect("root has emitted blocks");
        assert!(root_blocks.iter().any(|block| (0..16).contains(&block.pos[0]) && (0..16).contains(&block.pos[2]) && block.state == "minecraft:nether_bricks"));
    }

    #[test]
    fn negative_control_does_not_accept_the_root_only_height() {
        let mut random = stream(42, 0, 0);
        let (pieces, _) = generate(0, 0, &mut random);
        let all = pieces.iter().fold(pieces[0].bounding_box, |box_, piece| box_.encapsulate(piece.bounding_box));
        assert_eq!(all.min[1], 48, "the complete tree supplies the interval anchor: {all:?}");
        assert_eq!(pieces[0].bounding_box.min[1], 69);
        assert_ne!(pieces[0].bounding_box.min[1], all.min[1], "a root-only move would ignore the completed tree's lower extent");
    }

    #[test]
    fn post_carve_chunk_placement_fills_only_the_six_replaceable_support_cells() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 16, 128, 16, "minecraft:netherrack");
        let expected = [[4, 67, 9], [4, 67, 10], [9, 68, 4], [10, 68, 4], [4, 68, 9], [4, 68, 10]];
        for [x, y, z] in expected {
            world.set(x, y, z, "minecraft:cave_air");
        }
        let mut random = stream(42, 0, 0);
        let mut placement = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        let decoration_seed = placement.set_decoration_seed(42, 0, 0);
        placement.set_feature_seed(decoration_seed, 1, 7);
        place_for_chunk(
            0,
            0,
            0,
            0,
            &mut world,
            &mut random,
            &mut placement,
            &|state| !matches!(base_name(state), "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air" | "minecraft:lava"),
        );
        for [x, y, z] in expected {
            assert_eq!(world.get(x, y, z), "minecraft:nether_bricks", "support at ({x},{y},{z})");
        }
        assert_eq!(world.get(9, 68, 2), "minecraft:netherrack", "solid terrain is the negative control");
    }
}
