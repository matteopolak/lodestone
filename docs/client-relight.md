# Client-side relight

## What it is

The client's own light engine: a block change queues a relight, and once a frame the
client recomputes sky and block light in a bounded box around each change and re-meshes
what moved. It is vanilla's `LightEngine.checkBlock` plus `ClientLevel`'s
`runLightUpdates`, and without it a block broken on a real vanilla server leaves a
**permanently pitch-black hole**.

## Why it exists — the measurement

The reported symptom was "lighting when I break a block is extremely dark, but only on
non-integrated servers". Two causes were candidates: no client relight at all, or a
mask misreading in the `light_update` decode. **Only the first was real.**

**A real vanilla server does not send you a light update for a block you break.**
`ChunkHolder.broadcastChanges` sends the block change to
`playerProvider.getPlayers(pos, false)` — everyone tracking the chunk — but sends
`ClientboundLightUpdatePacket` to `getPlayers(pos, true)`, whose `borderOnly` arm is
`ChunkMap.isChunkOnTrackedBorder`: only players for whom that chunk sits on the **outer
ring** of their loaded area. A player standing in a chunk is never on that chunk's own
border, so the breaker gets the block and no light with it, ever.

Vanilla is fine because its client runs the same engine the server does.
`LevelChunk.setBlockState` calls
`level.getChunkSource().getLightEngine().checkBlock(pos)` when
`LightEngine.hasDifferentLightProperties(oldState, newState)`, and `LevelChunk` is
shared between both sides; `ClientLevel.tick` then drains the queue via
`pollLightUpdates` and `getLightEngine().runLightUpdates()`. **Server light packets are
a correction, not the mechanism.**

The mask decode was checked and is correct. `LightPatch::from_light_masks`'
`light_layer_from_masks` implements the three states properly — full-mask bit ⇒ replace,
empty-mask bit ⇒ explicit `Uniform(0)`, named by neither ⇒ absent from the patch so
`World::merge_light` leaves it alone — and the v770 adapter reads the four bitsets in
vanilla's `write` order (sky, block, empty-sky, empty-block) before reordering them into
the constructor's different argument order. Nothing there zeroes a section it should
leave alone.

Why it looked like a rendering bug rather than a missing subsystem: the mesher lights a
face from the cell the face *opens into* (`SnapshotLight::face_light`, matching vanilla's
`ModelBlockRenderer`), and an opaque cell stores light `0`. So the instant a solid
becomes air, its cell still holds `0` and every face now exposed to it renders at the
shader's dark floor. The integrated server hid this by relighting and pushing a
`light_update` about a tick later — singleplayer looked right *by accident of our own
server being generous*.

## How it works

Three pieces, in `lodestone-world` and `lodestone-shell`:

1. **The queue.** `World::set_block` and `World::set_blocks` record the positions they
   wrote (`World::queue_relight`), capped at `PENDING_RELIGHT_CAP`. Nothing recomputes
   light on the write itself: a `/fill` is 4096 writes under one lock, and a relight per
   cell inside the packet handler is exactly the frame stall vanilla avoids by batching
   onto its own tick.

2. **The engine**, `lodestone_world::relight` / `World::run_pending_relight`. Each drain
   groups the queue **by section** — positions sharing a `(x>>4, y>>4, z>>4)` become one
   job — and each job recomputes a bounded box:

   - The box is the changes' bounding box expanded by `AFFECTED_RADIUS` (15).
   - Its outermost one-cell **shell is fixed**: those cells keep their stored light and
     act as immovable sources. That is what makes a bounded recompute *exact*. Light
     decays at least one level per cell crossed, so a change can only alter cells within
     14 of itself (a cell 15 away would receive `15 - 15 = 0`), and every path from a
     source outside the box crosses the shell, whose stored value already sums up
     everything beyond it.
   - Only the interior is recomputed, and **from zero** — never from the stored value, so
     a block that now blocks light cannot leave a stale bright cell behind.
   - The result is **diffed** against the stored values and written back cell by cell.
     The diff is not an optimisation: writing every interior cell would expand ~26
     `Missing`/`Uniform` tags per column into 2048-byte arrays, where vanilla
     materialises only 2–7 sky and 0–10 block sections per chunk.
   - Sky light needs one extra rule because it is not radius-bounded in `y`: uncapping a
     shaft turns every cell down to its floor into a full-strength source, so the box's
     `y` range also spans the vertical run of transparent cells below each change
     (`sky_run_bottom`).
   - Openness is read off the box's own top shell rather than by scanning to the world
     ceiling, because **a cell holds sky light 15 if and only if it is a sky source** —
     propagation costs `max(1, dampening)`, so a cell that merely received light tops out
     at 14. Openness then descends by `open(y) = open(y+1) && dampening(y) == 0`, which
     is `ChunkSkyLightSources.isEdgeOccluded`'s scalar case.

3. **The driver**, `mesher::relight_changed_blocks`, an `Update` system in
   `FrameSet::Terrain` registered by `TerrainPlugin`. It takes the store's write guard,
   drains the queue, drops the guard, and re-meshes the sections the relight reported —
   budgeted at `LIGHT_DIRTY_SECTION_BUDGET` per frame. **The re-mesh half is not
   optional**: a relight that changes light and dirties no mesh changes nothing on
   screen. The block-update path's own dirty signal is not enough, because it covers only
   the 3×3×3 around the changed cell and it was serviced a frame before the relight ran.

### The server still wins

`World::merge_light` drops pending relights for the chunk it patches. So a real
correction arriving *before* we get to it is never overwritten by our own recomputation,
and one arriving *after* simply overwrites us — both orders end on the server's data. In
singleplayer the relight runs first (a frame is ~8 ms, the server's update ~50 ms) and
the server's patch then corrects it, which makes the integrated server a standing
cross-check on the client engine rather than a mask for it.

## Cost

Counters, not durations — a wall-clock figure on this machine gets attributed to the
wrong cause.

| case | jobs | cells recomputed | cells changed | sections dirtied |
|---|---|---|---|---|
| one block broken under a roof | 1 | 49,972 | 1 | 1 |
| a skylight rim widened (light moves in volume) | 1 | 51,894 | 162 | 4 |

`cells_visited` is `31 × height × 31`, where the height is shorter than 31 only where the
box clamps to the world. For comparison, `compute_column_light` over a whole column is
106,496 cells and `compute_column_light_with_neighbours` is 958,464 — so a break costs
roughly half a single-column recompute and about a twentieth of the neighbour-aware one,
and it is exact across chunk borders where the isolated single-column form is not.

`RELIGHT_CELL_BUDGET` (320,000 cells ≈ ten breaks) bounds one drain; jobs past it stay
queued for the next frame, so a `/fill` spreads across frames instead of stalling one.
`RELIGHT_JOB_CEILING` drops a single job above 1.2 M cells, which can only come from
uncapping a shaft hundreds of blocks deep; that case is counted in `Relit::dropped` and
warned about by the driver.

## Evidence

`crates/lodestone-world/tests/client_relight.rs`, 12 gates.

The expected value comes from **outside** the relight: every assertion is judged against
`compute_column_light_with_neighbours`, the from-scratch 3×3 flood. The two are
independent constructions of the same physical rule — a bounded box seeded from a fixed
shell versus a whole-neighbourhood flood from zero over a 48×48 field — sharing the
injected `LightProperties` and nothing else. That arm is in turn judged against the sky
and block light a real Mojang 26.2 server wrote into `.cache/mc/survival/world`, by
`crates/lodestone-world/tests/vanilla_light_oracle.rs`. So the chain runs vanilla →
full flood → incremental relight, and no link is our own encoder answering our own
decoder.

The discriminating scene is a **block broken under a solid roof with light arriving from
the side**. A block broken in open sky cannot separate the hypotheses: it comes out at 15
whether you relight properly or merely flood sky light downward. The fixture asserts its
own premise (`the_scene_puts_the_break_under_a_ceiling_with_only_lateral_light`) — the
cell above the break must hold a *partial* value, in `1..=14` — so a scene that drifted
into being open or sealed fails instead of passing vacuously.

Two negative controls, because one was thin. Skipping the relight after a single break
disagrees in exactly **one** cell of 18,944 (real, but easy to mistake for noise), so a
second control skips it after the volume change and requires **more than 100** sky
disagreements. Both print the bounding box of the disagreement: a count alone cannot tell
a thin residual across a room from one solid black block, and the second is the whole
reported symptom.

## How to change it, and the gotchas

- **`AFFECTED_RADIUS` is derived, not tuned.** Shrinking it puts the fixed shell on cells
  the change really does alter, and stale-bright artifacts then appear at the box
  boundary instead of at the broken block — much harder to attribute to this code.
- **A `Missing` sky section means 15, not 0.** Vanilla elides every sky section above the
  top populated one and `SkyLightSectionStorage.getLightValue` answers 15 there, while a
  genuinely dark section is sent as an explicit empty (`Uniform(0)`). Reading `Missing`
  as darkness blacks out everything above the terrain. This is the same rule the mesher's
  `SkyDefault` follows and the two must not disagree — the relight reads stored light
  through it and the mesher renders the result through it.
- **The props table must match the store's id space.** `relight_changed_blocks` picks
  `VanillaLightProps` (26.2's `lodestone_data::light_props`) when
  `TerrainMesh::column_source` is `ColumnSource::Streaming`, and the shell's
  `DemoLightProps` otherwise. Running the 26.2 census against the demo palette would not
  fail — it would light the demo world from an unrelated table.
- **Do not "fix" a darkness report by making our server send more.** We cannot change
  what a real server sends, and the integrated server already masks the defect.
- **Another writer needs `World::queue_relight`.** Not every client write goes through
  `World::set_block`; a path that mutates a `ChunkColumn` directly must queue the
  position itself, or the block it changed keeps the light of whatever used to be there.

## Configuration

No env vars. The tunables are constants: `AFFECTED_RADIUS`, `RELIGHT_CELL_BUDGET`,
`RELIGHT_JOB_CEILING` and `PENDING_RELIGHT_CAP` in `lodestone-world`, and
`LIGHT_DIRTY_SECTION_BUDGET` in the shell's mesher.

## Dependencies

- `lodestone_world::LightProperties`, injected — the engine holds no block registry.
- `lodestone_data::light_props` for the 26.2 dampening/emission census (live sessions);
  `lodestone_shell::blocks::DemoLightProps` for the offline demo palette.
- `lodestone_ecs::ChunkWorldWrite` / `ChunkWorld` for the store, and `TerrainMesh` for
  the re-mesh queue.
