//! Block light after a player's edit — the "torches emit no light" half.
//!
//! # What this is, and what the audit actually found
//!
//! This module holds the *decision* half of relighting: given the block state a
//! cell held and the one it now holds, does the column's **block light** need
//! recomputing and resending? [`crate::server`] is the consumer; the recompute
//! itself is not here at all, and that is the finding worth reading before
//! changing anything.
//!
//! The reported symptom was "torches don't emit light". The plausible diagnoses
//! were *"the server computes sky light only"* and *"it computes block light but
//! the emission source is not wired"*, and **both are wrong**:
//!
//! * `V770ServerProtocol::encode_chunk` calls
//!   `server_protocol::compute_served_light`, which is
//!   `lodestone_world::compute_column_light(column, &V770LightProps)`;
//! * that engine seeds *both* layers — sky from every cell open to the sky at
//!   `15`, **block light from every cell whose block emits** (`lighting.rs`'s own
//!   module doc, and `BlockLightEngine` behind it);
//! * `V770LightProps::emission` forwards straight to
//!   [`lodestone_data::light_props::emission`], whose census carries
//!   `minecraft:torch` at **14** and gates it in
//!   `lodestone-data/tests/light_props.rs`;
//! * so a torch that exists when a column is *encoded* really does light it.
//!
//! What was missing is the *update*. Light is computed once per column at serve
//! time, and after join nothing re-sends it:
//!
//! | link | state when this module was written |
//! |---|---|
//! | `LIGHT_UPDATE` (packet 48) client decode | **present** — `v770/src/adapter.rs` reads all six fields and calls `World::merge_light` |
//! | `LightPatch::from_light_masks` merge semantics | **present**, gated |
//! | client re-mesh on a light change | **present** — the decode arm emits `ClientEvent::ChunkLoaded`, which doubles as "this region is dirty" |
//! | **server-side `LIGHT_UPDATE` encoder** | **absent — no `ServerProtocol` method, no v770 override** |
//!
//! Eight of nine links green and the ninth never built: the island shape
//! `CLAUDE.md`'s first rule names. Placing a torch therefore changed the block
//! (the client got a `BLOCK_UPDATE` and drew the torch) and could not change the
//! light, because the packet that carries light never left the server.
//!
//! # The ninth link now exists
//!
//! [`crate::protocol::ServerProtocol::encode_light_update`] plus
//! [`compute_column_light`](crate::protocol::ServerProtocol::compute_column_light),
//! with the v770 overrides beside `encode_chunk`. So
//! [`crate::server`]'s `resend_column_for_light` sends a real light-only packet: a
//! few KiB of nibble arrays, no chunk batch (vanilla's `PlayerChunkSender` flow
//! control counts chunk *batches*, and a `light_update` is not one), and none of
//! `encode_column_body`'s palette/heightmap/NBT work.
//!
//! `tests/light_update.rs`'s `encode_light_update_matches_the_golden_wire_body`
//! pins the encoder against the hand-written golden body the *decode* arm was
//! already gated on — an outside expectation, and the only kind that can catch
//! the real trap here: the wire order is sky / block / empty-sky / empty-block
//! masks then the two array lists, which is **not**
//! `LightPatch::from_light_masks`' argument order. A transposition there is a
//! well-formed packet the client mis-merges in silence, so a round-trip through
//! our own decoder cannot see it.
//!
//! ## The column resend it replaced, kept as the fallback
//!
//! Before that, [`crate::server`] answered an emission-changing edit by re-sending
//! the whole column through `begin_chunk_batch`/`encode_chunk`/`end_chunk_batch`.
//! That still runs for a family implementing neither new method (both default to
//! "nothing"), so adoption is per family and the old path stays correct — but it
//! cost a full column, a few tens of KiB and 62 M instructions of `encode_chunk`,
//! per placed torch.
//!
//! Either way it is affordable **because [`should_relight`] is narrow** — it fires
//! only when emission actually changes, which is torches, lanterns, glowstone, sea
//! lanterns, campfires, a furnace lighting, and little else. It does not fire when
//! you mine a wall and let daylight in; see the gaps below.
//!
//! **What is still on the connection task is the `source.column(cx, cz)` fetch**,
//! and it is now the dominant cost of this path: a retained-column clone warm, a
//! full generation cold. Making that cheap is the same change gap 2 below needs —
//! light computed in the chunk source and carried on the column.
//!
//! # The named gaps
//!
//! Both survived the encoder, and that is worth stating plainly: the encoder was
//! the blocker named for each of them, and neither fell out of building it,
//! because neither is a *transport* problem. `light_update` is a cheaper carrier
//! for the same values, not a better computation. **Gap 1 is now closed; gap 2
//! is not.**
//!
//! **1. Sky light did not follow an edit — closed.** [`should_relight`] compared
//! *emission* only, so breaking a roof, a tree trunk or a floor changed the
//! block and could not change the light: dirt emits `0` and so does the air that
//! replaces it. The reported symptom was a freshly opened column staying pitch
//! black, and the diagnosis worth keeping is that **nothing was wrong with the
//! propagation**. `compute_column_light` floods from zero and seeds every cell
//! open to the sky at `15`, so it gets a newly opened shaft right on the first
//! try; the missing piece was entirely the trigger, one predicate away from the
//! flood that already worked.
//!
//! It now compares [`dampening`] as well, so it fires on essentially every
//! placement and break. The version of this paragraph that talked itself out of
//! that widening reasoned from cost — a full `compute_column_light` per block
//! placed, on the connection task — and the cost is real but it was never large:
//! ≈1.0 ms in release (`crates/protocol/v770/tests/server_light.rs`) against a
//! player edit rate of a handful per second. Incremental re-propagation from the
//! changed cell, which vanilla has and this crate does not, is the optimisation;
//! it was never the correctness fix, and treating it as one is what kept the bug
//! alive.
//!
//! **2. Light does not cross a chunk border**, so a torch at local `x = 15` does
//! not light its eastern neighbour at all. This is *not* a gap in this module: it
//! is the **isolated** compute, and it is the same open item
//! `docs/server-chunk-light.md` records as a measured **Δ5** sky-light dark bias
//! at column borders. `lodestone_world::compute_column_light_with_neighbours` is
//! exact for a centre column and would fix it — but it costs ~9× one column, and
//! the *neighbours'* light changes too, so answering one torch honestly means nine
//! packets and nine 3×3 floods. That is not something to do on the connection
//! task, and the shape that makes it affordable is the same one §12.117 already
//! names: compute light in the chunk source, where the neighbourhood is already
//! resident, and carry it on [`crate::ChunkColumn`] — plus one trap that plan does
//! not mention, recorded here because it would make the fix look like it worked
//! and serve stale light:
//!
//! > If `ChunkColumn` carries a precomputed `light`, then
//! > `ChunkColumn::set_block` and `ChunkStore::set_block` **must invalidate it**.
//! > Both write blocks into a retained column without touching anything derived
//! > from them. A `column.light()` that survives an edit would make the resend
//! > above serve the light the column had *before* the torch was placed — a
//! > correct-looking wire, a re-meshed client, and no change on screen.
//!
//! **3. Only the acting connection is told.** The update rides that
//! connection's own `Connection`, like every other confirmation in
//! `dispatch_play_packet`. On a singleplayer integrated server that is every
//! player; on open-to-LAN a second player sees the torch and not its light until
//! they leave and re-enter the column.

/// The block-light emission of one canonical block-state string, `0..=15`.
///
/// Straight through [`lodestone_data::light_props`], resolved by
/// [`crate::chunk::resolve_palette_state_id`] so a state string this crate
/// stores and one the encoder resolves cannot disagree about what a bare block
/// name means.
///
/// A state the census does not carry answers `0`. That direction is the safe one
/// and it is the census's own convention: *every gap in the props census darkens
/// or occludes; never brighten one* (`docs/server-chunk-light.md`). A wrong `0`
/// here costs one missed relight; a wrong non-zero would fire a column resend on
/// an ordinary placement.
#[must_use]
pub fn emission(state: &str) -> u8 {
    lodestone_data::light_props::emission(crate::chunk::resolve_palette_state_id(state))
}

/// The block-light **dampening** of one canonical block-state string, `0..=15` —
/// the real per-state light-dampening rule, raw, before the engine's own
/// `max(1, ·)` floor.
///
/// The other half of what a light computation reads off a state, resolved the
/// same way [`emission`] is. Air and glass are `0`; water, ice and leaves are
/// `1`; a full solid is `15`. A state the census does not carry answers `0`,
/// which is the transparent direction — see [`emission`] for why every gap in
/// this census darkens or occludes rather than brightening.
#[must_use]
pub fn dampening(state: &str) -> u8 {
    lodestone_data::light_props::dampening(crate::chunk::resolve_palette_state_id(state))
}

/// `true` iff replacing `old` with `new` changes the light the cell **emits or
/// occludes** — the gate on the relight described in this module's own doc
/// comment.
///
/// Compared on the **values**, not on the state strings: `minecraft:torch` and
/// `minecraft:wall_torch[facing=north]` both emit 14 and dampen 0, so
/// re-orienting a torch is not a relight, while lighting a furnace
/// (`lit=false` → `lit=true`, `0` → `13`) is. That is also why this cannot be
/// `emission(new) > 0`: **removing** a light source has to relight too, and a
/// break writes `minecraft:air`.
///
/// # Why dampening is in here, and what it fixed
///
/// It was emission only, and that made the *reported* symptom — break the trunk
/// of a tree and one dirt block under it, and the hole is pitch black — a
/// guaranteed outcome rather than a bug with a cause. Nothing about the
/// propagation was wrong: the engine floods from zero over the whole column, so
/// sky light really does fall down a freshly opened shaft at full strength. What
/// was missing was the **trigger**. Dirt and logs emit `0` before the break and
/// air emits `0` after it, so an emission-only comparison saw no change, no
/// light packet left the server, and the client — which deliberately never
/// recomputes light on the live path — kept serving the pre-break value, which
/// under a tree is `0`.
///
/// So the predicate has to compare the quantity the edit actually moved.
/// Breaking dirt is `15 → 0` in dampening, and placing it is `0 → 15`; both must
/// relight, in both directions, for the same reason removing a torch must.
///
/// This does mean it now fires on essentially every ordinary placement and
/// break, where before it fired only for the small emissive set. That is the
/// intended trade and it is affordable: the work behind it is one
/// `compute_column_light` over the edited column, measured at ≈1.0 ms in release
/// (`crates/protocol/v770/tests/server_light.rs`), plus a few KiB `light_update`
/// — against a player edit rate of a handful per second. What is *not*
/// affordable, and is still not attempted here, is recomputing the eight
/// neighbours as well; see this module's doc for that gap and its shape.
///
/// A state whose *only* change is decorative — `axis`, `facing`, `waterlogged`
/// on a full solid — still costs nothing, because both quantities compare equal.
#[must_use]
pub fn should_relight(old: &str, new: &str) -> bool {
    emission(old) != emission(new) || dampening(old) != dampening(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The census values this module's gate depends on, predicted from
    /// `lodestone-data/tests/light_props.rs`'s own committed table rather than
    /// from anything here. A change to the census that silently zeroed these
    /// would disable relighting entirely, and nothing else would notice.
    #[test]
    fn the_emissive_blocks_this_gate_exists_for_really_emit() {
        assert_eq!(emission("minecraft:torch"), 14, "a torch emits 14, not 15");
        assert_eq!(emission("minecraft:glowstone"), 15);
        assert_eq!(emission("minecraft:air"), 0);
        assert_eq!(emission("minecraft:stone"), 0);
    }

    /// The occlusion half of the census, predicted the same way — from
    /// `lodestone-data/src/light_props.rs`'s own stated scale (air and glass `0`,
    /// water/ice/leaves `1`, a full solid `15`) rather than from this module.
    ///
    /// The two `1`s are the discriminating rows and the reason this test is not
    /// just "solids are 15": a predicate keyed on `dampening != 0` instead of on
    /// a *change* in dampening would treat leaves-for-water as an edit and
    /// water-for-leaves as none, and both answers would be wrong.
    #[test]
    fn the_occluding_blocks_this_gate_exists_for_really_dampen() {
        assert_eq!(dampening("minecraft:air"), 0, "air occludes nothing");
        assert_eq!(dampening("minecraft:glass"), 0, "glass casts no shadow");
        assert_eq!(dampening("minecraft:dirt"), 15, "a full solid occludes fully");
        assert_eq!(dampening("minecraft:oak_log"), 15);
        assert_eq!(dampening("minecraft:water"), 1);
        assert_eq!(dampening("minecraft:oak_leaves"), 1);
    }

    /// Both directions of an emissive edit fire, and so do both directions of an
    /// **occluding** one.
    ///
    /// The dirt and log cases are the whole reported bug in one predicate. They
    /// were `!should_relight(…)` here, asserted deliberately, on the argument
    /// that firing on an ordinary placement was too expensive — so the gate that
    /// should have caught "break a tree trunk and the hole is pitch black" was
    /// instead pinning it in place. That is worth leaving on the record: the test
    /// was not weak, it was *pointed the wrong way*, and no amount of reading it
    /// would have said so. Only the screen did.
    #[test]
    fn relight_fires_on_any_edit_that_moves_emission_or_occlusion() {
        assert!(
            should_relight("minecraft:air", "minecraft:torch"),
            "placing a torch must relight"
        );
        assert!(
            should_relight("minecraft:torch", "minecraft:air"),
            "breaking a torch must relight — a `emission(new) > 0` gate would miss this"
        );
        assert!(
            should_relight("minecraft:oak_log[axis=y]", "minecraft:air"),
            "breaking a tree trunk must relight: sky now reaches the cell"
        );
        assert!(
            should_relight("minecraft:dirt", "minecraft:air"),
            "breaking the dirt under it must relight for the same reason"
        );
        assert!(
            should_relight("minecraft:air", "minecraft:stone"),
            "and placing a solid must darken what is under it"
        );
    }

    /// The other side of the trade, and the reason the predicate compares two
    /// *values* rather than asking "did the state string change": an edit that
    /// moves neither quantity must still cost nothing, or every rotation and
    /// every cosmetic swap pays for a flood and a packet.
    ///
    /// Each pair here is chosen so the naive `old != new` string comparison would
    /// fire and this one must not.
    #[test]
    fn a_decorative_edit_is_not_a_relight() {
        for (old, new) in [
            ("minecraft:oak_log[axis=y]", "minecraft:oak_log[axis=x]"),
            ("minecraft:stone", "minecraft:dirt"),
            ("minecraft:torch", "minecraft:wall_torch[facing=north]"),
        ] {
            assert!(
                !should_relight(old, new),
                "{old} -> {new} moves neither emission nor dampening, so it must not relight"
            );
        }
    }

    /// A redstone torch's `lit` property really moves the emission, so a state
    /// the redstone model flips is a relight. This is the one family where the
    /// *same block name* has two different emissions, which is why the gate
    /// compares resolved values instead of names.
    #[test]
    fn lit_and_unlit_states_of_one_block_are_a_relight() {
        let lit = emission("minecraft:redstone_torch[lit=true]");
        let unlit = emission("minecraft:redstone_torch[lit=false]");
        assert_eq!(lit, 7, "lit redstone torch");
        assert_eq!(unlit, 0, "unlit redstone torch emits nothing");
        assert!(should_relight(
            "minecraft:redstone_torch[lit=false]",
            "minecraft:redstone_torch[lit=true]"
        ));
    }
}
