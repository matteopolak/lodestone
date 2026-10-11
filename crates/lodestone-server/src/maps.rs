//! Filled-map saved data: per-id colour grids, decorations, terrain sampling and the
//! per-holder update stream, persisted in the world's `data/minecraft/maps` folder.
//!
//! # What it is
//!
//! [`MapData`] is one map: dimension, centre, scale, flags, the 128x128 colour grid,
//! banner and item-frame markers, and the decoration list shown on top. [`MapHandle`]
//! is the world-wide store (shared by every connection, rides in
//! [`crate::world_state::WorldStateHandle`]); [`MapSession`] is one connection's view:
//! the maps its player carries, the terrain snapshots it samples from, and the packets
//! it owes.
//!
//! # How it works
//!
//! Each server tick a connection calls [`MapSession::tick`]. Every filled map in the
//! inventory is ticked as carried: the carrier gets a marker (player, off-map or
//! off-limits) and a holder record that remembers which pixels and decorations that
//! client still lacks. A map in either hand that is not locked is also sampled
//! ([`MapData::sample_terrain`]): one in sixteen map columns per tick, each pixel the
//! most common map colour among its `scale x scale` blocks, shaded by the height step
//! from the pixel above (or by water depth for water), dithered to black at the rim.
//! Changed pixels dirty every holder; [`MapData::next_update`] turns a holder's dirty
//! rectangle and, every fifth tick, its decoration list into a [`MapUpdate`].
//!
//! The per-state colour comes from [`lodestone_data::map_colors`]. A map's pixel is
//! `colour id << 2 | shade`, shade `0` low, `1` normal, `2` high, `3` lowest.
//!
//! On disk a map is `maps/<id>.dat` and the id counter is `maps/last_id.dat`, both
//! gzip NBT `{data: {...}, DataVersion}` as the reference writes them, so a world moves
//! between this server and the reference.
//!
//! # How to change it
//!
//! - Terrain access is the [`MapTerrain`] trait; the connection's implementation is
//!   [`ColumnCache::terrain`], which samples snapshots of resident columns only and
//!   never generates one.
//! - Item frames: [`FramedMap`] is what an item frame holding a filled map tells the map
//!   each tenth tick ([`MapSession::tick`] takes the list); [`MapData::tick_in_frame`] keeps a
//!   [`MapFrame`] marker and a `frame` decoration, and [`MapData::removed_from_frame`] drops them
//!   when the frame loses the map. A framed map is sent to every player in the dimension.
//! - Not modelled: scale/lock post-processing (cartography table), exploration-map target
//!   decorations from the `map_decorations` item component, and hiding a marker when its
//!   wearer has an invisibility-equipment item on.
//!
//! # Configuration
//!
//! None. [`MapHandle::attach_directory`] enables persistence; without it maps live in memory.
//!
//! # Dependencies
//!
//! `lodestone-data` (colours), `lodestone-anvil` (gzip NBT container), `lodestone-model` (the
//! packet's decoration and patch types).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lodestone_core::{Nbt, NbtTag};
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_data::map_colors;
use lodestone_model::{BlockPos, Identifier, ItemStack, MapDecoration, MapPatch, Text};
use uuid::Uuid;

use crate::chunk::{ChunkColumn, ChunkSource};
use crate::dimension::Dimension;
use crate::inventory::{OFFHAND_NATIVE, PlayerInventory};

/// Width and height of a map's colour grid.
pub const MAP_SIZE: usize = 128;
const PIXELS: usize = MAP_SIZE * MAP_SIZE;
/// Highest zoom level; a map covers `128 << scale` blocks across.
pub const MAX_SCALE: u8 = 4;
/// Banner markers a map keeps before refusing another.
pub const TRACKED_DECORATION_LIMIT: i32 = 256;

const SHADE_LOW: u8 = 0;
const SHADE_NORMAL: u8 = 1;
const SHADE_HIGH: u8 = 2;
const COLOR_STONE: u8 = 11;
const COLOR_WATER: u8 = 12;
/// Distance, in map pixels, at which an off-map player marker stops being shown.
const OFF_MAP_LIMIT: f32 = 320.0;
/// How far inside the edge a marker may sit and still be drawn on the map.
const INSIDE_LIMIT: f32 = 63.0;
const DECORATION_RESEND_TICKS: u32 = 5;
/// Ticks between an item frame showing its map to each player.
const FRAME_INTERVAL_TICKS: u32 = 10;
/// Ticks between flushes of changed maps to disk.
const FLUSH_INTERVAL_TICKS: u32 = 200;
/// Ticks a column snapshot is reused before it is taken again.
const SNAPSHOT_TTL_TICKS: u64 = 20;
const SNAPSHOT_CAP: usize = 512;

/// A banner a map was told to mark.
#[derive(Clone, Debug, PartialEq)]
pub struct MapBanner {
    pub pos: BlockPos,
    /// Dye colour name, e.g. `light_blue`.
    pub color: String,
    pub name: Option<Text>,
}

impl MapBanner {
    /// The key its decoration is stored under.
    #[must_use]
    pub fn key(&self) -> String {
        format!("banner-{},{},{}", self.pos.x, self.pos.y, self.pos.z)
    }

    fn decoration_kind(&self) -> String {
        format!("banner_{}", self.color)
    }
}

/// An item frame holding the map.
#[derive(Clone, Debug, PartialEq)]
pub struct MapFrame {
    pub pos: BlockPos,
    pub rotation: i32,
    pub entity_id: i32,
}

impl MapFrame {
    fn decoration_key(&self) -> String {
        format!("frame-{}", self.entity_id)
    }
}

/// A filled map in an item frame, as the frame reports it to [`MapSession::tick`].
#[derive(Clone, Debug, PartialEq)]
pub struct FramedMap {
    pub map_id: i32,
    /// The frame's network entity id, which keys its decoration.
    pub entity_id: i32,
    /// The cell the frame occupies.
    pub pos: BlockPos,
    /// The marker rotation in degrees: the facing's quarter turns, or -90 for a frame on a floor
    /// or ceiling.
    pub rotation: i32,
}

/// One decoration as the map stores it: the type's registry path (`player`, `banner_red`, ...),
/// the half-pixel offset from the map centre, a 0-15 rotation and an optional label.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoration {
    pub kind: String,
    pub x: i8,
    pub y: i8,
    pub rotation: u8,
    pub name: Option<Text>,
}

impl Decoration {
    /// Player, frame, banner and off-map markers count toward [`TRACKED_DECORATION_LIMIT`].
    fn tracked(&self) -> bool {
        self.kind.starts_with("banner_")
            || matches!(
                self.kind.as_str(),
                "player" | "frame" | "red_marker" | "blue_marker" | "player_off_map" | "player_off_limits"
            )
    }

    fn to_wire(&self) -> Option<MapDecoration> {
        Some(MapDecoration {
            kind: Identifier::new("minecraft", self.kind.clone()).ok()?,
            x: self.x,
            y: self.y,
            rotation: self.rotation,
            name: self.name.clone(),
        })
    }
}

/// What one client is owed for one map: either half may be absent.
#[derive(Clone, Debug, PartialEq)]
pub struct MapUpdate {
    pub map_id: i32,
    pub scale: i8,
    pub locked: bool,
    pub decorations: Option<Vec<MapDecoration>>,
    pub patch: Option<MapPatch>,
}

/// One carrier's record on a map: the rectangle of pixels and the decorations its client lacks.
#[derive(Clone, Debug)]
struct Holder {
    uuid: Uuid,
    name: String,
    dirty_data: bool,
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
    dirty_decorations: bool,
    tick: u32,
    step: i32,
}

impl Holder {
    fn new(uuid: Uuid, name: &str) -> Self {
        Self {
            uuid,
            name: name.to_owned(),
            dirty_data: true,
            min_x: 0,
            min_y: 0,
            max_x: MAP_SIZE - 1,
            max_y: MAP_SIZE - 1,
            dirty_decorations: true,
            tick: 0,
            step: 0,
        }
    }

    fn mark_color(&mut self, x: usize, y: usize) {
        if self.dirty_data {
            self.min_x = self.min_x.min(x);
            self.min_y = self.min_y.min(y);
            self.max_x = self.max_x.max(x);
            self.max_y = self.max_y.max(y);
        } else {
            self.dirty_data = true;
            (self.min_x, self.min_y, self.max_x, self.max_y) = (x, y, x, y);
        }
    }
}

/// Who is ticking a map and where they are.
#[derive(Clone, Copy, Debug)]
pub struct Carrier<'a> {
    pub uuid: Uuid,
    pub name: &'a str,
    pub x: f64,
    pub z: f64,
    pub yaw: f32,
    pub dimension: Dimension,
    pub game_time: i64,
}

/// Read access to the blocks a map is sampled from.
pub trait MapTerrain {
    /// Whether the column holding block `(x, z)` is loaded; an unloaded column is skipped.
    fn is_loaded(&self, x: i32, z: i32) -> bool;
    /// Lowest block y of the world.
    fn min_y(&self) -> i32;
    /// One above the highest non-air block of the column, `min_y` for an all-air column.
    fn surface_top(&self, x: i32, z: i32) -> i32;
    fn state(&self, x: i32, y: i32, z: i32) -> StateId;
}

/// One saved map.
#[derive(Clone, Debug)]
pub struct MapData {
    /// Dimension key, e.g. `minecraft:overworld`.
    pub dimension: String,
    pub center_x: i32,
    pub center_z: i32,
    pub scale: u8,
    pub tracking_position: bool,
    pub unlimited_tracking: bool,
    pub locked: bool,
    pub colors: Vec<u8>,
    pub banners: Vec<MapBanner>,
    pub frames: Vec<MapFrame>,
    decorations: Vec<(String, Decoration)>,
    tracked_decorations: i32,
    holders: Vec<Holder>,
    dirty: bool,
}

impl MapData {
    fn bare(dimension: &str, center_x: i32, center_z: i32, scale: u8, tracking: bool, unlimited: bool, locked: bool) -> Self {
        Self {
            dimension: dimension.to_owned(),
            center_x,
            center_z,
            scale: scale.min(MAX_SCALE),
            tracking_position: tracking,
            unlimited_tracking: unlimited,
            locked,
            colors: vec![0; PIXELS],
            banners: Vec::new(),
            frames: Vec::new(),
            decorations: Vec::new(),
            tracked_decorations: 0,
            holders: Vec::new(),
            dirty: true,
        }
    }

    /// A blank map whose centre snaps to the grid of maps of this scale around `(x, z)`.
    #[must_use]
    pub fn fresh(x: f64, z: f64, scale: u8, tracking: bool, unlimited: bool, dimension: &str) -> Self {
        let size = (MAP_SIZE as i32) << scale.min(MAX_SCALE);
        let area_x = ((x + 64.0) / f64::from(size)).floor() as i32;
        let area_z = ((z + 64.0) / f64::from(size)).floor() as i32;
        Self::bare(
            dimension,
            area_x * size + size / 2 - 64,
            area_z * size + size / 2 - 64,
            scale,
            tracking,
            unlimited,
            false,
        )
    }

    /// Whether the map changed since it was last written out.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The pixel at `(x, y)`.
    #[must_use]
    pub fn color_at(&self, x: usize, y: usize) -> u8 {
        self.colors[x + y * MAP_SIZE]
    }

    /// The decorations currently shown, in insertion order.
    pub fn decorations(&self) -> impl Iterator<Item = (&str, &Decoration)> {
        self.decorations.iter().map(|(key, decoration)| (key.as_str(), decoration))
    }

    fn holder_index(&mut self, who: &Carrier<'_>) -> usize {
        if let Some(index) = self.holders.iter().position(|holder| holder.uuid == who.uuid) {
            return index;
        }
        self.holders.push(Holder::new(who.uuid, who.name));
        self.holders.len() - 1
    }

    /// Sets one pixel and dirties it for every holder.
    pub fn set_color(&mut self, x: usize, y: usize, color: u8) {
        self.colors[x + y * MAP_SIZE] = color;
        self.dirty = true;
        for holder in &mut self.holders {
            holder.mark_color(x, y);
        }
    }

    fn update_color(&mut self, x: usize, y: usize, color: u8) -> bool {
        if self.colors[x + y * MAP_SIZE] == color {
            return false;
        }
        self.set_color(x, y, color);
        true
    }

    fn mark_decorations_dirty(&mut self) {
        for holder in &mut self.holders {
            holder.dirty_decorations = true;
        }
    }

    fn remove_decoration(&mut self, key: &str) {
        if let Some(index) = self.decorations.iter().position(|(have, _)| have == key) {
            let (_, removed) = self.decorations.remove(index);
            if removed.tracked() {
                self.tracked_decorations -= 1;
            }
        }
        self.mark_decorations_dirty();
    }

    /// Whether more than `limit` markers are tracked.
    #[must_use]
    pub fn tracked_over(&self, limit: i32) -> bool {
        self.tracked_decorations > limit
    }

    /// Notes that `who` carries this map: creates their holder and refreshes their marker.
    pub fn tick_carried_by(&mut self, who: &Carrier<'_>) {
        self.holder_index(who);
        if who.dimension.key() == self.dimension && self.tracking_position {
            self.add_decoration("player", who.name, who.x, who.z, f64::from(who.yaw), None, Some(who.game_time));
        }
    }

    /// Notes that `who` is shown the map by the item frame `frame`: creates their holder and keeps
    /// the frame's marker. No holder marker is added, and one left from when `who` held the map
    /// is dropped unless `holds` says they still do.
    pub fn tick_in_frame(&mut self, who: &Carrier<'_>, frame: &FramedMap, holds: bool) {
        self.holder_index(who);
        if !holds && self.decorations.iter().any(|(key, _)| key == who.name) {
            self.remove_decoration(who.name);
        }
        if !self.tracking_position {
            return;
        }
        if let Some(existing) = self.frames.iter().find(|have| have.pos == frame.pos)
            && existing.entity_id != frame.entity_id
        {
            let key = existing.decoration_key();
            self.remove_decoration(&key);
        }
        let marker = MapFrame { pos: frame.pos, rotation: frame.rotation, entity_id: frame.entity_id };
        self.add_decoration(
            "frame",
            &marker.decoration_key(),
            f64::from(frame.pos.x),
            f64::from(frame.pos.z),
            f64::from(frame.rotation),
            None,
            Some(who.game_time),
        );
        match self.frames.iter().position(|have| have.pos == frame.pos) {
            Some(index) if self.frames[index] == marker => {}
            Some(index) => {
                self.frames[index] = marker;
                self.dirty = true;
            }
            None => {
                self.frames.push(marker);
                self.dirty = true;
            }
        }
    }

    /// Forgets the frame at `pos`, whose entity was `entity_id`, once it no longer holds this map.
    pub fn removed_from_frame(&mut self, pos: BlockPos, entity_id: i32) {
        self.remove_decoration(&format!("frame-{entity_id}"));
        self.frames.retain(|have| have.pos != pos);
        self.dirty = true;
    }

    /// Drops `uuid`'s holder and the marker kept under `name`.
    pub fn release(&mut self, uuid: Uuid) {
        if let Some(index) = self.holders.iter().position(|holder| holder.uuid == uuid) {
            let holder = self.holders.remove(index);
            self.remove_decoration(&holder.name);
        }
    }

    /// Adds or refreshes the decoration `key` at world position `(x, z)`, or removes it when the
    /// position is out of range for its type.
    pub fn add_decoration(
        &mut self,
        kind: &str,
        key: &str,
        x: f64,
        z: f64,
        yaw: f64,
        name: Option<Text>,
        nether_time: Option<i64>,
    ) {
        let scaling = (1i32 << self.scale) as f32;
        let dx = (x - f64::from(self.center_x)) as f32 / scaling;
        let dy = (z - f64::from(self.center_z)) as f32 / scaling;
        let inside = (-INSIDE_LIMIT..=INSIDE_LIMIT).contains(&dx) && (-INSIDE_LIMIT..=INSIDE_LIMIT).contains(&dy);
        let kind = if kind == "player" {
            if inside {
                kind
            } else if dx.abs() < OFF_MAP_LIMIT && dy.abs() < OFF_MAP_LIMIT {
                "player_off_map"
            } else if self.unlimited_tracking {
                "player_off_limits"
            } else {
                self.remove_decoration(key);
                return;
            }
        } else if inside || self.unlimited_tracking {
            kind
        } else {
            self.remove_decoration(key);
            return;
        };
        let rotation = self.rotation_for(yaw, nether_time);
        let decoration = Decoration {
            kind: kind.to_owned(),
            x: clamp_coordinate(dx),
            y: clamp_coordinate(dy),
            rotation,
            name,
        };
        match self.decorations.iter().position(|(have, _)| have == key) {
            Some(index) if self.decorations[index].1 == decoration => {}
            Some(index) => {
                let previous = std::mem::replace(&mut self.decorations[index].1, decoration);
                if previous.tracked() {
                    self.tracked_decorations -= 1;
                }
                if self.decorations[index].1.tracked() {
                    self.tracked_decorations += 1;
                }
                self.mark_decorations_dirty();
            }
            None => {
                if decoration.tracked() {
                    self.tracked_decorations += 1;
                }
                self.decorations.push((key.to_owned(), decoration));
                self.mark_decorations_dirty();
            }
        }
    }

    /// The marker rotation: sixteenths of a turn from the yaw, or a time-driven spin in the Nether.
    fn rotation_for(&self, yaw: f64, game_time: Option<i64>) -> u8 {
        if self.dimension == Dimension::Nether.key()
            && let Some(time) = game_time
        {
            let s = (time / 10) as i32;
            return ((s.wrapping_mul(s).wrapping_mul(34_187_121).wrapping_add(s.wrapping_mul(121))) >> 15 & 15) as u8;
        }
        let adjusted = if yaw < 0.0 { yaw - 8.0 } else { yaw + 8.0 };
        (((adjusted * 16.0 / 360.0) as i32) as i8 as u8) & 15
    }

    /// Marks or unmarks the banner at `pos`. Returns whether the click was accepted: a position
    /// off the map or a block that is not a banner is refused.
    pub fn toggle_banner(&mut self, banner: Option<MapBanner>, pos: BlockPos) -> bool {
        let scaling = f64::from(1i32 << self.scale);
        let (x, z) = (f64::from(pos.x) + 0.5, f64::from(pos.z) + 0.5);
        let dx = (x - f64::from(self.center_x)) / scaling;
        let dy = (z - f64::from(self.center_z)) / scaling;
        if !(-63.0..=63.0).contains(&dx) || !(-63.0..=63.0).contains(&dy) {
            return false;
        }
        let Some(banner) = banner else { return false };
        if let Some(index) = self.banners.iter().position(|have| *have == banner) {
            self.banners.remove(index);
            self.remove_decoration(&banner.key());
            self.dirty = true;
            return true;
        }
        if self.tracked_over(TRACKED_DECORATION_LIMIT) {
            return false;
        }
        self.add_decoration(&banner.decoration_kind(), &banner.key(), x, z, 180.0, banner.name.clone(), None);
        self.banners.push(banner);
        self.dirty = true;
        true
    }

    /// Drops the banner markers at column `(x, z)` whose banner is gone or recoloured.
    fn check_banners(&mut self, x: i32, z: i32, terrain: &dyn MapTerrain) {
        let stale: Vec<MapBanner> = self
            .banners
            .iter()
            .filter(|banner| {
                banner.pos.x == x
                    && banner.pos.z == z
                    && banner_color(terrain.state(banner.pos.x, banner.pos.y, banner.pos.z)).as_deref()
                        != Some(banner.color.as_str())
            })
            .cloned()
            .collect();
        for banner in stale {
            self.banners.retain(|have| *have != banner);
            self.remove_decoration(&banner.key());
            self.dirty = true;
        }
    }

    /// Samples the terrain around the carrier into this map: one in sixteen columns per call,
    /// continuing down the column while pixels keep changing.
    pub fn sample_terrain(&mut self, who: &Carrier<'_>, terrain: &dyn MapTerrain, has_ceiling: bool) {
        if who.dimension.key() != self.dimension {
            return;
        }
        let holder = self.holder_index(who);
        self.holders[holder].step += 1;
        let step = self.holders[holder].step;
        let scale = 1i32 << self.scale;
        let (center_x, center_z) = (self.center_x, self.center_z);
        let player_x = (who.x - f64::from(center_x)).floor() as i32 / scale + 64;
        let player_y = (who.z - f64::from(center_z)).floor() as i32 / scale + 64;
        let mut radius = 128 / scale;
        if has_ceiling {
            radius /= 2;
        }
        let min_y = terrain.min_y();
        let mut consecutive = false;
        for img_x in player_x - radius + 1..player_x + radius {
            if (img_x & 15) != (step & 15) && !consecutive {
                continue;
            }
            consecutive = false;
            let mut previous_height = 0.0f64;
            for img_y in player_y - radius - 1..player_y + radius {
                if !(0..128).contains(&img_x) || !(-1..128).contains(&img_y) {
                    continue;
                }
                let distance_sq = (img_x - player_x).pow(2) + (img_y - player_y).pow(2);
                let dither = distance_sq > (radius - 2).pow(2);
                let area_x = (center_x / scale + img_x - 64) * scale;
                let area_z = (center_z / scale + img_y - 64) * scale;
                if !terrain.is_loaded(area_x, area_z) {
                    continue;
                }
                let mut counts: Vec<(u8, i32)> = Vec::new();
                let mut count = |color: u8, times: i32| match counts.iter_mut().find(|(have, _)| *have == color) {
                    Some(entry) => entry.1 += times,
                    None => counts.push((color, times)),
                };
                let mut water_depth = 0i32;
                let mut height = 0.0f64;
                if has_ceiling {
                    let mut noise = area_x.wrapping_add(area_z.wrapping_mul(231_871));
                    noise = noise.wrapping_mul(noise).wrapping_mul(31_287_121).wrapping_add(noise.wrapping_mul(11));
                    if (noise >> 20) & 1 == 0 {
                        count(map_colors::map_color(Block::Dirt.default_state()), 10);
                    } else {
                        count(map_colors::map_color(Block::Stone.default_state()), 100);
                    }
                    height = 100.0;
                } else {
                    for dx in 0..scale {
                        for dz in 0..scale {
                            let (x, z) = (area_x + dx, area_z + dz);
                            let mut column_y = terrain.surface_top(x, z);
                            let color;
                            if column_y <= min_y {
                                color = COLOR_STONE;
                            } else {
                                let mut state;
                                loop {
                                    column_y -= 1;
                                    state = terrain.state(x, column_y, z);
                                    if !(map_colors::map_color(state) == 0 && column_y > min_y) {
                                        break;
                                    }
                                }
                                if column_y > min_y && map_colors::holds_fluid(state) {
                                    let mut solid_y = column_y - 1;
                                    loop {
                                        let below = terrain.state(x, solid_y, z);
                                        solid_y -= 1;
                                        water_depth += 1;
                                        if !(solid_y > min_y && map_colors::holds_fluid(below)) {
                                            break;
                                        }
                                    }
                                    color = map_colors::surface_map_color(state);
                                } else {
                                    color = map_colors::map_color(state);
                                }
                            }
                            self.check_banners(x, z, terrain);
                            height += f64::from(column_y) / f64::from(scale * scale);
                            count(color, 1);
                        }
                    }
                }
                water_depth /= scale * scale;
                let color = most_common(&counts);
                let parity = f64::from((img_x + img_y) & 1);
                let shade = if color == COLOR_WATER {
                    let diff = f64::from(water_depth) * 0.1 + parity * 0.2;
                    if diff < 0.5 {
                        SHADE_HIGH
                    } else if diff > 0.9 {
                        SHADE_LOW
                    } else {
                        SHADE_NORMAL
                    }
                } else {
                    let diff = (height - previous_height) * 4.0 / f64::from(scale + 4) + (parity - 0.5) * 0.4;
                    if diff > 0.6 {
                        SHADE_HIGH
                    } else if diff < -0.6 {
                        SHADE_LOW
                    } else {
                        SHADE_NORMAL
                    }
                };
                previous_height = height;
                if img_y >= 0 && distance_sq < radius * radius && (!dither || (img_x + img_y) & 1 != 0) {
                    consecutive |= self.update_color(img_x as usize, img_y as usize, (color << 2) | shade);
                }
            }
        }
    }

    /// What `uuid`'s client is owed now, clearing what is returned. `None` when nothing is.
    pub fn next_update(&mut self, uuid: Uuid, map_id: i32) -> Option<MapUpdate> {
        let index = self.holders.iter().position(|holder| holder.uuid == uuid)?;
        let (scale, locked) = (self.scale as i8, self.locked);
        let holder = &mut self.holders[index];
        let patch = if holder.dirty_data {
            holder.dirty_data = false;
            let (start_x, start_y) = (holder.min_x, holder.min_y);
            let (width, height) = (holder.max_x + 1 - holder.min_x, holder.max_y + 1 - holder.min_y);
            let mut colors = vec![0u8; width * height];
            for x in 0..width {
                for y in 0..height {
                    colors[x + y * width] = self.colors[start_x + x + (start_y + y) * MAP_SIZE];
                }
            }
            Some(MapPatch {
                start_x: start_x as u8,
                start_y: start_y as u8,
                width: width as u8,
                height: height as u8,
                colors,
            })
        } else {
            None
        };
        let holder = &mut self.holders[index];
        let decorations = if holder.dirty_decorations && {
            let due = holder.tick % DECORATION_RESEND_TICKS == 0;
            holder.tick += 1;
            due
        } {
            holder.dirty_decorations = false;
            Some(self.decorations.iter().filter_map(|(_, decoration)| decoration.to_wire()).collect())
        } else {
            None
        };
        if patch.is_none() && decorations.is_none() {
            return None;
        }
        Some(MapUpdate { map_id, scale, locked, decorations, patch })
    }

    // ----- persistence ---------------------------------------------------------------------

    /// The `data` compound the reference stores for a map.
    #[must_use]
    pub fn to_nbt(&self) -> Nbt {
        let flag = |value: bool| Nbt::Byte(i8::from(value));
        let banners: Vec<Nbt> = self
            .banners
            .iter()
            .map(|banner| {
                let mut fields = vec![
                    ("pos".to_owned(), Nbt::IntArray(vec![banner.pos.x, banner.pos.y, banner.pos.z])),
                    ("color".to_owned(), Nbt::String(banner.color.clone())),
                ];
                if let Some(name) = &banner.name {
                    fields.push(("name".to_owned(), name.to_nbt()));
                }
                Nbt::Compound(fields)
            })
            .collect();
        let frames: Vec<Nbt> = self
            .frames
            .iter()
            .map(|frame| {
                Nbt::Compound(vec![
                    ("pos".to_owned(), Nbt::IntArray(vec![frame.pos.x, frame.pos.y, frame.pos.z])),
                    ("rotation".to_owned(), Nbt::Int(frame.rotation)),
                    ("entity_id".to_owned(), Nbt::Int(frame.entity_id)),
                ])
            })
            .collect();
        let list = |elements: Vec<Nbt>| Nbt::List {
            element_type: if elements.is_empty() { NbtTag::End } else { NbtTag::Compound },
            elements,
        };
        Nbt::Compound(vec![
            ("dimension".to_owned(), Nbt::String(self.dimension.clone())),
            ("xCenter".to_owned(), Nbt::Int(self.center_x)),
            ("zCenter".to_owned(), Nbt::Int(self.center_z)),
            ("scale".to_owned(), Nbt::Byte(self.scale as i8)),
            ("colors".to_owned(), Nbt::ByteArray(self.colors.iter().map(|&c| c as i8).collect())),
            ("trackingPosition".to_owned(), flag(self.tracking_position)),
            ("unlimitedTracking".to_owned(), flag(self.unlimited_tracking)),
            ("locked".to_owned(), flag(self.locked)),
            ("banners".to_owned(), list(banners)),
            ("frames".to_owned(), list(frames)),
        ])
    }

    /// Reads a `data` compound, applying the reference's defaults for missing optional fields.
    ///
    /// # Errors
    ///
    /// A message naming the missing or malformed required field.
    pub fn from_nbt(data: &Nbt) -> Result<Self, String> {
        let get = |key: &str| compound_get(data, key);
        let int = |key: &str| match get(key) {
            Some(Nbt::Int(v)) => Ok(*v),
            _ => Err(format!("map field {key} is missing or not an int")),
        };
        let flag = |key: &str, default: bool| match get(key) {
            Some(Nbt::Byte(v)) => *v != 0,
            _ => default,
        };
        let Some(Nbt::String(dimension)) = get("dimension") else {
            return Err("map field dimension is missing".to_owned());
        };
        let scale = match get("scale") {
            Some(Nbt::Byte(v)) => (*v).clamp(0, MAX_SCALE as i8) as u8,
            _ => 0,
        };
        let mut map = Self::bare(
            dimension,
            int("xCenter")?,
            int("zCenter")?,
            scale,
            flag("trackingPosition", true),
            flag("unlimitedTracking", false),
            flag("locked", false),
        );
        match get("colors") {
            Some(Nbt::ByteArray(bytes)) if bytes.len() == PIXELS => {
                map.colors = bytes.iter().map(|&b| b as u8).collect();
            }
            Some(Nbt::ByteArray(_)) => {}
            _ => return Err("map field colors is missing".to_owned()),
        }
        let position = |entry: &Nbt| match compound_get(entry, "pos") {
            Some(Nbt::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
            _ => None,
        };
        if let Some(Nbt::List { elements, .. }) = get("banners") {
            for entry in elements {
                let Some(pos) = position(entry) else { continue };
                let color = match compound_get(entry, "color") {
                    Some(Nbt::String(name)) => name.clone(),
                    _ => "white".to_owned(),
                };
                let name = compound_get(entry, "name").map(Text::from_nbt);
                let banner = MapBanner { pos, color, name };
                map.add_decoration(&banner.decoration_kind(), &banner.key(), f64::from(pos.x), f64::from(pos.z), 180.0, banner.name.clone(), None);
                map.banners.push(banner);
            }
        }
        if let Some(Nbt::List { elements, .. }) = get("frames") {
            for entry in elements {
                let (Some(pos), Some(Nbt::Int(rotation)), Some(Nbt::Int(entity_id))) =
                    (position(entry), compound_get(entry, "rotation"), compound_get(entry, "entity_id"))
                else {
                    continue;
                };
                let frame = MapFrame { pos, rotation: *rotation, entity_id: *entity_id };
                map.add_decoration("frame", &frame.decoration_key(), f64::from(pos.x), f64::from(pos.z), f64::from(*rotation), None, None);
                map.frames.push(frame);
            }
        }
        map.dirty = false;
        Ok(map)
    }
}

fn most_common(counts: &[(u8, i32)]) -> u8 {
    let mut best: Option<(u8, i32)> = None;
    for &(color, times) in counts {
        if best.is_none_or(|(_, most)| times > most) {
            best = Some((color, times));
        }
    }
    best.map_or(0, |(color, _)| color)
}

/// A decoration offset on the wire: half pixels from the centre, saturating at the edges.
fn clamp_coordinate(delta: f32) -> i8 {
    if delta <= -INSIDE_LIMIT {
        i8::MIN
    } else if delta >= INSIDE_LIMIT {
        i8::MAX
    } else {
        (delta * 2.0 + 0.5) as i32 as i8
    }
}

/// The dye colour name of a banner block, `None` for anything else.
fn banner_color(state: StateId) -> Option<String> {
    let name = state.name().strip_prefix("minecraft:")?;
    let color = name.strip_suffix("_wall_banner").or_else(|| name.strip_suffix("_banner"))?;
    Some(color.to_owned())
}

/// The banner at `state`, as a marker for `pos`.
#[must_use]
pub fn banner_at(state: StateId, pos: BlockPos) -> Option<MapBanner> {
    Some(MapBanner { pos, color: banner_color(state)?, name: None })
}

fn compound_get<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, value)| value),
        _ => None,
    }
}

// ----- store -----------------------------------------------------------------------------

/// Every map of one world, plus the id counter.
#[derive(Debug, Default)]
pub struct MapStore {
    directory: Option<PathBuf>,
    last_id: Option<i32>,
    maps: BTreeMap<i32, MapData>,
    index_dirty: bool,
}

impl MapStore {
    fn file(&self, name: &str) -> Option<PathBuf> {
        Some(self.directory.as_ref()?.join(format!("{name}.dat")))
    }

    /// Roots the store at `directory` (the folder holding `last_id.dat` and `<id>.dat`) and reads the
    /// counter. Maps created before this keep their ids: the counter never moves backwards.
    pub fn attach_directory(&mut self, directory: PathBuf) {
        if self.directory.as_deref() == Some(directory.as_path()) {
            return;
        }
        self.directory = Some(directory);
        let stored = self
            .file("last_id")
            .and_then(|path| read_saved(&path))
            .and_then(|data| match compound_get(&data, "map") {
                Some(Nbt::Int(v)) => Some(*v),
                _ => None,
            });
        if stored > self.last_id {
            self.last_id = stored;
        } else if self.last_id.is_some() {
            self.index_dirty = true;
        }
        for map in self.maps.values_mut() {
            map.dirty = true;
        }
    }

    /// Stores a new map under the next free id.
    pub fn create(&mut self, data: MapData) -> i32 {
        let id = self.last_id.map_or(0, |last| last + 1);
        self.last_id = Some(id);
        self.index_dirty = true;
        self.maps.insert(id, data);
        id
    }

    /// The map `id`, read from disk the first time it is asked for.
    pub fn get(&mut self, id: i32) -> Option<&mut MapData> {
        if !self.maps.contains_key(&id) {
            let data = self.file(&format!("{id}")).and_then(|path| read_saved(&path))?;
            self.maps.insert(id, MapData::from_nbt(&data).ok()?);
        }
        self.maps.get_mut(&id)
    }

    /// Writes every changed map and the counter. Returns how many files were written.
    pub fn flush(&mut self) -> usize {
        let Some(directory) = self.directory.clone() else { return 0 };
        let mut written = 0;
        if self.index_dirty
            && let Some(last) = self.last_id
        {
            let data = Nbt::Compound(vec![("map".to_owned(), Nbt::Int(last))]);
            if write_saved(&directory.join("last_id.dat"), data) {
                self.index_dirty = false;
                written += 1;
            }
        }
        for (id, map) in &mut self.maps {
            if map.dirty && write_saved(&directory.join(format!("{id}.dat")), map.to_nbt()) {
                map.dirty = false;
                written += 1;
            }
        }
        written
    }

    fn release(&mut self, uuid: Uuid, ids: &BTreeSet<i32>) {
        for id in ids {
            if let Some(map) = self.maps.get_mut(id) {
                map.release(uuid);
            }
        }
    }
}

/// Reads a saved map file's `data` compound. The browser build has no filesystem, so there it
/// finds nothing.
#[cfg(not(target_arch = "wasm32"))]
fn read_saved(path: &Path) -> Option<Nbt> {
    let root = lodestone_anvil::player_dat::read_from_file(path).ok()??;
    compound_get(&root, "data").cloned()
}

#[cfg(target_arch = "wasm32")]
fn read_saved(_path: &Path) -> Option<Nbt> {
    None
}

/// Writes `data` as a saved map file; whether it was written.
#[cfg(not(target_arch = "wasm32"))]
fn write_saved(path: &Path, data: Nbt) -> bool {
    let root = Nbt::Compound(vec![
        ("data".to_owned(), data),
        ("DataVersion".to_owned(), Nbt::Int(lodestone_anvil::level_dat::DATA_VERSION_26_2)),
    ]);
    lodestone_anvil::player_dat::write_to_file(&root, path).is_ok()
}

#[cfg(target_arch = "wasm32")]
fn write_saved(_path: &Path, _data: Nbt) -> bool {
    false
}

/// The shared handle to a world's [`MapStore`].
#[derive(Clone, Debug, Default)]
pub struct MapHandle(Arc<Mutex<MapStore>>);

impl MapHandle {
    /// Runs `f` with the store locked.
    pub fn with<R>(&self, f: impl FnOnce(&mut MapStore) -> R) -> R {
        f(&mut self.0.lock().expect("map store lock poisoned"))
    }

    /// Enables persistence under `directory`; see [`MapStore::attach_directory`].
    pub fn attach_directory(&self, directory: PathBuf) {
        self.with(|store| store.attach_directory(directory));
    }

    /// Creates the map an empty map becomes when used at block `(x, z)`: scale 0, tracking the
    /// holder, centred by the snapping rule. Returns its id.
    #[must_use]
    pub fn create_for_use(&self, x: i32, z: i32, dimension: Dimension) -> i32 {
        self.with(|store| {
            store.create(MapData::fresh(f64::from(x), f64::from(z), 0, true, false, dimension.key()))
        })
    }

    /// Writes changed maps to disk.
    pub fn removed_from_frame(&self, map_id: i32, pos: BlockPos, entity_id: i32) {
        self.with(|store| {
            if let Some(map) = store.get(map_id) {
                map.removed_from_frame(pos, entity_id);
            }
        });
    }

    pub fn flush(&self) -> usize {
        self.with(MapStore::flush)
    }
}

// ----- per-connection ----------------------------------------------------------------------

/// Block reads for one connection's sampling, over resident-column snapshots.
#[derive(Debug, Default)]
pub struct ColumnCache {
    columns: RefCell<HashMap<(i32, i32), (u64, Option<Arc<ChunkColumn>>)>>,
    min_y: Cell<Option<i32>>,
}

/// [`MapTerrain`] over a [`ChunkSource`]'s resident columns.
pub struct SourceTerrain<'a, S: ChunkSource + ?Sized> {
    source: &'a S,
    cache: &'a ColumnCache,
    now: u64,
}

impl ColumnCache {
    /// A terrain view that refreshes snapshots older than [`SNAPSHOT_TTL_TICKS`] at `now`.
    #[must_use]
    pub fn terrain<'a, S: ChunkSource + ?Sized>(&'a self, source: &'a S, now: u64) -> SourceTerrain<'a, S> {
        let mut columns = self.columns.borrow_mut();
        if columns.len() > SNAPSHOT_CAP {
            columns.clear();
        }
        SourceTerrain { source, cache: self, now }
    }
}

impl<S: ChunkSource + ?Sized> std::fmt::Debug for SourceTerrain<'_, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceTerrain").field("now", &self.now).finish_non_exhaustive()
    }
}

impl<S: ChunkSource + ?Sized> SourceTerrain<'_, S> {
    fn column(&self, x: i32, z: i32) -> Option<Arc<ChunkColumn>> {
        let key = (x.div_euclid(16), z.div_euclid(16));
        let mut columns = self.cache.columns.borrow_mut();
        if let Some((taken, column)) = columns.get(&key)
            && self.now.saturating_sub(*taken) < SNAPSHOT_TTL_TICKS
        {
            return column.clone();
        }
        let column = self.source.resident_column(key.0, key.1).map(Arc::new);
        if let Some(column) = &column {
            self.cache.min_y.set(Some(column.min_y));
        }
        columns.insert(key, (self.now, column.clone()));
        column
    }
}

impl<S: ChunkSource + ?Sized> MapTerrain for SourceTerrain<'_, S> {
    fn is_loaded(&self, x: i32, z: i32) -> bool {
        self.column(x, z).is_some()
    }

    fn min_y(&self) -> i32 {
        self.cache.min_y.get().unwrap_or(-64)
    }

    fn surface_top(&self, x: i32, z: i32) -> i32 {
        let Some(column) = self.column(x, z) else { return self.min_y() };
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        let mut y = column.air_above_y().min(column.min_y + column.height) - 1;
        while y >= column.min_y {
            if !matches!(column.block_state_id(lx, y, lz).block(), Block::Air | Block::CaveAir | Block::VoidAir) {
                return y + 1;
            }
            y -= 1;
        }
        column.min_y
    }

    fn state(&self, x: i32, y: i32, z: i32) -> StateId {
        self.column(x, z)
            .map_or(StateId::AIR, |column| column.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16)))
    }
}

/// One connection's map bookkeeping: which maps it carries, its terrain snapshots, and when it last
/// flushed to disk. Dropping it releases the player's markers.
#[derive(Debug)]
pub struct MapSession {
    handle: MapHandle,
    uuid: Uuid,
    name: String,
    carried: BTreeSet<i32>,
    framed: BTreeSet<i32>,
    cache: ColumnCache,
    ticks: u32,
}

/// What a connection tells [`MapSession::tick`] about its player.
#[derive(Clone, Copy, Debug)]
pub struct PlayerPose {
    pub x: f64,
    pub z: f64,
    pub yaw: f32,
    pub dimension: Dimension,
    pub game_time: i64,
}

impl MapSession {
    #[must_use]
    pub fn new(handle: MapHandle, uuid: Uuid, name: &str) -> Self {
        Self { handle, uuid, name: name.to_owned(), carried: BTreeSet::new(), framed: BTreeSet::new(), cache: ColumnCache::default(), ticks: 0 }
    }

    /// One server tick: tick every carried map, sample the ones in hand, and return the updates owed.
    /// Every [`FRAME_INTERVAL_TICKS`]th tick the maps in `frames` are shown to this player as well.
    pub fn tick<S: ChunkSource + ?Sized>(
        &mut self,
        pose: PlayerPose,
        inventory: &PlayerInventory,
        source: &S,
        frames: &[FramedMap],
    ) -> Vec<MapUpdate> {
        self.ticks = self.ticks.wrapping_add(1);
        let hands = [usize::from(inventory.selected_hotbar_slot()), OFFHAND_NATIVE];
        let mut carried: BTreeMap<i32, bool> = BTreeMap::new();
        for native in 0..crate::inventory::PLAYER_NATIVE_SIZE {
            let Some(stack) = inventory.native(native) else { continue };
            if let Some(id) = filled_map_id(stack) {
                *carried.entry(id).or_insert(false) |= hands.contains(&native);
            }
        }
        let who = Carrier {
            uuid: self.uuid,
            name: &self.name,
            x: pose.x,
            z: pose.z,
            yaw: pose.yaw,
            dimension: pose.dimension,
            game_time: pose.game_time,
        };
        let terrain = self.cache.terrain(source, u64::from(self.ticks));
        let has_ceiling = pose.dimension == Dimension::Nether;
        let mut updates = Vec::new();
        let flush_due = self.ticks % FLUSH_INTERVAL_TICKS == 0;
        let frames_due = self.ticks % FRAME_INTERVAL_TICKS == 0;
        let framed: BTreeSet<i32> = if frames_due {
            frames.iter().map(|frame| frame.map_id).collect()
        } else {
            self.framed.clone()
        };
        self.handle.with(|store| {
            let dropped: BTreeSet<i32> = self
                .carried
                .union(&self.framed)
                .copied()
                .filter(|id| !carried.contains_key(id) && !framed.contains(id))
                .collect();
            store.release(self.uuid, &dropped);
            for (&id, &in_hand) in &carried {
                let Some(map) = store.get(id) else { continue };
                map.tick_carried_by(&who);
                if in_hand && !map.locked {
                    map.sample_terrain(&who, &terrain, has_ceiling);
                }
                if let Some(update) = map.next_update(self.uuid, id) {
                    updates.push(update);
                }
            }
            if frames_due {
                for frame in frames {
                    let Some(map) = store.get(frame.map_id) else { continue };
                    map.tick_in_frame(&who, frame, carried.contains_key(&frame.map_id));
                    if let Some(update) = map.next_update(self.uuid, frame.map_id) {
                        updates.push(update);
                    }
                }
            }
            if flush_due {
                store.flush();
            }
        });
        self.carried = carried.into_keys().collect();
        self.framed = framed;
        updates
    }
}

impl Drop for MapSession {
    fn drop(&mut self) {
        let mut carried = std::mem::take(&mut self.carried);
        carried.append(&mut self.framed);
        self.handle.with(|store| {
            store.release(self.uuid, &carried);
            store.flush();
        });
    }
}

/// The map id a filled-map stack points at.
#[must_use]
pub fn filled_map_id(stack: &ItemStack) -> Option<i32> {
    (stack.item.path() == "filled_map").then_some(stack.components.map_id).flatten()
}

#[cfg(test)]
mod tests;
