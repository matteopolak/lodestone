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

Sky seeding fills every open cell at full strength, but only enqueues cells on the boundary with
unlit, light-transmitting cells. Interior daylight cannot raise any neighbour and does not need a
flood-queue entry. `lodestone_world::lighting::compute_sky` owns this distinction; when changing it,
compare the resulting light arrays with a full-source flood across mixed roofs and partially opaque
blocks, not only a flat open column.

Live relighting reads canonical state IDs from resident server columns through a read-only block
view. It does not build packet-format columns just to sample their blocks. Initial packet encoding
still constructs its own packet representation, because that path also needs biome palettes and
section storage. When changing the live view, compare its sky and block arrays with the buffered
conversion for all three dimensions and include a cross-column source.

The shared Overworld initial-light pass borrows its centre and eight server neighbours through
`InitialLightVolume`. Settlement no longer builds and discards a packet-format centre before the
encoder builds it again, and overlapping footprints do not rebuild neighbour block and biome
palettes. The shared solver still computes all
nine layers and the centre retains the same initial-packet normalization; retained sky does not seed
this fresh Overworld pass. The borrowed view clips reads and its air ceiling to the served dimension
window, including when a source column extends beyond it. Other dimensions keep their allocation and
retained-light preparation. Change `compute_overworld_initial_lights_borrowed` when extending this
boundary, and compare every returned layer with the buffered control in `initial_light_view_tests`.
This path has no cache or additional configuration and uses the existing shared lighting engine.

Set `LODESTONE_LIGHT_VIEW_PERF_ITERATIONS` to an integer in `1..=128` when running
`borrowed_overworld_initial_light_matches_buffered_roof_seam_and_air` in release mode with
`--nocapture` to compare both paths in the same binary. Each iteration runs both paths, alternates
their order, and retains the output through `black_box`. `LIGHT_VIEW_PERF` labels the boundary
`initial_overworld_centre`: the buffered arm converts its centre inside the measurement; both arms
borrow the same neighbours and run the same shared solver. Returned-light destruction is outside
the interval. Call counts, elapsed sums and supported macOS process instruction/cycle sums are
reported without a timing assertion; ordinary runs skip measurement. The synthetic roof and seam
fixture has little terrain or biome variety and deliberately places exterior emitters beyond both
ends of the centre's served window. A wrong extended-window control must illuminate the adjacent
in-window air cells at level 14, while the clipped path leaves their block light zero. Every returned
layer and the settlement's final packet bytes are compared with a separately buffered reference.
These sums isolate representation cost; they do not predict natural-terrain throughput or browser
join latency. See [benchmark instrumentation](oracles-and-benchmarks.md) for counter calibration.

Unseeded light scans can skip block lookup above a column's proven air ceiling. The server derives
that ceiling from packed section indices, not the world-surface heightmap: air variants can exist
above the heightmap and still affect the light section's wire shape. The world column uses its
maintained non-air section counts. Other `BlockVolume` implementations default to the full height;
an override must be a conservative upper bound, and should be checked against a full-scan volume.
The shared nine-column initial-light pass reads each column's ceiling once and scans one 16×16
tile at a time. A tile above that ceiling (or below the column's minimum Y) uses the volume's
air state without block reads; if that state has zero opacity and emission, the already-zero
opacity and block buffers need no per-cell writes. Missing neighbours still fill an opaque tile.
When every loaded tile uses the same zero-opacity, non-emitting air and no retained sky values
need replay, the shared pass stops at a section boundary at least 15 cells above the highest air
ceiling. No block source below can reach beyond that boundary, and its last computed sky layer is
already open daylight. Upper sections are emitted directly as full sky for loaded columns (zero
sky for missing columns or a dimension without skylight) and zero block light. This also avoids
allocating the omitted portion of the flood fields. Retained sky, unusual air, or a ceiling beyond
the field height keeps the full-height path. The cutoff has no setting; it follows each volume's
`air_above_y` and the fixed maximum light level. The mixed-height nine-column test compares every
returned layer and retained full-sky budget with full-height sampling, and checks that a falsely
low neighbour ceiling produces a mismatch.
When changing this path, compare every returned light column against a full-height volume,
including retained sky and a dimension without skylight; a falsely low ceiling can silently
erase a light source. The `light_shared_initial_3x3` benchmark provides repeatable,
non-flat timing inputs. The benchmark
runs a full-scan control against the same nine columns in the same optimized binary; that
control also forces block reads above the air ceiling, so it is not an exact pre-cutoff baseline.

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

Hosted protocols whose initial packet must resolve light entering from an adjacent chunk opt into the
retained-initial-light path. Their source admission settles the complete 3x3 footprint before encoding,
and the initial encoder consumes that centre snapshot; protocols without that requirement continue to
use the isolated initial encoder.

### Dimension sky rules

Sky seeding is a dimension property, not a consequence of a column's vertical shape. The Nether and
the End both use a 0..256 served window, but only the Nether lacks skylight. Its initial chunk form
therefore omits every sky section and keeps zero block-light data only in sections whose storage is
allocated by the centre plus its loaded 3x3 footprint. Every non-air block section allocates its own
layer and the immediately adjacent section nodes, so a non-emitting block in a neighbouring column can
extend the explicit Empty mask by one section. This is an exact, potentially sparse mask: unallocated
sections remain Missing even when a higher section is allocated. Its later light updates retain
explicit zero sky and block values for the normal clear operation. A Nether initial packet admits
block light from the centre and cardinal-neighbour columns, while diagonal-neighbour contributions
wait for the later seam-aware update; this keeps the initial packet at the persisted-column lifecycle
boundary without changing the 3x3 storage mask. The End has sky light, and its
initial packet consumes the exact `ColumnLight` snapshot captured by the source's settlement
transaction. A generated source without a retained snapshot uses a bounded fallback: it keeps the
complete computed sky result, then removes both light layers from sections that the centre plus loaded
3x3 footprint would not allocate. This preserves the lower-apron omission and full-sky run seen in
the first small-batch capture while leaving persisted snapshots untouched. The same footprint mask is
applied to each newly computed End dependency layer: an all-air footprint remains entirely Missing,
while an all-air selected column inherits only the vertical corridor induced by admitted neighbouring
terrain. Its initial block layer admits centre and cardinal-neighbour sources while deferring diagonal
propagation until a later seam-aware update; the allocation mask still reflects the complete loaded
3x3 footprint. The mask is derived from block occupancy in the admitted columns, never from coordinates
or state-name strings. A retained non-zero sky layer in one dependency never filters a fresh terrain
neighbour out of that footprint walk. A `DependencyInitialized` all-sections layer is not a prior
centre allocation; only a `CentreSettled` snapshot can preserve a sparse prior mask. The Overworld keeps
the one-section sky form and elides uniform zero block light
in an initial chunk. `ChunkSource::dimension`
carries that choice to the protocol's dimension-aware initial-encoding and light-computation hooks. An
unlabelled source uses the Overworld as the compatibility default; an in-memory generator therefore
stays unlabelled, while `RegionChunkSource` forwards the typed dimension it was opened for so a bare
persistent source cannot silently apply Overworld normalization to Nether or End packets. A dimension
wrapper must always forward its label.

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
An allocated light column whose sky and block layers are all zero is still an
unsettled dependency, so a later admission may compute and populate it; only a
`CentreSettled` retained snapshot can satisfy the centre fast path.

The snapshot also carries a typed lifecycle stage on `ChunkColumn`: a
`DependencyInitialized` entry records storage created while settling another
centre, while `CentreSettled` records completion of that column's own initial
admission. The fast path checks this stage rather than inspecting light values,
so a populated dependency still receives its centre admission. The stage is
stored with the retained light in chunk NBT and is cleared with the light on any
block mutation. Version-specific initial-chunk encoders consume retained light
only after the centre reaches `CentreSettled`; dependency snapshots remain
available as seeds for the next admission and are never serialized as though
their own centre admission had completed. A later footprint may read a
`CentreSettled` column as a dependency, but its dependency result cannot
downgrade or replace that authoritative centre snapshot; a block mutation
clears the status before any replacement is allowed.

For both skylit dimensions, each fresh initial admission has a complete 3x3
terrain footprint, so its centre sky layer is recomputed from current terrain
even when older retained snapshots exist. Reusing a retained dependency's sky
cells as the new centre's flood source would let an old full-sky layer bypass
current terrain attenuation. A partial retained sky layer can also suppress
daylight over otherwise open terrain, so neither skylit dimension seeds its
fresh centre from it. The End still restores each retained dependency
verbatim after that fresh centre computation, preserving its sparse per-column
wire shape; the Overworld keeps its ordinary compact form. The Nether remains
block-light-only and keeps its lifecycle-aware retained block-light seeds.

The Nether admission path also keeps a missing dependency out of the flood
entirely. A generated column may be present as terrain for allocation accounting,
but until its light layer is admitted it is an opaque seam and cannot contribute
emission or propagation to a neighbouring centre. A later centre uses retained
block-light values from admitted dependencies as seeds, settles its own terrain
first, then records newly queued dependency allocation separately. This keeps
zero-valued storage distinguishable from an absent layer and prevents packet
section masks from being reconstructed from terrain after holder eviction. A
retained dependency uses the same light window as its column: the number of
block sections plus the lower and upper boundary sections. Helpers cloning an
empty dependency shape must remove those two boundaries before calling
`ColumnLight::new`, whose argument is a block-section count; passing an existing
light-section count would add the boundaries twice.

The initial chunk encoder consumes a `CentreSettled` retained snapshot verbatim. An independent sealed-
world capture showed that a persisted End section mask can differ from the first in-memory settlement,
so a reload must serve what storage restored rather than recomputing from the current terrain or a
neighbour subset. A `DependencyInitialized` snapshot instead remains input to the next centre
admission. That capture is external evidence for the storage contract; it does not claim that this
repository drives the capture's scheduler or loading-ticket sequence in a production test.

When an imported End `CentreSettled` snapshot has persisted sky storage but omitted uniformly-zero
block arrays, the region source restores explicit `Uniform(0)` block storage for those same sections.
This preserves the saved allocation mask while leaving all light values unchanged; other dimensions,
dependency snapshots, and unlabelled generated columns keep their existing representation rules.

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
A column whose storage height differs from the light the protocol sized for its wire dimension (a plugin dimension served under a standard dimension's framing) cannot retain that light, so the initial-packet path declines the transaction and the ordinary neighbour-aware encode serves it.
The typed native record path stores only a canonical, centre-settled `ColumnLight` beside terrain and
reattaches it to the decoded `ChunkColumn` on reopen. Native saves recompute when a dirty column has
only dependency-initialized storage, clear the attached lifecycle marker before writing, and mark the
separate decoded payload as `CentreSettled`; this prevents a dependency snapshot from being paired
with an unrelated canonical payload. An Anvil import follows the same rule, while the NBT decoder
rejects a lifecycle marker that has no retained light arrays. Persisted columns remain authoritative
after eviction and restart; admission and ticket policy remain the responsibility of the source/cache
lifecycle that owns the column.

The NBT field `LodestoneLightSections` records the retained light window (including when it differs
from the terrain column's own section count, for example a dimension-aware wire snapshot backed by a
compact fixture column). Older saves omit the field and use the terrain count plus the two boundary
sections.

The bounded cache must therefore sit above a persistence-capable source whenever retained light is
part of the serving contract. The focused `RegionChunkSource` control exercises a plural admission,
evicts and reloads both footprint members, then mutates the centre and verifies that invalidation
clears both restored snapshots; a bare generator is intentionally not an equivalent substitute.

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
bounded retry sequence ends in an exclusive footprint transaction. Only a write that actually lands
advances revisions: a non-blocking edit that bails out (column absent, not yet full, out of range,
or source busy) releases its gates without a bump, so a failed attempt cannot make an outstanding
halo stale. The sequence is covered by
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

Tick-driven changes use the connection's delivered-column ledger rather than the server cache's
resident set. A pending join column needs no block or light update because its later complete
snapshot contains the mutation; an already-delivered neighbour still receives any cross-boundary
light change. Producers that know both block states compare emission and dampening when they
publish the change; an unknown prior state conservatively requires relighting. Every block update
is still sent. Only light-changing updates add destinations to the per-connection FIFO queue,
which deduplicates across batches. Direct player edits use that same queue on native connections
with a shared source and a protocol-provided pure light computation. The block change is sent
immediately; nearby destinations share a solve admitted to the bounded worker dispatcher. Tick
relights require an already-resident footprint, while direct edits can complete a cold neighbouring
footprint on that worker. The connection keeps reading packets and sends a light update only if its
settled snapshot is still current and the destination remains delivered. A later block edit can
therefore invalidate an in-flight result without letting stale light overwrite its update. Other
source and protocol combinations retain the synchronous path.
The cooperative cache transaction, browser operation slices, shared input field and deferred
edge-halo retry policy are described in [Resident relighting](resident-relighting.md).
Scheduled fluid writes capture the replaced state immediately before each write, so changes to a
fluid's level can avoid a full light recomputation when their light properties are unchanged.

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
The Nether fallback admits centre and cardinal-neighbour block-light sources and defers diagonal
propagation until a light update; persisted snapshots remain authoritative.
If that fallback is handed only a partial neighbour list, it treats fresh neighbour emission as
deferred while still using the supplied terrain to derive allocation-only masks; only a complete
3x3 input crosses the cardinal-source boundary.
When a generated request carries an owning packet snapshot, its complete detached 3x3 is borrowed by
the same status-aware computation before encoding. The computed centre light is installed only on the
packet copy as `CentreSettled`; it is not a resident lifecycle transition or a persistence write.
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
connection runtime and prevent that very join stream from progressing. The compatible
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
- **Keep timer-driven relights resident-only.** A direct player edit can complete its light footprint
  off the connection loop, but a world-tick feed may precede a join snapshot. A detached compute capability must
  use the same settlement and invalidation rules as the synchronous path. Check the retained centre
  again before sending its result, because another edit may finish while the worker runs.
- **Keep the block-change feed's light decision at the producer.** A connection sees the new block
  after the write and cannot recover the old light properties from that cell. When extending a tick
  producer, publish both old and new states if available; use the conservative unknown-old path
  otherwise. LAN relays must forward the decision unchanged.
- **Never compute light on the client's own live (multiplayer) path.** The client's contract is that
  a live server connection supplies light and a local/offline world computes its own — this
  subsystem exists precisely so the server side of that contract is actually held up.

## Configuration

The shared timing sink exposes `resident-light-settlement`,
`resident-light-compute`, `resident-light-encode` and `connection-relight`.
Settlement includes snapshot capture, retries and commit; compute counts each
attempt separately. Connection relighting measures occupied future polls, not
suspended transport time. These nested totals overlap with `connection-poll`
and must not be added. Native detached computation reports on its worker;
browser resident computation runs inside connection polling. Initial chunk
lighting remains the separate `packet-lighting` phase.

There is no runtime setting. `ServerProtocol::retains_initial_column_light` is the capability
boundary for exact initial-light settlement; its default is `false`. The optional
`ServerProtocol::detached_light_compute` supplies a pure function for native single-output tick and
direct-edit relights. `detached_resident_light_compute` and `compute_resident_light_batch` share
grouped computation between native and browser execution; the 26.2 protocol implements them.
Compute timing covers occupied slices, not the host waits between them. Other protocols retain the ordinary
light-computation hooks. The per-block-state census is fixed data for a given
game version, regenerated from its real source only when that version changes.

## Dependencies

- The shared world-storage crate's lighting engine and light-data wire format.
- The generated per-block-state light census (dampening and emission), and the block-state
  name/property census it is keyed through.
- The chunk source/column types the light is computed over — see `docs/chunk-storage.md` and
  `docs/chunk-lifecycle.md`.
- The native bounded worker dispatcher for detached light settlement.
- An independent reference server (for oracle comparison only) and the pinned game-version data
  sources behind the census; neither is required for the engine to run in production.
