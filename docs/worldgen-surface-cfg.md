# Compiled surface-rule evaluation

## What it is

The surface stage parses data-defined rule trees once and evaluates them through
a compact continuation graph during column scans. The graph preserves the
recursive rule's short-circuit behavior while removing per-block call-stack and
tree-walk overhead.

## How it works

Sequences are lowered from right to left, so each condition node has explicit
success and fallback edges. Conditions remain in a shared immutable arena;
their lazy X/Z and Y cache slots, biome callback, heightmap reads, and random
factory are unchanged. The recursive evaluator remains available inside the
module as a parity control, while production scans use the compiled entry.
The production column path disables only the Y-slot memo itself: a compiled
path is acyclic and visits each condition node at most once for a block, so a
Y-cache lookup cannot hit there. X/Z column memoization and all top-material
cache behavior remain enabled.

Typed position callbacks return `SurfaceBiomeAnswer`, which keeps its identity,
temperature predicate, and inclusive Y bounds private. The context checks those
bounds on every typed biome access, including accesses without a preceding Y
update, and refreshes an expired answer lazily. An uncertified callback uses
`SurfaceBiomeAnswer::exact` for one Y. Only `top_material_typed` supplies a fixed
answer valid at every Y for its isolated lookup. String biome answers and their
built-in identity memo still expire at each scanned position.

The region biome callback can omit the positional zoom when all eight possible
quart corners are already resident and carry the same built-in biome identity.
It uses the same shifted quart parent and vertical clamping as the zoomed
lookup. Mixed identities or any missing cell keep the original lookup, including
its stateful exterior climate search. The omitted zoom offsets depend only on
seed and coordinates; populating their request-local cache has no random-stream
or biome-search effect. With `q = (y - 2).div_euclid(4)`, the same eight candidates
apply only over `4q + 2..=4q + 5`. A successful resident proof certifies that band,
so the context reuses its answer within those four heights. Crossing either
boundary requires a new proof, even when vertically clamped cells stay equal.
Mixed identities and missing cells return an exact-Y answer and retain every
zoomed lookup and exterior search cursor update.

`biome` conditions compile generated built-in names into a two-word canonical
biome bitset. Names outside that registry are retained in an ordered fallback
vector for extension registries, so built-ins avoid repeated string-set scans
without changing extension matching; each position resolves its supplied name
to the typed built-in id at most once. Packed stone spans assert the default
state invariant in debug builds once per span; release scans therefore do not
perform a second state read for every block. Both optimizations leave the
column, descending-Y, short-circuit, and random-draw order unchanged.
For a stone span, the next ceiling is its bottom row. Using the row above it
changes the depth-below condition at the span boundary.

`surface::interior::InteriorCertificate` separately proves constant netherrack
output over Y=5..122 for a 0..127 generation window with netherrack as its default
block. Both stone depths must exceed `max(1, 1 + actual surface_depth)`.
The proof walks the compiled graph, resolving only stone-depth gates with zero
offset and zero secondary range, deterministic vertical-gradient endpoints,
and absolute Y gates without depth terms. It explores both branches of every
unknown predicate, including biome and noise conditions. Every reachable leaf
must emit netherrack; a missing result, band result, or other state rejects the
certificate. Dimension names and biome identities never stand in for this proof.

The scalar scan used by `NetherGenerator::surface_stage` and the packed in-place
scan consume the same certificate. For a stone span from `bottom` to `top` with
`above_before` preceding stone positions, the eligible bounds are
`max(5, bottom + threshold)` through
`min(122, top, top + above_before - threshold)`. Air resets the upper depth;
fluid retains it and separates lower-depth spans. A solid Y=16..111 span at
surface depth 3 therefore emits 88 constant results at Y=20..107 and evaluates
eight boundary positions normally. Negative surface depths retain the strict
one-block boundary through the maximum with 1.

Certified positions omit rule evaluation and biome callbacks, while emitting
every result in the original descending order. Even netherrack-to-netherrack
writes remain present in the sparse diff or packed state carrier, preserving
later palette and write-history inputs. The scalar callback still checks each
pre-state so non-default stone positions retain their original state. Nether
biome selection uses positional zoom and stateless climate searches; omitted
requests affect only request-local cache warmth. This certificate is distinct
from the Overworld preliminary-surface proof, which certifies no output.

## How to change it

Extend `RuleParser` and both evaluators together when adding a rule or
condition. Keep the fallback edge as the next rule in a sequence and add a
focused control that compares compiled and recursive results, including any
observable callback or random-draw order. Do not put mutable scan state in the
compiled graph.

Keep `RegionBiomeSidecar::uniform_resident_biome_at` conservative when changing
biome storage or zoom candidates. Every possible selected cell must be resident
before returning a certificate; consulting the dynamic table during this proof
would advance its tie cursor. Do not extend the certified bounds beyond the
shifted quart band without proving all newly admitted candidates. The
`uniform_resident_surface_biome` tests compare the production callback with the
zoomed control and predict one callback per four homogeneous heights versus one
per height for mixed-corner and missing-cell controls. The
`typed_surface_biome` tests cover negative-Y boundaries and context expiry with
and without Y updates.

Keep interior proofs conservative when adding conditions: a new predicate starts
as unknown, and both continuations must prove the same constant result. Extend
the domain only with matching graph and scan controls. The
`surface::interior::tests::nether_interior_certificate` tests cover exact span
bounds, basalt floor and ceiling boundaries, bedrock endpoints, fluids, mixed
biomes, negative depths, non-default stone, custom outputs, and the packed
consumer. A deliberately injected unsafe certificate must produce a detected
write-history mismatch against the ordinary scalar scan.

## Configuration

The graph and certificates are built by `SurfaceSystem::new` from the dimension
settings and reused for the lifetime of that system. On native targets,
`LODESTONE_DISABLE_SURFACE_INTERIOR_SPAN` disables constant interior spans for
controlled comparisons. Browser builds use the graph proof directly.

## Dependencies

The graph uses `SurfaceSystem`'s existing condition cache, density/noise
objects, biome and heightmap callbacks, and interned `StateId` results. The
surface parity fixtures remain the external output check. The ignored
`surface::tests::surface_kernel_profile_shaped_fixture` test provides a fixed
seed-42 shaped/full characterization with an exact SHA-256 output digest and,
on macOS, retired-instruction and cycle counters; run it with
`cargo test -p lodestone-worldgen --release --lib surface::tests::surface_kernel_profile_shaped_fixture -- --ignored --nocapture`.
