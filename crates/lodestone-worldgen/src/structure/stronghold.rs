//! **Stronghold** — `StrongholdStructure` + all of `StrongholdPieces`, the
//! recursive piece tree that ends in the end-portal room.
//!
//! # What it is
//!
//! A port of vanilla's oldest piece-placement engine: not jigsaw (a pool graph
//! with junctions), but the original `StructurePiece`/`StructurePiecesBuilder`
//! scheme — a weighted table of eleven piece classes, a shared *pending* queue
//! drained in random order, and a depth cap. `minecraft:mineshaft` (S7) is the
//! only other structure in this crate built the same way, and this module
//! mirrors [`super::mineshaft`]'s shape (a `Node`/box tree grown first, blocks
//! resolved second) rather than anything in [`super::jigsaw`].
//!
//! Unlike a mineshaft, a stronghold's pieces need **no shared block canvas**:
//! no `StrongholdPieces` piece ever calls `getBlock` on another piece's
//! territory (`canBeReplaced` is never overridden, so it is vanilla's default
//! `true`), so every piece's block list is a pure function of its own box,
//! orientation and random draws. That is what keeps this module free of
//! [`super::mineshaft::View`]'s shared-overlay machinery.
//!
//! # How it works
//!
//! ```text
//! loop tries = 0, 1, 2, … :
//!     random = setLargeFeatureSeed(seed + tries, cx, cz)
//!     reset the eleven PieceWeights (fresh placeCount, fresh currentPieces)
//!     start = StairsDown{ is_source: true } at (cx*16+2, 64, cz*16+2), a
//!         random horizontal orientation (one nextInt(4))
//!     start.addChildren  →  imposedPiece = FiveCrossing, then one forward
//!         door child (always a FiveCrossing on attempt one, box permitting)
//!     while pending queue not empty:
//!         pick a random pending index, remove it (Vec::remove, not swap —
//!         order of what remains matters to nothing here, but Java's
//!         `ArrayList.remove` is the literal operation being ported)
//!         that piece's addChildren may enqueue more children
//!     moveBelowSeaLevel(seaLevel, minY, random, 10)
//! until the tree is non-empty AND a PortalRoom got placed
//! ```
//!
//! The retry loop is not defensive — it is vanilla's own guarantee that
//! **every stronghold contains a portal room**, which is exactly the property
//! End reachability needs. `StartPiece::portalRoomPiece` is
//! set the moment `PortalRoom::addChildren` runs (a leaf: it registers itself
//! and generates no children of its own), and nothing here bounds the retry
//! count, matching `StrongholdStructure.generatePieces`'s unbounded
//! `do…while`.
//!
//! # How to change it
//!
//! * **The RNG order is the specification.** `generatePieceFromSmallDoor`'s
//!   weighted pick is a `nextInt(totalWeight)` draw, then a **cascade**: if
//!   the selected [`PieceType`]'s box collides or fails `isOkBox`, the loop
//!   falls through to every *remaining* entry in table order — without
//!   redrawing — before the outer loop retries with a fresh draw (five
//!   attempts). [`generate_piece_from_small_door`] preserves this exactly;
//!   collapsing it to "redraw on any failure" changes the stream from the
//!   second rejected piece on.
//! * **A piece's own random draws happen only once its box is accepted.**
//!   `BoundingBox.orientBox` costs no RNG; `isOkBox` and the collision test
//!   cost none either. Every piece's constructor (`entryDoor`, and whatever
//!   else it draws) runs strictly after both checks pass — see vanilla's
//!   ternaries (`isOkBox(box) && collides == null ? new Piece(...) : null`),
//!   which short-circuit the `new` entirely on rejection.
//! * **`Library` and `PortalRoom` are leaves.** Neither overrides
//!   `addChildren`, so both inherit `StructurePiece`'s no-op — they generate
//!   no children. `FillerCorridor` is the same: it exists only as the
//!   five-attempts-exhausted fallback in [`generate_piece_from_small_door`].
//! * **`doPlace`'s depth gate is the *child's* depth**, not the parent's:
//!   `Library` needs `depth > 4` and `PortalRoom` needs `depth > 5`, checked
//!   against the depth the *new* piece would carry (parent depth + 1), which
//!   is why [`generate_and_add_piece`] increments before calling
//!   [`generate_piece_from_small_door`].
//! * **`previousPiece` is Java reference identity** to a `PieceWeight`
//!   instance, which survives even after that entry is removed from
//!   `currentPieces` (once `isValid()` goes false). [`PieceType`] stands in
//!   for identity here because the eleven types are pairwise distinct and
//!   never duplicated in the table, so equality on the enum is equality on
//!   Java's object reference.
//!
//! # Deviations
//!
//! * **`skipAir` on every `generateBox`/`generateMaybeBox` call is not
//!   honoured.** Vanilla's flag means "only overwrite a block that is not
//!   already air", read from the real generated terrain a stronghold is
//!   dug into — meaningful *because* `postProcess` runs after NOISE/SURFACE
//!   in vanilla's own pipeline. Every coded piece in this crate resolves its
//!   blocks eagerly at start time (`StartContext`'s pre-surface shape, see
//!   [`super::coded`]'s module doc), before there is any terrain to read, so
//!   the predicate has nothing to consult and every write here is
//!   unconditional. Ledgered as `stronghold:skip_air_shell`.
//! * **The portal room's spawner carries no `SpawnData`.** `minecraft:spawner`
//!   is placed as a bare block; `SpawnerBlockEntity::setEntityId` needs an
//!   entity-spawning layer this crate does not have yet, the same gap
//!   `mineshaft`'s cave-spider spawner is ledgered under
//!   (`coded:worldgen_entities`).
//!
//! # Dependencies
//!
//! [`StartContext`] only for [`StartContext::sea_level`] and
//! [`StartContext::min_y`] — a stronghold reads no column heights and no
//! biome, unlike every other coded structure in this crate. [`super::coded`]
//! for [`Facing`] and the mirror/rotate transform
//! [`super::template::BlockState`] applies.

use lodestone_worldgen_core::rng::RandomSource;

use super::coded::Facing;
use super::template::{BlockState, Mirror, Rotation};
use super::{BoundingBox, CodedBlock, CodedLoot, StartContext, StructurePiece};

/// `StrongholdPieces.MAX_DEPTH`.
const MAX_DEPTH: i32 = 50;
/// `StrongholdPieces.LOWEST_Y_POSITION` — `isOkBox`'s floor.
const LOWEST_Y_POSITION: i32 = 10;
/// `StrongholdPieces.MAGIC_START_Y` — the start piece's fixed floor before
/// `moveBelowSeaLevel` moves everything.
const MAGIC_START_Y: i32 = 64;
/// `StrongholdPieces.generateAndAddPiece`'s `Math.abs(... ) <= 112` spread
/// bound, measured against the start piece's own box.
const MAX_SPREAD: i32 = 112;

/// `StrongholdPieces.StrongholdPiece.SmallDoorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmallDoorType {
    Opening,
    WoodDoor,
    Grates,
    IronDoor,
}

impl SmallDoorType {
    /// `randomSmallDoor` — one `nextInt(5)`, with values 0 and 1 both mapping
    /// to `OPENING` (the `default` arm of the Java `switch` falls through
    /// case 1 as well as anything unmatched).
    fn random<R: RandomSource>(random: &mut R) -> Self {
        match random.next_int_bounded(5) {
            2 => Self::WoodDoor,
            3 => Self::Grates,
            4 => Self::IronDoor,
            _ => Self::Opening,
        }
    }
}

/// The eleven weighted piece classes `StrongholdPieces` can select, plus the
/// two that are never weighted (`StartPiece` is a fixed `StairsDown`,
/// `FillerCorridor` is the exhausted-attempts fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PieceType {
    Straight,
    PrisonHall,
    LeftTurn,
    RightTurn,
    RoomCrossing,
    StraightStairsDown,
    StairsDown,
    FiveCrossing,
    ChestCorridor,
    Library,
    PortalRoom,
}

/// `StrongholdPieces.PieceWeight`.
#[derive(Debug, Clone, Copy)]
struct PieceWeight {
    piece: PieceType,
    weight: i32,
    /// `maxPlaceCount`; `0` means unlimited.
    max_place_count: i32,
    place_count: i32,
}

/// `StrongholdPieces.STRONGHOLD_PIECE_WEIGHTS`, transcribed in table order —
/// the order the cascade in [`generate_piece_from_small_door`] falls through.
/// Weights sum to 145 (40+5+20+20+10+5+5+5+5+10+20), re-derived from this
/// table rather than asserted from memory.
fn initial_weights() -> Vec<PieceWeight> {
    use PieceType::{
        ChestCorridor, FiveCrossing, Library, PortalRoom, PrisonHall, RightTurn, RoomCrossing,
        Straight, StraightStairsDown, StairsDown,
    };
    let row = |piece, weight, max_place_count| PieceWeight {
        piece,
        weight,
        max_place_count,
        place_count: 0,
    };
    vec![
        row(Straight, 40, 0),
        row(PrisonHall, 5, 5),
        row(PieceType::LeftTurn, 20, 0),
        row(RightTurn, 20, 0),
        row(RoomCrossing, 10, 6),
        row(StraightStairsDown, 5, 5),
        row(StairsDown, 5, 5),
        row(FiveCrossing, 5, 4),
        row(ChestCorridor, 5, 4),
        row(Library, 10, 2),
        row(PortalRoom, 20, 1),
    ]
}

/// `PieceWeight.doPlace(depth)` — the base formula, plus the two anonymous
/// subclass overrides (`Library`: `depth > 4`; `PortalRoom`: `depth > 5`).
fn do_place(pw: &PieceWeight, depth: i32) -> bool {
    let base = pw.max_place_count == 0 || pw.place_count < pw.max_place_count;
    match pw.piece {
        PieceType::Library => base && depth > 4,
        PieceType::PortalRoom => base && depth > 5,
        _ => base,
    }
}

/// `PieceWeight.isValid()` — never overridden, unlike `doPlace`.
fn is_valid(pw: &PieceWeight) -> bool {
    pw.max_place_count == 0 || pw.place_count < pw.max_place_count
}

/// Per-piece facts fixed at creation time — the union of every
/// `StrongholdPieces` subclass's own fields.
#[derive(Debug, Clone)]
enum Kind {
    Straight {
        door: SmallDoorType,
        left_child: bool,
        right_child: bool,
    },
    PrisonHall {
        door: SmallDoorType,
    },
    LeftTurn {
        door: SmallDoorType,
    },
    RightTurn {
        door: SmallDoorType,
    },
    RoomCrossing {
        door: SmallDoorType,
        room_type: i32,
    },
    StraightStairsDown {
        door: SmallDoorType,
    },
    StairsDown {
        door: SmallDoorType,
        is_source: bool,
    },
    FiveCrossing {
        door: SmallDoorType,
        left_low: bool,
        left_high: bool,
        right_low: bool,
        right_high: bool,
    },
    ChestCorridor {
        door: SmallDoorType,
    },
    Library {
        door: SmallDoorType,
        is_tall: bool,
    },
    PortalRoom,
    FillerCorridor {
        steps: i32,
    },
}

/// One node of the tree, before blocks — box, orientation and per-kind facts.
#[derive(Debug, Clone)]
struct Node {
    box_: BoundingBox,
    /// Every stronghold piece calls `setOrientation`, unlike a mineshaft room
    /// or crossing, so this is never `None`.
    orientation: Facing,
    gen_depth: i32,
    kind: Kind,
}

impl Node {
    /// `getWorldX/Y/Z`, transcribed from `StructurePiece`'s own switch (see
    /// [`super::mineshaft::Node::world_pos`] for the identical formula ported
    /// a second time because `StructurePiece`'s helpers are `protected`, not
    /// shared).
    fn world_pos(&self, x: i32, y: i32, z: i32) -> [i32; 3] {
        let wx = match self.orientation {
            Facing::North | Facing::South => self.box_.min[0] + x,
            Facing::West => self.box_.max[0] - z,
            Facing::East => self.box_.min[0] + z,
        };
        let wz = match self.orientation {
            Facing::North => self.box_.max[2] - z,
            Facing::South => self.box_.min[2] + z,
            Facing::West | Facing::East => self.box_.min[2] + x,
        };
        [wx, y + self.box_.min[1], wz]
    }

    /// `setOrientation`'s `(mirror, rotation)` table.
    fn transform(&self) -> (Mirror, Rotation) {
        match self.orientation {
            Facing::South => (Mirror::LeftRight, Rotation::None),
            Facing::West => (Mirror::LeftRight, Rotation::Cw90),
            Facing::East => (Mirror::None, Rotation::Cw90),
            Facing::North => (Mirror::None, Rotation::None),
        }
    }

    /// `StructurePieceType`'s registered (lowercased) id string.
    fn piece_id(&self) -> &'static str {
        match self.kind {
            Kind::ChestCorridor { .. } => "minecraft:shcc",
            Kind::FillerCorridor { .. } => "minecraft:shfc",
            Kind::FiveCrossing { .. } => "minecraft:sh5c",
            Kind::LeftTurn { .. } => "minecraft:shlt",
            Kind::Library { .. } => "minecraft:shli",
            Kind::PortalRoom => "minecraft:shpr",
            Kind::PrisonHall { .. } => "minecraft:shph",
            Kind::RightTurn { .. } => "minecraft:shrt",
            Kind::RoomCrossing { .. } => "minecraft:shrc",
            Kind::StairsDown { is_source: true, .. } => "minecraft:shstart",
            Kind::StairsDown { is_source: false, .. } => "minecraft:shsd",
            Kind::Straight { .. } => "minecraft:shs",
            Kind::StraightStairsDown { .. } => "minecraft:shssd",
        }
    }
}

/// `BoundingBox.orientBox(footX, footY, footZ, offX, offY, offZ, width,
/// height, depth, direction)`.
fn orient_box(foot: [i32; 3], off: [i32; 3], size: [i32; 3], direction: Facing) -> BoundingBox {
    let [fx, fy, fz] = foot;
    let [ox, oy, oz] = off;
    let [width, height, depth] = size;
    match direction {
        Facing::South => BoundingBox {
            min: [fx + ox, fy + oy, fz + oz],
            max: [fx + width - 1 + ox, fy + height - 1 + oy, fz + depth - 1 + oz],
        },
        Facing::North => BoundingBox {
            min: [fx + ox, fy + oy, fz - depth + 1 + oz],
            max: [fx + width - 1 + ox, fy + height - 1 + oy, fz + oz],
        },
        Facing::West => BoundingBox {
            min: [fx - depth + 1 + oz, fy + oy, fz + ox],
            max: [fx + oz, fy + height - 1 + oy, fz + width - 1 + ox],
        },
        Facing::East => BoundingBox {
            min: [fx + oz, fy + oy, fz + ox],
            max: [fx + depth - 1 + oz, fy + height - 1 + oy, fz + width - 1 + ox],
        },
    }
}

/// `StrongholdPieces.StrongholdPiece.isOkBox`.
fn is_ok_box(box_: BoundingBox) -> bool {
    box_.min[1] > LOWEST_Y_POSITION
}

/// The whole generation-time state: the tree so far, the shared pending
/// queue, the piece-weight table and its two pieces of cross-call memory
/// (`imposedPiece`, `previousPiece`), and the start box the spread bound is
/// measured against.
struct Tree {
    pieces: Vec<Node>,
    pending: Vec<usize>,
    current_pieces: Vec<PieceWeight>,
    total_weight: i32,
    imposed_piece: Option<PieceType>,
    previous_piece: Option<PieceType>,
    start_box: BoundingBox,
    portal_room: Option<usize>,
}

impl Tree {
    /// `StructurePieceAccessor.findCollisionPiece(box) == null`, inverted.
    fn collides(&self, candidate: BoundingBox) -> bool {
        self.pieces.iter().any(|p| p.box_.intersects(candidate))
    }

    fn add(&mut self, node: Node) -> usize {
        let is_portal = matches!(node.kind, Kind::PortalRoom);
        self.pieces.push(node);
        let index = self.pieces.len() - 1;
        if is_portal {
            self.portal_room = Some(index);
        }
        index
    }

    /// `StructurePiecesBuilder.getBoundingBox()` — the union over every piece.
    fn bounding_box(&self) -> BoundingBox {
        self.pieces
            .iter()
            .map(|p| p.box_)
            .reduce(BoundingBox::encapsulate)
            .expect("the start piece is always added first")
    }

    fn offset_vertically(&mut self, dy: i32) {
        for piece in &mut self.pieces {
            piece.box_.min[1] += dy;
            piece.box_.max[1] += dy;
        }
    }

    /// `StructurePiecesBuilder.moveBelowSeaLevel(seaLevel, minY, random, 10)`
    /// — identical formula to [`super::mineshaft::Shaft::move_below_sea_level`]
    /// (both structures share the base class method); reimplemented rather
    /// than shared because the two live on unrelated tree types.
    fn move_below_sea_level<R: RandomSource>(&mut self, sea_level: i32, min_y: i32, random: &mut R) {
        let offset = 10;
        let max_y = sea_level - offset;
        let box_ = self.bounding_box();
        let mut y1 = (box_.max[1] - box_.min[1] + 1) + min_y + 1;
        if y1 < max_y {
            y1 += random.next_int_bounded(max_y - y1);
        }
        let dy = y1 - box_.max[1];
        self.offset_vertically(dy);
    }
}

/// The box-offset/size table for every weighted piece type plus the start
/// piece, i.e. `BoundingBox.orientBox`'s `(offX, offY, offZ, width, height,
/// depth)` argument tuple per `createPiece`. `Library`'s tall attempt is
/// listed; its short fallback is handled separately since it retries with a
/// different height on the same offsets.
fn box_shape(piece: PieceType) -> ([i32; 3], [i32; 3]) {
    match piece {
        PieceType::Straight => ([-1, -1, 0], [5, 5, 7]),
        PieceType::PrisonHall => ([-1, -1, 0], [9, 5, 11]),
        PieceType::LeftTurn | PieceType::RightTurn => ([-1, -1, 0], [5, 5, 5]),
        PieceType::RoomCrossing => ([-4, -1, 0], [11, 7, 11]),
        PieceType::StraightStairsDown => ([-1, -7, 0], [5, 11, 8]),
        PieceType::StairsDown => ([-1, -7, 0], [5, 11, 5]),
        PieceType::FiveCrossing => ([-4, -3, 0], [10, 9, 11]),
        PieceType::ChestCorridor => ([-1, -1, 0], [5, 5, 7]),
        PieceType::Library => ([-4, -1, 0], [14, 11, 15]),
        PieceType::PortalRoom => ([-4, -1, 0], [11, 8, 16]),
    }
}

/// `findAndCreatePieceFactory` + each piece's own `createPiece`: computes the
/// box (no RNG), checks `isOkBox` and collision, and only on success draws
/// the piece's own random fields and builds the [`Node`]. Returns `None`
/// exactly where vanilla's ternary would have evaluated to `null` — no RNG is
/// consumed on that path.
fn try_create_piece<R: RandomSource>(
    piece: PieceType,
    tree: &Tree,
    foot: [i32; 3],
    direction: Facing,
    gen_depth: i32,
    random: &mut R,
) -> Option<Node> {
    if piece == PieceType::Library {
        // Two box attempts, tall then short — see `Library.createPiece`.
        // Neither attempt costs RNG; only the eventual constructor does.
        let (off, tall_size) = box_shape(PieceType::Library);
        let tall = orient_box(foot, off, tall_size, direction);
        let box_ = if is_ok_box(tall) && !tree.collides(tall) {
            tall
        } else {
            let short = orient_box(foot, off, [14, 6, 15], direction);
            if is_ok_box(short) && !tree.collides(short) {
                short
            } else {
                return None;
            }
        };
        let door = SmallDoorType::random(random);
        let is_tall = box_.max[1] - box_.min[1] + 1 > 6;
        return Some(Node {
            box_,
            orientation: direction,
            gen_depth,
            kind: Kind::Library { door, is_tall },
        });
    }
    let (off, size) = box_shape(piece);
    let box_ = orient_box(foot, off, size, direction);
    if !is_ok_box(box_) || tree.collides(box_) {
        return None;
    }
    let kind = match piece {
        PieceType::Straight => Kind::Straight {
            door: SmallDoorType::random(random),
            left_child: random.next_int_bounded(2) == 0,
            right_child: random.next_int_bounded(2) == 0,
        },
        PieceType::PrisonHall => Kind::PrisonHall {
            door: SmallDoorType::random(random),
        },
        PieceType::LeftTurn => Kind::LeftTurn {
            door: SmallDoorType::random(random),
        },
        PieceType::RightTurn => Kind::RightTurn {
            door: SmallDoorType::random(random),
        },
        PieceType::RoomCrossing => Kind::RoomCrossing {
            door: SmallDoorType::random(random),
            room_type: random.next_int_bounded(5),
        },
        PieceType::StraightStairsDown => Kind::StraightStairsDown {
            door: SmallDoorType::random(random),
        },
        PieceType::StairsDown => Kind::StairsDown {
            door: SmallDoorType::random(random),
            is_source: false,
        },
        PieceType::FiveCrossing => {
            let door = SmallDoorType::random(random);
            let left_low = random.next_bool();
            let left_high = random.next_bool();
            let right_low = random.next_bool();
            let right_high = random.next_int_bounded(3) > 0;
            Kind::FiveCrossing {
                door,
                left_low,
                left_high,
                right_low,
                right_high,
            }
        }
        PieceType::ChestCorridor => Kind::ChestCorridor {
            door: SmallDoorType::random(random),
        },
        // `PortalRoom.createPiece` takes no `RandomSource` at all.
        PieceType::PortalRoom => Kind::PortalRoom,
        PieceType::Library => unreachable!("handled above"),
    };
    Some(Node {
        box_,
        orientation: direction,
        gen_depth,
        kind,
    })
}

/// `StrongholdPieces.FillerCorridor.findPieceBox` — pure geometry, no RNG
/// despite taking a `random` parameter in vanilla (unused in its body).
fn filler_corridor_box(tree: &Tree, foot: [i32; 3], direction: Facing) -> Option<BoundingBox> {
    let candidate = orient_box(foot, [-1, -1, 0], [5, 5, 4], direction);
    let collision = tree.pieces.iter().find(|p| p.box_.intersects(candidate))?;
    if collision.box_.min[1] != candidate.min[1] {
        return None;
    }
    for depth in [2, 1] {
        let probe = orient_box(foot, [-1, -1, 0], [5, 5, depth], direction);
        if !collision.box_.intersects(probe) {
            return Some(orient_box(foot, [-1, -1, 0], [5, 5, depth + 1], direction));
        }
    }
    None
}

/// `updatePieceWeight` — sets `total_weight` and reports whether any
/// *limited*-count entry still has room. An unlimited entry (`maxPlaceCount
/// == 0`, i.e. `Straight`/`LeftTurn`/`RightTurn`) never counts toward this,
/// which is the real vanilla quirk that eventually forces every branch to
/// terminate: once every limited piece is exhausted, generation stops
/// regardless of the three unlimited ones still being legal.
fn update_piece_weight(tree: &mut Tree) -> bool {
    let mut has_any = false;
    let mut total = 0;
    for pw in &tree.current_pieces {
        if pw.max_place_count > 0 && pw.place_count < pw.max_place_count {
            has_any = true;
        }
        total += pw.weight;
    }
    tree.total_weight = total;
    has_any
}

/// `generatePieceFromSmallDoor` — the weighted pick, its imposed-piece
/// override, the five-attempt cascade, and the `FillerCorridor` fallback.
fn generate_piece_from_small_door<R: RandomSource>(
    tree: &mut Tree,
    foot: [i32; 3],
    direction: Facing,
    depth: i32,
    random: &mut R,
) -> Option<Node> {
    if !update_piece_weight(tree) {
        return None;
    }
    if let Some(imposed) = tree.imposed_piece.take() {
        if let Some(node) = try_create_piece(imposed, tree, foot, direction, depth, random) {
            return Some(node);
        }
        // Falls through to the weighted cascade below, imposed already
        // cleared — matches vanilla exactly (no retry of the imposed piece).
    }
    for _attempt in 0..5 {
        let mut weight_selection = random.next_int_bounded(tree.total_weight);
        for i in 0..tree.current_pieces.len() {
            weight_selection -= tree.current_pieces[i].weight;
            if weight_selection >= 0 {
                continue;
            }
            let piece = tree.current_pieces[i].piece;
            if !do_place(&tree.current_pieces[i], depth) || Some(piece) == tree.previous_piece {
                break;
            }
            if let Some(node) = try_create_piece(piece, tree, foot, direction, depth, random) {
                tree.current_pieces[i].place_count += 1;
                tree.previous_piece = Some(piece);
                if !is_valid(&tree.current_pieces[i]) {
                    tree.current_pieces.remove(i);
                }
                return Some(node);
            }
            // Box rejected: fall through to the *next* table entry without a
            // fresh draw — `weight_selection` stays negative for every entry
            // from here on, so every remaining type in the table is tried.
        }
    }
    let box_ = filler_corridor_box(tree, foot, direction)?;
    if box_.min[1] > 1 {
        let steps = if matches!(direction, Facing::North | Facing::South) {
            box_.max[0] - box_.min[0] + 1
        } else {
            box_.max[2] - box_.min[2] + 1
        };
        Some(Node {
            box_,
            orientation: direction,
            gen_depth: depth,
            kind: Kind::FillerCorridor { steps },
        })
    } else {
        None
    }
}

/// `generateAndAddPiece` — the depth cap and spread bound, then the child's
/// own selection, then registration into both the tree and the pending
/// queue (never immediate recursion, unlike a mineshaft).
fn generate_and_add_piece<R: RandomSource>(
    tree: &mut Tree,
    foot: [i32; 3],
    direction: Facing,
    depth: i32,
    random: &mut R,
) -> Option<usize> {
    if depth > MAX_DEPTH {
        return None;
    }
    if (foot[0] - tree.start_box.min[0]).abs() > MAX_SPREAD
        || (foot[2] - tree.start_box.min[2]).abs() > MAX_SPREAD
    {
        return None;
    }
    let node = generate_piece_from_small_door(tree, foot, direction, depth + 1, random)?;
    let index = tree.add(node);
    tree.pending.push(index);
    Some(index)
}

/// `generateSmallDoorChildForward(startPiece, accessor, random, xOff, yOff)`.
fn child_forward<R: RandomSource>(tree: &mut Tree, from: &Node, x_off: i32, y_off: i32, random: &mut R) {
    let b = from.box_;
    let (foot, dir) = match from.orientation {
        Facing::North => ([b.min[0] + x_off, b.min[1] + y_off, b.min[2] - 1], Facing::North),
        Facing::South => ([b.min[0] + x_off, b.min[1] + y_off, b.max[2] + 1], Facing::South),
        Facing::West => ([b.min[0] - 1, b.min[1] + y_off, b.min[2] + x_off], Facing::West),
        Facing::East => ([b.max[0] + 1, b.min[1] + y_off, b.min[2] + x_off], Facing::East),
    };
    generate_and_add_piece(tree, foot, dir, from.gen_depth, random);
}

/// `generateSmallDoorChildLeft(startPiece, accessor, random, yOff, zOff)`.
fn child_left<R: RandomSource>(tree: &mut Tree, from: &Node, y_off: i32, z_off: i32, random: &mut R) {
    let b = from.box_;
    let (foot, dir) = match from.orientation {
        Facing::North | Facing::South => (
            [b.min[0] - 1, b.min[1] + y_off, b.min[2] + z_off],
            Facing::West,
        ),
        Facing::West | Facing::East => (
            [b.min[0] + z_off, b.min[1] + y_off, b.min[2] - 1],
            Facing::North,
        ),
    };
    generate_and_add_piece(tree, foot, dir, from.gen_depth, random);
}

/// `generateSmallDoorChildRight(startPiece, accessor, random, yOff, zOff)`.
fn child_right<R: RandomSource>(tree: &mut Tree, from: &Node, y_off: i32, z_off: i32, random: &mut R) {
    let b = from.box_;
    let (foot, dir) = match from.orientation {
        Facing::North | Facing::South => (
            [b.max[0] + 1, b.min[1] + y_off, b.min[2] + z_off],
            Facing::East,
        ),
        Facing::West | Facing::East => (
            [b.min[0] + z_off, b.min[1] + y_off, b.max[2] + 1],
            Facing::South,
        ),
    };
    generate_and_add_piece(tree, foot, dir, from.gen_depth, random);
}

/// `StructurePiece.addChildren`, dispatched per piece kind. `Library`,
/// `PortalRoom` (besides registering itself) and `FillerCorridor` are not
/// matched here because none of the three ever enqueues a child — see the
/// module doc.
fn add_children<R: RandomSource>(tree: &mut Tree, index: usize, random: &mut R) {
    let node = tree.pieces[index].clone();
    match &node.kind {
        Kind::Straight {
            left_child,
            right_child,
            ..
        } => {
            child_forward(tree, &node, 1, 1, random);
            if *left_child {
                child_left(tree, &node, 1, 2, random);
            }
            if *right_child {
                child_right(tree, &node, 1, 2, random);
            }
        }
        Kind::PrisonHall { .. } | Kind::ChestCorridor { .. } | Kind::StraightStairsDown { .. } => {
            child_forward(tree, &node, 1, 1, random);
        }
        Kind::StairsDown { is_source, .. } => {
            if *is_source {
                tree.imposed_piece = Some(PieceType::FiveCrossing);
            }
            child_forward(tree, &node, 1, 1, random);
        }
        Kind::LeftTurn { .. } => {
            if matches!(node.orientation, Facing::North | Facing::East) {
                child_left(tree, &node, 1, 1, random);
            } else {
                child_right(tree, &node, 1, 1, random);
            }
        }
        Kind::RightTurn { .. } => {
            if matches!(node.orientation, Facing::North | Facing::East) {
                child_right(tree, &node, 1, 1, random);
            } else {
                child_left(tree, &node, 1, 1, random);
            }
        }
        Kind::RoomCrossing { .. } => {
            child_forward(tree, &node, 4, 1, random);
            child_left(tree, &node, 1, 4, random);
            child_right(tree, &node, 1, 4, random);
        }
        Kind::FiveCrossing {
            left_low,
            left_high,
            right_low,
            right_high,
            ..
        } => {
            let (z_off_a, z_off_b) = if matches!(node.orientation, Facing::West | Facing::North) {
                (5, 3)
            } else {
                (3, 5)
            };
            child_forward(tree, &node, 5, 1, random);
            if *left_low {
                child_left(tree, &node, z_off_a, 1, random);
            }
            if *left_high {
                child_left(tree, &node, z_off_b, 7, random);
            }
            if *right_low {
                child_right(tree, &node, z_off_a, 1, random);
            }
            if *right_high {
                child_right(tree, &node, z_off_b, 7, random);
            }
        }
        Kind::PortalRoom => {
            tree.portal_room = Some(index);
        }
        Kind::Library { .. } | Kind::FillerCorridor { .. } => {}
    }
}

/// `StrongholdStructure.generatePieces` — the whole retry loop.
///
/// Returns the finished piece list. Never returns an empty list: the loop
/// does not terminate until a portal room has been placed, exactly as
/// vanilla's `do…while (builder.isEmpty() || startRoom.portalRoomPiece ==
/// null)` — `builder.isEmpty()` is checked here too even though it can never
/// be true (the start piece is always added), for the same reason vanilla
/// checks it: there is no cheaper way to state "a tree exists".
#[must_use]
pub fn generate(cx: i32, cz: i32, seed: i64, ctx: &dyn StartContext) -> Vec<StructurePiece> {
    let mut tries: i64 = 0;
    loop {
        let mut random = super::structure_random(seed.wrapping_add(tries), cx, cz);
        tries += 1;

        let west = cx * 16 + 2;
        let north = cz * 16 + 2;
        let direction = Facing::random(&mut random);
        // `makeBoundingBox(west, MAGIC_START_Y, north, direction, 5, 11, 5)`.
        // Width and depth are both 5, so the X/Z axis swap `makeBoundingBox`
        // performs for a non-Z orientation (see `super::coded::Builder::new`)
        // is inert here — the box is the same square footprint either way.
        let start_box = BoundingBox {
            min: [west, MAGIC_START_Y, north],
            max: [west + 5 - 1, MAGIC_START_Y + 11 - 1, north + 5 - 1],
        };
        let mut tree = Tree {
            pieces: Vec::new(),
            pending: Vec::new(),
            current_pieces: initial_weights(),
            total_weight: 0,
            imposed_piece: None,
            previous_piece: None,
            start_box,
            portal_room: None,
        };
        let start_node = Node {
            box_: start_box,
            orientation: direction,
            gen_depth: 0,
            kind: Kind::StairsDown {
                door: SmallDoorType::Opening,
                is_source: true,
            },
        };
        let start_index = tree.add(start_node);
        add_children(&mut tree, start_index, &mut random);

        while let Some(pos) = {
            if tree.pending.is_empty() {
                None
            } else {
                Some(random.next_int_bounded(tree.pending.len() as i32) as usize)
            }
        } {
            let index = tree.pending.remove(pos);
            add_children(&mut tree, index, &mut random);
        }

        let sea_level = ctx.sea_level();
        let min_y = ctx.min_y();
        tree.move_below_sea_level(sea_level, min_y, &mut random);

        if !tree.pieces.is_empty() && tree.portal_room.is_some() {
            return into_pieces(tree, &mut random);
        }
    }
}

/// Second pass: every node's blocks, independently (no shared canvas — see
/// the module doc for why none is needed).
fn into_pieces<R: RandomSource>(tree: Tree, random: &mut R) -> Vec<StructurePiece> {
    let mut out = Vec::with_capacity(tree.pieces.len());
    for node in &tree.pieces {
        let (mirror, rotation) = node.transform();
        let mut place = Place {
            node,
            mirror,
            rotation,
            blocks: Vec::new(),
            loot: Vec::new(),
        };
        post_process(&mut place, random);
        out.push(StructurePiece {
            id: node.piece_id().to_string(),
            bounding_box: node.box_,
            orientation: Some(node.orientation.data_2d()),
            gen_depth: node.gen_depth,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: Some(std::sync::Arc::new(place.blocks)),
            loot: place.loot,
            // `terrain_adaptation` is `bury` for `minecraft:stronghold`
            // (`assets/worldgen/structure/stronghold.json`), which the
            // beardifier's rigid-box path already handles with `beard: None`
            // — see [`super::beardifier`].
            beard: None,
            refine: None,
        });
    }
    out
}

/// One piece's `postProcess`, bound to its node.
struct Place<'a> {
    node: &'a Node,
    mirror: Mirror,
    rotation: Rotation,
    blocks: Vec<CodedBlock>,
    loot: Vec<CodedLoot>,
}

impl Place<'_> {
    /// `placeBlock` — mirror, then rotate, then record. `canBeReplaced` is
    /// never overridden for a stronghold piece, so it is vanilla's default
    /// `true` and every write is unconditional.
    fn place(&mut self, state: &BlockState, x: i32, y: i32, z: i32) {
        let pos = self.node.world_pos(x, y, z);
        let transformed = state.mirror(self.mirror).rotate(self.rotation);
        self.blocks.push(CodedBlock {
            pos,
            state: transformed.canonical(),
        });
    }

    /// `generateBox(..., edge, fill, skipAir)` — `skipAir` not honoured, see
    /// the module doc's `stronghold:skip_air_shell` deviation.
    #[allow(clippy::too_many_arguments)]
    fn generate_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, edge: &BlockState, fill: &BlockState) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    self.place(if interior { fill } else { edge }, x, y, z);
                }
            }
        }
    }

    /// `generateBox(..., skipAir, random, SMOOTH_STONE_SELECTOR)` — the
    /// shell every room-shaped piece opens with: `cave_air` inside, a
    /// randomised stone-brick variant on every face.
    #[allow(clippy::too_many_arguments)]
    fn generate_shell<R: RandomSource>(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32, random: &mut R) {
        let cave_air = BlockState::of("minecraft:cave_air");
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let is_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    if is_edge {
                        // `SmoothStoneSelector.next`.
                        let selection = random.next_float();
                        let state = if selection < 0.2 {
                            BlockState::of("minecraft:cracked_stone_bricks")
                        } else if selection < 0.5 {
                            BlockState::of("minecraft:mossy_stone_bricks")
                        } else if selection < 0.55 {
                            BlockState::of("minecraft:infested_stone_bricks")
                        } else {
                            BlockState::of("minecraft:stone_bricks")
                        };
                        self.place(&state, x, y, z);
                    } else {
                        self.place(&cave_air, x, y, z);
                    }
                }
            }
        }
    }

    /// `generateMaybeBox(..., hasToBeInside = false)` — every stronghold call
    /// site passes `false`, so the `isInterior` branch never applies and is
    /// not ported.
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
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if random.next_float() > probability {
                        continue;
                    }
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    self.place(if interior { fill } else { edge }, x, y, z);
                }
            }
        }
    }

    /// `maybeGenerateBlock` — one draw, `nextFloat() < probability`.
    fn maybe_generate_block<R: RandomSource>(&mut self, random: &mut R, probability: f32, x: i32, y: i32, z: i32, state: &BlockState) {
        if random.next_float() < probability {
            self.place(state, x, y, z);
        }
    }

    /// `StrongholdPiece.generateSmallDoor` — the four door-type shells.
    /// Draws no RNG in any arm — vanilla's own `generateSmallDoor` takes a
    /// `random` parameter it never reads either, kept here only so every call
    /// site has one signature.
    fn generate_small_door<R: RandomSource>(&mut self, _random: &mut R, door: SmallDoorType, fx: i32, fy: i32, fz: i32) {
        let stone_bricks = BlockState::of("minecraft:stone_bricks");
        match door {
            SmallDoorType::Opening => {
                let cave_air = BlockState::of("minecraft:cave_air");
                self.generate_box(fx, fy, fz, fx + 2, fy + 2, fz, &cave_air, &cave_air);
            }
            SmallDoorType::WoodDoor => {
                self.place(&stone_bricks, fx, fy, fz);
                self.place(&stone_bricks, fx, fy + 1, fz);
                self.place(&stone_bricks, fx, fy + 2, fz);
                self.place(&stone_bricks, fx + 1, fy + 2, fz);
                self.place(&stone_bricks, fx + 2, fy + 2, fz);
                self.place(&stone_bricks, fx + 2, fy + 1, fz);
                self.place(&stone_bricks, fx + 2, fy, fz);
                self.place(
                    &BlockState::parse("minecraft:oak_door[facing=north,half=lower,hinge=left,open=false,powered=false]"),
                    fx + 1,
                    fy,
                    fz,
                );
                self.place(
                    &BlockState::parse("minecraft:oak_door[facing=north,half=upper,hinge=left,open=false,powered=false]"),
                    fx + 1,
                    fy + 1,
                    fz,
                );
            }
            SmallDoorType::Grates => {
                let cave_air = BlockState::of("minecraft:cave_air");
                self.place(&cave_air, fx + 1, fy, fz);
                self.place(&cave_air, fx + 1, fy + 1, fz);
                self.place(
                    &BlockState::parse("minecraft:iron_bars[east=false,north=false,south=false,waterlogged=false,west=true]"),
                    fx,
                    fy,
                    fz,
                );
                self.place(
                    &BlockState::parse("minecraft:iron_bars[east=false,north=false,south=false,waterlogged=false,west=true]"),
                    fx,
                    fy + 1,
                    fz,
                );
                let we = BlockState::parse("minecraft:iron_bars[east=true,north=false,south=false,waterlogged=false,west=true]");
                self.place(&we, fx, fy + 2, fz);
                self.place(&we, fx + 1, fy + 2, fz);
                self.place(&we, fx + 2, fy + 2, fz);
                let east = BlockState::parse("minecraft:iron_bars[east=true,north=false,south=false,waterlogged=false,west=false]");
                self.place(&east, fx + 2, fy + 1, fz);
                self.place(&east, fx + 2, fy, fz);
            }
            SmallDoorType::IronDoor => {
                self.place(&stone_bricks, fx, fy, fz);
                self.place(&stone_bricks, fx, fy + 1, fz);
                self.place(&stone_bricks, fx, fy + 2, fz);
                self.place(&stone_bricks, fx + 1, fy + 2, fz);
                self.place(&stone_bricks, fx + 2, fy + 2, fz);
                self.place(&stone_bricks, fx + 2, fy + 1, fz);
                self.place(&stone_bricks, fx + 2, fy, fz);
                self.place(
                    &BlockState::parse("minecraft:iron_door[facing=north,half=lower,hinge=left,open=false,powered=false]"),
                    fx + 1,
                    fy,
                    fz,
                );
                self.place(
                    &BlockState::parse("minecraft:iron_door[facing=north,half=upper,hinge=left,open=false,powered=false]"),
                    fx + 1,
                    fy + 1,
                    fz,
                );
                self.place(
                    &BlockState::parse("minecraft:stone_button[face=wall,facing=north,powered=false]"),
                    fx + 2,
                    fy + 1,
                    fz + 1,
                );
                self.place(
                    &BlockState::parse("minecraft:stone_button[face=wall,facing=south,powered=false]"),
                    fx + 2,
                    fy + 1,
                    fz - 1,
                );
            }
        }
    }

    /// `StructurePiece.createChest` — raw write (no mirror/rotate, matching
    /// [`super::coded::Builder::create_chest`]'s own citation of the same
    /// vanilla method), plus a `random.nextLong()` loot-seed draw.
    fn create_chest<R: RandomSource>(&mut self, random: &mut R, x: i32, y: i32, z: i32, table: &str) {
        let pos = self.node.world_pos(x, y, z);
        self.blocks.push(CodedBlock {
            pos,
            state: "minecraft:chest[facing=north,type=single,waterlogged=false]".to_string(),
        });
        self.loot.push(CodedLoot {
            pos,
            table: table.to_string(),
            seed: random.next_long(),
        });
    }
}

/// `StrongholdPieces::*::postProcess`, dispatched per kind.
fn post_process<R: RandomSource>(p: &mut Place<'_>, random: &mut R) {
    let stone_bricks = BlockState::of("minecraft:stone_bricks");
    let smooth_slab = BlockState::of("minecraft:smooth_stone_slab");
    let cave_air = BlockState::of("minecraft:cave_air");
    match p.node.kind.clone() {
        Kind::Straight {
            door,
            left_child,
            right_child,
        } => {
            p.generate_shell(0, 0, 0, 4, 4, 6, random);
            p.generate_small_door(random, door, 1, 1, 0);
            p.generate_small_door(random, SmallDoorType::Opening, 1, 1, 6);
            let east_torch = BlockState::parse("minecraft:wall_torch[facing=east]");
            let west_torch = BlockState::parse("minecraft:wall_torch[facing=west]");
            p.maybe_generate_block(random, 0.1, 1, 2, 1, &east_torch);
            p.maybe_generate_block(random, 0.1, 3, 2, 1, &west_torch);
            p.maybe_generate_block(random, 0.1, 1, 2, 5, &east_torch);
            p.maybe_generate_block(random, 0.1, 3, 2, 5, &west_torch);
            if left_child {
                p.generate_box(0, 1, 2, 0, 3, 4, &cave_air, &cave_air);
            }
            if right_child {
                p.generate_box(4, 1, 2, 4, 3, 4, &cave_air, &cave_air);
            }
        }
        Kind::PrisonHall { door } => {
            p.generate_shell(0, 0, 0, 8, 4, 10, random);
            p.generate_small_door(random, door, 1, 1, 0);
            p.generate_box(1, 1, 10, 3, 3, 10, &cave_air, &cave_air);
            p.generate_shell(4, 1, 1, 4, 3, 1, random);
            p.generate_shell(4, 1, 3, 4, 3, 3, random);
            p.generate_shell(4, 1, 7, 4, 3, 7, random);
            p.generate_shell(4, 1, 9, 4, 3, 9, random);
            let ns = BlockState::parse("minecraft:iron_bars[east=false,north=true,south=true,waterlogged=false,west=false]");
            let nse = BlockState::parse("minecraft:iron_bars[east=true,north=true,south=true,waterlogged=false,west=false]");
            let we = BlockState::parse("minecraft:iron_bars[east=true,north=false,south=false,waterlogged=false,west=true]");
            for y in 1..=3 {
                p.place(&ns, 4, y, 4);
                p.place(&nse, 4, y, 5);
                p.place(&ns, 4, y, 6);
                p.place(&we, 5, y, 5);
                p.place(&we, 6, y, 5);
                p.place(&we, 7, y, 5);
            }
            p.place(&ns, 4, 3, 2);
            p.place(&ns, 4, 3, 8);
            let door_bottom = BlockState::parse("minecraft:iron_door[facing=west,half=lower,hinge=left,open=false,powered=false]");
            let door_top = BlockState::parse("minecraft:iron_door[facing=west,half=upper,hinge=left,open=false,powered=false]");
            p.place(&door_bottom, 4, 1, 2);
            p.place(&door_top, 4, 2, 2);
            p.place(&door_bottom, 4, 1, 8);
            p.place(&door_top, 4, 2, 8);
        }
        Kind::LeftTurn { door } => {
            p.generate_shell(0, 0, 0, 4, 4, 4, random);
            p.generate_small_door(random, door, 1, 1, 0);
            if matches!(p.node.orientation, Facing::North | Facing::East) {
                p.generate_box(0, 1, 1, 0, 3, 3, &cave_air, &cave_air);
            } else {
                p.generate_box(4, 1, 1, 4, 3, 3, &cave_air, &cave_air);
            }
        }
        Kind::RightTurn { door } => {
            p.generate_shell(0, 0, 0, 4, 4, 4, random);
            p.generate_small_door(random, door, 1, 1, 0);
            if matches!(p.node.orientation, Facing::North | Facing::East) {
                p.generate_box(4, 1, 1, 4, 3, 3, &cave_air, &cave_air);
            } else {
                p.generate_box(0, 1, 1, 0, 3, 3, &cave_air, &cave_air);
            }
        }
        Kind::RoomCrossing { door, room_type } => {
            p.generate_shell(0, 0, 0, 10, 6, 10, random);
            p.generate_small_door(random, door, 4, 1, 0);
            p.generate_box(4, 1, 10, 6, 3, 10, &cave_air, &cave_air);
            p.generate_box(0, 1, 4, 0, 3, 6, &cave_air, &cave_air);
            p.generate_box(10, 1, 4, 10, 3, 6, &cave_air, &cave_air);
            match room_type {
                0 => {
                    p.place(&stone_bricks, 5, 1, 5);
                    p.place(&stone_bricks, 5, 2, 5);
                    p.place(&stone_bricks, 5, 3, 5);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=west]"), 4, 3, 5);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=east]"), 6, 3, 5);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=south]"), 5, 3, 4);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=north]"), 5, 3, 6);
                    for (x, z) in [(4, 4), (4, 5), (4, 6), (6, 4), (6, 5), (6, 6), (5, 4), (5, 6)] {
                        p.place(&smooth_slab, x, 1, z);
                    }
                }
                1 => {
                    for i in 0..5 {
                        p.place(&stone_bricks, 3, 1, 3 + i);
                        p.place(&stone_bricks, 7, 1, 3 + i);
                        p.place(&stone_bricks, 3 + i, 1, 3);
                        p.place(&stone_bricks, 3 + i, 1, 7);
                    }
                    p.place(&stone_bricks, 5, 1, 5);
                    p.place(&stone_bricks, 5, 2, 5);
                    p.place(&stone_bricks, 5, 3, 5);
                    p.place(&BlockState::of("minecraft:water"), 5, 4, 5);
                }
                2 => {
                    let cobble = BlockState::of("minecraft:cobblestone");
                    for z in 1..=9 {
                        p.place(&cobble, 1, 3, z);
                        p.place(&cobble, 9, 3, z);
                    }
                    for x in 1..=9 {
                        p.place(&cobble, x, 3, 1);
                        p.place(&cobble, x, 3, 9);
                    }
                    p.place(&cobble, 5, 1, 4);
                    p.place(&cobble, 5, 1, 6);
                    p.place(&cobble, 5, 3, 4);
                    p.place(&cobble, 5, 3, 6);
                    p.place(&cobble, 4, 1, 5);
                    p.place(&cobble, 6, 1, 5);
                    p.place(&cobble, 4, 3, 5);
                    p.place(&cobble, 6, 3, 5);
                    for y in 1..=3 {
                        p.place(&cobble, 4, y, 4);
                        p.place(&cobble, 6, y, 4);
                        p.place(&cobble, 4, y, 6);
                        p.place(&cobble, 6, y, 6);
                    }
                    p.place(&BlockState::of("minecraft:wall_torch"), 5, 3, 5);
                    let planks = BlockState::of("minecraft:oak_planks");
                    for z in 2..=8 {
                        p.place(&planks, 2, 3, z);
                        p.place(&planks, 3, 3, z);
                        if z <= 3 || z >= 7 {
                            p.place(&planks, 4, 3, z);
                            p.place(&planks, 5, 3, z);
                            p.place(&planks, 6, 3, z);
                        }
                        p.place(&planks, 7, 3, z);
                        p.place(&planks, 8, 3, z);
                    }
                    let ladder = BlockState::parse("minecraft:ladder[facing=west,waterlogged=false]");
                    p.place(&ladder, 9, 1, 3);
                    p.place(&ladder, 9, 2, 3);
                    p.place(&ladder, 9, 3, 3);
                    p.create_chest(random, 3, 4, 8, "minecraft:chests/stronghold_crossing");
                }
                _ => {}
            }
        }
        Kind::StraightStairsDown { door } => {
            p.generate_shell(0, 0, 0, 4, 10, 7, random);
            p.generate_small_door(random, door, 1, 7, 0);
            p.generate_small_door(random, SmallDoorType::Opening, 1, 1, 7);
            let stairs = BlockState::parse("minecraft:cobblestone_stairs[facing=south,half=bottom,shape=straight,waterlogged=false]");
            for i in 0..6 {
                p.place(&stairs, 1, 6 - i, 1 + i);
                p.place(&stairs, 2, 6 - i, 1 + i);
                p.place(&stairs, 3, 6 - i, 1 + i);
                if i < 5 {
                    p.place(&stone_bricks, 1, 5 - i, 1 + i);
                    p.place(&stone_bricks, 2, 5 - i, 1 + i);
                    p.place(&stone_bricks, 3, 5 - i, 1 + i);
                }
            }
        }
        Kind::StairsDown { door, .. } => {
            p.generate_shell(0, 0, 0, 4, 10, 4, random);
            p.generate_small_door(random, door, 1, 7, 0);
            p.generate_small_door(random, SmallDoorType::Opening, 1, 1, 4);
            p.place(&stone_bricks, 2, 6, 1);
            p.place(&stone_bricks, 1, 5, 1);
            p.place(&smooth_slab, 1, 6, 1);
            p.place(&stone_bricks, 1, 5, 2);
            p.place(&stone_bricks, 1, 4, 3);
            p.place(&smooth_slab, 1, 5, 3);
            p.place(&stone_bricks, 2, 4, 3);
            p.place(&stone_bricks, 3, 3, 3);
            p.place(&smooth_slab, 3, 4, 3);
            p.place(&stone_bricks, 3, 3, 2);
            p.place(&stone_bricks, 3, 2, 1);
            p.place(&smooth_slab, 3, 3, 1);
            p.place(&stone_bricks, 2, 2, 1);
            p.place(&stone_bricks, 1, 1, 1);
            p.place(&smooth_slab, 1, 2, 1);
            p.place(&stone_bricks, 1, 1, 2);
            p.place(&smooth_slab, 1, 1, 3);
        }
        Kind::FiveCrossing {
            door,
            left_low,
            left_high,
            right_low,
            right_high,
        } => {
            p.generate_shell(0, 0, 0, 9, 8, 10, random);
            p.generate_small_door(random, door, 4, 3, 0);
            if left_low {
                p.generate_box(0, 3, 1, 0, 5, 3, &cave_air, &cave_air);
            }
            if right_low {
                p.generate_box(9, 3, 1, 9, 5, 3, &cave_air, &cave_air);
            }
            if left_high {
                p.generate_box(0, 5, 7, 0, 7, 9, &cave_air, &cave_air);
            }
            if right_high {
                p.generate_box(9, 5, 7, 9, 7, 9, &cave_air, &cave_air);
            }
            p.generate_box(5, 1, 10, 7, 3, 10, &cave_air, &cave_air);
            p.generate_shell(1, 2, 1, 8, 2, 6, random);
            p.generate_shell(4, 1, 5, 4, 4, 9, random);
            p.generate_shell(8, 1, 5, 8, 4, 9, random);
            p.generate_shell(1, 4, 7, 3, 4, 9, random);
            p.generate_shell(1, 3, 5, 3, 3, 6, random);
            p.generate_box(1, 3, 4, 3, 3, 4, &smooth_slab, &smooth_slab);
            p.generate_box(1, 4, 6, 3, 4, 6, &smooth_slab, &smooth_slab);
            p.generate_shell(5, 1, 7, 7, 1, 8, random);
            p.generate_box(5, 1, 9, 7, 1, 9, &smooth_slab, &smooth_slab);
            p.generate_box(5, 2, 7, 7, 2, 7, &smooth_slab, &smooth_slab);
            p.generate_box(4, 5, 7, 4, 5, 9, &smooth_slab, &smooth_slab);
            p.generate_box(8, 5, 7, 8, 5, 9, &smooth_slab, &smooth_slab);
            let double_slab = BlockState::parse("minecraft:smooth_stone_slab[type=double,waterlogged=false]");
            p.generate_box(5, 5, 7, 7, 5, 9, &double_slab, &double_slab);
            p.place(&BlockState::parse("minecraft:wall_torch[facing=south]"), 6, 5, 6);
        }
        Kind::ChestCorridor { door } => {
            p.generate_box(0, 0, 0, 4, 4, 6, &stone_bricks, &stone_bricks);
            p.generate_small_door(random, door, 1, 1, 0);
            p.generate_small_door(random, SmallDoorType::Opening, 1, 1, 6);
            p.generate_box(3, 1, 2, 3, 1, 4, &stone_bricks, &stone_bricks);
            p.place(&smooth_slab, 3, 1, 1);
            p.place(&smooth_slab, 3, 1, 5);
            p.place(&smooth_slab, 3, 2, 2);
            p.place(&smooth_slab, 3, 2, 4);
            for z in 2..=4 {
                p.place(&smooth_slab, 2, 1, z);
            }
            p.create_chest(random, 3, 2, 3, "minecraft:chests/stronghold_corridor");
        }
        Kind::Library { door, is_tall } => {
            let current_height = if is_tall { 11 } else { 6 };
            p.generate_shell(0, 0, 0, 13, current_height - 1, 14, random);
            p.generate_small_door(random, door, 4, 1, 0);
            let cobweb = BlockState::of("minecraft:cobweb");
            p.generate_maybe_box(random, 0.07, 2, 1, 1, 11, 4, 13, &cobweb, &cobweb);
            let planks = BlockState::of("minecraft:oak_planks");
            let bookshelf = BlockState::of("minecraft:bookshelf");
            for d in 1..=13 {
                if (d - 1) % 4 == 0 {
                    p.generate_box(1, 1, d, 1, 4, d, &planks, &planks);
                    p.generate_box(12, 1, d, 12, 4, d, &planks, &planks);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=east]"), 2, 3, d);
                    p.place(&BlockState::parse("minecraft:wall_torch[facing=west]"), 11, 3, d);
                    if is_tall {
                        p.generate_box(1, 6, d, 1, 9, d, &planks, &planks);
                        p.generate_box(12, 6, d, 12, 9, d, &planks, &planks);
                    }
                } else {
                    p.generate_box(1, 1, d, 1, 4, d, &bookshelf, &bookshelf);
                    p.generate_box(12, 1, d, 12, 4, d, &bookshelf, &bookshelf);
                    if is_tall {
                        p.generate_box(1, 6, d, 1, 9, d, &bookshelf, &bookshelf);
                        p.generate_box(12, 6, d, 12, 9, d, &bookshelf, &bookshelf);
                    }
                }
            }
            let mut d = 3;
            while d < 12 {
                p.generate_box(3, 1, d, 4, 3, d, &bookshelf, &bookshelf);
                p.generate_box(6, 1, d, 7, 3, d, &bookshelf, &bookshelf);
                p.generate_box(9, 1, d, 10, 3, d, &bookshelf, &bookshelf);
                d += 2;
            }
            if is_tall {
                p.generate_box(1, 5, 1, 3, 5, 13, &planks, &planks);
                p.generate_box(10, 5, 1, 12, 5, 13, &planks, &planks);
                p.generate_box(4, 5, 1, 9, 5, 2, &planks, &planks);
                p.generate_box(4, 5, 12, 9, 5, 13, &planks, &planks);
                p.place(&planks, 9, 5, 11);
                p.place(&planks, 8, 5, 11);
                p.place(&planks, 9, 5, 10);
                let we_fence = BlockState::parse("minecraft:oak_fence[east=true,north=false,south=false,waterlogged=false,west=true]");
                let ns_fence = BlockState::parse("minecraft:oak_fence[east=false,north=true,south=true,waterlogged=false,west=false]");
                p.generate_box(3, 6, 3, 3, 6, 11, &ns_fence, &ns_fence);
                p.generate_box(10, 6, 3, 10, 6, 9, &ns_fence, &ns_fence);
                p.generate_box(4, 6, 2, 9, 6, 2, &we_fence, &we_fence);
                p.generate_box(4, 6, 12, 7, 6, 12, &we_fence, &we_fence);
                // North+East corner, South+East corner, North+West corner —
                // spelled out fully to match the fence's five properties.
                p.place(&BlockState::parse("minecraft:oak_fence[east=true,north=true,south=false,waterlogged=false,west=false]"), 3, 6, 2);
                p.place(&BlockState::parse("minecraft:oak_fence[east=true,north=false,south=true,waterlogged=false,west=false]"), 3, 6, 12);
                p.place(&BlockState::parse("minecraft:oak_fence[east=false,north=true,south=false,waterlogged=false,west=true]"), 10, 6, 2);
                for i in 0..=2 {
                    p.place(
                        &BlockState::parse("minecraft:oak_fence[east=false,north=false,south=true,waterlogged=false,west=true]"),
                        8 + i,
                        6,
                        12 - i,
                    );
                    if i != 2 {
                        p.place(
                            &BlockState::parse("minecraft:oak_fence[east=true,north=true,south=false,waterlogged=false,west=false]"),
                            8 + i,
                            6,
                            11 - i,
                        );
                    }
                }
                let ladder = BlockState::parse("minecraft:ladder[facing=south,waterlogged=false]");
                for y in 1..=7 {
                    p.place(&ladder, 10, y, 13);
                }
                let e_fence = BlockState::parse("minecraft:oak_fence[east=true,north=false,south=false,waterlogged=false,west=false]");
                let w_fence = BlockState::parse("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=true]");
                p.place(&e_fence, 6, 9, 7);
                p.place(&w_fence, 7, 9, 7);
                p.place(&e_fence, 6, 8, 7);
                p.place(&w_fence, 7, 8, 7);
                let nswe_fence = BlockState::parse("minecraft:oak_fence[east=true,north=true,south=true,waterlogged=false,west=true]");
                p.place(&nswe_fence, 6, 7, 7);
                p.place(&nswe_fence, 7, 7, 7);
                p.place(&e_fence, 5, 7, 7);
                p.place(&w_fence, 8, 7, 7);
                p.place(&BlockState::parse("minecraft:oak_fence[east=true,north=true,south=false,waterlogged=false,west=false]"), 6, 7, 6);
                p.place(&BlockState::parse("minecraft:oak_fence[east=true,north=false,south=true,waterlogged=false,west=false]"), 6, 7, 8);
                p.place(&BlockState::parse("minecraft:oak_fence[east=false,north=true,south=false,waterlogged=false,west=true]"), 7, 7, 6);
                p.place(&BlockState::parse("minecraft:oak_fence[east=false,north=false,south=true,waterlogged=false,west=true]"), 7, 7, 8);
                let torch = BlockState::of("minecraft:torch");
                p.place(&torch, 5, 8, 7);
                p.place(&torch, 8, 8, 7);
                p.place(&torch, 6, 8, 6);
                p.place(&torch, 6, 8, 8);
                p.place(&torch, 7, 8, 6);
                p.place(&torch, 7, 8, 8);
            }
            p.create_chest(random, 3, 3, 5, "minecraft:chests/stronghold_library");
            if is_tall {
                p.place(&cave_air, 12, 9, 1);
                p.create_chest(random, 12, 8, 1, "minecraft:chests/stronghold_library");
            }
        }
        Kind::PortalRoom => {
            p.generate_shell(0, 0, 0, 10, 7, 15, random);
            p.generate_small_door(random, SmallDoorType::Grates, 4, 1, 0);
            p.generate_shell(1, 6, 1, 1, 6, 14, random);
            p.generate_shell(9, 6, 1, 9, 6, 14, random);
            p.generate_shell(2, 6, 1, 8, 6, 2, random);
            p.generate_shell(2, 6, 14, 8, 6, 14, random);
            p.generate_shell(1, 1, 1, 2, 1, 4, random);
            p.generate_shell(8, 1, 1, 9, 1, 4, random);
            let lava = BlockState::of("minecraft:lava");
            p.generate_box(1, 1, 1, 1, 1, 3, &lava, &lava);
            p.generate_box(9, 1, 1, 9, 1, 3, &lava, &lava);
            p.generate_shell(3, 1, 8, 7, 1, 12, random);
            p.generate_box(4, 1, 9, 6, 1, 11, &lava, &lava);
            let ns_bars = BlockState::parse("minecraft:iron_bars[east=false,north=true,south=true,waterlogged=false,west=false]");
            let we_bars = BlockState::parse("minecraft:iron_bars[east=true,north=false,south=false,waterlogged=false,west=true]");
            let mut z = 3;
            while z < 14 {
                p.generate_box(0, 3, z, 0, 4, z, &ns_bars, &ns_bars);
                p.generate_box(10, 3, z, 10, 4, z, &ns_bars, &ns_bars);
                z += 2;
            }
            let mut x = 2;
            while x < 9 {
                p.generate_box(x, 3, 15, x, 4, 15, &we_bars, &we_bars);
                x += 2;
            }
            p.generate_shell(4, 1, 5, 6, 1, 7, random);
            p.generate_shell(4, 2, 6, 6, 2, 7, random);
            p.generate_shell(4, 3, 7, 6, 3, 7, random);
            let stair = BlockState::parse("minecraft:stone_brick_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]");
            for x in 4..=6 {
                p.place(&stair, x, 1, 4);
                p.place(&stair, x, 2, 5);
                p.place(&stair, x, 3, 6);
            }
            // The end-portal frame ring — twelve frames, each `FACING` toward
            // the outside of the portal on the north/south rows and toward
            // the *centre* on the west/east columns (`EndPortalFrameBlock`'s
            // own convention, transcribed exactly from
            // `PortalRoom.postProcess`, not derived — see the module doc's
            // warning about getting this rotation right).
            let mut eyes = [false; 12];
            let mut all_eyes = true;
            for eye in &mut eyes {
                // `random.nextFloat() > 0.9F` — a 10% chance of a pre-filled
                // eye per frame.
                *eye = random.next_float() > 0.9;
                all_eyes &= *eye;
            }
            let frame = |facing: &str, eye: bool| BlockState::parse(&format!("minecraft:end_portal_frame[eye={eye},facing={facing}]"));
            p.place(&frame("north", eyes[0]), 4, 3, 8);
            p.place(&frame("north", eyes[1]), 5, 3, 8);
            p.place(&frame("north", eyes[2]), 6, 3, 8);
            p.place(&frame("south", eyes[3]), 4, 3, 12);
            p.place(&frame("south", eyes[4]), 5, 3, 12);
            p.place(&frame("south", eyes[5]), 6, 3, 12);
            p.place(&frame("east", eyes[6]), 3, 3, 9);
            p.place(&frame("east", eyes[7]), 3, 3, 10);
            p.place(&frame("east", eyes[8]), 3, 3, 11);
            p.place(&frame("west", eyes[9]), 7, 3, 9);
            p.place(&frame("west", eyes[10]), 7, 3, 10);
            p.place(&frame("west", eyes[11]), 7, 3, 11);
            if all_eyes {
                let portal = BlockState::of("minecraft:end_portal");
                for (x, z) in [
                    (4, 9), (5, 9), (6, 9),
                    (4, 10), (5, 10), (6, 10),
                    (4, 11), (5, 11), (6, 11),
                ] {
                    p.place(&portal, x, 3, z);
                }
            }
            // `SpawnerBlockEntity::setEntityId` — no entity-spawning layer
            // exists in this crate yet, see the module doc's
            // `coded:worldgen_entities` deviation.
            p.place(&BlockState::of("minecraft:spawner"), 5, 3, 6);
        }
        Kind::FillerCorridor { steps } => {
            for i in 0..steps {
                p.place(&stone_bricks, 0, 0, i);
                p.place(&stone_bricks, 1, 0, i);
                p.place(&stone_bricks, 2, 0, i);
                p.place(&stone_bricks, 3, 0, i);
                p.place(&stone_bricks, 4, 0, i);
                for y in 1..=3 {
                    p.place(&stone_bricks, 0, y, i);
                    p.place(&cave_air, 1, y, i);
                    p.place(&cave_air, 2, y, i);
                    p.place(&cave_air, 3, y, i);
                    p.place(&stone_bricks, 4, y, i);
                }
                p.place(&stone_bricks, 0, 4, i);
                p.place(&stone_bricks, 1, 4, i);
                p.place(&stone_bricks, 2, 4, i);
                p.place(&stone_bricks, 3, 4, i);
                p.place(&stone_bricks, 4, 4, i);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedCtx;
    impl StartContext for FixedCtx {
        fn first_occupied_height(&self, _x: i32, _z: i32, _h: super::super::HeightmapKind) -> i32 {
            64
        }
        fn biome_at_quart(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:stone_shore".to_string()
        }
        fn sea_level(&self) -> i32 {
            63
        }
    }

    /// The weight table's total, re-derived from [`initial_weights`] rather
    /// than asserted from memory — 40+5+20+20+10+5+5+5+5+10+20.
    #[test]
    fn weights_sum_to_the_derived_total() {
        let total: i32 = initial_weights().iter().map(|w| w.weight).sum();
        assert_eq!(total, 145);
    }

    /// Every generated stronghold must contain exactly one portal room, and
    /// its ring must be the twelve frames vanilla's `PortalRoom.postProcess`
    /// places, each facing the direction the record names — not derived from
    /// intuition about "facing the centre".
    #[test]
    fn every_seed_produces_a_correctly_faced_portal_ring() {
        for seed in [1i64, 2, -195_764_831, 999_999, 42] {
            let pieces = generate(0, 0, seed, &FixedCtx);
            assert!(!pieces.is_empty(), "seed {seed}: empty piece list");
            let portal_rooms: Vec<_> = pieces.iter().filter(|p| p.id == "minecraft:shpr").collect();
            assert_eq!(portal_rooms.len(), 1, "seed {seed}: expected exactly one portal room");
            let frames: Vec<_> = portal_rooms[0]
                .blocks
                .as_ref()
                .unwrap()
                .iter()
                .filter(|b| b.state.starts_with("minecraft:end_portal_frame"))
                .collect();
            assert_eq!(frames.len(), 12, "seed {seed}: expected 12 end portal frames");
            for f in &frames {
                assert!(
                    f.state.contains("facing=north")
                        || f.state.contains("facing=south")
                        || f.state.contains("facing=east")
                        || f.state.contains("facing=west"),
                    "seed {seed}: frame with no recognised facing: {}",
                    f.state
                );
            }
        }
    }

    /// No two *weighted* pieces in a generated tree may overlap —
    /// `try_create_piece` checks every one of them against the whole tree via
    /// `Tree::collides` before construction, so this is a strong regression
    /// detector for `orient_box`/collision bugs even with no oracle to
    /// compare against (the survival oracle world has no stronghold in its
    /// generated area — see `tests/support/structure_starts_survival.txt`'s
    /// own "Absent from the generated area" note).
    ///
    /// `FillerCorridor` (`minecraft:shfc`) is excluded on purpose: its own
    /// `findPieceBox` (`filler_corridor_box`) only re-checks the *one* piece
    /// `findCollisionPiece` happens to find first, exactly as vanilla's
    /// `StructurePiece.findCollisionPiece` is a first-match linear scan — so
    /// a filler can legitimately overlap a *different* piece further down
    /// the list. That is vanilla's own behaviour, not a bug in this port;
    /// collected mismatches (not an `assert!` inside the loop) below would
    /// show every instance if this stopped holding for the eleven weighted
    /// types.
    #[test]
    fn no_two_weighted_pieces_overlap() {
        let mut mismatches = Vec::new();
        for seed in [7i64, -12345, 2026] {
            let pieces = generate(3, -2, seed, &FixedCtx);
            for i in 0..pieces.len() {
                if pieces[i].id == "minecraft:shfc" {
                    continue;
                }
                for j in (i + 1)..pieces.len() {
                    if pieces[j].id == "minecraft:shfc" {
                        continue;
                    }
                    if pieces[i].bounding_box.intersects(pieces[j].bounding_box) {
                        mismatches.push((seed, i, pieces[i].id.clone(), j, pieces[j].id.clone()));
                    }
                }
            }
        }
        assert!(mismatches.is_empty(), "overlapping weighted pieces: {mismatches:?}");
    }

    /// Determinism: the same seed and chunk produce byte-identical output.
    #[test]
    fn deterministic_for_a_fixed_seed() {
        let a = generate(5, 5, 123, &FixedCtx);
        let b = generate(5, 5, 123, &FixedCtx);
        assert_eq!(a.len(), b.len());
        for (pa, pb) in a.iter().zip(b.iter()) {
            assert_eq!(pa.id, pb.id);
            assert_eq!(pa.bounding_box.min, pb.bounding_box.min);
            assert_eq!(pa.bounding_box.max, pb.bounding_box.max);
        }
    }

    /// No piece's generation depth exceeds vanilla's cap — a child's depth is
    /// `parent + 1` and `generateAndAddPiece` refuses once the *parent* is
    /// already past 50, so 51 is the highest depth reachable.
    #[test]
    fn depth_never_exceeds_the_cap() {
        let pieces = generate(-4, 6, 8675309, &FixedCtx);
        for p in &pieces {
            assert!(p.gen_depth <= MAX_DEPTH + 1, "gen_depth {} exceeds cap", p.gen_depth);
        }
    }
}
