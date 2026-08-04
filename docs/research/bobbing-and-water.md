# Diagnosis: view bobbing and distant water blockiness

## What it is

Two read-only diagnoses. **View bobbing**: not a bug — issue #391's fix is intact and
unchanged in every relevant file; the report's own install simply has the option
persisted off, which correctly never auto-heals on its own. **Distant water
blockiness**: two causes, one already fixed (#389's air-vs-unloaded chunk-seam
conflation) and one newly found here — the singleplayer integrated server never pads
its `view_radius` by the `+1` ring vanilla's `ChunkTrackingView` always sends, so the
outermost ring's neighbour section never arrives and that ring's mesh (water and
everything else) stays permanently deferred.

Investigation is read-only. All line numbers are against the working tree as of
this session (`HEAD b065021`, plus the usual uncommitted item-variant work in
other files that does not touch anything below).

---

## Report 1: "view bobbing also seems to not do anything"

### Root cause

**Not an island. Not a wrong value.** The whole chain — menu click → persisted
option → `Sim::view_bob.tick` (every physics tick) → `Sim::bob_frame()` →
`bobbed_camera` → `Sim::render_camera` → the actual `render_with_effects`/
`render_with_crack_and_effects` call `app.rs` makes every frame — is fully wired
and matches vanilla's constants exactly. This is **already-fixed issue #391**,
and the fix is unmodified since it landed.

Chain, with citations:

- `crates/lodestone-shell/src/sim.rs:3090-3096` — `ViewBob::tick` is called once
  per 20 Hz `GameTick`, unconditionally (not gated on the option).
- `crates/lodestone-shell/src/sim.rs:5134-5147` — `Sim::set_view_bobbing`/
  `Sim::bob_frame()`: `bob_frame()` returns `BobFrame::default()` when the option
  is off, else `self.view_bob.frame(interp_alpha)`.
- `crates/lodestone-shell/src/sim.rs:5150-5189` — `Sim::render_camera` folds the
  bob via `bobbed_camera(self.camera(aspect), self.bob_frame(), 0.0)`, **not**
  gated on third-person (verified against `.cache/mc/26.2/client-src`'s
  `GameRenderer.java:534-536`, which has no camera-type check in 26.2).
- `crates/lodestone-shell/src/app.rs:1879` — `render_camera` computed every
  frame; `:2113` and `:2123` — it is what's actually handed to `render.render_*`.
- `crates/lodestone-shell/src/app.rs:1823` — `self.sim.set_view_bobbing(self.nav.view_bobbing())`
  pushed down every presented frame.
- `crates/lodestone-shell/src/menu/nav.rs:1699-1706` — `apply_settings` maps
  `SettingsOutcome::Cycle(LiveOption::ViewBobbing)` → `toggle_view_bobbing()`.
- `crates/lodestone-shell/src/menu/nav.rs:1832-1835` — `toggle_view_bobbing`
  flips `Options::view_bobbing` and persists immediately (eager persistence).
- `crates/lodestone-shell/src/menu/options.rs:1508-1515` — `SettingsNav::click_row`
  resolves a click to *that row's own control* (the #391 fix: it used to be
  `hover(row)` + `MenuKey::Enter`, which meant every click on the old
  single-screen settings page fired `Enter`'s hardcoded meaning, "toggle View
  Bobbing").
- `crates/lodestone-shell/src/menu/options.rs:658-665` — View Bobbing now lives
  on its own row (`Accessibility Settings... → View Bobbing`), paired with
  `notificationDisplayTime`, not sharing a row with GUI Scale (which is on the
  separate Video page, `options.rs:452`).
- Math: `crates/lodestone-shell/src/camera_rig.rs:384-608` (`ViewBob`,
  `BobFrame`) is a line-for-line transcription of `GameRenderer.bobView`
  (`.cache/mc/26.2/client-src/.../GameRenderer.java:323-329`), with unit tests
  pinning it against a hand-evaluated `P·B·V` to 1e-4 and a pixel gate
  (`crates/lodestone-shell/tests/view_bob_pixels.rs`) predicting +8.50 px /
  −3.50 px displacement of a rendered chest and asserting a byte-identical
  negative control at `bob == 0`.

**This is verbatim issue #391** ("View bobbing does nothing in game"), fixed in
`4909cd1 fix(menu): view bobbing was off because clicking GUI SCALE toggled it (#391)`
(2026-08-02 00:13:25). `docs/view-bobbing.md` records the whole investigation,
including this line: *"A persisted `false` cannot be told from a deliberate
choice, so nothing auto-heals it: anyone who hit this has to turn the option
back on once (or delete the key from `options.json`)."*

No commit since 4909cd1 has touched `nav.rs`, `options.rs`, `config.rs`,
`camera_rig.rs`, `sim.rs`, or `app.rs`'s bob-relevant lines (`git log --oneline
4909cd1..HEAD -- <those files>` returns nothing for `nav.rs`/`options.rs`/
`config.rs`/`camera_rig.rs`; `sim.rs`/`app.rs` have unrelated later commits).

### Live evidence gathered on this machine

`crate::menu::servers::data_dir()` resolves to
`~/Library/Application Support/lodestone` on macOS (`servers.rs:290-318`), and
that is a real, populated directory here:

```
$ cat "/Users/matthew/Library/Application Support/lodestone/options.json"
{
  "gui_scale": 5,
  "view_bobbing": false
}
$ stat -f "%Sm" ".../options.json"
Aug  3 13:53:24 2026
```

The key is only ever *written* when the option is off (config.rs's asymmetric
persistence, matched by `options.rs:626-662`'s test
`view_bobbing_defaults_on_and_only_writes_a_key_when_turned_off`), so this file
is direct proof the option is **currently persisted OFF** on this install. The
release binary that produced it is fresh and post-fix:

```
$ ls -la target/release/lodestone
-rwxr-xr-x 24810408 Aug  3 12:08 lodestone
$ git log -1 --format="%h %cd" HEAD
b065021 2026-08-03 12:04:47
```

i.e. the binary was built from current `HEAD` (~3 min after that commit landed)
and the option file was written **after** that build, by a fully-fixed client.
So the "toggling does nothing" symptom, if reproduced against *this* binary, is
not the pre-#391 wiring bug — the wiring is correct and unchanged.

### Island / wrong-value / missing-feature verdict

**None of the three, as currently wired.** This is the "persisted `false`
doesn't auto-heal" trap the fix's own doc already names. The most likely
explanation for the current report is that the player toggled the option (or it
was already off from before) and is looking at a session that is genuinely
running with View Bobbing off — which is *correct* behaviour, not a bug.

If a fresh session confirms the player explicitly toggled it to ON via
`Options → Accessibility Settings → View Bobbing` (not the old, now-nonexistent
shared-row path) in this exact binary and *still* sees no movement, that would
contradict this code review and would need a live GPU capture — nothing in static
review explains that outcome given how thoroughly `view_bob_pixels.rs` and
`sim.rs`'s `the_walk_bob_reaches_the_projection_at_vanillas_own_magnitude_and_axis`
pin the magnitude (not just the sign) of the effect.

### Minimal fix

No code defect found. Practical remediation for the player: turn View Bobbing
back on from `Options... → Accessibility Settings... → View Bobbing` (or delete
the `"view_bobbing"` key from
`~/Library/Application Support/lodestone/options.json`), then confirm.

If the team wants a code-level fix anyway (belt-and-suspenders, not required by
evidence gathered here): nothing to change in the bob pipeline itself. The one
legitimate follow-up already on record and *not* a fix for this report is
landing roll (`docs/view-bobbing.md`'s "How to change it" — `bobHurt`, the
walk bob's last 0.3° of roll, and `xBob`/`yBob`), none of which would explain
"toggling does nothing."

### How to (dis)prove this

- Gate that already exists and is dispositive if run live:
  `cargo test -p lodestone-shell --test view_bob_pixels -- --ignored --nocapture`
  (needs a GPU adapter + `client.jar`). Predicted values are hand-derived from
  vanilla's constants (`GameRenderer.java:323-329`), not from our own encoder:
  dip moves a rendered chest **+8.50 px** down (tolerance 1.5 px), sway moves it
  **−3.50 px** left, and a `BobFrame::default()` frame must be **byte-identical**
  to the unbobbed frame (the negative control — run it and watch it pass/fail,
  don't assume).
- Confirmatory step for *this* report specifically: read
  `~/Library/Application Support/lodestone/options.json` before and after the
  player toggles the row, and correlate with what they saw. If it reads `true`
  (or the key is absent) and the bob is still invisible, that is new information
  this review does not explain and needs the live oracle + a GPU capture, not
  more static reading.
- Negative control already in the suite:
  `sim::tests::walking_accumulates_a_real_bob_that_only_the_render_camera_sees`
  asserts `Sim::camera` (pick ray / audio listener) stays bit-identical while
  `Sim::render_camera` moves — proving the split is real, not just documented.

### What was ruled out

- Island: `Sim::bob_frame()` is read from `render_camera`, and `render_camera`
  is what `app.rs` hands to the GPU every frame (traced above) — not a case of
  "computed and never consumed."
- Menu → `Sim` disconnect: `set_view_bobbing` is called every presented frame
  from the correct nav accessor (`nav.view_bobbing()`), not the wrong live
  option, not a per-entity `ingest` mix-up (this is a local-player scalar, not
  event-routed at all — no `handles_event` arm is involved for this option).
  n.b. this option doesn't travel through any of CLAUDE.md's three routers; it's
  a direct field push, so the "island via a `_ => {}` arm" defect class does not
  apply here.
- Wrong constants: `camera_rig.rs`'s formulas were checked line-for-line against
  `GameRenderer.java:323-329` in `.cache/mc/26.2/client-src` and match, including
  the two "easy to get subtly wrong" details the module doc calls out
  (`bd` is an extrapolation, not a lerp; the nod's `−0.2` is inside the cosine in
  radians, not `(bd−0.2)·π`).
- A regression reintroducing #391's GUI-Scale/View-Bobbing row collision: GUI
  Scale now lives on the separate **Video** page (`options.rs:452`), View
  Bobbing on **Accessibility** (`options.rs:664`) — they can no longer share a
  row, and `click_row` resolves per-row regardless.

---

## Report 2: "some water far away is blocky"

### Root cause — two candidates, both concrete

#### Candidate A: issue #389 (already fixed, symptom is a near-verbatim match)

`docs/section-mesh-invalidation.md` documents **exactly** this report as its
motivating case: *"distant water is visibly blocky along chunk boundaries —
'you can see where the chunks are' — and it corrects itself as the player
approaches."* Root cause there: `snapshot_section_in` used to fill every absent
neighbourhood slot with an all-air section regardless of whether that neighbour
was the edge of the world or simply hadn't streamed in yet. Air doesn't occlude
and isn't water, so a section meshed while a real neighbour was still in flight
baked a spurious double-sided translucent seam face **and** a tilted/animated
top surface (a non-fluid neighbour drags `corner_height`'s weighted average
down — `crates/lodestone-assets/src/fluid.rs`'s `corner_height`).

Fixed in `4fceb73 fix(render): a chunk seam no longer bakes against a neighbour
that has not arrived (#389)` (2026-08-02 00:10:43), via a real
`Neighbour::{Present, Air, Unloaded}` distinction
(`crates/lodestone-shell/src/mesher.rs`, `Neighbour` enum) and a
`SnapshotOutcome::Deferred` that holds a section's *first* build back until its
neighbourhood is complete (`mesher.rs:1323-1348`, `route`), while
`Sim::on_column_arrived` → `TerrainMesh::mark_neighbours_dirty` re-drives any
section whose missing neighbour has since landed — which is how "corrects
itself on approach" is supposed to happen.

**Caveat directly in the record**: this fix's own doc lists, under "What was
*not* run when this landed" — *"No live oracle and no GPU gate... the seam is
argued from the two hermetic/jar-backed gates and from vanilla's source, not
from a screenshot."* It has never been confirmed against a real render.

#### Candidate B: a newly-introduced regression of the same invariant, in singleplayer specifically

While tracing what actually supplies chunks to the client (the precondition
#389's fix depends on — "every rendered section's neighbourhood is complete"),
I found that the **singleplayer integrated server**, added *after* #389 in
`75b91dd feat(shell): Singleplayer starts a real integrated server`
(2026-08-02 21:35:47 — ~21 hours after the #389 fix), does not reproduce
vanilla's neighbour-padding:

- `crates/lodestone-shell/src/app.rs:1289-1294` (`begin_singleplayer`):
  ```rust
  // Vanilla streams `simulationDistance`/`viewDistance` chunks around the
  // player; ours is the same number the camera's far plane and the mesher
  // already use, so the server never sends a column the renderer would
  // discard and never withholds one it wants.
  let view_radius = i32::try_from(self.config.render_distance).unwrap_or(i32::MAX);
  match launch_singleplayer(self.config.protocol, view_radius, session) {
  ```
  This `view_radius` is threaded unmodified through
  `launch_singleplayer` (`app.rs:736-750`) → `NetClient::open_singleplayer`
  (`net.rs:767-783`) → `Origin::Integrated { view_radius, .. }` →
  `IntegratedServer::open_in_memory` → `serve_connection` →
  `ViewTracker::new`/`recenter` (`crates/lodestone-server/src/server.rs:176-184`,
  `:200-...`), which sends **exactly** `[-view_radius, view_radius]²` columns —
  no buffer ring, at every call site I traced.

- Vanilla's real server sends one more ring than the client's view distance,
  specifically so every column the client actually renders has all 8 of its
  neighbour columns available. Verified directly against decompiled source:
  ```java
  // ChunkTrackingView.java:92, :96
  return this.center.x() + this.viewDistance + 1;
  ...
  return this.center.z() + this.viewDistance + 1;
  ```
  `docs/section-mesh-invalidation.md`'s "Why deferring the frontier costs
  nothing" section states this explicitly and treats it as a standing
  assumption: *"our deferral lands on the same ring vanilla also does not
  draw."* That assumption is **only true when the server providing the world
  pads by +1**, and the singleplayer server added after the doc was written does
  not.

- Consequence: in singleplayer, the outermost ring of chunks at
  `render_distance` from the player permanently lacks its one outward
  neighbour (the server will never send it — the client isn't asking, and nothing
  else requests it). Per `mesher.rs:1335-1346`, a `Deferred` section that was
  never previously uploaded is held back indefinitely (**not** queued for
  removal, so it also isn't obviously "missing" in the logs — `TerrainMesh::deferred`
  just keeps counting it). Because `SnapshotOutcome::Deferred` fires when *any*
  of the full 3×3×3 neighbourhood is `Unloaded`, this isn't fluid-specific — the
  whole outermost ring of sections (terrain, water, everything) never draws on
  first arrival, and only resolves for a given column once the player has moved
  close enough that *that* column is no longer the edge.

  Partial mitigation, worth being honest about: this ring sits almost exactly at
  the fog cutoff — `sky_fog_end_for_render_distance_blocks` clamps fog end to
  `render_distance * 16` blocks (`crates/lodestone-render/src/sky.rs:500-502`),
  the same distance in blocks as `render_distance` chunks — so the affected ring
  is heavily fogged, which may be why this reads as "blocky" rather than as an
  obvious hole: what's visible through the fog is whatever solid ring **was**
  already uploaded before it became the edge (correct), interspersed with newly
  streamed-in columns at the true edge that never draw at all (a gap, right at
  the point where fog makes a gap hardest to distinguish from "just foggy").

I did not find any other place that pads `view_radius`/sends an extra ring for
singleplayer, and multiplayer against a real server (including this repo's own
`.sh` oracles, which run the genuine 26.2 `server.jar`) is unaffected — real
servers already send the `+1` ring vanilla always has.

### Island / wrong-value / missing-feature verdict

- Candidate A: **was** a wrong-value/missing-distinction bug (air-vs-unloaded
  conflation), now fixed and unchanged in code since the fix landed. Live/GPU
  confirmation is still owed, not done.
- Candidate B: a **missing feature** relative to vanilla's own
  `ChunkTrackingView` padding, freshly introduced by the singleplayer feature
  and not covered by any existing #389 test (those tests exercise the
  mesher/snapshot logic directly with a hand-built `ColumnSource`/neighbourhood,
  never the actual chunk-streaming radius a real `IntegratedServer` connection
  uses). Not an island in the classic sense (something built and never called)
  — more a case of a fixed invariant (#389's "the frontier the mesher defers on
  is the ring the server also doesn't draw") being silently violated by code
  that landed after it and never re-read the invariant.

### Minimal fix

For Candidate B (concrete, actionable, low-risk):

`crates/lodestone-shell/src/app.rs:1293` — pad the singleplayer view radius by
one, mirroring vanilla's `ChunkTrackingView` exactly:

```rust
let view_radius = i32::try_from(self.config.render_distance)
    .unwrap_or(i32::MAX)
    .saturating_add(1);
```

This only changes how many columns the **integrated server** streams/tracks; it
does not touch `self.config.render_distance` itself, so the camera far plane
(`Camera::far_for_render_distance`) and fog end
(`sky_fog_end_for_render_distance`) are unaffected — exactly like vanilla, where
the extra ring "exists to be a neighbour, not to be drawn"
(`docs/section-mesh-invalidation.md`). The stale comment immediately above the
line (which currently asserts the server "never withholds one [the renderer]
wants" — no longer true post-#389) should be corrected in the same change, or a
future reader will re-derive the same wrong assumption.

For Candidate A: no code change indicated by this review; the fix looks correct
by static and hermetic evidence. The outstanding action is the live/GPU
verification the fix's own doc says was never run.

### How to prove it

For Candidate B specifically (this is the one worth a new gate — nothing today
exercises the actual streamed radius of a real integrated-server connection):

- **Expected value, from outside our code**: vanilla's own
  `ChunkTrackingView.java:92,96` — `maxX/maxZ = center + viewDistance + 1`. A
  correct implementation must send `2*(view_radius) + 3` columns per side after
  the fix (`-(radius+1)..=(radius+1)`), not `2*view_radius + 1`.
- **The gate**: spin up `lodestone_server::IntegratedServer::open_in_memory`
  with a small `render_distance` (say 4) over a `Complete`-classified... no —
  deliberately over a **streaming** world source so `ColumnSource::Streaming`
  applies, join, let the client's `ChunkWorld` settle, then assert: every
  section whose chunk column is within `render_distance` of the player has
  `TerrainMesh`'s per-key state as *uploaded* (not sitting in
  `TerrainMesh::deferred` indefinitely across repeated `mesh_column` passes with
  no further column arrivals) — i.e. a convergence assertion in the same shape
  as `section-mesh-invalidation.md`'s existing
  `a_seam_meshed_without_its_neighbour_converges_on_the_neighbour_present_answer`,
  but driven by the real `view_radius` the singleplayer path computes instead of
  a hand-built neighbourhood.
- **Negative control**: run the identical gate with today's unpadded
  `view_radius` (i.e. revert the fix) and confirm the outermost ring's sections
  are *still* `Deferred`/never-uploaded after draining the scheduler to a fixed
  point with no new columns arriving — this must fail to converge, in contrast
  to Candidate A's existing controls which *do* converge once the fix lands
  (because in those tests the neighbour eventually "arrives" by construction).
- Print the failing rect/column coordinates on failure (not just a count), per
  CLAUDE.md's "make failure output say *where*" — a bounding box on the section
  grid, not a percentage of columns deferred.

For Candidate A, the owed live check:
`scripts/live-oracles/terrain.sh` (:25580, real terrain) — join, walk to a
shoreline near the edge of render distance, and run
`cargo test -p lodestone-shell --test water_seam_convergence -- --ignored --nocapture`
against a real session if the harness supports pointing it at a live server (as
currently written it builds its own fixture; if it only exercises the hermetic
path, the "owed" screenshot is a manual one: watch a real distant shoreline
while walking toward it and confirm the blockiness present before this session
is now gone).

### What was ruled out

- The "known gaps" list in `docs/fluid-rendering.md` (five divergences from
  `FluidRenderer`): four are closed (up-face culling, back faces, `0.001`
  z-fight insets, overlay material closed in `lodestone-render` though not yet
  wired into the live shell mesher — `SnapshotFluidView::overlay_at` is still
  the default `false`). None of the four is distance-dependent — they'd show at
  any range, near or far, wherever their trigger condition (a solid ceiling, an
  open lake surface, a glass/leaf neighbour) occurs. **Ruled out as the
  "distant" mechanism specifically**, though the overlay gap is real and
  separately worth closing per that doc's own instructions
  (`crates/lodestone-shell/src/mesher.rs`'s `SnapshotFluidView` needs the
  `overlay_at` override quoted in that doc).
- The fifth, still-open gap (partial occluders — `dirt_path`/`farmland` banks
  not culling an `8/9`-high water face) is also not distance-dependent; it
  fires wherever such a bank exists, near or far.
- `mesh_fluids`/`bake_fluid`'s own math (`crates/lodestone-assets/src/fluid.rs`,
  `crates/lodestone-render/src/models.rs`) was not re-derived from scratch here
  — `docs/fluid-rendering.md`'s own "belief that turned out false" section
  already ruled out tint/shade and wrong-sprite theories for the *shoreline*
  bug, and nothing in this report's symptom (distance-dependent, heals on
  approach) points back at per-face UV/winding math, which would be
  range-independent.
- Not a mip/texture-filtering LOD effect: grepped for any distance-based mesh
  simplification or texture LOD in `lodestone-render`/`lodestone-shell` and
  found none — there is no simplified/low-detail terrain mesh at range in this
  codebase (`--headless`'s `mesh_simple` is a *different, non-fluid* mesher used
  only for hermetic/offline rendering, not a distance LOD of the live one).
- Not touched by any in-flight uncommitted work: `git diff` on
  `crates/lodestone-render/src/block_models.rs` (modified in the working tree by
  another agent) contains no fluid/water-related hunks — that work is the
  unrelated item-variant system.
