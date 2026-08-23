# World-space text samples the lightmap

## What it is

The three world-space flat-colour text passes — entity nametags
(`gpu/nametag.rs`), sign text (`gpu/sign_text.rs`) and `text_display` glyphs and
panels (`gpu/display_text.rs`) — multiply vanilla's lightmap texel into every
vertex colour they emit, so a sign in a dark room reads dark and a glowing one
does not. Until this landed none of the three sampled any lightmap at all and
every glyph in the world drew at full brightness, which made `has_glowing_text`
a feature with no visible effect in the one situation it exists for.

## How it works

### The gap, and why it looked closed

Porting glowing sign text meant reading
`AbstractSignRenderer.submitSignText`'s single `hasGlowingText()` branch, which
sets three things at once:

| | plain | glowing |
|---|---|---|
| glyph colour | `getDarkColor` — `ARGB.scaleRGB(dye, 0.4)` | the full dye, unscaled |
| outline colour | none | `getDarkColor` |
| light coordinate | `state.lightCoords` | `15728880` — `LightCoordsUtil.FULL_BRIGHT` |

The first two were ported. The third was recorded as *needing no port*, on the
true observation that this project's sign pass sampled no lightmap, so both arms
already produced full brightness. That is exactly the shape of a defect this
repo keeps paying for: the claim was correct and the conclusion was that the
feature was complete, when in fact **the branch was inert** — a glowing sign and
a plain sign were equally bright in the dark. The same held for nametags and
`text_display`.

### Vanilla samples the lightmap in the *vertex* stage

`assets/minecraft/shaders/core/text.vsh` in the 26.2 `client.jar`:

```glsl
#if !defined(IS_GUI) && !defined(IS_SEE_THROUGH)
    vertexColor = Color * sample_lightmap(Sampler2, UV2);
#else
    vertexColor = Color;
#endif
```

`text_background.vsh` forks identically on `IS_SEE_THROUGH`.

Two consequences settle the whole design:

* **The multiply is `Color * texel`, per vertex, in gamma space.** These three
  passes share one shader (`crates/lodestone-shell/src/shaders/nametag.wgsl`)
  whose entire input is a flat vertex colour, and they draw into the target's
  raw non-sRGB view (see `world-text-gamma-blend.md`). So folding the texel into
  the colour on the CPU before upload is the *same arithmetic at the same rate*
  as vanilla's — no new vertex attribute, no second uniform, no lightmap texture
  to keep in sync, and no shader change at all. `gpu/nametag.rs`'s
  `WorldTextLight::tint` is that fold; it is `lodestone_render::light_color` and
  nothing else, the one authority `shaders/model.wgsl`, `shaders/entity.wgsl`
  and `shaders/fluid.wgsl` all duplicate.
* **The see-through variants sample nothing.** `IS_SEE_THROUGH` does not even
  declare the `UV2` input. So a see-through name tag and a `FLAG_SEE_THROUGH`
  `text_display` are full-bright in vanilla **by construction**, whatever
  `lightCoords` they were submitted with. Reading the submission's light
  argument rather than the shader it selects is the trap here, and it is a
  faithful-looking one — `SubmitNodeCollection.submitNameTag` does pass a real
  light coordinate to the see-through submission, and it is discarded downstream.

### The three passes take three different bytes

Checked one renderer at a time rather than made uniform:

| pass | vanilla source | byte here |
|---|---|---|
| sign text, plain side | `state.lightCoords` | `SignSpawn::light`, per **side** (`has_glowing_text` is per side) |
| sign text, glowing side | `15728880` | `TEXT_FULL_BRIGHT` = `0xFF` |
| name tag, depth-tested group, not sneaking | `LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)` | `entity_passes::entity_light`, both nibbles floored at 2 |
| name tag, depth-tested group, sneaking | `state.lightCoords` | `entity_passes::entity_light`, raw |
| name tag, see-through group | *(shader samples nothing)* | no tint |
| `text_display`, depth-tested ranges | `DisplayRenderer.getSkyLightLevel`/`getBlockLightLevel` — the **brightness override**'s nibbles if set, else the sample | `DisplayDraw::override_light()` else `EntityLightSource::sample` |
| `text_display`, see-through range | *(shader samples nothing)* | no tint |

Note the full-bright byte is `0xFF` and **not**
`lodestone_render::ENTITY_FULLBRIGHT`, which is `0xF0` — sky 15, block 0. The
sky half is scaled by the clock's `SKY_LIGHT_FACTOR`, so a glowing sign carrying
`0xF0` would fade to roughly a quarter brightness at midnight, precisely when it
has to be legible. `ENTITY_FULLBRIGHT` is a *fallback for a caller with no world
to sample*, not vanilla's `FULL_BRIGHT`; the two coincide at noon and only at
noon, which is why the gate for it runs at midnight.

### Every producer already existed

Nothing new samples the world. `block_entities::sign_spawns` has been filling
`SignSpawn::light` from `net::entity_light_at` all along;
`gpu/entity_passes.rs::entity_light` is the same eye-height-probe,
fire-forces-the-block-half rule every other entity pass uses; and
`DisplayDraw::brightness_override` was decoded and repacked when
`Display.DATA_BRIGHTNESS_OVERRIDE_ID` was wired. The gap was three consumers,
not a missing source — and this is worth knowing before reaching for a new
sampler, because a fourth implementation of "what light is at this block" is the
failure mode the record here warns about repeatedly.

### The name tag draw order was wrong, and it mattered here

`NameTagRenderer::draw` used to record the depth-tested group first and the
see-through group over it, citing the field **declaration** order in
`SubmitNodeCollection` (`nameTags`, then `seeThroughNameTags`). The order that
runs is `FeatureRenderDispatcher.executeTranslucent`'s, and it is the other way
round: `seeThroughNameTags`, then `nameTags`. Same trap as reading a packet's
wire order off its record's field list instead of off its `write` method.

It was inert while everything was full bright, and stopped being inert the
moment one of the two groups started taking real light: a full-bright
`129/255`-alpha copy painted *over* the lit one throws away roughly half the
darkening, so the light would have reached the vertex buffer and mostly not
reached the screen. Flipped, the lit opaque copy wins wherever the tag is
unoccluded and only the faded copy survives behind a wall — which is vanilla's
look.

### Measured, in the README screenshot

`scripts/screenshot-scenes/02-signs.txt` was described as a dark room and was
not one: nothing roofed it, so every sign sat at sky light 15 and the whole
frame rendered full-bright. Re-rendered against the unroofed scene, this change
moved **zero** pixels — and the control that established that is worth keeping,
because two runs of *identical* code differ by 78,215 pixels in a band at the
frame's right edge (`x 2309..2559, y 411..753`, worst channel delta 11). That
region of the capture is nondeterministic run to run; a single before/after diff
there would have been read as this change's effect. It is not, and it is not
this change's to fix.

With a ceiling added, over the same two screen rects:

| | glowing lime wall sign | plain `Lodestone` standing sign |
|---|---|---|
| before | p95 **255**, max 255 | p95 **255**, max 255 |
| after | p95 **255**, max 255 | p95 **211**, max 211 |

The plain sign's ink no longer reaches full brightness and the glowing sign's
still does — which is both halves of the claim in one table, and the second row's
left cell is the regression control for "this must not make glowing text dimmer
than it already was".

## How to change it, and the gotchas

* **A fixture where every subject sits in the same light cannot see this bug at
  all.** That is how it survived: every hermetic gate for these passes built its
  spawn with `ENTITY_FULLBRIGHT` and asserted colours, so a pass discarding the
  byte was indistinguishable from one honouring it. Any new gate here needs two
  arms at two different light bytes, and the gates in these three files are
  written that way — each measured against *the same fixture drawn at
  full-bright light*, so the dye and the outline scale cancel out and what is
  left is the texel alone.
* **Do not pick block light 14 as the "lit" arm.** `BLOCK_FACTOR` is `1.4`, so
  `brightness(14/15) * 1.4` already clamps to `1.0` and a torch-lit sign is
  byte-identical to one in open daylight. Measured — the first version of one
  gate compared `0.4` with `0.4` and reported a defect that was not there.
  Block light 4 discriminates.
* **The tint is applied at vertex emission, never to a cached layout.**
  `StyledInkLayoutCache` is shared across frames *and across all three passes*,
  and its `StyledRect::color` is the span's own resolved colour. Multiplying
  into it would light one pass with another's byte and would persist across
  frames.
* **`sky_darken` and `ambient` are per-frame, not per-draw.** `gpu/frame.rs`
  builds one `WorldTextLight` and hands it to all three `prepare` calls. Their
  unset defaults (`1.0` and `OVERWORLD_AMBIENT_LIGHT`) make the tint for a
  full-bright byte exactly `[1, 1, 1]`, which is why every pre-existing hermetic
  gate keeps asserting exactly what it always asserted.
* **When adding a fourth world-text pass, decide the see-through question
  first.** It is a property of the render type vanilla selects, not of the light
  argument it passes, and the two disagree.

## Configuration

None. There is no option, feature flag or constant to turn this off; the inputs
are the world's own light, the server clock (via `SkyDarkenSource`) and the
dimension's ambient colour (via `AmbientLightSource`), all of which
`RenderState` already polls once a frame for terrain and entities.

## Dependencies

* `lodestone_render::light_color` — vanilla's `lightmap.fsh`, in Rust. The one
  authority; changing it changes terrain, entities, fluids and now world text
  together.
* `lodestone_render::ENTITY_FULLBRIGHT` — the *fallback* byte, deliberately not
  the full-bright one used here.
* `gpu/sources.rs`'s `EntityLightSource`, `SkyDarkenSource`, `AmbientLightSource`.
* `gpu/entity_passes.rs::entity_light` — the shared entity light rule.
* `crate::block_entities::sign_spawns` → `net::entity_light_at` — the sign byte.
* `crate::display_entities::DisplayDraw::override_light` — the display byte.
* `.cache/mc/26.2/client.jar`'s `assets/minecraft/shaders/core/text.vsh` and
  `text_background.vsh`, plus `AbstractSignRenderer`, `SubmitNodeCollection`,
  `FeatureRenderDispatcher`, `NameTagFeatureRenderer`, `GlyphRenderTypes`,
  `DisplayRenderer` and `LightCoordsUtil` under `client-src/`.

## Verification

```bash
# the six light gates plus every pre-existing gate in the three passes
cargo test -p lodestone-shell --lib gpu::sign_text gpu::nametag gpu::display_text

# the pixel gates the draw-order flip could regress (all #[ignore]d)
cargo test -p lodestone-shell --test nametag_pixels --test world_text_gamma_blend_pixels \
  --test world_text_over_geometry_pixels -- --ignored

# the README screenshot whose scene is a deliberately dark room
LODESTONE_SCENES=02-signs just screenshots
```
