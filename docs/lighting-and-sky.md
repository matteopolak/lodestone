# Lighting and sky

## What it is

The client's whole lighting and atmosphere model: per-corner smooth lighting and
ambient occlusion on baked block models, the vanilla lightmap curve that turns a
light byte plus time-of-day into a shading factor, the day clock feed that drives
it, distance and environmental fog, the sky pass (disc, sun, moon, stars, clouds)
and per-dimension/per-biome tint, and the client's own light-relight engine that
fixes a broken block's lighting without waiting for the server.

## How it works

### Smooth lighting and ambient occlusion

Two meshers exist and only one has this. `mesh_simple`/`mesh_greedy` (the packed
full-cube path — untinted, geometrically-a-cube blocks, and what `--headless`
drives) has its own separate, older AO implementation. `mesh_models` — what live
terrain actually calls, for stairs, slabs, fences, cross-plants and every tinted or
partial block — is vanilla's real `ModelBlockRenderer.AmbientOcclusionFace` port,
`quad_corner_sample` in `crates/lodestone-render/src/models.rs`.

Per vertex, it averages **AO** (four per-cell shade samples: `1.0` open, `0.2`
occluded — vanilla's darkest AO sample is never `0.0`) and **light** (the same four
cells' sky/block levels, except an occluding neighbour's value is replaced by the
centre's own once the centre is lit above a threshold, so a corner pressed against a
wall reads dim rather than pitch black).

Two predicates decide this and they are **not the same question**: AO shade asks
vanilla's `getShadeBrightness` (a *collision*-shape question — full collision cube
→ `0.2`, with real exceptions for glass/ice/mud/soul sand/snow layers), while smooth
light's occlusion substitution asks the ordinary rendering-occlusion predicate.
Conflating the two under one "does this neighbour occlude" boolean is wrong for any
block whose full collision shape doesn't occlude for culling — leaves, slime, honey,
spawners, grates — which is invisible on glass/ice (where the two happen to agree)
and shows up as a tree canopy that fails to darken underneath. The AO census this
needs comes from a jar-dumped per-state table (`lodestone_data::shade_brightness`,
one bit per state), not a hand-derived exception list layered on the collision-shape
table — the exceptions move states in **both** directions and a hand list misses
that.

Which cell a quad's light/AO ring is centred on is also a *per-quad* fork, not a
per-block rule: a quad with a `cullface` samples the cell that face opens into
unconditionally; an unculled quad samples its own cell unless its plane is flush
with the block boundary (or the block is a full collision cube), in which case it
samples outward. Cross-plant models (grass, ferns, cross-shaped saplings) fail both
of the "sample outward" conditions, so they light from their own cell — getting this
fork wrong reads as "some plants are black on one side next to a solid block",
because a 45°-rotated cross model's four quads bake to only two facings, so a
neighbour on one axis darkens two blades and a neighbour on the perpendicular axis
has no effect at all.

Whether a block takes the AO path at all is gated by the model's own
`"ambientocclusion"` JSON flag (default true, read once per block from the first
resolved model of a possibly-multipart state) and, in vanilla, by the block's light
emission being zero — **the emission half is not modelled** here (no per-block-state
light-emission table exists anywhere in this codebase yet), so a light-emitting
full-cube block still takes the smooth-AO path where vanilla would flatten it.

The directional face shade multiplied into the same slot as AO is a fixed constant
per face direction, not a diffuse dot product: `down 0.5, up 1.0, north/south 0.8,
east/west 0.6` (vanilla's `CardinalLighting.DEFAULT`; the Nether's own darker
variant is unported). Both AO and shade multiply in **gamma space** — the same
`srgb_to_linear(linear_to_srgb(rgb) * tint * shade)` convention the whole tint/shade
model in this codebase follows; doing it in linear pulls the factor toward `1.0` and
visibly washes the result out.

Remaining gaps: non-cube models use the same nearest-corner AO approximation rather
than vanilla's per-quad face-shape-weighted interpolation for a partial (not
full-face) quad; fluids carry no AO term at all, matching vanilla, which renders
fluid surfaces through a completely separate path.

### The light ramp

Vanilla's lightmap curve — `get_brightness(level) = level / (4 - 3·level)` — is
applied to the raw sky/block levels, **then** the sky half is scaled by the
time-of-day factor (`SkyFactor`, i.e. `sky_darken`, see below), the two channels are
combined, and the whole thing is finally lifted by vanilla's own inverse-gamma curve at its
gamma option's
default of `0.5` — never applied in the opposite order, and never skipped. A flat
20%-floor linear ramp (`0.2 + 0.8 * level`) shipped here for a long time; the curve
and the linear ramp agree exactly at both endpoints and diverge in the middle,
peaking near a factor of roughly 1.3–1.6× too bright around sky level 4–8 — which is
precisely why the divergence didn't show up in either a full-daylight or a
pitch-black gate. Vanilla's darkest floor is also **not** `0.0` — it seeds the
combine with the dimension's own `AmbientColor` (grey `0x0A0A0A` in the overworld,
so an unlit surface is `0.0935` of daylight, not black), which converges toward
negligible as light rises and is exactly why it survives every daylight-only gate
while being wrong at night.

`SKY_LIGHT_COLOR` is a genuine hue shift, not a brightness-only effect: vanilla
keyframes the sky half's tint from white to a light blue
(`NIGHT_SKY_LIGHT_COLOR = 0xFF7A7AFF`) on the identical keyframe ticks as the
darkening factor, and block light gets its own separate warm tint
(`BLOCK_LIGHT_TINT`) scaled by a fixed `BlockFactor = 1.4`, the two channels
**added** rather than `max`ed. This is now ported (`lightmap_color`/
`light_color_from_levels` in `lodestone-render`), replacing the older grayscale
`lightmap_term` scalar for terrain/entity/fluid shading — the colour is
algebraically recoverable from the existing darkening factor alone, since both
tracks share keyframe ticks and interpolation, so no new uniform lane was needed.
The retained scalar path is still used by GUI/particle callers that never
distinguish the two colours.

The packed full-cube path (`block.wgsl`, the demo world and headless gates only)
has the darkening clock but still runs the old simple ramp, not this curve — a
deliberate, scoped gap since that path never draws live terrain.

### The day clock (`sky_darken`)

`sky_darken` is the whole of "what time is it" for rendering purposes, computed
once from the server's own day-clock ticks and riding a spare lane of the shared
group-0 fog uniform (the model shader is already at wgpu's 4-bind-group floor, so
there is no room for a dedicated binding). It reaches both the terrain and entity
shaders through the identical lane on purpose — wiring one without the other makes
mobs and blocks disagree about the hour, which reads as a mob-specific bug.

26.2's clock-sync packet is a **map** of clocks, and it is usually empty: a full
sync happens once at join, one entry on an explicit `/time set` or rate change, and
an otherwise-empty map roughly once a second meaning "nothing changed, keep what you
had". The client therefore holds an anchor (`total_ticks`, `rate`, `at_game_time`)
and extrapolates forward off the packet's own elapsed-tick counter rather than
running a local tick loop; a rate of `0.0` means paused. Which of the (currently
two, `overworld`/`the_end`) registered clocks is *the* day clock is resolved from
the current dimension type's own `default_clock` registry entry, not a hardcoded
holder id — a data pack can reorder the registry, and even on plain vanilla the
lowest-holder-id fallback gets the End's own clock wrong (its default clock is not
holder 0). The keyframe timeline itself (`730 → 1.0, 11270 → 1.0, 13140 → 0.24,
22860 → 0.24`, linearly eased, wrapping through tick 0) replaced an older
per-version cosine curve in vanilla and is what `sky_darken_for_time_of_day` ports.

### Fog

Vanilla combines **two independent ramps** with `max`, each with its own distance
metric: an *environmental* term (spherical distance, from a dimension/biome's own
`fog_start`/`fog_end` attributes, or overridden entirely by water/lava/blindness),
and a *render-distance* term (cylindrical distance — `max(|xz|, |y|)`, not
spherical, which matters directly overhead a valley: a spherical metric fully fogs
a fragment a cylindrical one correctly leaves clear). Both terms are always
computed; a dimension with its own environmental fog still gets the render-distance
term too. The render-distance span is an absolute, capped width measured back from
the render-distance edge — `clamp(render_distance_blocks / 10, 4, 64)` — **not** a
fixed fraction of the view distance, which was this client's older, wrong
approximation (worse the *larger* the render distance, since a fixed-fraction band
grows with view distance where vanilla's stays capped at 64 blocks). Environmental
presets: the Nether is a short fixed `10..96` block haze regardless of render
distance; water ramps from the eye out to `min(32, view distance)`; lava is
near-opaque at `0..3`; the overworld and End declare no dimension-level fog
attribute but still carry the registered default `0..1024`, which is not inert — it
contributes a mild spherical mid-field wash neither of this client's older presets
drew at all.

The fog mix itself happens in **gamma space** (`mix` over the raw texel byte,
matching vanilla, which is not colour-managed anywhere in this chain) — mixing in
linear light pulls the result toward the fog colour and is worst exactly where the
fog factor is smallest, the "too foggy too early" symptom. The sky disc's own colour
mix is a separate, still-linear-mixed code path (harmless there because the disc's
rim reaches full fog exactly where the two spaces agree). The fog *colour* itself is
still a flat renderer constant (`SKY_COLOR`) rather than vanilla's own
render-distance-dependent mix of a haze colour toward the sky colour, which is a
known, deliberately unaddressed residual gap — fixing it touches roughly a dozen
pixel gates that hardcode the current constant as their background.

### The sky pass and air bubbles

A sky disc, sun, 8-phase moon, star field and a flat camera-centred cloud plane draw
in their own pass **before** the terrain pass. The single most load-bearing detail
is what that pass clears to: **the resolved fog colour**, not a hardcoded black —
the disc is a finite 512-block-radius plane that does not cover the whole screen, so
wherever nothing else paints (distant open ocean, an unmeshed chunk, past the far
plane), the clear colour is literally what the player sees. Vanilla's own sky pass
works the same way (a separate "clear" pass fogs to the same colour, before the sky
geometry draws at all). Whether the pass draws any geometry at all is a *whole-pass*
gate on the dimension's own `skybox` attribute (`Overworld` vs `None`) — a colour
alone cannot express "draw no sun", so before this landed the Nether had correct fog
and clear colour but still rendered the overworld's sun and clouds overhead. Cloud
opacity is gated separately, on the dimension's own cloud-colour alpha.

The gradient itself comes from fogging a flat-coloured disc radially from its
centre outward, not from vertex colours — `skyEnd` (where the gradient reaches full
fog) is the render distance in blocks, clamped, not the attribute's raw registered
default; getting that clamp wrong stretches the gradient 4× too far at a small
render distance. Sky, fog and cloud colours all come from real vanilla keyframe
timelines (vanilla's own byte-wise colour multiply and lerp helpers — gamma-space byte
multiplies and lerps, per
the project's standing colour-space rule, never linear). Void fog below the world's
negative build limit is a quadratic falloff over an onset range that depends on
whether the level is flat (`1.0` block) or not (`32.0` blocks) — getting the flat
case wrong either suppresses the fade entirely at a Nether/End floor near `y=0`, or
darkens a superflat world's own surface. Per-biome sky tint is now wired end to end
off the server's own registry-carried biome colours (never a hardcoded jar-derived
table, so a data pack's renamed or recoloured biome is correct automatically),
resolved by scanning *downward* for the nearest non-empty section from the player's
eye, since the section actually at eye height is very often air over open ground.
Clouds remain vanilla's flat "fast" mesh; the true voxel-extruded "fancy" mode and
its settings-menu toggle are unbuilt.

Air bubbles (the underwater breath meter) are a straightforward six-hop chain from
the entity metadata field to the HUD, with vanilla's own visibility rule: shown
whenever the eye is underwater **or** air is below max (an "or", not an "and" — the
row must keep showing while air refills after surfacing).

### Dimension-conditioned rendering

Sky-light-default policy (what an absent/not-yet-loaded neighbour's sky sample
should read as while meshing) follows the dimension type's own `has_skylight` field
off the server's registry data, not a level-name guess — the Nether and End are not
interchangeable here (the End genuinely has real per-block sky exposure; a level-name
fallback that lumped it in with the Nether rendered sky-lit End terrain artificially
dark). Fog colour, clear colour and the sky-pass gate all branch on the connected
dimension the same way. What is *not* yet decoded off the wire is the dimension
type's `attributes` map (the real per-dimension fog/sky/cloud/ambient hex colours,
present in the registry payload and currently dropped at decode) — every colour
preset in this doc is still a hand-transcribed constant rather than server-derived,
and the End's own sky-darkening behaviour (`sky_light_factor: 0.0`, which per
vanilla's own formula should leave every sky-only-lit block on the End pure black
regardless of overworld time) is deliberately left unwired pending a live-server
comparison rather than shipping a guessed sign.

A player's connected dimension is read off one accessor
(`Sim::dimension`/`ServerDimension`) that updates on both `Login` and `Respawned` —
a historical bug here let a portal trip (server-initiated, the client sends nothing
to trigger it) leave the *rendering* path's copy of the dimension stale while
terrain geometry and camera placement (driven by separate state) updated correctly,
reproducing the original too-bright-Nether bug specifically on traversal rather than
fresh login.

### Client-side relight

A real vanilla server does **not** send you a light update for a block you break —
only players for whom that chunk sits on the outer border of their loaded view get
a light packet, and the player standing in the chunk they just edited is never on
its own border. Vanilla's own client survives this because it runs the identical
light engine the server does, sharing the same chunk object; a server correction is
a cross-check on that local engine, not the mechanism. Without a client-side engine,
every block broken on a real (non-integrated) server leaves a permanently
pitch-black hole, because the mesher lights an exposed face from the neighbour cell
it opens into, and an opaque cell's stored light is `0` until something recomputes
it.

The engine (`lodestone-world`'s `relight` module) queues changed positions on every
world write, groups a drain by section, and recomputes a **bounded box** around each
group: the box is the change's bounding box expanded by a fixed radius, its
outermost one-cell shell is held fixed as immovable light sources (light decays at
least one level per cell crossed, so nothing more than that radius away can be
affected, and the shell already sums everything beyond it), and only the interior is
recomputed — always from zero, never from the stored value, so a cell that newly
blocks light cannot leave a stale bright value behind. The result is diffed against
storage and only the changed cells are written back. Sky light needs one extra rule
because it is not radius-bounded vertically: the box's height also spans the whole
open shaft below each change, since uncapping a shaft turns every cell down to the
floor into a full-strength source.

A relight never overrides the server: `merge_light` drops any pending relight for a
chunk it patches, so whichever arrives second — a real correction or the client's
own recompute — simply wins, and in singleplayer the relight (a frame, ~8ms) beats
the server's own tick (~50ms), so the integrated server's next correction is a
standing cross-check on the client engine rather than a mask for it. Cost is bounded
by three constants (a per-drain cell budget, a per-job ceiling above which a single
pathological job — an uncapped shaft hundreds of blocks deep — is dropped and
counted rather than allowed to stall a frame, and a cap on how many pending
positions can queue at once), so a large `/fill` spreads its relight cost across
frames instead of stalling one.

## How to change it, and the gotchas

- **The AO occluder census and the smooth-light occlusion census are different
  predicates answering different vanilla methods** — do not derive one from the
  other's exception list, and do not substitute a hand-written collision-shape table
  for the jar-dumped shade-brightness one; both directions of disagreement are real.
- **Which cell an AO/light ring centres on is a per-quad fork** (cullface vs.
  unculled-and-on-boundary vs. unculled-off-boundary), not a per-block rule — a fix
  aimed at "the AO ring" that ignores the light-position fork will leave cross-plant
  models lit from the wrong cell.
- **Gate expectations for the light ramp must never call the code under test's own
  curve function** — every existing gate writes the curve out by hand from vanilla's
  formula, which is what actually distinguishes the retired linear ramp, the real
  curve, and the ambient-floor-dropped hypothesis; a gate importing the production
  function would be `decode(encode(x))`.
- **`sky_darken`'s `0.0` sentinel means "never wired", and every shader/caller reads
  it as full daylight** — a caller that forgets to install the source renders at
  noon forever rather than at the historical (now-fixed) 20%-floor midnight.
- **Fog's environmental and render-distance ranges must never be collapsed into one
  pair** — water/lava fog deliberately keeps its range in the render-distance slot
  rather than the environmental one, because several call sites structurally compare
  fog settings and relocating it would flip which term wins the shader's `max` for
  every existing caller.
- **A frame-average pixel check cannot see a ramp, gradient or fog-onset defect** —
  both the correct and the wrong hypothesis are `0` near and `1` at the edge with
  nearly identical means; only sampling by screen location (and printing a bounding
  box on failure) separates them.
- **Client relight and the server's own light patches must never race destructively**
  — a writer that bypasses `World::set_block`/`set_blocks` must call the relight
  queue itself, or the block it changed keeps stale light forever; conversely,
  merging a server patch must always drop any pending relight for that chunk.
- **A dimension-conditioned value read anywhere must go through `Sim::dimension`**,
  not a fresh copy of the "current dimension" chain — three separate consumers each
  grew their own copy of that lookup before, and each carried a different, silently
  stale fallback.

## Configuration

- No player-facing options for AO, the light ramp, or client relight (`AO_OCCLUDED`,
  `SMOOTH_LIGHT_MIN_CENTRE`, `BRIGHTNESS_FACTOR` are vanilla-derived constants, not
  meant to be tuned); `BRIGHTNESS_FACTOR` is vanilla's own gamma option's **default** value —
  wiring a real brightness slider means threading it through the shared uniform's
  two remaining free lanes.
- `Config::render_distance` is the only input to the overworld fog ramp.
- Sky/fog/cloud constants come from the decompiled 26.2 dimension-type and biome
  JSON, not from any env var; only the (still-undecoded) registry `attributes` map
  would make them server-controlled.
- `lodestone-world`'s relight tunables (`AFFECTED_RADIUS`, `RELIGHT_CELL_BUDGET`,
  `RELIGHT_JOB_CEILING`, `PENDING_RELIGHT_CAP`) and the shell's
  `LIGHT_DIRTY_SECTION_BUDGET` are compile-time constants; `RUST_LOG=light=debug`
  gives a per-job signed breakdown of what a relight actually changed.

## Dependencies

- `lodestone_render::light` — the single Rust authority for the lightmap curve and
  colour combine, duplicated verbatim into `model.wgsl`/`entity.wgsl`/`fluid.wgsl`
  (WGSL has no `#include`).
- `lodestone_render::models`/`block_models` — the AO/smooth-light corner math and
  the per-state census accessors.
- `lodestone_data::shade_brightness` — the jar-dumped AO occluder census.
- `crates/versions/26.2` — `DayClock`, the `set_time`/registry decode that resolves
  the day clock and dimension-type attributes.
- `lodestone_world::relight`/`LightProperties` (injected — the engine holds no block
  registry itself) and `lodestone_data::light_props` for the live 26.2 census.
- The decompiled 26.2 client source under `.cache/mc/26.2/client-src` for every
  constant and formula this doc cites — reference only, never linked by line number.
