# Server-side light

## What it is

How the integrated server computes the sky and block light it puts on the wire for a served chunk,
and how it keeps that light current after a block is placed or broken, rather than only computing
it once at the moment a chunk is first sent.

## How it works

### The engine and the census

Sky light and block light share one flood-fill algorithm, descending one level per step from a
source cell, differing only in what seeds it: sky light seeds every cell open to the sky at the
maximum level, block light seeds every cell whose block actually emits light. The engine takes its
per-block-state light behavior (how much a block dampens light passing through it, and how much it
emits on its own) through an injected lookup table built from a real per-block-state census for this
game version, so the lighting engine itself stays free of any registry or version dependency. Two
entry points exist: computing a column in isolation, and computing it against a real 3×3
neighborhood — the isolated compute is exact for everything except a thin band near the column's own
edge, since light decays fast enough that nothing more than one chunk away can ever reach into it.

**An absent light section on the wire means full daylight, not darkness.** A section present in
neither the sky nor the block light data is resolved by a real client to that dimension's own
default (maximum, for the overworld) rather than treated as zero — which is exactly why sending no
computed light at all (the state before this subsystem existed) produced a uniformly *bright* world:
lit caves, lit sealed rooms, no real night, rather than the reverse.

The served sky payload keeps the first uniformly full-sky section above the highest non-air terrain
section, then leaves higher sections absent. This is a wire-shape rule, not a lighting-value change:
the omitted sections still resolve to full daylight, while retaining them would allocate redundant
full arrays and fail byte parity. The engine leaves the result alone when that premise is not true,
so a non-full section is never silently converted into an omission. Block-light arrays are not
trimmed by this rule; a non-zero border value can be a legitimate contribution from a loaded
neighbour.

### Keeping light current after an edit

A block edit that changes what a cell emits (placing or breaking a torch, a lit furnace, glowstone)
or what it dampens (breaking a solid block open to reveal a shaft, or sealing one) triggers a real
recompute-and-resend of the affected column's light, over the same dedicated light-update wire
message a real client already merges into its own view. The predicate deciding whether an edit is
worth this cost compares the two blocks' **resolved** emission and dampening values, not their raw
state strings — a decorative-only change (a block's rotation, an unrelated property) must never
trigger a resend, while a change that alters neither the string's obvious "look" nor its emission but
does change its dampening (a block swapped for one of equal opacity but different type) still must,
since sky light crossing a boundary depends on dampening, not on emission alone. Getting this
predicate narrowed to emission alone was a real, owner-visible bug: breaking a tree trunk (which
emits nothing, same as the air replacing it) darkened nothing on the wire even though a real shaft of
daylight had just opened up, because the check never looked at what the edit had done to *occlusion*.

### Cross-chunk propagation after an edit

A hosted family can opt into the neighborhood compute for a relight through `ServerProtocol`. The server obtains
the edited column and its eight adjacent columns, then passes that transient 3×3 view to the version
adapter. The light radius is at most 15 blocks, so this view contains every possible external
contribution to the centre column. Families that do not opt in retain the isolated path.

When an edit changes emission or dampening, the server recomputes and sends all nine columns, not
only the edited one. That fanout matters at both edges and corners: changing one seam cell can alter
the light a client renders in either column. The calculation is intentionally not cached on a
`ChunkColumn`, avoiding a stale-derived-data path after a retained column is edited.

Initial chunk batches obtain the already-resident members of the same 3×3 view before the protocol
encoder writes the inline-light payload, so a boundary emitter or occluder is correct from the first
client-visible chunk when its neighbour is loaded. A missing neighbour stays an opaque seam, exactly
like the isolated result; this read must never generate eight extra columns. Protocols that do not
opt into cross-column light keep their one-column encoder and do not pay for adjacent reads. The
detached worker encoder is likewise bypassed for an opted-in family until it can carry the same
neighbourhood explicitly.

A retaining source must preserve an explicitly resident backing column when it has not retained its
own copy yet. Otherwise the initial centre column can be wrapped and served before its already-loaded
neighbours enter the cache, turning a fully resident 3×3 into eight opaque seams. The fallback is a
resident lookup, never a generated-column read.

The update still travels on the acting connection. Broadcasting the result to other players sharing
the world remains separate multiplayer work.

World-tick updates have a stricter loading rule than a player action. Their timer may observe a
mutation in a column that the joining connection has not streamed, so it copies the complete light
footprint through `ChunkSource::resident_column` and sends nothing when any member is absent. The
later chunk snapshot is authoritative and already includes that mutation. This avoids making a
connection timer synchronously generate a cold 3×3 footprint, which would otherwise monopolize the
current-thread connection runtime and prevent that very join stream from progressing. The compatible
whole-column fallback also uses the resident centre snapshot.

The connection sends every tick-feed block update even while its initial join stream is still in
flight. A pending chunk snapshot supersedes an earlier update when it eventually arrives, but it
cannot repair a column the client already received while later columns are still pending. Lighting
is still deduplicated by affected column and uses only resident data, so this correctness path never
turns a cache miss into join-time terrain generation.

### Validated against a real, already-lit vanilla world

The engine's correctness is checked against a real vanilla server's own generated-and-lit world data
— the world's stored sky and block light arrays, computed entirely independently of this project's
own terrain reader, so neither the input blocks nor the expected light output came from this code.
Sky light agrees with vanilla's own values completely on real, laterally-varied terrain (chunks whose
light is not merely straight-down attenuation through open sky or water, which would trivially agree
under almost any implementation); the small residual disagreement in block light is a documented gap
in the per-block-state emission census for a couple of specific block types, not a defect in the
propagation algorithm itself — and the same governing invariant applies here as at the chunk border:
this project's light is never *brighter* than a real vanilla server's own answer, only occasionally
too dark where a census entry is still missing, which is the safe direction for a gap of this kind to
fail in.

## How to change it

- **Adding or correcting a block's light behavior**: update its emission/dampening entry in the
  per-block-state census, from the real per-version data source, not from a value inferred by
  analogy with a similar-looking block.
- **Adding a hosted family to cross-column light**: implement
  `ServerProtocol::uses_cross_column_light` and
  `ServerProtocol::compute_column_light_with_neighbours` together. Preserve the 3×3 relative
  coordinates; absent columns must use that family's ordinary generated or unloaded-column result.
  Add a direct solver control and a client-observed edit test where the isolated result is provably
  different. The direct fixture must also supply all eight neighbours: for an opaque centre roof and
  open east column, east-only and full-3×3 results are both sky light 14 at the east border, while
  the seven-neighbour control without east is 7 through the longer north/south paths. This catches a
  supplied-neighbour assembly that works only when its list happens to contain one entry.
- **Extending an off-task chunk encoder to cross-column light**: carry the same 3×3 neighborhood into
  its worker-facing contract before enabling it for a family that opts into cross-column light. Do not
  fall back to the isolated encoder: the initial and live packet paths must agree at a border.
- **Do not widen the relight predicate to fire on every placement "to be safe."** A resend recomputes
  and re-sends a whole column's worth of data; firing it on every ordinary, non-light-relevant edit
  (a block swapped for another of identical light behavior) turns routine building into a stream of
  unnecessary full-column resends.
- **Keep timer-driven relights resident-only.** A direct player edit can deliberately load its own
  known area, but a world-tick feed may precede a join snapshot. Use
  `send_resident_lighting_for_tick` for the latter so a background mutation cannot starve connection
  progress by generating terrain from the connection runtime.
- **Never compute light on the client's own live (multiplayer) path.** The client's contract is that
  a live server connection supplies light and a local/offline world computes its own — this
  subsystem exists precisely so the server side of that contract is actually held up.

## Configuration

None. Light is computed unconditionally for every served column and on every light-relevant edit;
there is no feature flag or setting that disables it. The per-block-state census is fixed data for a
given game version, regenerated from its real source only when that version changes.

## Dependencies

- The shared world-storage crate's lighting engine and light-data wire format.
- The generated per-block-state light census (dampening and emission), and the block-state
  name/property census it is keyed through.
- The chunk source/column types the light is computed over — see `docs/chunk-storage.md` and
  `docs/chunk-lifecycle.md`.
- A real vanilla server (for the oracle comparison only) and the pinned game-version data sources
  behind the census; neither is required for the engine to run in production.
