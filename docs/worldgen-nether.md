# Nether world generation

## What it is

`lodestone_worldgen::nether::NetherGenerator` produces a complete Nether
column from the bundled noise, biome, feature, tag and structure documents. It
uses the legacy world-generation random family required by the Nether settings.

## How it works

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
The streaming replay retains admitted columns and completed source bodies
across successive target rows; each target adds only its new halo and its
not-yet-completed 3×3 sources. A target therefore observes the authenticated
earlier prefix rather than a freshly regenerated halo.
Each source completion builds its placement views from those live resident
columns, including writes committed by prior completions. The sparse transition
overlay remains the commit ledger, but it is not a substitute for the resident
read view: neighborhood predicates such as blob replacement must see retained
basalt, structure, and vegetation states regardless of frame batch size or
immutable-cache worker count.

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

The cached biome slice is stored as sixteen constructor-assigned `u8` ids rather
than sixteen owned strings. The generator's shared source-order table resolves
those ids for vegetation membership checks and expands them only at the public
`NetherColumn` boundary, so the cache remains immutable and output-compatible
while avoiding per-entry string payloads and their allocation metadata.

Set `LODESTONE_NETHER_PROFILE=1` for the optional `NetherGenerator::cache_stats`
timings. The report separates shard-lock wait/hold time from `OnceLock` waits
and actual computations, so a cache convoy can be attributed to lock
contention, repeated work or a real dependency wait rather than inferred from
wall time alone.

Biome carvers are normalized by `compose::build_biome_carvers` before that
prefix runs. A biome document may declare one carver id directly or an ordered
array; both forms become the same ordered carver list. Treating the direct form
as an empty array removes the entire cave pass, which leaves solid netherrack
where the generated terrain has cave air and lava.

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

## Configuration

There are no runtime flags. `noise_settings/nether.json` selects
`legacy_random_source`; the biome documents under
`crates/lodestone-server/assets/worldgen/biome/` select placed features and the
configured/placed-feature documents provide their bodies and placement
modifiers.

## Dependencies

`lodestone-worldgen`'s `compose`, `feature`, `feature::vegetation`,
`feature::region_view`, carver, surface and structure stages; bundled assets in
`lodestone-server`; and the cached external Nether region oracle described
above.
