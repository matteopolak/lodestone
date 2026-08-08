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
//! | link | state before this module |
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
//! # How the fix here works, and why it is a column resend
//!
//! [`crate::server`] answers an emission-changing edit by re-sending that whole
//! column through the `begin_chunk_batch`/`encode_chunk`/`end_chunk_batch`
//! sequence it already uses when a player walks into a new column. `encode_chunk`
//! recomputes light from the column it is handed — which now contains the torch —
//! so the light arrives correct with **no new encoder and no new trait method**.
//! The client's `level_chunk_with_light` arm replaces the chunk and emits
//! `ChunkLoaded`, the same re-mesh signal the `LIGHT_UPDATE` arm emits.
//!
//! That is a deliberately blunt instrument and the cost is honest: a full column
//! (a few tens of KiB) per emissive edit, rather than the 2 KiB nibble array a
//! `LIGHT_UPDATE` would carry. It is affordable **because [`should_relight`] is
//! narrow** — it fires only when emission actually changes, which is torches,
//! lanterns, glowstone, sea lanterns, campfires, a furnace lighting, and little
//! else. It does not fire when you mine a wall and let daylight in; see the gaps
//! below.
//!
//! # The named gaps, and the patch that closes them
//!
//! **1. Sky light does not follow an edit.** [`should_relight`] compares
//! *emission* only, not dampening, so breaking a roof does not re-send the
//! column's sky light. Widening it to `dampening` as well would fire on nearly
//! every placement and turn every block placed into a column resend, which is
//! not affordable at this granularity. The right fix is the `LIGHT_UPDATE`
//! encoder, not a wider predicate here.
//!
//! **2. Light does not cross a chunk border**, so a torch at local `x = 15` does
//! not light its eastern neighbour at all. This is *not* a gap in this module: it
//! is `compute_served_light` running the **isolated** compute, and it is the same
//! open item `docs/server-chunk-light.md` records as a measured **Δ5** sky-light
//! dark bias at column borders. Its fix needs the brokered
//! `crates/protocol/v770/src/server_protocol.rs` change that plan's step 4
//! describes — plus one trap that plan does not mention, recorded here because
//! it would make the fix look like it worked and serve stale light:
//!
//! > If `ChunkColumn` carries a precomputed `light`, then
//! > `ChunkColumn::set_block` and `ChunkStore::set_block` **must invalidate it**.
//! > Both write blocks into a retained column without touching anything derived
//! > from them. A `column.light()` that survives an edit would make the resend
//! > above serve the light the column had *before* the torch was placed — a
//! > correct-looking wire, a re-meshed client, and no change on screen.
//!
//! **3. Only the acting connection is told.** The resend rides that
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
