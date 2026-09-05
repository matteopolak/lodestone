//! Why a sign in front of you is drawing no text — one log line per sign,
//! naming the block position and what the client actually holds for it.
//!
//! # What it is
//!
//! Sign text has now been reported blank twice against a real server after two
//! separate, individually-evidenced fixes (`lodestone_world::sign_text`'s
//! component parse, and `lodestone_core`'s `{"": element}` list unboxing). Both
//! fixes were reasoned from the codec and confirmed against captured bytes; the
//! signs stayed blank. What was never established is *which link of the chain
//! is empty* on the owner's server, and no gate in this tree can establish it,
//! because every sign gate installs its own fixture and therefore proves the
//! renderer rather than the supply.
//!
//! This module is the instrument that answers it from real play. It is also
//! what the "nothing may be silently skipped" rule asks for: a sign that
//! yields zero spans has to say why rather than draw an empty board.
//!
//! # How it works
//!
//! [`report`] scans a small box around the eye for block **states** that are
//! signs — deliberately state-first rather than record-first, because the
//! hypothesis that has never been excluded is *"the block-entity record never
//! arrived at all"*, and a walk over `chunk.block_entities` structurally
//! cannot see a sign that is missing from that list. For each sign state it
//! then asks, in order:
//!
//! * is there a block-entity record at this position at all? → [`Verdict::NoRecord`]
//! * does the record's type id match the one this state owns? → [`Verdict::TypeMismatch`]
//! * is the record's NBT `TAG_End` (a record synthesized by
//!   `World::sync_block_entity` from a bare state write, carrying no text)? →
//!   [`Verdict::EmptyNbt`]
//! * does the compound carry `front_text`/`back_text` at all? → [`Verdict::NoTextKeys`]
//! * do those parse into any spans? → [`Verdict::NoSpans`], which dumps a
//!   compact rendering of the real NBT so an unmodelled component kind
//!   (`translatable`, `selector`, `score`, `keybind`, `nbt`, `object` — see
//!   `lodestone_world::sign_text`'s module doc for what is deliberately not
//!   modelled) is visible in the line itself
//! * did the state resolve to a renderer and a placement? → [`Verdict::NoPlacement`]
//! * otherwise → [`Verdict::Ok`], with the per-side span counts and whether
//!   the position actually reached this frame's [`SignSpawn`] list.
//!
//! The last of those matters as much as the failures: a run in which every
//! sign reports `ok` with real span counts moves the defect *below* the
//! supply, into layout or draw, and that is a different investigation.
//!
//! A **decode error** is deliberately not one of the verdicts, because it
//! cannot reach here: `LevelChunkWithLight`'s decode runs `ensure_empty` over
//! the whole packet, so a malformed sign NBT rejects the entire chunk and
//! shows up as missing terrain, not as a blank sign.
//!
//! [`report`] also names, separately, any sign whose record **did** parse into
//! real spans and which is nonetheless absent from this frame's spawn list,
//! with its distance beside `block_entities::VIEW_DISTANCE`. "Culled" and
//! "had no text" are different answers and a log that conflates them sends the
//! next reader at the wrong layer; beyond the cutoff vanilla drops the sign
//! too and there is nothing to fix, inside it the *draw* dropped it and
//! [`report_draw_budget`] will have said so in the same run.
//!
//! # How to change it
//!
//! The scan is off unless `RUST_LOG` names the `signs` target at `debug`, and
//! it is rate-limited by a call counter rather than a clock — `Instant::now()` traps
//! on `wasm32` and this crate links into the browser bundle, so there is no
//! timer here on purpose.
//!
//! The scan is cubic in [`DEFAULT_SCAN_RADIUS`]; raising it past ~48 costs
//! real milliseconds per report. It is deliberately *narrower* than
//! `block_entities::VIEW_DISTANCE`, since the question is always about a sign
//! the owner is standing in front of.
//!
//! [`report_draw_budget`] uses the same opt-in `signs=debug` target. A dense
//! server can legitimately bind the finite buffer on every renderer epoch, so
//! this is diagnostic state rather than an unconditional warning. Its
//! recovery/re-entry transitions remain latched.
//!
//! # Configuration
//!
//! * `RUST_LOG=signs=debug` — turn the per-sign scan on. Nothing is scanned or
//!   logged by [`report`] otherwise; its whole body is behind one
//!   `tracing::enabled!` check. Budget transitions use this target too.
//! * `LODESTONE_SIGN_DIAG_RADIUS` — scan radius in blocks, default
//!   [`DEFAULT_SCAN_RADIUS`].
//! * `LODESTONE_SIGN_DIAG_INTERVAL` — report once every this many calls
//!   (one call per rendered frame), default [`DEFAULT_REPORT_INTERVAL`].
//!
//! # Dependencies
//!
//! `lodestone_data::block_states`/`block_entity_types` for the state tables,
//! `lodestone_world::SignText` for the parse under test, and
//! `crate::block_entities` for the two state resolvers production itself uses
//! — reused rather than re-derived, so a divergence between this instrument
//! and the real gather is impossible by construction.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use glam::Vec3;
use lodestone_data::block_states::StateId;
use lodestone_core::Nbt;
use lodestone_render::SignSpawn;
use lodestone_world::{ChunkPos, SignText, World};

use crate::net::SharedHandle;

/// Half-width, in blocks, of the box [`report`] scans for sign states.
pub const DEFAULT_SCAN_RADIUS: i32 = 24;

/// One report every this many calls — one call per rendered frame, so roughly
/// every two seconds at 60 Hz. The first call always reports.
pub const DEFAULT_REPORT_INTERVAL: u64 = 120;

/// The tracing target every line here uses: `RUST_LOG=signs=debug`.
const TARGET: &str = "signs";

/// Never print more than this many per-sign lines in one report — a wall of
/// signs must not drown the terminal, and the summary line still carries the
/// true totals.
const MAX_LINES: usize = 24;

/// How many characters of an NBT rendering one line may carry.
const MAX_NBT_CHARS: usize = 600;

static CALLS: AtomicU64 = AtomicU64::new(0);

/// What the client actually holds for one sign block state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The state is a sign and there is no block-entity record at that
    /// position at all — the chunk payload never carried one, or it was
    /// removed. This is the hypothesis two prior fixes could not exclude.
    NoRecord,
    /// A record exists but its `BLOCK_ENTITY_TYPE` id is not the one this
    /// block state owns, so it is some other block's stale record.
    TypeMismatch {
        /// The type id on the record.
        found: u32,
        /// The type id `lodestone_data::block_entity_types` says the state owns.
        expected: Option<u32>,
    },
    /// The record is present and its NBT is `TAG_End` — the shape
    /// `World::sync_block_entity` creates from a bare state write, and the
    /// shape a server that sent no `front_text` would leave.
    EmptyNbt,
    /// A real compound, carrying neither `front_text` nor `back_text`.
    NoTextKeys,
    /// `front_text`/`back_text` are present and the parse produced no spans on
    /// either side.
    NoSpans,
    /// Text parsed, but the block state resolves to no sign renderer or no
    /// placement, so `block_entities::sign_spawn` drops it.
    NoPlacement,
    /// Spans exist on at least one side.
    Ok {
        /// Span count across the front side's four lines.
        front: usize,
        /// Span count across the back side's four lines.
        back: usize,
    },
}

impl Verdict {
    /// The short token that leads each log line.
    const fn tag(&self) -> &'static str {
        match self {
            Verdict::NoRecord => "no-block-entity-record",
            Verdict::TypeMismatch { .. } => "block-entity-type-mismatch",
            Verdict::EmptyNbt => "record-nbt-is-TAG_End",
            Verdict::NoTextKeys => "no-front_text-or-back_text",
            Verdict::NoSpans => "parsed-zero-spans",
            Verdict::NoPlacement => "no-sign-kind-or-orientation",
            Verdict::Ok { .. } => "ok",
        }
    }

    /// Whether this verdict explains a blank board.
    const fn is_blank(&self) -> bool {
        !matches!(self, Verdict::Ok { .. })
    }
}

/// Classifies one sign state at `block` against the world's own record.
///
/// Split out from [`report`] so it is callable from a test that builds a real
/// [`World`] — the classification is the part worth pinning, not the scan.
#[must_use]
pub fn classify(world: &World, block: [i32; 3], state_id: StateId) -> Verdict {
    let [x, y, z] = block;
    let pos = ChunkPos::from_block(x, z);
    let Some(chunk) = world.get(pos) else {
        return Verdict::NoRecord;
    };
    let rel_x = (x & 15) as u8;
    let rel_z = (z & 15) as u8;
    let Some(record) = chunk
        .block_entities
        .iter()
        .find(|be| be.rel_x == rel_x && be.rel_z == rel_z && i32::from(be.y) == y)
    else {
        return Verdict::NoRecord;
    };

    let expected = lodestone_data::block_entity_types::block_entity_type(state_id)
        .map(|kind| kind.raw());
    if expected != Some(record.type_id) {
        return Verdict::TypeMismatch {
            found: record.type_id,
            expected,
        };
    }

    let Nbt::Compound(fields) = &record.nbt else {
        return Verdict::EmptyNbt;
    };
    let has_text_key = fields
        .iter()
        .any(|(name, _)| name == "front_text" || name == "back_text");
    if !has_text_key {
        return Verdict::NoTextKeys;
    }

    let text = SignText::parse(&record.nbt);
    let front: usize = text.front.lines.iter().map(Vec::len).sum();
    let back: usize = text.back.lines.iter().map(Vec::len).sum();
    if front == 0 && back == 0 {
        return Verdict::NoSpans;
    }
    if crate::block_entities::sign_kind_for_state(state_id).is_none()
        || crate::block_entities::sign_orientation(state_id).is_none()
    {
        return Verdict::NoPlacement;
    }
    Verdict::Ok { front, back }
}

/// Scans the box around `eye` and logs one line per sign that is not drawing
/// text, plus a summary. A no-op unless `RUST_LOG` enables the `signs` target
/// at `debug`, and then only once every [`DEFAULT_REPORT_INTERVAL`] calls.
///
/// `spawned` is this frame's real spawn list, so a line can say whether the
/// position reached the draw at all — the one fact a verdict derived purely
/// from world state cannot supply.
pub fn report(handle: &SharedHandle, eye: Vec3, spawned: &[SignSpawn]) {
    if !tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        return;
    }
    let interval = env_u64("LODESTONE_SIGN_DIAG_INTERVAL", DEFAULT_REPORT_INTERVAL).max(1);
    if CALLS.fetch_add(1, Ordering::Relaxed) % interval != 0 {
        return;
    }
    let Some(client) = handle.get() else {
        tracing::debug!(target: TARGET, "no client handle yet; nothing to scan");
        return;
    };
    let radius = i32::try_from(env_u64(
        "LODESTONE_SIGN_DIAG_RADIUS",
        DEFAULT_SCAN_RADIUS as u64,
    ))
    .unwrap_or(DEFAULT_SCAN_RADIUS)
    .clamp(1, 96);

    let store = client.chunk_world();
    let world = store.read();
    let found = scan(&world, eye, radius);

    let blank = found.iter().filter(|(_, _, v)| v.is_blank()).count();
    for (block, state_id, verdict) in found.iter().filter(|(_, _, v)| v.is_blank()).take(MAX_LINES)
    {
        let name = state_id.name();
        let drawn = spawned.iter().any(|s| s.pos == *block);
        let detail = detail_for(&world, *block, verdict);
        tracing::debug!(
            target: TARGET,
            "{} at {},{},{} ({name}, state {}, in this frame's spawn list: {drawn}){detail}",
            verdict.tag(),
            block[0],
            block[1],
            block[2], state_id.raw(),
        );
    }

    let ok: Vec<_> = found
        .iter()
        .filter_map(|(block, _, v)| match v {
            Verdict::Ok { front, back } => Some((block, front, back)),
            _ => None,
        })
        .collect();

    // **"Culled" and "no text" are different answers and the log has to say
    // which.** A sign whose record parsed into real spans and which is
    // nonetheless absent from this frame's spawn list was not silent about
    // its text — it was removed on the way to the draw, and the only two
    // things that remove one are `block_entities::VIEW_DISTANCE` and the
    // sign-text pass's own vertex budget. Printing the distance beside the
    // cutoff separates them without another instrument: beyond it, vanilla
    // drops the sign too and there is nothing to fix; inside it, the budget
    // did, and `report_draw_budget` will have warned in the same run.
    for (block, front, back) in ok
        .iter()
        .filter(|(block, _, _)| !spawned.iter().any(|s| s.pos == **block))
        .take(MAX_LINES)
    {
        let centre = Vec3::new(
            block[0] as f32 + 0.5,
            block[1] as f32 + 0.5,
            block[2] as f32 + 0.5,
        );
        let distance = centre.distance(eye);
        let verdict = if distance > crate::block_entities::VIEW_DISTANCE {
            "beyond VIEW_DISTANCE, as vanilla would also drop it"
        } else {
            "INSIDE VIEW_DISTANCE — the draw dropped it, not the gather"
        };
        tracing::debug!(
            target: TARGET,
            "has-text-but-not-drawn at {},{},{} (front {front}, back {back}):              {distance:.1} blocks from the eye against a {:.0}-block cutoff — {verdict}",
            block[0],
            block[1],
            block[2],
            crate::block_entities::VIEW_DISTANCE,
        );
    }

    tracing::debug!(
        target: TARGET,
        "scanned {radius} blocks around {:.1},{:.1},{:.1}: {} sign state(s), {blank} drawing no text, {} with spans ({}), {} spawn(s) submitted this frame",
        eye.x,
        eye.y,
        eye.z,
        found.len(),
        ok.len(),
        ok.iter()
            .take(MAX_LINES)
            .map(|(block, front, back)| format!(
                "{},{},{} front {front} back {back}",
                block[0], block[1], block[2]
            ))
            .collect::<Vec<_>>()
            .join("; "),
        spawned.len(),
    );
}

/// Whatever extra evidence this verdict needs to be actionable — the real NBT
/// for a parse that produced nothing, and nothing at all for a verdict that
/// already says everything.
fn detail_for(world: &World, block: [i32; 3], verdict: &Verdict) -> String {
    match verdict {
        Verdict::NoSpans | Verdict::NoTextKeys => match record_nbt(world, block) {
            Some(nbt) => format!(": nbt = {}", render_nbt(nbt)),
            None => String::new(),
        },
        _ => String::new(),
    }
}

fn record_nbt<'a>(world: &'a World, block: [i32; 3]) -> Option<&'a Nbt> {
    let [x, y, z] = block;
    let chunk = world.get(ChunkPos::from_block(x, z))?;
    let rel_x = (x & 15) as u8;
    let rel_z = (z & 15) as u8;
    chunk
        .block_entities
        .iter()
        .find(|be| be.rel_x == rel_x && be.rel_z == rel_z && i32::from(be.y) == y)
        .map(|be| &be.nbt)
}

/// Every sign block state in the box, classified.
fn scan(world: &World, eye: Vec3, radius: i32) -> Vec<([i32; 3], StateId, Verdict)> {
    let (ex, ey, ez) = (
        eye.x.floor() as i32,
        eye.y.floor() as i32,
        eye.z.floor() as i32,
    );
    let mut out = Vec::new();
    for cx in (ex - radius).div_euclid(16)..=(ex + radius).div_euclid(16) {
        for cz in (ez - radius).div_euclid(16)..=(ez + radius).div_euclid(16) {
            let Some(chunk) = world.get(ChunkPos { x: cx, z: cz }) else {
                continue;
            };
            let y_lo = (ey - radius).max(chunk.column.min_y());
            let y_hi = (ey + radius).min(chunk.column.max_y());
            for x in (ex - radius).max(cx * 16)..=(ex + radius).min(cx * 16 + 15) {
                for z in (ez - radius).max(cz * 16)..=(ez + radius).min(cz * 16 + 15) {
                    for y in y_lo..=y_hi {
                        let raw_state_id = chunk.column.get_block(
                            (x & 15) as usize,
                            y,
                            (z & 15) as usize,
                        );
                        let Some(state_id) = StateId::new(raw_state_id) else {
                            continue;
                        };
                        if crate::block_entities::sign_kind_for_state(state_id).is_some() {
                            let block = [x, y, z];
                            out.push((block, state_id, classify(world, block, state_id)));
                        }
                    }
                }
            }
        }
    }
    out
}

/// A compact, bounded, SNBT-ish rendering — enough to read a `messages` list
/// element by element and see an unmodelled component kind, without dumping a
/// shulker box's worth of items into a log line.
fn render_nbt(nbt: &Nbt) -> String {
    let mut out = String::new();
    write_nbt(nbt, &mut out);
    if out.chars().count() > MAX_NBT_CHARS {
        out = out.chars().take(MAX_NBT_CHARS).collect::<String>() + "…(truncated)";
    }
    out
}

fn write_nbt(nbt: &Nbt, out: &mut String) {
    use std::fmt::Write as _;
    match nbt {
        Nbt::End => out.push_str("END"),
        Nbt::Byte(v) => {
            let _ = write!(out, "{v}b");
        }
        Nbt::Short(v) => {
            let _ = write!(out, "{v}s");
        }
        Nbt::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Nbt::Long(v) => {
            let _ = write!(out, "{v}L");
        }
        Nbt::Float(v) => {
            let _ = write!(out, "{v}f");
        }
        Nbt::Double(v) => {
            let _ = write!(out, "{v}d");
        }
        Nbt::ByteArray(v) => {
            let _ = write!(out, "[B;{} bytes]", v.len());
        }
        Nbt::IntArray(v) => {
            let _ = write!(out, "[I;{} ints]", v.len());
        }
        Nbt::LongArray(v) => {
            let _ = write!(out, "[L;{} longs]", v.len());
        }
        // Quoted and escaped: a collapsed literal that is empty, or that has
        // leading spaces, must be distinguishable from an absent one — that
        // difference is the whole question for a blank line.
        Nbt::String(v) => {
            let _ = write!(out, "{v:?}");
        }
        Nbt::List {
            element_type,
            elements,
        } => {
            let _ = write!(out, "[{element_type:?}:");
            for (i, element) in elements.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_nbt(element, out);
            }
            out.push(']');
        }
        Nbt::Compound(fields) => {
            out.push('{');
            for (i, (name, value)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                let _ = write!(out, "{name}:");
                write_nbt(value, out);
            }
            out.push('}');
        }
    }
}

/// Per-renderer state for the sign-text budget diagnostic. The renderer owns
/// this rather than using process-global state so a newly-created renderer
/// (and therefore a new world/render resource epoch) gets one fresh report.
#[derive(Debug, Default)]
pub(crate) struct BudgetWarningState {
    exhausted: AtomicBool,
    warned: AtomicBool,
}

/// The only transitions worth logging. In particular, a changed non-zero
/// drop count is not a transition: camera movement can change which signs fit
/// while the pass remains continuously exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BudgetEvent {
    Exhausted,
    Reentered,
    Recovered,
}

fn budget_event(state: &BudgetWarningState, dropped: usize) -> Option<BudgetEvent> {
    if dropped == 0 {
        return state
            .exhausted
            .swap(false, Ordering::Relaxed)
            .then_some(BudgetEvent::Recovered);
    }

    if state.exhausted.swap(true, Ordering::Relaxed) {
        return None;
    }
    if state.warned.swap(true, Ordering::Relaxed) {
        Some(BudgetEvent::Reentered)
    } else {
        Some(BudgetEvent::Exhausted)
    }
}

/// Says at debug level that the sign-text pass could not draw
/// every sign the gather handed it.
///
/// This exists because the pass used to drop the tail of its list in
/// complete silence — no log, no counter, no red test — which is the failure
/// mode `CLAUDE.md`'s "nothing may be silently skipped" rule is about, and
/// which presents to a player as whole boards blinking in and out as they
/// move. The report is gated behind `RUST_LOG=signs=debug` and emits only on
/// transitions. Camera movement can briefly recover and re-exhaust the pass;
/// a newly-created renderer gets a fresh lifetime latch.
///
/// * `gathered` — what `block_entities::sign_spawns` returned.
/// * `in_front` — how many of those were not discarded as behind the eye.
/// * `drawn` — how many actually reached the vertex buffer.
pub(crate) fn report_draw_budget(
    state: &BudgetWarningState,
    gathered: usize,
    in_front: usize,
    drawn: usize,
    vertices: usize,
    capacity: usize,
) {
    let dropped = in_front.saturating_sub(drawn);
    match budget_event(state, dropped) {
        Some(BudgetEvent::Recovered) => tracing::debug!(
            target: TARGET,
            "sign-text budget no longer binding: {drawn} of {gathered} gathered sign(s)              drawn, {vertices}/{capacity} vertices"
        ),
        Some(BudgetEvent::Exhausted) => tracing::debug!(
            target: TARGET,
            "sign-text budget exhausted: {dropped} of {in_front} in-front sign(s) drew NO text ({gathered} gathered, {vertices}/{capacity} vertices); farthest signs dropped first"
        ),
        Some(BudgetEvent::Reentered) => tracing::debug!(
            target: TARGET,
            "sign-text budget exhausted again: {dropped} in-front sign(s) drew NO text; initial report already emitted for this renderer"
        ),
        None => {}
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::NbtTag;
    use lodestone_world::{ChunkColumn, ColumnLight, Heightmaps, LoadedChunk, PaletteKind};

    /// The **verbatim** `front_text`/`back_text` compound a real vanilla 26.2
    /// server put on the wire for a mixed-style sign, read out of a live
    /// `World` by `tests/live_sign_text_wire.rs` and pasted here so this gate
    /// keeps working with no container running.
    ///
    /// Note the shape the two prior fixes were about: `messages` declares
    /// `element_type: Compound` because two of its four elements are styled,
    /// and the two unstyled ones arrive as bare strings only because
    /// `lodestone_core`'s reader already stripped their `{"": …}` box.
    fn live_sign_nbt() -> Nbt {
        let side = |messages: Vec<Nbt>, element_type: NbtTag| {
            Nbt::Compound(vec![
                ("has_glowing_text".to_owned(), Nbt::Byte(0)),
                ("color".to_owned(), nbt_string("black")),
                (
                    "messages".to_owned(),
                    Nbt::List {
                        element_type,
                        elements: messages,
                    },
                ),
            ])
        };
        Nbt::Compound(vec![
            (
                "back_text".to_owned(),
                side(
                    vec![
                        nbt_string(""),
                        nbt_string(""),
                        nbt_string(""),
                        nbt_string(""),
                    ],
                    NbtTag::String,
                ),
            ),
            ("is_waxed".to_owned(), Nbt::Byte(0)),
            (
                "front_text".to_owned(),
                side(
                    vec![
                        Nbt::Compound(vec![
                            ("color".to_owned(), nbt_string("red")),
                            ("text".to_owned(), nbt_string("REDLINE")),
                        ]),
                        Nbt::Compound(vec![
                            ("text".to_owned(), nbt_string("BOLDY")),
                            ("bold".to_owned(), Nbt::Byte(1)),
                        ]),
                        nbt_string("plain"),
                        nbt_string(""),
                    ],
                    NbtTag::Compound,
                ),
            ),
        ])
    }

    const SIGN_BLOCK: [i32; 3] = [3, 65, 9];

    fn oak_sign_state() -> StateId {
        StateId::from_state_str("minecraft:oak_sign").expect("26.2 has minecraft:oak_sign")
    }

    /// A world holding one chunk whose block at [`SIGN_BLOCK`] is a standing
    /// oak sign, with whatever block-entity records the caller supplies.
    fn world_with(records: Vec<lodestone_world::BlockEntity>) -> World {
        let mut column = ChunkColumn::new(
            -64,
            24,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            0,
            0,
        );
        let [x, y, z] = SIGN_BLOCK;
        column.set_block((x & 15) as usize, y, (z & 15) as usize, oak_sign_state());
        let mut world = World::default();
        world.load(
            ChunkPos::from_block(x, z),
            LoadedChunk::new(column, ColumnLight::new(24), Heightmaps::new(), records),
        );
        world
    }

    fn record(nbt: Nbt) -> lodestone_world::BlockEntity {
        let [x, y, z] = SIGN_BLOCK;
        lodestone_world::BlockEntity {
            rel_x: (x & 15) as u8,
            rel_z: (z & 15) as u8,
            y: y as i16,
            type_id: lodestone_data::block_states::StateId::new(oak_sign_state())
                .and_then(lodestone_data::block_entity_types::block_entity_type)
                .map(|kind| kind.raw())
                .expect("a sign state owns a block-entity type"),
            nbt,
        }
    }

    /// **The verdict the whole instrument exists for.** A sign block state
    /// whose chunk carried no block-entity record at all is the one
    /// hypothesis two prior fixes could not exclude, and it is invisible to
    /// any walk over `chunk.block_entities`.
    #[test]
    fn a_sign_state_with_no_record_reports_no_record() {
        let world = world_with(Vec::new());
        assert_eq!(
            classify(&world, SIGN_BLOCK, oak_sign_state()),
            Verdict::NoRecord
        );
    }

    /// The record `World::sync_block_entity` synthesizes from a bare state
    /// write. It is a *different* bug from a missing record — the chunk
    /// payload had nothing to say versus the client inventing the entry — and
    /// the log line has to separate them.
    #[test]
    fn a_record_carrying_tag_end_reports_empty_nbt() {
        let world = world_with(vec![record(Nbt::End)]);
        assert_eq!(
            classify(&world, SIGN_BLOCK, oak_sign_state()),
            Verdict::EmptyNbt
        );
    }

    /// A type id that is not the one the state owns — a stale record left by
    /// some other block — must not be read as this sign's text.
    #[test]
    fn a_foreign_type_id_reports_a_mismatch() {
        let mut stale = record(live_sign_nbt());
        let expected = stale.type_id;
        stale.type_id = expected.wrapping_add(1);
        let world = world_with(vec![stale]);
        assert_eq!(
            classify(&world, SIGN_BLOCK, oak_sign_state()),
            Verdict::TypeMismatch {
                found: expected.wrapping_add(1),
                expected: Some(expected),
            }
        );
    }

    /// A component kind this parse does not model (`selector`) yields no
    /// spans, which is the *other* remaining hypothesis for the owner's
    /// signs. It has to be distinguishable from a missing record, not folded
    /// into one "no text" outcome.
    ///
    /// Not `translate`: that component kind is modelled (it resolves against
    /// a caller-supplied table, falling back to its own key with no table in
    /// hand), so it now reports a span rather than none — `keybind`, `score`,
    /// `selector`, `nbt` and `object` are the ones this parse still leaves
    /// unmodelled.
    #[test]
    fn an_unmodelled_component_reports_zero_spans() {
        let messages = Nbt::List {
            element_type: NbtTag::Compound,
            elements: vec![
                Nbt::Compound(vec![("selector".to_owned(), nbt_string("@e"))]),
                nbt_string(""),
                nbt_string(""),
                nbt_string(""),
            ],
        };
        let nbt = Nbt::Compound(vec![(
            "front_text".to_owned(),
            Nbt::Compound(vec![("messages".to_owned(), messages)]),
        )]);
        let world = world_with(vec![record(nbt)]);
        assert_eq!(
            classify(&world, SIGN_BLOCK, oak_sign_state()),
            Verdict::NoSpans
        );
    }

    /// The healthy arm, against bytes a **real vanilla server** produced: the
    /// mixed-style sign resolves to three front spans and no back spans. This
    /// is the arm that makes a green run informative rather than vacuous — if
    /// the owner's log says `ok front 3`, the defect is below the supply.
    #[test]
    fn the_live_captured_sign_classifies_as_ok_with_real_span_counts() {
        let world = world_with(vec![record(live_sign_nbt())]);
        assert_eq!(
            classify(&world, SIGN_BLOCK, oak_sign_state()),
            Verdict::Ok { front: 3, back: 0 }
        );
    }

    /// The scan is what finds a sign the record-walk cannot see, so it has to
    /// be exercised in its own right: it must locate the state by position
    /// and hand back the same verdict [`classify`] would.
    #[test]
    fn the_scan_finds_a_recordless_sign_by_its_block_state() {
        let world = world_with(Vec::new());
        let [x, y, z] = SIGN_BLOCK;
        let eye = Vec3::new(x as f32, y as f32, z as f32);
        let found = scan(&world, eye, 4);
        assert_eq!(
            found,
            vec![(SIGN_BLOCK, oak_sign_state(), Verdict::NoRecord)],
            "the scan must see a sign whose record is absent"
        );
    }

    /// The negative control for the scan: move the eye out of range and the
    /// same world yields nothing, so a finding is a real locate rather than
    /// the scan reporting whatever it was handed.
    #[test]
    fn the_scan_reports_nothing_when_the_sign_is_out_of_range() {
        let world = world_with(Vec::new());
        let [x, y, z] = SIGN_BLOCK;
        let far = Vec3::new(x as f32, (y + 40) as f32, z as f32);
        assert!(scan(&world, far, 4).is_empty());
    }

    fn nbt_string(v: &str) -> Nbt {
        Nbt::String(v.to_owned())
    }

    /// The rendering has to keep an **empty** string distinguishable from an
    /// absent element, because "line 3 arrived as `""`" and "line 3 never
    /// arrived" are two different bugs and the log line is the only place the
    /// difference is visible.
    #[test]
    fn an_empty_string_element_renders_as_a_quoted_empty_string() {
        let rendered = render_nbt(&Nbt::List {
            element_type: NbtTag::String,
            elements: vec![nbt_string("hi"), nbt_string("")],
        });
        assert_eq!(rendered, "[String:\"hi\",\"\"]");
    }

    /// An unmodelled component kind must be readable straight off the line —
    /// this is one of the two hypotheses the instrument exists to separate,
    /// and a renderer that elided compound keys would hide it.
    #[test]
    fn an_unmodelled_component_kind_survives_into_the_rendering() {
        let rendered = render_nbt(&Nbt::Compound(vec![(
            "translate".to_owned(),
            nbt_string("block.minecraft.stone"),
        )]));
        assert!(
            rendered.contains("translate"),
            "an unmodelled kind must be visible: {rendered}"
        );
    }

    /// A `tracing` writer that keeps everything in a buffer, so a gate can
    /// assert on the **line the owner will actually see** rather than on the
    /// state that would have produced it.
    #[derive(Clone, Default)]
    struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Capture {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
        }
    }

    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Runs [`report`] under a captured subscriber built from `filter`,
    /// enough times to clear the call-count rate limit, and returns whatever
    /// reached the log.
    fn captured(filter: &str) -> String {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
            .finish();
        let handle: crate::net::SharedHandle = std::sync::Arc::new(std::sync::OnceLock::new());
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..=DEFAULT_REPORT_INTERVAL {
                report(&handle, Vec3::ZERO, &[]);
            }
        });
        capture.text()
    }

    /// **The control that this instrument is reachable at all.** Five of
    /// `wasm-check.sh`'s rules reported PASS for weeks while their detector
    /// was erroring; the lesson is that a guard nobody has watched fire has
    /// measured nothing. This drives the real `report` entry point under a
    /// real subscriber and asserts a line came out.
    #[test]
    fn report_emits_a_line_when_the_signs_target_is_enabled() {
        let out = captured("signs=debug");
        assert!(
            out.contains("no client handle yet"),
            "RUST_LOG=signs=debug must produce a line, got: {out:?}"
        );
    }

    /// The other half, and the reason the first half is not vacuous: with the
    /// target off, the identical call sequence must emit nothing. Without
    /// this, a subscriber that logged unconditionally would satisfy the gate
    /// above.
    #[test]
    fn report_is_silent_when_the_signs_target_is_off() {
        let out = captured("signs=off");
        assert!(out.is_empty(), "expected silence, got: {out:?}");
    }

    /// Camera movement can change the exact number of signs that fit without
    /// actually recovering the pass. That churn must not turn one persistent
    /// overflow into a warning every frame.
    #[test]
    fn budget_warning_latches_until_the_pass_recovers() {
        let state = BudgetWarningState::default();

        assert_eq!(budget_event(&state, 24), Some(BudgetEvent::Exhausted));
        assert_eq!(budget_event(&state, 25), None);
        assert_eq!(budget_event(&state, 24), None);
        assert_eq!(budget_event(&state, 0), Some(BudgetEvent::Recovered));
        assert_eq!(budget_event(&state, 1), Some(BudgetEvent::Reentered));
        assert_eq!(budget_event(&state, 2), None);
        assert_eq!(budget_event(&state, 0), Some(BudgetEvent::Recovered));
        assert_eq!(budget_event(&state, 3), Some(BudgetEvent::Reentered));
    }

    /// The initial debug report must remain one line even when the camera
    /// oscillates across the budget boundary several times.
    #[test]
    fn budget_diagnostic_emits_only_one_initial_report_per_renderer() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_env_filter(tracing_subscriber::EnvFilter::new("signs=debug"))
            .finish();
        let state = BudgetWarningState::default();

        tracing::subscriber::with_default(subscriber, || {
            report_draw_budget(&state, 191, 191, 167, 524_000, 524_288);
            report_draw_budget(&state, 191, 191, 191, 524_288, 524_288);
            report_draw_budget(&state, 191, 191, 166, 524_100, 524_288);
            report_draw_budget(&state, 191, 191, 191, 524_288, 524_288);
            report_draw_budget(&state, 191, 191, 165, 524_200, 524_288);
        });

        let out = capture.text();
        assert_eq!(
            out.matches("sign-text budget exhausted:").count(),
            1,
            "alternating overflow/recovery must not repeat the initial report: {out:?}"
        );
        assert!(
            out.contains("initial report already emitted for this renderer"),
            "re-entry should remain diagnosable at debug level: {out:?}"
        );
    }

    /// The bound is on the *rendering*, not on the input, so a shulker box's
    /// NBT cannot push a real finding off the end of the terminal.
    #[test]
    fn a_huge_compound_is_truncated_and_says_so() {
        let fields = (0..500)
            .map(|i| (format!("field_{i}"), Nbt::Int(i)))
            .collect();
        let rendered = render_nbt(&Nbt::Compound(fields));
        assert!(rendered.ends_with("…(truncated)"), "{rendered}");
        assert!(rendered.chars().count() < MAX_NBT_CHARS + 32);
    }
}
