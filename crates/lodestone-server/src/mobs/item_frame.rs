//! `MobSim`'s item-frame slice: spawn, the framed item and its rotation, the
//! hit that pops the item and then the frame, and the periodic support check.
//!
//! # What it is
//!
//! A frame is a block-attached entity like a cushion: stationary, no AI. It
//! lives in its own map ([`super::TrackedFrame`]) and streams from
//! [`MobSim::push_frame_snapshots`] as three metadata fields (facing, framed
//! item, rotation). The geometry and the use-on-block rule are
//! [`crate::item_frame`]'s; this file owns what happens to a placed frame.
//!
//! # How it works
//!
//! * [`MobSim::interact_frame`]: an empty frame takes one held item; a filled
//!   one turns its item an eighth. A fixed frame ignores both.
//! * [`MobSim::hurt_frame`]: a hit with an item in the frame pops the item
//!   (frame stays); a hit on an empty frame breaks it. A fixed frame only
//!   yields to a creative player. Creative breakers drop nothing.
//! * Every 101st tick [`MobSim::plan_frame_checks`] reports frames that no
//!   longer survive; the tick driver breaks them once its world read was complete.
//! * Persistence is `minecraft:item_frame` / `minecraft:glow_item_frame`
//!   records: `Item`, `ItemRotation`, `ItemDropChance`, `Facing`, `Invisible`,
//!   `Fixed` and `block_pos`, through both the Anvil and the native stores.
//! * A frame holding a filled map reports it through
//!   [`MobSim::framed_maps`] so every connection's map session can show it.
//!
//! # How to change it
//!
//! Sounds and the map-marker removal are done by the callers that interact
//! with or break a frame (the play dispatcher and the tick driver), from the
//! `FramedMapRef` the outcomes carry, not here.

use lodestone_core::Nbt;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use super::{MobSim, TrackedFrame};
use crate::entity_record::SavedEntity;
use crate::item_frame::{self, NUM_ROTATIONS, Neighbour};
use crate::protocol::{EntitySnapshot, MetadataField};

/// Ticks a frame waits between support checks; the check runs on the tick
/// after the counter has reached this.
const CHECK_INTERVAL: u32 = 100;
/// Upward speed of a dropped item, in blocks per tick.
const DROP_LIFT: f64 = 0.2;
/// The shared-flags bit that makes an entity invisible.
const FLAG_INVISIBLE: u8 = 0x20;

/// A filled map that a frame holds or held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramedMapRef {
    /// The map's id.
    pub map_id: i32,
    /// The cell the frame hangs in.
    pub cell: BlockPos,
    /// The frame's network entity id.
    pub entity_id: i32,
}

/// What using a frame did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameInteraction {
    /// Nothing happened: an empty hand on an empty frame, or a fixed frame.
    Pass,
    /// The held map is over the marker limit; nothing happened.
    Refused,
    /// One item went in; the caller consumes it from the hand.
    Inserted {
        /// Whether the frame is the glowing kind.
        glow: bool,
        /// The frame's box centre.
        centre: Vec3,
    },
    /// The item turned an eighth.
    Rotated {
        /// Whether the frame is the glowing kind.
        glow: bool,
        /// The frame's box centre.
        centre: Vec3,
    },
}

/// What hitting a frame did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FrameHit {
    /// A fixed frame shrugged it off.
    Ignored,
    /// The item came out and the frame stayed.
    ItemPopped {
        /// Whether the frame is the glowing kind.
        glow: bool,
        /// The frame's box centre.
        centre: Vec3,
        /// The map the frame no longer holds.
        map: Option<FramedMapRef>,
    },
    /// The frame is gone.
    Broken(FrameBroken),
}

/// A removed frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameBroken {
    /// Whether it was the glowing kind.
    pub glow: bool,
    /// Its box centre.
    pub centre: Vec3,
    /// The map it held.
    pub map: Option<FramedMapRef>,
}

impl<'w> MobSim<'w> {
    /// Hangs a frame in `cell` facing `facing` and returns its network entity id.
    pub fn spawn_item_frame(&mut self, cell: BlockPos, facing: BlockFace, glow: bool) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.frames.insert(
            id,
            TrackedFrame {
                uuid: Uuid::new_v4(),
                cell,
                facing,
                glow,
                item: None,
                rotation: 0,
                drop_chance: 1.0,
                invisible: false,
                fixed: false,
                ticks_since_check: 0,
            },
        );
        id
    }

    /// The number of live frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Whether `id` is an item frame.
    #[must_use]
    pub fn is_item_frame(&self, id: i32) -> bool {
        self.frames.contains_key(&id)
    }

    /// A frame's `(cell, facing, item, rotation)`, if `id` is one.
    #[must_use]
    pub fn frame_state(&self, id: i32) -> Option<(BlockPos, BlockFace, Option<ItemStack>, u8)> {
        self.frames.get(&id).map(|f| (f.cell, f.facing, f.item.clone(), f.rotation))
    }

    /// Every frame except `exclude`, as survival sees it.
    #[must_use]
    pub fn frame_neighbours(&self, exclude: Option<i32>) -> Vec<Neighbour> {
        self.frames
            .iter()
            .filter(|(id, _)| Some(**id) != exclude)
            .map(|(_, f)| f.neighbour())
            .collect()
    }

    /// Every frame holding a filled map, for the map sessions to show.
    #[must_use]
    pub fn framed_maps(&self) -> Vec<crate::maps::FramedMap> {
        let mut out: Vec<_> = self
            .frames
            .iter()
            .filter_map(|(&id, f)| {
                let map_id = f.item.as_ref().and_then(crate::maps::filled_map_id)?;
                Some(crate::maps::FramedMap {
                    map_id,
                    entity_id: id,
                    pos: f.cell,
                    rotation: item_frame::data_2d(f.facing) * 90,
                })
            })
            .collect();
        out.sort_by_key(|m| m.entity_id);
        out
    }

    /// Uses frame `id` with `held` in the hand. `map_over_limit` says whether a
    /// map id already carries too many markers to take another frame.
    pub fn interact_frame(
        &mut self,
        id: i32,
        held: Option<&ItemStack>,
        map_over_limit: &dyn Fn(i32) -> bool,
    ) -> FrameInteraction {
        let Some(frame) = self.frames.get_mut(&id) else {
            return FrameInteraction::Pass;
        };
        if frame.fixed {
            return FrameInteraction::Pass;
        }
        let (glow, centre) = (frame.glow, frame.centre());
        if frame.item.is_some() {
            frame.rotation = (frame.rotation + 1) % NUM_ROTATIONS;
            return FrameInteraction::Rotated { glow, centre };
        }
        let Some(held) = held.filter(|stack| stack.count > 0) else {
            return FrameInteraction::Pass;
        };
        if crate::maps::filled_map_id(held).is_some_and(map_over_limit) {
            return FrameInteraction::Refused;
        }
        let mut single = held.clone();
        single.count = 1;
        frame.item = Some(single);
        FrameInteraction::Inserted { glow, centre }
    }

    /// A player hits frame `id`; `creative` breakers drop nothing.
    pub fn hurt_frame(&mut self, id: i32, creative: bool) -> Option<FrameHit> {
        let frame = self.frames.get(&id)?;
        if frame.fixed {
            return if creative { self.kill_frame(id, false).map(FrameHit::Broken) } else { Some(FrameHit::Ignored) };
        }
        if frame.item.is_none() {
            return self.kill_frame(id, !creative).map(FrameHit::Broken);
        }
        let (glow, centre) = (frame.glow, frame.centre());
        let map = frame.map_ref(id);
        let drop_chance = frame.drop_chance;
        let popped = self.frames.get_mut(&id).and_then(|f| f.item.take())?;
        if !creative && self.drop_roll(drop_chance) {
            let at = self.frames.get(&id).map(TrackedFrame::drop_position)?;
            self.drop_frame_stack(&popped, at);
        }
        Some(FrameHit::ItemPopped { glow, centre, map })
    }

    /// Removes frame `id`; with `drops` its item and the framed stack drop.
    /// A fixed frame drops nothing either way.
    pub fn kill_frame(&mut self, id: i32, drops: bool) -> Option<FrameBroken> {
        let frame = self.frames.remove(&id)?;
        if drops && !frame.fixed {
            let at = frame.drop_position();
            self.drop_frame_stack(&ItemStack::new(item_frame::item_key(frame.glow), 1), at);
            if let Some(stack) = &frame.item
                && self.drop_roll(frame.drop_chance)
            {
                self.drop_frame_stack(stack, at);
            }
        }
        Some(FrameBroken { glow: frame.glow, centre: frame.centre(), map: frame.map_ref(id) })
    }

    fn drop_roll(&mut self, chance: f32) -> bool {
        chance >= 1.0 || (chance > 0.0 && self.orb_rng.next_f32() < chance)
    }

    fn drop_frame_stack(&mut self, stack: &ItemStack, at: Vec3) {
        self.spawn_item(
            stack,
            at,
            Vec3::new(0.0, DROP_LIFT, 0.0),
            lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                u8::try_from(stack.count).unwrap_or(1),
                lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
            ),
        );
    }

    /// Advances every frame's check counter one tick and returns the ids whose
    /// check fired and found no surviving place. Nothing is removed: the
    /// driver calls [`kill_frame`](Self::kill_frame) once it knows the world
    /// read behind `state` was complete.
    pub(crate) fn plan_frame_checks(&mut self, state: &dyn Fn(i32, i32, i32) -> StateId) -> Vec<i32> {
        let mut ids: Vec<i32> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        ids.retain(|id| {
            let Some(f) = self.frames.get_mut(id) else {
                return false;
            };
            if f.ticks_since_check < CHECK_INTERVAL {
                f.ticks_since_check += 1;
                return false;
            }
            f.ticks_since_check = 0;
            !f.fixed
        });
        ids.retain(|id| {
            let f = &self.frames[id];
            let others = self.frame_neighbours(Some(*id));
            !item_frame::survives(f.cell, f.facing, state, &others)
        });
        ids
    }

    /// The disk records of every frame.
    pub(super) fn saved_frames(&self) -> Vec<SavedEntity> {
        let mut ids: Vec<i32> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| self.frames.get(&id))
            .map(|f| {
                let (yaw, pitch) = item_frame::yaw_pitch(f.facing);
                SavedEntity {
                    id: item_frame::entity_key(f.glow),
                    uuid: f.uuid,
                    pos: f.centre(),
                    motion: Vec3::new(0.0, 0.0, 0.0),
                    rotation: Rotation::new(yaw, pitch),
                    health: None,
                    item: f.item.clone(),
                    age: None,
                    pickup_delay: None,
                    extra: vec![
                        ("ItemRotation".to_owned(), Nbt::Byte(f.rotation as i8)),
                        ("ItemDropChance".to_owned(), Nbt::Float(f.drop_chance)),
                        ("Facing".to_owned(), Nbt::Byte(item_frame::data_3d(f.facing) as i8)),
                        ("Invisible".to_owned(), Nbt::Byte(i8::from(f.invisible))),
                        ("Fixed".to_owned(), Nbt::Byte(i8::from(f.fixed))),
                        ("block_pos".to_owned(), Nbt::IntArray(vec![f.cell.x, f.cell.y, f.cell.z])),
                    ],
                }
            })
            .collect()
    }

    /// Re-creates one saved frame with its stored uuid. A missing `Facing`
    /// reads as down, and a missing `block_pos` as the cell holding the saved
    /// box centre.
    pub(super) fn restore_frame(&mut self, saved: &SavedEntity) {
        let field = |key: &str| saved.extra.iter().find(|(name, _)| name == key).map(|(_, value)| value);
        let flag = |key: &str| matches!(field(key), Some(Nbt::Byte(v)) if *v != 0);
        let facing = match field("Facing") {
            Some(Nbt::Byte(v)) => item_frame::from_data_3d(i32::from(*v)),
            _ => None,
        }
        .unwrap_or(BlockFace::Down);
        let cell = match field("block_pos") {
            Some(Nbt::IntArray(v)) if v.len() == 3 => BlockPos::new(v[0], v[1], v[2]),
            _ => item_frame::cell_containing(saved.pos),
        };
        let glow = saved.id.path() == "glow_item_frame";
        let id = self.spawn_item_frame(cell, facing, glow);
        let Some(frame) = self.frames.get_mut(&id) else { return };
        frame.uuid = saved.uuid;
        // The Anvil reader leaves a non-item entity's `Item` in the extra fields; the
        // native store hands it over as the record's own stack.
        let item = saved.item.clone().or_else(|| field("Item").and_then(crate::item_nbt::stack_from_nbt));
        frame.item = item.filter(|stack| stack.count > 0).map(|mut stack| {
            stack.count = 1;
            stack
        });
        if let Some(Nbt::Byte(v)) = field("ItemRotation") {
            frame.rotation = (i32::from(*v)).rem_euclid(i32::from(NUM_ROTATIONS)) as u8;
        }
        if let Some(Nbt::Float(v)) = field("ItemDropChance") {
            frame.drop_chance = *v;
        }
        frame.invisible = flag("Invisible");
        frame.fixed = flag("Fixed");
    }

    pub(super) fn push_frame_snapshots(&self, out: &mut Vec<EntitySnapshot>) {
        let mut ids: Vec<i32> = self.frames.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(f) = self.frames.get(&id) else {
                continue;
            };
            let (yaw, pitch) = item_frame::yaw_pitch(f.facing);
            let mut metadata = Vec::with_capacity(4);
            if f.invisible {
                metadata.push(MetadataField::SharedFlags(FLAG_INVISIBLE));
            }
            metadata.push(MetadataField::HangingFacing(item_frame::data_3d(f.facing)));
            metadata.push(match &f.item {
                Some(stack) => MetadataField::FrameItem {
                    item: stack.item.clone(),
                    count: 1,
                    components: (stack.components != lodestone_model::ItemComponents::default())
                        .then(|| std::sync::Arc::new(stack.components.clone())),
                },
                None => MetadataField::FrameItem {
                    item: ResourceKey::new("minecraft", "air").expect("air is a valid key"),
                    count: 0,
                    components: None,
                },
            });
            metadata.push(MetadataField::FrameRotation(f.rotation));
            out.push(EntitySnapshot {
                id,
                uuid: f.uuid,
                entity_type: item_frame::entity_key(f.glow),
                // The spawn packet carries the attachment cell's corner, not the box centre.
                position: item_frame::anchor(f.cell),
                rotation: Rotation::new(yaw, pitch),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                on_ground: false,
                metadata,
                object_data: i32::from(item_frame::data_3d(f.facing)),
                equipment: Vec::new(),
                leash_link: None,
            });
        }
    }
}

impl TrackedFrame {
    fn centre(&self) -> Vec3 {
        item_frame::centre(self.cell, self.facing)
    }

    fn drop_position(&self) -> Vec3 {
        item_frame::drop_position(self.cell, self.facing)
    }

    fn neighbour(&self) -> Neighbour {
        Neighbour {
            cell: self.cell,
            facing: self.facing,
            has_map: self.item.as_ref().is_some_and(|stack| stack.components.map_id.is_some()),
        }
    }

    fn map_ref(&self, entity_id: i32) -> Option<FramedMapRef> {
        let map_id = self.item.as_ref().and_then(crate::maps::filled_map_id)?;
        Some(FramedMapRef { map_id, cell: self.cell, entity_id })
    }
}
