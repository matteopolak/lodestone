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

use lodestone_worldgen_core::rng::RandomSource;

use super::coded::Facing;
use super::{BoundingBox, CodedBlock, CodedLoot, StructurePiece};
use crate::dense_grid::DenseBlockGrid;

const MAX_DEPTH: i32 = 30;
const MAX_SPREAD: i32 = 112;
const LOWEST_Y: i32 = 10;
const START_Y: i32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
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

impl Kind {
    fn id(self) -> &'static str {
        match self {
            Self::Start => "minecraft:NeStart",
            Self::LongBridge => "minecraft:NeBS",
            Self::WideJunction => "minecraft:NeBCr",
            Self::SmallJunction => "minecraft:NeRC",
            Self::RisingJunction => "minecraft:NeSR",
            Self::SpawnerHall => "minecraft:NeMT",
            Self::CastleGate => "minecraft:NeCE",
            Self::CastleHall => "minecraft:NeSC",
            Self::CastleJunction => "minecraft:NeSCSC",
            Self::RightElbow => "minecraft:NeSCRT",
            Self::LeftElbow => "minecraft:NeSCLT",
            Self::Ascender => "minecraft:NeCCS",
            Self::Balcony => "minecraft:NeCTB",
            Self::Garden => "minecraft:NeCSR",
            Self::EndCap => "minecraft:NeBEF",
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    kind: Kind,
    box_: BoundingBox,
    facing: Facing,
    depth: i32,
    chest: bool,
    end_seed: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct Weight {
    kind: Kind,
    weight: i32,
    max: i32,
    placed: i32,
    repeatable: bool,
}

fn bridge_weights() -> Vec<Weight> {
    vec![
        Weight { kind: Kind::LongBridge, weight: 30, max: 0, placed: 0, repeatable: true },
        Weight { kind: Kind::WideJunction, weight: 10, max: 4, placed: 0, repeatable: false },
        Weight { kind: Kind::SmallJunction, weight: 10, max: 4, placed: 0, repeatable: false },
        Weight { kind: Kind::RisingJunction, weight: 10, max: 3, placed: 0, repeatable: false },
        Weight { kind: Kind::SpawnerHall, weight: 5, max: 2, placed: 0, repeatable: false },
        Weight { kind: Kind::CastleGate, weight: 5, max: 1, placed: 0, repeatable: false },
    ]
}

fn castle_weights() -> Vec<Weight> {
    vec![
        Weight { kind: Kind::CastleHall, weight: 25, max: 0, placed: 0, repeatable: true },
        Weight { kind: Kind::CastleJunction, weight: 15, max: 5, placed: 0, repeatable: false },
        Weight { kind: Kind::RightElbow, weight: 5, max: 10, placed: 0, repeatable: false },
        Weight { kind: Kind::LeftElbow, weight: 5, max: 10, placed: 0, repeatable: false },
        Weight { kind: Kind::Ascender, weight: 10, max: 3, placed: 0, repeatable: true },
        Weight { kind: Kind::Balcony, weight: 7, max: 2, placed: 0, repeatable: false },
        Weight { kind: Kind::Garden, weight: 5, max: 2, placed: 0, repeatable: false },
    ]
}

#[derive(Debug)]
struct Tree {
    pieces: Vec<Node>,
    pending: Vec<usize>,
    bridge: Vec<Weight>,
    castle: Vec<Weight>,
    previous: Option<Kind>,
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

fn shape(kind: Kind) -> ([i32; 3], [i32; 3]) {
    match kind {
        Kind::Start | Kind::WideJunction => ([-8, -3, 0], [19, 10, 19]),
        Kind::LongBridge => ([-1, -3, 0], [5, 10, 19]),
        Kind::SmallJunction => ([-2, 0, 0], [7, 9, 7]),
        Kind::RisingJunction => ([-2, 0, 0], [7, 11, 7]),
        Kind::SpawnerHall => ([-2, 0, 0], [7, 8, 9]),
        Kind::CastleGate | Kind::Garden => ([-5, -3, 0], [13, 14, 13]),
        Kind::CastleHall | Kind::CastleJunction | Kind::RightElbow | Kind::LeftElbow => ([-1, 0, 0], [5, 7, 5]),
        Kind::Ascender => ([-1, -7, 0], [5, 14, 10]),
        Kind::Balcony => ([-3, 0, 0], [9, 7, 9]),
        Kind::EndCap => ([-1, -3, 0], [5, 10, 8]),
    }
}

fn candidate(tree: &Tree, kind: Kind, foot: [i32; 3], facing: Facing, depth: i32) -> Option<Node> {
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
    let mut node = candidate(tree, Kind::EndCap, foot, facing, depth)?;
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
                    if matches!(weight.kind, Kind::RightElbow | Kind::LeftElbow) {
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
        Kind::Start | Kind::WideJunction => {
            forward(tree, random, node.clone(), 8, 3, false);
            left(tree, random, node.clone(), 3, 8, false);
            right(tree, random, node, 3, 8, false);
        }
        Kind::LongBridge => forward(tree, random, node, 1, 3, false),
        Kind::SmallJunction => {
            forward(tree, random, node.clone(), 2, 0, false);
            left(tree, random, node.clone(), 0, 2, false);
            right(tree, random, node, 0, 2, false);
        }
        Kind::RisingJunction => right(tree, random, node, 6, 2, false),
        Kind::CastleGate => forward(tree, random, node, 5, 3, true),
        Kind::CastleHall => forward(tree, random, node, 1, 0, true),
        Kind::CastleJunction => {
            forward(tree, random, node.clone(), 1, 0, true);
            left(tree, random, node.clone(), 0, 1, true);
            right(tree, random, node, 0, 1, true);
        }
        Kind::RightElbow => right(tree, random, node, 0, 1, true),
        Kind::LeftElbow => left(tree, random, node, 0, 1, true),
        Kind::Ascender => forward(tree, random, node, 1, 0, true),
        Kind::Balcony => {
            let side = if matches!(node.facing, Facing::West | Facing::North) { 5 } else { 1 };
            let left_opens = random.next_int_bounded(8) > 0;
            left(tree, random, node.clone(), 0, side, left_opens);
            let right_opens = random.next_int_bounded(8) > 0;
            right(tree, random, node, 0, side, right_opens);
        }
        Kind::Garden => {
            forward(tree, random, node.clone(), 5, 3, true);
            forward(tree, random, node, 5, 11, true);
        }
        Kind::SpawnerHall | Kind::EndCap => {}
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
    blocks.push(CodedBlock { pos: local_pos(piece, x, y, z), state: state.to_string() });
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
    const WE: &str = "minecraft:nether_brick_fence[east=true,north=false,south=false,waterlogged=false,west=true]";
    const NS: &str = "minecraft:nether_brick_fence[east=false,north=true,south=true,waterlogged=false,west=false]";
    let (we, ns) = if piece.facing.is_z_axis() { (WE, NS) } else { (NS, WE) };
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
        box_fill(blocks, piece, we, x0, 5, z, x1, 5, z);
    }
    for (x, z0, z1) in [(0, 2, 4), (6, 2, 4)] {
        box_fill(blocks, piece, BRICK, x, 6, z0, x, 6, z1);
        box_fill(blocks, piece, ns, x, 5, z0, x, 5, z1);
    }
}

fn emit_rising_room(blocks: &mut Vec<CodedBlock>, piece: &Node) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    const WE: &str = "minecraft:nether_brick_fence[east=true,north=false,south=false,waterlogged=false,west=true]";
    const NS: &str = "minecraft:nether_brick_fence[east=false,north=true,south=true,waterlogged=false,west=false]";
    let (we, ns) = if piece.facing.is_z_axis() { (WE, NS) } else { (NS, WE) };
    box_fill(blocks, piece, BRICK, 0, 0, 0, 6, 1, 6);
    box_fill(blocks, piece, AIR, 0, 2, 0, 6, 10, 6);
    box_fill(blocks, piece, BRICK, 0, 2, 0, 1, 8, 0);
    box_fill(blocks, piece, BRICK, 5, 2, 0, 6, 8, 0);
    box_fill(blocks, piece, BRICK, 0, 2, 1, 0, 8, 6);
    box_fill(blocks, piece, BRICK, 6, 2, 1, 6, 8, 6);
    box_fill(blocks, piece, BRICK, 1, 2, 6, 5, 8, 6);
    box_fill(blocks, piece, ns, 0, 3, 2, 0, 5, 4);
    box_fill(blocks, piece, ns, 6, 3, 2, 6, 5, 2);
    box_fill(blocks, piece, ns, 6, 3, 4, 6, 5, 4);
    write(blocks, piece, BRICK, 5, 2, 5);
    box_fill(blocks, piece, BRICK, 4, 2, 5, 4, 3, 5);
    box_fill(blocks, piece, BRICK, 3, 2, 5, 3, 4, 5);
    box_fill(blocks, piece, BRICK, 2, 2, 5, 2, 5, 5);
    box_fill(blocks, piece, BRICK, 1, 2, 5, 1, 6, 5);
    box_fill(blocks, piece, BRICK, 1, 7, 1, 5, 7, 4);
    box_fill(blocks, piece, AIR, 6, 8, 2, 6, 8, 4);
    box_fill(blocks, piece, BRICK, 2, 6, 0, 4, 8, 0);
    box_fill(blocks, piece, we, 2, 5, 0, 4, 5, 0);
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
        Kind::Start | Kind::WideJunction => {
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
        Kind::SmallJunction | Kind::RisingJunction => {
            for x in 0..=6 {
                for z in 0..=6 {
                    support_in_placing_chunk(piece, placing_cx, placing_cz, world, x, z);
                }
            }
        }
        _ => {}
    }
}

/// Resolves the common masonry shell, its front/back passages, and the
/// piece-specific interior landmarks. Downward supports deliberately stop at the
/// piece floor here: the start-time API has no stable post-carve terrain read, so
/// an unbounded column would be a different class of eager-world-read error.
fn emit_blocks<R: RandomSource>(piece: &Node, random: &mut R) -> (Vec<CodedBlock>, Vec<CodedLoot>) {
    const BRICK: &str = "minecraft:nether_bricks";
    const AIR: &str = "minecraft:air";
    const FENCE: &str = "minecraft:nether_brick_fence[east=false,north=false,south=false,waterlogged=false,west=false]";
    const STAIRS: &str = "minecraft:nether_brick_stairs[facing=south,half=bottom,shape=straight,waterlogged=false]";
    let (size_off, size) = shape(piece.kind);
    let _ = size_off;
    let [width, height, depth] = size;
    let mut blocks = Vec::new();
    // Most variants are closed shells. The three bridge-family rooms below
    // have their own sparse operation sequences instead.
    if !matches!(piece.kind, Kind::Start | Kind::WideJunction | Kind::SmallJunction | Kind::RisingJunction) {
        box_fill(&mut blocks, piece, BRICK, 0, 0, 0, width - 1, 1.min(height - 1), depth - 1);
        box_fill(&mut blocks, piece, AIR, 1, 2, 1, width - 2, height - 2, depth - 2);
        box_fill(&mut blocks, piece, BRICK, 0, 2, 0, width - 1, height - 1, 0);
        box_fill(&mut blocks, piece, BRICK, 0, 2, depth - 1, width - 1, height - 1, depth - 1);
        box_fill(&mut blocks, piece, BRICK, 0, 2, 0, 0, height - 1, depth - 1);
        box_fill(&mut blocks, piece, BRICK, width - 1, 2, 0, width - 1, height - 1, depth - 1);
        box_fill(&mut blocks, piece, BRICK, 0, height - 1, 0, width - 1, height - 1, depth - 1);
        // A three-wide, three-tall portal at both longitudinal faces links
        // every generated child to its parent rather than producing sealed boxes.
        let middle = width / 2;
        box_fill(&mut blocks, piece, AIR, middle - 1, 2, 0, middle + 1, 4.min(height - 2), 0);
        box_fill(&mut blocks, piece, AIR, middle - 1, 2, depth - 1, middle + 1, 4.min(height - 2), depth - 1);
    }
    match piece.kind {
        Kind::LongBridge => {
            box_fill(&mut blocks, piece, BRICK, 0, 3, 0, width - 1, 4, depth - 1);
            box_fill(&mut blocks, piece, AIR, 1, 5, 0, width - 2, 7, depth - 1);
            box_fill(&mut blocks, piece, FENCE, 0, 1, 1, 0, 4, depth - 2);
            box_fill(&mut blocks, piece, FENCE, width - 1, 1, 1, width - 1, 4, depth - 2);
        }
        Kind::WideJunction | Kind::Start => emit_cross(&mut blocks, piece),
        Kind::SmallJunction => emit_room(&mut blocks, piece),
        Kind::RisingJunction => emit_rising_room(&mut blocks, piece),
        Kind::Ascender => {
            for step in 0..depth.min(10) {
                let floor = (height - 3 - step).max(1);
                box_fill(&mut blocks, piece, BRICK, 0, 0, step, width - 1, floor, step);
                if step < 7 && width > 2 && floor + 1 < height {
                    box_fill(&mut blocks, piece, STAIRS, 1, floor + 1, step, width - 2, floor + 1, step);
                }
            }
        }
        Kind::SpawnerHall => {
            write(&mut blocks, piece, "minecraft:spawner", width / 2, 5.min(height - 1), 5.min(depth - 1));
            box_fill(&mut blocks, piece, FENCE, 0, 6.min(height - 1), 3, width - 1, 6.min(height - 1), depth - 1);
        }
        Kind::Garden => {
            let y = 5.min(height - 2);
            box_fill(&mut blocks, piece, "minecraft:soul_sand", 3, y, 3, width - 4, y, depth - 4);
            box_fill(&mut blocks, piece, "minecraft:nether_wart[age=0]", 3, y + 1, 3, width - 4, y + 1, depth - 4);
        }
        Kind::Balcony => box_fill(&mut blocks, piece, FENCE, 1, 3, depth - 1, width - 2, 3, depth - 1),
        Kind::EndCap => {
            // Its construction seed has already been consumed during tree growth;
            // use it only to make the terminal masonry jagged without touching
            // the shared structure stream again.
            let mut state = piece.end_seed.unwrap_or_default() as u32;
            for x in 0..width {
                for y in 0..height.min(6) {
                    state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    let z = (state % depth as u32) as i32;
                    box_fill(&mut blocks, piece, BRICK, x, y, 0, x, y, z);
                }
            }
        }
        _ => {}
    }
    let mut loot = Vec::new();
    if piece.chest {
        let x = if matches!(piece.kind, Kind::RightElbow) { 1 } else { width - 2 };
        let pos = local_pos(piece, x, 2, 3.min(depth - 2));
        write(&mut blocks, piece, "minecraft:chest[facing=north,type=single,waterlogged=false]", x, 2, 3.min(depth - 2));
        loot.push(CodedLoot { pos, table: "minecraft:chests/nether_bridge".to_string(), seed: random.next_long() });
    }
    (blocks, loot)
}

fn finish<R: RandomSource>(tree: Tree, random: &mut R) -> Vec<StructurePiece> {
    tree.pieces.into_iter().map(|piece| {
        let (blocks, loot) = emit_blocks(&piece, random);
        StructurePiece {
        id: piece.kind.id().to_string(),
        bounding_box: piece.box_,
        orientation: Some(piece.facing.data_2d()),
        gen_depth: piece.depth,
        template: None,
        placement: None,
        extra_placements: Vec::new(),
        blocks: Some(Arc::new(blocks)),
        loot,
        beard: None,
        refine: None,
    }}).collect()
}

fn build_tree<R: RandomSource>(cx: i32, cz: i32, random: &mut R) -> (Tree, [i32; 3]) {
    let facing = Facing::random(random);
    let start = BoundingBox {
        min: [cx * 16 + 2, START_Y, cz * 16 + 2],
        max: [cx * 16 + 20, START_Y + 9, cz * 16 + 20],
    };
    let mut tree = Tree { pieces: Vec::new(), pending: Vec::new(), bridge: bridge_weights(), castle: castle_weights(), previous: None, start };
    let root = tree.add(Node { kind: Kind::Start, box_: start, facing, depth: 0, chest: false, end_seed: None });
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
    (finish(tree, random), origin)
}

/// Places one fortress start into its current receiving chunk, including
/// supports that need the post-carve grid to locate their solid boundary.
pub fn place_for_chunk<R: RandomSource>(
    start_cx: i32,
    start_cz: i32,
    placing_cx: i32,
    placing_cz: i32,
    world: &mut DenseBlockGrid,
    random: &mut R,
) {
    let (tree, _) = build_tree(start_cx, start_cz, random);
    for piece in tree.pieces {
        let (blocks, _) = emit_blocks(&piece, random);
        for block in blocks {
            world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
        }
        place_supports_in_chunk(&piece, placing_cx, placing_cz, world);
    }
}

#[cfg(test)]
mod tests {
    use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
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
        place_for_chunk(0, 0, 0, 0, &mut world, &mut random);
        for [x, y, z] in expected {
            assert_eq!(world.get(x, y, z), "minecraft:nether_bricks", "support at ({x},{y},{z})");
        }
        assert_eq!(world.get(9, 68, 2), "minecraft:netherrack", "solid terrain is the negative control");
    }
}
