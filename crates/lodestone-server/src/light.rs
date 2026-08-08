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
//! # The named gaps, and the patch that closes them
//!
//! **Both survived the encoder, and that is worth stating plainly**: the encoder
//! was the blocker named for each of them, and neither fell out of building it,
//! because neither is a *transport* problem. `light_update` is a cheaper carrier
//! for the same values, not a better computation.
//!
//! **1. Sky light does not follow an edit.** [`should_relight`] compares
//! *emission* only, not dampening, so breaking a roof does not re-send the
//! column's sky light. Widening it to `dampening` as well fires on nearly every
//! placement — and even at `light_update`'s size that is a full
//! `compute_column_light` (a `build_world_column` state-id resolution plus the
//! flood) per block placed, on the connection task. So the encoder made this
//! *cheaper* without making it affordable. What it needs is **incremental**
//! re-propagation from the changed cell, which vanilla has and this crate does
//! not.
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

/// `true` iff replacing `old` with `new` changes how much light the cell emits —
/// the gate on the column resend described in this module's own doc comment.
///
/// Compared on the **value**, not on the state string: `minecraft:torch` and
/// `minecraft:wall_torch[facing=north]` both emit 14, so re-orienting a torch is
/// not a relight, while lighting a furnace (`lit=false` → `lit=true`, `0` → `13`)
/// is. That is also why this cannot be `emission(new) > 0`: **removing** a light
/// source has to relight too, and a break writes `minecraft:air`.
#[must_use]
pub fn should_relight(old: &str, new: &str) -> bool {
    emission(old) != emission(new)
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

    /// Both directions of an emissive edit fire, and an ordinary one does not.
    ///
    /// The last case is the load-bearing one: if `should_relight` fired on
    /// stone-for-air it would resend a column on every block a player places,
    /// which is the reason this is an emission comparison rather than a
    /// "did anything change" test.
    #[test]
    fn relight_fires_on_placing_and_removing_a_light_source_only() {
        assert!(
            should_relight("minecraft:air", "minecraft:torch"),
            "placing a torch must relight"
        );
        assert!(
            should_relight("minecraft:torch", "minecraft:air"),
            "breaking a torch must relight — a `emission(new) > 0` gate would miss this"
        );
        assert!(
            !should_relight("minecraft:air", "minecraft:stone"),
            "an ordinary placement must NOT resend a column"
        );
        assert!(
            !should_relight("minecraft:stone", "minecraft:air"),
            "an ordinary break must NOT resend a column"
        );
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
