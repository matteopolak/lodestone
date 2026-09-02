# World persistence: saving, loading, and the world's own state

## What it is

Everything that makes a Lodestone world survive quitting and reopening: the on-disk container
formats (Anvil region files, `level.dat`, and the world-generation-settings file that actually
holds the seed), the server-side wiring that intercepts the chunk pipeline to load from and save
back to those files, the world's own persisted scalars (game rules, difficulty, the clock), where a
fresh or respawning player appears, and the separate point-of-interest index vanilla keeps for
things like beds, workstations, and lit nether portals.

## How it works

### Two layers: the container format and the chunk schema

`lodestone-anvil` is a version-free, dependency-light crate that knows the **container** formats —
the Anvil region file envelope (an 8 KiB header of sector locations and timestamps, followed by
compressed, sector-addressed chunk payloads, with very large chunks spilling into a sibling file),
the gzip-wrapped named-NBT `level.dat` envelope, and the separate file 26.2 actually stores the
world seed in (`level.dat` itself carries no seed field in this version, unlike older releases —
the seed instead lives in `<world>/data/minecraft/world_gen_settings.dat`, which is why "the seed
isn't persisted" is a trap easy to fall into by checking the wrong file). It deliberately parses no
chunk *schema* — what NBT tree a chunk actually contains is `lodestone-server`'s problem, kept
separate so the same container code serves region files, entity-region files, and the
point-of-interest region files all as one instance each, and so a browser build that has no
filesystem can still depend on `lodestone-server` without dragging in the disk-based half of it
(`lodestone-anvil` is a non-wasm-target dependency for exactly this reason).

`lodestone-server`'s own chunk-NBT schema module owns the mapping between an in-memory
`ChunkColumn` and the actual chunk NBT tree; the persistence layer sits below the chunk cache and
above the terrain generator in the chunk-source stack, so that a cache eviction never loses an
edit (the persistence layer is the one that retains edits permanently) and a column loaded from
disk always wins over a freshly generated one. A block-set call on this layer deliberately does
**not** forward down to the generator's own edit-tracking, because the generator's edit map is
seeded by generating the column fresh — forwarding would silently regenerate and discard a
disk-loaded edit with no error. Every mutation in the server funnels through this one call, so
hooking persistence in cost no changes to the tick loop or the mob simulation.

A save writes only the dirty set, not everything resident — a player standing still should not
cost megabytes of disk writes every autosave interval — and untouched chunks inside a rewritten
region file are re-emitted as their original compressed bytes rather than being decoded and
re-encoded, since the region-file format has no incremental single-chunk update and always rewrites
a whole file in one pass. Neither generation nor saving is allowed to run on the world's own tick
thread; both are pushed onto a blocking thread pool, since a synchronous disk write there is
exactly the same class of stall as a synchronous chunk generation would be.

### The world's own scalars: game rules, difficulty, and the clock

A single shared, persistable store holds the world's scalar state — game rules, difficulty, and
the world clock — reached by a cloneable handle threaded to the connection loop, the tick loop, and
whatever persists it. The organizing rule for all three is the same: a value that is merely stored
and broadcast, with no real reader at its actual decision point, is exactly as absent as if it had
never been implemented at all, and every accessor here is expected to have a named production
reader (game rules gating the natural-tick/drop/spawn/mob-griefing paths that vanilla gates the
same way; difficulty gating peaceful-mob eviction and spawning, the starvation floor, and fire
spread odds; the clock's own two-part rule that game time always advances while the *displayed*
day/night time advances only when the corresponding rule allows it, matching vanilla's own
unconditional-versus-gated tick split). Persisting these scalars reuses vanilla's own `level.dat`
field names, so a world this server writes stays readable by a real client, and — a genuine
vanilla-format gotcha — every game rule is stored as a **string** in that file regardless of its
real type, because that is how vanilla's own codec represents them.

### World spawn

A fresh player's spawn point is found the same way vanilla finds one for a brand-new world: a fixed
spiral search outward from the origin, testing each candidate column from the top down for a
standable surface. The standability test matters more than it looks: testing "is this a solid
block" is the wrong question, since real generated surface cover (short grass, flowers, snow
layers) has no collision at all and would wrongly fail that test, while some things that *do* look
walkable are correctly treated as solid because vanilla itself would let a player stand there
(including a treetop, which is a genuine vanilla spawn outcome, not a bug to route around). A world
whose spawn search area is entirely unsuitable (for instance, entirely ocean) falls back to a fixed
height a couple of blocks above sea level rather than to a hardcoded low value that would place a
player underground or inside bedrock. A per-player bed respawn point is stored and consulted
separately, falling back to the world spawn whenever the recorded bed is gone.

### Point-of-interest storage

Vanilla keeps a third, independent region-file set — `poi/`, per dimension — indexing things like
workstations, beds, bells, and lit nether portals, each with a maximum simultaneous "claim" count
and a live occupancy state; a claim-count field that is *absent* on disk means "no claims remain",
not "never claimed," which is easy to misread as the opposite. Each point of interest is a fixed
block position, so unlike a moving entity, saving one only ever needs the caller's complete state
for a given chunk, never a separate "clear the old copy out" pass. The only current real consumer
is the nether-portal index, whose own persistence closes a real, previously-reported gap: without
it, a portal lit in an earlier session vanished from a fresh in-memory index on restart, and a
distant return trip built a duplicate portal instead of reusing the original. Restoring this index
has to scan every point-of-interest file in the world, not just a radius around spawn or around the
currently-loaded area, because a portal can exist anywhere the player has ever walked.

### Save parity against a real vanilla server

A dedicated live test hands a world to a real Mojang server running in a container, lets it load
and (optionally) save the world, and compares the result — in both directions: our writer handed to
a real reader, and vice versa. A byte-for-byte comparison of the two directories is the wrong
assertion and cannot ever pass, because several kinds of difference are genuinely correct vanilla
behavior (wall-clock and tick-count fields that are supposed to advance, chunk payloads
recompressed at a different point in the sector layout, NBT compound field order, which is never
part of a value's identity). The real assertion is semantic identity after a structural NBT
comparison, decoding certain packed fields down to individual cells rather than comparing packed
bytes, since two different but equally valid packings of the same content encode to different raw
bytes. An explicit, narrowly-scoped allowlist names every field a real vanilla server is expected to
change and why; nothing touching actual block states, positions, or persisted structures is ever on
that list, and a control asserts that no allowed pattern can ever match one of those fields. This
kind of gate is the only way several real defects were ever found and are worth remembering as a
class: a generator writing two different string spellings for what should be one identical fluid
state, and — historically — a save path that silently flattened 3-D biome data into one value per
column and dropped structure references outright, neither of which any purely internal round-trip
test could ever have seen, since our own writer and reader would have shared the same mistake.

## How to change it

- **A new persisted scalar or rule**: add its typed accessor, forward it through the shared world
  state handle, and find its real decision point before considering it done — an accessor with no
  reader is exactly the kind of island this subsystem exists to avoid creating again.
- **Persisting a new `level.dat`-adjacent field**: check whether 26.2 actually keeps it in
  `level.dat` at all before adding it there — several concepts people reach for first (weather, the
  day/night clock, the world border, the persisted game-rule table itself) are each their own
  separate save file in this version, not fields inside `level.dat`.
- **Adding a new persisted block-entity or point-of-interest kind**: prefer keeping an unrecognized
  on-disk entry's data around unmodified (a passthrough) over silently dropping it — a real vanilla
  world loaded and re-saved here should not come back with emptied chests for every container kind
  this project doesn't yet simulate.
- **Verifying any on-disk schema change**: check it against a real file in both directions, and
  prefer an external, independently-written parser for the expected values over trusting this
  project's own reader to grade its own writer.

### Gotchas

- A stored seed always wins over a requested one when reopening an existing world — regenerating an
  already-explored world from a different seed would make it self-inconsistent exactly at the edge
  of wherever the player had already been.
- Packed index arrays are non-spanning (a fixed number of entries per machine word, with leftover
  high bits left as padding, not a dense bit stream) — a test built only from small palettes cannot
  tell this apart from a dense packing, since the two only disagree once an entry width doesn't
  evenly divide the word size.
- A missing heightmap on disk is fine and expected (a real client recomputes it on load); a
  deliberately *wrong* one is trusted outright and silently corrupts what a client believes about
  the terrain, which is why an unmodelled heightmap kind is better left absent than approximated.
- A world-metadata field's on-disk type is often surprising in a way that is easy to get backwards
  by analogy with a neighboring field (a numeric-looking value stored as a string, a delta stored as
  a signed rather than unsigned quantity, a priority stored as vanilla's actual numeric value rather
  than as an enum ordinal) — verify each one against a real written file rather than against a
  sibling field's own shape.
- Persistence for a subsystem that also runs in the browser build must stay behind its own
  native-only gate even when a *caller* of that subsystem is shared with the browser build — the
  caller still has to compile there, it just skips the disk-based half.

## Configuration

- The world directory is chosen by the world-select flow; each world is its own directory rather
  than sharing one implicit save slot.
- The autosave interval is a small constant, far shorter than vanilla's own default, since a save
  here only ever costs the dirty set rather than the whole resident cache — a quiet world writes
  almost nothing, and a clean quit does not depend on the timer anyway because shutdown always
  flushes before the process exits.
- Chunks are written with vanilla's own default compression scheme.

## Dependencies

- `lodestone-anvil` — the container formats (region files, `level.dat`, the world-generation-settings
  file), native-only.
- `lodestone-core` — the shared NBT tree and codec both layers build on.
- The chunk store this whole layer sits beneath — see `docs/chunk-lifecycle.md`.
- A real vanilla server running in a container, for the save-parity gate only; not required for any
  other path described here.
