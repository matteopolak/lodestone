//! Block placement / item use — the inverse of [`crate::mining`].
//!
//! This module is the version-free *game* half of "the player right-clicks a
//! block with an item". It answers three questions vanilla answers, in the same
//! order vanilla answers them, and it must match — servers reject and roll back
//! a placement whose target or legality the client got wrong.
//!
//! 1. **Does this right-click actuate the block instead of placing?**
//!    ([`Placement::use_on`]). Right-clicking a chest, door, button, furnace or
//!    crafting table *opens/actuates* it and places nothing — unless the player
//!    is **sneaking while holding an item**, which suppresses the block use and
//!    forces placement. This is vanilla `MultiPlayerGameMode.performUseItemOn`'s
//!    `suppressUsingBlock = isSecondaryUseActive() && haveSomethingInOurHands`
//!    ordering (26.2), not a nicety.
//! 2. **Where does the block go?** ([`resolve_target`]). Right-clicking a face
//!    places into the *adjacent* position — **unless the clicked block is itself
//!    replaceable** (air, water, lava, tall grass, a snow layer), in which case
//!    the block replaces it *in place*. This is `BlockPlaceContext`'s
//!    `replaceClicked = getBlockState(clickedPos).canBeReplaced(this)` and its
//!    `getClickedPos() = replaceClicked ? clickedPos : relativePos`. Getting it
//!    backwards is the classic placement bug.
//! 3. **What block state results?** ([`resolve_state`]). Facing (stairs,
//!    furnaces), axis (pillars, logs) and half (slabs, stairs) depend on the
//!    *player*, not just the item. This is the large, per-block surface of
//!    vanilla's `Block.getStateForPlacement`; this crate resolves the
//!    **geometry-derived** subset exactly and is honest about the boundary (see
//!    [`OrientationKind`]).
//!
//! # Injected world, never a block table
//!
//! Whether a block is *replaceable* or *interactable*, and whether a target is
//! obstructed by an entity, is per-version world/registry data. This crate holds
//! **no** such table: the caller injects it through [`PlacementWorld`], exactly
//! the way [`crate::mining`] injects the break formula's inputs and
//! `lodestone-physics` injects geometry through `CollisionView`. A test that
//! agreed with a hardness/replaceable table we minted ourselves would prove
//! nothing; the live gate (`tests/live_place.rs`) reads the *server's* truth.
//!
//! # Server-authoritative reconcile
//!
//! Placement is predicted then reconciled, the same seam as [`crate::reconcile`]
//! for containers. [`Placement::use_on`] stamps every placement with the modern
//! block-prediction `sequence` and records it pending; the client optimistically
//! shows the block immediately. The server echoes the sequence in a
//! block-changed-ack and, for an *accepted* placement, has already broadcast the
//! authoritative block update; for a *rejected* one (spawn protection, illegal
//! support, an occupied space) it never does, so the optimistic block must snap
//! back. [`Placement::reconcile`] models that divergence explicitly rather than
//! assuming the client is always right — a design that assumes success works
//! offline and desynchronises against a real server the first time a placement
//! is refused.
//!
//! # Behavioural reference, not a copy
//!
//! Every rule below was derived from the decompiled 26.2 client/server as a
//! *behavioural reference only* and re-implemented from scratch; equivalence is
//! proven by the hermetic golden tests in this file and the live gate, never by
//! transliteration.

use lodestone_model::math::{BlockPos, Rotation, Vec3f};
use lodestone_model::{BlockFace, ClientAction, Hand, Identifier};

/// A block axis (`Direction.Axis`). Version-free; the model models faces but not
/// a bare axis, so pillar/log placement needs its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    /// East–west.
    X,
    /// Up–down.
    Y,
    /// North–south.
    Z,
}

/// The vertical half a slab or stair occupies (`Half` / `SlabType` bottom vs
/// top). Double slabs and waterlogging are out of scope (see [`OrientationKind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Half {
    /// Lower half — the default for a top-face click or a lower hit.
    Bottom,
    /// Upper half — a bottom-face click or an upper hit.
    Top,
}

/// The unit offset of a face, matching `Direction`'s normal vectors exactly.
#[must_use]
pub fn face_offset(face: BlockFace) -> (i32, i32, i32) {
    match face {
        BlockFace::Down => (0, -1, 0),
        BlockFace::Up => (0, 1, 0),
        BlockFace::North => (0, 0, -1),
        BlockFace::South => (0, 0, 1),
        BlockFace::West => (-1, 0, 0),
        BlockFace::East => (1, 0, 0),
    }
}

/// The block adjacent to `pos` across `face` (`BlockPos.relative(direction)`).
#[must_use]
pub fn offset(pos: BlockPos, face: BlockFace) -> BlockPos {
    let (dx, dy, dz) = face_offset(face);
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// The opposite face (`Direction.getOpposite`).
#[must_use]
pub fn opposite(face: BlockFace) -> BlockFace {
    match face {
        BlockFace::Down => BlockFace::Up,
        BlockFace::Up => BlockFace::Down,
        BlockFace::North => BlockFace::South,
        BlockFace::South => BlockFace::North,
        BlockFace::West => BlockFace::East,
        BlockFace::East => BlockFace::West,
    }
}

/// The axis a face lies on (`Direction.getAxis`).
#[must_use]
pub fn face_axis(face: BlockFace) -> Axis {
    match face {
        BlockFace::Down | BlockFace::Up => Axis::Y,
        BlockFace::North | BlockFace::South => Axis::Z,
        BlockFace::West | BlockFace::East => Axis::X,
    }
}

/// The horizontal face a yaw points along (`Direction.fromYRot`).
///
/// The 2D-data order is `[SOUTH, WEST, NORTH, EAST]` and the index is
/// `floor(yaw / 90 + 0.5) & 3`, bit-for-bit vanilla — this is what
/// `UseOnContext.getHorizontalDirection` (`player.getDirection()`) returns and
/// what every horizontal-facing block reads at placement.
#[must_use]
pub fn horizontal_from_yaw(yaw: f32) -> BlockFace {
    const BY_2D: [BlockFace; 4] = [
        BlockFace::South,
        BlockFace::West,
        BlockFace::North,
        BlockFace::East,
    ];
    let idx = ((yaw / 90.0 + 0.5).floor() as i64 & 3) as usize;
    BY_2D[idx]
}

/// The six faces ordered nearest-to-farthest from the player's look vector
/// (`Direction.orderedByNearest`). Index `0` is `getNearestLookingDirection`,
/// which directional blocks (dispenser, observer, piston) place *opposite* to.
///
/// Re-implemented from the trig in `Direction.orderedByNearest`: the look vector
/// is `(-sin(yaw)·cos(pitch), sin(-pitch), cos(yaw)·cos(pitch))` in vanilla's
/// convention, and the ordering picks the dominant axis, then the next, then the
/// last, each toward the sign of the look component.
#[must_use]
pub fn ordered_by_nearest(rotation: Rotation) -> [BlockFace; 6] {
    let pitch = rotation.pitch.to_radians();
    let yaw = -rotation.yaw.to_radians();
    let pitch_sin = pitch.sin();
    let pitch_cos = pitch.cos();
    let yaw_sin = yaw.sin();
    let yaw_cos = yaw.cos();

    let x_pos = yaw_sin > 0.0;
    let y_pos = pitch_sin < 0.0;
    let z_pos = yaw_cos > 0.0;
    let x_yaw = if x_pos { yaw_sin } else { -yaw_sin };
    let y_mag = if y_pos { -pitch_sin } else { pitch_sin };
    let z_yaw = if z_pos { yaw_cos } else { -yaw_cos };
    let x_mag = x_yaw * pitch_cos;
    let z_mag = z_yaw * pitch_cos;

    let axis_x = if x_pos { BlockFace::East } else { BlockFace::West };
    let axis_y = if y_pos { BlockFace::Up } else { BlockFace::Down };
    let axis_z = if z_pos { BlockFace::South } else { BlockFace::North };

    let arr = |a, b, c| make_direction_array(a, b, c);
    if x_yaw > z_yaw {
        if y_mag > x_mag {
            arr(axis_y, axis_x, axis_z)
        } else if z_mag > y_mag {
            arr(axis_x, axis_z, axis_y)
        } else {
            arr(axis_x, axis_y, axis_z)
        }
    } else if y_mag > z_mag {
        arr(axis_y, axis_z, axis_x)
    } else if x_mag > y_mag {
        arr(axis_z, axis_x, axis_y)
    } else {
        arr(axis_z, axis_y, axis_x)
    }
}

/// `Direction.makeDirectionArray`: the three dominant faces followed by their
/// opposites in reverse, so all six faces appear exactly once.
fn make_direction_array(a: BlockFace, b: BlockFace, c: BlockFace) -> [BlockFace; 6] {
    [a, b, c, opposite(c), opposite(b), opposite(a)]
}

/// The half a slab/stair occupies given the clicked face and the hit's
/// block-local Y (`cursor.y` in `0.0..=1.0`), matching the shared expression in
/// `SlabBlock`/`StairBlock.getStateForPlacement`:
///
/// > bottom unless the face is `DOWN`, or the face is a side and the hit is in
/// > the upper half (`> 0.5`).
#[must_use]
pub fn half_from_hit(face: BlockFace, cursor_y: f32) -> Half {
    if face != BlockFace::Down && (face == BlockFace::Up || cursor_y <= 0.5) {
        Half::Bottom
    } else {
        Half::Top
    }
}

/// The world facts placement needs, injected by the driver. This crate holds no
/// block table of its own — the same discipline as `CollisionView`.
pub trait PlacementWorld {
    /// Whether the block at `pos` can be replaced by a placement
    /// (`BlockState.canBeReplaced`): air, fluids, tall grass, snow layers, etc.
    /// Drives both the place-into-vs-adjacent choice and legality.
    fn is_replaceable(&self, pos: BlockPos) -> bool;

    /// Whether right-clicking the block at `pos` actuates it
    /// (`BlockState.useItemOn`/`useWithoutItem` consumes the action): chests,
    /// doors, buttons, furnaces, crafting tables, etc. When `true` and the
    /// player is not sneaking-with-an-item, the click opens/actuates and places
    /// nothing.
    fn is_interactable(&self, pos: BlockPos) -> bool;

    /// Whether placing at `pos` is blocked by an entity or existing collision
    /// (`Level.isUnobstructed`). Defaults to unobstructed; override to model the
    /// "can't place a block where a mob or the player stands" rule.
    fn is_obstructed(&self, _pos: BlockPos) -> bool {
        false
    }
}

/// The resolved placement target: where the block lands and how it got there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The block position the new block occupies.
    pub pos: BlockPos,
    /// Whether the clicked block was itself replaced in place
    /// (`replacingClickedOnBlock`). `false` means the adjacent cell was used.
    pub replaced_clicked: bool,
}

/// Resolves *where* a face-click places, mirroring `BlockPlaceContext`.
///
/// If the clicked block is replaceable the placement replaces it in place;
/// otherwise it goes to the adjacent cell across `face`. This never consults
/// legality — [`Placement::use_on`] does that — so the result is defined even
/// for an obstructed or illegal target, matching vanilla's split between
/// `getClickedPos` and `canPlace`.
#[must_use]
pub fn resolve_target(
    clicked: BlockPos,
    face: BlockFace,
    world: &dyn PlacementWorld,
) -> Target {
    if world.is_replaceable(clicked) {
        Target {
            pos: clicked,
            replaced_clicked: true,
        }
    } else {
        Target {
            pos: offset(clicked, face),
            replaced_clicked: false,
        }
    }
}

/// How a block derives its orientation from placement geometry. The driver
/// classifies the block from registry data (its block-state properties) and
/// hands the kind in; this crate never maps an id to a kind (that is per-version
/// data). Each kind's convention is the exact one from the matching vanilla
/// `getStateForPlacement`.
///
/// # The boundary
///
/// This resolves only the **geometry-derived** state — the subset a right-click
/// determines from the face, the hit position and the player's rotation. It is
/// deliberately bounded and honest about it:
///
/// * **Resolved exactly:** [`Fixed`](Self::Fixed) (no orientation),
///   [`Pillar`](Self::Pillar) (logs, basalt), [`Slab`](Self::Slab),
///   [`FacingHorizontal`](Self::FacingHorizontal) (stairs' facing, ladders,
///   banners on floor), [`FacingHorizontalOpposite`](Self::FacingHorizontalOpposite)
///   (furnaces, chests, pumpkins, most "faces the player" blocks),
///   [`FacingAll`](Self::FacingAll) (dispensers, droppers, observers, pistons)
///   and [`Stairs`](Self::Stairs) (facing + half).
/// * **Not resolved (falls back to the block's default state):** stair *shape*
///   (inner/outer corners, which depends on neighbouring stairs, not geometry),
///   waterlogging, multi-part blocks (doors, beds, tall flowers), rotation-16
///   blocks (signs, banners on 16-step yaw), wall-vs-floor variants, and any
///   state a block computes from its neighbours. A driver placing one of these
///   should use the default state and let the server's authoritative block
///   update correct it — the reconcile seam exists precisely for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrientationKind {
    /// No orientation property (stone, dirt, glass).
    Fixed,
    /// A pillar whose `axis` is the clicked face's axis (`RotatedPillarBlock`).
    Pillar,
    /// A slab whose half comes from the face and hit Y (`SlabBlock`). Placing a
    /// slab into a matching half to form a double slab is **not** modelled here.
    Slab,
    /// Faces the player's horizontal look direction (`StairBlock` facing,
    /// `LadderBlock`, floor banners): `facing = getHorizontalDirection()`.
    FacingHorizontal,
    /// Faces *away* from the player horizontally (`HorizontalDirectionalBlock`:
    /// furnace, chest, pumpkin, end-portal frame): `getHorizontalDirection()
    /// .getOpposite()`.
    FacingHorizontalOpposite,
    /// Faces away from the player in any of the six directions
    /// (`DirectionalBlock`: dispenser, dropper, observer, piston):
    /// `getNearestLookingDirection().getOpposite()`.
    FacingAll,
    /// A stair: `facing = getHorizontalDirection()`, `half` from face and hit Y.
    /// Shape (corners) is left at the straight default and corrected by the
    /// server.
    Stairs,
}

/// The geometry-derived block state a placement resolves. Every field is
/// `Option`: a field is `Some` only when the [`OrientationKind`] defines it, so
/// a consumer can apply exactly the properties vanilla would set and leave the
/// block's other properties at their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlacedState {
    /// The `facing` property, for the horizontal- and all-facing kinds and
    /// stairs.
    pub facing: Option<BlockFace>,
    /// The `axis` property, for pillars.
    pub axis: Option<Axis>,
    /// The `half`/`type` property, for slabs and stairs.
    pub half: Option<Half>,
}

/// Everything a use-on-block needs, in canonical form. Mirrors the fields of
/// `ServerboundUseItemOnPacket`'s `BlockHitResult` plus the player context the
/// interaction ordering reads.
#[derive(Debug, Clone)]
pub struct UseOnContext {
    /// Hand holding the item.
    pub hand: Hand,
    /// The block face was hit on.
    pub clicked: BlockPos,
    /// The face that was hit.
    pub face: BlockFace,
    /// The hit position within the block face, block-local in `0.0..=1.0`.
    pub cursor: Vec3f,
    /// Whether the ray started inside a block.
    pub inside_block: bool,
    /// The player's rotation, from which facing state is derived.
    pub rotation: Rotation,
    /// Whether the player is sneaking (secondary-use active).
    pub sneaking: bool,
    /// Whether either hand holds an item (`haveSomethingInOurHands`).
    pub has_item_in_hand: bool,
    /// The block this item would place, if it is a block item; `None` for a
    /// non-placeable item (food, tools) — such an item never places, though it
    /// may still trigger a block interaction.
    pub placing: Option<Identifier>,
    /// How the placed block derives its orientation.
    pub orientation: OrientationKind,
}

/// Resolves the geometry-derived [`PlacedState`] for a placement.
///
/// Pure in its inputs: the clicked face, the hit's block-local Y, the player
/// rotation and the block's [`OrientationKind`]. It never reads the world, so a
/// caller can resolve a state before deciding whether the placement is legal.
#[must_use]
pub fn resolve_state(
    face: BlockFace,
    cursor_y: f32,
    rotation: Rotation,
    orientation: OrientationKind,
) -> PlacedState {
    match orientation {
        OrientationKind::Fixed => PlacedState::default(),
        OrientationKind::Pillar => PlacedState {
            axis: Some(face_axis(face)),
            ..PlacedState::default()
        },
        OrientationKind::Slab => PlacedState {
            half: Some(half_from_hit(face, cursor_y)),
            ..PlacedState::default()
        },
        OrientationKind::FacingHorizontal => PlacedState {
            facing: Some(horizontal_from_yaw(rotation.yaw)),
            ..PlacedState::default()
        },
        OrientationKind::FacingHorizontalOpposite => PlacedState {
            facing: Some(opposite(horizontal_from_yaw(rotation.yaw))),
            ..PlacedState::default()
        },
        OrientationKind::FacingAll => PlacedState {
            facing: Some(opposite(ordered_by_nearest(rotation)[0])),
            ..PlacedState::default()
        },
        OrientationKind::Stairs => PlacedState {
            facing: Some(horizontal_from_yaw(rotation.yaw)),
            half: Some(half_from_hit(face, cursor_y)),
            ..PlacedState::default()
        },
    }
}

/// The decision [`Placement::use_on`] reaches for a right-click, mirroring the
/// branch structure of `MultiPlayerGameMode.performUseItemOn`.
#[derive(Debug, Clone, PartialEq)]
pub enum UseOnDecision {
    /// The block was actuated (chest/door/etc.); nothing is placed. Vanilla
    /// still sends the `use_item_on` packet — the server runs the same branch —
    /// so the action is carried here for the driver to send.
    Interact {
        /// The `use_item_on` action to send.
        action: ClientAction,
    },
    /// A block was placed. The action is the `use_item_on` packet; the
    /// [`prediction`](UseOnDecision::Place::prediction) is what the client
    /// should optimistically show until the server reconciles.
    Place {
        /// The `use_item_on` action to send.
        action: ClientAction,
        /// The optimistic placement to apply locally.
        prediction: PlacePrediction,
    },
    /// Neither interaction nor placement applies (an empty or non-placeable hand
    /// on a non-interactable block, or an illegal/obstructed target). Vanilla
    /// still sends the packet; nothing changes locally.
    Nothing {
        /// The `use_item_on` action to send.
        action: ClientAction,
    },
}

/// An optimistic placement the client applies before the server confirms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacePrediction {
    /// The block-prediction sequence the server echoes on ack.
    pub sequence: i32,
    /// Where the block was placed.
    pub pos: BlockPos,
    /// The block that was placed.
    pub block: Identifier,
    /// The geometry-derived state the placement resolved.
    pub state: PlacedState,
}

/// The result of reconciling a placement against the server's authoritative
/// block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaceReconciliation {
    /// Whether the server disagreed with the prediction — i.e. the player saw
    /// the optimistic block snap back (a rejected or altered placement).
    pub corrected: bool,
}

/// The version-free block-placement / item-use state machine.
///
/// Drive it from the input loop: [`use_on`](Self::use_on) when the player
/// right-clicks a block. It owns the block-prediction `sequence` counter the
/// modern protocol requires (shared conceptually with [`crate::mining`] but a
/// separate counter per machine is fine — the server tracks the max acked
/// sequence, and each machine's are monotonic) and the ledger of predictions
/// awaiting server confirmation.
#[derive(Debug, Default)]
pub struct Placement {
    next_sequence: i32,
    pending: Vec<PlacePrediction>,
}

impl Placement {
    /// A fresh machine with no pending placements.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The placements still awaiting a server ack.
    #[must_use]
    pub fn pending(&self) -> &[PlacePrediction] {
        &self.pending
    }

    fn take_sequence(&mut self) -> i32 {
        // Vanilla's `BlockStatePredictionHandler` pre-increments, so the first
        // prediction is sequence 1.
        self.next_sequence += 1;
        self.next_sequence
    }

    /// Handle a right-click on a block, mirroring `performUseItemOn`'s ordering.
    ///
    /// 1. If the player is **not** sneaking-with-an-item and the block is
    ///    interactable, the click actuates it and nothing is placed
    ///    ([`UseOnDecision::Interact`]).
    /// 2. Otherwise, if the held item is a placeable block and the resolved
    ///    target is legal (replaceable target, unobstructed), the block is
    ///    placed ([`UseOnDecision::Place`]) and recorded pending.
    /// 3. Otherwise nothing changes ([`UseOnDecision::Nothing`]).
    ///
    /// Every branch returns the `use_item_on` [`ClientAction`] to send: vanilla
    /// always transmits the packet (the server re-runs the same decision), so
    /// the adapter lowers it regardless of the local outcome.
    pub fn use_on(&mut self, ctx: &UseOnContext, world: &dyn PlacementWorld) -> UseOnDecision {
        let sequence = self.take_sequence();
        let action = ClientAction::UseItemOn {
            hand: ctx.hand,
            pos: ctx.clicked,
            face: ctx.face,
            cursor: ctx.cursor,
            inside_block: ctx.inside_block,
            sequence,
        };

        let suppress_block_use = ctx.sneaking && ctx.has_item_in_hand;
        if !suppress_block_use && world.is_interactable(ctx.clicked) {
            return UseOnDecision::Interact { action };
        }

        let Some(block) = ctx.placing.clone() else {
            return UseOnDecision::Nothing { action };
        };

        let target = resolve_target(ctx.clicked, ctx.face, world);
        let legal = (target.replaced_clicked || world.is_replaceable(target.pos))
            && !world.is_obstructed(target.pos);
        if !legal {
            return UseOnDecision::Nothing { action };
        }

        let state = resolve_state(ctx.face, ctx.cursor.y, ctx.rotation, ctx.orientation);
        let prediction = PlacePrediction {
            sequence,
            pos: target.pos,
            block,
            state,
        };
        self.pending.push(prediction.clone());
        UseOnDecision::Place { action, prediction }
    }

    /// Clear predictions the server has acknowledged (`endPredictionsUpTo`).
    ///
    /// The block-changed-ack carries the highest sequence the server has
    /// processed. Every pending prediction at or below it is *settled*: for an
    /// accepted placement the server has already sent the authoritative block
    /// update (which the driver applied over the optimistic block), and for a
    /// rejected one it did not, so the driver reverts the optimistic block to
    /// server truth. Either way this machine stops tracking it. Returns the
    /// settled predictions so the driver knows which optimistic blocks to
    /// reconcile against its world.
    pub fn acknowledge(&mut self, sequence: i32) -> Vec<PlacePrediction> {
        let mut settled = Vec::new();
        self.pending.retain(|p| {
            if p.sequence <= sequence {
                settled.push(p.clone());
                false
            } else {
                true
            }
        });
        settled
    }

    /// Reconcile a pending placement against the server's authoritative block at
    /// its position, clearing it from the ledger.
    ///
    /// `server_block` is what the server reports at the predicted position
    /// (`None` for air). The placement *diverged* — the player saw a rollback —
    /// when the server's block is not the one we predicted. This is the seam a
    /// driver that owns a block store calls when a block update lands for a
    /// predicted position; it makes the accept/reject observable rather than
    /// silently trusting the prediction.
    ///
    /// A position with no pending prediction reconciles as `corrected: false`.
    pub fn reconcile(
        &mut self,
        pos: BlockPos,
        server_block: Option<&Identifier>,
    ) -> PlaceReconciliation {
        let mut corrected = false;
        self.pending.retain(|p| {
            if p.pos == pos {
                if server_block != Some(&p.block) {
                    corrected = true;
                }
                false
            } else {
                true
            }
        });
        PlaceReconciliation { corrected }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        format!("minecraft:{s}").parse().unwrap()
    }

    /// A world driven by explicit sets, so a test states exactly which blocks are
    /// replaceable/interactable/obstructed and nothing is inferred from a table.
    #[derive(Default)]
    struct FakeWorld {
        replaceable: Vec<BlockPos>,
        interactable: Vec<BlockPos>,
        obstructed: Vec<BlockPos>,
    }
    impl PlacementWorld for FakeWorld {
        fn is_replaceable(&self, pos: BlockPos) -> bool {
            self.replaceable.contains(&pos)
        }
        fn is_interactable(&self, pos: BlockPos) -> bool {
            self.interactable.contains(&pos)
        }
        fn is_obstructed(&self, pos: BlockPos) -> bool {
            self.obstructed.contains(&pos)
        }
    }

    fn ctx(clicked: BlockPos, face: BlockFace) -> UseOnContext {
        UseOnContext {
            hand: Hand::Main,
            clicked,
            face,
            cursor: Vec3f::new(0.5, 0.5, 0.5),
            inside_block: false,
            rotation: Rotation {
                yaw: 0.0,
                pitch: 0.0,
            },
            sneaking: false,
            has_item_in_hand: true,
            placing: Some(id("stone")),
            orientation: OrientationKind::Fixed,
        }
    }

    #[test]
    fn face_offsets_match_direction_normals() {
        assert_eq!(face_offset(BlockFace::Up), (0, 1, 0));
        assert_eq!(face_offset(BlockFace::Down), (0, -1, 0));
        assert_eq!(face_offset(BlockFace::North), (0, 0, -1));
        assert_eq!(face_offset(BlockFace::South), (0, 0, 1));
        assert_eq!(face_offset(BlockFace::West), (-1, 0, 0));
        assert_eq!(face_offset(BlockFace::East), (1, 0, 0));
    }

    #[test]
    fn target_is_adjacent_for_a_solid_clicked_block() {
        // Clicking the top face of a solid (non-replaceable) block places above.
        let world = FakeWorld::default();
        let t = resolve_target(BlockPos::new(10, 64, 10), BlockFace::Up, &world);
        assert_eq!(t.pos, BlockPos::new(10, 65, 10));
        assert!(!t.replaced_clicked);
    }

    #[test]
    fn target_is_in_place_for_a_replaceable_clicked_block() {
        // Clicking tall grass (replaceable) places *into* it, not above it —
        // the classic bug is doing the adjacent thing here.
        let grass = BlockPos::new(10, 64, 10);
        let world = FakeWorld {
            replaceable: vec![grass],
            ..FakeWorld::default()
        };
        let t = resolve_target(grass, BlockFace::Up, &world);
        assert_eq!(t.pos, grass);
        assert!(t.replaced_clicked);
    }

    #[test]
    fn interactable_block_actuates_and_places_nothing() {
        let chest = BlockPos::new(0, 64, 0);
        let world = FakeWorld {
            interactable: vec![chest],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        let d = m.use_on(&ctx(chest, BlockFace::North), &world);
        assert!(matches!(d, UseOnDecision::Interact { .. }));
        assert!(m.pending().is_empty(), "an interaction places nothing");
    }

    #[test]
    fn sneaking_with_an_item_forces_placement_over_interaction() {
        let chest = BlockPos::new(0, 64, 0);
        let world = FakeWorld {
            interactable: vec![chest],
            replaceable: vec![BlockPos::new(0, 65, 0)], // air above the chest
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        let mut c = ctx(chest, BlockFace::Up);
        c.sneaking = true;
        let d = m.use_on(&c, &world);
        match d {
            UseOnDecision::Place { prediction, .. } => {
                // Placed on top of the chest, since the chest itself is not
                // replaceable.
                assert_eq!(prediction.pos, BlockPos::new(0, 65, 0));
            }
            other => panic!("sneak+item must place, got {other:?}"),
        }
    }

    #[test]
    fn empty_hand_on_interactable_still_interacts() {
        let door = BlockPos::new(1, 64, 1);
        let world = FakeWorld {
            interactable: vec![door],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        let mut c = ctx(door, BlockFace::North);
        c.placing = None;
        c.has_item_in_hand = false;
        // Not sneaking, so even an empty hand actuates the door.
        assert!(matches!(
            m.use_on(&c, &world),
            UseOnDecision::Interact { .. }
        ));
    }

    #[test]
    fn non_placeable_item_on_plain_block_does_nothing() {
        let stone = BlockPos::new(2, 64, 2);
        let world = FakeWorld::default();
        let mut m = Placement::new();
        let mut c = ctx(stone, BlockFace::Up);
        c.placing = None; // e.g. a sword
        assert!(matches!(m.use_on(&c, &world), UseOnDecision::Nothing { .. }));
    }

    #[test]
    fn obstructed_target_is_illegal() {
        // The adjacent cell is air (placeable) but a mob stands there:
        // placement is refused by the obstruction, not the replaceable check.
        let ground = BlockPos::new(3, 64, 3);
        let above = BlockPos::new(3, 65, 3);
        let world = FakeWorld {
            replaceable: vec![above],
            obstructed: vec![above],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        assert!(matches!(
            m.use_on(&ctx(ground, BlockFace::Up), &world),
            UseOnDecision::Nothing { .. }
        ));
    }

    #[test]
    fn use_on_always_carries_the_packet_and_a_fresh_sequence() {
        let world = FakeWorld::default();
        let mut m = Placement::new();
        let d1 = m.use_on(&ctx(BlockPos::new(0, 64, 0), BlockFace::Up), &world);
        let d2 = m.use_on(&ctx(BlockPos::new(0, 64, 0), BlockFace::Up), &world);
        let seq = |d: &UseOnDecision| match d {
            UseOnDecision::Interact { action }
            | UseOnDecision::Place { action, .. }
            | UseOnDecision::Nothing { action } => match action {
                ClientAction::UseItemOn { sequence, .. } => *sequence,
                _ => panic!("expected UseItemOn"),
            },
        };
        assert_eq!(seq(&d1), 1, "first prediction is sequence 1");
        assert_eq!(seq(&d2), 2, "sequence pre-increments per use");
    }

    #[test]
    fn pillar_axis_follows_the_clicked_face() {
        let up = resolve_state(BlockFace::Up, 0.5, Rotation { yaw: 0.0, pitch: 0.0 }, OrientationKind::Pillar);
        assert_eq!(up.axis, Some(Axis::Y));
        let east = resolve_state(BlockFace::East, 0.5, Rotation { yaw: 0.0, pitch: 0.0 }, OrientationKind::Pillar);
        assert_eq!(east.axis, Some(Axis::X));
        let north = resolve_state(BlockFace::North, 0.5, Rotation { yaw: 0.0, pitch: 0.0 }, OrientationKind::Pillar);
        assert_eq!(north.axis, Some(Axis::Z));
    }

    #[test]
    fn slab_half_from_face_and_hit() {
        let r = Rotation { yaw: 0.0, pitch: 0.0 };
        // Top face -> always bottom slab.
        assert_eq!(
            resolve_state(BlockFace::Up, 0.9, r, OrientationKind::Slab).half,
            Some(Half::Bottom)
        );
        // Bottom face -> always top slab.
        assert_eq!(
            resolve_state(BlockFace::Down, 0.1, r, OrientationKind::Slab).half,
            Some(Half::Top)
        );
        // Side face, lower hit -> bottom; upper hit -> top.
        assert_eq!(
            resolve_state(BlockFace::North, 0.25, r, OrientationKind::Slab).half,
            Some(Half::Bottom)
        );
        assert_eq!(
            resolve_state(BlockFace::North, 0.75, r, OrientationKind::Slab).half,
            Some(Half::Top)
        );
    }

    #[test]
    fn horizontal_facing_from_yaw() {
        // fromYRot: 0=south, 90=west, 180=north, 270=east.
        assert_eq!(horizontal_from_yaw(0.0), BlockFace::South);
        assert_eq!(horizontal_from_yaw(90.0), BlockFace::West);
        assert_eq!(horizontal_from_yaw(180.0), BlockFace::North);
        assert_eq!(horizontal_from_yaw(-90.0), BlockFace::East);
        // Furnace faces the opposite of the player look.
        let s = resolve_state(
            BlockFace::North,
            0.5,
            Rotation { yaw: 0.0, pitch: 0.0 },
            OrientationKind::FacingHorizontalOpposite,
        );
        assert_eq!(s.facing, Some(BlockFace::North));
    }

    #[test]
    fn facing_all_is_opposite_the_nearest_look() {
        // Looking straight down: nearest face is DOWN, so a dispenser faces UP.
        let down = Rotation { yaw: 0.0, pitch: 90.0 };
        assert_eq!(ordered_by_nearest(down)[0], BlockFace::Down);
        let s = resolve_state(BlockFace::Up, 0.5, down, OrientationKind::FacingAll);
        assert_eq!(s.facing, Some(BlockFace::Up));
        // Looking straight up: nearest is UP, dispenser faces DOWN.
        let up = Rotation { yaw: 0.0, pitch: -90.0 };
        assert_eq!(ordered_by_nearest(up)[0], BlockFace::Up);
    }

    #[test]
    fn ordered_by_nearest_is_a_permutation_of_all_six() {
        for &(yaw, pitch) in &[(0.0, 0.0), (45.0, 30.0), (200.0, -60.0), (-130.0, 15.0)] {
            let arr = ordered_by_nearest(Rotation { yaw, pitch });
            let mut seen = arr.to_vec();
            seen.sort_by_key(|f| face_offset(*f));
            seen.dedup();
            assert_eq!(seen.len(), 6, "all six faces appear exactly once");
        }
    }

    #[test]
    fn stairs_resolve_facing_and_half() {
        let s = resolve_state(
            BlockFace::North,
            0.75,
            Rotation { yaw: 0.0, pitch: 0.0 },
            OrientationKind::Stairs,
        );
        assert_eq!(s.facing, Some(BlockFace::South));
        assert_eq!(s.half, Some(Half::Top));
    }

    #[test]
    fn acknowledge_clears_predictions_up_to_the_sequence() {
        let world = FakeWorld {
            replaceable: vec![
                BlockPos::new(0, 65, 0),
                BlockPos::new(1, 65, 0),
                BlockPos::new(2, 65, 0),
            ],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        m.use_on(&ctx(BlockPos::new(0, 64, 0), BlockFace::Up), &world);
        m.use_on(&ctx(BlockPos::new(1, 64, 0), BlockFace::Up), &world);
        m.use_on(&ctx(BlockPos::new(2, 64, 0), BlockFace::Up), &world);
        assert_eq!(m.pending().len(), 3);
        let settled = m.acknowledge(2);
        assert_eq!(settled.len(), 2, "sequences 1 and 2 settle");
        assert_eq!(m.pending().len(), 1, "sequence 3 is still pending");
        assert_eq!(m.pending()[0].sequence, 3);
    }

    #[test]
    fn reconcile_flags_a_rejected_placement() {
        let ground = BlockPos::new(5, 64, 5);
        let above = BlockPos::new(5, 65, 5);
        let world = FakeWorld {
            replaceable: vec![above],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        let d = m.use_on(&ctx(ground, BlockFace::Up), &world);
        assert!(matches!(d, UseOnDecision::Place { .. }));
        // Server refused it: the position is still air.
        let r = m.reconcile(above, None);
        assert!(r.corrected, "a rejected placement must reconcile as corrected");
        assert!(m.pending().is_empty());
    }

    #[test]
    fn reconcile_accepts_a_matching_placement() {
        let ground = BlockPos::new(6, 64, 6);
        let above = BlockPos::new(6, 65, 6);
        let world = FakeWorld {
            replaceable: vec![above],
            ..FakeWorld::default()
        };
        let mut m = Placement::new();
        m.use_on(&ctx(ground, BlockFace::Up), &world);
        // Server confirms stone at the predicted position.
        let stone = id("stone");
        let r = m.reconcile(above, Some(&stone));
        assert!(!r.corrected, "a confirmed placement is not a correction");
    }
}
