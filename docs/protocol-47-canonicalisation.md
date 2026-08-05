# Protocol 47 (1.8.9) block canonicalisation

## What it is

The retrofit that made `lodestone-v47`'s chunk decoder emit **canonical 26.2** block-state
ids instead of 1.8's raw `(blockId << 4) | meta` wire composite — unit U3 of epic #343's
dispatch plan ([`plans/multi-version-protocol.md`](./plans/multi-version-protocol.md)).
Before it, every 1.8.9 world this client joined was meshed and collided as the wrong
blocks, with a fully green test suite.

## The defect, and why nothing caught it

1.8's `map_chunk` has **no palette on the wire**: each of a section's 4096 cells is a 16-bit
little-endian composite `(blockId << 4) | meta`. The old decoder stored that composite
straight into `lodestone-world`'s `PalettedContainer`, and the module header stated the
composite "*is* the natural block-state id".

Three things made this invisible:

- `PalettedContainer` is **version-free** and accepts any `u32`, so no type or assertion in
  `lodestone-v47` could disagree.
- The crate's own tests were a **closed loop**: they built golden blobs from composites and
  asserted the same composites came back out. A layout test and a mapping test look
  identical when both sides use one id space.
- The real consumers — the mesher's atlas and collision — live in *other* crates and are
  built from the canonical 26.2 space. Nothing in the seam announces the mismatch; the
  blocks simply come out as different blocks.

Measured, by reverting the fix and running the gate (the numbers are the gate's own failure
output, not a prediction):

| 1.8 block | wire composite | read as, in 26.2 | should be |
|---|---|---|---|
| stone `1:0` | 16 | `minecraft:spruce_planks` | `minecraft:stone` (1) |
| bedrock `7:0` | 112 | `minecraft:lava` | `minecraft:bedrock` (85) |

The bedrock row is the one worth remembering: the bottom layer of every 1.8.9 world was
**lava**.

## How it works

`decode_column` in `crates/protocol/v47/src/packets/chunk.rs` now passes every cell through
[`lodestone-canonical`](./canonical-block-states.md)'s
`canonical::resolve_composite_or_air`, and `ChunkShape::air_id` is
`canonical::air_state_id()` rather than the literal `0`, so section-emptiness is judged in
the same space the cells now live in.

Unresolvable values become a **counted** air substitution on `ChunkData::fallback`
(a `FallbackTally`), logged once per column by `report_fallback` with the per-outcome
breakdown — the same treatment `v340` gives them, so the two pre-Flattening families now
report identically.

Two deliberate differences from `v340` are worth knowing before you change either:

- **Resolution is per cell, not per palette entry.** v340 translates a small palette once;
  1.8 has no palette, so there is nothing to translate once. This is not a hot path
  problem (`resolve` is an index into a lazily-built 4096-entry array) and it is what makes
  the tally count *blocks* substituted, which is what its log line claims.
- **An out-of-range composite is a fallback here and a hard error there.** A 16-bit wire
  value can name a block id past 255. In v47 that costs one cell; in v340 the same value
  arrived in a *palette*, so a bad entry means the index stream is suspect and the packet
  is rejected. The shared 12-bit rule is `canonical::split_composite`; the **policy** on
  `None` is each family's own, and v340's `legacy_id_meta` is now just that policy.

## The gate, and what makes it non-vacuous

`crates/protocol/v47/tests/canonicalisation.rs`, two arms. Both predict values; neither
takes an expected value from code under test.

**Arm 1 — adversarial pairs.** Nine `(id, meta)` pairs, each anchored three ways:

1. the 1.13-era name and properties are re-read at test time from the committed **dump
   text** of the real 1.13.2 jar's `DataFixerUpper`
   (`lodestone-canonical/tests/support/flattening_1_13_2_jvm.txt`) — deliberately the dump
   text and **not** `lodestone-canonical`'s generated Rust table, which is downstream of
   the thing being trusted;
2. the expected 26.2 state id is predicted by searching `lodestone-data`'s jar census for
   that name and properties, and **requiring exactly one match** — so the expectation is a
   value, not a range;
3. the **naive** composite (precisely what the crate used to store) is asserted to name a
   *different* 26.2 block, for all nine.

**Arm 2 — real server output.** A section lifted out of the vanilla 1.8.9 server's own
world save, committed as `tests/support/real_1_8_9_section_save.txt` with the source region
file's SHA-256 in its header. Anvil 1.8 stores the same `(id, meta)` pair the wire sends in
the same YZX order, so real server bytes replay through the real decoder with **no server
running** — which matters because Docker and a JVM are not available in every session.

**Why two arms.** That world is `level-type=FLAT`: it holds four distinct pairs and every
one is `meta = 0`, so on its own it structurally cannot exercise the `meta` half of the
composite. That is the *world* species of vacuous test — the flaw is in the input data, so
no amount of reading the test reveals it. Five adversarial pairs carry a non-zero meta, and
a third test (`the_real_save_fixture_cannot_exercise_meta_which_is_why_the_other_arm_exists`)
fails if that premise ever stops holding, so the reason is checkable rather than a comment.

**Controls, observed:**

| control | observed failure |
|---|---|
| restore the raw-composite store | `1.8 1:0 must decode to the 26.2 state the 1.13.2 jar dump names (minecraft:stone, []); got 16 (minecraft:spruce_planks) instead of 1 (minecraft:stone)`, and `real save cell (0,0,0) = 7:0 (minecraft:bedrock) decoded as 112 (minecraft:lava)` |
| read the **neighbouring** dump slot (`n + 1`) | `1.8 1:0 must decode to the 26.2 state the 1.13.2 jar dump names (minecraft:granite, []); got 1 (minecraft:stone) instead of 2 (minecraft:granite)` — proving the expectation comes from the dump text, not from our decoder |

## How to change it, and the gotchas

- **`cargo xtask connectedness` is silent on this whole class of bug.** It reported
  byte-identical output before and after (`v47 clientbound decoded 21/74; emits 21/74`),
  correctly: U3 changed *what flows through* an already-connected wire, which is the
  instrument's documented blind spot. Do not read a green connectedness run as evidence a
  decoded value is right.
- **Regenerating the real-save fixture needs a richer world to be worth it.** If you
  regenerate from a world containing non-zero metas, delete the premise test rather than
  weakening arm 1 — arm 1 is what covers meta, and it should keep covering it.
- **`tests/chunk.rs` separates the two id spaces on purpose.** It builds blobs from
  `WIRE_BEDROCK` (the composite) and asserts `bedrock()` (looked up **by name** in the 26.2
  census). Do not collapse those back into one constant; that is what let the original
  defect hide.
- **No `cargo check` at any feature setting can see a stale expected value in
  `tests/live_chunk.rs`** — it is behind the `live-chunk` feature *and* `#[ignore]`d. It
  asserted the literal `112` for bedrock; it now asserts by name, and prints layer names
  rather than bare ids, because a bare number in a report is what let the wrong space go
  unnoticed.

## Configuration

None of its own. The `live-chunk` feature plus `--ignored` runs the live 1.8.9 join gate;
the canonicalisation gate is always-run and needs no server.

## Dependencies

`lodestone-canonical` (the shared table and bridge), `lodestone-world` (the palette the
values land in), and `lodestone-data` as a **dev**-dependency only, for the 26.2 census the
gate anchors on. Adding `lodestone-canonical` does not affect deletability: `cargo xtask
check-deletable v47` still reports the folder plus its registry dependency and feature line,
and names `lodestone-canonical` nowhere.
