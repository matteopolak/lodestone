//! Per-block-state light dampening and emission for protocol 776 (Minecraft
//! 26.2) — the two inputs vanilla's light engine reads off a block state, and
//! the only game data `lodestone-world`'s propagation engine needs.
//!
//! # What it is
//!
//! `lodestone_world::LightProperties` is deliberately an *injected* trait: the
//! propagation engine stays version- and registry-free, and whoever runs it hands
//! in `opacity(state)`/`emission(state)` for the block-state ids that engine will
//! see. This module is the 26.2 answer to that trait — the census that lets the
//! integrated server (and singleplayer worldgen) light a real chunk instead of
//! sending a constant.
//!
//! * **dampening** is vanilla's `BlockState.getLightDampening()`
//!   (`BlockBehaviour.java:298-305`), `0..=15`. The engine applies `max(1, ·)`
//!   itself — see `LightEngine.getOpacity`, `LightEngine.java:77-79` — so this is
//!   the **raw** dampening, not the stepped opacity. Air and glass are `0`; water,
//!   ice and leaves are `1`; a full solid is `15`.
//! * **emission** is `BlockState.getLightEmission()`, `0..=15`. Non-emitters are
//!   `0`.
//!
//! Keyed by **global block-state id**, not block id, because both quantities are
//! per state: a lit furnace differs from an unlit one, and a double slab occludes
//! where a bottom slab does not.
//!
//! # How it works
//!
//! `tests/light_props.rs` owns the generator, the provenance argument and the
//! drift guard; read its module docs before changing anything here. The short
//! version: this is **not** a JVM dump, because neither quantity is exposed on
//! `BlockBehaviour.Properties` (emission is a private `ToIntFunction<BlockState>`,
//! dampening a protected method with per-block overrides). It is generated from
//! `vendor/minecraft-data/data/pc/1.21.11/blocks.json`'s `filterLight`/`emitLight`
//! — cross-checked against vanilla's own formula in the decompiled 26.2 tree on
//! the cases that formula discriminates — plus the 30 blocks 26.2 adds and three
//! per-state corrections (`type=double`, `waterlogged=true`, `lit=false`).
//!
//! # How to change it, and the one gotcha
//!
//! **Every gap in this table darkens or occludes; none brightens.** Keep it that
//! way. `crates/protocol/v770/tests/live_terrain_light.rs` judges our light engine
//! against a real vanilla server by asserting we never produce *more* light than
//! it does, and that claim is sound only because a props shortfall cannot fake it.
//! A "fix" that guesses an emission upward — or that drops the `lit=false`
//! correction — silently invalidates that gate's argument while leaving it green.
//!
//! To extend: add the state's real behaviour to `tests/light_props.rs` (a
//! `NEW_IN_26_2` row or a fourth per-state correction), then
//! `LODESTONE_REGEN=1 cargo test -p lodestone-data --test light_props \
//! committed_table_matches_source -- --ignored`.
//!
//! # Memory design
//!
//! Both values are `0..=15`, so at most 256 distinct pairs can exist. The table
//! is therefore a de-duplicated `ENTRIES` array plus a per-state `u8` index —
//! 32,366 bytes of rodata and **zero heap**, a third of the neighbouring
//! per-state tables' footprint. Lookup is one bounds-checked index, so calling it
//! once per cell over a 98,304-cell column (which the light engine does) costs no
//! allocation and no search.
//!
//! # Dependencies
//!
//! None beyond [`crate::generated_light_props`]. Consumers:
//! `lodestone-v770`'s `V770ServerProtocol` (the integrated server's chunk
//! encoder) and any host wiring `lodestone_world::LightProperties`.

use crate::generated_light_props as table;

pub use table::STATE_COUNT;

/// The `(dampening, emission)` pair for block-state `id`, or `None` if `id` is
/// not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: reads straight from rodata. O(1) indexing, no search.
#[must_use]
pub fn light_props(id: u32) -> Option<(u8, u8)> {
    let &entry = table::STATE_ENTRY.get(id as usize)?;
    Some(table::ENTRIES[entry as usize])
}

/// Vanilla `BlockState.getLightDampening()` for `id` — the **raw** dampening,
/// `0..=15`, before the engine's `max(1, ·)`. `0` for an id out of range, which
/// is the transparent (and therefore harmless) default; use
/// [`light_props`] when the distinction between "transparent" and "unknown id"
/// matters.
#[must_use]
pub fn dampening(id: u32) -> u8 {
    light_props(id).map_or(0, |(dampening, _)| dampening)
}

/// Vanilla `BlockState.getLightEmission()` for `id`, `0..=15`. `0` for an id out
/// of range — never invent light for an id we cannot resolve.
#[must_use]
pub fn emission(id: u32) -> u8 {
    light_props(id).map_or(0, |(_, emission)| emission)
}
