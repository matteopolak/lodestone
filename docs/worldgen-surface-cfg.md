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

`frontend26_3::MaterialBaker` resolves the current rule and condition holder
domains into typed operands and lowers them through this same continuation
compiler. Conditions retain one occurrence per graph site; resolving a named
holder does not silently introduce shared Y-cache identities. The current
material graph reuses the built-in biome bitsets and residual jump table.
Resource lookup, state validation and density-root deduplication happen at
loading. Its density roots are compiled together by the numeric binding;
their typed ordinals are not independent numeric programs.

An optional ore leaf has a fallback edge. Non-positive density or a first draw
greater than density produces no result; an admitted leaf emits ore or filler
and terminates the sequence. Richness is requested after the first draw,
gap only after the strict richness gate, and the raw-ore draw only after a
negative gap. Density and richness use prepared volume values for resident
positions, with scalar fallback outside; gap uses scalar sampling. The
request's `MaterialInputs` binding supplies these values and the positional
stream without resource lookup in the block walk.

Current material graph construction leaves basal, interior and deep-skip
certificates disabled. The older surface constructors cannot consume an ore
leaf without current numeric inputs. Wiring the material graph into dimension
scans and top-material lookups, supplying the current surface noise and
preliminary function, and retiring the independent vein override are separate
generator integration requirements; compiling this graph alone does not
establish current natural-world execution.

Typed position callbacks return `SurfaceBiomeAnswer`, which keeps its identity,
temperature predicate, and inclusive Y bounds private. The context checks those
bounds on every typed biome access, including accesses without a preceding Y
update, and refreshes an expired answer lazily. An uncertified callback uses
`SurfaceBiomeAnswer::exact` for one Y. Only `top_material_typed` supplies a fixed
answer valid at every Y for its isolated lookup. String biome answers and their
built-in identity memo still expire at each scanned position.

Typed column scans use a biome residual jump table over the same continuation
graph. Construction resolves chains containing only biome membership and its
negation for every built-in identity. The table contains one `u32` row offset
per original node and one compact destination row per biome node; it does not
clone rule graphs or retain coordinates. A scan reaches the original first
biome predicate before requesting its typed answer, then jumps across the known
biome chain. All other predicates retain their original nodes, operand order,
noise calls, and positional random draws. Each jump checks the answer's actual
Y bounds through the ordinary context resolver. String and extension inputs
use the original graph, and a path returning before its first biome predicate
does not request a biome.

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

`surface::basal::BasalCertificate` admits the stock Overworld four-arm outer
rule: the -64/-59 bedrock floor gradient, preliminary-surface guard, sulfur-only
biome guard, and 0/8 gradient emitting Y-axis deepslate. Every ceiling-depth
condition, including one inside a negation, must test only depth below <=1.
The condition proof matches all variants explicitly so adding a predicate
requires checking its depth dependency. Geometry must be -64..319 with stone
as the default state in both the system and its packed carrier.

The in-place scan consumes that proof only when the existing conservative
resident-biome mask excludes sulfur, the column's minimum surface level is at
least 9, and the original scan start is at least 8. It walks upper rows with
exact above-depth and water transitions. One lower neighbor distinguishes
below-depth 1 from a value of at least 2, which is sufficient for every admitted
ceiling predicate. Upper predicates still use the original compiled graph,
lazy caches, and demand points. The no-output middle and Y=8 are omitted;
Y=7..1 and -63..-60 keep their original positional gradient evaluations, and
Y=-64 keeps its deterministic floor result.

The constant Y=-59..0 output is deferred through one four-word column mask
(32 bytes per chunk) and an immutable result state. No per-Y cache or second
shape scan is added. `PackedStateCarrier::into_world` decodes only default-stone
codes in those certified columns during its existing materialization pass.
Its prepared vein batch still resolves first, and its fallback decoder applies
the certificate; air, fluids, and explicit surface states retain their own
decode. Ocean-floor observation receives the final resolved state. An outer
materialization branch leaves the ordinary uncertified inner loop unchanged.
Omitted basal biome requests are pure resident lookups, so their call count
and request-local cache warmth change. Column setup, initial column-biome
requests, upper demands, and retained boundary demands keep their order.
Custom graphs, low preliminary levels, and missing or unadmitted resident
contexts use the complete span walk.

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

Current resource changes belong in `MaterialBaker`; extend its typed conditions
and the request binding together. An optional leaf must preserve both the
no-result continuation and the first emitted result. The focused material
controls independently predict bedrock-before-ore priority, exact ore demand
and draw order, strict gap and raw-chance boundaries, compact default states,
holder-domain separation, load errors and mixed-precision discriminators.

`surface::residual::BiomeResidual` can prune only pure biome predicates. Keep
temperature and mixed predicates on the original graph. The `biome_residual`
controls predict 18 generic node visits versus two typed visits over sixteen
rejected biome branches, compare the actual production walk's demand and
noise/random predicate order across Y=5..6, and deliberately poison the new
band's destination to ensure stale selection produces a detected state mismatch.

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

Keep the basal outer-rule matcher and exhaustive below-depth dependency proof
conservative. Its controls compare the actual in-place scan and materialized
states with the same system's original span path, including fragmented stone,
air, both fluid codes, negative coordinates, gradient boundaries, retained
biome demand order, and fallback contexts. Forced depth-one evaluation of a
two-deep badlands ceiling selects red sandstone instead of red sand; forced
sulfur absence and early decoding before vein preparation must each produce a
detected publication mismatch. Do not rewrite packed shape codes before the
vein batch has been prepared.

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
controlled comparisons. `LODESTONE_DISABLE_SURFACE_BASAL_FUSION` disables the
Overworld bounded walk and deferred basal decode, retaining the original span
path. Setting `LODESTONE_DISABLE_SURFACE_DEEP_SKIP` also prevents basal admission.
Browser builds use the constructor proofs directly.

The material baker limits nested traversal to 128 levels and 65,536 visits,
including expanded holders. Generation-window anchors and integer operands
must fit `i32`; water and Y-gate surface-depth multipliers are in `[-20, 20]`.
Only omitted `is_3d` has a condition-field default, which is false. Density
documents remain load-time artifacts until the caller validates and compiles
their shared numeric arena. Material input bindings own sampling preparation
and choose no release at runtime.

## Dependencies

The graph uses `SurfaceSystem`'s existing condition cache, density/noise
objects, biome and heightmap callbacks, and interned `StateId` results. The
surface parity fixtures remain the external output check. The ignored
`surface::tests::surface_kernel_profile_shaped_fixture` test provides a fixed
seed-42 shaped/full characterization with an exact SHA-256 output digest and,
on macOS, retired-instruction and cycle counters; run it with
`cargo test -p lodestone-worldgen --release --lib surface::tests::surface_kernel_profile_shaped_fixture -- --ignored --nocapture`.
