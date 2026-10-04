# Nether world generation

## What it is

`lodestone_worldgen::nether::NetherGenerator` produces a complete Nether
column from the bundled noise, biome, feature, tag and structure documents. It
uses the legacy world-generation random family required by the Nether settings.

## How it works

`NetherGenerator::generation_identity` identifies a shaped producer by seed,
complete settings, trusted immutable resolver assets, and the resolved biome
parameter table. The constructor reads that selected table exactly once for
both parsing and its cold digest: identical backing assets can select different
tables. `Resolver::immutable_shaped_asset_fingerprint` is an explicit trust
contract with a default of `None`; forwarding the ordinary asset fingerprint
does not make an arbitrary wrapper eligible. `TableResolver` opts in for its
immutable id-keyed documents and templates. Nether shaping never consults the
selected temperature map or block freeze/survival callbacks; those callbacks
only feed later decoration. If a shaped stage starts consuming another selected
view, capture and identify it here and advance
`NetherGenerationIdentity::SHAPED_VERSION`. Coordinates and proof that a resident
column is still pristine remain the caller's responsibility; no identity is
inferred by scanning a resident column's content.

The generator caches the pure base prefix (structure references, fill, surface
and carvers) by exact chunk coordinate. Target-local structure pieces are
placed after that prefix, not stored in every admitted column. It then drives
the shared 3×3 feature writers: neighbouring source chunks can place blocks
into the served chunk, exactly as a feature near a border requires.

The lifecycle materializer admits base columns first and completes sources in
the captured order. When a target source completes, its cached structure pieces
are placed against the accumulated absolute-state overrides produced by earlier
sources, followed by that source's mixed features. The feature dispatcher uses
the source-centred shaped prefix plus that overlay; it does not substitute a
different centre column merely because a neighbour is already resident. This
matters for replacement predicates: a
neighbouring air-only mushroom can occupy a target cell before a fortress
foundation reaches it, so the foundation starts one block higher rather than
overwriting the mushroom. The ordinary `NetherGenerator::column` API has no
external completion stream, so it applies the target's structures directly
before composing its packet-ready mixed window.
Huge fungus bodies use a narrower ownership boundary than ordinary vegetation.
Their stems, caps, decorative blocks and hanging vines are first written into
the live source view, so later features see the same border state when they
evaluate heightmaps and replacement predicates. Direct packet generation folds
only source-owned fungus cells into its centre slice, while lifecycle replay
keeps source-crossing writes marked as transient in the ordered spill stream so
later sources and packet snapshots observe the same state. The boundary is
expressed by the feature body plus the commit path, not by a coordinate
exception.
Source-filtered completion also makes that target-local structure result the
resident center view before step-7 ore and vegetation writers read it, while
retaining structure-before-feature write order in the emitted spill stream.
The mixed source dispatcher derives its bounded 3×3 completion order from the
request's admission wavefront. Requests are tiled from their minimum admitted
chunk and ordered by tile-z, tile-x, local-z, then local-x; the target's source
window is sorted by that same key. This is why there is no universal
centre-first, centre-last, or axis-major permutation. In the seed-42 lifecycle
control, source `(5, 4)` first accepts gravel at world `(95, 32, 63)`, so the
later magma attempt from `(6, 3)` observes a non-netherrack resident block and
is rejected. Reversing those explicit completions leaves magma instead, which
makes the ordering regression observable without special-casing either
coordinate.
The streaming comparator (`LifecycleMaterializer::streaming_comparator`, used
by the captured-stream parity tests) retains admitted columns and completed
source bodies across successive target rows, persisting cross-target writes
regardless of the source's own `target_spills_persist` policy. Production
packet requests instead complete
each target as a transaction. Neighboring source writes remain visible to later
sources in that transaction, but only target-owned feature writes become durable.
This prevents a later target from revising an already-served column. A completed
resident column takes precedence over a cached shaped prefix when a neighboring
target is admitted, including after the column is loaded from disk. Saved full
columns do not carry client heightmaps, so the lifecycle derives those maps
from the saved block field when it first needs them.

Live streaming can initially retain a shaped target while the join worker is
filling the view. Before packet encoding, the source's admission hook upgrades
such a partial column through the same ordered full-column path used by direct
generation; this admits cross-border ore writes (for example, a source at
`(1, 7)` writing into `(2, 7)`) without adding a coordinate-specific rule.
Wrapper sources forward the hook, and a complete resident column remains a
cache hit, so the extra work applies only to sources whose packet contract needs
full decoration.

An integrated server registers each lazily-created Nether save handle before
the sibling is exposed to portal travel. Shutdown and autosave flush those
handles alongside the primary world, so portal blocks and generated columns
survive a restart. The connection also retains the primary source separately
from its active dimension; a player restored directly into the Nether can
therefore resolve the return portal through the same world-level sibling graph.

The noise carrier is 128 rows tall, but Nether vegetal decoration evaluates the
dimension's 256-row resident window. A border source can therefore place a
mushroom in the served chunk at or above y=128. `NetherColumn::decoration_spills`
keeps those upper-window writes separate from the compact terrain carrier, and
`ChunkColumn::from_nether` applies them after padding; otherwise the generated
heightmap would report air one block below the external result.

Resident lifecycle completion imports only the centre into a mutable dense
grid. The other 24 positions in the 5×5 read context retain concrete
`BlockRead::Packed` handles over the server's shared sections and their matching
typed palettes. Missing residents fall back to the immutable pre-decoration
prefix. `NetherOreView` borrows the vegetation grid's mutable overlay and source
table, so a completed structure, decoration or ore entry is visible to the next
entry without copying cells between two world representations. Ore reads the
5×5 window `[-32,48)` and writes only the inner 3×3 `[-16,32)`, over the 128-row
terrain carrier. Vegetation keeps its `[-24,40)` padded footprint and 256-row
receiving window.

Each structure step borrows its source-column baseline through `StructureWorld`
and retains only touched cells in a temporary write set. Reads outside that
source box return air. Every accepted write remains in the ordered structure
trace, including equal writes and writes later restored; only final net changes
reach the outer decoration overlay, in Y/Z/X order. This preserves the distinction
between an absent override and an explicit equal-state override without importing
and diffing the entire source column.

Nested structure feature groups borrow that live baseline at helper entry. Their
world-generation heights remain frozen during the group, while live heights see
earlier group writes. The borrow ends before the ordered captured writes replay
through the structure mutation context. Extend the shared placement body and
`StructureWorld` together; do not introduce a separate sparse placement algorithm.

The mixed placement body returns a `MixedDecorationResult` before output
materialization. Lifecycle source and target finalizers read its dirty stream
once to emit ordered spills and retain the original structure sidecar; they do
not build a dense return column that the lifecycle would discard. Only the
scalar `NetherGenerator::column` path folds writes into its centre and collects
upper-window output. Keep placement and random draws in the shared body when
changing these finalizers. Target completion already excluded captured spills
from dense writes, so this boundary removes no full-cell copy in that case;
source-only completion also avoids the discarded dense fold and its possible
copy-on-write allocation.

Those bounds also determine read precedence. Within the ore writer, reads see
the shared live overlay before the frozen resident or prefix snapshot. The
outer ore ring remains read-only: it sees completed-source overrides, then the
snapshot, without observing new padded vegetation writes. Its coordinates are
not clamped to the vegetation footprint. A sorted immutable override vector
retains the last supplied seed per coordinate and also identifies unchanged
seeds when emitting spills.

`NetherChunkSource::feature_result_for_target` projects lifecycle overrides
into that target-centred `[-32,48)` X/Z read window before allocating the
dispatcher input. It ranges over X in the ordered map and filters Z, retaining
`(x,y,z)` order and present-air entries. Overrides outside the window cannot
reach the centre snapshot, padded vegetation field or ore read ring. Y remains
unfiltered here: the generator owns its settings-dependent vertical seed
bounds, which need not match the server's fixed receiving window. When changing
placement bounds, update `nether_override_vec` with the widest override consumer;
the ore write window or vegetation footprint alone would discard readable seeds.

Source-ordered full completions prepare each spill destination once for resident
materialization and heightmap readiness. A completion-local 5×5 coordinate table
holds admission, mutability and padding facts; an exact overflow list handles
more distant destinations. The table is discarded before the next completion,
so later admission and mutability changes remain visible. Cell undo values,
override revisions and observer events stay in the original spill order, and
resident writes retain their existing per-destination batches. End uses the same
source-ordered boundary with its persistent-spill rule; target-owned completions
retain scalar preparation. Extend this in `LifecycleMaterializer` rather than
retaining destination readiness across source completions.

Ore entries record each touched cell's entry-start overlay value and write
ordinal. At entry completion their final changes append to the vegetation
dirty log in `(x,z,y)` order, once per coordinate. A cell restored to its prior
overlay value contributes no event; a previously absent overlay cell still
contributes an event even when its new state equals the source terrain. This
keeps original source palettes and their first-introduction order unchanged.
Decoration retains its own write order and tree-local dirty scopes. Spill
projection sorts touched cells once in `(x,y,z)` order, collapses repeated
positions to the shared final state, then removes unchanged non-transient
seeds. The resulting sequence is the lifecycle spill ordinal order.

Centre pre-pass overrides are applied before its immutable vegetation snapshot
is captured. WG height lanes read that snapshot, including those overrides and
upper resident rows, while later live overlay writes affect only live height
lanes. The generation ceiling remains 128 even when the receiving window is
256 rows. Shared resident writes detach through compact-storage copy-on-write
and cannot alter an already captured pass.

To change this handoff, follow
`NetherChunkSource::feature_result_for_target`,
`NetherGenerator::parity_target_pass_with_read_resident`,
`NetherOreView`, and
`VegGrid::with_read_sources_and_flat_biome_ids_shared_zoomed`.
Keep the centre dense and neighbour payloads shared. With `gen-counters`,
`ResidentDenseImport` reports cells and bytes allocated for dense imports,
`ResidentPacked` reports palette-index probes, and `ResidentPayloadCopy`
reports payload bytes detached by a resident write.

The prefix values are immutable for a fixed seed and coordinate. Normal
generation keeps a 1,024-entry sharded memo: each coordinate maps to a small
mutex-protected shard, while the value itself is published through `OnceLock`.
That keeps adjacent workers off one global cache lock and makes a racing miss
wait for the one computation instead of repeating terrain, structure and biome
work. Large packet replay calls `NetherGenerator::prepare_packet_replay` with
their target coordinates; that helper derives the full target-plus-neighbour
closure from the 5×5 prefix read radius and raises retention when needed. The
`pre_decoration_computations` and `pre_decoration_evictions` counters make
closure work and any thrashing visible without changing generation decisions.

The cached biome slice holds sixteen `BiomeRef` values. Mixed feature selection
borrows those values from the pass's held 5×5 prefix window and acquires a plan
only after its source passes the selected/completed-source checks. A selected
completion therefore selects one source's 3×3 biome union. The ordinary full
column still executes all nine sources in its existing order; every selected
entry retains its global step index and random stream. Parsed plans continue
to share the generator's existing biome-mask memo.

Flat-biome placement gates use `flat_biome_allows_membership` for both ore and
vegetation. The gate reads the four possible horizontal quart candidates and
skips the eight-corner distance calculation only when every candidate exists
and has the same typed feature-membership answer. Different biome identities
may agree on that answer. Missing candidates or disagreement retain the exact
three-dimensional zoom, including Y and its strict corner tie order. The gate
stays at its original modifier position and consumes no placement randomness;
there is no retained lookup cache or string parsing. Keep this helper limited
to flat biome sources when changing the placement adapters. Source-slot census
counts include the proof reads, not just the ultimately selected biome.

Surface biome conditions use typed `SurfaceBiomeAnswer` values through the
ordinary `SurfaceSystem` diff traversal. `ClimateSampler::is_xz_pure` admits a
horizontal shortcut only when all six density trees prove Y independence.
The scan's existing bounded corner products then separate into 36 horizontal
biome answers and the Y-dependent fiddle corners. When the four horizontal
biomes agree, every possible zoom winner has that same biome, so the answer is
valid for the entire vertical scan and no fiddle distances are evaluated.
No products survive the scan or enter another generator memo.

Different horizontal biomes retain all eight three-dimensional corner
distances, in the original order with the first strictly best corner winning.
Those answers certify only the queried Y. A climate tree that depends on Y
also keeps exact three-dimensional climate queries and never uses horizontal
answers. Out-of-window corners stay exact without extending the fixed storage.
Both paths retain the Nether's existing false snow-temperature answer.

Biome carvers are normalized by `compose::build_biome_carvers` before that
prefix runs. A biome document may declare one carver id directly or an ordered
array; both forms become the same ordered carver list. Treating the direct form
as an empty array removes the entire cave pass, which leaves solid netherrack
where the generated terrain has cave air and lava.

At construction, `uniform_carver_biome` compares every possible biome's
normalized ordered registry IDs. Identical declarations use one borrowed
catalog list throughout the 17×17 carving neighborhood, avoiding source-biome
climate samples. Different IDs, list lengths, or ordering retain per-source
climate selection. Direct-ID and single-element-array declarations agree;
carver order and source RNG seeding remain unchanged in both paths.

The Nether differs from the Overworld at decoration step 7. Each bundled biome
has a mixed list containing springs, fire, glowstone and mushrooms alongside
quartz, gold and debris ores. `NetherGenerator` splits only the placement body:
ore entries use the ore engine, other entries use the configured-feature
interpreter. Both retain their original step-7 array index and use step 7 in
their feature seed. Local-modification entries at step 2 remain in that same
global catalog and run before the mixed step; this is where soul-sand-valley
basalt pillars are selected. Step 9 then runs the usual vegetal interpreter.
Keeping step-2 entries in the source plan is necessary even when a later step
also writes the same terrain: a pillar must see the pre-ore substrate, and
removing the entry changes the feature stream that follows. This avoids the
tempting but wrong approach of filtering the list before seeding, which changes
every later feature stream.

Nether forest vegetation resolves its provider state before checking whether the
candidate can survive. The survival check is state-specific: crimson roots use
the `supports_crimson_roots` tag, while ordinary vegetation uses
`supports_vegetation`. Do not replace this with the generic sturdy-floor test;
that accepts or rejects the same candidate differently for provider states that
have their own support family.

The shared `count_on_every_layer` placement searches each candidate X/Z lane
downward for air-or-fluid above non-bedrock ground. `find_on_ground_y` reads
each lower position once and passes that state to the existing `VegTags::has`
air/fluid predicates and bedrock check. The upper-position short-circuit reads,
layer numbering and unsuccessful terminating placement draws stay unchanged.
Keep tag queries on the supplied tag table when changing this scan: replacing
them with direct block classification would change unbound-table behavior.
The focused `ground_search_` controls pin read order/counts, fluid states,
negative bounds, live overlays and independently calculated placement draws.
This path has no additional cache or configuration.

The reference world at
`.cache/mc/survival/world/dimensions/minecraft/the_nether/region/` supplies
the biome and bedrock-shell oracle fixture used by `nether_gen`. The feature
consumer control compares those same recorded full chunks with a resolver that
withholds feature documents; it must observe live production block changes,
while the cached bedrock masks remain exact.

## How to change it

When changing structure placement, keep the base prefix and the target-local
completion stage separate. Do not restore structure blocks to the cached
prefix: that makes every neighbour observe a future target write and changes
cross-chunk replacement decisions. The focused external controls in
`crates/lodestone-worldgen-parity/tests/nether_lifecycle_order.rs` cover both
the mushroom spill and an air control that must still accept the fortress
support.

When changing admission or spill ownership, keep consecutive target output
identical across an uninterrupted session and a save/reopen boundary. The
focused region-source regression covers both paths and checks that a later
target leaves a finalized neighbor unchanged. Do not promote target-local
neighbor writes into durable source completions without persisting their full
ordered spill state.

When changing the mixed mutable plane, preserve the distinct ore and vegetation
read bounds and the immutable outer-ring seeds. The
`single_nether_plane_preserves_read_ring_and_vertical_boundaries` control checks
unclamped reads, the padded boundary, y=127/128/255 and frozen versus live
heightmaps against the independent two-view fixture.
`single_nether_plane_matches_bridge_logs_palettes_and_spill_ordinals` checks
entry restoration, repeated writes, fungus ownership flags, palette order and
final spill sequence. The fixture bridge is test-only; production writes the
single vegetation overlay directly.

When adding a Nether feature body, keep its entry in the mixed list even if the
body is not supported yet. Unsupported entries still own a raw feature index;
removing one shifts the seed of every entry after it. Extend the shared feature
parser/body, then let `build_nether_feature_lists` route the new type rather
than adding a Nether-specific algorithm.

The shared `feature::apply_ore_step_3x3_per_source` wrapper is intentionally
fixed to step 6 for the Overworld. Nether code must call its explicit-step
variant with 7, because the step participates in each feature's seed. Do not
replace `LegacyRandomSource` with xoroshiro in either Nether feature pass;
either error creates plausible terrain with a different world layout.

When changing the biome-document parser, preserve a direct carver id as a
single-element list and preserve array order exactly. The source chunk and list
index seed each carver, so dropping or reordering an entry changes the whole
17×17 carve neighbourhood.

Keep the uniform-declaration proof aligned with `compose::build_biome_carvers`
if supported declaration forms change. Keep mixed-plan biome reads relative
to the held target window: a source offset of one chunk can read two chunks
from the target, so replacing the 5×5 context with a 3×3 context is invalid.

Keep surface shortcut admission tied to the density-tree purity proof, not the
dimension name or one sampled height. Mixed corners can choose a different
biome at adjacent heights even when climate itself is Y invariant; do not
certify them for a quart band. Extend the shared typed surface consumer rather
than copying its traversal, and preserve the strict distance tie comparison.

## Configuration

`LODESTONE_NETHER_PROFILE=1` enables optional cache timings. The
`noise_settings/nether.json` asset selects
`legacy_random_source`; the biome documents under
`crates/lodestone-server/assets/worldgen/biome/` select placed features and the
configured/placed-feature documents provide their bodies and placement
modifiers.

## Dependencies

`lodestone-worldgen`'s `compose`, `feature`, `feature::vegetation`,
`feature::region_view`, carver, surface and structure stages; bundled assets in
`lodestone-server`; and the cached external Nether region oracle described
above.
