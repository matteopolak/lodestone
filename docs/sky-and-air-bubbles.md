# The sky pass and the air-bubble row

## What it is

Two features that landed as complete, tested, unreachable modules and were then
wired into the frame:

- **The sky** — a sky disc, sun, moon (8 phases), a star field and a cloud plane,
  drawn before the terrain pass. `crates/lodestone-render/src/sky.rs` is the pure
  half (time-of-day maths and geometry), `sky_pipeline.rs` the GPU half.
- **The air-bubble row** — vanilla's underwater breath meter, ported in
  `crates/lodestone-render/src/air_bubbles.rs` and drawn by the HUD.

They are documented together because they shared a failure mode, not a
subsystem: both were **islands**, individually green and reaching zero pixels.
That is the dominant defect class in this repo (`CLAUDE.md` rule 1), and these
were instances nine and ten.

## How it works

### Sky

`RenderState` holds `sky: Option<SkyRenderer>`, installed via `install_sky` and
fed by `set_time_of_day_source` — the same injected-closure pattern
`set_sky_darken_source` already used, so the renderer never learns about the
network. Both are installed at the **two** connect sites in `app.rs` that install
every other render source.

In `render_inner` the sky runs in its **own pass, before the block pass**, and
the block pass's colour attachment then becomes:

```rust
load: if stats.sky_drawn { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(self.clear) }
```

**Conditional on the sky having actually drawn, not on a renderer being
installed.** That distinction is load-bearing: an unconditional `Load` would
leave a headless or pre-install frame with no clear at all, which reads as
smeared history rather than as an obviously-missing sky — a much harder bug to
recognise.

Every sky pipeline sets `depth_stencil: None` and runs with **no depth
attachment**. This is deliberate and worth preserving: our depth is `[0,1]`
DirectX-style rather than vanilla's reversed-Z, so every ported depth comparison
flips sign. Having nothing to flip is worth more than having it right.

### Air bubbles

`airSupply` was decoded nowhere, so the chain is six hops:

`metadata.rs` (`IDX_AIR_SUPPLY = 1`) → `EntityMetadataUpdate::air_supply` →
`Vitals::air` (via `apply_local_player_air_supply`, registered after
`apply_entity_metadata`) → `PlayerSnapshot::air` → `Sim::air` →
`HudFrame::air: Option<(i32, i32, bool)>` → `sprite_vitals`.

The index is **verified, not assumed**: `Entity.java:260` defines
`DATA_SHARED_FLAGS_ID` (index 0) and everything between it and
`DATA_AIR_SUPPLY_ID` at `:268` is `int FLAG_*` constants, not accessors — so air
supply is the next accessor and index 1 is correct. A wrong metadata index reads
a different field entirely and produces plausible nonsense.

The GUI atlas needed **no** work: `GuiAtlas` globs `gui/sprites/**`, so
`hud/air`, `hud/air_empty` and `hud/air_bursting` were already stitched in. A
regression test in `gui_atlas.rs` pins that.

Visibility follows vanilla exactly — `Hud.java:910`:

```java
if (isUnderWater || currentAirSupplyTicks < maxAirSupplyTicks)
```

An **or**, not an and. The row stays visible out of water while air is below max,
which is what makes the gradual refill watchable after surfacing.

## How to change it, and the gotchas

**The `Clear`/`Load` handover is the fragile part of the sky wiring.** If you add
another pass before the block pass, decide explicitly which one owns the clear.
`stats.sky_drawn` is the signal; do not re-derive it from `self.sky.is_some()`.

**`sprite_vitals` lays out relative to a moving anchor.** `row_y` derives from
`cluster_top`, which starts at `b.h - margin` and is pulled up only `if
frame.hotbar` and again only `if frame.xp`. Any test or layout change that
assumes a fixed offset from the bottom will be wrong for some frames — see the
gate note below, where exactly that cost a false negative.

**The bubble `wobble` argument is always `false` today.** Vanilla samples
`tickCount % 2 == 0` (plus a second RNG coin flip) for a 0–1px jitter on a fully
empty row's last bubble. No per-frame tick parity is piped into `HudFrame`, so
this is deliberately unwired rather than approximated. Purely cosmetic.

**Deliberate sky omissions, so nobody reads them as bugs:** clouds are vanilla's
flat "fast" mode, not the 3-D voxel-extruded fancy mode; there is no
below-horizon dark disc; the biome's own `minecraft:visual/fog_color` is not
decoded, so only the disc *centre* is per-biome and the horizon end stays the
dimension fog colour (the sky tint itself landed — see
[below](#per-biome-sky-tint-issue-96s-fourth-box)); and the star field uses
splitmix64 rather than Java's RNG — same
distribution shape, different exact positions, a visual choice and not a
decode-parity claim.

## The gates, and what they cost to get right

Both features have pixel gates driving the real shell path, and **both gates were
wrong before they were right** — in ways worth recording, because each is a
general trap rather than a one-off.

`crates/lodestone-shell/tests/sky_pixels.rs` first asserted that a sky-less frame
clears *uniformly* to `SKY_COLOR`. It failed at 3.5%. A location report put the
offending pixels at `x221..255 y180..255` in dark browns: the **first-person bare
arm**, which `gpu.rs`'s hand pass draws whenever `third_person_body_drawn` is
false — i.e. always, in first person, with nothing installed. The control's
premise was false before the sky existed. The gate now measures inside the sky's
own screen rect, and `arm_is_what_we_excluded` pins the reason so the excluded
rows are a measurement rather than a magic number.

`crates/lodestone-shell/tests/air_bubble_pixels.rs` failed twice. First it
reported 0 px everywhere and looked like a dead chain — the rect had hardcoded
the *with-hotbar* `cluster_top`, so it was measuring ~20 logical pixels above a
row that was drawing perfectly. Then its control asserted that leaving the water
hides the row immediately; `Hud.java:910` says otherwise. The controls now
isolate vanilla's two disjuncts separately: full air + dry draws nothing, full
air + **underwater** still draws — which is what makes `eye_in_water`
demonstrably load-bearing rather than incidentally satisfied.

Both lessons reduce to the same rule, and it applies to gates as much as to bugs:
**ask where, not just how much.** A percentage cannot distinguish a
uniform-but-wrong frame from a localised blob, and this repo has a documented
case (`DESIGN.md` §12) of a frame average producing a confident wrong conclusion
that clustering by location immediately overturned. Both gates now print a
bounding box on failure for exactly that reason.

Measured, on this machine:

| gate | subject | control(s) |
|---|---|---|
| sky | 100% of the sky rect differs from `SKY_COLOR` | 0.0% with no sky installed |
| sky (day/night) | 97.6% near-black at midnight | 1.1% at noon |
| bubbles | 524 px underwater at 150/300 air | 0 px full-air-dry, **760 px full-air-wet**, 0 px `air: None` |

## Update: the sun/cloud regression (issue #24), and the gap that let it through

The sky pass reached the screen (the table above), but the real art was wrong
in three ways the user reported after actually playing: the sun was
oversized and solid black, the clouds were a black-fringed silhouette with a
gradient drop-off, and the clouds visibly teleported once a second instead of
scrolling. All three were real, and all three are now fixed.

**Why the original gates missed this.** `sky_pixels.rs` and
`sky_pipeline_gpu.rs` both built their `sky_manager()` from solid-colour
in-memory PNGs, deliberately, to prove the *wiring* reached pixels without a
`client.jar` dependency (see the section above). That meant **no gate had
ever loaded or sampled real vanilla celestial/cloud art** — the wiring was
proven, the art path was not, and the three defects lived entirely in the
part nothing exercised. This is the durable lesson: a synthetic pack that
substitutes solid colours for real textures cannot catch a bug that only
exists in the *shape* of real texture data (an opaque near-black falloff, a
hard binary alpha mask). `sky_pipeline_gpu.rs` now also has three
`real_jar_*` tests, `#[ignore]`d like the others, that load the actual
`.cache/mc/26.2/client.jar` and fail closed (never skip) if it or a GPU
adapter is missing.

### 1. The sun (and moon): wrong blend mode, not wrong size

`environment/celestial/sun.png` in the 26.2 client jar is a fully **opaque**
PNG — palette-indexed, no `tRNS` chunk, confirmed by walking its raw PNG
chunks — whose RGB is a near-black-to-bright-white radial falloff baked
straight in. Vanilla's `RenderPipelines.CELESTIAL`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/RenderPipelines.java`)
blends it with `BlendFunction.OVERLAY` — `(SrcAlpha, One)` for colour, i.e.
**additive**, with the destination left unattenuated — so that near-black RGB
only ever adds a sliver onto the sky. `CelestialPipeline` used ordinary
`SrcAlpha`/`OneMinusSrcAlpha` blending, which *replaces* the destination
wherever alpha is 1.0 — everywhere, in this texture — painting the whole
opaque 60-block-wide quad as a mostly-black square. That square being fully
visible (instead of only its small additive glow) is also why the sun looked
oversized: `SUN_SIZE = 30.0` was never wrong. It matches vanilla's own
half-extent exactly (`SkyRenderer.java`'s `modelViewStack.scale(30.0F, 1.0F,
30.0F)` applied to the same `-1..1` local quad `celestial_quad_positions`
uses) — only how much of the quad was visible changed. Fix:
`sky_pipeline.rs`'s new `CELESTIAL_BLEND` constant, `(SrcAlpha, One)` /
`(One, Zero)`, used by both `CelestialPipeline` and `StarPipeline` (vanilla's
`RenderPipelines.STARS` is the same `OVERLAY` function).

### 2. The clouds: linear filtering of a hard binary alpha mask

`clouds.png` is a hard **binary** alpha mask — every texel is either fully
transparent `(0,0,0,0)` or fully opaque white `(255,255,255,255)`, confirmed
by decoding the real file. `CLOUD_WGSL`'s fragment shader alpha-tests at
`0.04` and writes the sampled colour straight through with no blend (opaque
pipeline). Sampling that mask with **linear** filtering — this pipeline's
setting before this fix — produces a fringe of partial-coverage texels at
every cell boundary, whose colour interpolates proportionally from black
toward white; the ones that just clear the alpha threshold still carry
mostly-black colour, written as-is: the reported black rim, with the gradient
continuing toward full white just inside it. Fix: the cloud texture's sampler
is now `Nearest`, which can never return a partial-coverage texel (and reads
closer to vanilla's own per-*cell*, not per-pixel-sampled, mesh — see the
"deliberate simplification" note on `cloud_plane_geometry` above). Also
discovered along the way: vanilla's actual "flat" cloud mode does not sample
`clouds.png` per pixel in the GPU at all — `CloudRenderer.buildFlatCell`
builds one CPU-side quad per opaque texel/cell, uniformly coloured — so this
crate's single-textured-quad approach was already a bigger simplification
than its own doc comment claimed. Reproducing vanilla's cell-mesh approach
exactly is out of scope for this fix; `Nearest` filtering removes the visible
defect within the existing (documented, deliberate) simplified architecture.

### 3. The clouds: once-per-second teleport, not smooth scroll

`sky.rs`'s cloud scroll reads `time_of_day: i64` — the same day clock the
rest of the renderer uses, by design (no second clock). The problem is what
feeds that clock: `app.rs` polled `ClientHandle::world_time().1`, which is a
flat snapshot (`WorldTime` in `lodestone-ecs`) only overwritten when a
`SET_TIME` packet decodes (`ClientEvent::TimeChanged`), and the server sends
that roughly once per second (`docs/served-session-liveness.md`'s
`TIME_SYNC_INTERVAL`). `sky.rs::cloud_plane_geometry`'s `scroll_x = time_of_day
* CLOUD_SCROLL_BLOCKS_PER_TICK` therefore stepped once/sec — a visible ~0.6
block jump each time. **Vanilla's own `CloudRenderer.render`** computes its
scroll from `gameTime % (width * 400L) + partialTicks` — a continuously
advancing client-side tick count plus the render partial-tick, not a raw
server snapshot; this repo has an equivalent continuous value already
(`lodestone_ecs::FrameClock::interp_alpha`), but it is scoped to `Sim`
(`lodestone-shell/src/sim.rs`, a file this fix could not touch) and was never
wired to the sky's time source. Rather than plumb a new public accessor
through a held file, the fix stays local to `app.rs`: a small
`ContinuousTimeOfDay` helper anchors `(last_seen_tick, Instant::now())` and
extrapolates forward at the standard 20 ticks/sec between packets, re-
anchoring whenever a new packet arrives — the same predict-then-correct shape
vanilla's own client-side day-time uses. This is a minimal, self-contained
`app.rs` edit (one struct, two call-site changes at both connect paths); it
does not touch `sim.rs`, `gpu.rs`'s `TimeOfDaySource` signature, or any other
shared file, and is meant to be reconciled centrally if a broader continuous
clock lands later.

### Real-jar gate numbers, measured on this machine

`crates/lodestone-render/tests/sky_pipeline_gpu.rs`'s three `real_jar_*`
tests, run with `-- --ignored --nocapture`:

| gate | subject (shipped, fixed) | control (pre-fix, EXECUTED, same real art) |
|---|---|---|
| sun blend | 0.0% near-black | 28.9% near-black |
| cloud filter | 1.6% fringe (neither sky nor cloud colour) | 29.6% fringe |

The cloud control could not simply reuse the shipped 3-D camera placement:
looking straight up at the cloud plane from a normal player height put every
screen pixel in a *magnified* regime (many screen pixels per texel), where
even the pre-fix Linear filter's blended edge band is sub-pixel and
undetectable — measured as a literal 0.0% control on the first two attempts.
The control instead renders a hand-verified real boundary texel pair
(`clouds.png`'s texels `(4,23)` opaque / `(5,23)` transparent, confirmed by
decoding the file) magnified into view, isolating the filter-mode mechanism
deterministically rather than depending on where in-frame a real 3-D camera
happens to land relative to cloud blob edges.

**Not fixed, and not claimed to be:** the missing below-horizon dark disc, and
(at the time) the missing sunrise/sunset tint fan — pre-existing, documented
omissions, not part of that regression. The fan landed later; see the next
section.

## Update: the gradient, the sunrise band and void fog (issue #96)

Issue #96 asked for four things. Its body opened with *"nothing in this renderer
draws a sky dome at all today"*, which was true when written and false by the
time it was picked up — the dome, sun, moon, stars and clouds had all landed in
the meantime (the sections above). **The stale premise is the reason to record
the re-derivation, not a footnote to it.** What #96 actually still owed was:

| # | scope | status |
|---|---|---|
| 1 | horizon-to-zenith gradient, without banding | **done** |
| 2 | sunrise/sunset horizon tint band | **done** |
| 3 | void fog below the negative build limit | **done** |
| 4 | per-biome sky tint | **done** — see [below](#per-biome-sky-tint-issue-96s-fourth-box); the estimate below it of "a 4-file plumbing chain" was itself wrong, and how is recorded |

### Where vanilla's gradient actually comes from

Not from vertex colours. `SkyRenderer.renderSkyDisc` draws the disc a single
flat colour and `assets/minecraft/shaders/core/sky.fsh` then *fogs* it:

```glsl
fragColor = apply_fog(ColorModulator, sphericalVertexDistance, cylindricalVertexDistance,
                      0.0, FogSkyEnd, FogSkyEnd, FogSkyEnd, FogColor);
```

which with `include/fog.glsl` is `mix(sky_color, fog_color, clamp(dist/512, 0, 1))`.
`512` is `EnvironmentAttributes.SKY_FOG_END_DISTANCE`'s default. The disc sits at
`y = 16` with radius `512`, so its centre is at distance 16 (factor 0.031, pure
sky) and its rim at 512 (factor 1.0, pure fog). **That radial ramp is the
gradient**, and it is why the horizon end of the dome must be the *fog* colour —
`RenderState::render_inner` passes `self.fog.color`, not a second sky constant.

Two consequences worth writing down:

- The gradient is geometrically compressed into a few degrees above the horizon.
  A ray only reaches the fully-fogged rim at `atan(16/512) = 1.79°` of elevation
  and is at half fog by `3.6°`. That is not a bug and it is why the gate uses a
  30° vertical FOV rather than 90° — at 90° most pixels sit in the flat
  near-zenith regime and exercise almost none of the ramp.
- **The banding in #96's title is vanilla's own**, from computing the factor in
  `sky.vsh` — once per vertex, over ten vertices and eight triangles hundreds of
  blocks wide. `SKY_DISC_WGSL` interpolates the camera-relative *position* and
  takes `length()` per fragment instead. One `sqrt` per pixel, no banding, and
  strictly closer to the radial gradient vanilla is describing.

`apply_fog`'s second (cylindrical) term is **provably dead** for this geometry
and deliberately not implemented: `max(|xz|, |y|) <= sqrt(x²+y²+z²)` always, so
any fragment where its step at `SkyEnd` fires already has spherical distance
`>= SkyEnd`, where the first term is already 1.0.

### The timeline tracks, and a stale note that had rotted into a real divergence

26.2's colours come from keyframe tracks in
`.cache/mc/26.2/src/data/minecraft/timeline/day.json`, and `sky.rs` now ports
four of them — `SUNRISE_SUNSET_COLOR`, `SKY_COLOR`, `FOG_COLOR`, `CLOUD_COLOR`.
Things that are easy to get wrong and were checked rather than assumed:

- **`sunrise_sunset_color` is ARGB, not RGBA.** `#feda6333` is alpha `0xfe` over
  `(0xda, 0x63, 0x33)` — a warm orange at near-full opacity, not a green at 20%
  alpha. Blue is a constant `0x33` across every keyframe; **alpha** is what
  animates. The authority is the declared `AttributeTypes.ARGB_COLOR`, not the
  hex string's appearance.
- **`sky_color`/`fog_color`/`cloud_color` are `multiply` modifiers**, so their
  keyframes are per-tick *multipliers* over a base (a biome's own colour in
  vanilla). `ARGB.multiply` is `red(lhs)*red(rhs)/255` — **gamma byte space**,
  which is why `fog::multiply_gamma` exists and why a linear-space multiply is
  wrong here.
- **The interpolation is `ARGB.srgbLerp`**, a byte-space lerp — `ARGB.linearLerp`
  sits right next to it in the same file and is *not* what these tracks use —
  and `Mth.lerpInt` **floors**, never rounds.
- **The night sky disc is genuinely `#000000`.** The dark-blue night sky people
  remember is `FOG_COLOR` (`#0c0c16`/`#161616`) showing through at the horizon
  end of the gradient. The retired code blended toward a hand-invented
  `NIGHT = [0.006, 0.008, 0.02]`.

That last change is why `CLOUD_COLOR` is in scope at all: the cloud tint used to
be `sky_color * 0.9`, which becomes exactly invisible once the sky is correctly
black at night. Vanilla keeps clouds visible with their own non-black track.

**And the stale note.** `sky.rs`'s module doc used to say it ported the classic
1.21 cosine formulas, *"the same ones `entity.rs`'s validated port already uses
for `sky_darken`"*. True when written. By the time #96 was picked up,
`entity::sky_darken_for_time_of_day` had been rewritten as a timeline port
validated at all 24000 ticks (`tests/sky_light_factor_timeline.rs`, issue #49) —
so `sky.rs`'s private `sky_darken_shape` cosine had silently stopped being a
*duplicate* of a validated formula and become a *divergent second opinion*.
Nothing about the note looked wrong on inspection, which is precisely
`CLAUDE.md` rule 2. It is deleted; the sky colour reads the real track.

`celestial_angle_for_time_of_day` and `star_brightness_for_time_of_day` are
**still** the classic cosines, i.e. still #49. The sunrise band is deliberately
immune to that: it consumes only the *sign* of `sin(sun_angle)` to pick the dawn
or dusk side of the sky, and that sign is stable across the whole of each band's
non-zero-alpha window (measured on the JVM dump: dusk `11302..=14175` all
positive, dawn `21825..=702` all negative), so a wrong ramp cannot make the band
flicker sides.

### The sunrise fan's geometry is not what it looks like

`buildSunriseFan`'s perimeter vertices are **not offsets from the bright centre
vertex**. The centre is `(0, 100, 0)`; the perimeter is
`(sin·120, cos·120, -cos·40)`. After `sunrise_fan_transform` — which is
`Rx(90°) · Rz(90° + flip) · S(1, 1, alpha)`, in that order, because `mulPose`
*post*-multiplies — the perimeter is a ring of radius 120 centred on the **eye**,
with the bright apex 100 blocks off toward the sun. So the fan wraps the entire
sky and no screen rect localises it. What makes it read as a *band* is the
vertical squash (±40·alpha tall against 120 wide) plus the centre-to-rim alpha
ramp.

Getting that matrix order backwards puts the band 90° from the sun, in the middle
of nowhere, still looking like a plausible horizon glow in a screenshot.

### Void fog

`FogRenderer.computeFogColor`, and the sign is easy to invert from a summary:

```java
float darkness = Mth.clamp((onsetRange + level.getMinY() - camera.position().y) / onsetRange, 0, 1);
float brightness = Mth.square(1.0F - darkness);   // note the square
```

`darkness` is 0 at `min_y + onset_range` and **1 at `min_y`**, and the falloff is
**quadratic** — halfway down the onset range is quarter brightness, not half.
`onsetRange` is `1.0` for a *flat* world and `32.0` otherwise
(`ClientLevel.java:1277`). The scale multiplies `ARGB.redFloat(color)`, i.e.
gamma space, hence `fog::scale_gamma`.

### Per-biome sky tint (issue #96's fourth box)

**Done.** What follows is the record of two successive blockers, both of which
were true when written and false when acted on, and one estimate that was simply
wrong — kept in full, because the pattern is more valuable than the conclusion.
Skip to [What actually landed](#what-actually-landed) for current state.

#### Blocker one: "the client does not decode `registry_data`"

**Stale within the hour.** In 26.2 a biome
carries `"attributes": {"minecraft:visual/sky_color": "#78a7ff"}`.

The paragraph this replaces said biome ids arrive in `registry_data`, "which this
client decodes only as a packet id … there is no runtime registry parse". That
was true and evidenced when written. **#288 landed the `registry_data` ingest
about an hour later** (harvested into `a19e5e4`, whose message describes chests),
so it is false now. This is `CLAUDE.md` rule 2 in its purest form: nothing about
the claim looked wrong on inspection, and a re-read of the file it cited would
have confirmed it, because the thing that changed lives in a different file.

#### Blocker two: "the names have no caller across the version-free seam"

Re-verified end to end, by grepping the *producer* across the whole tree rather
than a named consumer file. This table was correct when written, and it is what
the work below acted on:

| link | state | evidence |
|---|---|---|
| wire → decode | **wired** | `packets/registry.rs` decodes `RegistryData`; biome arrives as `minecraft:worldgen/biome`, **not** `minecraft:biome` |
| decode → ordered names | **wired** | `ClientRegistries::apply` keeps entry names in registry order (index `i` = holder id `i`) in its `other` map |
| names → readable | **wired** | `ClientRegistries::entry_names(registry)` |
| names → version-free seam | **MISSING** | `entry_names` has **no caller outside `registry.rs`'s own unit tests** (whole-tree grep, unfiltered: 5 hits, 4 in that file, 1 an unrelated `fog.rs` doc comment). Neither `ClientEvent::Login` (`entity_id`, `game_mode`, `dimension`) nor `ClientEvent::DimensionTypeChanged` carries them, so the `Vec<String>` stays inside the adapter's `Mutex<ClientRegistries>`. `grep -c biome` on `lodestone-model/src/event.rs` is 5, all prose |
| biome id at the camera | **wired** | chunk decode fills the biome container (`PaletteKind::biomes()`), `ChunkSection::biome_at_block` floors a block coord into its 4×4×4 cell, `World::section` is public and the shell already calls it to mesh |
| id → colour table | absent | no data yet; deliberately not built, see below |
| colour → draw | **wired** | `SkyFrame::day_sky_color`, fed at `gpu.rs`'s sky block from `self.clear` |

So **both ends were built and the join was missing**, and the ids cannot be
hardcoded around it: a data pack reorders the registry, which is exactly why
#288's own doc says never to assume a holder id. That analysis was right, and the
patch it proposed was not.

#### The estimate that was wrong: "four edits, carrying the names"

Recorded above as the smallest patch: carry the ordered biome **names** on
`ClientEvent::Login`, land them in a resource, and look the colour up in the
shell against a table derived from our jar — `event.rs`, `adapter.rs`,
`session.rs`/`state.rs`, `gpu.rs`. Two things were wrong with it, and both were
found by trying to write it:

* **`Login` cannot cheaply carry anything.** It is constructed at 17 sites across
  **four** protocol families (`v47`, `v340`, `v735`, `v770`) plus a dozen tests;
  `DimensionTypeChanged` is cheaper but is constructed inside
  `lodestone-ecs/src/ingest.rs`. A **new** `#[non_exhaustive]` variant has zero
  existing construction sites and needs one arm in one routing switch, which is
  strictly the smaller change.
* **`gpu.rs` had nowhere for the colour to enter.** `SkyFrame::day_sky_color` was
  fed from `RenderState::clear`, and `app.rs` sets the clear colour to
  `FogSettings::color` — so the disc centre and the horizon were reading the same
  value by construction, and there was no second channel to tint. The colour also
  has to be resolved *at the camera* (the standing biome changes as the player
  walks and nothing on the network announces it), which is `Sim`'s job, not
  `RenderState`'s.

Both of these are the same lesson in different clothes: the previous pass counted
the hops it could see from `entry_names` outward, and the cost was in the hops it
had not opened yet. Nothing in the analysis looked wrong on inspection.

#### What actually landed

**Names never travel. The colours do.** Because we reply to
`select_known_packs` with an empty list, the server elides nothing, so every
biome entry arrives with its full NBT — including the sky colour. That makes the
server authoritative and deletes the jar-derived table entirely: a data pack can
reorder the registry, rename a biome, or change a colour, and shipping the value
at the holder id is correct for all three. It also means no table needs
re-deriving each version.

| hop | where |
|---|---|
| decode the attribute | `protocol/v770/src/packets/registry.rs` — `ClientRegistries::biome_sky_colors()`, a `Vec<Option<u32>>` where index `i` is holder id `i`, packed `0x00RR_GGBB` in **sRGB bytes** |
| cross the version-free seam | `lodestone-model/src/event.rs` — `ClientEvent::BiomeVisuals { sky_colors }`, emitted beside `DimensionTypeChanged` at `Login` |
| route it | `lodestone-ecs/src/session.rs` — an arm in **`session::handles_event`** (not `ingest`: registry data is a session scalar), folded into `ServerBiomeSkyColors(Arc<[Option<u32>]>)` |
| surface it | `lodestone-client/src/state.rs` — `PlayerSnapshot::biome_sky_colors`, an `Arc` clone per frame rather than a copy of the table |
| resolve at the camera | `lodestone-shell/src/sim.rs` — `Sim::biome_sky_color()`: eye block → `ChunkSection::biome_at_block` → index the table → `srgb_u8_to_linear` |
| reach the draw | `lodestone-render/src/fog.rs` — `FogSettings::sky_color`, consumed by `gpu.rs`'s `SkyFrame` |

Two design notes worth keeping:

* **The sky colour rides in `FogSettings`, a struct named for fog.** Deliberately.
  In vanilla they are one record (`EnvironmentAttributes` carries
  `visual/fog_color` and `visual/sky_color` side by side, and
  `FogRenderer.computeFogColor` blends them), and the disc's horizon *is* its fog
  end — so two independently-callable setters is exactly how the horizon has
  banded in a colour the sky never is before. One struct, one call, cannot drift.
  Every constructor defaults `sky_color` to `color`, so an untinted frame is
  byte-identical to the pre-#96 behaviour.
* **`Sim::biome_sky_color` scans *downward* for a section.** `sections_at` elides
  an empty section to `None`, and the section holding the player's feet is very
  often empty — standing on a plain at `y=64` puts the eye in section `64..80`
  while the ground is the last block of `48..64`. Sampling only the eye's section
  leaves the sky untinted over open ground, which is precisely where a sky is
  visible.

Every `None` in that chain means one thing — *the server has not told us* — and
falls back to the dimension colour the caller already computed. Never to a
plausible-looking overworld blue; that is the explicit-fallback shape #34 was
filed over.

The *composition* half was already pinned: `sunrise_sunset_timeline.rs`'s fourth
column checks `ARGB.multiply(plains #78a7ff, sky_color_track)` against the JVM at
every tick, so the gamma-space arithmetic was proven correct before the biome
path existed. The colour table was deliberately **not** landed ahead of its
caller in the previous pass — that is the island shape `CLAUDE.md` rule 1 is
about, and it would have been the fourteenth.

#### A measured warning for whoever gates it

Surveyed all 66 files in
`.cache/mc/26.2/client-src/data/minecraft/worldgen/biome` by parsing each one
(not by grepping): 56 declare `minecraft:visual/sky_color` and they hold only
**16 distinct values**. The 10 without it are exactly the Nether and End biomes,
which is consistent — the Nether's `"skybox": "none"` means fog alone is correct
there. No file uses the pre-26.2 `effects.sky_color` integer form; the key moved
into `attributes` and is a hex string.

The overworld spread is genuinely slight, and blue is a constant `0xff`:

| value | count | examples |
|---|---|---|
| `#6eb1ff` | 7 | desert, savanna, badlands |
| `#7ba4ff` | 13 | ocean, river, meadow, lush_caves |
| `#78a7ff` | 8 | **plains, swamp**, beach, deep_dark |
| `#859dff` | 2 | frozen_peaks, jagged_peaks |
| `#b9b9b9` | 1 | **pale_garden** — the one dramatic outlier, a desaturated grey |

**`plains` and `swamp` are byte-identical.** A "plains versus swamp"
discriminator — the obvious pick, and the one this task was briefed with — is
vacuous by construction: it would pass unchanged against the hardcoded constant
the feature is meant to replace. It is the *world* species of vacuous test from
`CLAUDE.md`'s table, where the flaw is in the input data and cannot be found by
reading the test. Gate `pale_garden` (`#b9b9b9`) against `desert` (`#6eb1ff`) for
a difference no constant can fake, and add `desert` against `frozen_peaks`
(`#859dff`, ΔR 23 / ΔG 20) to prove the gate resolves a *slight* difference too —
otherwise it only ever proves grey is not blue.

**That survey has since been confirmed against the wire, independently.**
`live_registry_data.rs::biome_sky_colours_from_a_real_server_match_mojangs_own_biome_files`
joins the creative oracle and checks all **66** entries of the server's own
`minecraft:worldgen/biome` payload (20238 bytes) against Mojang's own
`worldgen/biome/*.json`, parsed at test time: **56** declare a sky colour, **16**
distinct values, and the 10 without are exactly `basalt_deltas`,
`crimson_forest`, `nether_wastes`, `soul_sand_valley`, `warped_forest`,
`end_barrens`, `end_highlands`, `end_midlands`, `small_end_islands`, `the_end`.
The advice above stands, and both pairs it recommends are now gated — plus the
vacuous pair itself, as `control_plains_and_swamp_cannot_discriminate`, which
**asserts zero differing pixels**. Recording the zero is the cheapest inoculation
against someone "simplifying" the real gates back onto plains-versus-swamp.

The wire shape took three guesses to get right and only one of them is obvious:
`attributes` → an attribute-**id** key (`minecraft:visual/sky_color`) → an
`Either<value, {modifier, argument}>`. `EnvironmentAttributes.SKY_COLOR` is
`AttributeTypes.RGB_COLOR`, whose value codec is
`ExtraCodecs.STRING_RGB_COLOR = withAlternative(hexColor(6), RGB_COLOR_CODEC)`,
and vanilla *encodes* through the first alternative — so what arrives is the NBT
**string** `"#78a7ff"`, not an int. Note the contrast with
`visual/sunrise_sunset_color`, which is `ARGB_COLOR`: there the alpha animates
and the hex is 8 digits. A hermetic fixture built with our own `Nbt` writer would
have confirmed all three guesses at once whether or not any was right, which is
why the authority for the shape is the live gate and the hermetic sibling
(`registry_data.rs`) is scoped to the holder-id mapping and the failure modes.

Also still open: the below-horizon dark disc, and void fog's `min_y`/`onset_range`
come from `VoidFog::OVERWORLD` (`-64`/`32.0`) rather than the real dimension
height. `lodestone_ecs::ChunkWorld::extent` already carries `min_y`; threading it
to `RenderState` is a two-site `app.rs` change plus a source closure, and was
skipped here to keep this change out of two more shared files.

### The gates, and the two premises that were false

`crates/lodestone-render/tests/sunrise_sunset_timeline.rs` — all three colour
tracks, **byte-exact at every one of the 24000 ticks**, against
`oracle-java/SunriseSunsetTimelineOracle.java` (a sibling of #49's
`SkyLightTimelineOracle.java`; boots the real registries and samples
`Timeline.createTrackSampler`, so the expected values originate outside the code
under test). Keyframe *endpoints* are trivially right, so a test that checked
only named ticks like noon and peak sunset would pass vacuously on a sampler with
broken wraparound, easing or rounding. Controls, executed:

| deliberately-wrong sampler | ticks where it disagrees with the JVM |
|---|---|
| clamp to the first keyframe instead of wrapping | 71 of the 71 pre-first-keyframe ticks |
| `round` instead of `Mth.lerpInt`'s `floor` | 8825 of 24000 |

`crates/lodestone-render/tests/sky_gradient_pixels.rs` — pixel gates, measured on
this machine:

| gate | subject | control(s), EXECUTED |
|---|---|---|
| gradient | **0** of 28142 disc px outside a 3/255 tolerance; worst error 1 | fog == sky (the pre-#96 flat disc): **28142** bad; per-vertex fog factor: **16062** bad |
| sunrise band | 9408 of 30880 disc px warm, bbox `y82..120`, mean elevation **7.5°** against the disc region's 18.6° | noon (band alpha `0x00`): **0** warm; camera turned 180°: **0** warm |
| void fog | eye `+32` → mean byte 135.3; eye `-48` → 7.5; eye `-64` → 0.0. Measured midpoint ratio **0.0552** vs gamma-space prediction **0.0554** | `VoidFog::DISABLED` at the same eye height `-64`: 135.3 |
| **biome tint, gross** | `pale_garden #b9b9b9` vs `desert #6eb1ff`: **28142 of 28142** disc px differ, worst per-channel delta **115**; each frame **0** px outside the 3/255 gradient tolerance against *its own* colour | same colour twice: **0** differ · **`plains` vs `swamp` (byte-identical `#78a7ff`): 0 differ** — the vacuous pair, asserted rather than avoided |
| **biome tint, slight** | `desert #6eb1ff` vs `frozen_peaks #859dff`: **28142 of 28142** differ, worst delta **23** — proves the path resolves a real neighbouring value, not a coarse bucket | (shares the two controls above) |
| **biome tint, composition** | every disc px moves by exactly `(1 - t)` of the colour change: worst error vs that prediction **1/255** over 28142 px, fog value spanning `0.121..0.832`; delta **115** at `t=0.121` falling to **22** at `t=0.832` | a tint applied to the fog end too, or replacing the disc colour, differs from its neighbour just as much and breaks the `(1-t)` law immediately |

Four things about those gates are worth keeping:

**Two control premises were false, and running them is what found it.**

- The band gate's noon control found **244 warm pixels in a three-row line at
  `y119..121`** with the band provably not drawn. The culprit was the gate's own
  choice of a *warm* fog colour: the disc's fogged rim is warm, so "red beats
  blue" was never a statement about the band. Each gate now picks the fog colour
  that makes the *other* draw in frame unable to satisfy its discriminator —
  warm fog for the gradient, cool for the band.
- The band gate originally projected the fan's vertices to a screen rect and
  asserted the warm pixels landed inside it. The rect came out as the entire
  upper frame, which is how the fan-wraps-the-eye geometry above was discovered.
  The measurement is now confined to pixels where the *disc* paints (below the
  horizon the destination is the pass's black clear, so any band fragment
  trivially "beats blue" there) and localisation is by **mean elevation** and by
  **turning the camera around** — the only measurement that can distinguish a
  horizon band from a global warm tint, which no frame average can.

**The banding control's discriminator is a count, not a magnitude.** The
per-vertex frame's worst per-channel error is only `8/255`, so an assertion on
the worst error read as "no banding". Its *count* is 16062 pixels against the
shipped path's 0.

**The gates derive their geometry from the matrix the draw uploads.** Expected
fog values come from inverting `Camera::sky_view_projection` and intersecting the
resulting ray with the disc plane, not from a hand-rolled `tan(fov/2)`; the
band camera's yaw comes from where `sunrise_fan_transform` actually puts the
apex; the void-fog heights come from `VoidFog::OVERWORLD`'s own fields. That is
the rule a HUD gate in this repo learned the hard way by hardcoding a moving
anchor.

**A third control premise was false, in the biome-tint gate, and the same rule
caught it.** The composition gate was first written as "the tint moves the disc's
*centre* (`t < 0.15`) and not its fogged *rim* (`t > 0.97`)". The rim bucket came
back **empty — 0 of 28142 px** — so the rim assertion measured nothing while
looking rigorous. The cause is not the camera: `expected_fog_value` caps the disc
at `0.9 ×` the nine-gon's inradius on purpose, so
`hypot(0.9 · 512 · cos 22.5°, 16) / 512 = 0.832` is the largest fog value any
admitted pixel can have, and `0.97` was unreachable by construction. `0.97` was a
restated constant; the fix predicts every pixel from **its own** `t` and prints
the range it actually saw (`0.121..0.832`), so an empty bucket can no longer
satisfy it. Third instance of this exact failure on this one feature, all three
found by asserting the premise rather than reasoning about it.

## Configuration

None. The sky reads the same day clock the rest of the renderer does — there is
no second clock. `SkyFrame::day_fog_color` is fed the renderer's existing fog
colour and `day_sky_color` is fed `FogSettings::sky_color`, which every
`FogSettings` constructor defaults to the fog colour — so a caller that never
tints is byte-identical to before the gradient existed. The only thing that moves
it is `Sim::biome_sky_color` finding a real biome colour on a live server. Air
supply is entirely server-driven.

## Dependencies

- `lodestone-assets` — `CelestialAtlas` (sun + 8 moon phases stitched by the same
  `AtlasBuilder` every other atlas uses) and `load_cloud_texture`.
- `lodestone-render` — `sky`, `sky_pipeline`, `fog` (`VoidFog`, `multiply_gamma`,
  `scale_gamma`), `air_bubbles`, and `Camera::sky_view_projection`
  (translation-stripped, with a test whose negative control proves ordinary
  `view_projection` *is* translation-sensitive).
- A JDK (25, or the `eclipse-temurin:25-jdk` container) **only to regenerate**
  `tests/support/sunrise_sunset_timeline_jvm.txt` after a version bump; the dump
  is committed, so the gate itself needs no Java.
- `crates/protocol/v770` — `metadata.rs` decodes `airSupply`.
- `lodestone-ecs` / `lodestone-client` — `Vitals::air` and `PlayerSnapshot::air`.
  Note `ingest::handles_event` must list the event or `SharedState::apply` never
  forwards it in production, regardless of what a hermetic test shows. That trap
  hid working code twice in one session.
- `sky_pipeline_gpu.rs`'s three `real_jar_*` tests additionally depend on a
  fetched `.cache/mc/26.2/client.jar` (`xtask fetch-assets`) — `#[ignore]`d
  like every other GPU gate, but unlike the synthetic-pack gates they fail
  closed (never skip) when the jar is missing, per this repo's convention for
  real-asset gates.
