# Diagnosis: "fog too extreme / longer dropoff" + "at night the shadow is a different colour"

## What it is

Two read-only diagnoses, each with a specific, previously-unrecorded root cause. **Fog**:
three real divergences from vanilla — the terrain/entity fog colour never receives vanilla's
day/night colour track (an island: the mechanism exists and is already wired to the sky disc,
just not to terrain), the overworld's `linear(0, 1024)` environmental fog term is missing
entirely (the "longer dropoff"), and the render-distance term uses spherical distance where
vanilla uses cylindrical, erasing valleys under nearby hills. **Night shadow colour**: not a
brightness bug — the scalar lightmap this client computes is exactly vanilla's blue channel,
so red and green read 1.8x too bright; vanilla's lightmap is a genuine RGB colour (blue
moonlight, warm torchlight) that this client renders as flat grey. §4 gives minimal, low-risk
fixes for both, cheapest first.

Read-only investigation. Every vanilla number below is quoted from
`.cache/mc/26.2/{src,client-src}` with `file:line`. No repository file was modified.

---

## 0. Executive summary

| # | Finding | Severity | Already in the record? |
|---|---|---|---|
| **F1** | The terrain/entity **fog colour never gets the `FOG_COLOR` day/night track**. At midnight our distant terrain fades to a full-brightness sky blue `#87B5EB` where vanilla fades it to `#0D0E11`. The sky *disc* does get the track, so there is also a hard horizon seam. | **critical, night** | **No — new.** |
| F2 | No **environmental fog term**. Vanilla's overworld carries `linear(spherical, 0, 1024)` on top of the narrow render-distance band, reaching 0.225 by the time our ramp even starts. This *is* the "longer dropoff". | high | yes, `docs/fog.md` gap 1 |
| F3 | **Spherical** distance where vanilla's render-distance term is **cylindrical**. At an ordinary hilltop camera this reads **1.0** where vanilla reads **0.131**. | high | partly — `docs/fog.md` gap 2 pins an extreme ray, not this mundane case |
| F4 | Fog base colour is `SKY_COLOR` (`#87B5EB`), vanilla's is `#B2CEFF` (RD 8). Saturated blue vs pale haze. | medium | yes, `docs/fog.md` gap 4 |
| **N1** | **The night-shadow complaint is a HUE difference, not a brightness one.** Vanilla's midnight lightmap texel at (sky 15, block 0) is **`(0.2784, 0.2784, 0.5047)`** — blue. Ours is **`(0.5047, 0.5047, 0.5047)`** — grey. Our scalar is *exactly vanilla's blue channel*, which is why `light.rs`'s midnight gate passed. | **critical** | described in `docs/time-of-day-lighting.md`, decided "not built" |
| N2 | Block light is warm in vanilla (`#FFD88C`, `BlockFactor 1.4`, additive) and grey in ours. At block level 8: vanilla `(0.586, 0.507, 0.352)`, ours `0.482` grey. | high | yes, "#383's third divergence" |
| **N3** | **The `SKY_LIGHT_COLOR` tint needs no new uniform lane and no fifth bind group.** It is algebraically recoverable from the `sky_darken` lane we already send, verified byte-exact against the existing JVM oracle. This removes the stated blocker. | — | **No — new.** |

---

## 1. Vanilla's fog model, with the real numbers

### 1.1 The uniform, and the two terms

`FogRenderer` builds a six-float UBO
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/fog/FogRenderer.java:229-236`):

```java
Std140Builder.intoBuffer(byteBuffer)
   .putVec4(fogColor)
   .putFloat(environmentalStart)
   .putFloat(environmentalEnd)
   .putFloat(renderDistanceStart)
   .putFloat(renderDistanceEnd)
   .putFloat(skyEnd)
   .putFloat(endClouds);
```

`assets/minecraft/shaders/include/fog.glsl:13-24` is the whole falloff:

```glsl
float linear_fog_value(float vertexDistance, float fogStart, float fogEnd) {
    if (vertexDistance <= fogStart) { return 0.0; }
    else if (vertexDistance >= fogEnd) { return 1.0; }
    return (vertexDistance - fogStart) / (fogEnd - fogStart);
}

float total_fog_value(float sphericalVertexDistance, float cylindricalVertexDistance, ...) {
    return max(linear_fog_value(sphericalVertexDistance,   environmentalStart, environmantalEnd),
               linear_fog_value(cylindricalVertexDistance, renderDistanceStart, renderDistanceEnd));
}
```

`fog.glsl:32-40`:

```glsl
float fog_spherical_distance(vec3 pos)   { return length(pos); }
float fog_cylindrical_distance(vec3 pos) { return max(length(pos.xz), abs(pos.y)); }
```

So: **two independent linear ramps, two different metrics, combined with `max`.** The
falloff shape is linear in both — we get the shape right; we get the *terms* wrong.

### 1.2 The render-distance term

`FogRenderer.java:198-200`, run unconditionally *after* the environment hook:

```java
float renderDistanceFogSpan = Mth.clamp(renderDistanceInBlocks / 10.0F, 4.0F, 64.0F);
fog.renderDistanceStart = renderDistanceInBlocks - renderDistanceFogSpan;
fog.renderDistanceEnd = renderDistanceInBlocks;
```

`renderDistanceInBlocks = renderDistanceInChunks * 16` (`:185`), and the chunk count is
`options.getEffectiveRenderDistance()` (`GameRenderer.java:632`).

**We already match this exactly** for render distances 3–40 chunks (`FOG_START_FRACTION = 0.9`
is `1 - span/blocks` wherever the clamp is inactive). Not the bug.

### 1.3 The environmental term — the overworld's is NOT inert

`EnvironmentAttributes.java:18-24` registered defaults:

```java
FOG_START_DISTANCE = ... .defaultValue(0.0F) ...
FOG_END_DISTANCE   = ... .defaultValue(1024.0F) ...
```

`AtmosphericFogEnvironment.java:68-69` reads exactly those attributes:

```java
fog.environmentalStart = camera.attributeProbe().getValue(EnvironmentAttributes.FOG_START_DISTANCE, partialTicks);
fog.environmentalEnd   = camera.attributeProbe().getValue(EnvironmentAttributes.FOG_END_DISTANCE, partialTicks);
```

**Confirmed by data, not inference:** `src/data/minecraft/dimension_type/overworld.json` declares
only `visual/ambient_light_color`, `cloud_color`, `cloud_height`, `fog_color`, `sky_color` —
**no fog distances**. And of all 60-odd biome files, only `swamp.json` and
`mangrove_swamp.json` override them (`grep -l fog_start_distance src/data/minecraft/worldgen/biome/*.json`).

So the plain overworld runs `linear(spherical, 0.0, 1024.0)` — a gentle haze that begins at
the eye. That is precisely the "longer dropoff" the player is describing.

### 1.4 Sky and cloud ends (for completeness)

`AtmosphericFogEnvironment.java:73-76`:

```java
fog.skyEnd   = Math.min(renderDistance, probe.getValue(SKY_FOG_END_DISTANCE, partialTicks));   // default 512
fog.cloudEnd = Math.min(options.cloudRange().get() * 16, probe.getValue(CLOUD_FOG_END_DISTANCE, ...)); // default 2048
```

We already implement `skyEnd` (`sky::sky_fog_end_for_render_distance`, #399). Not the bug.

### 1.5 The fog **colour**, including the night track

`AtmosphericFogEnvironment.getBaseColor` (`:26-48`):

```java
int fogColor = camera.attributeProbe().getValue(EnvironmentAttributes.FOG_COLOR, partialTicks);
...
int skyColor = camera.attributeProbe().getValue(EnvironmentAttributes.SKY_COLOR, partialTicks);
float skyFogEnd = Math.min(probe.getValue(SKY_FOG_END_DISTANCE, partialTicks) / 16.0F, renderDistance); // CHUNKS
float skyColorMixFactor = Mth.clampedLerp(skyFogEnd / 32.0F, 0.25F, 1.0F);
skyColorMixFactor = 1.0F - (float)Math.pow(skyColorMixFactor, 0.25);
return ARGB.srgbLerp(skyColorMixFactor, fogColor, skyColor);
```

Base attributes, from `dimension_type/overworld.json:36-37`:

```json
"minecraft:visual/fog_color": "#c0d8ff",
"minecraft:visual/sky_color": "#78a7ff"
```

and **both are multiplied by a per-tick timeline track** (`Timelines.java:58-70`):

```java
.addModifierTrack(EnvironmentAttributes.FOG_COLOR, ColorModifier.MULTIPLY_RGB,
   track -> track.addKeyframe(133, -1).addKeyframe(11867, -1)
                 .addKeyframe(13670, NIGHT_FOG_COLOR_MULTIPLIER_START)
                 .addKeyframe(22330, NIGHT_FOG_COLOR_MULTIPLIER_END))
.addModifierTrack(EnvironmentAttributes.SKY_COLOR, ColorModifier.MULTIPLY_RGB,
   track -> track.addKeyframe(133, -1).addKeyframe(11867, -1)
                 .addKeyframe(13670, -16777216).addKeyframe(22330, -16777216))
```

with (`Timelines.java:33-34`)

```java
int NIGHT_FOG_COLOR_MULTIPLIER_START = ARGB.colorFromFloat(1.0F, 0.05F, 0.05F, 0.09F);  // #0D0D16
int NIGHT_FOG_COLOR_MULTIPLIER_END   = ARGB.colorFromFloat(1.0F, 0.09F, 0.09F, 0.09F);  // #161616
```

**Worked, at our default render distance 8:**

`skyFogEnd = min(512/16, 8) = 8` chunks → `clampedLerp(8/32, 0.25, 1.0) = 0.4375` →
`skyColorMixFactor = 1 - 0.4375^0.25 = 0.18671`.

| clock | `FOG_COLOR` after track | `SKY_COLOR` after track | `getBaseColor` = `srgbLerp(0.1867, fog, sky)` |
|---|---|---|---|
| noon (6000) | `#C0D8FF` (192,216,255) | `#78A7FF` (120,167,255) | **`#B2CEFF` (178,206,255)** |
| midnight (18000) | `#101216` → (16,18,22) | `#000000` | **`#0D0E11` (13,14,17)** |

(`ARGB.multiply` is integer `red(lhs)*red(rhs)/255`, `ARGB.java:80`; `srgbLerp` is per-channel
`Mth.lerpInt`, i.e. `p0 + floor(alpha*(p1-p0))`, `ARGB.java:155-160` — gamma space throughout,
consistent with CLAUDE.md's "vanilla is not colour-managed".)

Void darkening and `darkenWorldAmount` are further multipliers (`FogRenderer.java:134-145`);
both are 0 in the ordinary case.

---

## 2. Our fog implementation, and the precise divergences

Ramp math: `crates/lodestone-render/src/fog.rs`.
Shader: `crates/lodestone-render/src/shaders/{model,entity,fluid}.wgsl` (`fog_amount`, line 129 in `model.wgsl`).
Preset selection: `sim.rs::fog_for_render_distance`.
Upload: `gpu.rs::state::RenderState::fog_with_clock`.

**Ruled in as correct:**

* The falloff is linear, matching `linear_fog_value`.
* `start`/`end` at RD 3–40 are vanilla-exact (`230.4 → 256` at RD 16).
* The mix is in **gamma** space (`model.wgsl:246`), matching `fog.glsl:29` over
  `terrain.fsh`'s non-colour-managed bytes. Fixed earlier; not a regression source.

### F1 — the fog colour has no clock (this is the "too extreme" at night)

`sim::fog_for_render_distance` returns `SKY_COLOR` flat. `app.rs::WindowApp::redraw`
folds in **weather only** (`weather_darken_linear`, `lightning_flash_linear`). `set_fog`
(`gpu.rs::state::RenderState::set_fog`) just stores it. `fog_with_clock`
(`gpu.rs::state::RenderState::fog_with_clock`) folds in the clock for
the **sky-darken lane only**:

```rust
fn fog_with_clock(&self, eye: glam::Vec3) -> FogUniform {
    let mut fog = FogUniform::new(&self.fog, [eye.x, eye.y, eye.z]);
    fog.end_enabled[2] = self.sky_darken.value();
    fog
}
```

Meanwhile `fog_color_for_time_of_day` **exists** in `crates/lodestone-render/src/sky.rs:361`
with a correct port of the `FOG_COLOR` track (`sky.rs:175-179`, `#0c0c16` / `#161616`) — and
its only consumer in the whole tree is `sky_pipeline.rs:861`, the **sky disc**. Verified:

```
grep -rn "fog_color_for_time_of_day" crates/   →  sky.rs (def), lib.rs (re-export),
                                                  sky_pipeline.rs:48,861, one test
```

This is the classic island shape from CLAUDE.md rule 1, inverted: the mechanism exists, is
tested, and one of its two consumers was never wired.

**Consequence, midnight, RD 8:**

| | terrain fog colour | sky disc horizon |
|---|---|---|
| vanilla | `#0D0E11` | `#0D0E11` (same value) |
| ours | **`#87B5EB`** | `SKY_COLOR × #161616` ≈ `#0C1014` |

So at night the outer chunks dissolve into a **bright saturated blue** against a near-black
sky, with a hard seam at the horizon. That is the dominant "the fog seems a bit too extreme"
signal, and it is a colour bug wearing a fog bug's clothes.

### F2 — no environmental term (this is the "longer dropoff")

`FogUniform` carries exactly one start/end pair, so the presets are mutually exclusive rather
than `max`-combined. Profile at RD 8 (128 blocks), level ray:

| d | vanilla env `(0,1024)` | vanilla RD `(115.2,128)` | **vanilla total** | **ours** |
|---|---|---|---|---|
| 16 | 0.0156 | 0 | **0.0156** | **0** |
| 32 | 0.0313 | 0 | **0.0313** | **0** |
| 64 | 0.0625 | 0 | **0.0625** | **0** |
| 96 | 0.0938 | 0 | **0.0938** | **0** |
| 115.2 | 0.1125 | 0 | **0.1125** | **0** |
| 121.6 | 0.1188 | 0.5 | **0.5** | **0.5** |
| 128 | 0.1250 | 1.0 | **1.0** | **1.0** |

At RD 16 the pre-haze reaches **0.225** at 230.4 blocks, where we are still at 0.

Note the *direction*: the missing term means we render **less** haze than vanilla at mid range.
But the player's phrasing is about the **profile shape**, and it is exactly right — vanilla
spreads the fade over the whole view volume and finishes with a band; we render nothing at all
and then a **hard wall in the last 10%**. The per-block rate in that band is also 29% steeper
than vanilla's (`1/25.6` vs `0.775/25.6` at RD 16), because vanilla's band only has to climb
the remaining 0.775.

### F3 — spherical where vanilla's render-distance term is cylindrical

`model.wgsl:245` (and the entity/fluid twins):

```wgsl
let amount = fog_amount(length(in.world - camera.fog_eye.xyz));
```

`fog.rs`'s `spherical_distance_over_fogs_a_pitched_ray_versus_vanillas_cylindrical` already
pins this with a 36.87° ray. **A far more mundane case makes the same point harder** — an
ordinary hilltop, RD 8:

* eye at `y = 140`, valley floor at `y = 64`, horizontal separation 110 blocks
* spherical = `sqrt(110² + 76²) = 133.7` → ours = `fog_factor(133.7, 115.2, 128)` = **1.0**
* cylindrical = `max(110, 76) = 110` → vanilla RD term = **0**, env term = `110/1024` = 0.107,
  total = **0.107**

The valley below a hill is **completely erased** in our client and barely hazed in vanilla.
This is the biggest single contributor to "too extreme" in daylight.

### F4 — fog base colour

`#87B5EB` (135,181,235) against vanilla's `#B2CEFF` (178,206,255): 43/25/20 bytes darker and
markedly more saturated. Already documented as `docs/fog.md` gap 4, deliberately deferred
because ~a dozen shell pixel gates hardcode `SKY_COLOR` as "the background". F1 fixes the night
complaint without touching this.

---

## 3. The "night shadow colour" complaint: it is HUE, not brightness

### 3.1 What the player is seeing

Vanilla's lightmap is a **16×16 RGB texture**, not a scalar, generated by a real fragment
shader every frame — `assets/minecraft/shaders/core/lightmap.fsh:35-65`:

```glsl
float block_level = floor(texCoord.x * 16) / 15;
float sky_level   = floor(texCoord.y * 16) / 15;
float block_brightness = get_brightness(block_level) * lightmapInfo.BlockFactor;
float sky_brightness   = get_brightness(sky_level)   * lightmapInfo.SkyFactor;
vec3 color = max(lightmapInfo.AmbientColor, lightmapInfo.NightVisionColor * lightmapInfo.NightVisionFactor);
color += lightmapInfo.SkyLightColor * sky_brightness;
vec3 BlockLightColor = mix(lightmapInfo.BlockLightTint, vec3(1.0), 0.9 * parabolicMixFactor(block_level));
color += BlockLightColor * block_brightness;
...
color = clamp(color, 0.0, 1.0);
color = mix(color, notGamma(color), lightmapInfo.BrightnessFactor);
```

with `parabolicMixFactor(l) = (2l-1)²` (`:31-33`) and `notGamma` scaling the triple by
`maxScaled / maxComponent`, `maxScaled = 1 - (1-max)⁴` (`:24-29`).

The uniforms come from `LightmapRenderStateExtractor.extract` (`:53-56, 67`):

```java
renderState.blockFactor    = this.blockLightFlicker + 1.4F;
renderState.blockLightTint = ARGB.vector3fFromRGB24(probe.getValue(EnvironmentAttributes.BLOCK_LIGHT_TINT, partialTicks));
renderState.skyFactor      = probe.getValue(EnvironmentAttributes.SKY_LIGHT_FACTOR, partialTicks);
renderState.skyLightColor  = ARGB.vector3fFromRGB24(probe.getValue(EnvironmentAttributes.SKY_LIGHT_COLOR, partialTicks));
renderState.ambientColor   = ARGB.vector3fFromRGB24(probe.getValue(EnvironmentAttributes.AMBIENT_LIGHT_COLOR, partialTicks));
```

`vector3fFromRGB24` is a bare `byte/255` (`ARGB.java:213-215`) — no linearisation. **All of
this is gamma space.**

The three colours, decoded from the raw ints:

| uniform | source | value |
|---|---|---|
| `AmbientColor` | `overworld.json:33` `#0a0a0a`, = `DimensionTypes.java:36`'s `-16119286` = `0xFF0A0A0A` | `(0.03922, 0.03922, 0.03922)` — **grey** |
| `BlockLightTint` | `DimensionDefaults.java:5` `BLOCK_LIGHT_TINT = -10100` = `0xFFFFD88C` | `(1.0, 0.84706, 0.54902)` — **warm amber** |
| `SkyLightColor` | `Timelines.java:71-75` track: `730→-1`, `11270→-1`, `13140→NIGHT`, `22860→NIGHT`; `NIGHT_SKY_LIGHT_COLOR = ARGB.colorFromFloat(1.0, 0.48, 0.48, 1.0)` = `0xFF7A7AFF` (`Timelines.java:30`) | day `(1,1,1)`, midnight **`(0.47843, 0.47843, 1.0)` — blue** |
| `SkyFactor` | `Timelines.java:76-80` track: `730→1.0`, `11270→1.0`, `13140→0.24`, `22860→0.24` | day `1.0`, midnight `0.24` |
| `BrightnessFactor` | `Options.gamma` default | `0.5` |

### 3.2 The measurement that settles brightness-vs-hue

Midnight (tick 18000), sky level 15, block level 0, computed by hand from the constants above:

```
sky_brightness = get_brightness(1.0) * 0.24               = 0.24
color   = (0.039216, 0.039216, 0.039216)                  # AmbientColor
       += (0.478431, 0.478431, 1.0) * 0.24                # SkyLightColor
        = (0.154039, 0.154039, 0.279216)
notGamma: max = 0.279216, (1-max)^4 = 0.269911, maxScaled = 0.730089, ratio = 2.614236
        = (0.402695, 0.402695, 0.730089)
mix(color, notGamma, 0.5)
        = (0.278367, 0.278367, 0.504652)
```

**Vanilla midnight, open sky, unlit: `(0.278367, 0.278367, 0.504652)`.**

Ours (`light.rs:192-199`, mirrored in `model.wgsl:91-96`, `entity.wgsl:74`, `fluid.wgsl:54`):

```rust
let sky = brightness(sky_level) * sky_darken;   // 1.0 * 0.24
let block = brightness(block_level);            // 0.0
apply_brightness_option(AMBIENT_LIGHT + sky.max(block))   // → 0.504652, applied to all 3 channels
```

**Ours: `(0.504652, 0.504652, 0.504652)`.**

So the answer is unambiguous: **it is a hue difference, and a brightness one in R and G only.**

* Blue channel: **exact match**.
* Red/green: ours is **1.813×** vanilla's.
* Channel ratio R/B: vanilla **0.5516**, ours **1.000**.

In sRGB terms the lightmap multiplier is roughly `#474781` (blue-grey moonlight) in vanilla
versus `#818181` (neutral grey) in ours.

### 3.3 Why every existing gate passed — worth adding to the record

`light.rs`'s `midnight_lands_on_vanillas_value_and_not_on_either_wrong_one` asserts
`0.504652` and passes. That number is **exactly vanilla's blue channel**, because
`not_gamma_grey(c) = 1 - (1-c)⁴` is algebraically the same as vanilla's
`notGamma`'s treatment of its *max* component, and blue is the max at night. The gate is a
textbook CLAUDE.md **magnitude** vacuous test — well-constructed, with three discriminating
hypotheses, and its subject is one channel of a three-channel quantity. A scalar model cannot
be wrong about the channel it happens to reproduce.

### 3.4 Block light is a second, independent hue bug

Vanilla, sky 0, block level 8 (`8/15`), any time of day:

```
block_brightness  = (0.533333/(4-1.6)) * 1.4 = 0.222222 * 1.4 = 0.311111
parabolic(0.533333) = (0.066667)^2 = 0.004444 ; 0.9 * = 0.004
BlockLightColor = mix((1.0, 0.847059, 0.549020), vec3(1.0), 0.004)
                = (1.0, 0.847671, 0.550824)
color = ambient + BlockLightColor * 0.311111 = (0.350327, 0.302902, 0.210583)
notGamma ratio = 2.34597 → (0.821853, 0.710601, 0.494024)
mix 0.5 → (0.586090, 0.506752, 0.352304)
```

Ours: `apply_brightness_option(0.039216 + 0.222222)` = **`0.481948` grey**.

Torchlight in vanilla is **warm** (R/B = 1.664); ours is neutral (R/B = 1.000), 22% too dark in
red and 44% too bright in blue. Two separate causes: the missing `BLOCK_LIGHT_TINT`, and
`max(sky, block)` where vanilla adds and scales block by `BlockFactor = 1.4`.

### 3.5 N3 — the tint needs no new uniform lane (removes the stated blocker)

`docs/time-of-day-lighting.md` costs this as needing a packed 24-bit lane. It does not.
`SKY_LIGHT_COLOR` and `SKY_LIGHT_FACTOR` have **identical keyframe ticks** —
`730 / 11270 / 13140 / 22860` (`Timelines.java:71-80`) — and **neither track calls
`.setEasing(...)`**, so both interpolate linearly on the same parameter. Therefore:

```
t = clamp((1.0 - sky_darken) / (1.0 - 0.24), 0.0, 1.0)
sky_light_color = srgbLerp(t, vec3(1.0), vec3(122.0/255.0, 122.0/255.0, 1.0))
```

**Verified byte-exact** against the existing JVM oracle
`crates/lodestone-render/tests/support/sky_light_timeline_jvm.txt` (columns:
`tick`, `sky_light_factor` as f32 bits, `sky_light_color` ARGB):

| tick | oracle factor | derived `t` | derived colour (with `Mth.lerpInt`'s floor) | oracle colour |
|---|---|---|---|---|
| 0 | `0x3f340c7c` = 0.703285 | 0.390415 | `255 + floor(0.390415·(122-255)) = 203` → `ffcbcbff` | **`ffcbcbff`** ✓ |
| 12000 | `0x3f340c7c` = 0.703285 | 0.390415 | `ffcbcbff` | **`ffcbcbff`** ✓ |
| 13000 | `0x3e980310` = 0.296894 | 0.925139 | `255 + floor(0.925139·(-133)) = 131` → `ff8383ff` | **`ff8383ff`** ✓ |
| 13140 | `0x3e75c28f` = 0.240000 | 1.000000 | `ff7a7aff` | **`ff7a7aff`** ✓ |

The invariant also survives weather: `WeatherAttributes.java:17-19` and `:28-30` alpha-blend
`SKY_LIGHT_COLOR` and `SKY_LIGHT_FACTOR` with the **same** alpha (0.3125 rain / 0.52734375
thunder) toward the same night endpoints, and `addLayer` then lerps both by the same
`rainLevel`/`thunderLevel`.

**Two known exceptions, both momentary — flag but do not block:**
`ClientLevel.java:268` forces `SKY_LIGHT_FACTOR = 1.0` during a sky flash without touching the
colour, and `LightmapRenderStateExtractor.java:57-64` adds End-flash intensity to `skyFactor`.
Both push the factor to/above 1.0, so the derivation yields white where vanilla keeps blue for
a few ticks. The `clamp` handles the >1.0 case safely.

---

## 4. Minimal fixes

### FIX A — clock the terrain fog colour (F1). **Do this first; it is ~3 lines.**

**File: `crates/lodestone-shell/src/gpu.rs`, in `fog_with_clock` (line 624).** Nowhere else.

```rust
fn fog_with_clock(&self, eye: glam::Vec3) -> FogUniform {
    let mut settings = self.fog;
    settings.color = lodestone_render::fog_color_for_time_of_day(
        self.time_of_day.value(),
        self.fog.color,
    );
    let mut fog = FogUniform::new(&settings, [eye.x, eye.y, eye.z]);
    fog.end_enabled[2] = self.sky_darken.value();
    fog
}
```

Why here and not in `app.rs`:

* `self.time_of_day` already lives on `RenderState` (`gpu.rs::state::RenderState::time_of_day`)
  and is already read by the sky pass in `gpu.rs::frame::RenderState::render_inner`. One clock,
  one place.
* `fog_with_clock` is the *only* producer of the fogged passes' uniform — three call sites
  (`gpu.rs::frame::RenderState::render_inner`, `gpu.rs::entity_passes::prepare_entities`,
  `gpu.rs::entity_passes::prepare_block_entities`), covering terrain/models, entities and block
  entities. One edit reaches every fogged pixel.
* **Critical: `self.fog.color` must stay the un-tracked day base.** `gpu.rs::frame::RenderState::render_inner` passes it to
  `SkyFrame::with_fog_color`, and the sky pass applies `fog_color_for_time_of_day` itself
  (`sky_pipeline.rs:861`). Pre-multiplying it in `app.rs` or inside `set_fog` would
  **double-apply the track to the sky disc** — `#161616²/255` ≈ `#020202`. Doing it inside
  `fog_with_clock` leaves the stored base alone, so the disc and the terrain derive one colour
  from one base and one clock and structurally cannot drift.
* Layer order is right by construction: `app.rs` has already folded lightning-flash and
  weather into `self.fog.color`, and vanilla runs the timeline layers first, then
  `WeatherAttributes.addBuiltinLayers` on top (`Timelines` tracks are registered on the
  timeline; `WeatherAttributes.addBuiltinLayers` adds later layers). Applying the clock
  outermost here is a commutative multiply against weather's own multiply, so the order is
  moot for `MULTIPLY_RGB`; the one non-commutative case (`BLEND_TO_GRAY` on `SKY_COLOR`) does
  not touch the fog colour.

**No bind group change. No uniform change. No shader change.**

Former loose end, now closed: `app.rs::WindowApp::redraw` calls
`set_clear_color_tracked(desired_fog.color)` (`gpu.rs::state::RenderState::set_clear_color_tracked`),
which applies the same `FOG_COLOR` day/night track as `fog_with_clock`, so the clear colour and
the terrain fog cannot drift — no separate jar-less-with-no-sky follow-up remains.

### FIX B+C — the environmental term and cylindrical distance (F2, F3). Ship together.

Both edit the same three `fog_amount` bodies, so splitting them doubles the churn.

1. `crates/lodestone-render/src/fog.rs`: add `environmental_start` / `environmental_end` to
   `FogSettings`, and pack them into `FogUniform`'s **two remaining free lanes** — `eye.w` and
   `end_enabled.w`. `docs/fog.md:244-248` already confirms these are free (`end_enabled.z` is
   the sky-darken lane). **The struct does not grow, so no fifth bind group and no
   `max_bind_groups` risk.** Overworld and End: `0.0 / 1024.0`. Nether: `10.0 / 96.0`, with
   `for_render_distance`'s real band restored in the render-distance pair rather than
   `FogSettings::nether` repurposing it. Water/lava keep using the environmental pair (that is
   what they are).
2. `model.wgsl`, `entity.wgsl`, `fluid.wgsl` — replace the single `fog_amount(length(...))`
   with:

   ```wgsl
   fn fog_amount(rel: vec3<f32>) -> f32 {
       let sph = length(rel);
       let cyl = max(length(rel.xz), abs(rel.y));
       let env = linear_fog(sph, camera.fog_eye.w,          camera.fog_end_enabled.w);
       let rd  = linear_fog(cyl, camera.fog_color_start.w,   camera.fog_end_enabled.x);
       return max(env, rd) * camera.fog_end_enabled.y;
   }
   ```

   `rel` is already computed at the call site. **Cylindrical distance costs no uniform data at
   all** — the shader already has both `in.world` and `camera.fog_eye.xyz`.
3. `fog.rs`'s CPU twin (`fog_factor`) needs a two-term sibling so the headless gates keep
   describing the shader.

Note `sky_disc.wgsl` should be left alone in this change — its ramp end is `skyEnd`, a third
quantity, already handled by #399.

### FIX D — fog base colour `#C0D8FF` + `skyColorMixFactor` (F4)

Leave deferred exactly as `docs/fog.md:234-242` argues. It is a genuine divergence but its
blast radius is a dozen shell pixel-gate backgrounds, and Fix A removes the visible night
symptom without it.

### FIX E — the lightmap becomes a `vec3` (N1, N2)

This is the shading-model change `docs/time-of-day-lighting.md` scoped and declined. N3 removes
the uniform-budget half of the objection; the gate re-baselining half stands.

1. `crates/lodestone-render/src/light.rs`: add `light_color_from_levels(sky, block, sky_darken) -> [f32; 3]`
   implementing `lightmap.fsh` faithfully — real vec3 `not_gamma` (`maxScaled/maxComponent`,
   guarding `max == 0`), additive combine, `BLOCK_FACTOR = 1.4`,
   `BLOCK_LIGHT_TINT = (1.0, 216/255, 140/255)`, the parabolic block-tint mix, and the
   `SKY_LIGHT_COLOR` derivation from N3. Keep the scalar `light_term` for the GUI/particle
   callers until they are audited.
2. `model.wgsl` and `fluid.wgsl`: `VsOut.shade: f32` → `vec3<f32>`; the fragment multiply
   `linear_to_srgb(tex.rgb) * tint_col * in.shade` is already component-wise.
   `entity.wgsl`: `VsOut.light_term: f32` → `vec3<f32>`; `entity.wgsl:215` likewise.
   Two extra interpolated floats per shader.
3. **No uniform change, no bind-group change** — the only per-frame input is `sky_darken`,
   already riding `fog_end_enabled.z`.
4. `block.wgsl` is the demo-only packed path (#400); leave it.
5. Consider porting `blockLightFlicker` (`LightmapRenderStateExtractor.java:33-35`) as its own
   small follow-up — it is a visible torch shimmer, and modelling `BlockFactor` as a flat 1.4
   keeps every hermetic gate deterministic.

---

## 5. How to prove each fix

Every expected value below originates in the decompiled jar or in the existing JVM oracle
dump, never in our own formula.

### Gate A — night fog colour (hermetic, CPU, no GPU needed)

Where: a new `#[test]` next to `gpu.rs`'s `fog_start_fraction_matches_vanillas_span`, or in
`crates/lodestone-render/tests/`.

Expected value, **written out by hand** from `Timelines.java:34` and `ARGB.java:80`:
`NIGHT_FOG_COLOR_MULTIPLIER_END = ARGB.colorFromFloat(1.0, 0.09, 0.09, 0.09)`, and
`as8BitChannel` floors, so the multiplier is `(22, 22, 22)`. `ARGB.multiply` is integer
`a*b/255`. For our day base `#87B5EB`:

| clock | predicted terrain fog colour, sRGB bytes | current build |
|---|---|---|
| noon 6000 | **(135, 181, 235)** — unchanged, track is `#ffffff` | (135, 181, 235) |
| midnight 18000 | **(11, 15, 20)** | (135, 181, 235) |
| dusk 13670 | `#0D0D16` multiplier → **(6, 9, 19)** | (135, 181, 235) |

Assert all three. The noon row is the second discriminating hypothesis: a fix that darkens fog
at *every* tick passes a midnight-only assertion and fails this.

**Negative control** (must fail, and must be executed, not described): build the uniform the
old way — `FogUniform::new(&self.fog, eye)` with no clock — and assert it lands on
(135, 181, 235) at tick 18000, i.e. that the detector fires.

**Pixel gate, if a GPU is available.** Draw one terrain quad at a distance where
`fog_factor == 1.0`, at tick 6000 and 18000, and read **the quad's own screen rect** (derived
from the same `view_proj` expression the draw uses, per CLAUDE.md's HUD-rect trap), never a
frame average. Midnight must be within 2 bytes of (11, 15, 20); noon within 2 of
(135, 181, 235). Also read a **second rect** on a *near* quad (factor 0) at both ticks: it must
change only by the lightmap, not by the fog, proving the change is localised to the fogged
population.

### Gate B+C — the fog profile

Where: extend `fog.rs`'s `ramp_gate` module, whose expectations are already hand-written
literals rather than calls back into our formula.

Expectations from `fog.glsl:23-24` + `EnvironmentAttributes.java:18-24`, RD 8, level ray
(where `spherical == cylindrical`, so the table is metric-independent and therefore
vanilla-exact):

```
(16, 0.015625) (32, 0.031250) (64, 0.062500) (96, 0.093750)
(115.2, 0.112500) (121.6, 0.500000) (128, 1.000000)
```

**Negative control:** the current single-term implementation must fail this table, and the
failure's bounding box must be **`[16.0, 115.2]`** — i.e. it must localise to the mid field,
*not* to the endpoints, which both models already agree on. Assert the box string, as
`the_old_fraction_model_fails_this_gate` does, so the control proves the *detector* localises.

For C, the mundane hilltop case as its own row — eye `(0, 140, 0)`, sample at horizontal 110,
`y = 64`:

```
spherical    = 133.7,  cylindrical = 110.0
ours today   = 1.0000
vanilla      = max(linear(133.7, 0, 1024), linear(110.0, 115.2, 128)) = max(0.13057, 0) = 0.13057
```

`spherical_distance_over_fogs_a_pitched_ray_versus_vanillas_cylindrical` will (correctly) fail
when C lands — it is written as a pin on the gap, and its own message says so.

### Gate E — the night shadow hue. **A channel ratio, never a brightness.**

The predicate must not be "night is bluer than day" — that is the direction-only trap the
hurt-overlay gate fell into. Use a **ratio of ratios**, which cancels the texture's own colour
and therefore needs no knowledge of the subject:

```
q = (R/B at tick T) / (R/B at tick 6000)     on the same pixel, same surface
```

on an **unlit** (block 0), **sky-15** neutral surface. Predicted from `lightmap.fsh` +
`Timelines.NIGHT_SKY_LIGHT_COLOR` + `overworld.json`'s `#0a0a0a` + `Options.gamma` 0.5:

| tick | vanilla lightmap texel | vanilla `q` | current build `q` |
|---|---|---|---|
| 18000 | `(0.278367, 0.278367, 0.504652)` | **0.551596** | **1.000000** |
| 13000 | `(0.325627, 0.325627, 0.570925)` | **0.570359** | **1.000000** |

(The 13000 row's inputs come from `sky_light_timeline_jvm.txt`: factor `0x3e980310` = 0.296894,
colour `ff8383ff`. Two ticks, so a single-point coincidence cannot pass.)

**Also assert the magnitude, per channel** — a hue-only assertion passes a fix that gets the
tint right and the level wrong:

* midnight blue channel = **0.504652** (unchanged from today — this is the number the existing
  scalar gate matches);
* midnight red channel = **0.278367**, and it must **miss 0.504652 by more than 0.2**. Stating
  it this way makes the gate a direct correction of `light.rs`'s
  `midnight_lands_on_vanillas_value_and_not_on_either_wrong_one`.

**Measure by location — two spatially distinct populations in one frame:**

* **open-sky night pixel** (sky 15, block 0): blue, `q = 0.5516`;
* **cave pixel** (sky 0, block 0): vanilla's `AmbientColor` is neutral `#0a0a0a`, so this must
  stay **exactly grey** at `(0.093545, 0.093545, 0.093545)`, `q = 1.0`.

The cave row is the control against the laziest wrong fix — a global night blue-tint — which
would pass the open-sky row and fail here. A frame average cannot separate the two, which is
the whole point.

**Third population, time-invariant:** a torch-lit pixel (sky 0, block 8) must read
`(0.586090, 0.506752, 0.352304)` at *any* tick, where today it reads `0.481948` grey
(`q_block = 1.664` vanilla vs `1.000` ours). Independent of the sky-light work, so it also
discriminates a fix that ported `SKY_LIGHT_COLOR` and skipped `BLOCK_LIGHT_TINT` / the additive
combine.

**Negative control:** call the retained scalar `light_term` in place of
`light_color_from_levels` and assert it produces `q = 1.000` on the open-sky row and fails the
red-channel magnitude assertion — executed, and observed to fail.

**Daylight regression pin:** `SKY_LIGHT_COLOR` is white `-1` across the whole
`[730, 11270)` plateau, so every noon expectation in the tree must be byte-identical after the
fix. Assert `light_color_from_levels(sky, block, 1.0)` is grey and equals today's
`light_term(packed, 1.0)` for all 256 packed bytes. That is the cheapest guard on the
re-baselining risk `docs/time-of-day-lighting.md` flags, and it should be written *before* the
shader edit.

---

## 6. Ruled out — do not re-investigate

* **The fog falloff curve.** Linear in both (`fog.glsl:13-21` vs `fog.rs::fog_factor`). Not a
  gamma/exponential mismatch.
* **The fog mix's colour space.** Already gamma (`model.wgsl:246`, `fog::apply_fog_gamma`),
  matching `fog.glsl:29` over `terrain.fsh`'s non-colour-managed bytes. Fixed earlier this
  session and correct.
* **`FOG_START_FRACTION = 0.9` as a taste knob.** It is algebraically
  `1 - span/blocks` wherever `Mth.clamp` is inactive, so `sim::fog_for_render_distance`'s ramp
  is **vanilla-exact** for RD 3–40 — including our default 8 and the reported 16. The
  outstanding migration to `for_render_distance` only matters at RD < 3 or > 40. Not the bug,
  and worth migrating for tidiness only.
* **`sky_fog_end` / the sky disc's gradient length.** Correct since #399
  (`sky::sky_fog_end_for_render_distance` implements
  `min(renderDistanceInBlocks, 512)` per `AtmosphericFogEnvironment.java:73`).
* **`sky_darken_for_time_of_day`.** It *is* `SKY_LIGHT_FACTOR`, JVM-gated to 1e-4 at all 24000
  ticks (`tests/sky_light_factor_timeline.rs`). `1.0` at noon, `0.24` at midnight. The scalar
  factor is right; only its *colour* companion is missing.
* **`AMBIENT_LIGHT = 10/255`.** Independently confirmed twice: `DimensionTypes.java:36`'s
  `-16119286` decodes to `0xFF0A0A0A`, and `dimension_type/overworld.json:33` literally reads
  `"minecraft:visual/ambient_light_color": "#0a0a0a"`. Also confirmed **grey**, which is why
  the cave population stays neutral and the light term can remain a scalar underground.
* **Biome-level fog distances.** Only `swamp.json` and `mangrove_swamp.json` override
  `visual/fog_start_distance` / `fog_end_distance` in the whole biome set; the standing-biome
  case is the registered `0.0 / 1024.0` default.
* **`get_brightness` / `notGamma` / `BrightnessFactor 0.5`.** All three match
  `lightmap.fsh:20-33` and `Options.java:900`. The scalar chain is right; its *rank* is wrong.
* **Face shade / AO as the "night shadow".** Vanilla's per-face shade is a colour-neutral
  scalar and `notGamma`'s grey specialisation is exact for a grey input — neither can produce a
  hue. The hue lives entirely in the lightmap.
* **An entity drop-shadow decal.** We do not render one (`grep -rl "entity_shadow|drop_shadow|shadow_radius" crates/`
  hits only two font/container tests), so "the shadow" cannot mean that.
* **`AmbientColor` being black.** It is not; an earlier fix that drove the unlit floor to
  `0.000` was wrong, and `0.093545` is correct. Already in `light.rs`'s record.
