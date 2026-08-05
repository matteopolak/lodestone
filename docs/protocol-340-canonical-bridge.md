# Wiring v340's flattening table into the adapter (`crates/lodestone-canonical/src/canonical.rs`)

## What it is

The follow-up to [`protocol-340-flattening-table.md`](./protocol-340-flattening-table.md):
that document built and verified the `id:meta` → modern-block table but explicitly did not
wire it up (its own "What wiring `v340` would need" section). This closes that gap.
`crates/lodestone-canonical/src/canonical.rs` bridges [`flattening::lookup`]'s answer — vanilla's own
first flattening step, in its own intermediate spelling — to a real, canonical **26.2**
[`lodestone_data::block_states`] id, and `crates/protocol/v340/src/packets/chunk.rs`'s
`map_chunk` decode now calls it for every block a 1.12.2 server sends. A 1.12.2 chunk decodes
into the same id space `lodestone-world`'s storage, the mesher, and collision already assume.

## How it works

### Two separate bridging problems

`flattening::lookup(old_block_id, meta)` already solves "which modern block is this pair" —
but "modern" there means vanilla's own 1.13.2-era intermediate spelling, not 26.2's current
registry. Two gaps had to be closed, independently:

1. **Renames.** A handful of names are stale even relative to 1.13.2's own final registry
   (`mob_spawner`→`spawner`, `melon_block`→`melon`, `portal`→`nether_portal`, `*_bark`→`*_wood`
   — jar-confirmed disagreements the flattening-table doc already recorded) and a few more are
   stale relative to *later* 26.2 renames the original task never chased (`sign`→`oak_sign`,
   `wall_sign`→`oak_wall_sign`, `grass`→`short_grass` at 1.20.3, `grass_path`→`dirt_path` at
   1.17). The second group was **not** traced through the 1.14+/26.2 jar's own rename fixers —
   that remains real, unstarted follow-up work. Each was instead verified by confirming the
   target name exists in the 26.2 registry with the exact state shape (property count/kind) the
   legacy meta range implies — stronger than a guess (a wrong name would not match a real entry
   with a plausible shape), but a different, weaker standard of evidence than the original
   table's jar-tracing. `canonical.rs`'s module docs say this explicitly at the point of use.
2. **New/changed properties.** 26.2 added properties pre-1.13 storage cannot carry at all
   (`waterlogged` on ~255 of the mismatched slots — safe to default `false` unconditionally,
   since pre-1.13 has no waterlogging concept, not because "false" is a good guess for an
   unknown), or repurposed a property's meaning (leaves' `decayable`/`check_decay` →
   `persistent`/`distance` — `persistent` is the exact logical inverse of `decayable`, a real
   derived value; `distance` has no legacy equivalent at all and is a documented placeholder,
   `7`, safe only because nothing in this project currently runs decay logic client-side and
   `distance` does not affect a leaf block's mesh or collision).

Two families also needed an **identity change driven by a property's value**, not just a name
swap: cauldron (`level=0` stays `cauldron`; `level=1..3` splits to `water_cauldron` with a
`level` property) and the two 1.12.2 walls (booleans bridge to the post-1.16 `none`/`low`/`tall`
enum — `true`→`low`, since a legacy connection has no way to express "tall").

### The four-outcome shape survives, plus one

[`canonical::CanonicalBlockState`] mirrors [`flattening::LegacyBlockState`]'s four variants —
`Resolved`, `NoTableEntry`, `RequiresAdditionalContext`, `OutOfBounds` — and adds a fifth,
`Unmapped`, for a resolved name/properties that even this bridging pass cannot match to a
canonical state. `Unmapped` is empirically **never produced** for the table as it exists today
(`canonical::tests::no_slot_is_unmapped` checks all 4095 slots exhaustively and asserts zero); it
exists as a drift guard for the day a flattening-table regeneration (a jar update) introduces a
name/shape this pass's bridging does not cover, so that case fails loudly in a test rather than
silently producing a wrong id.

### What the adapter does with each outcome

Per CLAUDE.md: "if you choose air, it must be a visible, logged, counted fallback." All four
non-`Resolved` outcomes get the **same** substitution — [`canonical::air_state_id`] — at
`packets/chunk.rs`'s single integration point, [`canonical::resolve_or_air`]:

- `NoTableEntry` (2400 slots) — matches vanilla's own runtime behaviour (vanilla substitutes air
  too), just made explicit and counted rather than silent.
- `RequiresAdditionalContext` (32 slots: flower pots, skulls, double-plant upper halves) — this
  crate does not decode block entities at all (`packets/chunk.rs` consumes and discards them, to
  keep the zero-trailing-bytes detector meaningful — see that module's docs), so there is no
  TileEntity data available to resolve these even if flagged. Air is the only option without
  extending the block-entity decode, which is out of this task's scope.
- `OutOfBounds` (1 slot) — no real client ever sends this pair; air is a formality.
- `Unmapped` — never occurs today (see above); air is the fallback if it ever does, same as the
  others, rather than panicking mid-decode over one unrecognised block.

Every substitution increments a [`canonical::FallbackTally`] threaded through the whole column
decode. `packets/chunk.rs`'s `MapChunk::decode` logs one `tracing::warn!` (`target: "v340::chunk"`)
per column **only if the tally is non-empty**, with the per-category counts — silent for the
overwhelming majority of real columns (a live 1.12.2 server's own generated terrain never places
`NoTableEntry`/`RequiresAdditionalContext` ids), loud and traceable the moment one appears.

### Where the id space itself changes

`ChunkShape::air_id` used to be the legacy composite `0` (block id 0, meta 0). It is now
[`canonical::air_state_id()`] — looked up from the registry, not hardcoded — because
`ChunkSection`'s `non_air_count`/`is_empty` compare directly against `air_id`, and every block
reaching a `PalettedContainer` is canonical from this point on.

## How to change it

- **Rename table**: `canonical::bridge_name`. Adding an entry is safe and mechanical; the
  exhaustive test (`canonical::tests::no_slot_is_unmapped`) will tell you immediately if a
  regenerated flattening table needs a new one (it asserts `resolved == 1663` etc. too, so a
  *shift* in outcome counts — not just new `Unmapped`s — also fails loudly).
- **Property fixups**: `canonical::bridge_properties` and the two identity-splitting cases inside
  `canonical::bridge` (cauldron, walls). Each is deliberately narrow (matched by exact block
  name), per this project's existing convention of hardcoding enumerated ambiguous cases rather
  than a general heuristic — see the flattening-table doc's own note about the generator doing
  the same for flower pots/double-plants.
- **Regenerating the flattening table** (a jar update) can change which slots are `Resolved` vs.
  not, which can change what this module needs to bridge. Re-run
  `cargo test -p lodestone-v340 --lib canonical` first; a failure in `no_slot_is_unmapped` names
  the exact `(old_block_id, meta, name)` triples that need a new rename or property fixup.
- **The chunk decode integration**: `packets/chunk.rs::decode_section_blocks` translates the
  (typically small) **palette**, not each of a section's 4096 cells, when a palette is present;
  the direct/global-palette case (large `bits_per_block`, no palette) translates each cell
  individually since there is no shared palette to translate once. Both paths go through the same
  `canonical::resolve_or_air`.

## Configuration

No environment variables or flags beyond what `docs/protocol-340-flattening-table.md` already
documents for regenerating the underlying table. `scripts/live-oracles/legacy-1.12.sh` (new, see
"Dependencies") accepts no flags either — it hardcodes its ports to avoid colliding with the 26.2
oracles.

## Dependencies

- `lodestone-data` (new dependency of `lodestone-v340`) for the canonical `block_states` table.
  This does **not** violate the version-crate isolation `lib.rs` describes ("depends only on
  `lodestone-core`/`lodestone-model`/`lodestone-macros`") in spirit: `lodestone-data`'s own module
  docs already carve out exactly this case — "Older protocol crates (`v47`, `v340`, `v735`) keep
  their own version-specific translation tables ... because that data is genuinely about
  translating an old wire format into this canonical space" — citing this flattening table by
  name before this wiring even existed.
- `tracing` (new dependency), matching `lodestone-v770`'s existing convention for the
  fallback-tally logging.
- `scripts/live-oracles/legacy-1.12.sh` (new) — a real vanilla 1.12.2 server oracle, following the
  `creative.sh`/`terrain.sh` pattern but for protocol 340: game port `25568` (matches
  `tests/live_chunk.rs`'s existing `LODESTONE_V340_PORT` default, which predates this file), RCON
  `25569`. Uses `.cache/mc/1.12.2/server.jar` (already fetched per the project's data-source
  policy) under `eclipse-temurin:8-jdk`. Unlike `creative.sh`, it patches `server.properties`
  in place (port + RCON) on every start, since the cached instance was fetched with RCON disabled
  and the default port.
- `crates/protocol/v340/tests/live_canonical.rs` (new, `live-chunk`-feature-gated, `#[ignore]`d)
  — places a representative slice of the bridged families (plain resolve, bark→wood rename +
  axis default, leaves persistent/distance, note block, trapdoor, both cauldron-split branches,
  both walls) on a real server via RCON `/setblock` using **1.12.2's own legacy block names**
  (its command parser rejects a bare numeric id — confirmed directly, not assumed), confirms
  placement with `/testforblock` before trusting it, then asserts the *decoded* canonical
  `(name, properties)` against expectations derived independently of `crate::canonical` (the
  legacy `id`/`meta` handed to the server, or mechanics stated in prose — never by calling
  `canonical::resolve` and comparing it to itself). All 9 passed. This is the strongest evidence
  in this pass; `tests/live_chunk.rs`'s existing bedrock-floor check (updated to assert the
  canonical id, not the legacy composite `112`) additionally confirms the general decode path
  end-to-end across 81 real chunks with zero decode errors.
