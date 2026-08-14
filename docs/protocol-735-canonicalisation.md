# Protocol 754 (1.16.5) block canonicalisation

## What it is

The retrofit that made `lodestone-v735`'s chunk decoder emit **canonical 26.2** block-state
ids instead of 1.16.5's own flat wire state ids — unit U4 of epic #343's dispatch plan
([`plans/multi-version-protocol.md`](./plans/multi-version-protocol.md)). Before it, every
1.16.5 world this client joined was meshed and collided as the wrong blocks, with a fully
green test suite: `v47` and `v340` had already received the equivalent fix (see
[`protocol-47-canonicalisation.md`](./protocol-47-canonicalisation.md) and
[`canonical-block-states.md`](./canonical-block-states.md)), and `v735` was the last live
instance of the defect.

## The defect, and why it differs from v47/v340's

`v47`/`v340` are pre-Flattening: their wire carries `(blockId << 4) | meta`, which is not a
block-state id at all. `v735` is **post**-Flattening — a `map_chunk` palette entry is
already a single flat block-state id, decoded correctly by
[`lodestone_world::PalettedContainer::decode`] — but it is **1.16.5's own** flat id space,
not 26.2's. Both games assign ids by global-palette registration order, and 26.2 has
inserted thousands of blocks since 1.16.5, so the same number now names a different block.
The old decoder stored that id straight into `lodestone-world` storage.

Measured (from `tests/canonicalisation.rs`'s discriminating-state test, anchored to the
real jar dumps described below — not predicted):

| 1.16.5 block | wire state id | read as, in 26.2, unmapped | should be (26.2 id) |
|---|---|---|---|
| `minecraft:diamond_block` | 3355 | `minecraft:warped_shelf` | `minecraft:diamond_block` (5309) |
| `minecraft:bedrock` | 33 | `minecraft:birch_sapling` (see `tests/chunk.rs`) | `minecraft:bedrock` (85) |
| `minecraft:grass_path` | 9227 | `minecraft:resin_brick_wall` | `minecraft:dirt_path` (14815) |

The bedrock row matters most in practice: it is the world's floor.

## How it works

Unlike the pre-Flattening bridge, 1.16.5 has no ambiguous "requires additional context"
cases — a flat state id already carries full block identity, nothing is deferred to a
TileEntity — so there is nothing left to resolve at *runtime*. The whole `1.16.5 state id ->
26.2 state id` mapping is baked into a flat generated array,
`lodestone_v735::generated_canonical::STATE_TO_CANONICAL`, at regeneration time.
`src/canonical.rs` is a thin wrapper: `resolve`/`resolve_or_air` are a plain array index,
and `air_state_id()` returns the baked `AIR_STATE_ID` constant rather than doing a runtime
`lodestone-data` lookup — this crate keeps zero runtime dependency on `lodestone-data`
(only a `[dev-dependencies]` one, for the table generator), so `cargo xtask check-deletable`
stays accurate.

`decode_sections` in `crates/protocol/v735/src/packets/chunk.rs` materialises each decoded
section's cells (`PalettedContainer::iter()`), translates every one through
`canonical::resolve_or_air`, and rebuilds the container via `PalettedContainer::from_values`
— per cell rather than per palette entry (matching `v47`'s tradeoff, not `v340`'s: there is
no separately-addressable palette to translate once through this crate's public API, and
`resolve_or_air` is a cheap array index either way). `ChunkShape::air_id` is
`canonical::air_state_id()` rather than the literal `0`, so section-emptiness is judged in
the canonical space cells now live in. An out-of-range wire id (`>= SOURCE_STATE_COUNT`,
which no real 1.16.5 server sends) becomes a counted air substitution on
`ChunkData::fallback`, logged once per column — the same visible-not-silent treatment
`v47`/`v340` give their own fallback cases.

## Data provenance

Two jar-derived sources, neither this crate's own encoder/decoder:

1. **1.16.5's own state table** — `tests/support/blocks_1_16_5_jar.json`, the unmodified
   output of Mojang's own data generator (`net.minecraft.data.Main --reports`) run against
   the real `.cache/mc/1.16.5/server.jar` under Apple `container` (see
   [`oracle-runtimes.md`](./oracle-runtimes.md)). Every state lists its own `id` and
   `properties` explicitly — no combinatorial re-derivation of vanilla's state-numbering
   algorithm is needed, unlike a naive read of `minStateId`/`maxStateId` ranges from
   community datasets.
2. **The 26.2 target space** — `lodestone_data::block_states`, itself derived the same way
   from `.cache/mc/26.2/generated/reports/blocks.json` (see
   [`canonical-block-states.md`](./canonical-block-states.md)'s sibling doc,
   `crates/lodestone-data/tests/block_states.rs`).

`tests/canonicalisation.rs` builds a `(name, properties) -> 26.2 id` reverse index from
source 2 (the same construction `lodestone_canonical::canonical::canonical_reverse_index`
uses) and resolves each of source 1's 17,112 states against it: a direct match, then a
three-entry rename table (`grass`->`short_grass`, `grass_path`->`dirt_path`,
`chain`->`iron_chain`; the first two are shared with `v47`/`v340`'s own rename table), then
two generic single-property fallbacks — `waterlogged=false` (leaves, all four rail blocks,
barrier all gained `waterlogged` after 1.16.5) and `powered=false` (every mob-head/skull
block gained a redstone-signal `powered` property after 1.16.5) — plus a hand-written
cauldron identity split (`level=0` -> bare `cauldron`, `level>0` -> `water_cauldron`,
mirroring `lodestone_canonical::canonical`'s pre-Flattening cauldron arm exactly). Every
fallback default was confirmed against the decompiled 26.2 source
(`LeavesBlock`/`BaseRailBlock`/`BarrierBlock`/`AbstractSkullBlock` all
`registerDefaultState(...PROPERTY, false)`), not guessed. Across the full 17,112-state
corpus this leaves **zero** unmapped states — the generator panics naming the offending
state if a future jar update ever reintroduces one, rather than silently defaulting it to
air.

## The gate, and what makes it non-vacuous

`crates/protocol/v735/tests/canonicalisation.rs`:

- `committed_table_matches_dump` (`#[ignore]`d, heavy — builds the full 32,366-entry 26.2
  reverse index): the drift guard. Regenerate with `LODESTONE_REGEN=1 cargo test -p
  lodestone-v735 --test canonicalisation committed_table_matches_dump -- --ignored
  --nocapture` after either source changes.
- `committed_table_is_internally_consistent` (always runs): every baked id is a valid 26.2
  state, and the baked air id really is `minecraft:air`.
- `discriminating_states_resolve_to_their_26_2_ids_not_their_wire_ids` (always runs): five
  hand-picked states whose 1.16.5 wire id and 26.2 id name different registry slots —
  exercising the rename table, both generic fallbacks, and the cauldron split — each with a
  **negative control** asserting what the *unfixed* direct-index path would have named
  instead (a different, unrelated block for every case, not a near-miss), so the pairs
  cannot be the coincidence class where wire id and canonical id happen to agree.

`crates/protocol/v735/tests/chunk.rs`'s existing hermetic decode tests were also updated:
they build wire fixtures with 1.16.5's own ids (`BEDROCK_WIRE = 33`, `STONE_WIRE = 1`,
`AIR_WIRE = 0`) but now assert decoded output against the canonical 26.2 ids
(`BEDROCK_CANONICAL = 85`, `STONE_CANONICAL = 1`, `AIR_CANONICAL = 0`) — before this
retrofit they asserted the wire ids came back unchanged, which is exactly the defect this
unit fixes, so those two assertions are expected to have flipped.

## How to change it, and the gotchas

- **`cargo xtask connectedness` cannot see this class of bug**, for the same reason
  `protocol-47-canonicalisation.md` documents for `v47`: it answers "is this clientbound
  packet reaching anything", and U4 changed *what flows through* an already-connected wire.
  A green connectedness run is not evidence a decoded block id is right.
- **`src/generated/canonical.rs` is generated. Never hand-edit it.** Regenerate per the
  provenance section above; `tests/canonicalisation.rs`'s module docs carry the exact
  `container run` invocation for re-running the data generator if the 1.16.5 jar itself
  ever changes (it will not — 1.16.5 is a frozen historical release — so in practice only a
  26.2 registry regeneration should trigger this).
- **Adding a rename or property fallback is hand-written work and needs its own
  justification**, checked against the decompiled 26.2 source the way the two generic
  fallbacks above were, not merely "the registry has a plausible-shaped entry".
- **This crate carries zero runtime dependency on `lodestone-data`.** `air_state_id()` reads
  a baked constant, not a live lookup — do not "fix" this by adding `lodestone-data` to
  `[dependencies]`; that would be pure regression against the doc comment in `Cargo.toml`
  explaining why it is dev-only.

## Configuration

`LODESTONE_REGEN=1` switches the generator from assert to write. Nothing else.

## Dependencies

`lodestone-data` (dev-only, for the table generator). Consumed today by `lodestone-v735`
alone — this is a **per-family** table, unlike `lodestone-canonical`'s shared pre-Flattening
one, because each post-1.13 family speaks its own flat state-id space (see
`plans/multi-version-protocol.md`'s "Post-1.13 families" note); there is nothing for a
second family to share it with yet.
