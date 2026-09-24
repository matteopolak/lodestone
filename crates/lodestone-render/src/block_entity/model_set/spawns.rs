///
/// **The only spawn in this module whose every animated field is client-simulated
/// with nothing on the wire.** Vanilla's own book-animation tick runs
/// on the client, driven by the nearest player's position, and the server sends
/// none of `time`/`open`/`flip`/`rot` — so a source that captured a stale copy of
/// this state freezes the book, and there is no packet whose absence would
/// explain it.
///
/// Interpolation belongs to the caller (vanilla's own render-state extraction
/// does it, not the submit step): `open` and `flip` are `lerp(partialTicks, o*, *)`,
/// `time` is `time + partialTicks`, and `y_rot` is the **shortest-arc** lerp of
/// `o_rot`→`rot`. That last one is not an ordinary lerp — see
/// [`Self::y_rot`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnchantingTableSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The book's facing, in **radians**, already shortest-arc interpolated:
    /// `oRot + wrap(rot - oRot) * partialTicks`, where `wrap` brings the delta
    /// into `-PI..PI`.
    ///
    /// Skipping the wrap makes the book spin the long way round — a full
    /// backwards revolution in one tick — every time the angle crosses `±PI`,
    /// which happens whenever a player walks past the north-west corner. A plain
    /// `lerp` is wrong in exactly one place and looks right everywhere else.
    pub y_rot: f32,
    /// The block entity's own `time + partialTicks` — vanilla's raw tick
    /// counter, feeding both the hover and the openness breath.
    pub time: f32,
    /// `lerp(partialTicks, oOpen, open)`, `0..1`: how far the book has opened.
    /// `0` is fully shut, which is a **closed book** and not an absent one —
    /// [`enchanting_table_book_openness`] returns `0`, and [`book_part_poses`]
    /// at openness `0` puts `left_lid` at `PI` against `right_lid` at `0`, i.e.
    /// the covers folded together over six real posed parts.
    /// Vanilla's own submit step has no early return: vanilla draws a book
    /// for every enchanting table it renders, and the nearest-player test
    /// decides only whether it opens. A caller that skips a shut book therefore
    /// deletes every table nobody is standing at, which is exactly what this
    /// field's doc used to license.
    pub open: f32,
    /// `lerp(partialTicks, oFlip, flip)` — the page-flip accumulator, **not** a
    /// `0..1` phase. It is unbounded and drifts in either direction; the two
    /// phases come out of [`enchanting_table_page_flips`].
    pub flip: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl EnchantingTableSpawn {
    /// A fully-open, resting, full-bright book over the table at `pos` — the
    /// minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        EnchantingTableSpawn {
            pos,
            y_rot: 0.0,
            time: 0.0,
            open: 1.0,
            flip: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one lectern's book to draw this frame.
///
/// Two fields and no animation state at all, which makes this the cheapest type
/// in the module: the lectern block's own HAS_BOOK property decides whether
/// there is a spawn to make in the first place (a bookless lectern draws
/// nothing here — its shelf is a real block model), and `FACING` gives the
/// yaw. There is no NBT read and
/// nothing on the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LecternSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The clockwise-rotated yaw of the lectern's own FACING property, in degrees
    /// — see [`horizontal_facing_clockwise_yaw`], which is the only correct way
    /// to produce this. Passing the facing's bare yaw puts the book sideways.
    pub facing_yaw_deg: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl LecternSpawn {
    /// A north-facing, full-bright lectern book at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        LecternSpawn {
            pos,
            facing_yaw_deg: horizontal_facing_clockwise_yaw("north").unwrap_or(270.0),
            light: ENTITY_FULLBRIGHT,
        }
    }
}

impl Default for BlockEntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

/// The version-free description of one chest to draw this frame.
///
/// The caller owns every field: block state → `facing_yaw_deg`/`half`, block
/// path → `material`, block event viewer count → `openness`, world light →
/// `light`. Keeping this a plain struct is what stops the render crate depending
/// on a protocol version or a client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChestSpawn {
    /// Block position (the block's minimum corner, in world coordinates).
    pub pos: [i32; 3],
    /// The facing direction's yaw, of the chest's `facing` property.
    pub facing_yaw_deg: f32,
    /// Which layer to draw.
    pub half: ChestHalf,
    /// Which sheet to draw with.
    pub material: ChestMaterial,
    /// **Raw** openness in `0..=1` — the eased value is computed here, so a
    /// caller that already eased would double-ease.
    pub openness: f32,
    /// Packed sky/block light (`sky << 4 | block`) at this block. Pass
    /// [`ENTITY_FULLBRIGHT`] only when there is genuinely no world to sample.
    pub light: u8,
}

impl ChestSpawn {
    /// A closed, full-bright, south-facing single chest at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ChestSpawn {
            pos,
            facing_yaw_deg: 0.0,
            half: ChestHalf::Single,
            material: ChestMaterial::Regular,
            openness: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one skull/head to draw this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: block
/// state → `orientation`/`skull_type`, world light → `light`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkullSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Floor or wall placement.
    pub orientation: SkullOrientation,
    /// Which mob's model and sheet.
    pub skull_type: SkullType,
    /// Static skull sheet or the URL of a placed player head's remote skin.
    pub texture: BlockEntityTexture,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl SkullSpawn {
    /// A floor-placed, `rotation_segment = 0`, full-bright skeleton skull at
    /// `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        SkullSpawn {
            pos,
            orientation: SkullOrientation::Floor { rotation_segment: 0 },
            skull_type: SkullType::Skeleton,
            texture: BlockEntityTexture::Static(skull_texture_stem(SkullType::Skeleton)),
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one bell to draw this frame.
///
/// Unlike [`ChestSpawn`]/[`SkullSpawn`], placement needs no facing at all:
/// Vanilla's own bell renderer applies no rotation of its own before
/// submitting the model (contrast the chest renderer's explicit
/// rotate-around-pivot step), so every `FACING`/`ATTACHMENT` combination poses the
/// body identically — only the block's own attachment-frame *model* (drawn
/// by the ordinary block mesher, not this pass) differs per attachment.
/// [`BlockEntityModelSet::resolve_bell`] therefore calls
/// [`block_entity_placement_matrix`] with a fixed `facing_yaw_deg` of `0.0`,
/// reusing the chest's placement function unchanged rather than adding a
/// bell-specific one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BellSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The in-progress shake (direction plus vanilla's raw tick counter,
    /// `0..50`), or `None` at rest.
    ///
    /// **`None` is the only value this pass can produce today.** The
    /// block-event trigger that starts a shake (`b0 == 1`, direction packed
    /// in `b1` — vanilla's own bell trigger-event handler) is not wired from any
    /// gather in this crate; see `docs/block-entity-renderers.md`'s Bell
    /// section for exactly what is missing and why (the install call site is
    /// outside this crate's file ownership for the session that ported the
    /// geometry). A bell always draws — closing the "hole" the doc's chest
    /// section describes for a model-less block entity — it just never
    /// shakes yet.
    pub shake: Option<(BellShakeDirection, f32)>,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BellSpawn {
    /// A resting, full-bright bell at `pos` — the minimum a hermetic gate
    /// needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BellSpawn {
            pos,
            shake: None,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one shulker box to draw this frame.
///
/// Three fields and no animation state, which is why this type was the cheapest
/// one to add after bell: the box's facing and its dye colour both come straight
/// off the block state (`FACING`, and the block id for the colour), and a closed
/// box needs no part override — so a shulker box slots into
/// [`plan_block_entities`]' existing `(model, texture)` batch key untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShulkerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// The shulker box block's own FACING property, defaulting to
    /// [`ShulkerFacing::Up`] the way vanilla's own render-state extraction
    /// does.
    pub facing: ShulkerFacing,
    /// The dye colour name (`"red"`, …) or `None` for the undyed box.
    pub colour: Option<&'static str>,
    /// Vanilla's own open/close progress accessor — `0.0` closed, `1.0`
    /// fully open.
    ///
    /// **`0.0` is the only value this pass can produce today.** Progress comes
    /// from the block entity's own open/close counter, which the server drives
    /// through the same block-event path a chest lid uses — and unlike a chest,
    /// nothing in this workspace folds a shulker box's event yet. A closed box is
    /// what a shulker box looks like whenever nobody has it open, so this is the
    /// honest state rather than a placeholder; see
    /// `docs/block-entity-renderers.md`.
    pub progress: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl ShulkerSpawn {
    /// A closed, upward-facing, undyed, full-bright box at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ShulkerSpawn {
            pos,
            facing: ShulkerFacing::Up,
            colour: None,
            progress: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// The version-free description of one banner — standing **or** wall — to draw
/// this frame.
///
/// The caller owns every field, the same contract as [`ChestSpawn`]: the
/// `ROTATION` property or `FACING`, whichever the block has → `attachment`; the
/// banner **block's own** colour (vanilla's own per-block-registration colour
/// — one banner block per dye colour, there is no `type`-style state
/// property, so this is
/// not read off block state the way [`ChestSpawn::material`] is) →
/// `base_color`; the block entity's own NBT `"patterns"` key
/// (`docs/banner-shield-patterns.md`'s "Prerequisite 1 does not block the
/// block-entity consumer" section — this is *not* an item component) →
/// `patterns`; the world clock → `phase` (see [`banner_phase`]); world light
/// → `light`.
///
/// Everything past `attachment` is shared by both forms, including the sway and
/// the whole pattern-layer stack — vanilla's own banner renderer picks two
/// meshes and an angle off the attachment type and then runs one banner
/// submit step for either.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerSpawn {
    /// Block position.
    pub pos: [i32; 3],
    /// Standing or wall, carrying that form's own angle — see
    /// [`BannerAttachment`], and [`banner_ground_placement_matrix`] for why a
    /// rotation segment is not [`horizontal_facing_yaw`]'s convention.
    pub attachment: BannerAttachment,
    /// The banner block's own dye colour.
    pub base_color: DyeColor,
    /// The block entity's stored pattern layers, in stack order.
    pub patterns: Vec<StoredPatternLayer>,
    /// This frame's cloth-sway phase, `0.0..1.0` — see [`banner_phase`].
    pub phase: f32,
    /// Packed sky/block light. Pass [`ENTITY_FULLBRIGHT`] only when there is
    /// genuinely no world to sample.
    pub light: u8,
}

impl BannerSpawn {
    /// A resting (`phase = 0`), full-bright, segment-`0` **standing**,
    /// pattern-less white banner at `pos` — the minimum a hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        BannerSpawn {
            pos,
            attachment: BannerAttachment::Ground { rotation_segment: 0 },
            base_color: DyeColor::White,
            patterns: Vec::new(),
            phase: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }

    /// The wall sibling of [`Self::at`]: a resting, full-bright, pattern-less
    /// white banner on a wall facing `facing_yaw_deg`.
    #[must_use]
    pub fn on_wall(pos: [i32; 3], facing_yaw_deg: f32) -> Self {
        BannerSpawn {
            attachment: BannerAttachment::Wall { facing_yaw_deg },
            ..BannerSpawn::at(pos)
        }
    }
}

/// One item cooking in one campfire slot.
///
/// **The only `*Spawn` here that [`BlockEntityModelSet`] does not resolve**, and
/// deliberately so: a campfire's renderer draws item *models*, not a cuboid part
/// rig, so this feeds the model pipeline through
/// [`crate::entity::campfire_item_mesh`] the way a dropped item does — see
/// [`campfire_item_matrix`]'s doc for why there is no mesh and no sheet on this
/// path at all. Sending it through `resolve_*` would need a texture stem that
/// does not exist.
///
/// One per **occupied** slot, so a campfire holding two steaks yields two of
/// these and an empty campfire yields none — matching vanilla's own submit
/// step's per-slot non-empty guard.
#[derive(Debug, Clone, PartialEq)]
pub struct CampfireItemSpawn {
    /// Block position of the campfire.
    pub pos: [i32; 3],
    /// The campfire block's `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// Which of the four cooking slots (`0..CAMPFIRE_SLOTS`) this item is in.
    /// Vanilla offsets it by the facing, so this is *not* a world corner —
    /// see [`campfire_item_matrix`].
    pub slot: CampfireSlot,
    /// The item id whose baked geometry to draw, from the block entity's NBT
    /// `Items` list.
    pub item: ResourceLocation,
    /// Packed sky/block light at the campfire.
    pub light: u8,
}

/// One suspicious sand/gravel block's revealed item, for this frame —
/// vanilla's own brushable-block renderer.
///
/// **A second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: vanilla's own brushable-block
/// renderer draws an *item model*, not a cuboid part rig — the sand/gravel a player
/// sees is the ordinary **block** model, real geometry the terrain mesher
/// already draws (`suspicious_sand`/`suspicious_gravel` are not a hole in the
/// world), so this feeds the model pipeline through
/// [`crate::entity::brushable_item_mesh`] the way a dropped item does.
///
/// Present only once **both** vanilla's own hit-direction accessor is
/// non-null (a player has brushed at least once) and `item` is non-empty (a
/// loot table has actually rolled a reward) and `dust_progress > 0` —
/// vanilla's own three-part guard in its own submit step. A brand
/// new, never-brushed block therefore contributes no spawn at all.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushableItemSpawn {
    /// Block position of the suspicious sand/gravel.
    pub pos: [i32; 3],
    /// The face a player last brushed, from `hit_direction` NBT
    /// (vanilla's own legacy direction-id codec) — feeds [`brushable_item_matrix`].
    pub hit_direction: lodestone_assets::Direction,
    /// The block state's own `dusted` property, `0..=3` —
    /// vanilla's own completion-state range, read off the
    /// state rather than re-derived from the block entity's own brush counter
    /// (which is not on the wire; only the property is).
    pub dust_progress: u8,
    /// The revealed item's id, from the block entity's `item` NBT
    /// (vanilla's own item-stack codec).
    pub item: ResourceLocation,
    /// Packed sky/block light at the block.
    pub light: u8,
}

/// One item on a shelf's `slot`, for this frame — vanilla's own shelf renderer.
///
/// **A third `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`]/[`BrushableItemSpawn`] are the first
/// two: vanilla's own shelf renderer draws item models, not a cuboid part rig — a shelf's
/// own board/back/sides are all real block-model geometry the terrain
/// mesher already draws — so this feeds the model pipeline through
/// [`crate::entity::shelf_item_mesh`] the way a dropped item does.
///
/// One per **occupied** slot, matching vanilla's own submit step's per-slot
/// non-null render-state guard — an empty shelf yields none.
#[derive(Debug, Clone, PartialEq)]
pub struct ShelfItemSpawn {
    /// Block position of the shelf.
    pub pos: [i32; 3],
    /// The shelf block's `facing`, in [`horizontal_facing_yaw`]'s convention.
    pub facing_yaw_deg: f32,
    /// Which of the three slots (`0..SHELF_SLOTS`) this item is in.
    pub slot: ShelfSlot,
    /// Vanilla's own align-items-to-bottom accessor's own NBT flag.
    pub align_to_bottom: bool,
    /// The item id whose baked geometry to draw, from the block entity's
    /// `Items` NBT list.
    pub item: ResourceLocation,
    /// Packed sky/block light at the shelf.
    pub light: u8,
}

/// One vault's floating display-item cluster, for this frame — vanilla's
/// own vault renderer.
///
/// **A third `*Spawn` here [`BlockEntityModelSet`] does not resolve**, for the
/// same reason [`CampfireItemSpawn`] is the first: vanilla's own vault-submit
/// step draws multiple item-entity-style instances at a fixed pose, not a cuboid
/// part rig — the vault's own cage, base and door are all real *block* model
/// geometry the ordinary terrain mesher already draws (`blockstates/vault.json`
/// is a plain `variants` map over `facing`/`ominous`/`vault_state`, the same
/// shape the mob-spawner cage and trial-spawner's per-state textures already
/// proved), so this feeds the model pipeline through
/// [`crate::entity::vault_display_item_mesh`] the way a dropped item does.
///
/// Present only when vanilla's own client-side active-effects check
/// (its own has-display-item test) is true — an empty `shared_data.display_item`
/// yields **no** spawn for that vault, matching vanilla's own
/// non-empty guard in its own render-state extraction. A
/// vault the server has not yet rolled a reward for (state `INACTIVE`) is
/// therefore silent, not a partially-drawn cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct VaultSpawn {
    /// Block position of the vault.
    pub pos: [i32; 3],
    /// The display item's id, from `shared_data.display_item.id`.
    pub item: ResourceLocation,
    /// The display item's stack count, from `shared_data.display_item.count`
    /// (vanilla's codec defaults this to `1` when absent) — feeds
    /// [`crate::entity::rendered_amount`] the same way a dropped stack's count
    /// does.
    pub count: u32,
    /// This frame's spin, in degrees — [`crate::entity::vault_spin_degrees`]
    /// evaluated at the gather's `(game_time, partial_tick)`, already resolved
    /// so the draw site needs no clock of its own.
    pub spin_deg: f32,
    /// Packed sky/block light at the vault.
    pub light: u8,
}

/// One `moving_piston` block entity for this frame — vanilla's own
/// piston-head renderer.
///
/// **The second `*Spawn` here [`BlockEntityModelSet`] does not resolve**, and for
/// the same reason [`CampfireItemSpawn`] is the first: vanilla's own
/// piston-head renderer's constructor bakes no model layer, so it owns no
/// cuboid rig. What it draws is
/// whole *block models* posed somewhere other than their own cell, which is the
/// moving-block seam (`gpu/moving_blocks.rs` in the shell) rather than either the
/// entity or the item pipeline.
///
/// # Everything here is semantic, not a matrix
///
/// The offset is deliberately **not** precomputed into a `Mat4` by the gather.
/// Vanilla's own extended-progress calculation is the one piece of arithmetic
/// in this renderer that a plausible reading gets backwards — it is
/// `progress - 1.0` while extending and `1.0 - progress` while retracting, and
/// the two agree at `progress == 0.5` — so
/// it lives next to its sibling `falling_block_pose` where its wrong hypothesis is
/// evaluated against it. Carrying `direction`/`progress`/`extending` keeps that
/// possible.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MovingPistonSpawn {
    /// The cell the `moving_piston` block entity itself occupies. Geometry draws
    /// at this cell **plus** the offset derived from the three fields below; the
    /// cell itself has no block model (`moving_piston` renders as
    /// invisible), which is why an unset source leaves a hole.
    pub pos: [i32; 3],
    /// The global block-state id to draw offset — already resolved by the gather,
    /// because two of vanilla's own render-state extraction's three branches
    /// *synthesise* a state rather than using the stored one (a `piston_head`
    /// with `short` set from the
    /// progress, in particular).
    pub state_id: u32,
    /// The retracting **source** piston's own base, drawn at [`Self::pos`] with
    /// no offset at all — vanilla's own submit step pops the translated pose
    /// before submitting it.
    /// `None` for every other case, which is the common one.
    pub base_state_id: Option<u32>,
    /// The block entity's own facing direction's unit step.
    ///
    /// **Not the movement direction.** Vanilla's own movement-direction
    /// accessor is `extending ? direction : direction.opposite()`, but the
    /// per-axis direction-step accessors multiply the *raw* `direction` step
    /// by a signed progress that carries the
    /// retraction's sign itself. Using the movement direction here and a positive
    /// progress would double-negate the retracting case.
    pub direction: [i32; 3],
    /// Vanilla's own progress accessor — `lerp(a, progress_o, progress)`, in `0..=1`.
    pub progress: f32,
    /// The block entity's own extending flag.
    pub extending: bool,
    /// Packed sky/block light for the offset geometry. Vanilla samples it one cell
    /// **back** along the movement direction
    /// (the block's own position, offset by the opposite of the movement
    /// direction), not at the block entity's own cell — the cell being moved
    /// *into* is the one full of
    /// `moving_piston`.
    pub light: u8,
    /// Packed sky/block light for [`Self::base_state_id`], sampled at
    /// [`Self::pos`] itself. Ignored when there is no base.
    pub base_light: u8,
}

/// Which block-entity rig and sheet draw one `minecraft:special` **item** form —
/// vanilla's own special-model-renderer family, the ex-`builtin/entity` items.
///
use lodestone_assets::ResourceLocation;
use lodestone_model::{CampfireSlot, ShelfSlot};

use crate::banner_pattern::{DyeColor, StoredPatternLayer};
use crate::entity::ENTITY_FULLBRIGHT;

use super::resolvers::BlockEntityModelSet;
use super::super::batching::BlockEntityTexture;
use super::super::model_families::*;
