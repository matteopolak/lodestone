//! A connection's held brush: the raycast, the stroke cadence and the world writes a stroke makes.
//!
//! The block-entity rules live in [`crate::archaeology`]; this file applies them to the world.
//! A use starts from a right-click on a block, runs for [`USE_TICKS`], and on every tick
//! re-aims; a tick whose ray misses every block ends the use. Every tenth tick, starting at the
//! fifth, is a stroke.

use super::*;
use crate::archaeology::{PendingLoot, Stroke};
use crate::effects::WorldEffect;
use crate::loot::{LootContext, LootTableSet};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, ItemStack, ResourceKey, Vec3, Vec3f};

/// The registry name of the brush item.
pub(super) const BRUSH_ITEM: &str = "minecraft:brush";
/// Ticks one use lasts before it ends on its own.
pub(super) const USE_TICKS: u64 = 200;
/// Ticks between two strokes.
const STROKE_PERIOD: u64 = 10;
/// The tick within each period a stroke lands on.
const STROKE_PHASE: u64 = 5;

/// Whether `stack` is a brush.
pub(super) fn is_brush(stack: Option<&ItemStack>) -> bool {
    stack.is_some_and(|stack| stack.item.to_string() == BRUSH_ITEM)
}

/// The use a brush right-click starts, stamped at `now` on the mob clock.
pub(super) fn begin(native: usize, now: u64) -> ItemInUse {
    ItemInUse {
        native,
        item: BRUSH_ITEM.to_owned(),
        finish_tick: now + USE_TICKS,
        last_effect_remaining: None,
    }
}

/// What the caller owes after a span of brush ticks.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct BrushTick {
    /// The ray found no block: the use ends.
    pub(super) release: bool,
    /// Durability the brush loses.
    pub(super) wear: u32,
}

/// The world a brush acts on.
pub(super) struct BrushWorld<'a, S: ChunkSource + ?Sized> {
    pub(super) source: &'a S,
    pub(super) block_entities: &'a BlockEntityHandle,
    pub(super) block_ticks: &'a BlockTickFeed,
    pub(super) mobs: &'a MobHandle,
    pub(super) tables: &'a LootTableSet,
    pub(super) game_time: u64,
    pub(super) creative: bool,
}

/// Where the player's eye is and which way it looks.
pub(super) struct Aim {
    pub(super) feet: Vec3,
    pub(super) yaw: f32,
    pub(super) pitch: f32,
}

/// The block under the crosshair and the face the ray entered through.
#[derive(Debug, Clone, Copy)]
struct Target {
    cell: BlockPos,
    face: BlockFace,
    point: Vec3,
}

fn target<S: ChunkSource + ?Sized>(world: &BrushWorld<'_, S>, aim: &Aim) -> Option<Target> {
    let eye = Vec3::new(aim.feet.x, aim.feet.y + EYE_HEIGHT, aim.feet.z);
    let reach = crate::boat::block_interaction_range(world.creative);
    let view = crate::boat::view_direction(aim.yaw, aim.pitch);
    let end = Vec3::new(eye.x + view.x * reach, eye.y + view.y * reach, eye.z + view.z * reach);
    let hit = crate::boat::clip(eye, end, &|x, y, z| world.source.block_state_id(x, y, z))?;
    Some(Target { cell: hit.cell, face: entered_face(hit.cell, hit.position, view), point: hit.position })
}

/// The face of `cell` the ray entered through: the one whose plane the hit point lies on, with
/// the ray heading against that face's normal.
fn entered_face(cell: BlockPos, point: Vec3, view: Vec3) -> BlockFace {
    let local = [point.x - f64::from(cell.x), point.y - f64::from(cell.y), point.z - f64::from(cell.z)];
    let heading = [view.x, view.y, view.z];
    let mut best = (BlockFace::Up, f64::MAX);
    for (axis, (low, high)) in [
        (BlockFace::West, BlockFace::East),
        (BlockFace::Down, BlockFace::Up),
        (BlockFace::North, BlockFace::South),
    ]
    .into_iter()
    .enumerate()
    {
        let (face, gap) = if heading[axis] > 0.0 { (low, local[axis].abs()) } else { (high, (1.0 - local[axis]).abs()) };
        if gap < best.1 {
            best = (face, gap);
        }
    }
    best.0
}

fn offset(pos: BlockPos, face: BlockFace) -> BlockPos {
    let (dx, dy, dz) = match face {
        BlockFace::Down => (0, -1, 0),
        BlockFace::Up => (0, 1, 0),
        BlockFace::North => (0, 0, -1),
        BlockFace::South => (0, 0, 1),
        BlockFace::West => (-1, 0, 0),
        BlockFace::East => (1, 0, 0),
    };
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// Whether a right-click at `aim` lands on any block, which is all a use needs to start.
pub(super) fn aims_at_block<S: ChunkSource + ?Sized>(world: &BrushWorld<'_, S>, aim: &Aim) -> bool {
    target(world, aim).is_some()
}

/// Runs the brush ticks `from_elapsed + 1 ..= to_elapsed` of a use.
pub(super) fn tick<S: ChunkSource + ?Sized>(
    world: &BrushWorld<'_, S>,
    aim: &Aim,
    rng: &mut SpawnRng,
    from_elapsed: u64,
    to_elapsed: u64,
) -> BrushTick {
    let mut out = BrushTick::default();
    for elapsed in from_elapsed + 1..=to_elapsed {
        let Some(hit) = target(world, aim) else {
            out.release = true;
            return out;
        };
        if elapsed % STROKE_PERIOD != STROKE_PHASE {
            continue;
        }
        out.wear += stroke(world, hit, rng);
    }
    out
}

/// The sound a stroke on `state` makes.
fn stroke_sound(state: StateId) -> &'static str {
    match state.name() {
        "minecraft:suspicious_sand" => "minecraft:item.brush.brushing.sand",
        "minecraft:suspicious_gravel" => "minecraft:item.brush.brushing.gravel",
        _ => "minecraft:item.brush.brushing.generic",
    }
}

/// One stroke: the sound and dust, then the block entity's count. Returns the wear it costs.
fn stroke<S: ChunkSource + ?Sized>(world: &BrushWorld<'_, S>, hit: Target, rng: &mut SpawnRng) -> u32 {
    let pos = hit.cell;
    let state = world.source.block_state_id(pos.x, pos.y, pos.z);
    let centre = Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y) + 0.5, f64::from(pos.z) + 0.5);
    world.block_ticks.publish_effect(WorldEffect::Sound {
        sound: stroke_sound(state).to_owned(),
        category: lodestone_model::SoundCategory::Block,
        pos: centre,
        volume: 1.0,
        pitch: 1.0,
        seed: i64::from(rng.next_int(i32::MAX)),
    });
    world.block_ticks.publish_effect(WorldEffect::BlockParticles {
        state,
        pos: hit.point,
        offset: Vec3f { x: 0.15, y: 0.1, z: 0.15 },
        max_speed: 0.05,
        count: 7 + rng.next_int(5),
    });
    if !crate::structure_loot::is_brushable_block(state.name()) {
        return 0;
    }
    world.block_entities.with(|registry| {
        if !matches!(registry.get(pos), Some(crate::block_entities::BlockEntity::Brushable(_))) {
            let held = world
                .source
                .block_entity(pos.x, pos.y, pos.z)
                .filter(|entity| matches!(entity, crate::block_entities::BlockEntity::Brushable(_)))
                .unwrap_or_else(|| crate::block_entities::BlockEntity::Brushable(Box::default()));
            registry.insert(pos, held);
        }
    });
    let outcome = world.block_entities.with(|registry| {
        let Some(crate::block_entities::BlockEntity::Brushable(entity)) = registry.get_mut(pos) else { return None };
        let stroke = entity.brush(world.game_time, hit.face, |loot| roll(world.tables, loot));
        Some((stroke, entity.hit_direction()))
    });
    let Some((stroke, _)) = outcome else { return 0 };
    match stroke {
        Stroke::Cooldown => 0,
        Stroke::Counted { dusted } => {
            world.block_entities.with(|registry| registry.mark_client_dirty(pos));
            world.block_ticks.request_scheduled_ticks(crate::gravity_tick::ticks_after_place_id(pos, state));
            if let Some(dusted) = dusted
                && let Some(next) = crate::archaeology::with_dusted(state, dusted)
            {
                world.source.set_block(pos.x, pos.y, pos.z, next);
                world.block_ticks.publish_change(pos.x, pos.y, pos.z, state, next);
            }
            0
        }
        Stroke::Finished => {
            complete(world, pos, state, rng);
            1
        }
    }
}

/// The first item of the table the entity names, rolled from its own seed.
fn roll(tables: &LootTableSet, loot: &PendingLoot) -> Option<ItemStack> {
    let table: ResourceKey = loot.table.parse().ok()?;
    let mut rng = SpawnRng::new(loot.seed as u64);
    tables.roll(&table, &LootContext::default(), &mut rng).into_iter().next()
}

/// The tenth stroke: the item drops from the face the first stroke hit, the block becomes its
/// base block, and clients get the completion event.
fn complete<S: ChunkSource + ?Sized>(world: &BrushWorld<'_, S>, pos: BlockPos, state: StateId, rng: &mut SpawnRng) {
    let removal = world.block_entities.with(|registry| registry.destroy_at(pos, state));
    if let Some(crate::block_entities::BlockEntity::Brushable(mut entity)) = removal.entity {
        let face = entity.hit_direction().unwrap_or(BlockFace::Up);
        let amount = 10 + u32::try_from(rng.next_int(21)).unwrap_or(0);
        if let Some(item) = entity.take_item(amount) {
            drop_item(world, offset(pos, face), &item);
        }
    }
    if let Some(effect) = crate::effects::brush_completed(pos, state) {
        world.block_ticks.publish_effect(effect);
    }
    let turned = crate::archaeology::base_block(state).unwrap_or_else(crate::chunk::air_state);
    world.source.set_block(pos.x, pos.y, pos.z, turned);
    world.block_ticks.publish_change(pos.x, pos.y, pos.z, state, turned);
    world.block_ticks.request_scheduled_ticks(crate::gravity_tick::ticks_after_place_id(pos, turned));
}

/// Width and height of an item entity, which place the dropped stack in the cell it falls from.
const ITEM_WIDTH: f64 = 0.25;
const ITEM_HEIGHT: f64 = 0.25;

fn drop_item<S: ChunkSource + ?Sized>(world: &BrushWorld<'_, S>, cell: BlockPos, stack: &ItemStack) {
    let spread = 1.0 - ITEM_WIDTH;
    let at = Vec3::new(
        f64::from(cell.x) + 0.5 * spread + ITEM_WIDTH / 2.0,
        f64::from(cell.y) + 0.5 + ITEM_HEIGHT / 2.0,
        f64::from(cell.z) + 0.5 * spread + ITEM_WIDTH / 2.0,
    );
    let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
    world.mobs.with(|sim| {
        sim.spawn_item(
            stack,
            at,
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE),
        );
    });
}
