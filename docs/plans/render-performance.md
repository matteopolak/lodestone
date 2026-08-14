# Plan: render performance — culling first, then submission

## What it is

The sequenced plan for making terrain rendering scale from the shipped render distance 8 to
16 and 32: frustum culling, vanilla's circular view-membership distance cull, the section
occlusion graph (vanilla's cave culling), draw-submission batching, and the translucency
ordering that culling changes the cost model for. Written 2026-08-07 against `297706a1`;
every number below was re-verified against the tree, the recorded diagnostics, or the 26.2
decompiled client — not inherited from the briefing, whose errors are listed at the end.

## Verified baseline

**Superseded 2026-08-14 — U1/U2/U3 have since landed and are wired into production.** Everything
below this note described the tree at `297706a1` on 2026-08-07. Commit `5cd65646` ("the section
occlusion graph reaches the draw loop, and translucent water is ordered back to front (U3, U5)")
changed the picture: `crates/lodestone-shell/src/gpu/frame.rs`'s `render_inner` now calls
`TerrainCull::classify` per section and increments real `RenderStats` counters —
`sections_drawn`, `sections_culled_distance`, `sections_culled_frustum`,
`sections_culled_occlusion`, `water_sections_drawn`, `water_sections_culled` — and
`crates/lodestone-shell/src/gpu/occlusion.rs` (new file, 195 lines) is the camera-walk consumer
side of U3, with its own `TerrainOcclusion` mode enum and cache. So the "no-cull claim" and the
"zero production consumers" framing immediately below are **no longer true for U1/U2/U3** — grep
`TerrainCull::classify` in `frame.rs` to re-confirm. **What is still accurate:** U5's *intra*-
section resort (`SortViewpoint`/`TranslucentMesh`) is still deliberately unwired — `frame.rs`'s
own comment at the water-draw site says so and explains why (water quads within one section are
near-coplanar, so the cross-section order was the part worth landing first) — so those two types
still have zero production callers outside their own tests. **U4 (draw-submission batching via
`section_arena`/`arena`/`driver`) is also still unlanded** — `grep -rn SectionArena
crates/lodestone-shell/src` outside test/bench code is empty. The rest of this document (unit
designs, constants, the units below) is otherwise still a reasonable read for U4 and U5's
remaining half; re-verify unit-by-unit before trusting a "not yet built" framing anywhere else in
it, per this repo's rule that status claims are the highest-decay content here.

**The no-cull claim was true, verbatim, as of 2026-08-07.** `crates/lodestone-shell/src/gpu/frame.rs` had three
loops that iterated **every resident section, one draw call each, with no frustum, distance or
occlusion test**:

| loop | pass |
|---|---|
| `self.sections.values()` | packed table (demo world only), in `render_inner` |
| `model.sections.values()` | live opaque terrain, in `render_inner` |
| `model.sections.values()` | translucent water, in `render_inner` |

Each visible-section draw costs ~4 encoder calls (`set_bind_group` with dynamic offset,
`set_vertex_buffer`, `set_index_buffer`, `draw_indexed`); water sections pay a second set.
Per-frame per-section uniform writes were already eliminated in earlier work: the only
per-frame per-section cost left **is** the submission itself, which is why culling and
batching are the whole plan.

**The recorded magnitude.** The deleted diagnostic's commit message (`45a93e4`) is the one
measured figure: *"model pipeline draws 931 sections with 441k quads"* at the default
render distance 8 (289–361 resident columns, per `docs/mesh-fill-rate.md`). That is
**~2.6–3.2 non-empty sections per column** (~474 quads/section) on default terrain — fully
solid underground sections and all-air sky sections mesh to nothing and are never uploaded
(`gpu/sections.rs`'s `upload_resident` drops empty geometry). Projections that matter:

| rd | streamed columns (square, `view_radius = rd+1`) | vanilla-drawn columns (circle, see U2) | projected terrain draws at ~3 sections/col |
|---|---|---|---|
| 8 | 361 | 257 | ~1.0k |
| 16 | 1,225 | 921 | ~3.5–4k |
| 32 | 4,489 | 3,461 | ~12–15k |

The briefing's 40,401 (9 sections/column) overstates by ~3× on default terrain; the
conclusion — submission scales quadratically with rd and is the dominant term — is
unchanged. Terrain with caves/mountains raises the density, so treat 3/col as a floor, not
a constant, and re-measure with the U1 counters before quoting any of these.

**The mesher is not the bottleneck** (`docs/mesh-fill-rate.md`, unchanged at HEAD): 1.000
mesh events per section, 0 of 26,168,839 frames with a full queue; post-`bdf93a28` fill is
~17 ms/column, linear. This plan does not touch meshing throughput.

**Adjacent known cost, out of scope here:** `Sim::refresh_stats` (`sim/step.rs`) still calls
`store.read().heap_bytes()` (a full resident-world walk under the read lock) every frame for
an F3 field, verified present at `297706a1`. Another agent may own it; nothing below depends
on it either way.

## What already exists — this is mostly a wiring plan, not a build plan

**Re-verified 2026-08-14 — three of these five bullets are stale in the direction of
understating progress; the "zero production consumers" framing is now wrong for the first three
items.** `visibility.rs`, `camera.rs`'s `Frustum` and `scene.rs`'s cull-classification shape are
consumed in production today through `gpu/frame.rs`'s `TerrainCull` and `gpu/occlusion.rs`
(commit `5cd65646`) — not necessarily by calling `WorldScene::plan_frame` itself (that
orchestration type may still be test/bench-only; `frame.rs` reimplements the wiring at the call
site rather than calling it directly, so re-grep `WorldScene::plan_frame` in `gpu/` before citing
it as the production entry point). `strategy.rs` and the arena/translucency items are unaffected
by that landing:

* **`crates/lodestone-render/src/visibility.rs`** — `compute_visibility` is vanilla's
  `VisGraph` (verified line-against-line with
  `.cache/mc/26.2/client-src/net/minecraft/client/renderer/chunk/VisGraph.java`: the
  `< 256`-opaque sparse shortcut, the fully-solid shortcut, flood-fill face connectivity;
  our union-find over open cells computes the same face-pair relation as vanilla's
  edge-seeded flood). `walk_visible` is the camera BFS with the never-reverse-axis rule.
  Unit-tested, no GPU. **Landed and consumed in production since `5cd65646`** — the
  per-section reachability this computes backs `gpu/occlusion.rs`'s camera walk, which
  `TerrainCull::classify` reads every frame in `gpu/frame.rs`'s `render_inner`.
* **`crates/lodestone-render/src/camera.rs`** — `Camera::frustum()` →
  `Frustum::section_visible(coord)`, Gribb–Hartmann planes for the `[0,1]` depth
  convention, conservative AABB test. `Camera::far_for_render_distance` (= `rd·16·4`) is
  already used by the live camera (`camera_rig.rs`) — the far plane tracks rd but at 4×
  the view distance it culls nothing resident; distance culling must come from U2, not the
  far plane. **U2 has since landed**: `RenderStats::sections_culled_distance` is a real
  counter incremented in `frame.rs`.
* **`crates/lodestone-render/src/scene.rs`** — `WorldScene::plan_frame` composes frustum ∩
  occlusion walk and returns `CullStats` whose invariant
  `drawable == drawn + culled_frustum + culled_occlusion` and `is_meaningful()`
  (drew something *and* culled something) are exactly the anti-vacuity shape the gates
  below reuse. **As of 2026-08-14, `plan_frame` itself still had no confirmed production
  caller** (re-grep before trusting this) — but the *behaviour* it composes (frustum ∩
  occlusion, culled/drawn counters) is now live in `frame.rs` via `TerrainCull`, whether or
  not that specific function is the call path. Do not read "this type is untested-in-prod"
  as "this capability is unwired" — they diverged here.
* **`crates/lodestone-render/src/strategy.rs`** — the answer to "what does wgpu on Metal
  actually support", already measured: wgpu 30 exposes **no** public
  `MULTI_DRAW_INDIRECT` feature; base `multi_draw_indexed_indirect` is gated on the
  `INDIRECT_EXECUTION` downlevel flag and **CPU-emulated as a per-draw loop on Metal,
  WebGPU and GL**; the only honest native-multi-draw signal is
  `MULTI_DRAW_INDIRECT_COUNT`, absent on this M5. `select_strategy` therefore returns
  `PerDraw` on Metal. This kills several "modern" candidates below. **Still accurate** — U4
  is unaffected by the U1/U2/U3/U5-half landing.
* **`crates/lodestone-render/src/section_arena.rs` + `arena.rs` + `driver.rs`** — shared
  vertex/index arena suballocation producing `DrawRegion`s (`first_index`, `index_count`,
  `base_vertex`), eviction with coalescing, and a full `WorldMesher` driver. Built against
  the **packed** (demo) mesh path, not the production `ModelMesh` path. **Still true, re-verified**:
  `grep -rn SectionArena crates/lodestone-shell/src` outside tests/benches is empty. U4
  (draw-submission batching) is the one unit in this plan that has not moved.
* **`crates/lodestone-render/src/translucency.rs`** — `SortViewpoint` (vanilla's
  `TranslucencyPointOfView` octant quantization, verified against the 26.2 source) and
  `TranslucentMesh` (centroid back-to-front resort). **Still consumed only by
  `tests/gpu.rs`, re-verified** — but only for the *intra*-section resort. The
  *cross*-section half of vanilla's translucent ordering (sorting sections back-to-front
  by distance) landed in the same `5cd65646` commit, directly in `frame.rs`'s water loop
  (`sort_back_to_front`, not via this module) — see that commit's own comment for why the
  intra-section resort was deliberately left for later (near-coplanar water quads make the
  cross-section order the one that produces the visible artefact). So this bullet's
  "both halves ... currently absent" is now half-stale: cross-section order is live,
  intra-section order is not.

So the plan is: carry these into the production model path in `gpu/frame.rs` /
`gpu/terrain.rs` / `mesher.rs`, in an order where every intermediate step is consumed on
screen.

## Constraints that shape (and kill) designs

1. **4 bind groups is the floor and the model shader spends all four** (camera+fog+origins /
   atlas / palette / anim). Every unit below is CPU-side or reuses existing groups; U4's
   origin-array option changes a *binding within group 0*, never adds a group. Note the
   constraint applies per pipeline layout — a compute prepass would get its own layout —
   so the honest reason GPU-driven culling is rejected is Metal's emulated multi-draw
   (strategy.rs), not the bind-group floor.
2. **Depth is `[0,1]` DirectX-style, not reversed-Z.** All culling here is CPU-side and
   depth-agnostic. Reversed-Z and a depth prepass are evaluated (and rejected) below.
3. **Vanilla parity is the premise.** Every cull must be a set vanilla itself does not draw
   (occlusion graph, circular membership) or provably pixel-identical. Sodium-style
   *visual* changes (fog removal options, LOD) are out of scope by charter.
4. **Counters, not durations.** Every expected win is stated as a draw/section/quad count
   with its derivation; wall-clock deltas on this machine reproduce to 10.8% and are not
   evidence.

## The units

Sequenced so nothing is an island at any step: each lands with its consumer, its gate, and
its control. U1→U2→U3 are strictly ordered; U4 and U5 are independent of each other and
come after U3.

---

### U1 — Frustum culling, CPU per section, inside `render_inner`

**Landed (`5cd65646`), re-verified 2026-08-14** — `RenderStats::sections_culled_frustum` is a
live counter in `gpu/frame.rs`. The design below is now a record of how, not a proposal.

**Where.** `gpu/frame.rs`: compute `camera.frustum()` once per frame (the machinery exists;
`prepare_entities` already frustum-culls entities — terrain is the only unculled geometry).
Guard all three loops with `frustum.section_visible(coord)` via a small
`SectionKey → SectionCoord` helper (`origin()/16`). Apply vanilla's
`offsetToFullyIncludeCameraCube(8)` equivalent (`SectionOcclusionGraph.offsetFrustum`):
translate the near plane back so sections straddling the camera never pop — without this,
the section you stand in flickers at cell boundaries, which is the classic
"correct-looking cull that fails at certain positions".

Not a compute prepass: with ~1k–15k resident sections, N conservative AABB-vs-6-planes
tests are microseconds of CPU, and Metal gives us no way to consume GPU-side results
without a per-draw CPU loop anyway (strategy.rs).

**Expected win (counter).** `sections_drawn` drops from *all resident* to *resident ∩
frustum*. At 70° vertical FOV / 16:9 (≈102° horizontal), a surface camera at a fixed
heading should submit roughly 30–40% of resident sections (horizontal wedge + conservative
straddle). Predict per-fixture exactly in the gate; in live F3, the invariant is the claim —
**per pass**, which is the correction item 2 below asked for:
`sections_drawn + sections_culled_frustum == resident_with_OPAQUE_geometry`, and a matching
`water_sections_drawn + water_sections_culled_frustum == resident_with_WATER_geometry`.
Written against a single `resident_with_geometry` it is simply false: `sections_drawn` is
incremented only by the opaque loop in `render_inner`, so a **water-only** section (`mesh:
None`, still issuing a water draw in the same function's water loop) appears in neither term — measured 189
`sections_drawn` against 195 uploads. New `RenderStats` fields, both pairs surfaced in F3 —
the on-screen consumer that keeps this unit from being an island even before U2/U3 land.

**Gate.** Headless GPU test (pattern: `gpu/pixel_gates.rs`): build 4 sections at hand-chosen
coords — dead ahead, dead behind, far left outside the wedge, straddling the near plane.
The expected visible set is **derived by hand from the camera geometry in the test's
comments** (outside expected value — not by calling `Frustum`, which is the code under
test). Assert `sections_drawn == 2` (ahead + straddler), `sections_culled_frustum == 2`,
and *both* hypotheses: the no-cull hypothesis (4) is computed and asserted `!=`.
Pixel half: the ahead section's texels cover its projected rect (rect derived from the same
`view_projection` expression the draw uses, not restated constants); render with no sky
installed and a clear colour distinct from the atlas texel so nothing else paints there.

**Control (must fail).** Same scene, camera yawed 180°: the coverage assertion on the
ahead-section rect must **fail** (readback collapses to clear colour) — run it and observe
the failure; this proves the pixel detector can see an absent section at all.

**Vacuity audit.** *Assertion:* both counters asserted against hand-derived values, not
`> 0`. *Magnitude:* both hypotheses computed (2 vs 4). *World:* the fixture must actually
contain out-of-frustum geometry — asserted by `sections_culled_frustum == 2`, which is 0 if
the fixture degenerates. *Precondition:* assert the 4 uploads succeeded
(`section_count() == 4`) rather than skipping.

**False-cull detection.** (a) The F3 invariant count — a section wrongly dropped still
appears in `resident_with_geometry`, so the split is visible, but a *frustum* false cull is
angle-dependent: add an **angle-sweep gate** — camera inside a closed 3×3×3 room fixture,
24 headings × 3 pitches, assert interior-wall coverage in the subject rect at every
orientation (a wrong plane sign or a bad near-offset fails at specific angles and nowhere
else). (b) The straddle fixture above is the known failure mode pinned as a permanent test.

---

### U2 — Distance culling: vanilla's circular view membership

**Landed (`5cd65646`), re-verified 2026-08-14** — `RenderStats::sections_culled_distance` is a
live counter in `gpu/frame.rs`.

**What the free cull actually is.** Not the fog bound the briefing proposed (see
Corrections). Vanilla does not render the streamed square: section membership is gated by
`ChunkTrackingView.isInViewDistance`, whose real expression lives in
`ChunkTrackingView.isWithinDistance` —

```java
long dx = Math.max(0, Math.abs(chunkX - centerX) - 1);
long dz = Math.max(0, Math.abs(chunkZ - centerZ) - 1);
return dx*dx + dz*dz < viewDistance * viewDistance;
```

— a **rounded circle with a 1-chunk buffer**, applied by `SectionOcclusionGraph.
getRelativeFrom`/`isInViewDistance` so out-of-circle sections are never drawn. Porting the
predicate verbatim is vanilla parity **by construction** and needs no fog argument at all.
Computed membership counts (exact, from the predicate):

| rd | streamed square | drawn circle | culled columns | % of resident |
|---|---|---|---|---|
| 8 | 361 | 257 | 104 | 29% |
| 16 | 1,225 | 921 | 304 | 25% |
| 32 | 4,489 | 3,461 | 1,028 | 23% |

This cull is heading-independent — it removes the square's corners even where the frustum
keeps them — and it composes with U1 multiplicatively.

**Where.** Same guard site as U1: `in_circle(camera_chunk, rd, section_chunk) &&
frustum.section_visible(...)`. `RenderState` already carries `render_distance_chunks`
(`gpu/state.rs`, set via `RenderState::set_fog`). New counter `sections_culled_distance`, added to
the same F3 invariant sum.

**Fog corollary (bounds check, not the mechanism).** The render-distance fog term is
cylindrical `max(|Δxz|, |Δy|)` with **end = rd·16** (`fog.rs`'s `fog_factor`); a
drawn column inside the circle has nearest-point cylindrical distance < ~(rd+1)·16·√2 only
at the buffer rim, and in practice everything the circle keeps starts before full fog — so
the circle cull cannot create a visible hard edge inside the fog ramp. That is the check;
the fog is not the cull.

**Gate.** Pure-unit half: port the predicate and assert the three table rows above (the
expected values are the Java expression evaluated independently — this doc's table was
computed from the Java, not from the Rust under test). GPU half: fixture with columns at
`(dx,dz) ∈ {(8,0),(9,0),(9,9)}` at rd 8 — expect drawn/culled/culled
(`(8-1)²=49<64`; `(9-1)²=64≮64`; corners well out). Both hypotheses asserted: no-cull
predicts 3 drawn.

**Control.** Set `render_distance_chunks = 40` in the same fixture: all three columns must
draw (the detector responds to the parameter, not to a hardcoded set).

**Vacuity audit.** *World:* the fixture's columns must actually be resident — assert
`section_count()` before culling. *Magnitude:* exact membership counts, both hypotheses.
The subtle one is *assertion*: `(9,0)` sits exactly on the boundary (`64 < 64` false) — it
pins the strict inequality, which is the transcription most likely to rot (a `<=` draws a
whole extra ring and no other fixture notices).

**False-cull detection.** Distance false culls are position-dependent, not
angle-dependent: the failure is a missing far column while walking. The F3 invariant plus a
gate that sweeps the camera chunk across a boundary (camera at chunk 0 then chunk 1;
membership of a fixed far column flips exactly when the predicate says) covers it.

---

### U3 — The section occlusion graph (the big one), in three landable steps

**Landed (`5cd65646`), re-verified 2026-08-14** — `crates/lodestone-shell/src/gpu/occlusion.rs`
(new file) is the camera-walk consumer, with `RenderStats::sections_culled_occlusion`,
`occlusion_active`, `occlusion_graph_sections` and `occlusion_walks` all live counters read from
`gpu/frame.rs`. Check which of the three landable steps below (Off/Shadow/On, per
`occlusion.rs`'s `TerrainOcclusion` enum) is the live default before assuming full landing —
the module supports a soak mode deliberately.

This is what makes caves cheap: standing on the surface, the frustum still contains the
entire underground column; only connectivity reachability removes it. Vanilla's shape,
confirmed from `SectionOcclusionGraph.java`: per-section face-connectivity computed **at
mesh time** (`VisGraph` → `VisibilitySet`), a BFS from the camera **decoupled from the
frustum** (reachability is recomputed only when invalidated; the frustum is applied
per-frame over the cached reachable set via the Octree walk), invalidation when the camera
crosses an **8-block (half-section) cell** or FOV changes (`SectionOcclusionGraph.invalidateIfNeeded`),
incremental re-propagation from changed sections
(`schedulePropagationFrom`), and the full rebuild ran **async** on a background executor.

#### U3a — producer + graph + shadow-mode counter (lands first, consumed by F3)

* **Producer.** In the mesh worker (`mesher.rs`), compute `SectionVisibility` for every
  meshed section via `compute_visibility` over an adapter from the worker's existing
  snapshot (`SnapshotModelView::occludes_at` is the same face-culling occlusion predicate
  family vanilla's `isSolidRender` feeds into `VisGraph.setOpaque`; erring toward
  "not opaque" only ever draws more — the safe direction). Ride it on
  `Meshed { key, mesh, visibility }`. Cost is bounded by the two shortcuts: most sections
  are sparse (<256 opaque → `all()`, no flood) or solid (no flood); the flood itself is
  once per remesh, off the render thread, and is noise next to meshing.
* **The empty-section requirement (the gotcha that would silently break the walk).**
  `walk_visible` stops at any coord missing from the graph, and all-air sections are never
  uploaded as geometry. **The graph must contain every section of every resident column**
  — empty sections as `SectionVisibility::all()` — or the walk dies at the first air gap
  above the terrain and the fallback (pure frustum) silently runs forever, which is a
  *world*-species vacuity for every downstream gate. If the mesher skips all-air sections
  entirely, populate those graph entries at column arrival from the store's cheap
  emptiness check. Removal: on `drain_removals` keys *and* on column unload for the
  never-uploaded empties (column-keyed sweep).
* **Consumer (what makes this not an island).** `RenderState` holds the graph; each frame
  (this step only) runs the walk and reports `sections_occlusion_would_cull` to
  `RenderStats` → F3, **without applying it**. Shadow mode is the soak: play sessions with
  the counter on screen while nothing can yet disappear.

#### U3b — walk fidelity upgrade + flip the cull on

* **Upgrade `walk_visible` to vanilla's node semantics before trusting it.** Verified
  divergence: our BFS visits each section once with a single entry face; vanilla
  **merges source directions on re-reach** (`existingNode.addSourceDirection(direction)`,
  in `SectionOcclusionGraph.runUpdates`) and passes a neighbour if *any* accumulated source
  face connects. Ours is stricter and can **over-cull** — a section reachable
  through face B but first visited through face C gets B's exits pruned. That is exactly
  the "geometry disappears at certain angles" bug class; fix it in `visibility.rs` first,
  with a unit fixture whose section is reachable only via the second-visited face (the
  current code must fail this test — that failure is the control proving the fixture can
  detect the defect).
* **Decouple frustum from reachability, vanilla-style.** Walk with `|_| true` (the
  never-reverse-axis direction masks already prune behind-camera expansion, as vanilla's
  `directions` byte does); cache the reachable set; apply U1's frustum per frame over it.
  Camera rotation then never re-walks.
* **Invalidation cadence (vanilla's, exactly):** re-walk when `floor(camera/8)` changes on
  any axis, on FOV change, and on any graph insert/remove (batched per frame). Synchronous
  at first: ~9k graph nodes at rd 8 is sub-millisecond; measure the walk with a counter
  (`occlusion_walks_per_second`, `graph_nodes`) before assuming rd 32's ~100k-node walk
  needs U3c.
* **Runtime escape hatch:** a debug keybind/flag to force `smart_cull = false` (vanilla has
  the same switch — spectator-in-wall disables it). This is both the live false-cull
  diagnostic (toggle it: if terrain reappears, the walk over-culled) and the A/B lever for
  counters.
* **Not ported:** vanilla's `MINIMUM_ADVANCED_CULLING_DISTANCE = 60` ray-march
  (also in `SectionOcclusionGraph.runUpdates`) — an *additional* aggressive cull for distant sections with measured
  false-cull history upstream (it is why vanilla caps it to >60 blocks). Skip it; the
  connectivity walk is the win, and the ray-march can be a separate later unit with its own
  soak if counters justify it.

**Expected win (counter).** Surface camera over default terrain: underground sections are
the delta. At ~3 non-empty sections/column with roughly 1 surface + 1–2 subsurface,
`sections_drawn` should drop by the subsurface share of the frustum∩circle set — predict
**30–60% fewer sections drawn** standing on the surface, and near-total (>90%) reduction
inside a closed cave looking at a wall. Both are hypotheses to pin with the shadow-mode
counter before flipping on; quote the measured split, never this paragraph.

**Gate.** GPU fixture: a sealed hollow stone shell (3×3×3 sections, walls thick enough that
`compute_visibility` of a wall section connects nothing — **asserted** on the wall
section's `SectionVisibility` directly, which is the world-species check that the fixture
actually occludes), camera inside, a distinctively-textured marker section outside the
shell. Assert: marker in `sections_culled_occlusion` (exact count, both hypotheses — the
no-walk hypothesis draws it), marker texel absent from its projected rect, interior wall
texels present (the frame drew *something* — `CullStats::is_meaningful`'s two-sided shape).

**Control (must fail, run and observed).** Open a one-section hole in the shell wall and
remesh: the marker must move to `drawn`, its texels must appear in the derived rect, and
the wall section's recomputed `SectionVisibility` must now connect the hole's face pair.
This is the same scene, same assertions, opposite expected outcome — it proves the pixel
detector, the counter, *and* the remesh-propagation path (a stale visibility that never
recomputes passes the sealed arm forever and fails only here).

**Premise check on the control** (this repo's five premise-false controls): before trusting
the marker-rect assertion, render the sealed arm with the marker section *removed* and
assert the rect reads background — i.e. nothing else paints in that rect. Cheap, and it is
the step that caught two premise-false controls elsewhere in this repo.

**Vacuity audit.** *World* is the dangerous species here twice over: (1) a scene with
nothing occluded exercises nothing — the sealed-shell connectivity assertion is the guard;
(2) the missing-empty-sections gotcha above makes the walk silently degrade to
pure-frustum — guard with an assertion that the camera's own (air) section is in the graph
(`graph.contains(camera_section)`), which is precisely the condition that separates the
walk path from the fallback path. *Duration:* the invalidation cadence means a gate that
renders one frame can pass while the re-walk-on-move path is broken — the gate must move
the camera across an 8-block cell and assert the reachable set changed
(walk-count counter increments).

**False-cull detection (live).** (a) the `smart_cull` toggle above — the one-keystroke
diagnosis; (b) F3 shows the three-way cull split, so "terrain missing + occlusion count
implausibly high" is readable at the moment of the bug; (c) the angle-sweep gate from U1
re-run with the occlusion graph active over a cave-mouth fixture (the mouth must stay
visible from every heading that can see it); (d) the source-direction-merge unit fixture
pins the known over-cull mode permanently.

#### U3c — (conditional) incremental / async walk

Only if U3b's counters at rd 32 show the walk exceeding budget (measure
`graph_nodes` × walks/sec first; a ~100k-node BFS a few times per second of movement may
simply be fine). Vanilla's two mechanisms, in order of cost: incremental re-propagation
from changed sections only (`schedulePropagationFrom` — cheap, no threading), then the
full-rebuild-on-background-thread with atomic graph swap (`scheduleFullUpdate`). Do not
build either speculatively; each is independently landable behind the same counters.

---

### U4 — Draw-submission reduction: arena suballocation for the model path

**What wgpu on Metal supports (measured, not assumed — strategy.rs):** no
`MULTI_DRAW_INDIRECT_COUNT`; base multi-draw is a CPU-emulated per-draw loop; `PerDraw` is
the correct strategy on this backend. So the reduction available is not fewer `draw_indexed`
calls — it is **fewer encoder state changes per draw and fewer buffer objects**:

* Suballocate every `ModelSectionGpu` mesh out of **two shared arenas** (vertex + index)
  per pass — the exact `section_arena.rs` pattern, generalized over the model vertex
  stride (its `DrawRegion` derivation is already stride-generic via `base_vertex`).
  `set_vertex_buffer`/`set_index_buffer` move out of the per-section loop: per-draw work
  drops from ~4 encoder calls to ~2 (dynamic-offset bind + draw), and thousands of small
  `wgpu::Buffer` objects become 4 (opaque + water arenas). Eviction/coalescing and the
  remesh-replaces-span path come with the pattern.
* Optional second step, only if the bind itself measures hot afterwards: replace the
  group-0 dynamic-offset origin bind with an origin **array** indexed by
  `instance_index` (draw with `first_instance = slot`, `InstanceTable` from `driver.rs`
  keeps slots dense). This changes a binding *inside* group 0 — still 4 groups. Verify
  non-zero `first_instance` on direct draws against the wgpu 30 Metal backend before
  committing to it (it is core for direct draws, but this repo checks limits, not
  assumptions).
* Stretch, explicitly deferred: coalescing adjacent visible regions into single
  `draw_indexed` ranges when their index spans abut in the arena (Sodium's region-batching
  analog). Requires allocation-order control; do not attempt until the arena unit has
  soaked.

**Expected win (counters).** `encoder_calls_terrain` (new stat): from `≈4×drawn` to
`≈2×drawn + 4`. Buffer-object count: from `2×resident sections` to `4`. `draw_calls`
unchanged by design — assert that too, so the unit cannot silently change *what* is drawn
(byte-identical pixel output is the gate: same scene, arena path vs per-buffer path,
readback equal — the strategy.rs "PerDraw is the correctness reference" discipline).

**Gate + control.** Pixel-identity A/B as above (both arms in one test binary, same
fixture); counter assertions on both hypotheses. Control: force an arena allocation
failure (tiny arena capacity in the fixture) and assert the degrade path drops exactly
that section with a log, not a panic — the same degrade contract `SectionOriginArena`
already documents. Vacuity: *world* — the fixture must have ≥2 sections so shared-buffer
offsets are actually exercised (`base_vertex != 0` asserted for the second).

**Why after culling:** the arena rewrite touches the same three loops culling guards; doing
it second means its A/B runs over the culled set (smaller diffs, and the pixel-identity
gate inherits the culling gates' fixtures).

---

### U5 — Translucency ordering (correctness the culling work exposes and bounds)

**Half landed (`5cd65646`), re-verified 2026-08-14** — the cross-section back-to-front order
below is live in `render_inner`'s water loop (`sort_back_to_front` over `TerrainDraw::sort_dist2`,
not `HashMap` order anymore). The intra-section resort
(`SortViewpoint`/`TranslucentMesh`) described next is still unwired, deliberately — see that
commit's comment on why the cross-section half was judged the one producing the visible artefact.

Verified current state as of 2026-08-07 (the cross-section half below is now stale, see above):
`SortViewpoint`/`TranslucentMesh` are implemented and match
vanilla's `TranslucencyPointOfView`, but production water never resorts (static index
order from mesh time) and cross-section draw order is **HashMap iteration order**
(in `render_inner`'s water loop) — both halves of vanilla's ordering are absent, which is a live visual
parity bug at some camera angles today, independent of performance.

* **Cross-section order:** sort visible water sections back-to-front by section-center
  distance each frame. Cost is `O(visible_water · log)` — bounded *because* U1–U3 shrank
  the set; this is the interaction that makes U5 sequence after culling.
* **Intra-section resort:** keep the water quad refs CPU-side (`TranslucentMesh`), resort
  on octant change and re-upload that section's index buffer via `write_buffer`. Cadence
  is vanilla's (`LevelRenderer.scheduleTranslucentSectionResort`): nearby sections when the camera block
  changes, plus a round-robin `max(visible/8, 15)` per frame over the rest — bounded by
  counter, never all-sections-per-frame (`translucency.rs`'s own module doc: "sorting
  every section every frame is unaffordable").

**Gate + control.** The existing `tests/gpu.rs` two-quad readback pattern extends to two
*sections*; the assertion shape is dictated by the ALPHA_BLENDING constraint (CLAUDE.md):
at full alpha, submission order alone decides the winner — byte-identical, no tolerance —
so camera-on-each-side must flip the winning texel exactly; mid-alpha assertions use the
anchored-distance shape (the mid-anchor row is load-bearing; a discard-then-overwrite
pipeline passes everything else). Control: with the resort disabled, the flipped-camera arm
must fail. Counter gate: resorts-per-frame `≤ nearby + ceil(visible/8)` with both
hypotheses (the naive resort-everything count computed and asserted `!=`).

**Vacuity audit.** *Duration:* one-frame gates cannot see the octant-cadence logic — the
gate must cross an octant boundary and assert exactly one resort fired (counter 0→1), and
move within an octant and assert none did. *World:* the fixture needs water in ≥2 sections
at different depths along the view axis or cross-section order is unexercised.

---

## Rejected, and on which constraint

| candidate | verdict | killing fact |
|---|---|---|
| `MdiZeroInstance` on Metal | **rejected** | wgpu-hal emulates base multi-draw as a per-draw CPU loop on Metal — strictly worse than `PerDraw` because culled (zeroed) draws are still CPU-iterated (`strategy.rs` module doc, measured) |
| `MdiCount` (GPU-driven submission) | **rejected on this target** | `MULTI_DRAW_INDIRECT_COUNT` absent on M5/Metal in wgpu 30; the selection logic already exists and will pick it up on Vulkan/DX12 hardware untouched |
| GPU compute-shader culling writing indirect args | **rejected** | its output is only consumable via multi-draw, which is emulated (above). Note: *not* killed by the 4-bind-group floor — a compute pipeline has its own layout — the honest reason is the submission side |
| Depth prepass | **rejected** | doubles draw calls, and draw submission is the measured bottleneck; on Apple-silicon TBDR the hardware already hidden-surface-removes opaque overdraw, so the fragment-cost win it buys on immediate-mode GPUs largely does not exist here; constraint 2 makes its `EQUAL`-compare second pass bias-sensitive for no measured gain. Re-openable only with a fill-rate measurement showing opaque overdraw cost |
| Reversed-Z | **rejected as part of this plan** | cross-cutting: flips every ported depth comparison and bias in the tree (each already sign-flipped once per constraint 2 — a second global flip is maximal churn); no observed z-fighting motivates it; if far-plane precision artifacts ever appear at rd 32 (far = 2048, near = 0.05), it becomes its own plan with an enumeration of every `DepthStencilState`, bias and `LessEqual` in `crates/` |
| Front-to-back opaque sort for early-Z | **rejected** | the win it targets (depth-test rejection of far fragments) is what TBDR HSR already provides; sorting the visible set adds per-frame CPU for no counter that can move. The *translucent* sort (U5) is a correctness need, not this |
| Instanced section drawing | **rejected** | instancing amortizes repeated geometry; every section mesh is unique. The instancing-shaped win here is the origin-array-by-`instance_index` option inside U4 |
| Vanilla's >60-block ray-march cull | **deferred** | additional cull on top of U3 with a known upstream false-cull history; revisit with U3's counters if the reachable-set size at rd 32 still hurts |
| LOD / reduced far detail (Distant Horizons-shape) | **rejected** | visual change; the client's premise is vanilla parity |
| A 5th bind group for anything | **rejected** | constraint 1; every unit above states how it stays at 4 |
| Fog-derived "fully fogged" cull | **rejected** | see Corrections: the premise mis-states the fog curve, and the pixels are not provably identical against the sky gradient; U2's circle predicate is the parity-correct superset of the honest version of this idea |

## What I verified vs assumed

**Verified against the tree at `297706a1`:** the three uncalled loops and their exact
lines; zero production consumers of `visibility.rs`/`scene.rs`/`strategy.rs`/
`section_arena.rs`/`translucency.rs` (tree-wide grep for producers, not one named file);
`RenderStats`/F3 wiring (`app/redraw.rs`'s `redraw`); `render_distance_chunks` reaching `RenderState`;
the live far plane varying with rd (`camera_rig.rs`); `heap_bytes` still per-frame in
`Sim::refresh_stats` (`sim/step.rs`); `SectionKey`/`Meshed`/`SectionGeometry` shapes and the empty-mesh-removal
behaviour; the mesh-worker's `occludes_at` being available off-thread; wgpu = "30"
in the workspace `Cargo.toml`'s `[workspace.dependencies]`.

**Verified against the 26.2 decompiled client (record definitions, not summaries):**
`VisGraph`'s 256-threshold and flood; `VisibilitySet`'s symmetric face-pair bitset;
`SectionOcclusionGraph`'s async full rebuild, 8-block invalidation grid, frustum/walk
decoupling, source-direction merge, frustum offset, and the >60-block ray-march;
`ChunkTrackingView.isWithinDistance`'s exact expression; `LevelRenderer`'s translucent
resort cadence; `FogRenderer`'s span/start/end (via `fog.rs`'s quoted source, cross-checked
against `docs/fog.md`).

**Assumed (marked hypotheses, to be measured by the units' own counters):** the ~3
non-empty sections/column density generalizes beyond the one recorded fill (it is
terrain-dependent); the 30–40% frustum share and 30–60% occlusion share; walk cost at
rd 32; that non-zero `first_instance` on direct draws works on this Metal backend (U4
checks before relying on it). Sodium specifics are cited from public documentation only as
direction (region batching, mesh-time visibility) — no Sodium figure is load-bearing
anywhere above.

## Corrections to the briefing

1. **The fog formula quoted is the fog *start*, not the end.** `rd·16 − clamp(rd·16/10, 4, 64)`
   is where fading *begins* (`FogRenderer.setupFog`, `fog.rs`'s `fog_factor`); geometry there is
   barely fogged. Full fog is at cylindrical distance ≥ **rd·16**.
2. **"Fully fogged ⇒ provably invisible" does not hold.** A fully-fogged fragment is exactly
   the fog colour, which matches the below-horizon clear and the horizon rim — but a
   silhouette poking above the horizon paints fog colour over the *sky gradient*, which
   differs; vanilla draws those silhouettes, so culling them is a parity break, not a free
   win. The parity-correct free distance cull is vanilla's own circular membership
   predicate (U2), which needs no fog reasoning.
3. **The section counts.** The recorded figure (`45a93e4`'s commit message) is **931
   sections / 441k quads** at default rd, i.e. ~2.6–3.2 non-empty sections per column —
   not ~9. "~509 sections" matches no record I could find and is presumably a live F3
   sample; rd 32 projects to ~12–15k terrain draws on default terrain, not 40,401. The
   quadratic conclusion stands.
4. **4,489 at rd 32 is the *streamed* square** (`view_radius = rd+1`); vanilla would draw at
   most the 3,461-column circle of U2 — the briefing's column count silently assumes the
   no-distance-cull status quo it is arguing against.
5. **The mesher-not-bottleneck figures check out**, with the standing caveat already
   recorded in `docs/mesh-fill-rate.md`: the old 761 ms / 15.2× numbers are wrong-multiplier
   and must not be requoted (the briefing didn't; noting because this plan cites that doc).
6. **`heap_bytes`**: still present at HEAD exactly as described; not planned around here.
7. **Not in the briefing at all, and the biggest fact in this plan:** the culling/
   submission stack the briefing asks to be designed **already exists in
   `lodestone-render`, tested and benched, with zero production consumers** — including a
   faithful `VisGraph` port and the measured Metal multi-draw verdict. The work is
   dominated by wiring and by the walk-fidelity gap (U3b's source-direction merge), not by
   new architecture.

## Measured baseline in instructions retired (added after the plan)

This plan says of its projected draw counts: *"re-measure with the U1 counters before quoting
any of these"*. The submission term has now been measured **before** U1 exists, so each unit's
expected win can be stated in instructions rather than only in section counts.
`crates/lodestone-shell/tests/client_chunk_cycles.rs` (`d7b823f6`), method and controls in
[`../client-chunk-cycles.md`](../client-chunk-cycles.md), record in `DESIGN.md` §12.120.

| quantity | measured |
|---|---|
| frame with no terrain resident | 2,055,154 instructions |
| frame with 189 sections drawn / 304 draw calls | 5,762,063 instructions |
| **marginal cost per section drawn** | **19,024–19,613 instructions** |
| extrapolated to `45a93e4`'s 931 sections | **17.7M–18.3M instructions/frame** |

Three consequences for the units above:

1. **The ordering is confirmed, not reordered.** Submission is **36×** `step.rs`'s
   `heap_bytes` term (490,238 instructions/frame at rd 8, now throttled in `f4e73530`) and
   ~160× the per-frame `Vec<ChunkPos>`. Culling first is right. Using the measured
   19,024/section against this plan's own cull fractions, **U1 alone is worth 10.7M–12.4M
   instructions/frame** — more than an order of magnitude beyond every per-frame F3 field
   combined.

2. **U1's stated invariant would not have held as written — now corrected above.** `sections_drawn`
   is incremented only by the opaque loop in `render_inner`, and a water-only section carries
   `mesh: None` there while still issuing a water draw in the same function's water loop — measured **189**
   `sections_drawn` against **195** uploads and **304** `draw_calls`. A single
   `sections_drawn + sections_culled_frustum == resident_with_geometry` therefore reads 189
   against 195 and looks exactly like a cull bug. U1's paragraph now states the invariant
   **per pass** (opaque and water each closing against their own resident set), so the gate
   cannot be built on the false premise. The 189-vs-304 gap is also, directly, U4's target
   quantified.

3. **Gate U1–U3 on section counts, not on instructions.** The per-column terms in that harness
   reproduce to 0.01–0.02% across processes; the submission term reproduces to only **3.1%**,
   because the Metal driver's own threads retire instructions asynchronously inside the
   measurement window. Section counts are exact; use instructions for magnitude.

**The plan's biggest miss, from the same measurement:** this document scopes itself to
submission and culling, which is correct for *frame* cost — but the client chunk path's
one-off cost is **96.3% meshing** (112,245,079 instructions per column, of which `mesh_fluids`
is 58.8% at 13,708 instructions per fluid cell). `docs/mesh-fill-rate.md`'s
"the mesher is not the bottleneck" is a statement about the mesher not being the *rate limiter*
for filling the view, and it is true; it does not mean meshing is not where the CPU work is.
Both statements hold, and this plan should not be read as implying the second.
