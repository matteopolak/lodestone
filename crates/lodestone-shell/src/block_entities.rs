//! The shell's block-entity source: turns the client-owned world's decoded
//! block-entity records into the render crate's [`ChestSpawn`]s, and owns the
//! chest-lid animation state that no other layer has anywhere to put.
//!
//! This is the **consumer end** of a chain that already existed and reached
//! nothing. Before this module the chain stopped one hop short of
//! pixels at every link:
//!
//! ```text
//! level_chunk_with_light ─► BlockEntity::decode_list  ─► LoadedChunk.block_entities
//! block_update           ─► World::sync_block_entity  ─┤   (create / keep /
//! section_blocks_update  ─► World::sync_block_entity  ─┤    replace / remove)
//! local placement        ─► World::sync_block_entity  ─┤
//! block_entity_data      ─► World::set_block_entity   ─┘
//!                                                      │  ← nothing in the shell
//!                                                      │    read this field: zero
//!                                                      │    call sites
//!                                                      ▼
//!                                          chest_spawns() ─► gpu/block_entities.rs
//! ```
//!
//! The fourth row is the client's own right-click prediction
//! ([`crate::sim::write_predicted_block`], that fix) and it is **not** a packet:
//! it is what stops a placed chest from being a hole for one server round trip.
//! See `docs/block-placement-prediction.md`.
//!
//! # There are **four** creation routes, not two
//!
//! The first version of that diagram listed only the chunk packet and
//! `block_entity_data`, which was accurate and read as exhaustive. It was not:
//! in vanilla, **writing a block state is what creates a block entity** — no
//! packet involved (26.2 `LevelChunk.java`,
//! `((EntityBlock)newBlock).newBlockEntity(pos, state)`) — and
//! `block_entity_data` is only ever data for an entity that already exists. Our
//! `block_update` / `section_blocks_update` arms wrote the state and nothing
//! else, so a freshly placed chest had a state, no record, and this module's
//! `for be in &chunk.block_entities` loop never saw it. It drew zero pixels and
//! still *opened*, because interaction resolves from the block state.
//!
//! [`lodestone_world::World::sync_block_entity`] is the fix, driven by
//! [`lodestone_data::block_entity_types`]. Its **removal** half matters as much
//! as its creation half: without it, breaking a chest would leave a stale record
//! and this module would keep drawing a chest in empty air.
//!
//! That fix wired that into the two *packet* arms only, which left the same bug on the
//! **prediction** side — the client wrote no state at all on a right-click, so a
//! chest you placed did not exist locally until `BLOCK_UPDATE` came back (issue
//! That fix). [`crate::sim::write_predicted_block`] closes it with the same pair, and
//! the removal half is what corrects a placement the server refuses.
//!
//! # Why the block-entity list is the candidate set, and the block state is the
//! truth
//!
//! Each [`lodestone_world::BlockEntity`] carries a `type_id` from the *block
//! entity type* registry and an NBT payload. This module uses **neither** to
//! decide what to draw:
//!
//! * The `type_id` does not identify the block. `minecraft:chest` and
//!   `minecraft:trapped_chest` are distinct types, but all four copper chests map
//!   to `minecraft:chest` — measured, in
//!   [`lodestone_data::block_entity_types`]' census. (That census now exists, so
//!   the older reason given here — "the shell has no block-entity-type table" —
//!   is stale as of that fix; the type is still the wrong question.)
//! * The NBT payload is `Nbt::End` for a chest the server sent no data for,
//!   which is the common case.
//!
//! What the list *is* good for is being the **set of positions worth looking
//! at** — exactly how vanilla's `BlockEntityRenderDispatcher` iterates
//! `level.getBlockEntities()` rather than scanning blocks. The appearance then
//! comes from the block state at that position, via
//! [`lodestone_data::block_states`]: the block name gives the material and the
//! `facing`/`type` properties give the rotation and half. That keeps the cost
//! O(number of block entities) instead of O(blocks in range) *and* makes the
//! block-entity decode a real dependency rather than a decorative one.
//!
//! # The lid animation lives here because nothing else can hold it
//!
//! Chest openness is not on the wire. The server sends `BLOCK_EVENT` with
//! `b0 == 1` and `b1 == viewer count` (`ChestBlockEntity.triggerEvent`, 26.2:
//! `if (b0 == 1) { this.chestLidController.shouldBeOpen(b1 > 0); }`), and the
//! *client* integrates that into an angle over the following ticks. So the
//! authoritative value is a client-side accumulator, and [`ChestLids`] is a
//! direct port of `ChestLidController`:
//!
//! * `tickLid()` ramps `openness` by **±0.1 per tick**, clamped to `0..=1`.
//! * `getOpenness(a)` is `lerp(a, oOpenness, openness)` — the *previous* tick's
//!   value interpolated toward the current one by the partial tick.
//!
//! Both halves matter. Dropping the ramp gives a lid that teleports open;
//! dropping the partial-tick lerp gives one that visibly steps at 20 Hz. The
//! ramp is tested against its exact 10-tick duration, and the lerp against the
//! midpoint of a tick, because the endpoints alone cannot tell either apart from
//! a snap.
//!
//! # How to change it
//!
//! * A second animated block entity (a bell's swing, a conduit's spin) wants its
//!   own map alongside [`ChestLids`], not a field on it — they are driven by
//!   different packets and tick with different rules.
//! * [`VIEW_DISTANCE`] is vanilla's own default and is the one number here worth
//!   keeping honest; see its doc.
//! * `chest_spawns` takes a `&SharedHandle` rather than a `&ClientHandle` so the
//!   whole thing can be moved into a `'static` render-source closure the way
//!   `Sim::outline_shape_source` does. Taking a borrow would make it
//!   uninstallable.

use std::collections::HashMap;

use glam::Vec3;
use lodestone_render::{
    BannerAttachment, BannerSpawn, BellShakeDirection, BellSpawn, ChestHalf, ChestMaterial,
    ChestSpawn, LecternSpawn,
    SHULKER_COLOURS, ShulkerFacing, ShulkerSpawn, SignKind, SignOrientation, SignSpawn,
    SkullOrientation, SkullSpawn, SkullType, horizontal_facing_clockwise_yaw,
    horizontal_facing_yaw,
};
use lodestone_render::banner_pattern::{DyeColor, StoredPatternLayer};
use lodestone_world::{ChunkPos, SignText, World};

use crate::net::{SharedHandle, entity_light_at};

/// Vanilla's per-renderer cutoff: `BlockEntityRenderer.getViewDistance()`
/// returns `64`, and `shouldRender` compares it against the distance from the
/// camera to `Vec3.atCenterOf(blockPos)` — the block **centre**, not its corner.
///
/// Ported as the real thing rather than "the render distance" because it is
/// genuinely a fixed 64 blocks in vanilla regardless of the video setting, and
/// because the `atCenterOf` offset is the difference between a chest popping in
/// at 64.0 and at 63.1.
pub const VIEW_DISTANCE: f32 = 64.0;

/// Vanilla's `ChestLidController` ramp, per tick.
const LID_SPEED: f32 = 0.1;

/// One chest's lid state — `ChestLidController`'s three fields.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Lid {
    should_be_open: bool,
    openness: f32,
    /// `oOpenness`: the value at the start of the current tick, for the
    /// partial-tick lerp.
    previous: f32,
}

/// Per-position chest lid animation state, driven by `BLOCK_EVENT` and advanced
/// once per client tick.
///
/// Keyed by absolute block position. Entries for a fully-closed, not-opening
/// chest are dropped by [`tick`](Self::tick) so the map does not grow without
/// bound as a player walks past thousands of chests — a chest at rest is
/// indistinguishable from an absent entry (both are openness `0`), which is what
/// makes that safe.
#[derive(Debug, Default, Clone)]
pub struct ChestLids {
    lids: HashMap<[i32; 3], Lid>,
}

impl ChestLids {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one `BLOCK_EVENT` to the chest at `pos`.
    ///
    /// `b0`/`b1` are the packet's two opaque parameter bytes. Only `b0 == 1` is
    /// a chest lid event; every other `b0` belongs to some other block type
    /// (a note block's pitch, a piston's direction) and is ignored here rather
    /// than at the caller, so the vanilla rule stays in one place. Returns
    /// whether the event was a lid event.
    ///
    /// `b1 > 0` is `shouldBeOpen`: vanilla sends the *viewer count*, not a
    /// boolean, and a second player opening the same chest sends `2`. Treating
    /// the byte as a boolean directly happens to work for `0`/`1` and shuts the
    /// lid the moment anyone is the second viewer — which is why the comparison
    /// is `> 0`.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        let should_be_open = b1 > 0;
        let entry = self.lids.entry(pos).or_insert(Lid {
            should_be_open,
            openness: 0.0,
            previous: 0.0,
        });
        entry.should_be_open = should_be_open;
        true
    }

    /// Advances every lid one client tick — `ChestLidController.tickLid()`.
    ///
    /// Also garbage-collects lids that are shut and staying shut.
    pub fn tick(&mut self) {
        self.lids.retain(|_, lid| {
            lid.previous = lid.openness;
            if !lid.should_be_open && lid.openness > 0.0 {
                lid.openness = (lid.openness - LID_SPEED).max(0.0);
            } else if lid.should_be_open && lid.openness < 1.0 {
                lid.openness = (lid.openness + LID_SPEED).min(1.0);
            }
            // Keep anything still moving or still open; drop the settled-shut.
            lid.should_be_open || lid.openness > 0.0 || lid.previous > 0.0
        });
    }

    /// The interpolated openness at `pos` — `ChestLidController.getOpenness(a)`,
    /// i.e. `lerp(partial_tick, oOpenness, openness)`.
    ///
    /// `0.0` for a position with no entry, which is exactly a shut chest.
    #[must_use]
    pub fn openness(&self, pos: [i32; 3], partial_tick: f32) -> f32 {
        match self.lids.get(&pos) {
            Some(lid) => lid.previous + (lid.openness - lid.previous) * partial_tick.clamp(0.0, 1.0),
            None => 0.0,
        }
    }

    /// Number of tracked lids (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.lids.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lids.is_empty()
    }
}

/// `BellBlockEntity.DURATION` — a shake runs 50 ticks and then stops
/// (`BellBlockEntity.tick`: `if (entity.ticks >= 50) { shaking = false; ticks = 0; }`).
const BELL_SHAKE_DURATION: f32 = 50.0;

/// One bell's shake — `BellBlockEntity`'s `clickDirection` plus its `ticks`
/// counter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Shake {
    direction: BellShakeDirection,
    /// `BellBlockEntity.ticks`, counted up from `0`.
    ticks: f32,
    /// The value at the start of the current tick, for the partial-tick lerp.
    previous: f32,
}

/// Per-position bell shake state, driven by `BLOCK_EVENT` and advanced once per
/// client tick — the bell sibling of [`ChestLids`].
///
/// Keyed by absolute block position, and entries are dropped once their 50-tick
/// shake finishes: a bell at rest is indistinguishable from an absent entry (both
/// give [`shake`](Self::shake) `None`), the same property that makes `ChestLids`'
/// own garbage collection safe.
#[derive(Debug, Default, Clone)]
pub struct BellShakes {
    shakes: HashMap<[i32; 3], Shake>,
}

impl BellShakes {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one `BLOCK_EVENT` to the bell at `pos`, returning whether it was a
    /// bell event.
    ///
    /// `BellBlockEntity.triggerEvent` (`:43-53`): only `b0 == 1` is a bell ring,
    /// and `b1` is `Direction.from3DDataValue(...)` — the *face the bell was hit
    /// on*, not a viewer count. A ring always restarts the animation from tick 0
    /// even mid-shake, which is why this overwrites rather than merging.
    ///
    /// **`b0 == 1` is also a chest lid event**, and that collision is real: the
    /// two are told apart by the block at `pos`, not by the packet, which is why
    /// both trackers accept the same event and the *gather* decides which of them
    /// a given position reads from. A note block's `b0` is its instrument and a
    /// piston's is its direction, so neither reaches either tracker.
    pub fn apply_block_event(&mut self, pos: [i32; 3], b0: u8, b1: u8) -> bool {
        if b0 != 1 {
            return false;
        }
        let Some(direction) = shake_direction_from_3d(b1) else {
            // `Direction.from3DDataValue` gives UP/DOWN for `0`/`1`, which
            // `BellModel.setupAnim` has no rotation for — vanilla stores it and
            // then multiplies by nothing. Dropping it here is the same picture and
            // keeps the map free of entries that can never move.
            return false;
        };
        self.shakes.insert(
            pos,
            Shake {
                direction,
                ticks: 0.0,
                previous: 0.0,
            },
        );
        true
    }

    /// Advances every shake one client tick, dropping the finished ones.
    pub fn tick(&mut self) {
        self.shakes.retain(|_, shake| {
            shake.previous = shake.ticks;
            shake.ticks += 1.0;
            shake.ticks < BELL_SHAKE_DURATION
        });
    }

    /// The shake at `pos` for this partial tick, or `None` for a bell at rest.
    ///
    /// The tick counter is interpolated because that is what
    /// `BellRenderer.extractRenderState` passes into `setupAnim` — `ticks +
    /// partialTick`, not the whole number. Interpolating matters here for the same
    /// reason it does for a chest lid: `bell_shake_angle` is a `sin` of it, so a
    /// stepped counter reads as a stutter at 60 fps.
    #[must_use]
    pub fn shake(&self, pos: [i32; 3], partial_tick: f32) -> Option<(BellShakeDirection, f32)> {
        let shake = self.shakes.get(&pos)?;
        let t = partial_tick.clamp(0.0, 1.0);
        Some((shake.direction, shake.previous + (shake.ticks - shake.previous) * t))
    }

    /// Number of bells currently shaking (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.shakes.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shakes.is_empty()
    }
}

/// `EnchantingTableBlockEntity.bookAnimationTick`'s trigger radius, in blocks:
/// `level.getNearestPlayer(x + 0.5, y + 0.5, z + 0.5, 3.0, false)`.
///
/// Measured from the block **centre** to the player's position, in three
/// dimensions — not horizontally, so a player on the floor below a table on a
/// shelf does not open its book.
const ENCHANTING_TABLE_PLAYER_RADIUS: f64 = 3.0;

/// One enchanting table's book animation — `EnchantingTableBlockEntity`'s ten
/// public animation fields, none of which are on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct Book {
    /// Vanilla's `time`, incremented once per tick and never reset.
    time: i32,
    flip: f32,
    /// `oFlip`: the value at the start of the current tick, for the partial-tick
    /// lerp.
    o_flip: f32,
    /// `flipT`: the *target* page, re-rolled at random. Unbounded, and it drifts
    /// in both directions.
    flip_t: f32,
    /// `flipA`: the page-turn velocity, itself smoothed toward the target
    /// difference at 90% per tick.
    flip_a: f32,
    open: f32,
    /// `oOpen`, for the partial-tick lerp.
    o_open: f32,
    /// `rot`, radians, wrapped into `-PI..PI`.
    rot: f32,
    /// `oRot`, for the partial-tick lerp — which must be **shortest-arc**.
    o_rot: f32,
    /// `tRot`: the angle the book is chasing. Points at the nearest player, or
    /// creeps by `0.02` rad/tick when there is nobody to look at.
    t_rot: f32,
}

/// Brings an angle into `-PI..PI`, vanilla's two `while` loops.
fn wrap_radians(mut angle: f32) -> f32 {
    const TAU: f32 = std::f32::consts::TAU;
    while angle >= std::f32::consts::PI {
        angle -= TAU;
    }
    while angle < -std::f32::consts::PI {
        angle += TAU;
    }
    angle
}

impl Book {
    /// One tick of `bookAnimationTick` for the table at `pos`.
    ///
    /// `player` is the nearest player's position, or `None` when there is none
    /// within [`ENCHANTING_TABLE_PLAYER_RADIUS`] — the caller does the radius test
    /// so this stays a pure function of its inputs.
    fn tick(&mut self, pos: [i32; 3], player: Option<glam::DVec3>, rng: &mut JavaRandom) {
        self.o_open = self.open;
        self.o_rot = self.rot;
        if let Some(player) = player {
            let xd = player.x - (f64::from(pos[0]) + 0.5);
            let zd = player.z - (f64::from(pos[2]) + 0.5);
            #[expect(
                clippy::cast_possible_truncation,
                reason = "an angle in radians; f32 is what the renderer takes"
            )]
            {
                self.t_rot = zd.atan2(xd) as f32;
            }
            self.open += 0.1;
            // `open < 0.5` makes the pages riffle *while opening* regardless of
            // the dice, and then it is a 1-in-40 chance per tick. Dropping the
            // first half leaves a book that opens dead still.
            if self.open < 0.5 || rng.next_int(40) == 0 {
                let old = self.flip_t;
                // Vanilla's `do { .. } while (old == flipT)`: the difference of
                // two `nextInt(4)` draws can be zero, and the loop **must**
                // re-roll rather than accept it. A plain `if` leaves the page
                // occasionally not turning at all when it was asked to.
                loop {
                    self.flip_t += (rng.next_int(4) - rng.next_int(4)) as f32;
                    if old != self.flip_t {
                        break;
                    }
                }
            }
        } else {
            self.t_rot += 0.02;
            self.open -= 0.1;
        }

        self.rot = wrap_radians(self.rot);
        self.t_rot = wrap_radians(self.t_rot);
        // The chase is 40% of the **wrapped** remaining arc. Without the wrap the
        // book takes the long way round whenever the angle crosses `±PI`, which
        // is a full backwards revolution in a couple of ticks and happens every
        // time a player walks past one particular corner.
        self.rot += wrap_radians(self.t_rot - self.rot) * 0.4;
        self.open = self.open.clamp(0.0, 1.0);
        self.time += 1;

        self.o_flip = self.flip;
        let diff = ((self.flip_t - self.flip) * 0.4).clamp(-0.2, 0.2);
        self.flip_a += (diff - self.flip_a) * 0.9;
        self.flip += self.flip_a;
    }

    /// Whether this entry is indistinguishable from having none: a fully shut
    /// book with no page motion left.
    ///
    /// `open == 0` makes [`lodestone_render::enchanting_table_book_openness`] zero
    /// and `book_part_poses` fold the lids flat, so a shut book renders exactly
    /// like an absent one — the same property that makes `ChestLids`' garbage
    /// collection safe. `flip` is not checked because a shut book's pages are
    /// inside it.
    fn is_rested(&self) -> bool {
        self.open == 0.0 && self.o_open == 0.0
    }
}

/// `java.util.Random`, whose algorithm is specified in its own documentation —
/// a 48-bit truncated linear congruential generator.
///
/// Ported rather than pulled in as a dependency (neither `lodestone-shell` nor
/// `lodestone-render` has an RNG crate) and ported *exactly* rather than
/// approximated, because it is 12 lines and because `next_int`'s two branches are
/// not interchangeable: a power-of-two bound is a multiply-and-shift and every
/// other bound is a **rejection loop**. `nextInt(4)` takes the first path and
/// `nextInt(40)` the second, and this animation uses both.
#[derive(Debug, Clone, Copy, PartialEq)]
struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    const MULTIPLIER: u64 = 0x5DEEC_E66D;
    const ADDEND: u64 = 0xB;
    const MASK: u64 = (1 << 48) - 1;

    /// `new Random(seed)` — the constructor scrambles the seed.
    fn new(seed: u64) -> Self {
        JavaRandom {
            seed: (seed ^ Self::MULTIPLIER) & Self::MASK,
        }
    }

    /// `next(bits)`.
    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self.seed.wrapping_mul(Self::MULTIPLIER).wrapping_add(Self::ADDEND)
            & Self::MASK;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the shift leaves at most 32 significant bits, which is what next(bits) returns"
        )]
        {
            (self.seed >> (48 - bits)) as i32
        }
    }

    /// `nextInt(bound)`, `bound > 0`.
    fn next_int(&mut self, bound: i32) -> i32 {
        debug_assert!(bound > 0);
        if bound & (bound - 1) == 0 {
            // Power of two: `(bound * next(31)) >> 31`, in 64-bit arithmetic.
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let value = bits % bound;
            // Vanilla's overflow guard, kept because it is the *whole* difference
            // between this and a plain modulo: it rejects the tail that would bias
            // low values.
            if bits.wrapping_sub(value).wrapping_add(bound - 1) >= 0 {
                return value;
            }
        }
    }
}

/// Per-position enchanting-table book animation state, advanced once per client
/// tick — the third animation fold beside [`ChestLids`] and [`BellShakes`], and
/// the first with **no packet driving it at all**.
///
/// Chest lids and bell shakes are both started by a `BLOCK_EVENT`; this one is
/// started by the player *standing near a block*, so nothing on the wire would
/// ever reveal that it had stopped working. Entries are created for tables within
/// [`ENCHANTING_TABLE_PLAYER_RADIUS`] and dropped once fully shut, which is safe
/// for [`Book::is_rested`]'s reason.
#[derive(Debug, Clone)]
pub struct EnchantingTableBooks {
    books: HashMap<[i32; 3], Book>,
    rng: JavaRandom,
}

impl Default for EnchantingTableBooks {
    fn default() -> Self {
        Self::new()
    }
}

impl EnchantingTableBooks {
    /// An empty set.
    ///
    /// The RNG seed is a fixed constant rather than a clock read, for the reason
    /// `docs/`'s evidence rules give: a test that seeds from the wall clock cannot
    /// be reproduced, and nothing about a page-flip phase benefits from being
    /// unpredictable across runs. Vanilla's own `RandomSource.create()` is
    /// time-seeded, but it is a *shared static* across every enchanting table in
    /// the world, which is the property that actually matters and which this keeps.
    #[must_use]
    pub fn new() -> Self {
        EnchantingTableBooks {
            books: HashMap::new(),
            rng: JavaRandom::new(0x1BADB002),
        }
    }

    /// Advances every tracked book one client tick, creating entries for the
    /// tables in `tables` that a player is close enough to wake.
    ///
    /// `tables` is every enchanting-table position worth considering (the caller
    /// gathers it — see [`enchanting_table_positions`]) and `player` is the local
    /// player's position.
    ///
    /// # Only the local player
    ///
    /// Vanilla asks the level for the *nearest* player, which on a busy server can
    /// be someone else. We use the local player, which is the one case that
    /// matters for what this client's user sees and the only position this layer
    /// has cheaply. A remote player standing at a table the local player can see
    /// therefore leaves its book shut — a fidelity gap, recorded rather than
    /// silently taken, and closing it means scanning tracked player entities here.
    pub fn tick(&mut self, tables: &[[i32; 3]], player: glam::DVec3) {
        let radius_squared = ENCHANTING_TABLE_PLAYER_RADIUS * ENCHANTING_TABLE_PLAYER_RADIUS;
        for pos in tables {
            let centre = glam::DVec3::new(
                f64::from(pos[0]) + 0.5,
                f64::from(pos[1]) + 0.5,
                f64::from(pos[2]) + 0.5,
            );
            if centre.distance_squared(player) <= radius_squared {
                self.books.entry(*pos).or_default();
            }
        }
        // Borrowed separately from `self.rng` because the tick draws from it.
        let rng = &mut self.rng;
        self.books.retain(|pos, book| {
            let centre = glam::DVec3::new(
                f64::from(pos[0]) + 0.5,
                f64::from(pos[1]) + 0.5,
                f64::from(pos[2]) + 0.5,
            );
            let near = (centre.distance_squared(player) <= radius_squared).then_some(player);
            book.tick(*pos, near, rng);
            !book.is_rested()
        });
    }

    /// This frame's interpolated animation state for the table at `pos`, or `None`
    /// when there is no entry — which is a fully shut book, i.e. nothing to draw.
    ///
    /// Returns `(y_rot, time, open, flip)` ready for
    /// [`lodestone_render::EnchantingTableSpawn`]. The `y_rot` lerp is
    /// **shortest-arc**, matching `EnchantTableRenderer.extractRenderState`'s three
    /// `while` loops rather than a plain `lerp`.
    #[must_use]
    pub fn state(&self, pos: [i32; 3], partial_tick: f32) -> Option<(f32, f32, f32, f32)> {
        let book = self.books.get(&pos)?;
        let alpha = partial_tick.clamp(0.0, 1.0);
        let y_rot = book.o_rot + wrap_radians(book.rot - book.o_rot) * alpha;
        #[expect(
            clippy::cast_precision_loss,
            reason = "vanilla's own `blockEntity.time + partialTicks` is a float add"
        )]
        let time = book.time as f32 + alpha;
        Some((
            y_rot,
            time,
            book.o_open + (book.open - book.o_open) * alpha,
            book.o_flip + (book.flip - book.o_flip) * alpha,
        ))
    }

    /// Number of tracked books (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.books.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }
}

/// `Direction.from3DDataValue(b1)`, narrowed to the four horizontal directions
/// [`BellShakeDirection`] models: `0` down, `1` up, `2` north, `3` south, `4`
/// west, `5` east.
///
/// **The order is the jar's, not alphabetical and not `BellShakeDirection`'s own
/// declaration order** — getting it wrong swings the bell along the wrong axis,
/// which still looks like a working animation.
fn shake_direction_from_3d(b1: u8) -> Option<BellShakeDirection> {
    match b1 {
        2 => Some(BellShakeDirection::North),
        3 => Some(BellShakeDirection::South),
        4 => Some(BellShakeDirection::West),
        5 => Some(BellShakeDirection::East),
        _ => None,
    }
}

/// Reads one block state's `facing`/`type` properties into the chest fields the
/// renderer needs.
///
/// Returns `None` when the state has no `facing` — which for a chest cannot
/// happen, and for anything else means the caller pointed this at a block that
/// is not a chest.
#[must_use]
fn chest_orientation(state_id: u32) -> Option<(f32, ChestHalf)> {
    let props = lodestone_data::block_states::properties(state_id)?;
    let mut yaw = None;
    let mut half = ChestHalf::Single;
    for (name, value) in props {
        match *name {
            "facing" => yaw = horizontal_facing_yaw(value),
            "type" => half = ChestHalf::parse(value),
            _ => {}
        }
    }
    // An ender chest has `facing` but no `type`, and that is correct: it is
    // always single. A missing `facing` is the real failure and must not
    // silently become south — a wall of chests all facing one way is much harder
    // to spot as a bug than a chest that does not draw.
    Some((yaw?, half))
}

/// Resolves one block state id into a chest material, or `None` if it is not a
/// chest at all.
#[must_use]
fn chest_material(state_id: u32) -> Option<ChestMaterial> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    ChestMaterial::from_block_path(path)
}

/// Every block-entity position within [`VIEW_DISTANCE`] of `eye`, paired with the
/// block state actually at it — the candidate set [`chest_spawns`] filters.
///
/// Split out of [`chest_spawns`] so a gate can drive the real gather against a
/// real [`World`] without a live `ClientHandle`: this is the loop that reads
/// `chunk.block_entities`, and therefore the loop that saw nothing at all before
/// That fix was fixed. Everything `chest_spawns` adds on top of this and
/// [`chest_spawn`] is lock handling and a light sample.
#[must_use]
pub fn chest_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            // Vanilla's `shouldRender`: distance from the camera to the block
            // *centre*, not its corner, against a flat 64.
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            // `rel_x`/`rel_z` are section-relative and `y` absolute, which is
            // exactly `ChunkColumn::get_block`'s signature — no conversion, and
            // no second lookup through a position that would have to be re-split.
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id));
        }
    }
    candidates
}

/// One candidate resolved into a [`ChestSpawn`], or `None` if the state at that
/// position is not a chest.
///
/// The block **state** is the truth about appearance (see the module docs); the
/// block-entity record only says the position is worth looking at. So a stale or
/// orphan record whose state is not a chest resolves to `None` here and draws
/// nothing — which is what makes `block_entity_data`'s create-on-miss fallback
/// inert rather than a way to paint phantom chests.
#[must_use]
pub fn chest_spawn(
    block: [i32; 3],
    state_id: u32,
    openness: f32,
    light: u8,
) -> Option<ChestSpawn> {
    let material = chest_material(state_id)?;
    let (facing_yaw_deg, half) = chest_orientation(state_id)?;
    Some(ChestSpawn {
        pos: block,
        facing_yaw_deg,
        half,
        material,
        openness,
        light,
    })
}

/// Every chest to draw this frame, gathered from the client-owned world's
/// block-entity records.
///
/// `eye` is the camera position and `partial_tick` the fraction through the
/// current client tick (`0..=1`) used to interpolate the lid. Returns an empty
/// vec before login, or when the handle has no world dimensions yet — never a
/// panic, for the same reason [`entity_light_at`] returns `None` rather than
/// darkness.
///
/// # Ordering, and why it is sorted
///
/// The output is sorted by position. `HashMap` iteration order over chunks is
/// non-deterministic per process, and an unsorted list makes the instance order
/// inside a batch differ run to run — which turns any pixel gate that reads back
/// a frame into a flaky one for reasons that look like a GPU problem.
#[must_use]
pub fn chest_spawns(
    handle: &SharedHandle,
    lids: &ChestLids,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<ChestSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let mut out = Vec::new();

    // `loaded_chunks()` takes the world's read lock itself, so it is called
    // *before* the guard below rather than inside it. `std::sync::RwLock` gives
    // no re-entrancy guarantee — a nested read is allowed to deadlock once a
    // writer is queued, which on this world happens every time a chunk packet
    // lands. Taking it twice would produce a hang under load and never in a test.
    let chunks = client.loaded_chunks();

    // Then one read lock for the whole gather. The guard is dropped before the
    // light-sampling loop below, for exactly the same reason:
    // `entity_light_at` reaches for the same lock.
    let candidates = {
        let world = store.read();
        // `loaded_chunks` speaks `lodestone_model::ChunkPos`; the world is keyed by
        // `lodestone_world::ChunkPos`. Same two fields, distinct types.
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    // Resolved once for the whole frame, from the `client` this function already
    // holds, rather than per chest: `player()` clones a snapshot behind an ECS read
    // lock, and the loop below runs once per visible chest. The point samplers on
    // the render thread read a shared cell instead because they are `'static`
    // closures with no per-frame value available — see `net::SkyDefaultCell`.
    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    for (block, state_id) in candidates {
        // The light sample is the only thing here that needs the handle, which is
        // why it is the only thing `chest_candidates`/`chest_spawn` do not cover.
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) =
            chest_spawn(block, state_id, lids.openness(block, partial_tick), light)
        {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Reads a skull/head block state's orientation — `rotation` (`0..16`, floor
/// placement) or `facing` (wall placement) — into the renderer's fields.
///
/// A real skull state carries exactly one of the two (see
/// `assets/minecraft/blockstates/skeleton_skull.json` vs
/// `.../skeleton_wall_skull.json` in the real jar): floor skulls have
/// `rotation`, wall skulls have `facing`. `None` for a state with neither,
/// which cannot happen for a real skull and for anything else means the
/// caller pointed this at a block that is not one.
#[must_use]
fn skull_orientation(state_id: u32) -> Option<SkullOrientation> {
    let props = lodestone_data::block_states::properties(state_id)?;
    for (name, value) in props {
        match *name {
            "rotation" => {
                return value
                    .parse::<u8>()
                    .ok()
                    .map(|rotation_segment| SkullOrientation::Floor { rotation_segment });
            }
            "facing" => {
                return horizontal_facing_yaw(value)
                    .map(|facing_yaw_deg| SkullOrientation::Wall { facing_yaw_deg });
            }
            _ => {}
        }
    }
    None
}

/// Resolves one block state id into a skull/head type, or `None` if it is not
/// one of the five ported types (see
/// [`lodestone_render::SkullType::from_block_path`] for what is declined) —
/// including not being a skull at all.
#[must_use]
fn skull_type_for_state(state_id: u32) -> Option<SkullType> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    SkullType::from_block_path(path)
}

/// One candidate resolved into a [`SkullSpawn`], or `None` if the state at
/// that position is not a ported skull type.
///
/// Same shape as [`chest_spawn`]: the block **state** is the truth about
/// appearance, the block-entity record only says the position is worth
/// looking at, so a stale or orphan record whose state is not a skull draws
/// nothing.
#[must_use]
pub fn skull_spawn(block: [i32; 3], state_id: u32, light: u8) -> Option<SkullSpawn> {
    let skull_type = skull_type_for_state(state_id)?;
    let orientation = skull_orientation(state_id)?;
    Some(SkullSpawn {
        pos: block,
        orientation,
        skull_type,
        light,
    })
}

/// Every skull/head to draw this frame, gathered from the client-owned
/// world's block-entity records.
///
/// Reuses [`chest_candidates`] rather than a second scan of
/// `chunk.block_entities`: that gather is already generic over block-entity
/// *type* (it filters by distance and returns the raw state id, never
/// touching anything chest-specific), so a second copy here would only be
/// able to drift from it. Everything this adds on top is skull-specific
/// resolution and the light sample, the same division [`chest_spawns`] keeps.
///
/// No lid-style animation state: none of the five ported skull types pose
/// their head (see [`lodestone_render::BlockEntityModelSet::resolve_skull`]'s
/// doc), so there is nothing here to tick.
#[must_use]
pub fn skull_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<SkullSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    // Same lock-ordering rule as `chest_spawns`: `loaded_chunks()` takes its
    // own read lock, so it must not be called from inside the guard below.
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = skull_spawn(block, state_id, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Resolves one block state id into whether it names a bell — `None` for
/// anything else. Unlike [`chest_material`]/[`skull_type_for_state`] there is
/// no per-block-path variant to select: every bell block state (any
/// `FACING`/`ATTACHMENT`/`POWERED` combination) draws the identical rig, so
/// this only needs to confirm the block *is* one.
#[must_use]
fn bell_is_present(state_id: u32) -> bool {
    let Some(name) = lodestone_data::block_states::block_name(state_id) else {
        return false;
    };
    name == "minecraft:bell"
}

/// One candidate resolved into a [`BellSpawn`], or `None` if the state at
/// that position is not a bell. Same shape as [`chest_spawn`]/[`skull_spawn`]:
/// the block **state** is the truth about whether this is a bell at all, so a
/// stale or orphan record whose state is not a bell draws nothing.
///
/// `shake` comes from [`BellShakes`], the `BLOCK_EVENT`-driven tracker — `None`
/// for a bell at rest, which is every bell until one is rung.
#[must_use]
pub fn bell_spawn(
    block: [i32; 3],
    state_id: u32,
    light: u8,
    shakes: &BellShakes,
    partial_tick: f32,
) -> Option<BellSpawn> {
    if !bell_is_present(state_id) {
        return None;
    }
    Some(BellSpawn {
        pos: block,
        shake: shakes.shake(block, partial_tick),
        light,
    })
}

/// Every bell to draw this frame, gathered from the client-owned world's
/// block-entity records. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`] does, for the same reason: that gather is already generic
/// over block-entity type.
#[must_use]
pub fn bell_spawns(
    handle: &SharedHandle,
    shakes: &BellShakes,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<BellSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = bell_spawn(block, state_id, light, shakes, partial_tick) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Resolves a block state id into `(dye colour, facing)` for a shulker box, or
/// `None` if the state is not one.
///
/// **The colour is the block *id*, not a property and not NBT.** Vanilla has
/// seventeen shulker-box blocks (`shulker_box` plus one per dye), so
/// `minecraft:red_shulker_box` → `Some("red")` and the plain `minecraft:shulker_box`
/// → `None`, which is the undyed sheet. Reading a `color` property here would find
/// nothing and draw every box undyed.
///
/// `facing` defaults to [`ShulkerFacing::Up`] when the property is missing, which
/// is `ShulkerBoxRenderer.extractRenderState`'s own `getValueOrElse(FACING, UP)`
/// — unlike a chest, where a missing `facing` is treated as a failure, because a
/// shulker box genuinely has a sensible default and vanilla uses it.
#[must_use]
fn shulker_orientation(state_id: u32) -> Option<(Option<&'static str>, ShulkerFacing)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    let colour = if path == "shulker_box" {
        None
    } else {
        // `SHULKER_COLOURS`' own entries, so the returned `&'static str` is the
        // one `shulker_texture_stem` matches against — a `&path[..]` slice would
        // not outlive this call.
        let stem = path.strip_suffix("_shulker_box")?;
        Some(*SHULKER_COLOURS.iter().find(|c| **c == stem)?)
    };
    let mut facing = ShulkerFacing::Up;
    if let Some(props) = lodestone_data::block_states::properties(state_id) {
        for (name, value) in props {
            if *name == "facing"
                && let Some(parsed) = ShulkerFacing::from_name(value)
            {
                facing = parsed;
            }
        }
    }
    Some((colour, facing))
}

/// One candidate resolved into a [`ShulkerSpawn`], or `None` if the state at that
/// position is not a shulker box.
///
/// `progress` is fixed at `0.0` — closed. See [`ShulkerSpawn::progress`] for why
/// that is the honest value rather than a placeholder.
#[must_use]
pub fn shulker_spawn(block: [i32; 3], state_id: u32, light: u8) -> Option<ShulkerSpawn> {
    let (colour, facing) = shulker_orientation(state_id)?;
    Some(ShulkerSpawn {
        pos: block,
        facing,
        colour,
        progress: 0.0,
        light,
    })
}

/// Every shulker box to draw this frame. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`] and [`bell_spawns`] do.
#[must_use]
pub fn shulker_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<ShulkerSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    // The guard is taken and dropped *inside* this block, before the light
    // sampling below — the no-nested-read-lock rule every gather here follows.
    let candidates = {
        let world = store.read();
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = shulker_spawn(block, state_id, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// One candidate resolved into a [`LecternSpawn`], or `None` if the state at
/// that position is not a lectern **with a book in it**.
///
/// Two `None` cases that are both correct and mean different things:
///
/// * The block is not a lectern. Same shape as every other gather here — the
///   block *state* is the truth, so a stale record draws nothing.
/// * The block is a lectern with `has_book=false`. There is genuinely nothing to
///   draw: a lectern's shelf, base and posts are all real block models
///   (`block/lectern.json` has geometry, unlike `chest.json`), so an empty
///   lectern is complete without this pass. Only the open book is missing, and
///   only when a book is in it. That also means an unwired lectern source
///   degrades to "no books on lecterns", not to a hole in the world.
///
/// `facing_yaw_deg` goes through [`horizontal_facing_clockwise_yaw`] and not
/// [`horizontal_facing_yaw`]: `LecternRenderer.extractRenderState` stores
/// `FACING.getClockWise().toYRot()`, and the plain facing lays the book across
/// the shelf at right angles to the reader.
#[must_use]
pub fn lectern_spawn(block: [i32; 3], state_id: u32, light: u8) -> Option<LecternSpawn> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:lectern" {
        return None;
    }
    let props = lodestone_data::block_states::properties(state_id)?;
    let mut yaw = None;
    let mut has_book = false;
    for (name, value) in props {
        match *name {
            "facing" => yaw = horizontal_facing_clockwise_yaw(value),
            "has_book" => has_book = *value == "true",
            _ => {}
        }
    }
    if !has_book {
        return None;
    }
    Some(LecternSpawn {
        pos: block,
        facing_yaw_deg: yaw?,
        light,
    })
}

/// Every lectern book to draw this frame. Reuses [`chest_candidates`] exactly as
/// [`skull_spawns`], [`bell_spawns`] and [`shulker_spawns`] do.
#[must_use]
pub fn lectern_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<LecternSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    // The guard is taken and dropped inside this block, before the light
    // sampling below — the no-nested-read-lock rule every gather here follows.
    let candidates = {
        let world = store.read();
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = lectern_spawn(block, state_id, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// Whether a block state is an enchanting table.
///
/// One block, no properties that matter: an enchanting table has **no `facing`**
/// and no other state at all in 26.2. That absence is load-bearing — the book's
/// angle is client-simulated (`EnchantingTableBlockEntity.rot`), so there is
/// nothing on the block state a facing could have been read from, and a port that
/// looks for one finds nothing and draws no book.
#[must_use]
fn is_enchanting_table(state_id: u32) -> bool {
    lodestone_data::block_states::block_name(state_id) == Some("minecraft:enchanting_table")
}

/// Every enchanting-table position worth ticking, within `radius` blocks of
/// `player`.
///
/// A much tighter cutoff than [`VIEW_DISTANCE`] on purpose: this runs at 20 Hz,
/// and the only thing that *starts* an animation is a player within
/// [`ENCHANTING_TABLE_PLAYER_RADIUS`]. A table further away than that can only
/// ever be shut, and [`EnchantingTableBooks::tick`] keeps ticking entries it
/// already has regardless of this list, so a table the player walks away from
/// still closes properly.
#[must_use]
pub fn enchanting_table_positions(
    handle: &SharedHandle,
    player: glam::DVec3,
    radius: f64,
) -> Vec<[i32; 3]> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let cutoff = radius * radius;
    let world = store.read();
    let mut out = Vec::new();
    for pos in chunks {
        let pos = ChunkPos { x: pos.x, z: pos.z };
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre =
                glam::DVec3::new(f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5);
            if centre.distance_squared(player) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            if is_enchanting_table(state_id) {
                out.push([x, y, z]);
            }
        }
    }
    out
}

/// Every enchanting-table book to draw this frame.
///
/// Unlike every other gather here the *appearance* comes from `books` rather than
/// from the world: the block state says only "there is a table", and all four
/// animated values are client-simulated. A table with no entry in `books` has a
/// fully shut book, which renders identically to no book, so it is skipped.
///
/// Reuses [`chest_candidates`] exactly as [`lectern_spawns`] does — the block
/// state is still what confirms the block entity is a table.
#[must_use]
pub fn enchanting_table_spawns(
    handle: &SharedHandle,
    books: &EnchantingTableBooks,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<lodestone_render::EnchantingTableSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        chest_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id) in candidates {
        if !is_enchanting_table(state_id) {
            continue;
        }
        let Some((y_rot, time, open, flip)) = books.state(block, partial_tick) else {
            continue;
        };
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        out.push(lodestone_render::EnchantingTableSpawn {
            pos: block,
            y_rot,
            time,
            open,
            flip,
            light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// The `facing` yaw of a campfire block, or `None` for any other block.
///
/// Both campfire blocks count: `soul_campfire` has the identical block entity,
/// the identical four cooking slots and the identical renderer registration — the
/// only difference is the flame's colour, which lives in the *block* model and
/// therefore nowhere near this path.
#[must_use]
fn campfire_facing_yaw(state_id: u32) -> Option<f32> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    if name != "minecraft:campfire" && name != "minecraft:soul_campfire" {
        return None;
    }
    let props = lodestone_data::block_states::properties(state_id)?;
    props
        .iter()
        .find(|(name, _)| *name == "facing")
        .and_then(|(_, value)| horizontal_facing_yaw(value))
}

/// The occupied cooking slots in a campfire's NBT, as `(slot, item id)`.
///
/// `ContainerHelper.saveAllItems` writes an `Items` list of
/// `ItemStackWithSlot.CODEC`, i.e. `{Slot: <unsigned byte>, id: <item id>,
/// count: <int>}` — so the slot is an explicit field and **the list index is not
/// the slot**. A campfire holding one steak in its third slot writes a
/// single-element list with `Slot: 2`; reading the index instead would cook it in
/// the wrong corner, and with a full campfire the two agree, so the bug hides
/// until a partial one.
///
/// `count` is not read: `CampfireRenderer` draws one copy per slot regardless
/// (a campfire slot holds at most one item anyway).
///
/// An entry whose `Slot` is out of range is dropped rather than clamped, matching
/// `ItemStackWithSlot.isValidInContainer`.
#[must_use]
fn campfire_items(nbt: &lodestone_core::Nbt) -> Vec<(usize, lodestone_assets::ResourceLocation)> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return Vec::new();
    };
    let Some(Nbt::List { elements, .. }) =
        fields.iter().find(|(name, _)| name == "Items").map(|(_, v)| v)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|entry| {
            let Nbt::Compound(entry) = entry else {
                return None;
            };
            let field = |key: &str| entry.iter().find(|(name, _)| name == key).map(|(_, v)| v);
            // `ExtraCodecs.UNSIGNED_BYTE` — an `Nbt::Byte`, not an int.
            let slot = match field("Slot") {
                Some(Nbt::Byte(slot)) => usize::try_from(*slot).ok()?,
                // Vanilla's `optionalAlwaysPresentFieldOf(.., "Slot", 0)`
                // defaults a missing slot to zero rather than dropping the item.
                None => 0,
                _ => return None,
            };
            if slot >= lodestone_render::CAMPFIRE_SLOTS {
                return None;
            }
            let Some(Nbt::String(id)) = field("id") else {
                return None;
            };
            Some((slot, id.parse().ok()?))
        })
        .collect()
}

/// Every campfire position within [`VIEW_DISTANCE`], paired with its block state
/// and stored item list.
///
/// A third NBT-reading candidate gather beside [`sign_candidates`] and
/// [`banner_candidates`], for the same reason both of those exist:
/// [`chest_candidates`] discards `be.nbt`, and a campfire's *entire* appearance
/// from this renderer's point of view is in there — the fire and the logs are
/// block-model geometry the terrain mesher already draws.
#[must_use]
fn campfire_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, Vec<(usize, lodestone_assets::ResourceLocation)>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            // The NBT parse is behind the block-name test, unlike the banner
            // gather's: every block entity in range would otherwise walk its
            // `Items` list, and chests and shulker boxes both have one.
            if campfire_facing_yaw(state_id).is_none() {
                continue;
            }
            candidates.push(([x, y, z], state_id, campfire_items(&be.nbt)));
        }
    }
    candidates
}

/// Every campfire cooking item to draw this frame — one
/// [`CampfireItemSpawn`](lodestone_render::CampfireItemSpawn) per **occupied**
/// slot, so a lit but empty campfire yields none.
///
/// Unlike every other gather in this module this feeds the *model* pipeline
/// rather than the entity one: `CampfireRenderer` owns no mesh and no sheet, only
/// four item poses. See `lodestone_render::campfire_item_matrix`.
///
/// No clock and no partial tick — vanilla's `CampfireRenderer` has no animation
/// at all (the flame flicker is the block model's animated texture, and the
/// `CookingTimes` in the NBT drive nothing on the client). Installed per frame
/// anyway, for `Sim::skull_source`'s reason.
#[must_use]
pub fn campfire_spawns(
    handle: &SharedHandle,
    eye: Vec3,
) -> Vec<lodestone_render::CampfireItemSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        campfire_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, items) in candidates {
        let Some(facing_yaw_deg) = campfire_facing_yaw(state_id) else {
            continue;
        };
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        for (slot, item) in items {
            out.push(lodestone_render::CampfireItemSpawn {
                pos: block,
                facing_yaw_deg,
                slot,
                item,
                light,
            });
        }
    }
    out.sort_by_key(|s| (s.pos, s.slot));
    out
}

/// Resolves a block's registry path into which of vanilla's two sign
/// renderers applies — `None` for anything that is not a sign at all.
///
/// Every sign block path ends in `_sign`, including both wall variants
/// (`oak_wall_sign`, `oak_wall_hanging_sign`); a hanging one always contains
/// `hanging` (`oak_hanging_sign`, `oak_wall_hanging_sign`) — checked first,
/// since the two families share that `_sign` suffix and their text
/// transforms differ (see [`SignKind`]).
///
/// **This used to return a bool and decline hanging signs outright**, on the
/// recorded belief that they needed "a different model set again (chains, a
/// bar)". They do not: 26.2's `HangingSignRenderer` declares no model, and
/// the chains and bar are real block-model geometry the terrain mesher
/// already draws. See [`SignKind`]'s own doc for the measurement.
#[must_use]
fn sign_kind_for_path(path: &str) -> Option<SignKind> {
    if !path.ends_with("_sign") {
        return None;
    }
    Some(if path.contains("hanging") {
        SignKind::Hanging
    } else {
        SignKind::Plain
    })
}

/// Resolves one block state id into which sign renderer it uses — `None` for
/// anything that is not a sign (see [`sign_kind_for_path`]).
#[must_use]
fn sign_kind_for_state(state_id: u32) -> Option<SignKind> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    sign_kind_for_path(path)
}

/// Reads a plain sign's placement — `rotation` (`0..16`, ground) or `facing`
/// (wall) — into [`SignOrientation`]. Mirrors [`skull_orientation`] exactly:
/// a real sign state carries exactly one of the two (`oak_sign.json` has
/// `rotation`, `oak_wall_sign.json` has `facing`), and `None` for a state
/// with neither cannot happen for a real sign.
#[must_use]
fn sign_orientation(state_id: u32) -> Option<SignOrientation> {
    let props = lodestone_data::block_states::properties(state_id)?;
    for (name, value) in props {
        match *name {
            "rotation" => {
                return value
                    .parse::<u8>()
                    .ok()
                    .map(|rotation_segment| SignOrientation::Ground { rotation_segment });
            }
            "facing" => {
                return horizontal_facing_yaw(value)
                    .map(|facing_yaw_deg| SignOrientation::Wall { facing_yaw_deg });
            }
            _ => {}
        }
    }
    None
}

/// Every sign block-entity position within [`VIEW_DISTANCE`], paired with its
/// block state **and** typed text — the one candidate gather in this module
/// that needs the NBT half of a [`lodestone_world::BlockEntity`], because
/// sign text lives there and nowhere else (see
/// `docs/block-entity-renderers.md`'s Sign section for the captured wire
/// shape). [`chest_candidates`] cannot be reused here: it deliberately
/// discards `be.nbt` because neither chest nor skull reads it, and widening
/// its return type would ripple through both of those working, tested
/// gathers for a field only this caller needs. The NBT is parsed into
/// [`SignText`] right here rather than threaded further as a raw
/// [`lodestone_core::Nbt`] — nothing downstream wants the untyped form.
#[must_use]
fn sign_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, SignText)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id, SignText::parse(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`SignSpawn`], or `None` if the state at
/// that position is not a sign. Same shape as [`chest_spawn`]/
/// [`skull_spawn`]: the block **state** is the truth about whether this is a
/// sign at all and how it sits, so a stale or orphan record whose state is
/// not a sign draws nothing.
#[must_use]
fn sign_spawn(block: [i32; 3], state_id: u32, text: SignText, light: u8) -> Option<SignSpawn> {
    let kind = sign_kind_for_state(state_id)?;
    let orientation = sign_orientation(state_id)?;
    Some(SignSpawn {
        pos: block,
        kind,
        orientation,
        front: text.front,
        back: text.back,
        light,
    })
}

/// Every sign to draw this frame, gathered from the client-owned world's
/// block-entity records. `eye` is the camera position, the same gate
/// [`chest_spawns`]/[`skull_spawns`] apply. No lid-style animation state:
/// sign text does not animate, so there is nothing here to tick.
///
/// Sorted by position for the same reason [`chest_spawns`] is — deterministic
/// batch order for pixel gates, not a correctness requirement of the draw
/// itself.
#[must_use]
pub fn sign_spawns(handle: &SharedHandle, eye: Vec3) -> Vec<SignSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();

    // Same lock-ordering rule as `chest_spawns`: `loaded_chunks()` takes its
    // own read lock, so it must not be called from inside the guard below.
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        sign_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, text) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        if let Some(spawn) = sign_spawn(block, state_id, text, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// The block's own dye colour, for a **standing** banner — `white_banner` →
/// `DyeColor::White`.
///
/// The base colour is the *block*, not a state property: vanilla ships sixteen
/// separate banner blocks. Grepping for a `color` property here finds nothing and
/// draws every banner white, which is the natural mistake because shulker boxes
/// are spelled the same way and skulls are not.
///
/// **Both** forms resolve now. `*_wall_banner` used to return `None` because the
/// asset corpus had no `createBodyLayer(false)` mesh and the standing rig would
/// have hung a full 42-texel pole in mid-air off the block face; both wall meshes
/// exist since `banner_wall_body_model`/`banner_wall_flag_model` landed.
///
/// The suffix order is load-bearing: `_wall_banner` has to be tried **before**
/// `_banner`, because `"red_wall_banner"` ends in `_banner` too and would
/// otherwise strip to `"red_wall"`, which is not a dye name — so every wall
/// banner in the world would silently draw nothing rather than draw wrong.
#[must_use]
fn banner_colour(state_id: u32) -> Option<(DyeColor, bool)> {
    let name = lodestone_data::block_states::block_name(state_id)?;
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(dye) = path.strip_suffix("_wall_banner") {
        return Some((DyeColor::from_name(dye)?, true));
    }
    Some((DyeColor::from_name(path.strip_suffix("_banner")?)?, false))
}

/// How a banner is attached, read off the property its own block actually has.
///
/// A standing banner has `rotation` (`RotationSegment`, `0..16`, `22.5` degrees a
/// step) and a wall banner has `facing` (four horizontals, `90` degrees a step) —
/// **neither has the other's**, so this is a fork on which block it is rather than
/// a property lookup that tries both. Reading `rotation` off a wall banner finds
/// nothing and draws no banner; reading `facing` off a standing one does the same.
#[must_use]
fn banner_attachment(state_id: u32, is_wall: bool) -> Option<BannerAttachment> {
    let props = lodestone_data::block_states::properties(state_id)?;
    if is_wall {
        let value = props
            .iter()
            .find(|(name, _)| *name == "facing")
            .map(|(_, value)| *value)?;
        return Some(BannerAttachment::Wall {
            facing_yaw_deg: horizontal_facing_yaw(value)?,
        });
    }
    let rotation_segment = props
        .iter()
        .find(|(name, _)| *name == "rotation")
        .and_then(|(_, value)| value.parse::<u8>().ok())?;
    Some(BannerAttachment::Ground { rotation_segment })
}

/// The block entity's stored pattern stack, parsed out of its NBT.
///
/// `BannerPatternLayers.Layer.CODEC` is `{pattern: <id>, color: <dye name>}` and
/// the list key is `patterns`. Both fields are namespaced ids on the wire, so the
/// namespace is stripped — [`lodestone_assets::banner_pattern_atlas`] keys its
/// sprites on the **bare** asset id (`"creeper"`), and passing
/// `"minecraft:creeper"` through resolves nothing and silently drops the layer.
///
/// A layer whose colour or pattern does not parse is dropped rather than
/// defaulted: a wrong-coloured layer is harder to notice than a missing one.
#[must_use]
fn banner_patterns(nbt: &lodestone_core::Nbt) -> Vec<StoredPatternLayer> {
    use lodestone_core::Nbt;

    let field = |compound: &'_ Nbt, key: &str| -> Option<String> {
        let Nbt::Compound(fields) = compound else {
            return None;
        };
        match fields.iter().find(|(name, _)| name == key).map(|(_, v)| v) {
            Some(Nbt::String(value)) => Some(value.clone()),
            _ => None,
        }
    };
    let Nbt::Compound(fields) = nbt else {
        return Vec::new();
    };
    let Some(Nbt::List { elements, .. }) = fields
        .iter()
        .find(|(name, _)| name == "patterns")
        .map(|(_, v)| v)
    else {
        return Vec::new();
    };
    elements
        .iter()
        .filter_map(|layer| {
            let pattern = field(layer, "pattern")?;
            let colour = field(layer, "color")?;
            Some(StoredPatternLayer {
                pattern_asset_id: pattern
                    .strip_prefix("minecraft:")
                    .unwrap_or(&pattern)
                    .to_string(),
                color: DyeColor::from_name(colour.strip_prefix("minecraft:").unwrap_or(&colour))?,
            })
        })
        .collect()
}

/// Every banner position within [`VIEW_DISTANCE`], paired with its block state and
/// pattern stack.
///
/// A second NBT-reading candidate gather beside [`sign_candidates`], and for the
/// same reason that one exists: [`chest_candidates`] discards `be.nbt`, and a
/// banner's whole appearance past its base colour lives there.
#[must_use]
fn banner_candidates(
    world: &World,
    chunks: impl IntoIterator<Item = ChunkPos>,
    eye: Vec3,
) -> Vec<([i32; 3], u32, Vec<StoredPatternLayer>)> {
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;
    let mut candidates = Vec::new();
    for pos in chunks {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let x = pos.x * 16 + i32::from(be.rel_x);
            let z = pos.z * 16 + i32::from(be.rel_z);
            let y = i32::from(be.y);
            let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
            if centre.distance_squared(eye) > cutoff {
                continue;
            }
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            candidates.push(([x, y, z], state_id, banner_patterns(&be.nbt)));
        }
    }
    candidates
}

/// One candidate resolved into a [`BannerSpawn`], standing or wall, or `None` when
/// the state is not a banner at all.
#[must_use]
fn banner_spawn(
    block: [i32; 3],
    state_id: u32,
    patterns: Vec<StoredPatternLayer>,
    phase: f32,
    light: u8,
) -> Option<BannerSpawn> {
    // One read decides both the dye and which form this is, so the colour and the
    // attachment can never disagree about whether it is a wall banner.
    let (base_color, is_wall) = banner_colour(state_id)?;
    Some(BannerSpawn {
        pos: block,
        attachment: banner_attachment(state_id, is_wall)?,
        base_color,
        patterns,
        phase,
        light,
    })
}

/// Every banner to draw this frame.
///
/// `game_time` and `partial_tick` are both needed and both come from the caller:
/// `banner_phase` mixes the block position into the tick so two adjacent banners
/// sway out of step, and the partial tick is what makes the sway smooth rather
/// than 20 Hz. A source that captured either would freeze every banner — the same
/// warning `bell_source` carries.
#[must_use]
pub fn banner_spawns(
    handle: &SharedHandle,
    eye: Vec3,
    game_time: i64,
    partial_tick: f32,
) -> Vec<BannerSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();

    let candidates = {
        let world = store.read();
        banner_candidates(
            &world,
            chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }),
            eye,
        )
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, state_id, patterns) in candidates {
        let light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        let phase = lodestone_render::block_entity::banner_phase(block, game_time, partial_tick);
        if let Some(spawn) = banner_spawn(block, state_id, patterns, phase, light) {
            out.push(spawn);
        }
    }
    out.sort_by_key(|s| s.pos);
    out
}

/// `PistonMovingBlockEntity.tick`'s ramp: `progress += 0.5F` per tick, so with
/// `TICKS_TO_EXTEND = 2` a whole push lasts **two ticks** — a tenth of a second.
///
/// The shortest animation in this module by a factor of five (a chest lid is ten
/// ticks, a bell fifty), which is why a stale render source is so much more
/// visible here: there is no window in which the frozen value looks like a
/// mid-animation frame.
const PISTON_PROGRESS_SPEED: f32 = 0.5;

/// One moving piston's animation clock — `PistonMovingBlockEntity`'s `progress`
/// and `progressO`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PistonMove {
    progress: f32,
    /// `progressO`: the value at the start of the current tick, for the
    /// partial-tick lerp.
    previous: f32,
}

/// Per-position moving-piston animation state — the piston sibling of
/// [`ChestLids`] and [`BellShakes`], and the only one of the three that **no
/// packet drives**.
///
/// # Why a client-side clock is needed at all
///
/// `PistonMovingBlockEntity.getUpdateTag` is `saveCustomOnly`, so the wire does
/// carry a `progress` — but it is `progressO`, the value at the *start* of the tick
/// the block entity was created on, and it is sent once. Vanilla's client then runs
/// `PistonMovingBlockEntity.tick` locally, adding [`PISTON_PROGRESS_SPEED`] each
/// tick. Without that local ramp every push would draw at its seed value for its
/// whole two-tick life, and the seed is normally `0.0` — which
/// [`piston_head_pose`](crate::gpu) turns into a displacement of one **whole** cell
/// backwards, i.e. geometry buried inside the piston base. So the missing clock
/// does not degrade to "no animation", it degrades to overlapping blocks.
///
/// # Removal is driven by the world, not by a counter
///
/// Vanilla drops the block entity itself once `progressO >= 1.0` (after five
/// `deathTicks` on the client). Here the authority is simpler and more robust: a
/// tracked entry is dropped as soon as its cell stops holding a `moving_piston`
/// block entity, which is exactly when the server's own `finalTick` replaces the
/// cell. A piston whose removal packet is lost therefore settles at `progress ==
/// 1.0` — geometry exactly on its destination cell, indistinguishable from the
/// finished block — rather than being stranded mid-travel.
#[derive(Debug, Default, Clone)]
pub struct PistonMoves {
    moves: HashMap<[i32; 3], PistonMove>,
}

impl PistonMoves {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advances every tracked piston one client tick — `PistonMovingBlockEntity.tick`.
    ///
    /// `present` is every `moving_piston` block entity the world still holds,
    /// paired with the `progress` its NBT carries — [`moving_piston_seeds`] builds
    /// it. It does double duty: it is the **liveness set** (anything not in it is
    /// dropped) and the **seed** for a position seen for the first time.
    ///
    /// # A newly-seen piston is seeded, not advanced, in the same call
    ///
    /// The insert happens *after* the advance, deliberately. Advancing on the
    /// discovery tick would start every push at `progress == 0.5` and halve the
    /// animation to a single tick — visible as a head that appears already
    /// half-way out. Vanilla's ordering is the same shape for a different reason:
    /// the block entity is constructed during chunk load and `tick` first runs on
    /// the following tick.
    pub fn tick(&mut self, present: &[([i32; 3], f32)]) {
        self.moves.retain(|pos, m| {
            if !present.iter().any(|(p, _)| p == pos) {
                return false;
            }
            m.previous = m.progress;
            m.progress = (m.progress + PISTON_PROGRESS_SPEED).min(1.0);
            true
        });
        for &(pos, seed) in present {
            self.moves.entry(pos).or_insert(PistonMove {
                progress: seed.clamp(0.0, 1.0),
                previous: seed.clamp(0.0, 1.0),
            });
        }
    }

    /// The interpolated progress at `pos` — `getProgress(a)`, i.e.
    /// `lerp(a, progressO, progress)`.
    ///
    /// `None` for an untracked position. That is **not** the same "absent equals
    /// at rest" shortcut [`ChestLids::openness`] can take: `0.0` is a real, and the
    /// most displaced, progress value, so a caller must be able to tell "not
    /// tracked yet" from "at the start of its travel". The gather uses the NBT's own
    /// seed in that case, so a piston is never drawn from a value this map made up.
    #[must_use]
    pub fn progress(&self, pos: [i32; 3], partial_tick: f32) -> Option<f32> {
        let m = self.moves.get(&pos)?;
        let t = partial_tick.clamp(0.0, 1.0);
        Some(m.previous + (m.progress - m.previous) * t)
    }

    /// Number of pistons currently moving (for stats and tests).
    #[must_use]
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// Whether nothing is being tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }
}

/// One `moving_piston` block entity's NBT, decoded — `PistonMovingBlockEntity`'s
/// five `loadAdditional` fields.
#[derive(Debug, Clone, PartialEq)]
struct MovingPistonNbt {
    /// `blockState`, resolved through [`lodestone_data::block_states::state_id`].
    moved_state: u32,
    /// `facing`, as a unit step. `Direction.LEGACY_ID_CODEC` is `Codec.BYTE` over
    /// `get3DDataValue`, so this is an [`lodestone_core::Nbt::Byte`], **not** an
    /// int — reading it as one silently defaults every piston to `DOWN`.
    direction: [i32; 3],
    /// `progress`, which vanilla writes as `progressO`. Seeds
    /// [`PistonMoves::tick`]; it is not the value drawn.
    progress: f32,
    extending: bool,
    /// `source`: whether this cell is the piston *base*'s own, rather than a cell
    /// a pushed block is travelling through.
    source: bool,
}

/// `Direction.from3DDataValue`'s unit step, for the byte
/// `Direction.LEGACY_ID_CODEC` stores.
///
/// The order is `DOWN, UP, NORTH, SOUTH, WEST, EAST` — vanilla's own enum
/// declaration order, which is *not* alphabetical and not the horizontal-facing
/// order the sign and chest gathers use. `None` rather than a wrapping index for
/// anything out of range: vanilla's `BY_ID` wraps, but a wrapped facing here would
/// silently push a contraption sideways.
#[must_use]
fn direction_step_from_3d(id: i8) -> Option<[i32; 3]> {
    Some(match id {
        0 => [0, -1, 0],
        1 => [0, 1, 0],
        2 => [0, 0, -1],
        3 => [0, 0, 1],
        4 => [-1, 0, 0],
        5 => [1, 0, 0],
        _ => return None,
    })
}

/// Renders a `BlockState.CODEC` NBT compound — `{Name: "...", Properties: {...}}`
/// — as the canonical state string [`lodestone_data::block_states::state_id`]
/// parses.
///
/// Going via the string rather than a direct table lookup is not a detour: that
/// function's three-tier fallback (exact, default-plus-overrides, then the bare
/// default) is exactly what a hand-rolled property match would have to
/// reimplement, and it is the tier-2 arm that makes a *synthesised* state such as
/// `piston_head[facing=up,short=true,type=normal]` resolve at all.
///
/// Properties are sorted, because tier 1 compares against the generated table's
/// own sorted slice.
#[must_use]
fn nbt_block_state_string(nbt: &lodestone_core::Nbt) -> Option<String> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let field = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, v)| v);
    let Some(Nbt::String(name)) = field("Name") else {
        return None;
    };
    let mut props: Vec<(&str, &str)> = match field("Properties") {
        Some(Nbt::Compound(pairs)) => pairs
            .iter()
            .filter_map(|(key, value)| match value {
                Nbt::String(value) => Some((key.as_str(), value.as_str())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    if props.is_empty() {
        return Some(name.clone());
    }
    props.sort_unstable();
    let rendered: Vec<String> = props
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    Some(format!("{name}[{}]", rendered.join(",")))
}

/// Decodes one moving piston's NBT, or `None` if any required field is missing or
/// the wrong tag type.
///
/// A missing `blockState` is `AIR` in vanilla and `extractRenderState` then draws
/// nothing (`!blockState.isAir()`), so this declines rather than defaulting: the
/// caller has nothing to draw either way, and declining keeps the untracked/at-rest
/// distinction [`PistonMoves::progress`] documents.
#[must_use]
fn moving_piston_nbt(nbt: &lodestone_core::Nbt) -> Option<MovingPistonNbt> {
    use lodestone_core::Nbt;

    let Nbt::Compound(fields) = nbt else {
        return None;
    };
    let field = |key: &str| fields.iter().find(|(name, _)| name == key).map(|(_, v)| v);

    let moved_state =
        lodestone_data::block_states::state_id(&nbt_block_state_string(field("blockState")?)?)?;
    if moved_state == lodestone_data::block_states::air_state_id() {
        return None;
    }
    let Some(Nbt::Byte(facing)) = field("facing") else {
        return None;
    };
    let direction = direction_step_from_3d(*facing)?;
    // Vanilla's `getFloatOr("progress", 0.0F)`: a missing progress is the start of
    // the travel, which is a real state rather than a decode failure.
    let progress = match field("progress") {
        Some(Nbt::Float(v)) => *v,
        None => 0.0,
        _ => return None,
    };
    // `getBooleanOr` — an `Nbt::Byte`, and absent means `false`.
    let flag = |key: &str| match field(key) {
        Some(Nbt::Byte(v)) => Some(*v != 0),
        None => Some(false),
        _ => None,
    };
    Some(MovingPistonNbt {
        moved_state,
        direction,
        progress,
        extending: flag("extending")?,
        source: flag("source")?,
    })
}

/// Whether a block state is `minecraft:moving_piston`.
#[must_use]
fn is_moving_piston(state_id: u32) -> bool {
    lodestone_data::block_states::block_name(state_id) == Some("minecraft:moving_piston")
}

/// One block state's named property value, or `None`.
#[must_use]
fn state_property(state_id: u32, key: &str) -> Option<&'static str> {
    lodestone_data::block_states::properties(state_id)?
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

/// `PistonHeadRenderer.extractRenderState`'s three-way branch: which state to draw
/// offset, and whether a *second*, unoffset state (the retracting source piston's
/// own base) draws with it.
///
/// # The three arms, and why two of them synthesise a state
///
/// 1. **The moved state already is a `piston_head`** — the arm a plain extension
///    takes, because `PistonBaseBlock` pushes a head state into the cell in front
///    of it. Vanilla rewrites `short` from the progress. Its guard is `progress <=
///    4.0F`, which is *always* true (progress is `0..=1`); it is not ported as a
///    condition because a condition that cannot be false reads as one that can.
/// 2. **A retracting source piston** (`isSourcePiston && !isExtending`) — a sticky
///    piston pulling its head home. The stored state is the *base* block, so a head
///    has to be built from scratch: `type` from whether the base is sticky, `facing`
///    from the base's own `facing`, and `short` from the progress with the
///    **opposite** comparison to arm 1 (`>= 0.5`, not `<= 0.5`, because the head is
///    travelling the other way). Its base draws too, forced to `extended=true`.
/// 3. **Anything else** — an ordinary pushed or pulled block, drawn as stored.
///
/// `short` is a genuine visual difference (a short head's arm is 4/16 deep instead
/// of 12/16), and getting arm 2's comparison backwards produces a head that pops
/// long at the wrong moment — plausible enough to survive a screenshot.
#[must_use]
fn moving_piston_states(nbt: &MovingPistonNbt, progress: f32) -> Option<(u32, Option<u32>)> {
    use lodestone_data::block_states::{block_name, state_id};

    let moved_name = block_name(nbt.moved_state)?;
    if moved_name == "minecraft:piston_head" {
        let facing = state_property(nbt.moved_state, "facing")?;
        let head_type = state_property(nbt.moved_state, "type")?;
        let short = progress <= 0.5;
        return Some((
            state_id(&format!(
                "minecraft:piston_head[facing={facing},short={short},type={head_type}]"
            ))?,
            None,
        ));
    }
    if nbt.source && !nbt.extending {
        // `PistonType.DEFAULT`'s serialized name is `"normal"`, not `"default"`.
        let head_type = if moved_name == "minecraft:sticky_piston" {
            "sticky"
        } else {
            "normal"
        };
        let facing = state_property(nbt.moved_state, "facing")?;
        let short = progress >= 0.5;
        let head = state_id(&format!(
            "minecraft:piston_head[facing={facing},short={short},type={head_type}]"
        ))?;
        let base = state_id(&format!("{moved_name}[extended=true,facing={facing}]"))?;
        return Some((head, Some(base)));
    }
    Some((nbt.moved_state, None))
}

/// Every `moving_piston` block entity in the world, paired with the `progress` its
/// NBT carries — [`PistonMoves::tick`]'s whole input.
///
/// **Unbounded by [`VIEW_DISTANCE`], unlike every gather in this module**, and the
/// asymmetry is deliberate: this feeds the *clock*, not the draw. A push lasts two
/// ticks, so a piston that a player walks toward mid-push would otherwise be seeded
/// at the progress it had when it entered range rather than when it started, and
/// would visibly restart. The list is short by construction — a `moving_piston`
/// cell exists for two ticks — so the cost is a walk of the loaded block-entity
/// records, not of blocks.
#[must_use]
pub fn moving_piston_seeds(handle: &SharedHandle) -> Vec<([i32; 3], f32)> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let world = store.read();
    let mut out = Vec::new();
    for pos in chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }) {
        let Some(chunk) = world.get(pos) else {
            continue;
        };
        for be in &chunk.block_entities {
            let y = i32::from(be.y);
            let state_id = chunk
                .column
                .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
            if !is_moving_piston(state_id) {
                continue;
            }
            let Some(decoded) = moving_piston_nbt(&be.nbt) else {
                continue;
            };
            out.push((
                [
                    pos.x * 16 + i32::from(be.rel_x),
                    y,
                    pos.z * 16 + i32::from(be.rel_z),
                ],
                decoded.progress,
            ));
        }
    }
    out
}

/// Every moving piston to draw this frame — vanilla's `PistonHeadRenderer`.
///
/// Feeds neither the entity pipeline (no `bakeLayer`, so no rig) nor the item path
/// (not an item), but the **moving-block-model** seam falling blocks use: see
/// `crate::gpu::MovingPistonSource`.
///
/// # Where each of the two light samples comes from
///
/// `extractRenderState` computes `pos = getBlockPos().relative(
/// getMovementDirection().getOpposite())` and samples light there, one cell *back*
/// along the push. That is not a detail: the block entity's own cell is full of
/// `moving_piston`, and the cell behind it is the air (or the piston base) the
/// geometry is actually travelling out of. The base's sample is taken at
/// `pos.relative(getMovementDirection())`, which for the retracting case arm 2
/// serves collapses back to the block entity's own cell.
#[must_use]
pub fn moving_piston_spawns(
    handle: &SharedHandle,
    moves: &PistonMoves,
    eye: Vec3,
    partial_tick: f32,
) -> Vec<lodestone_render::MovingPistonSpawn> {
    let Some(client) = handle.get() else {
        return Vec::new();
    };
    let store = client.chunk_world();
    let chunks = client.loaded_chunks();
    let cutoff = VIEW_DISTANCE * VIEW_DISTANCE;

    let candidates = {
        let world = store.read();
        let mut candidates = Vec::new();
        for pos in chunks.into_iter().map(|p| ChunkPos { x: p.x, z: p.z }) {
            let Some(chunk) = world.get(pos) else {
                continue;
            };
            for be in &chunk.block_entities {
                let x = pos.x * 16 + i32::from(be.rel_x);
                let z = pos.z * 16 + i32::from(be.rel_z);
                let y = i32::from(be.y);
                let centre = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, z as f32 + 0.5);
                if centre.distance_squared(eye) > cutoff {
                    continue;
                }
                let state_id = chunk
                    .column
                    .get_block(usize::from(be.rel_x), y, usize::from(be.rel_z));
                if !is_moving_piston(state_id) {
                    continue;
                }
                let Some(decoded) = moving_piston_nbt(&be.nbt) else {
                    continue;
                };
                candidates.push(([x, y, z], decoded));
            }
        }
        candidates
    };

    let sky_default = {
        let player = client.player();
        crate::mesher::sky_default_for_dimension(
            player.dimension.as_ref(),
            player.dimension_type.as_ref(),
        )
    };

    let mut out = Vec::new();
    for (block, decoded) in candidates {
        // The tracker's value when it has one, and the NBT's own seed otherwise —
        // never a made-up `0.0`, which is the *most* displaced progress there is.
        let progress = moves
            .progress(block, partial_tick)
            .unwrap_or(decoded.progress)
            .clamp(0.0, 1.0);
        let Some((state_id, base_state_id)) = moving_piston_states(&decoded, progress) else {
            continue;
        };
        // `getMovementDirection()` is `extending ? direction : -direction`, so its
        // opposite — the cell vanilla samples light at — is `-direction` while
        // extending and `+direction` while retracting.
        let back = if decoded.extending { -1 } else { 1 };
        let light_cell = [
            block[0] + decoded.direction[0] * back,
            block[1] + decoded.direction[1] * back,
            block[2] + decoded.direction[2] * back,
        ];
        let light = entity_light_at(handle, light_cell[0], light_cell[1], light_cell[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        let base_light = entity_light_at(handle, block[0], block[1], block[2], sky_default)
            .unwrap_or(lodestone_render::ENTITY_FULLBRIGHT);
        out.push(lodestone_render::MovingPistonSpawn {
            pos: block,
            state_id,
            base_state_id,
            direction: decoded.direction,
            progress,
            extending: decoded.extending,
            light,
            base_light,
        });
    }
    out.sort_by_key(|s| s.pos);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const POS: [i32; 3] = [4, 65, -9];

    #[test]
    fn a_non_lid_block_event_is_ignored() {
        let mut lids = ChestLids::new();
        // `b0 == 3` is a note block's instrument, not a chest lid.
        assert!(!lids.apply_block_event(POS, 3, 1));
        assert!(lids.is_empty());
        assert!(lids.apply_block_event(POS, 1, 1));
        assert_eq!(lids.len(), 1);
    }

    /// `b1` is a viewer *count*, not a boolean: two players looking into one
    /// chest send `2`, and that must still be open.
    #[test]
    fn a_viewer_count_above_one_still_opens() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 2);
        for _ in 0..10 {
            lids.tick();
        }
        assert!((lids.openness(POS, 1.0) - 1.0).abs() < 1e-5);
    }

    /// The ramp is ±0.1 per tick, so a lid takes exactly 10 ticks (half a
    /// second) to open. Asserted as a *duration*, not just at the endpoints —
    /// the endpoints are satisfied by a lid that teleports.
    #[test]
    fn the_lid_takes_ten_ticks_to_open_and_ten_to_shut() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        let mut seen = Vec::new();
        for _ in 0..10 {
            lids.tick();
            seen.push(lids.openness(POS, 1.0));
        }
        assert!((seen[0] - 0.1).abs() < 1e-5, "{seen:?}");
        assert!((seen[4] - 0.5).abs() < 1e-4, "{seen:?}");
        assert!((seen[9] - 1.0).abs() < 1e-5, "{seen:?}");
        // Monotone, and never overshoots.
        for pair in seen.windows(2) {
            assert!(pair[1] >= pair[0]);
            assert!(pair[1] <= 1.0 + 1e-6);
        }

        lids.apply_block_event(POS, 1, 0);
        for _ in 0..10 {
            lids.tick();
        }
        assert!(lids.openness(POS, 1.0).abs() < 1e-5);
    }

    /// The partial-tick lerp reads between the previous and current tick's
    /// values. Without it a lid steps at 20 Hz, which reads as a stutter rather
    /// than as a missing feature.
    #[test]
    fn openness_interpolates_within_a_tick() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        lids.tick(); // previous 0.0 -> openness 0.1
        assert!(lids.openness(POS, 0.0).abs() < 1e-6, "start of tick");
        assert!((lids.openness(POS, 0.5) - 0.05).abs() < 1e-6, "mid tick");
        assert!((lids.openness(POS, 1.0) - 0.1).abs() < 1e-6, "end of tick");
        // Out-of-range partial ticks clamp rather than extrapolating past 1.0.
        assert!((lids.openness(POS, 4.0) - 0.1).abs() < 1e-6);
        assert!(lids.openness(POS, -1.0).abs() < 1e-6);
    }

    /// A settled-shut lid is dropped so the map cannot grow without bound; the
    /// reported openness is unchanged by that, because an absent entry and a
    /// shut chest are the same value.
    #[test]
    fn settled_shut_lids_are_forgotten_without_changing_what_is_drawn() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        lids.tick();
        lids.apply_block_event(POS, 1, 0);
        for _ in 0..12 {
            lids.tick();
        }
        assert!(lids.is_empty(), "{} lids retained", lids.len());
        assert_eq!(lids.openness(POS, 1.0), 0.0);
        assert_eq!(lids.openness([0, 0, 0], 1.0), 0.0);
    }

    /// An open chest is *not* garbage-collected while it is open.
    #[test]
    fn an_open_lid_is_retained() {
        let mut lids = ChestLids::new();
        lids.apply_block_event(POS, 1, 1);
        for _ in 0..40 {
            lids.tick();
        }
        assert_eq!(lids.len(), 1);
        assert!((lids.openness(POS, 1.0) - 1.0).abs() < 1e-5);
    }

    /// Orientation comes from the real 26.2 state table, not a fixture: this is
    /// the check that the property *names* are right. A chest's `facing` is a
    /// horizontal direction and its `type` is single/left/right.
    #[test]
    fn chest_states_resolve_facing_and_half_from_the_real_table() {
        // Walk the whole table for chest states rather than hardcoding an id —
        // block state ids are not stable across versions and a hardcoded one is
        // the classic silently-stale fixture.
        let mut seen_halves = std::collections::BTreeSet::new();
        let mut seen_yaws = std::collections::BTreeSet::new();
        let mut chest_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:chest") {
                continue;
            }
            chest_states += 1;
            let (yaw, half) = chest_orientation(id).expect("a chest state must have facing");
            seen_yaws.insert(yaw as i32);
            seen_halves.insert(half);
            assert_eq!(chest_material(id), Some(ChestMaterial::Regular));
        }
        assert!(chest_states > 0, "no chest states in the table at all");
        assert_eq!(
            seen_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four horizontal facings must be reachable"
        );
        assert_eq!(
            seen_halves,
            [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right]
                .into_iter()
                .collect(),
            "all three chest types must be reachable"
        );
    }

    #[test]
    fn every_chest_block_in_the_real_table_resolves_to_a_material() {
        for path in [
            "chest",
            "trapped_chest",
            "ender_chest",
            "copper_chest",
            "exposed_copper_chest",
            "weathered_copper_chest",
            "oxidized_copper_chest",
        ] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert!(
                chest_material(id).is_some(),
                "{name} (state {id}) resolved to no material"
            );
            assert!(
                chest_orientation(id).is_some(),
                "{name} (state {id}) resolved no facing"
            );
        }
    }

    /// A non-chest with a block entity (a furnace has `facing` too) must resolve
    /// to no material, so `chest_spawns` skips it. This is the control on the
    /// material filter: without it every block entity in range would draw a
    /// chest.
    #[test]
    fn a_non_chest_block_entity_resolves_to_no_material() {
        for path in ["furnace", "barrel", "shulker_box", "beacon", "oak_sign"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert_eq!(chest_material(id), None, "{name} matched a chest material");
        }
    }

    #[test]
    fn chest_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        let lids = ChestLids::new();
        assert!(chest_spawns(&handle, &lids, Vec3::ZERO, 0.0).is_empty());
    }
}

/// Kept as its own module rather than folded into `tests` above so it never
/// has to touch that block's interior — this file is shared with the chest
/// lid/gather work and a separate module is the lowest-collision way to add
/// coverage alongside it.
#[cfg(test)]
mod skull_tests {
    use super::*;

    /// Orientation comes from the real 26.2 state table, not a fixture — the
    /// check that the property *names* are right and that both the floor
    /// (`rotation`) and wall (`facing`) shapes are reachable. Mirrors
    /// `chest_states_resolve_facing_and_half_from_the_real_table`.
    #[test]
    fn skull_states_resolve_orientation_from_the_real_table() {
        let mut floor_segments = std::collections::BTreeSet::new();
        let mut wall_yaws = std::collections::BTreeSet::new();
        let mut floor_states = 0usize;
        let mut wall_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            match lodestone_data::block_states::block_name(id) {
                Some("minecraft:skeleton_skull") => {
                    floor_states += 1;
                    match skull_orientation(id).expect("a floor skull must have an orientation") {
                        SkullOrientation::Floor { rotation_segment } => {
                            floor_segments.insert(rotation_segment);
                        }
                        SkullOrientation::Wall { .. } => panic!("skeleton_skull resolved as wall"),
                    }
                }
                Some("minecraft:skeleton_wall_skull") => {
                    wall_states += 1;
                    match skull_orientation(id).expect("a wall skull must have an orientation") {
                        SkullOrientation::Wall { facing_yaw_deg } => {
                            wall_yaws.insert(facing_yaw_deg as i32);
                        }
                        SkullOrientation::Floor { .. } => {
                            panic!("skeleton_wall_skull resolved as floor")
                        }
                    }
                }
                _ => {}
            }
        }
        assert!(floor_states > 0, "no floor skeleton_skull states at all");
        assert!(wall_states > 0, "no wall skeleton_wall_skull states at all");
        assert_eq!(
            floor_segments,
            (0..16).collect(),
            "all sixteen rotation segments must be reachable"
        );
        assert_eq!(
            wall_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four wall facings must be reachable"
        );
    }

    #[test]
    fn every_ported_skull_block_in_the_real_table_resolves() {
        for path in [
            "skeleton_skull",
            "skeleton_wall_skull",
            "wither_skeleton_skull",
            "wither_skeleton_wall_skull",
            "zombie_head",
            "zombie_wall_head",
            "creeper_head",
            "creeper_wall_head",
            "player_head",
            "player_wall_head",
        ] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert!(
                skull_type_for_state(id).is_some(),
                "{name} (state {id}) resolved to no skull type"
            );
            assert!(
                skull_orientation(id).is_some(),
                "{name} (state {id}) resolved no orientation"
            );
        }
    }

    /// The two real skull types this renderer declines — dragon and piglin —
    /// must still be *present* in the state table (so this is testing the
    /// decline, not a stale block name) and must resolve to no skull type.
    #[test]
    fn declined_skull_types_are_present_but_resolve_to_nothing() {
        for path in ["dragon_head", "dragon_wall_head", "piglin_head", "piglin_wall_head"] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(
                skull_type_for_state(id),
                None,
                "{name} unexpectedly resolved a skull type"
            );
        }
    }

    /// A non-skull block entity with a `facing` property (a furnace) must not
    /// resolve — the control on the type filter, mirroring
    /// `a_non_chest_block_entity_resolves_to_no_material`.
    #[test]
    fn a_non_skull_block_entity_resolves_to_no_skull_type() {
        for path in ["furnace", "chest", "barrel", "bell"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert_eq!(
                skull_type_for_state(id),
                None,
                "{name} matched a skull type"
            );
        }
    }

    #[test]
    fn skull_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(skull_spawns(&handle, Vec3::ZERO).is_empty());
    }
}

/// Kept as its own module for the same reason `skull_tests` is: this file is
/// shared with the chest/skull/sign gather work.
#[cfg(test)]
mod bell_tests {
    use super::*;

    /// A real 26.2 `bell` state must resolve, and a bell has no per-block-path
    /// variant to pick between — unlike chest/skull, every state of the one
    /// `minecraft:bell` block draws the identical rig, so this only checks
    /// presence and resolution, not orientation.
    #[test]
    fn bell_is_present_and_resolves_from_the_real_table() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:bell"))
            .expect("bell must be in the 26.2 state table");
        assert!(bell_is_present(id));
        let shakes = BellShakes::new();
        let spawn = bell_spawn([1, 2, 3], id, lodestone_render::ENTITY_FULLBRIGHT, &shakes, 0.0)
            .expect("must resolve");
        assert_eq!(spawn.pos, [1, 2, 3]);
        assert_eq!(spawn.shake, None, "an unrung bell is at rest");
    }

    /// The `BLOCK_EVENT` -> shake chain, end to end on the CPU side: a ring makes
    /// the gather report a shake, the tick counter advances, and the entry is gone
    /// once vanilla's 50-tick window closes.
    #[test]
    fn a_block_event_rings_the_bell_for_fifty_ticks_and_then_stops() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:bell"))
            .expect("bell must be in the 26.2 state table");
        let pos = [4, 5, 6];
        let mut shakes = BellShakes::new();
        assert!(shakes.apply_block_event(pos, 1, 2), "b0 == 1 with a north face rings");
        let spawn = bell_spawn(pos, id, lodestone_render::ENTITY_FULLBRIGHT, &shakes, 0.0)
            .expect("must resolve");
        assert_eq!(spawn.shake, Some((BellShakeDirection::North, 0.0)));

        // Ten ticks in, the counter is ten and the angle is non-zero — the whole
        // point of the chain, since `bell_shake_angle(_, 0.0)` is also zero and a
        // frozen counter would be indistinguishable from a bell at rest.
        for _ in 0..10 {
            shakes.tick();
        }
        // At partial tick 0 the value is the *start* of the current tick, which
        // after ten ticks is 9 — the same convention `ChestLids::openness` uses,
        // and the reason both trackers keep a `previous`.
        let (direction, ticks) = shakes.shake(pos, 0.0).expect("still shaking");
        assert_eq!(direction, BellShakeDirection::North);
        assert!((ticks - 9.0).abs() < 0.001, "ticks did not advance: {ticks}");
        let (_, end) = shakes.shake(pos, 1.0).expect("still shaking");
        assert!((end - 10.0).abs() < 0.001, "the partial tick does not interpolate: {end}");
        let (x_rot, z_rot) = lodestone_render::bell_shake_angle(Some(direction), ticks);
        assert!(x_rot.abs() > 0.0001, "a shaking bell must be rotated: {x_rot}");
        assert_eq!(z_rot, 0.0, "a north hit swings on x only");

        // And it ends: `BellBlockEntity.tick` clears at 50.
        for _ in 0..45 {
            shakes.tick();
        }
        assert!(shakes.is_empty(), "the shake outlived its 50-tick window");
        assert_eq!(shakes.shake(pos, 0.0), None);
    }

    /// The four horizontal faces map to vanilla's own `from3DDataValue` order, and
    /// the two vertical ones are dropped rather than stored as a direction the
    /// model has no rotation for.
    #[test]
    fn the_shake_direction_is_vanillas_own_3d_data_order() {
        assert_eq!(shake_direction_from_3d(2), Some(BellShakeDirection::North));
        assert_eq!(shake_direction_from_3d(3), Some(BellShakeDirection::South));
        assert_eq!(shake_direction_from_3d(4), Some(BellShakeDirection::West));
        assert_eq!(shake_direction_from_3d(5), Some(BellShakeDirection::East));
        assert_eq!(shake_direction_from_3d(0), None, "DOWN has no swing");
        assert_eq!(shake_direction_from_3d(1), None, "UP has no swing");
        // And a non-ring event never starts one, whatever its parameter says.
        let mut shakes = BellShakes::new();
        assert!(!shakes.apply_block_event([0, 0, 0], 0, 2));
        assert!(shakes.is_empty());
    }

    /// A non-bell block entity with a `facing` property (a furnace, a chest)
    /// must not resolve — the control on the type filter, mirroring
    /// `a_non_skull_block_entity_resolves_to_no_skull_type`.
    #[test]
    fn a_non_bell_block_entity_resolves_to_no_bell_spawn() {
        for path in ["furnace", "chest", "barrel", "skeleton_skull"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert!(!bell_is_present(id), "{name} matched as a bell");
            assert_eq!(
                bell_spawn(
                    [0, 0, 0],
                    id,
                    lodestone_render::ENTITY_FULLBRIGHT,
                    &BellShakes::new(),
                    0.0,
                ),
                None,
                "{name} unexpectedly resolved a bell spawn"
            );
        }
    }

    #[test]
    fn bell_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(bell_spawns(&handle, &BellShakes::new(), Vec3::ZERO, 0.0).is_empty());
    }
}

/// Kept as its own module for the same reason `skull_tests` is: this file is
/// shared with the chest/skull gather work.
#[cfg(test)]
mod sign_tests {
    use super::*;

    /// Orientation comes from the real 26.2 state table, not a fixture —
    /// mirrors `skull_states_resolve_orientation_from_the_real_table`. Only
    /// `oak_sign`/`oak_wall_sign` are walked (not every wood), since the
    /// property *shape* — not the wood — is what is under test, exactly the
    /// same choice `chest_states_resolve_facing_and_half_from_the_real_table`
    /// makes for one chest block.
    #[test]
    fn sign_states_resolve_orientation_from_the_real_table() {
        let mut ground_segments = std::collections::BTreeSet::new();
        let mut wall_yaws = std::collections::BTreeSet::new();
        let mut ground_states = 0usize;
        let mut wall_states = 0usize;
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            match lodestone_data::block_states::block_name(id) {
                Some("minecraft:oak_sign") => {
                    ground_states += 1;
                    assert!(sign_kind_for_state(id).is_some());
                    match sign_orientation(id).expect("a ground sign must have an orientation") {
                        SignOrientation::Ground { rotation_segment } => {
                            ground_segments.insert(rotation_segment);
                        }
                        SignOrientation::Wall { .. } => panic!("oak_sign resolved as wall"),
                    }
                }
                Some("minecraft:oak_wall_sign") => {
                    wall_states += 1;
                    assert!(sign_kind_for_state(id).is_some());
                    match sign_orientation(id).expect("a wall sign must have an orientation") {
                        SignOrientation::Wall { facing_yaw_deg } => {
                            wall_yaws.insert(facing_yaw_deg as i32);
                        }
                        SignOrientation::Ground { .. } => panic!("oak_wall_sign resolved as ground"),
                    }
                }
                _ => {}
            }
        }
        assert!(ground_states > 0, "no ground oak_sign states at all");
        assert!(wall_states > 0, "no wall oak_wall_sign states at all");
        assert_eq!(
            ground_segments,
            (0..16).collect(),
            "all sixteen rotation segments must be reachable"
        );
        assert_eq!(
            wall_yaws,
            [0, 90, 180, 270].into_iter().collect(),
            "all four wall facings must be reachable"
        );
    }

    /// Every plain sign block in the real 26.2 table — every wood, both
    /// standing and wall — must resolve as a sign with an orientation.
    /// Mirrors `every_ported_skull_block_in_the_real_table_resolves`.
    #[test]
    fn every_plain_sign_block_in_the_real_table_resolves() {
        for wood in [
            "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry",
            "pale_oak", "bamboo", "crimson", "warped",
        ] {
            for suffix in ["sign", "wall_sign"] {
                let name = format!("minecraft:{wood}_{suffix}");
                let found = (0..lodestone_data::block_states::STATE_COUNT)
                    .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
                let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
                assert!(sign_kind_for_state(id).is_some(), "{name} (state {id}) not a sign");
                assert!(
                    sign_orientation(id).is_some(),
                    "{name} (state {id}) resolved no orientation"
                );
            }
        }
    }

    /// Hanging signs now resolve — as [`SignKind::Hanging`], **not** as
    /// plain. This test replaces `hanging_signs_are_present_but_declined`,
    /// which asserted the opposite: the decline was recorded as needing "a
    /// different model set again (chains, a bar)", and that was 1.20's shape,
    /// not 26.2's (see [`SignKind`]'s doc).
    ///
    /// The load-bearing half is the *kind*, not the `is_some()`: the two
    /// families share the `_sign` suffix, so a name check that forgot to look
    /// for `hanging` first would pass an `is_some()` assertion and draw every
    /// hanging sign's text at a plain sign's height and scale.
    #[test]
    fn hanging_signs_resolve_as_hanging_and_plain_ones_as_plain() {
        for path in [
            "oak_hanging_sign",
            "oak_wall_hanging_sign",
            "bamboo_hanging_sign",
            "bamboo_wall_hanging_sign",
        ] {
            let name = format!("minecraft:{path}");
            let found = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()));
            let id = found.unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(
                sign_kind_for_state(id),
                Some(SignKind::Hanging),
                "{name} (state {id}) must resolve as a hanging sign"
            );
            assert!(
                sign_orientation(id).is_some(),
                "{name} (state {id}) resolved no orientation"
            );
        }
        for path in ["oak_sign", "oak_wall_sign"] {
            let name = format!("minecraft:{path}");
            let id = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
                .unwrap_or_else(|| panic!("{name} is not in the 26.2 state table"));
            assert_eq!(sign_kind_for_state(id), Some(SignKind::Plain), "{name}");
        }
    }

    /// Every hanging sign block in the real 26.2 table — every wood, both
    /// ceiling and wall — resolves with a kind *and* an orientation, and the
    /// wall variant resolves as [`SignOrientation::Wall`] while the ceiling
    /// one resolves as [`SignOrientation::Ground`]. The orientation fork is
    /// the part that could silently go wrong: a ceiling hanging sign carries
    /// `attached` and `rotation`, a wall one carries `facing`, and
    /// [`sign_orientation`] returns on whichever it meets first.
    #[test]
    fn every_hanging_sign_block_in_the_real_table_resolves_with_the_right_fork() {
        for wood in [
            "oak", "spruce", "birch", "jungle", "acacia", "dark_oak", "mangrove", "cherry",
            "pale_oak", "bamboo", "crimson", "warped",
        ] {
            for (suffix, wall) in [("hanging_sign", false), ("wall_hanging_sign", true)] {
                let name = format!("minecraft:{wood}_{suffix}");
                let ids: Vec<u32> = (0..lodestone_data::block_states::STATE_COUNT)
                    .filter(|id| {
                        lodestone_data::block_states::block_name(*id) == Some(name.as_str())
                    })
                    .collect();
                assert!(!ids.is_empty(), "{name} is not in the 26.2 state table");
                for id in ids {
                    assert_eq!(sign_kind_for_state(id), Some(SignKind::Hanging), "{name}");
                    match sign_orientation(id) {
                        Some(SignOrientation::Wall { .. }) if wall => {}
                        Some(SignOrientation::Ground { .. }) if !wall => {}
                        other => panic!("{name} (state {id}) resolved {other:?}, wall={wall}"),
                    }
                }
            }
        }
    }

    /// A non-sign block entity must not resolve — the control on the type
    /// filter, mirroring `a_non_skull_block_entity_resolves_to_no_skull_type`.
    #[test]
    fn a_non_sign_block_entity_resolves_to_no_sign_kind() {
        for path in ["furnace", "chest", "barrel", "bell", "skeleton_skull"] {
            let name = format!("minecraft:{path}");
            let Some(id) = (0..lodestone_data::block_states::STATE_COUNT)
                .find(|id| lodestone_data::block_states::block_name(*id) == Some(name.as_str()))
            else {
                continue;
            };
            assert!(
                sign_kind_for_state(id).is_none(),
                "{name} matched a sign kind"
            );
        }
    }

    /// A real 26.2 `oak_sign` state joined with typed text (the shape
    /// `docs/block-entity-renderers.md`'s live probe captured, parsed once
    /// already in `lodestone-world`'s own tests) must survive the whole
    /// `sign_spawn` resolution — the join between that parse and the
    /// block-state-driven orientation/kind gate, which is the one thing
    /// `lodestone-world`'s tests cannot see since they have no state table.
    #[test]
    fn a_real_sign_state_plus_real_text_resolves_to_a_spawn_with_that_text() {
        let id = (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some("minecraft:oak_sign"))
            .expect("oak_sign must be in the 26.2 state table");
        let mut text = SignText::default();
        text.front.lines[0] = "LODESTONE PROBE".to_owned();
        let spawn = sign_spawn([0, 64, 0], id, text, lodestone_render::ENTITY_FULLBRIGHT)
            .expect("a real oak_sign state must resolve to a spawn");
        assert_eq!(spawn.front.lines[0], "LODESTONE PROBE");
        assert!(matches!(spawn.orientation, SignOrientation::Ground { .. }));
    }

    #[test]
    fn sign_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(sign_spawns(&handle, Vec3::ZERO).is_empty());
    }
}

/// Shulker boxes — kept in its own module beside `bell_tests` for the
/// same reason: this file is shared across every block-entity family.
#[cfg(test)]
mod shulker_tests {
    use super::*;

    /// Finds the first state id whose block name matches, against the real 26.2
    /// table rather than a fixture.
    fn state_named(name: &str) -> u32 {
        (0..lodestone_data::block_states::STATE_COUNT)
            .find(|id| lodestone_data::block_states::block_name(*id) == Some(name))
            .unwrap_or_else(|| panic!("{name} must be in the 26.2 state table"))
    }

    /// **The colour is the block id, not a property.** A `color` lookup finds
    /// nothing on any of the seventeen blocks and would draw every box undyed —
    /// which looks like a texture-loading problem rather than a resolver bug.
    #[test]
    fn the_dye_colour_comes_off_the_block_id_and_the_plain_box_has_none() {
        let plain = shulker_orientation(state_named("minecraft:shulker_box"))
            .expect("the plain box resolves");
        assert_eq!(plain.0, None);
        for colour in SHULKER_COLOURS {
            let id = state_named(&format!("minecraft:{colour}_shulker_box"));
            let (resolved, _) = shulker_orientation(id).expect("a dyed box resolves");
            assert_eq!(resolved, Some(colour), "{colour} did not resolve");
        }
        // Not every block with a `facing` property is a shulker box.
        assert!(shulker_orientation(state_named("minecraft:chest")).is_none());
    }

    /// Every `FACING` value resolves, including the two vertical ones a chest
    /// cannot have — and a state with no `facing` at all takes vanilla's own
    /// `getValueOrElse(FACING, UP)` default rather than failing.
    #[test]
    fn every_facing_resolves_and_a_missing_one_defaults_to_up() {
        let mut seen = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:shulker_box") {
                continue;
            }
            let (_, facing) = shulker_orientation(id).expect("resolves");
            seen.insert(facing);
        }
        assert_eq!(
            seen.len(),
            6,
            "the plain shulker box should span all six facings, saw {seen:?}"
        );
        let spawn = shulker_spawn(
            [7, 8, 9],
            state_named("minecraft:shulker_box"),
            lodestone_render::ENTITY_FULLBRIGHT,
        )
        .expect("resolves");
        assert_eq!(spawn.pos, [7, 8, 9]);
        assert_eq!(spawn.progress, 0.0, "a box nobody has open is closed");
    }

    /// The gather is empty rather than a panic before login, matching every other
    /// family's — the guard that lets the source be installed unconditionally.
    #[test]
    fn shulker_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(shulker_spawns(&handle, Vec3::ZERO).is_empty());
    }

    /// `has_book` decides whether there is anything to draw, and `facing` goes
    /// through the *clockwise* yaw.
    ///
    /// Driven over every real `minecraft:lectern` state id in the data crate
    /// rather than a hand-built one, so the four facings and both `has_book`
    /// values all come from the jar. Two things the walk pins that a single
    /// hand-picked state cannot: a bookless lectern yields **no** spawn at all
    /// (there is genuinely nothing to draw — the shelf is a real block model),
    /// and every book-bearing one yields a yaw that is *not* its plain facing
    /// yaw, which is the quarter-turn trap.
    #[test]
    fn a_lectern_only_spawns_with_a_book_and_takes_the_clockwise_yaw() {
        let mut with_book = 0_usize;
        let mut without_book = 0_usize;
        let mut yaws = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            if lodestone_data::block_states::block_name(id) != Some("minecraft:lectern") {
                continue;
            }
            let props = lodestone_data::block_states::properties(id).expect("lectern has properties");
            let facing = props
                .iter()
                .find(|(n, _)| *n == "facing")
                .map(|(_, v)| *v)
                .expect("lectern has facing");
            let has_book = props
                .iter()
                .any(|(n, v)| *n == "has_book" && *v == "true");

            match lectern_spawn([1, 2, 3], id, lodestone_render::ENTITY_FULLBRIGHT) {
                None => {
                    assert!(!has_book, "a lectern with a book must spawn");
                    without_book += 1;
                }
                Some(spawn) => {
                    assert!(has_book, "a bookless lectern must not spawn");
                    assert_eq!(spawn.pos, [1, 2, 3]);
                    let plain = horizontal_facing_yaw(facing).expect("horizontal");
                    assert_ne!(
                        spawn.facing_yaw_deg, plain,
                        "{facing}: the plain facing yaw lays the book sideways"
                    );
                    assert_eq!(
                        Some(spawn.facing_yaw_deg),
                        horizontal_facing_clockwise_yaw(facing)
                    );
                    #[expect(
                        clippy::cast_possible_truncation,
                        reason = "the four yaws are exact multiples of 90"
                    )]
                    yaws.insert(spawn.facing_yaw_deg as i32);
                    with_book += 1;
                }
            }
        }
        assert_eq!(yaws.len(), 4, "all four facings, saw {yaws:?}");
        assert!(with_book > 0 && without_book > 0, "{with_book}/{without_book}");
        assert!(
            lectern_spawn([0, 0, 0], state_named("minecraft:bell"), 0).is_none(),
            "a bell is not a lectern"
        );
    }

    /// **The suffix-order trap, and the two angle conventions.**
    ///
    /// `"red_wall_banner"` ends in `_banner`, so a colour parse that tries
    /// `_banner` first strips it to `"red_wall"` — not a dye name — and **every
    /// wall banner in the world silently draws nothing**. The gate drives all
    /// sixteen dyes through both block families, so the ordering cannot regress
    /// for one colour and pass for the rest.
    ///
    /// It also pins that the two forms take their angle from *different*
    /// properties, since neither block has the other's: a standing banner has
    /// `rotation` and a wall banner has `facing`.
    #[test]
    fn every_dye_resolves_for_both_banner_families_and_takes_its_own_angle() {
        use lodestone_render::BannerAttachment;

        let mut ground = 0_usize;
        let mut wall = 0_usize;
        let mut segments = std::collections::HashSet::new();
        let mut facings = std::collections::HashSet::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let Some(name) = lodestone_data::block_states::block_name(id) else {
                continue;
            };
            if !name.ends_with("_banner") {
                continue;
            }
            let is_wall_block = name.ends_with("_wall_banner");
            let (_, is_wall) = banner_colour(id)
                .unwrap_or_else(|| panic!("{name} must resolve a dye colour and a form"));
            assert_eq!(is_wall, is_wall_block, "{name}");

            let attachment = banner_attachment(id, is_wall)
                .unwrap_or_else(|| panic!("{name} must resolve an attachment"));
            match attachment {
                BannerAttachment::Ground { rotation_segment } => {
                    assert!(!is_wall_block, "{name} resolved as standing");
                    segments.insert(rotation_segment);
                    ground += 1;
                }
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the four facing yaws are exact multiples of 90"
                )]
                BannerAttachment::Wall { facing_yaw_deg } => {
                    assert!(is_wall_block, "{name} resolved as wall");
                    facings.insert(facing_yaw_deg as i32);
                    wall += 1;
                }
            }
        }
        // 16 dyes x 16 rotations, and 16 dyes x 4 facings.
        assert_eq!(ground, 256, "sixteen dyes across sixteen rotation segments");
        assert_eq!(wall, 64, "sixteen dyes across four facings");
        assert_eq!(segments.len(), 16, "every rotation segment, saw {segments:?}");
        assert_eq!(facings.len(), 4, "every facing, saw {facings:?}");

        // The control that makes the ordering assertion mean something: the
        // wrong-order parse really does fail on a wall banner.
        assert!(
            DyeColor::from_name("red_wall").is_none(),
            "stripping `_banner` first leaves `red_wall`, which must not parse"
        );
    }

    /// The gather is empty rather than a panic before login, like every other
    /// family's.
    #[test]
    fn lectern_spawns_before_login_is_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        assert!(lectern_spawns(&handle, Vec3::ZERO).is_empty());
    }

    /// The enchanting-table gathers are empty rather than a panic before login,
    /// like every other family's.
    #[test]
    fn enchanting_table_gathers_before_login_are_empty_rather_than_a_panic() {
        let handle: SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        let books = EnchantingTableBooks::new();
        assert!(enchanting_table_positions(&handle, glam::DVec3::ZERO, 8.0).is_empty());
        assert!(enchanting_table_spawns(&handle, &books, Vec3::ZERO, 1.0).is_empty());
    }

    /// A book takes exactly **10 ticks** to open and 10 to shut, at `±0.1` a tick.
    ///
    /// Asserted as a *duration* with a value predicted at every step, not just at
    /// the endpoints — the endpoints alone are satisfied by a book that teleports
    /// open, the same trap `the_lid_takes_ten_ticks_to_open_and_ten_to_shut`
    /// records for chests. The rate is vanilla's `entity.open += 0.1F` per tick,
    /// which is why this must not be advanced per frame.
    #[test]
    fn a_book_takes_ten_ticks_to_open_and_ten_to_shut() {
        const POS: [i32; 3] = [4, 65, -9];
        let near = glam::DVec3::new(4.5, 65.5, -8.5);
        let far = glam::DVec3::new(4.5, 65.5, 60.0);
        let mut books = EnchantingTableBooks::new();
        for tick in 1..=12 {
            books.tick(&[POS], near);
            let (_, _, open, _) = books.state(POS, 1.0).expect("tracked while a player is near");
            let expected = (0.1 * tick as f32).min(1.0);
            assert!(
                (open - expected).abs() < 1e-5,
                "tick {tick}: open {open}, expected {expected}"
            );
        }
        for tick in 1..=10 {
            books.tick(&[POS], far);
            let expected = (1.0 - 0.1 * tick as f32).max(0.0);
            let open = books
                .state(POS, 1.0)
                .map_or(0.0, |(_, _, open, _)| open);
            assert!(
                (open - expected).abs() < 1e-5,
                "closing tick {tick}: open {open}, expected {expected}"
            );
        }
        // One more tick and the settled-shut entry is collected, the same
        // garbage-collection `ChestLids` does — and `state` then answers `None`,
        // which the gather reads as "no book", identical on screen to a shut one.
        books.tick(&[POS], far);
        assert!(
            books.is_empty(),
            "a fully shut book should be collected, {} left",
            books.len()
        );
    }

    /// The book chases the player the **short** way round the `±PI` seam.
    ///
    /// Both hypotheses computed from outside arithmetic. Starting at `rot = 3.0`
    /// with a target of `-3.0`, the raw difference is `-6.0`; wrapped into
    /// `-PI..PI` it is `+0.28319`, so 40% of it puts `rot` at
    /// `3.0 + 0.11327 = 3.11327`. Without the wrap the book takes the long way and
    /// lands at `3.0 - 2.4 = 0.6` — nearly a full revolution backwards, every time
    /// a player walks past one particular corner.
    #[test]
    fn the_book_chases_the_player_the_short_way_round() {
        let mut rng = JavaRandom::new(1);
        let mut book = Book {
            rot: 3.0,
            // `+0.02` is applied before the chase when no player is near, so this
            // lands the target on exactly `-3.0`.
            t_rot: -3.02,
            ..Book::default()
        };
        book.tick([0, 0, 0], None, &mut rng);
        assert!(
            (book.rot - 3.113_274).abs() < 1e-4,
            "rot is {}, expected 3.113274 (the short way); 0.6 is the long way",
            book.rot
        );
    }

    /// `tRot = atan2(zd, xd)`, in that argument order.
    ///
    /// The swap is the failure this catches and it is invisible any other way: a
    /// player due **east** of the table (`+x`) must give `0`, and one due
    /// **south** (`+z`) must give `PI/2`. Swapped arguments produce exactly those
    /// two values in the opposite order, so a single-position check passes.
    ///
    /// Due **west** is deliberately not one of the samples: `atan2` returns `+PI`
    /// there and the wrap into `-PI..PI` is half-open, so the stored value is
    /// `-PI` — correct, matching vanilla's own `while (tRot >= PI)`, and a
    /// misleading thing to assert an expected sign on. The third sample sits off
    /// the seam at `3*PI/4`, where a swapped call gives `-PI/4` instead.
    #[test]
    fn the_target_angle_points_at_the_player_in_atan2s_argument_order() {
        const POS: [i32; 3] = [10, 64, 20];
        let mut rng = JavaRandom::new(2);
        for (offset, expected) in [
            (glam::DVec3::new(1.0, 0.0, 0.0), 0.0),
            (glam::DVec3::new(0.0, 0.0, 1.0), std::f32::consts::FRAC_PI_2),
            (
                glam::DVec3::new(-1.0, 0.0, 1.0),
                3.0 * std::f32::consts::FRAC_PI_4,
            ),
        ] {
            let centre = glam::DVec3::new(
                f64::from(POS[0]) + 0.5,
                f64::from(POS[1]) + 0.5,
                f64::from(POS[2]) + 0.5,
            );
            let mut book = Book::default();
            book.tick(POS, Some(centre + offset), &mut rng);
            assert!(
                (book.t_rot - expected).abs() < 1e-5,
                "player at {offset:?} gave t_rot {}, expected {expected}",
                book.t_rot
            );
        }
    }

    /// Vanilla's `do { flipT += nextInt(4) - nextInt(4) } while (old == flipT)`
    /// must **always** move the target, and a plain `if` leaves it occasionally
    /// unmoved when a page was asked to turn.
    ///
    /// While `open < 0.5` a re-roll happens every tick unconditionally, so the
    /// first **four** ticks are four guaranteed re-rolls — which is what makes this
    /// assertable without controlling the dice. The difference of two `nextInt(4)`
    /// draws is zero one time in four, so four ticks of a plain `if` would fail
    /// this with probability about `1 - (3/4)^4 = 68%`; across the seeds swept
    /// below it is a certainty.
    ///
    /// **Four and not five, and the off-by-one is vanilla's**: the test is
    /// `open < 0.5` *after* the `+= 0.1`, so the fifth tick's `open` is exactly
    /// `0.5` and falls through to the 1-in-40 dice instead. This test asserted five
    /// on its first run and failed at tick 4 for exactly that reason — a wrong test
    /// premise, not a wrong port.
    #[test]
    fn a_page_reroll_always_moves_the_target() {
        const POS: [i32; 3] = [0, 64, 0];
        let centre = glam::DVec3::new(0.5, 64.5, 0.5);
        for seed in 0..16 {
            let mut rng = JavaRandom::new(seed);
            let mut book = Book::default();
            for tick in 0..4 {
                let before = book.flip_t;
                book.tick(POS, Some(centre), &mut rng);
                assert!(
                    (book.flip_t - before).abs() > f32::EPSILON,
                    "seed {seed} tick {tick}: flip_t stayed at {before}"
                );
            }
        }
    }

    /// `java.util.Random.nextInt(bound)`'s two branches are not interchangeable,
    /// and this animation uses both: `nextInt(4)` is the power-of-two
    /// multiply-and-shift and `nextInt(40)` is the rejection loop.
    ///
    /// Asserted as coverage of the whole range in both, not just as "in bounds":
    /// a bound-off-by-one, or a rejection loop that never terminates its tail,
    /// stays in bounds while losing values. `nextInt(4)` must produce all four.
    #[test]
    fn the_java_random_covers_both_bound_branches() {
        let mut rng = JavaRandom::new(0xDEAD_BEEF);
        let mut small = [false; 4];
        let mut seen_low = false;
        let mut seen_high = false;
        for _ in 0..4000 {
            let a = rng.next_int(4);
            assert!((0..4).contains(&a), "nextInt(4) produced {a}");
            small[a as usize] = true;
            let b = rng.next_int(40);
            assert!((0..40).contains(&b), "nextInt(40) produced {b}");
            seen_low |= b == 0;
            seen_high |= b == 39;
        }
        assert!(small.iter().all(|seen| *seen), "nextInt(4) missed a value");
        assert!(
            seen_low && seen_high,
            "nextInt(40) never reached an endpoint, so its range is wrong"
        );
    }
}

/// Moving-piston gates — `PistonHeadRenderer` and `PistonMovingBlockEntity`.
///
/// Its own module for the same reason `sign_tests` is: this file is shared, and a
/// per-renderer module keeps the pathspec commit and the failure output honest
/// about which unit broke.
#[cfg(test)]
mod piston_tests {
    use super::*;

    const PISTON_POS: [i32; 3] = [12, 71, -40];

    /// `Direction.LEGACY_ID_CODEC`'s byte, resolved against vanilla's own enum
    /// declaration order rather than an alphabetical or a horizontal-facing one.
    ///
    /// The wrong hypothesis worth excluding is the **2-D** order the sign and
    /// banner gathers use (`SOUTH, WEST, NORTH, EAST`), which shares no value with
    /// this table except by accident — so the assertion is the whole six-entry map,
    /// and the two out-of-range probes prove it declines rather than wrapping the
    /// way vanilla's `BY_ID` does.
    #[test]
    fn the_facing_byte_is_the_3d_data_value_not_the_2d_one() {
        let mut wrong: Vec<String> = Vec::new();
        for (id, expected) in [
            (0_i8, [0, -1, 0]),
            (1, [0, 1, 0]),
            (2, [0, 0, -1]),
            (3, [0, 0, 1]),
            (4, [-1, 0, 0]),
            (5, [1, 0, 0]),
        ] {
            match direction_step_from_3d(id) {
                Some(step) if step == expected => {}
                other => wrong.push(format!("{id} -> {other:?}, expected {expected:?}")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // Vanilla's `BY_ID` wraps out-of-range ids. Wrapping here would push a
        // contraption along an axis nobody asked for, so this declines.
        assert_eq!(direction_step_from_3d(6), None);
        assert_eq!(direction_step_from_3d(-1), None);
    }

    /// `BlockState.CODEC`'s compound renders as the canonical state string, and the
    /// rendered string resolves against the **real 26.2 table** — not a fixture.
    ///
    /// The expected id is derived from `lodestone_data`'s own table on the other
    /// side of the string, so the two agree only if the property names, the sort
    /// order and the bracket syntax are all right. A bare `Name` with no
    /// `Properties` must resolve to that block's *default* state, which is the arm
    /// where "lowest id sharing the name" used to be wrong for 661 blocks.
    #[test]
    fn a_codec_block_state_compound_renders_a_string_the_real_table_resolves() {
        use lodestone_core::Nbt;

        let compound = Nbt::Compound(vec![
            ("Name".into(), Nbt::String("minecraft:piston_head".into())),
            (
                "Properties".into(),
                Nbt::Compound(vec![
                    // Deliberately out of sorted order on the wire.
                    ("type".into(), Nbt::String("sticky".into())),
                    ("facing".into(), Nbt::String("up".into())),
                    ("short".into(), Nbt::String("true".into())),
                ]),
            ),
        ]);
        let rendered = nbt_block_state_string(&compound).expect("a renderable compound");
        assert_eq!(
            rendered, "minecraft:piston_head[facing=up,short=true,type=sticky]",
            "properties must be sorted by key, which is what the generated table's \
             own slice comparison assumes"
        );
        let id = lodestone_data::block_states::state_id(&rendered).expect("a real state");
        assert_eq!(
            lodestone_data::block_states::block_name(id),
            Some("minecraft:piston_head")
        );
        let props = lodestone_data::block_states::properties(id).expect("properties");
        assert!(props.contains(&("facing", "up")), "{props:?}");
        assert!(props.contains(&("short", "true")), "{props:?}");
        assert!(props.contains(&("type", "sticky")), "{props:?}");

        // A bare name resolves to the default state, and the default is not
        // necessarily the lowest id sharing the name.
        let bare = Nbt::Compound(vec![(
            "Name".into(),
            Nbt::String("minecraft:sticky_piston".into()),
        )]);
        let bare_id = lodestone_data::block_states::state_id(
            &nbt_block_state_string(&bare).expect("a renderable bare compound"),
        )
        .expect("a real state");
        assert_eq!(
            lodestone_data::block_states::properties(bare_id),
            Some(&[("extended", "false"), ("facing", "north")][..]),
            "`PistonBaseBlock`'s registered default is `facing=north, extended=false`"
        );
    }

    /// `extractRenderState`'s branch 1: the moved state already *is* a piston head,
    /// and `short` is rewritten from the progress with `<= 0.5`.
    ///
    /// The discriminating input is **not** `0.5` — that is the boundary, where the
    /// inclusive and exclusive readings of the comparison coincide with each other
    /// and where branch 2's `>= 0.5` also fires. `0.25` and `0.75` are on opposite
    /// sides of it and both are checked, because asserting only one is satisfied by
    /// a hardcoded `short`.
    #[test]
    fn a_moved_piston_head_takes_its_short_from_the_progress() {
        let head = lodestone_data::block_states::state_id(
            "minecraft:piston_head[facing=up,short=false,type=normal]",
        )
        .expect("a real head state");
        let nbt = MovingPistonNbt {
            moved_state: head,
            direction: [0, 1, 0],
            progress: 0.0,
            extending: true,
            source: false,
        };

        let mut wrong: Vec<String> = Vec::new();
        for (progress, expected_short) in [(0.25_f32, "true"), (0.75, "false")] {
            let (state, base) = moving_piston_states(&nbt, progress).expect("a resolvable state");
            if base.is_some() {
                wrong.push(format!("progress {progress}: an extension drew a base"));
            }
            let short = state_property(state, "short");
            if short != Some(expected_short) {
                wrong.push(format!(
                    "progress {progress}: short={short:?}, expected {expected_short}"
                ));
            }
            // Facing and type must survive the rewrite untouched.
            if state_property(state, "facing") != Some("up")
                || state_property(state, "type") != Some("normal")
            {
                wrong.push(format!("progress {progress}: facing/type were rewritten"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// `extractRenderState`'s branch 2: a retracting **source** piston synthesises a
    /// head from its base block and draws its base as well.
    ///
    /// Three separate claims, each of which a plausible port gets wrong on its own:
    ///
    /// * the head's `type` is `sticky` for a sticky base and `normal` — **not**
    ///   `default` — for a plain one, because `PistonType.DEFAULT`'s serialized name
    ///   is `normal`;
    /// * the head's `short` uses `>= 0.5`, the **opposite** comparison to branch 1,
    ///   so at `0.25` it is `false` where branch 1 would say `true`;
    /// * the base is forced to `extended=true` and keeps the base's own facing.
    ///
    /// `0.25` discriminates the second claim from branch 1's rule; `0.5` would not.
    #[test]
    fn a_retracting_source_piston_synthesises_a_head_and_draws_its_base() {
        let mut wrong: Vec<String> = Vec::new();
        for (base_block, expected_type) in [
            ("minecraft:sticky_piston", "sticky"),
            ("minecraft:piston", "normal"),
        ] {
            let base_state = lodestone_data::block_states::state_id(&format!(
                "{base_block}[extended=false,facing=west]"
            ))
            .expect("a real base state");
            let nbt = MovingPistonNbt {
                moved_state: base_state,
                direction: [-1, 0, 0],
                progress: 0.0,
                extending: false,
                source: true,
            };
            let (head, base) = moving_piston_states(&nbt, 0.25).expect("a resolvable state");
            if lodestone_data::block_states::block_name(head) != Some("minecraft:piston_head") {
                wrong.push(format!("{base_block}: head is not a piston head"));
            }
            if state_property(head, "type") != Some(expected_type) {
                wrong.push(format!(
                    "{base_block}: head type is {:?}, expected {expected_type}",
                    state_property(head, "type")
                ));
            }
            if state_property(head, "facing") != Some("west") {
                wrong.push(format!("{base_block}: head facing did not follow the base"));
            }
            // Branch 2's comparison is `>= 0.5`, so a quarter of the way through a
            // retraction the head is still long.
            if state_property(head, "short") != Some("false") {
                wrong.push(format!(
                    "{base_block}: short is {:?} at progress 0.25 — branch 1's \
                     `<= 0.5` rule was used instead of branch 2's `>= 0.5`",
                    state_property(head, "short")
                ));
            }
            match base {
                Some(base) => {
                    if lodestone_data::block_states::block_name(base) != Some(base_block) {
                        wrong.push(format!("{base_block}: base block changed identity"));
                    }
                    if state_property(base, "extended") != Some("true") {
                        wrong.push(format!("{base_block}: base was not forced extended"));
                    }
                    if state_property(base, "facing") != Some("west") {
                        wrong.push(format!("{base_block}: base facing changed"));
                    }
                }
                None => wrong.push(format!("{base_block}: no base drew")),
            }
            // And at 0.75 the head has gone short — so `short` is a function of the
            // progress here and not a constant that happened to read correctly.
            let (late_head, _) = moving_piston_states(&nbt, 0.75).expect("a resolvable state");
            if state_property(late_head, "short") != Some("true") {
                wrong.push(format!("{base_block}: short did not flip by progress 0.75"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// `extractRenderState`'s branch 3: an ordinary pushed block is drawn exactly as
    /// stored, with no base.
    ///
    /// The control for the two branch tests above: without it, a
    /// `moving_piston_states` that returned its input unchanged in *every* case
    /// would still have to fail them, but one that synthesised a head in every case
    /// would not be caught anywhere.
    #[test]
    fn an_ordinary_pushed_block_is_drawn_as_stored() {
        let stone = lodestone_data::block_states::state_id("minecraft:stone").expect("stone");
        let nbt = MovingPistonNbt {
            moved_state: stone,
            direction: [0, 0, 1],
            progress: 0.0,
            extending: true,
            source: false,
        };
        assert_eq!(moving_piston_states(&nbt, 0.25), Some((stone, None)));
        // A pushed block belonging to a *source* piston that is extending is still
        // branch 3 — `isSourcePiston` alone does not select branch 2.
        let extending_source = MovingPistonNbt {
            source: true,
            ..nbt.clone()
        };
        assert_eq!(
            moving_piston_states(&extending_source, 0.25),
            Some((stone, None)),
            "branch 2's guard is `isSourcePiston && !isExtending`, both halves"
        );
    }

    /// The NBT decode reads each field at the tag type `saveAdditional` writes:
    /// `facing` as a **byte**, `progress` as a **float**, `extending`/`source` as
    /// bytes.
    ///
    /// Reading `facing` as an int is the shipped-bug shape — it would default every
    /// piston to `DOWN` while the parse still looked clean — so the negative arm
    /// hands it an `Nbt::Int` with the *right value* and requires the decode to
    /// decline rather than to succeed by coincidence.
    #[test]
    fn the_nbt_decode_is_keyed_by_tag_type_not_only_by_field_name() {
        use lodestone_core::Nbt;

        let block_state = Nbt::Compound(vec![(
            "Name".into(),
            Nbt::String("minecraft:stone".into()),
        )]);
        let good = Nbt::Compound(vec![
            ("blockState".into(), block_state.clone()),
            ("facing".into(), Nbt::Byte(1)),
            ("progress".into(), Nbt::Float(0.5)),
            ("extending".into(), Nbt::Byte(1)),
            ("source".into(), Nbt::Byte(0)),
        ]);
        let decoded = moving_piston_nbt(&good).expect("a decodable record");
        assert_eq!(decoded.direction, [0, 1, 0]);
        assert_eq!(decoded.progress, 0.5);
        assert!(decoded.extending);
        assert!(!decoded.source);

        // `facing` at the wrong tag type, same value.
        let wrong_tag = Nbt::Compound(vec![
            ("blockState".into(), block_state.clone()),
            ("facing".into(), Nbt::Int(1)),
        ]);
        assert_eq!(
            moving_piston_nbt(&wrong_tag),
            None,
            "an int `facing` must be declined, not silently defaulted"
        );

        // Absent `extending`/`source`/`progress` are vanilla's `getBooleanOr`/
        // `getFloatOr` defaults, which are real states rather than decode failures.
        let sparse = Nbt::Compound(vec![
            ("blockState".into(), block_state),
            ("facing".into(), Nbt::Byte(0)),
        ]);
        let decoded = moving_piston_nbt(&sparse).expect("a sparse record still decodes");
        assert_eq!(decoded.progress, 0.0);
        assert!(!decoded.extending);
        assert!(!decoded.source);

        // An air moved state draws nothing — `!blockState.isAir()`.
        let air = Nbt::Compound(vec![
            (
                "blockState".into(),
                Nbt::Compound(vec![("Name".into(), Nbt::String("minecraft:air".into()))]),
            ),
            ("facing".into(), Nbt::Byte(0)),
        ]);
        assert_eq!(moving_piston_nbt(&air), None);
    }

    /// The clock ramps by exactly `0.5` per tick and reaches `1.0` in **two** ticks
    /// — `TICKS_TO_EXTEND`, not the plausible ten a chest lid takes.
    ///
    /// The whole sequence is predicted, and the discovery tick is asserted to be a
    /// *seed* rather than an advance: a tracker that advanced on discovery would read
    /// `0.5` on the first observation and finish the push in one tick, halving an
    /// animation that is only two ticks long to begin with.
    #[test]
    fn a_push_ramps_by_half_a_tick_and_completes_in_two() {
        let present = [(PISTON_POS, 0.0_f32)];
        let mut moves = PistonMoves::new();

        moves.tick(&present);
        assert_eq!(
            moves.progress(PISTON_POS, 1.0),
            Some(0.0),
            "the discovery tick seeds and must not advance"
        );

        let mut wrong: Vec<String> = Vec::new();
        for expected in [0.5_f32, 1.0, 1.0] {
            moves.tick(&present);
            let got = moves.progress(PISTON_POS, 1.0);
            if got != Some(expected) {
                wrong.push(format!("expected {expected}, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// The partial-tick lerp reads between the previous and the current tick's
    /// progress, and out-of-range alphas clamp rather than extrapolating.
    ///
    /// Without the lerp a two-tick animation is two frames of a 60 fps second — a
    /// snap, not a slide. The mid-tick value is predicted (`0.25`, half way through
    /// the `0.0 -> 0.5` step) rather than merely required to lie between the ends,
    /// which a stepped counter also satisfies at the endpoints.
    #[test]
    fn progress_interpolates_within_a_tick() {
        let present = [(PISTON_POS, 0.0_f32)];
        let mut moves = PistonMoves::new();
        moves.tick(&present); // seed
        moves.tick(&present); // previous 0.0 -> progress 0.5
        assert_eq!(moves.progress(PISTON_POS, 0.0), Some(0.0));
        assert_eq!(moves.progress(PISTON_POS, 0.5), Some(0.25));
        assert_eq!(moves.progress(PISTON_POS, 1.0), Some(0.5));
        assert_eq!(moves.progress(PISTON_POS, 4.0), Some(0.5), "clamped, not extrapolated");
        assert_eq!(moves.progress(PISTON_POS, -1.0), Some(0.0));
    }

    /// An untracked position reports `None`, **not** `0.0`.
    ///
    /// This is the one place where the chest lid's "absent equals at rest" shortcut
    /// would be actively harmful, and the difference is worth a gate of its own:
    /// `0.0` is the *most displaced* progress a piston has, so a `0.0` here would
    /// draw a head a full cell inside the piston base. Paired with the removal
    /// half — a cell that stops holding a moving piston is forgotten, so the map
    /// cannot grow as a player walks past contraptions.
    #[test]
    fn an_untracked_piston_is_none_rather_than_zero_and_a_finished_one_is_forgotten() {
        let mut moves = PistonMoves::new();
        assert_eq!(moves.progress(PISTON_POS, 1.0), None);
        assert!(moves.is_empty());

        moves.tick(&[(PISTON_POS, 0.0)]);
        assert_eq!(moves.len(), 1);
        // The cell no longer holds a moving piston: the server's `finalTick` has
        // replaced it.
        moves.tick(&[]);
        assert!(moves.is_empty(), "{} entries retained", moves.len());
        assert_eq!(moves.progress(PISTON_POS, 1.0), None);
    }

    /// A seed from the wire is honoured rather than overwritten with zero, and it is
    /// clamped into `0..=1`.
    ///
    /// Vanilla writes `progressO` into the update tag, so a client that joins
    /// mid-push is told where the push already is. Seeding at zero instead would
    /// restart every in-flight contraption on chunk load — visible as a stutter that
    /// looks like a network problem.
    #[test]
    fn the_wire_seed_is_honoured_and_clamped() {
        let mut moves = PistonMoves::new();
        moves.tick(&[(PISTON_POS, 0.5)]);
        assert_eq!(moves.progress(PISTON_POS, 1.0), Some(0.5));
        moves.tick(&[(PISTON_POS, 0.5)]);
        assert_eq!(
            moves.progress(PISTON_POS, 1.0),
            Some(1.0),
            "a push seeded half way finishes one tick later, not two"
        );

        let mut moves = PistonMoves::new();
        moves.tick(&[([0, 0, 0], 9.0)]);
        assert_eq!(moves.progress([0, 0, 0], 1.0), Some(1.0), "clamped");
        let mut moves = PistonMoves::new();
        moves.tick(&[([0, 0, 0], -3.0)]);
        assert_eq!(moves.progress([0, 0, 0], 1.0), Some(0.0), "clamped");
    }

    /// `moving_piston` is a real block in the 26.2 table and every one of its states
    /// is recognised — the gather's whole entry condition.
    ///
    /// The negative arm matters as much: a `piston`, a `sticky_piston` and a
    /// `piston_head` must **not** be recognised, or the gather would draw a moving
    /// copy of every static piston in the world on top of the terrain mesh.
    #[test]
    fn only_moving_piston_states_enter_the_gather() {
        let mut moving = 0usize;
        let mut wrong: Vec<String> = Vec::new();
        for id in 0..lodestone_data::block_states::STATE_COUNT {
            let name = lodestone_data::block_states::block_name(id);
            match name {
                Some("minecraft:moving_piston") => {
                    moving += 1;
                    if !is_moving_piston(id) {
                        wrong.push(format!("{id} is a moving piston but was not recognised"));
                    }
                }
                Some("minecraft:piston" | "minecraft:sticky_piston" | "minecraft:piston_head") => {
                    if is_moving_piston(id) {
                        wrong.push(format!("{id} ({name:?}) was recognised as a moving piston"));
                    }
                }
                _ => {}
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
        // `moving_piston` is `facing` (6) x `type` (2).
        assert_eq!(
            moving, 12,
            "expected 12 `moving_piston` states (6 facings x 2 types)"
        );
    }
}
