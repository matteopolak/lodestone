# Distant terrain LOD (the rd-512 mesh tier)

## What it is

A design and staged implementation plan for a Distant Horizons-class far-terrain tier: a
world-anchored surface pyramid (heightfield + baked surface colour + water surface, at
halving resolution per band) that fills the horizon from the edge of the real-chunk near
field out to render distance 512 chunks, drawn by a dedicated heightfield pipeline. It is
the follow-on that `docs/plans/progressive-chunk-generation.md` names and deliberately does
not attempt: that plan is generation-side and honestly tops out at rd 64; this one answers
whether 512 is reachable at all, and at what fidelity.

The owner's framing, verbatim:

> for 512 chunks, we need to be extremely efficient of memory. take insights from eg jpeg
> compression or whatever for this (or newer formats) to see what we can do. and yes we
> will need distant horizons-style mesh LOD and whatnot to compress chunks and mesh data
> massively

This plan was written read-only against the tree shortly after `a024ed20`. Every "exists
today" claim was verified against source, not quoted from another doc. Doc status
annotations are the highest-decay content in this repo — re-verify before executing.

**Headline, derived below: rd 512 is reachable.** The full-horizon far tier costs
**~32 MiB CPU + ~32 MiB GPU residency and an estimated 4–8 MB on the wire** — against the
generation plan's measured/extrapolated **~32 GiB server residency, ~195 GB client mesh,
~58 GB wire** for real chunks at 512. The price is fidelity, itemised honestly in the
fidelity ledger: surface shape, colour and water only; no per-block detail, no canopy
geometry beyond the chunk-derived inner band, no caves or overhangs at distance.

---

## Verified facts this design rests on

- **A per-column height query already exists and never generates a chunk.** The private
  `preliminary_surface_level(sample_x, sample_z)` in `crates/lodestone-worldgen/src/surface/mod.rs`
  is our own port of the same early-estimate technique real generation uses for surface rules:
  it evaluates the `preliminary_surface_level` density router for one (x, z) and
  returns a surface Y. It is exactly the seam this design needs, and — like the generator
  stage seam the progressive plan found — it exists but has no public name. Real generation
  uses this same estimate for surface rules, so its error against the real surface is bounded by
  construction, but **the bound is unmeasured here** (aquifers, carvers and surface rules
  all act after it). Stage 0 measures it.
- **Biome and sea level are queryable without generation**: `OverworldGenerator::biome_at_quart`
  and `OverworldGenerator::sea_level` are public today.
- **The packed near-field quad is 72 B** — `lodestone_render::vertex::vram_bytes` prices an
  indexed quad at 4 × 12 B (`PackedVertex`, compile-asserted) + 6 × 4 B indices. The model
  path's `ModelVertex` (`crates/lodestone-render/src/models.rs`) is the 152 B/quad figure's
  source. Mesh VRAM at rd 8 is ~67 MB live (residency, measured) ⇒ ~186 KB/column; linear
  extrapolation gives ~227 MB at rd 16 and ~830 MB at rd 32 (both residency, extrapolated).
- **Server residency is 31.1 KiB/column packed** (measured flat across an 8.9× range,
  `docs/chunk-store.md`), of which ~24 KiB is block storage. This is the number "abandon
  block identity" is up against.
- **The terrain shader has bind-group headroom.** `block.wgsl` uses **two** groups
  (group 0: `Camera` + `Origin`, group 1: atlas + sampler + sprite UV table); only
  `model.wgsl` sits at the four-group floor. The `Camera` uniform already carries the
  frame's fog (`fog_eye`, `fog_color_start`, `fog_end_enabled`) precisely so pipelines
  cannot disagree about fog — a distant tier reusing group 0 inherits that property.
- **The background is `SkyFrame::clear_color`** — a time-of-day and eye-height resolved fog
  colour (`crates/lodestone-shell/src/gpu/frame.rs`), not a constant. Any far-tier pixel
  gate diffs against a rendered reference frame, never a hardcoded sky value.
- **Fog is per-dimension data-driven** (`visual/fog_start_distance` / `visual/fog_end_distance`
  attributes, `crates/lodestone-render/src/fog.rs`), with `fog_factor(distance, start, end)`
  as the single curve.
- **A custom-payload carrier exists in v770** (`CUSTOM_PAYLOAD` in
  `crates/protocol/v770/src/server_protocol.rs` and the adapter) — the wire vehicle that
  lets our client receive LOD tiles while a vanilla client, which ignores unknown plugin
  channels, receives none.
- **`flate2` is already a `lodestone-server` dependency** (zlib-rs backed) and that crate
  compiles for wasm32 — the entropy-coding backend needs no new dependency and no new
  wasm hazard class.
- **Depth is reversed-Z `[0,1]`, the same sense as vanilla**, and the terrain compare is
  `LessEqual`; a vanilla-positive depth bias is negative here.
- **Tint and shade multiply in gamma space** (`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)`;
  `crates/lodestone-render/src/biome_tint.rs` is the tint source). A far tier that bakes
  colour must bake in the same space or the seam will show as a brightness step.

---

## The owner's framing, taken and corrected

The transferable JPEG insight is real, but it is not the DCT, and adopting the DCT would be
adopting the wrong half. What transfers:

1. **Change representation before you entropy-code.** JPEG's biggest single win is chroma
   subsampling and quantisation — throwing away what perception does not weight — not the
   arithmetic coder. Our analogue is bigger than JPEG's: **at distance, block identity
   itself is the chroma.** A stored chunk column is 31.1 KiB because it answers "what block
   is at every (x, y, z)"; the horizon only needs "how high is the ground, what colour is
   it, is there water". One L1 cell (2×2 blocks) costs 8 B; a chunk column at L1 is 64
   cells = **512 B versus 31.1 KiB — a 62× reduction before any coder runs**, and 3,976×
   at L4 where a whole column is one 8 B cell. No transform achieves that; the
   representation change does.
2. **Energy compaction via prediction, not frequency transform.** Terrain heightfields are
   smooth, so a parent-predicted residual (upsample the coarser pyramid level, code the
   difference — the lossless-JPEG/PNG-filter/JPEG-LS family) concentrates energy exactly as
   a wavelet detail band does. In fact **the LOD pyramid with parent-predicted residuals
   *is* a Haar-style wavelet decomposition** — we get the wavelet benefit as a byproduct of
   the structure the renderer needs anyway, with tile-local random access and cheap
   incremental update (a block edit dirties one tile per level), which a global transform
   codec does not give.
3. **Progressive refinement.** JPEG's progressive scan maps directly: send the coarsest
   level's full-horizon coverage first (~1–2 MB estimated), then refine inward band by
   band as residuals against what was already sent. The player sees a complete horizon in
   seconds and it sharpens.

Where the DCT specifically is **wrong** for us: a heightfield is viewed edge-on at grazing
angles, where lossy coefficient quantisation becomes **silhouette wobble** — and the
silhouette is the one thing you see at 500 m. So the lossy budget goes where JPEG's does
not: quantise **colour** hard (RGB565) and **resolution** (the pyramid itself), keep
**height effectively lossless** (small residuals entropy-code to a few bits/cell anyway,
so losslessness is nearly free precisely because terrain is smooth). Ringing artefacts on
cliffs, DCT block boundaries in the skyline, and the loss of tile-local addressability are
what we avoid.

The owner's "distant horizons-style mesh LOD" half survives contact with the numbers
completely — it is the representation change in point 1, and it is what makes 512
reachable.

---

## The representation: a world-anchored surface pyramid

Four levels (more are cheap — each further octave is one more constant-cost level), each a
world-aligned grid of **64×64-cell tiles**. Level k has cells of 2^k blocks (L1 = 2 blocks
… L4 = 16 blocks). Each level stores full coverage out to its own outer radius — not just
its drawn annulus — because the coarser level is the finer level's wire predictor and its
draw-time morph target; the overlap costs ~25% and buys both.

**Per-cell record, 8 B (CPU residency; GPU mirrors it as textures):**

| field | size | notes |
|---|---|---|
| terrain height | u16 | stored as `y + 64`, range 0..=384 fits with headroom |
| water surface | u16 | `0xFFFF` = dry; sea/river surface height otherwise |
| surface colour | RGB565, 2 B | baked **in gamma space**, biome tint included |
| flags + spare | 2 B | material class (fog/sound-of-colour tweaks), canopy offset reserve |

Normals are not stored — the shader derives them from neighbouring height texels.

**Byte budget per level** (residency; near field N = 16 chunks assumed for the drawn-annulus
column — N is a tunable, see the seam story):

| level | cell | stored coverage (chunks) | drawn annulus (chunks) | cells stored | CPU residency | GPU residency (R16Uint height + RGBA8 colour/flags + R16 water = 8 B/cell) |
|---|---|---|---|---|---|---|
| L1 | 2 b | 0–64 | 16–64 | 1024² = 1,048,576 | 8.0 MiB | 8.0 MiB |
| L2 | 4 b | 0–128 | 64–128 | 1,048,576 | 8.0 MiB | 8.0 MiB |
| L3 | 8 b | 0–256 | 128–256 | 1,048,576 | 8.0 MiB | 8.0 MiB |
| L4 | 16 b | 0–512 | 256–512 | 1,048,576 | 8.0 MiB | 8.0 MiB |
| **total** | | | | **4,194,304** | **32.0 MiB** | **32.0 MiB** |

Every figure above is **residency**. The constant-cost-per-octave property is the point:
doubling the horizon to 1024 chunks would add one 8 MiB level, not double anything.

**Draw cost (per-frame, not residency):** tiles are drawn by vertex-pull — one shared
static index grid (a 64×64 tile is 8,192 triangles; the shared index buffer is ~96 KiB
residency total), heights fetched with `textureLoad` in the vertex stage, so tiles have
**no per-tile vertex buffers at all**. Drawn-annulus tile counts: L1 240, L2–L4 192 each =
**816 tiles, ~6.7 M triangles before frustum culling; roughly 1.7–3.3 M/frame after**
(per-frame, estimated — measured in Stage 4, with a stride-2 far-tile index variant as the
fallback if it measures heavy).

**Wire (estimate, to be measured in Stage 0c):** parent-predicted residuals + zlib
(`flate2`, already in-tree). Smooth-terrain height residuals should code at ~2–3 bits/cell
and colour mostly as runs; the working estimate is **1–2 B/cell ⇒ 4–8 MB for the entire
rd-512 horizon**, with the L4 full-coverage base layer at ~1–2 MB. Even the **uncompressed
ceiling is 32 MiB** — three orders of magnitude under the 58 GB real-chunk wire — so the
codec is an optimisation, not a feasibility condition. Do not gold-plate it.

**Totals against the numbers to beat** (all residency unless labelled):

| | real chunks at 512 (gen plan, extrapolated) | this design at 512 |
|---|---|---|
| server | ~32 GiB | near-field chunks unchanged (139.2 MiB measured at rd 32) + 32 MiB pyramid |
| client mesh/GPU | ~195 GB | near mesh ~227 MB at N=16 (extrapolated) + 32 MiB LOD GPU |
| client CPU | — | + 32 MiB pyramid mirror |
| wire | ~58 GB | near field unchanged + 4–8 MB (estimate) |

The pyramid is **world-anchored and shared**: one dataset per world serves every
connection; per-connection state is only "which tiles sent at which residual depth".
Persistence is a per-level tile cache next to the region files — ~32 MiB per fully
explored 512-radius horizon, linear in explored area.

### Fidelity ledger — what 512 costs

What the far tier **keeps**: real terrain silhouette (mountains, valleys, river channels
carved by the density router), coastlines and ocean/river water at its real surface,
biome-correct surface colour (grass/foliage/sand/snow via biome LUT + `biome_tint`
machinery), day-night shading, fog continuity.

What it **loses**, by band:

- **Canopy geometry** beyond the chunk-derived region: query-based cells have no trees, so
  a forest reads as green terrain (colour is right, silhouette bumps are not). Where L1
  builds from real chunk data (below), canopy appears as the same blocky bumps Distant
  Horizons shows. A per-cell canopy height offset from a cheap vegetation-density noise is
  the reserved 1-byte extension if the owner wants tree silhouettes further out —
  measure-first.
- **Caves, overhangs, floating islands**: a heightfield cannot represent them; at 250+ m
  they are invisible anyway. The End's islands are out of scope for this tier (no oracle
  exists for End terrain either); the Nether's fog is short enough that a far tier buys
  nothing there — both dimensions simply do not enable it.
- **Player builds at scale**: an edited chunk re-renders into the pyramid from its saved
  data (below), so a build is visible **at the resolution of the band it falls in** — a
  tower at 60 chunks is a 2-block-cell spike with its real top colour; the same tower at
  400 chunks is one 16-block cell. Mountain-scale terraforming reads everywhere; a
  one-block pixel-art wall does not. This satisfies the owner's "see a structure they made
  that's really far away" constraint at the honest resolution distance implies.
- **Ores, light, block entities, entities, structures' interiors**: structurally absent —
  see "what is not worth compressing".

**The largest genuinely-good distance:** with L1 chunk-derived wherever chunks exist (and
the progressive plan's Shaped tier as its cheap source when that lands), 16–64 chunks
matches Distant Horizons quality. Beyond 64 the query-based bands are good for shape,
water and colour, and honest about canopy. If the owner finds colour-only forests
unacceptable at L2+, the canopy-offset byte is the lever; if block-fidelity at 512 is
demanded, this is the wrong design (see that section).

---

## Data sources, priority-ordered

A tile cell is built from the best source available, and the order is load-bearing (it is
the same disk-wins rule the progressive plan pins):

1. **Saved chunk data** (region files, including every edited chunk): downsample the
   stored column's motion-blocking surface — top solid block height, its block's baked
   colour, water surface. This is what makes player builds appear on the horizon.
2. **Resident generated chunks** (`ChunkStore`): same downsample, no disk read.
3. **The height query**: `preliminary_surface_level` + `biome_at_quart` + `sea_level` +
   biome→colour LUT. Never generates or loads a chunk; this is what makes 512 affordable.

Edits dirty the covering tile at every level (≤ 4 tiles per edit) through the existing
`ChunkSource::set_block` choke point; rebuilds are lazy and throttled. Tiles record which
source class built each region so a chunk-derived cell is never downgraded to query-derived
by a later rebuild.

---

## Rendering

- **New pipeline, new `.wgsl` file** (`lod_terrain.wgsl` under
  `crates/lodestone-render/src/shaders/`, `include_str!` like the other 13 — the
  no-inline-WGSL gate applies). **Bind groups: group 0 is the existing `Camera` + `Origin`
  layout, shared** — which answers the four-group constraint: fog comes along inside the
  camera uniform exactly as `block.wgsl` gets it, so near and far terrain structurally
  cannot disagree about fog or the clock. **Group 1 is per-tile**: height texture
  (R16Uint), colour+flags (RGBA8), water (R16Uint), plus a tile uniform (world origin,
  cell size, morph band). Two of four groups used; two spare.
- **Vertex-pull heightfield**: `textureLoad` in the vertex stage (core WebGPU, works on
  the browser backend), shared index grid, per-tile skirts (edge vertices extruded down)
  so cracks within and between tiles are backstopped by geometry, not luck.
- **Depth: partitioned passes.** The far tier draws first with its own projection
  (near' ≈ the hole radius, far' ≈ 17,000 blocks — a ~2×10² near/far ratio, comfortably
  inside f32 depth precision even without reversed-Z), then depth is cleared and the
  existing near scene draws over it. Correctness argument: every near-scene object lies
  within the near field, every far-tier fragment beyond the hole, so along any ray where
  both exist the near scene wins — which is exactly what "draw after clearing depth"
  produces, without a 16,384-block far plane ever entering the near projection. Sky,
  celestial and clouds belong to the far partition (they already draw first). The
  alternative single-partition route (stretch the existing far plane, depth-bias the LOD)
  is rejected: at [0,1] non-reversed depth a 16 k far plane concentrates precision loss
  exactly where LOD tiles meet, and the bias sign trap (vanilla-positive = ours-negative)
  is a standing hazard this repo has already paid for once.
- **Colour and shading**: baked gamma-space colour × the same day-night shade factor the
  near field uses, fog via `fog_factor` from the shared uniform. No block light at
  distance (sky-lit only); slope-derived normals give the terrain its read.
- **The hole and the crossfade**: LOD tiles fully inside the near field are skipped; in an
  overlap ring (hole radius = N − 4 chunks) LOD draws underneath real terrain and a
  screen-door dither fades it out where real mesh exists — no blend pass, no sorted
  transparency. Fog's terrain curve retargets from the near render distance to the LOD
  horizon (the per-dimension `visual/fog_*` plumbing already parameterises it).
- **Band morphing**: at each annulus boundary, edge-region vertices lerp toward the parent
  level's height (clipmap-style transition regions — this is the geometry-clipmaps shape,
  and the parent data is resident by construction because every level stores full
  coverage). Prevents both T-junction cracks and popping as annuli move with the player.

## The seam story, consolidated

Where this class of system visibly fails, and what this design does about each:

| failure | mitigation | cost |
|---|---|---|
| crack at the near-field boundary | overlap ring + LOD drawn under real terrain + skirts | overlap ring redraws ~(N²−(N−4)²) chunks' worth of LOD cells per frame — trivial |
| z-fighting in the overlap | depth partition: near scene drawn after a depth clear always wins | a second render-pass begin; measured in Stage 4 |
| colour step at the boundary | colour baked in gamma space from the same `biome_tint` sources; gate diffs mean band colour across the seam | a colour-LUT audit per biome |
| popping when chunks load/unload | screen-door crossfade over the overlap ring | none beyond the dither |
| popping at LOD band boundaries | clipmap morph regions against resident parent data | ~25% storage overlap (already counted) |
| skyline disagreement between query-derived LOD and later-loaded real chunks | Stage 0b measures the query's error; if p95 > tolerance over land, L1 falls back to chunk-derived | the go/no-go below |
| fog mismatch | one shared `Camera` uniform carries fog for both pipelines | zero, by construction |

---

## wasm32 story

- The tile builder is pure arithmetic over the density router + biome query — no clocks,
  no threads, no filesystem. Generation budgets are denominated in **cells per tick, a
  counter, not a duration** — which simultaneously satisfies the prefer-counters rule and
  keeps `Instant::now` out of a crate that ships to wasm32 (the clock confinement rules in
  `wasm-check.sh` apply; five crates are already banned from the clock paths).
- `flate2`'s zlib-rs backend already compiles in `lodestone-server` for wasm32.
- Browser singleplayer fills the pyramid lazily nearest-first with the per-tick cell
  budget; the horizon grows over tens of seconds (Distant Horizons behaves the same way).
  Nothing waits on it; the near field is unaffected.
- Streaming: one tile per `select!` pass, exactly the `ColumnPipeline` discipline. The
  unserviced window holds at most one tile encode — 4,096 cells, 32 KiB raw, prediction +
  deflate — estimated well under a millisecond native; the `LoopStallWatch` 200 ms
  threshold is the tripwire either way. A full-horizon stream is 1,024 tile sends,
  coarse-to-fine, so a play packet is never behind more than one tile. This is the
  columns-per-unserviced-window lesson applied at design time rather than discovered live.
- No new render passes are wasm-conditional; the depth-partition change is
  target-independent.

---

## Staged implementation plan

Each stage independently landable; each gate names its failing control. New counters
(tiles resident, pyramid bytes, tiles sent) get an **instrument-validation gate before
anything reasons from them**: pure camera rotation must move them by exactly zero — the
`vram_bytes` incident's cheapest discriminator, applied from day one.

### Stage 0 — measure before building (go/no-go)

**Owns:** an `#[ignore]`d harness in `lodestone-worldgen` (own test binary); no production
code.

1. **Cost** of one `preliminary_surface_level` evaluation (instructions retired and
   wall-clock alone on a quiet machine; the counter is the trusted one). Derive per-level
   pyramid build cost from it — 4.19 M cells total; at an illustrative 20 µs/cell that is
   ~84 s single-threaded for the whole horizon, which sets whether L3/L4 need
   sample-every-other-cell + interpolation.
2. **Accuracy**: query height vs the real generated `MOTION_BLOCKING` surface over ≥ 3
   census-asserted terrains (mountains, forest, ocean — the census guard refuses an
   all-ocean fixture, which would score every hypothesis alike; the
   ranking-metric hazard applies to fixture selection). Report an error histogram and p95,
   not a mean.
3. **Compression**: bytes/cell of parent-predicted residual + deflate on tiles built from
   **real terrain** (never synthetic smoothness — that is the *world*-species vacuity).
4. **Downsample cost** of the chunk-derived source on resident columns.

**Gate:** the census guards themselves. **Control:** run the accuracy diff with the query
arm replaced by a constant sea-level height and watch it report the large error — the
detector observed firing before any number is believed.

**Go/no-go:** if the p95 height error over land exceeds ~2 L1 cells, the query is not a
usable L1 source and L1 becomes chunk-derived-only (cost re-derived from item 4); if the
per-cell cost is 10× the illustrative figure, band radii shrink or L3/L4 interpolate. The
design survives both in degraded form; measuring first is what keeps the degradation a
decision instead of a discovery.

### Stage 1 — name the seam

**Owns:** `crates/lodestone-worldgen` only.

Public `OverworldGenerator::surface_sample(x, z) -> SurfaceSample { height, water_surface,
biome }` over the private `preliminary_surface_level`, `biome_at_quart`, `sea_level`.
Deterministic, pure, no clock.

**Gates:** golden comparison against generated-chunk heightmaps within the Stage 0
tolerance; byte-determinism across two constructions. **Control:** a perturbed seed must
fail the golden diff (proves the diff sees the terrain, not the tolerance).

### Stage 2 — the pyramid dataset and builder (server side)

**Owns:** a new `lodestone-lod` crate plus the `set_block`-adjacent dirty hook in
`lodestone-server`'s store layer. Not `server.rs`, not `tick.rs`.

World-anchored tile store (level, tx, tz → 64×64 cell records), the three-source builder
with its priority rule, per-level dirty tracking off the existing mutation choke point,
tile persistence beside the region files, and the residual codec.

**Gates:** (a) source priority — an edited saved chunk's build appears in the covering
tile at every level (assert the cell height/colour, a value not a flag); **control:**
neuter the disk consult and watch it fail naming the tile. (b) dirty-on-edit — one
`set_block` marks exactly the ≤ 4 covering tiles (a count with a verdict); **control:**
edit outside the pyramid's built region, count 0. (c) codec round-trip against **captured
tile bytes from a real world**, not self-generated fixtures (`decode(encode(x))` alone is
the closed loop the evidence rules forbid) — the committed fixture carries pairwise-
distinct field values so a transposition cannot survive. (d) the byte-budget gate: built
pyramid bytes per level land within the derived table above (a floor as well as a ceiling
— zero means the terrain vanished).

### Stage 3 — the wire

**Owns:** `lodestone-server` streaming (`server.rs` — a choke file; do not run
concurrently with other `server.rs` work) and the client's decode/apply in the shell's
net layer.

`lodestone:lod` channel over `CUSTOM_PAYLOAD`, opt-in negotiated at configuration; tiles
stream coarse-to-fine, nearest-first, **one tile per `select!` pass** through the existing
pacing; per-connection sent-set; residual layers keyed on what that connection already
holds.

**Gates:** (a) loopback byte-diff — the client-decoded tile equals the server tile,
compared as bytes over a census-asserted terrain; (b) the latency gate — a play packet is
serviced mid-stream during a full-horizon join (the unserviced-window class, bounded by
design at one tile); (c) a vanilla-protocol connection receives **zero** LOD payloads —
a count with a verdict. **Controls:** (a) with one residual layer dropped must fail the
byte diff; (c) with negotiation removed must go non-zero — observed failing, then
restored.

### Stage 4 — the heightfield pipeline

**Owns:** `crates/lodestone-render` (new shader, pipeline, tile texture management) and
the far-pass insertion in `crates/lodestone-shell/src/gpu/frame.rs`.

Everything in the Rendering section: vertex-pull, skirts, morph bands, depth partition,
shared group 0.

**Gates — all rasterised readback; a draw counter is never evidence** (the measured
59-draw-calls-zero-pixels incident), **and never a vertex-sampled probe** (blind to any
quad larger than the probe — these tiles are exactly that):

- (a) a known synthetic tile renders with pixel coverage inside its projected screen rect,
  and the failure output's bounding box **localises** (a degenerate or constant box is a
  broken transform — the `opaque_ink` lesson); **control:** the tile's height texture
  zeroed must change the readback (the detector fires).
- (b) crack gate: adjacent tiles from different levels along a morph boundary — zero
  background-coloured pixels in the seam column, diffed against a rendered no-LOD
  reference frame (never a hardcoded sky constant — `SkyFrame::clear_color` is resolved,
  not constant); **control:** skirts and morphing disabled must produce cracks, observed.
- (c) colour-seam gate: the same flat terrain rendered as real chunks and as LOD, mean
  gamma-space colour difference across the boundary band under threshold; **control:**
  bake the LOD colour in linear space and watch the gate fail — the gamma/linear
  signature (divergence large on dark, ~0 near white) printed in the failure output.
- (d) depth-partition gate: a distant LOD mountain visible above near terrain, and near
  terrain occluding LOD behind it, both asserted from the rasterised frame.

### Stage 5 — integration, the hole, and the slider

**Owns:** `crates/lodestone-shell` (`config.rs`, fog retarget, crossfade), docs.

Hole tracking against the live near field, screen-door crossfade, fog-end retarget, an
"LOD horizon" setting (near render distance stays 2–32; the horizon slider is new and
independent, so `MAX_RENDER_DISTANCE` is untouched — the progressive plan owns raising
it).

**Gates:** end-to-end pixel gate — join a real `IntegratedServer` world, horizon on vs
off: the horizon band gains coverage (readback diff against the off arm) while the frame
inside the near field stays byte-identical outside the crossfade ring. **Control:** the
horizon-on arm with tile streaming routed to `None` must equal the off arm — which is
also this feature's island detector: green build, plausible counters, zero pixels is the
repo's dominant defect, and this control is the one that catches it wired-but-dead.

### Stage 6 — the owner's sweep

**Owns:** an `#[ignore]`d GPU-gated harness; no production tuning constants land before
it runs.

Sweep near-field N ∈ {12, 16, 24, 32} × canopy-offset on/off × band radii, capture frames
and per-frame triangle/pass timings, and put the curve in front of the owner — his stated
preference is picking operating points interactively. The chosen defaults land as
constants with the measured curve in their doc comments.

---

## What would make this the wrong design

- **The owner wants block fidelity at the horizon** (real tree silhouettes, structure
  geometry at 512). Then the query-based tier is wrong and the answer is chunk-derived LOD
  everywhere — Distant Horizons' actual architecture — whose cost is generating ~1.05 M
  Shaped columns (hours of compute at the progressive plan's measured rates, plus that
  plan as a hard dependency). This design deliberately trades that fidelity for a
  three-orders-of-magnitude cheaper horizon; the fidelity ledger is where the trade is
  visible, and Stage 6 is where the owner rules on it.
- **Rejected: a JPEG/DCT-family transform codec** for the tiles — silhouette-visible
  quantisation artefacts, loss of tile-local random access and cheap dirty-update, and
  the measured-irrelevant gain: the uncompressed pyramid is already 32 MiB, so transform
  coding optimises a number that stopped mattering once the representation changed.
- **Rejected: compressing real chunks harder** (e.g., zstd over 1.05 M stored columns).
  Even a generous 10× on the 32 GiB leaves ~3 GiB residency, adds a decompress step in
  front of every read, and does nothing about the ~195 GB mesh problem — it compresses
  the wrong representation. Same verdict for cleverer palettes/RLE on full-detail
  sections: the near field already has them; the far field should not store blocks at
  all.
- **Rejected: sparse voxel octrees / DAG dedup** for the far tier. They preserve exactly
  the 3D structure (caves, overhangs) that is invisible at these distances, at a large
  cost in build complexity, streaming complexity and pointer-chasing draw paths.
  Revisit only for an End-islands tier, where a heightfield genuinely cannot serve.
- **Rejected: impostors/billboard clouds for the farthest band.** They pay when geometry
  is expensive; L4 is 6 MiB of annulus drawing ~1.6 M pre-cull triangles, which is not
  expensive. Parallax error on player movement and re-render churn buy nothing here.
  Kept as the escape hatch if Stage 4 measures the far pass heavy on low-end adapters.
- **Rejected: client-side generation from the seed** — breaks server authority for
  edited chunks (the exact case the owner names) and costs the browser build the most;
  same rejection the progressive plan recorded.
- **Deferred, explicitly not part of this design: near-field vertex compression.** The
  brief asks for the floor: per-quad instancing (one ~16 B instance record replacing 4
  vertices + 6 indices) is a real ~4.5× over the 72 B packed quad. But at 512 the near
  field is at most ~830 MB (rd 32, residency, extrapolated) of a budget the far tier just
  cut by three orders of magnitude — it is a good independent project and a wrong
  critical-path item. Filed as follow-on, not entangled here.

**What is not worth compressing, because it never reaches the far tier at all:** light
(sky-shading is recomputed from slope + time), ores and everything underground (the
surface query never evaluates them — they are not discarded, they are never computed),
entities and block entities, 3D biome structure, and scheduled/simulation state. The
cheapest byte is the one whose producer never runs.

## Where the risk actually is, ranked

1. **Query accuracy** (Stage 0b): `preliminary_surface_level` predates aquifers, carvers
   and surface rules in the pipeline, so rivers-below-prelim, badlands strata and carved
   coastlines are candidate divergences. If p95 error over land is bad, L1 degrades to
   chunk-derived and the horizon beyond 64 keeps only approximate shape. Cheap to
   measure, first in line, and the design survives it in degraded form — which is why it
   is risk 1 and not a go/no-go on the whole plan.
2. **Seam aesthetics.** Cracks, colour steps and popping are the failure class users
   screenshot. Every mitigation is named and gated, but "reads as broken" is ultimately
   the owner's eyes in Stage 6 — no unit test measures ugliness.
3. **Pyramid build throughput on wasm** (single-threaded, 10× slower debug): if Stage 0's
   per-cell cost lands high, the browser horizon fills in minutes, not seconds.
   Mitigations: smaller browser horizon default, L3/L4 interpolation, tile cache
   persistence (IndexedDB-backed storage is a separate open question the browser port
   doc owns).
4. **Depth-partition integration** — reordering sky/cloud/weather/near passes in
   `frame.rs` is choke-file work with a history of backdrop regressions; the Stage 4d
   gate and the Stage 5 byte-identical-inside-near-field assertion are the guards.
5. **The island** — the repo's dominant defect: a pyramid built, streamed, tested green,
   and never reaching pixels. The Stage 5 control (streaming routed to `None` must equal
   LOD-off) exists specifically to make that state fail loudly.
6. **Instrument drift** — new tile/byte counters lying the way `vram_bytes` did. The
   rotation-invariance gate lands with the counters, not after them.
7. **Codec cleverness creep** — the residual coder is 4–8 MB vs 32 MB on a wire that
   needed to beat 58 GB; any week spent past deflate is misallocated. The budget gate's
   ceiling keeps it honest in one direction; this note is the other.

**First measurement:** Stage 0's pair — the per-call cost and the height-error histogram
of `preliminary_surface_level` against real generated heightmaps on census-asserted
mountain/forest/ocean fixtures. It is the cheapest experiment in the plan, both top risks
hang on it, and every band radius, source-priority decision and wasm budget downstream is
parameterised by its two numbers.
