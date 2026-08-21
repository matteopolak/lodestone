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
//! # How to change it
//!
//! It is off unless `RUST_LOG` names the `signs` target at `debug`, and it is
//! rate-limited by a call counter rather than a clock — `Instant::now()` traps
//! on `wasm32` and this crate links into the browser bundle, so there is no
//! timer here on purpose.
//!
//! The scan is cubic in [`DEFAULT_SCAN_RADIUS`]; raising it past ~48 costs
//! real milliseconds per report. It is deliberately *narrower* than
//! `block_entities::VIEW_DISTANCE`, since the question is always about a sign
//! the owner is standing in front of.
//!
//! # Configuration
//!
//! * `RUST_LOG=signs=debug` — turn it on. Nothing is scanned or logged
//!   otherwise; the whole body is behind one `tracing::enabled!` check.
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

use std::sync::atomic::{AtomicU64, Ordering};

use glam::Vec3;
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
pub fn classify(world: &World, block: [i32; 3], state_id: u32) -> Verdict {
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

    let expected = lodestone_data::block_entity_types::block_entity_type(state_id);
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
        let name = lodestone_data::block_states::block_name(*state_id).unwrap_or("<unknown state>");
        let drawn = spawned.iter().any(|s| s.pos == *block);
        let detail = detail_for(&world, *block, verdict);
        tracing::debug!(
            target: TARGET,
            "{} at {},{},{} ({name}, state {state_id}, in this frame's spawn list: {drawn}){detail}",
            verdict.tag(),
            block[0],
            block[1],
            block[2],
        );
    }

    let ok: Vec<_> = found
        .iter()
        .filter_map(|(block, _, v)| match v {
            Verdict::Ok { front, back } => Some((block, front, back)),
            _ => None,
        })
        .collect();
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
fn scan(world: &World, eye: Vec3, radius: i32) -> Vec<([i32; 3], u32, Verdict)> {
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
                        let state_id = chunk.column.get_block(
                            (x & 15) as usize,
                            y,
                            (z & 15) as usize,
                        );
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
