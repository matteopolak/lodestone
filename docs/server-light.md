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

The ordinary sky payload keeps a bounded run of uniformly full-sky sections above the highest non-air
terrain section, then leaves higher sections absent. This is a wire-shape rule, not a lighting-value change:
the omitted sections still resolve to full daylight, while retaining them would allocate redundant
full arrays and fail byte parity. The engine leaves the result alone when that premise is not true,
so a non-full section is never silently converted into an omission. A protocol-specific initial
chunk form can retain a longer bounded run without changing the flood result; later light updates
keep the ordinary compact form. Explicit zero block-light sections in an update clear a client's
existing value, so update packets always retain them.

### Dimension sky rules

Sky seeding is a dimension property, not a consequence of a column's vertical shape. The Nether and
the End both use a 0..256 served window, but only the Nether lacks skylight. Its initial chunk form
therefore omits every sky section and keeps zero block-light data only in sections whose storage is
allocated by the centre plus its loaded 3x3 footprint. Every non-air block section allocates its own
layer and the immediately adjacent section nodes, so a non-emitting block in a neighbouring column can
extend the explicit Empty mask by one section. This is an exact, potentially sparse mask: unallocated
sections remain Missing even when a higher section is allocated. Its later light updates retain
explicit zero sky and block values for the normal clear operation. The End has sky light, and its
initial packet consumes the exact `ColumnLight` snapshot captured by the source's settlement
transaction. A generated source without a retained snapshot uses a bounded fallback: it keeps the
complete computed sky result, then removes both light layers from sections that the centre plus loaded
3x3 footprint would not allocate. This preserves the lower-apron omission and full-sky run seen in
the first small-batch capture while leaving persisted snapshots untouched. The Overworld keeps the
one-section sky form and elides uniform zero block light in an initial chunk. `ChunkSource::dimension`
carries that choice to the protocol's dimension-aware initial-encoding and light-computation hooks. An
unlabelled source uses the Overworld as the compatibility default; a dimension wrapper must always
forward its label.

### Retained snapshots and reloads

For a protocol that opts into retained initial light, one admission may produce more than the centre
snapshot. `ServerProtocol::compute_initial_column_lights_with_neighbours_in_dimension` returns a
typed `ColumnLightSettlement`: the centre is mandatory, and each additional entry names a relative
coordinate in the admitted 3x3 footprint together with its exact `ColumnLight` (including its
`Missing`, explicit-empty, full, or varied section representation). `ChunkStore` captures every
requested coordinate, checks all of their revisions, and forwards the returned entries to the source
as one batch before releasing the gates. Existing protocol adapters use the default centre-only
adapter, while a version adapter can return whichever sparse or fully populated dependency records
its wire lifecycle actually produces; the source does not infer a dimension policy.

The initial chunk encoder consumes a retained snapshot verbatim. An independent sealed-world capture
showed that a persisted End section mask can differ from the first in-memory settlement, so a reload
must serve what storage restored rather than recomputing from the current terrain or a neighbour
subset. That capture is external evidence for the storage contract; it does not claim that this
repository drives the capture's scheduler or loading-ticket sequence in a production test.

`column_to_nbt` writes the retained snapshot's sky and block arrays, including the light-only sections
immediately below and above the block range, and marks the column as light-complete. `column_from_nbt`
restores those arrays into `ChunkColumn` before a source can serve the column again. Only protocols
that return `true` from `ServerProtocol::retains_initial_column_light` enter this settlement path;
the default is `false`, so legacy one-column protocols do not admit neighbours or persist a snapshot
they do not consume. `RegionChunkSource` keeps opted-in snapshots in its edit/persistence path, while
`ChunkStore::store_resident_columns` updates the cache and forwards the complete batch instead of
allowing an in-memory cache hit to hide a future reload. The source trait's default batch method
preserves compatibility for small sources by forwarding individual columns; persistent sources
that need an all-at-once visibility boundary override it.
The typed native record path stores the same `ColumnLight` beside terrain and reattaches it to the
decoded `ChunkColumn` on reopen, so a caller can feed that record directly back to the serving source.
Persisted columns remain authoritative after eviction and restart; admission and ticket policy remain
the responsibility of the source/cache lifecycle that owns the column.

The coordinate-free controls in `crates/versions/26.2/tests/overworld_light_lifecycle.rs` make the
initial wire distinction independently observable. They build the same synthetic column with 29
non-air cells, then compare an attached snapshot against an unsettled generated fallback. The retained
packet's raw suffix contains the exact high sky present bit and explicit zero block empty bit from the
snapshot; the fallback keeps only its compact sky run and elides uniform-zero block sections. The tests
parse the non-light prefix separately, so an unrelated terrain mismatch cannot masquerade as a light
settlement failure.

`ChunkStore` uses a per-coordinate revision/CAS check from snapshot through commit, while keeping both the
global cache mutex and the coordinate gates out of the expensive optimistic light computation. A block
mutation in the centre or any dependency that completes after capture invalidates the older result; a
bounded retry sequence ends in an exclusive footprint transaction. The sequence is covered by
`a_light_snapshot_commit_rejects_a_block_write_after_capture`,
`a_neighbour_snapshot_commit_rejects_a_neighbour_write_after_capture`, and
`exclusive_light_settlement_makes_progress_before_a_waiting_neighbour_write`; source/cache ordering
is covered by `same_coordinate_light_refresh_cannot_overwrite_a_newer_block_mutation`.

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
the light a client renders in either column. The computed value is replaced on the affected
`ChunkColumn` only after the complete 3×3 result is available, so the retained snapshot cannot be
partially updated.

For an opted-in protocol, the recalculated result replaces the affected column's retained snapshot
before the update is sent. `ChunkColumn::set_block` also invalidates any old snapshot immediately, so
a later initial send cannot mistake derived light for current state.

For an opted-in protocol, initial chunk encoding assembles the centre and its eight neighbours in a
fixed relative-coordinate order, then settles every snapshot returned for that admission through the
validated read/compute/commit fence. The order is only a deterministic admission/assembly rule; the
light result is not allowed to depend on direction, coordinate, or holder ordinal. A missing
neighbour is resolved through the source's normal column path so the fence has a complete 3×3 input;
the centre and any returned dependency snapshots are stored before the initial packet is written.
Protocols that do not opt into retained initial light
keep their one-column encoder and do not pay for adjacent reads. The detached worker encoder remains
on the one-column contract until it can carry the same neighbourhood explicitly.

A retaining source must preserve an explicitly resident backing column when it has not retained its
own copy yet. Otherwise the initial centre column can be wrapped and served before its already-loaded
neighbours enter the cache, turning a fully resident 3×3 into eight opaque seams. The fallback is a
resident lookup before the source's normal generation path is used to complete the fence.

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

### Validated against an independent, already-lit reference world

The engine's correctness is checked against an independent reference server's generated-and-lit world
data — stored sky and block light arrays computed independently of this project's terrain reader, so
neither the input blocks nor the expected light output came from this code. Sky light agrees completely
on laterally-varied terrain; the small residual block-light disagreement is a documented gap in the
per-block-state emission census for a couple of specific block types, not a propagation defect.

An external End lifecycle trace also showed that packet/storage snapshots can agree at settlement while
the resident column is still present, then differ after reload: one target's sky mask grew from
sections `0..=5` to `0..=6`, and empty sections became explicit stored light. The sealed reload export
is therefore the external parity oracle. It is not a production scheduler/ticket test or a runtime
comparator; the production contract is the exact `ColumnLight` persistence and restore path described
above.

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
- **Opting a family into retained initial light**: implement
  `ServerProtocol::retains_initial_column_light` only when the initial chunk encoder consumes the
  column's exact snapshot. Exercise the source/cache/save/reload path and conflicts where a block write
  completes between snapshot capture and light commit, including a dependency column; leave the
  capability disabled for a family whose wire form does not consume it.
- **Adding a dimension or changing its sky rule**: route the dimension through the protocol's
  `*_in_dimension` chunk and light hooks, then decode-test both its initial chunk and a light update.
  Do not infer skylight from `min_y`, height, or section count: the Nether and End share the same
  served window while differing from one another's sky rule.
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

There is no runtime setting. `ServerProtocol::retains_initial_column_light` is the capability
boundary for exact initial-light settlement; its default is `false`. Light-relevant edits still use
the protocol's ordinary light-computation hooks. The per-block-state census is fixed data for a given
game version, regenerated from its real source only when that version changes.

## Dependencies

- The shared world-storage crate's lighting engine and light-data wire format.
- The generated per-block-state light census (dampening and emission), and the block-state
  name/property census it is keyed through.
- The chunk source/column types the light is computed over — see `docs/chunk-storage.md` and
  `docs/chunk-lifecycle.md`.
- An independent reference server (for oracle comparison only) and the pinned game-version data
  sources behind the census; neither is required for the engine to run in production.
