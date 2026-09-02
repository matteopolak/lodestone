# Server-side chunk column storage and wire encoding

## What it is

How a server-side `ChunkColumn` holds its block-state data in memory, how those states reach a
real client on the wire as the `level_chunk_with_light` packet body, and how the one heightmap this
server currently computes (`MOTION_BLOCKING`) is derived and sent alongside them.

## How it works

### In-memory storage: a column-wide palette over per-section packed cells

A column keeps one column-wide palette of block-state strings; each 16-row section stores its
cells as either a single repeated value (an all-one-block section, allocating nothing) or a
bit-packed array sized to the widest palette id that section actually uses, widening only when a
write needs more bits than the section currently has and never narrowing back down afterward
(narrowing is bookkeeping for a case that doesn't recur — a column is built once and edited a
handful of times, not repacked on every edit). This is deliberately simpler than the client's own
per-section paletted container (which keeps a *local* palette per section, not a shared
column-wide one) — the column-wide palette is already small enough in practice that the remaining
gap is single-KiB per section, and a per-section local palette would add a remap-and-rewrite step
on every palette growth that must never have a bug, since a wrong remap silently serves the wrong
block instead of failing loudly. See `docs/architecture.md`'s "World storage and memory" section
for the shared paletted-container thresholds, bit-packing rules, and index order this crate's
client-facing container follows; this section is specifically about the server's own simpler,
column-scoped representation.

Loading a saved column reconstructs this representation directly from the region file's own
per-section local palette and packed indices, rather than replaying one block-set call per cell —
replaying per-cell would mean re-resolving each of a column's ~98,000 cells against the whole
column-wide palette one string comparison at a time, when the region file already hands over
almost all of that structure pre-computed.

### Wire encoding: real per-cell state, resolved as integers, not strings

The server's terrain data is real block variety end to end (grass, dirt, ores, water, whatever the
generator produces); the wire encoder resolves each cell's real global state id directly from the
column's own pre-resolved integer palette rather than re-resolving a block-state string per cell.
That integer resolution happens once per distinct palette entry (a few dozen times per column), not
once per cell (tens of thousands of times) — resolving a state string per block is both far more
expensive and, in an earlier version of this encoder, was silently skipped altogether in favor of
collapsing every solid block to one hardcoded stand-in and everything else (including every fluid)
to air. Fixing that collapse alone was not sufficient: a fluid's *bare* block name (with no
explicit properties) has no valid state by itself, since every real fluid state carries a property,
so the string-resolution fallback needs a third tier beyond "exact match" and "give up as air" — a
same-name default state, matched against the game's own jar-marked default rather than assumed to
be the lowest-numbered id for that block (the two disagree for most multi-state blocks, and only
coincidentally agree for water and lava specifically).

Per-section block/fluid-count wire fields are derived from the same real ids the container holds,
which happens to also correct a second, narrower undercount a real client would otherwise trust
verbatim for any fluid-bearing section (a real client stores what the wire tells it and never
recomputes the count itself). Heightmap and light data sent alongside a column are separate,
independently-tracked gaps from the block-state fix described here.

### The `MOTION_BLOCKING` heightmap

The generator computes a real per-column `MOTION_BLOCKING` heightmap (the height of the first block
from the top that blocks motion or carries a fluid) as the final step of generation, and the server
carries it across to the encoder rather than sending an empty, well-framed-but-zero-entry heightmap
as it used to. A column with no such block anywhere reports height zero (the world's own minimum),
which is deliberately different from "no heightmap at all" — an absent heightmap tells a real client
nothing and it computes its own, while a wrong one sent as real data is trusted outright, so sending
a knowingly-wrong map is worse than sending none. For that reason a column with no generator-derived
facts available (a freshly constructed, non-generated, or loaded-from-disk column) still sends the
empty case rather than a guessed one.

This is a **snapshot taken once, at generation time**, not a maintained live value — a later block
edit does not update it, matching this crate's choice not to persist heightmaps at all and to rely
on a real client re-deriving them after any edit, the same way vanilla re-derives them on load. The
other three heightmap kinds vanilla also sends are not computed here at all; sending one of them
today would mean sending a knowingly wrong map (in particular, the "exclude leaves" variant has no
real exclusion logic behind it yet), which is exactly the failure mode the `MOTION_BLOCKING` fix
was careful to avoid.

## How to change it

- **Never resolve a block-state string per cell, in the encoder or anywhere else on a hot path.**
  Route through the column's own pre-resolved integer palette instead; a local hash-map memo over
  strings does not recover the cost, since hashing the strings themselves is a measurable fraction
  of the total.
- **A same-name fallback resolution is not safe to extend casually to a new property-requiring
  block without checking that block's own jar-marked default state first** — "lowest id" and
  "actual default" disagree for most multi-state blocks, and only happen to agree for the two
  fluids this fallback currently needs to handle.
- **Adding a second sent heightmap kind**: read its registry id and its "blocks motion" predicate
  off that heightmap kind's own real definition, not by inferring one from a neighboring kind's
  behavior or an ordinal position — the "excludes leaves" variant in particular is not simply the
  general one with a filter, and sending a plausible-looking but wrong predicate is worse than not
  sending the map at all.
- **A promotion from a single-value section to a packed one must fill the new backing array with
  the original value before applying the write that triggered it** — skipping that step silently
  turns every other cell in that section into the wrong block with no panic and no visible symptom
  until someone notices the terrain looks wrong.

## Configuration

None. Section widths, palette thresholds, and the heightmap predicate are all derived from the
chunk format and the game's own data, not independently tunable constants.

## Dependencies

- The shared world-storage crate's paletted container and section types for the client-facing
  representation and its already-generic bit-packing strategies (unmodified by anything described
  here — the fix was entirely in what the encoder fed that container, not in the container itself).
- The generated block-state table (`lodestone-data`) for the string-to-id resolution the encoder's
  fallback tiers use.
- The world-generation crate for the real `MOTION_BLOCKING` computation, which the server only
  carries across a seam and the encoder only serializes.
- `docs/architecture.md` for the general paletted-container thresholds, index order, and the
  version-boundary long-array framing rule shared across this whole area.
