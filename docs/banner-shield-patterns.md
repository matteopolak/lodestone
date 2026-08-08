# Banner and shield pattern compositing (issue #174)

## What it is

The shared colour/ordering math behind vanilla's banner and shield pattern
system: a base dye colour plus an ordered list of pattern layers, each drawn
as a masked sprite tinted by its own dye colour. `lodestone_render::banner_pattern`
(`crates/lodestone-render/src/banner_pattern.rs`) is that math: a
[`DyeColor`] enum carrying vanilla's real per-colour tint, and
`banner_pattern_layers`/`shield_pattern_layers`, which turn a base colour
plus a pattern list into the exact ordered draw list vanilla itself builds.

**This module is pure, GPU-free colour and ordering logic, and it is now
genuinely consumed.** `lodestone_render::block_entity::BlockEntityModelSet::resolve_banner`
calls `banner_pattern_layers` directly and its result reaches a real GPU
pipeline (`EntityPipeline::banner_layer_pipeline`), exercised end to end by
`crates/lodestone-render/tests/banner_pattern_layer_pixels.rs` — see "Steps
D–F: landed" below for what that closed and what is still missing (a real
consumer wired into the live client from world state, and the real
pattern-mask atlas).

## How it works

### Vanilla draws N quads, not one composited texture

The natural assumption — "composite the layers into one texture, like a
Photoshop stack" — is not what vanilla does, and porting that assumption
would have been a wrong implementation even with a mesh to hang it on.
`BannerRenderer.submitPatterns`
(`.cache/mc/26.2/client-src/net/minecraft/client/renderer/blockentity/BannerRenderer.java:172-201`)
draws the **same** flag/shield mesh once per layer:

1. Layer 0: the base mask (`Sheets.BANNER_PATTERN_BASE`/`SHIELD_PATTERN_BASE`,
   i.e. `entity/banner/base` or `entity/shield/base`), tinted by the item's
   base colour.
2. Then up to `MAX_PATTERNS = 16` (`BannerRenderer.java:41,189`) pattern
   layers, each `entity/banner/<pattern-asset-id>` or
   `entity/shield/<pattern-asset-id>` (`Sheets.getBannerSprite`/
   `getShieldSprite`, `Sheets.java:100-106`), tinted by *that layer's own*
   dye colour, in the item's stored order.

So `banner_pattern_layers`/`shield_pattern_layers` return an ordered
`Vec<PatternLayer>` — `(sprite: ResourceLocation, color: [f32; 3])` — one
entry per draw a renderer needs to issue. A future consumer with a real mesh
issues exactly that many draws, each sampling its `sprite` and tinting by its
`color`; nothing here forces a texture-compositing approach, but nothing
stops one either — the ordered list is what either implementation walks.

### The colour table, and the one bug this module's own tests caught

`DyeColor`'s 16 variants carry vanilla's `textureDiffuseColor`
(`DyeColor.java:30-45`'s constructor argument right after each colour's
name), packed `0x00RRGGBB`, **gamma-space** sRGB bytes — the same "vanilla is
not colour-managed" rule as every other tint in this codebase.
`gamma_rgb()` unpacks that to `[f32; 3]` in `0.0..=1.0`, ready for a shader's
`srgb_to_linear(linear_to_srgb(rgb) * tint)` round-trip
(`crates/lodestone-render/src/shaders/overlay.wgsl` is the same pattern for
a different pass) — never a linear multiply, which would wash out every
banner and shield in the game.

Worth recording: the hex table was hand-transcribed from the jar's decimal
constants once, and `LIME` came out `0x80C71C` instead of the jar's
`0x80C71F` — a one-hex-digit slip a 3-entry spot-check test did not catch.
`every_dye_diffuse_color_matches_its_jar_decimal_constant` checks all 16
against the *decimal* literals (a second, independently-typed source) in one
pass and is what actually caught it. Kept as the test to trust over the
smaller spot-check, which is kept only because it also happens to pass.

## What is still missing (historical — see "Steps D–F: landed" for the current state)

**This section is a record of a past scoping decision, kept for the reasoning,
not a claim about the tree today.** `grep -rn "banner_pattern_layers"` now
also hits `lodestone_render::block_entity::BlockEntityModelSet::resolve_banner`
and that function's own tests — `banner_pattern_layers` is no longer an
island for the block-entity path (the item-icon/held-item paths below are
unaffected; they still route through `lodestone-game`, not built here).

Two prerequisites were identified at the time, both outside that session's
file ownership, so neither was built speculatively then:

1. **No typed decode of the pattern data** — for the **item** (GUI icon,
   held-item) consumer specifically. `minecraft:banner_patterns` and
   `minecraft:base_color` are item data components. Item components this
   codebase has not given a dedicated typed variant land as
   `lodestone_game::item::ComponentValue::Opaque(Vec<u8>)` — structurally
   present (item components decode generically) but not interpretable as a
   colour/pattern list; `ItemComponents::get_int`/`get_str` cannot read them.
   Adding a typed variant means editing `lodestone-game/src/item.rs`, which
   this task's file ownership assigns to the cost-screens agent, not here.
   **This prerequisite does not block the block-entity consumer** — see
   below.
2. **No banner/shield mesh, resolver, shell wiring, or pixel gate — at the
   time this was written.** Steps A and B (tint plumbing onto block
   entities; the `banner_layer_pipeline` pipeline variant) landed in an
   earlier session and were **not** this prerequisite — they removed a
   *different* blocker (see "A third prerequisite" below). Steps D
   (mesh)/E (resolver)/F (pixel gate) landed in a later session — see
   "Steps D–F: landed" above — leaving only the shell installer and the
   NBT gather (both squarely issue #23's scope: "the in-world banner block
   entity rendering itself is #23's scope... land the shared compositing
   function here and have #23's banner work consume it") plus the real
   pattern-mask atlas (`lodestone-assets` work, independent of #23).

   **Corrected, because the old wording overstated this by a lot.** This entry
   used to claim `BannerFlagModel` needs "per-vertex cloth-wave animation this
   codebase has never ported". That is wrong for 26.2. `BannerFlagModel.setupAnim`
   is a **single per-part rotation** —
   `flag.xRot = (-0.0125 + 0.01 * cos(2π * phase)) * π` — mechanically identical
   to the chest lid and the bell body, both of which `BlockEntityMesh`'s
   `part_transforms` overrides already handle. The flag is **one box**
   (`addBox(-10, 0, -2, 20, 40, 1)` on a 64×64 sheet) and the stand is a `pole`
   plus a `bar` (`BannerModel.createBodyLayer`). Phase is per-position:
   `(floorMod(x*7 + y*9 + z*13 + gameTime, 100) + partialTick) / 100`
   (`BannerRenderer.java:93`).

   So the mesh is ordinary. **The real remaining constraint is the one
   per-instance tint does not solve**: vanilla's `RenderPipelines.BANNER_PATTERN`
   (`RenderPipelines.java:311-318`) draws the flag opaque and then each mask layer
   with **translucent blend, depth write off, no alpha cutout**, at equal depth.
   Our `EntityPipeline` is `blend: None`, `depth_write: true`, and its shader
   discards below alpha 0.5 — so the layers need a third pipeline variant. And
   because translucent depth-write-off draws are **order-dependent**, they cannot
   ride `plan_block_entities`' `(model, texture)` batching: two banners whose
   layers use the same two sprites in opposite orders could not both be right.
   Vanilla dodges this by stitching every pattern into one atlas and preserving
   submission order. **Do not build atlas stitching for this.** Send the opaque
   flag/pole/bar through the existing batcher and the tinted layers through a
   small separate ordered draw list — banners are rare, and a handful of extra
   draw calls costs nothing.

Shield's first-person hand and third-person held item (this issue's other
named scope) are blocked on the same two prerequisites — there is no shield
pattern to draw until both land, regardless of which draw call would host
it.

### Prerequisite 1 does not actually block the block-entity consumer — checked, not assumed

A banner **block entity**'s pattern list is not an item component at all.
`BannerBlockEntity.saveAdditional`/`loadAdditional`
(`.cache/mc/26.2/client-src/net/minecraft/world/level/block/entity/BannerBlockEntity.java:49-63`)
stores it under NBT key `"patterns"` (`BannerPatternLayers.CODEC`), and the
base colour comes from the **block itself**
(`AbstractBannerBlock.getColor()`, one banner block per dye colour — there is
no `minecraft:white_banner` vs `minecraft:red_banner` *state property*, they
are sixteen separate blocks), not from any component. `BlockEntity.nbt` is
already generic and already proven to carry a block entity's real payload —
`docs/block-entity-renderers.md`'s Sign section captured this NBT shape for a
sign and it holds for any block entity type alike. So a `BannerSpawn`
gather could parse `"patterns"` directly out of the world's own
`BlockEntity.nbt`, the same way `lodestone_world::sign_text::SignText::parse`
does for a sign, **without touching `lodestone-game` at all**. Prerequisite 1
is real for the item-icon/held-item consumers (they start from an
`ItemStack`, which *does* carry these as components) but not for the
block-entity one — the next attempt at #23's banner work should start from
the block-entity NBT path, not wait on the item-component work to land
first.

### A third prerequisite — corrected: per-instance tint already existed, and is now on block entities too

**This section previously claimed "no per-instance tint" and estimated a
wide-blast-radius `EntityPipeline` change to add one. A Fable architecture
review caught that the premise was false before any of that speculative work
started, and it is worth recording exactly how, because the wrong claim was
specific and confident-sounding, not a vague guess:**

Per-instance gamma-space tint has existed **since before this doc's original
scoping pass**, at shader location 9:
`lodestone_render::entity_pipeline::EntityInstanceRaw::tint` packs
`AARRGGBB` — bits 0–23 the gamma-space tint, bits 24–31 the hurt-overlay
alpha — with `with_tint`/`InstanceTint`/`upload_instances_tinted` all `pub`
and already in production use for sheep wool, dyed leather armour, the hurt
overlay and the creeper flash. `entity.wgsl:269` already does the multiply in
**gamma** space, per this repo's "vanilla is not colour-managed" rule. The
"no per-instance tint" claim was checked against `BlockEntityInstance`/
`BlockEntityBatch` alone (correctly — those *did* carry no tint field) and
the finding was generalised to "the engine has no per-instance tint",
which does not follow: the entity path already had it, block entities simply
had not been plumbed onto the same mechanism yet.

**That plumbing gap is closed as of this session (issue #174 step A):**
`BlockEntityInstance` gained `tint: [u8; 3]` (all three resolvers —
`resolve_chest`/`resolve_skull`/`resolve_bell` — pass white,
`[255, 255, 255]`, i.e. `InstanceTint::NONE`'s rgb half) and
`BlockEntityBatch` gained `tints: Vec<InstanceTint>`, filled by
`plan_block_entities` in lockstep with the existing per-instance `lights`.
`lodestone_shell::gpu::RenderState::prepare_block_entities` now uploads
through `upload_instances_tinted` instead of the plain `upload_instances`.
Chest/skull/bell are provably unaffected — white tint packs to exactly the
same `EntityInstanceRaw` bits `upload_instances` produced (`with_tint([255,
255, 255])` on an already-`NO_TINT` word is a no-op; `with_hurt_overlay(false)`
and `with_creeper_white_overlay(0)` likewise), and all three block-entity
pixel gates plus `placed_chest_block_entity_pixels` stayed green. **So the
mechanism a banner base colour or a pattern-layer tint would use already
reaches block entities**; no `EntityPipeline`-widening was needed, because
the tint lives in the *instance buffer*, not the pipeline.

**What genuinely does need a pipeline change — and step B built it** —
is the *pipeline state* the original entry below correctly identified, which
tint has nothing to do with: vanilla's `RenderPipelines.BANNER_PATTERN`
(`RenderPipelines.java:311-318`) draws the flag opaque and then each mask
layer with **translucent blend, depth write off, no alpha cutout**, at equal
depth, while `EntityPipeline`'s mob/armour pipelines are `blend: None,
depth_write: true` with an unconditional cutout `discard`. `build_entity_pipeline`
(`crates/lodestone-render/src/entity_pipeline.rs`) is now parameterised by
`blend: Option<wgpu::BlendState>` and `depth_write: bool` in addition to the
`depth_compare` it already took, and `EntityPipeline::banner_layer_pipeline()`
requests `LessEqual` (vanilla's `GREATER_THAN_OR_EQUAL` under this engine's
`[0,1]` depth, per `CLAUDE.md`), `Some(BlendState::ALPHA_BLENDING)`,
`depth_write: false`. Both existing callers (`EntityPipeline::new`,
`armour_pipeline`) pass `None, true` explicitly — the same hardcoded values
`build_entity_pipeline` used before this change — so this is zero behaviour
change for mobs and armour; only `entity_pipeline`'s own hermetic unit tests
plus a `lodestone-render`/`lodestone-shell` compile were re-run to confirm
that (there is no pixel gate to add yet — nothing constructs a banner mesh to
draw through it).

**Step C, landed: the alpha-cutout `discard` no longer applies to banner
layers.** The paragraph above used to end here saying the fix was deferred.
`entity.wgsl` now has a second fragment entry point, `fs_main_no_cutout` —
byte-identical shading to `fs_main` (both call a shared `shade_entity`
helper) except it never runs the `tex_col.a < 0.5` discard —  and
`banner_layer_pipeline` binds it instead of `fs_main`.
`build_entity_pipeline` grew a `fragment_entry: &str` parameter for this;
both existing callers (`EntityPipeline::new`, `armour_pipeline`) pass
`"fs_main"` explicitly, so this is zero behaviour change for mobs and
armour — mirroring exactly how `blend`/`depth_write` were threaded through
for step B. Verified with `cargo test -p lodestone-render --test
wgsl_valid` (naga parses and validates both entry points) plus
`lodestone-render`'s own 572 lib tests; there is still no pixel gate for
this pipeline specifically, because nothing yet constructs a banner mesh to
draw through it (see "Steps D-F: handoff" below).

The order-dependency finding this section originally reached is still
correct and unaffected by the tint correction: because translucent
depth-write-off draws must submit in the item's stored pattern order, they
still cannot ride `plan_block_entities`' `(model, texture)` batching (two
banners reusing the same two sprites in opposite orders could not both be
right), so a real consumer still wants the opaque flag/pole/bar through the
existing batcher and the tinted layers through a small separate ordered draw
list, not atlas stitching.

## How to change it, and the gotchas

- **`DyeColor` here is intentionally a separate type from anything in
  `lodestone-game`.** This crate does not depend on `lodestone-game`'s item
  types, and duplicating a 16-variant enum is cheaper than adding a
  dependency edge for one enum. If `lodestone-game` grows its own decoded
  `DyeColor` (as part of fixing prerequisite 1), the natural follow-up is a
  `From`/`TryFrom` between the two by name (`DyeColor::from_name`/`name()`
  already exist for exactly this), not a merge.
- **Gamma space, always.** Every colour `packed_rgb`/`gamma_rgb` returns is
  a direct, unconverted read of the jar's own gamma-space byte — resist the
  urge to route it through `srgb_to_linear` before returning it. The
  conversion belongs in the consuming shader, at the same point the sampled
  mask texel is converted, not here.
- **The base layer is not optional and is not "pattern 0".** Every banner
  and shield has a base colour and draws it as its own always-present layer
  0, independent of how many (including zero) pattern layers follow —
  `no_patterns_still_draws_the_base_layer` pins this.
- **16 is a hard cap on top of whatever the stack carries**, not a
  reflection of it — `caps_at_sixteen_pattern_layers_plus_the_base` feeds 20
  and asserts exactly 17 layers come back (1 base + 16), keeping the first
  16 pattern entries, matching `BannerRenderer.java:189`'s loop bound
  literally rather than trusting an untrusted stack (command-given or
  foreign-save) not to exceed it.
- **Pattern asset ids are un-namespaced path fragments**, e.g. `"creeper"`,
  matching `BannerPattern.assetId()` and the pattern's own registry file
  (`assets/minecraft/data/minecraft/banner_pattern/creeper.json`'s
  `asset_id` field) — not a `ResourceLocation` on `StoredPatternLayer`
  itself, since the banner/shield split changes the namespace prefix
  (`entity/banner/…` vs `entity/shield/…`) the *consumer* needs, which a
  pre-resolved location would bake in wrong for one of the two callers.
- **The jar ships individual sprite PNGs, not a pre-built atlas** — settled by
  listing `client.jar` directly rather than assuming, per a reviewer flag on
  this doc. `assets/minecraft/textures/entity/banner/*.png` holds one PNG per
  pattern asset id (`base.png`, `creeper.png`, `cross.png`, … 43 files,
  measured — see "Steps D–F: landed" below) plus `banner_base.png` (44
  total); the shield family is the parallel `entity/shield/*.png` set, not
  yet loaded (no shield mesh exists to consume it — see the "Consumers"
  section below). `assets/minecraft/atlases/banner_patterns.json` is not a
  second, competing source — it is a *directory-source* atlas descriptor
  (`{"sources": [{"type": "minecraft:directory", "prefix": "entity/banner/",
  "source": "entity/banner"}]}`) telling vanilla's own runtime stitcher to
  combine every PNG under that directory into one GPU atlas texture, the
  same "stitch many individual sprites at startup" shape this codebase's own
  vanilla block-atlas loader already implements — not a format to parse for
  compositing math. **Landed**:
  [`lodestone_assets::banner_pattern_atlas::BannerPatternAtlas`](../crates/lodestone-assets/src/banner_pattern_atlas.rs)
  loads the individual pattern PNGs by asset id, resolving the real sprite
  list through that same descriptor rather than a hand-transcribed filename
  list (mirroring
  [`chest_texture_stems`](../crates/lodestone-render/src/block_entity.rs)/
  `skull_texture_stems`'s stem-list-plus-loader shape, one `Image` per
  pattern id rather than a stitched sheet) — not a read or rebuild of
  vanilla's atlas file. See "Steps D–F: landed" below for the measured
  counts and the pixel gate this unblocked.

## Configuration

None — pure, deterministic function of its inputs.

## Dependencies

- `lodestone_assets::ResourceLocation` — the only external type this module
  uses, for the resolved sprite path on each [`PatternLayer`].
- Nothing else. In particular, no dependency on `lodestone-game` (see "How
  to change it" above) and no GPU handle of any kind.

## Consumers, once the two prerequisites land

- **#23's banner block-entity renderer** calls `banner_pattern_layers` with
  the block entity's decoded base colour + pattern list, gets back the
  ordered draw list, and issues one draw per entry over its own flag mesh.
- **The banner/shield item icon** (chest's `SpecialIconDraw` in
  `crates/lodestone-shell/src/hud/item_icon.rs` is the shape to follow —
  currently off-limits to this task, owned by the cost-screens agent) would
  do the same over a GUI-posed instance of the same mesh.
- **Shield in the first-person hand / third-person held item**
  (`crates/lodestone-shell/src/gpu/first_person.rs`, this task's ownership)
  is the same call again, once the mesh exists.

None of the three exist today; this module is what all three will share
instead of each re-deriving `submitPatterns`' layer math independently.

## Steps D–F: landed

Built this pass (issue #174), following the handoff research below almost
unchanged — re-verified against `.cache/mc/26.2/client-src/` line by line
before trusting it, per `CLAUDE.md`'s "re-verify before routing around"
rule, and every claim in the original handoff held up.

- **D (mesh).** `lodestone_assets::block_entity_models::banner_body_model`/
  `banner_flag_model` — the standing pole+bar and the flag, exactly the
  `BannerModel.createBodyLayer(true)`/`BannerFlagModel.createFlagLayer(true)`
  box coordinates below, baked through the existing `CubeDef`/`PartDef`/
  `bake_entity_parts` pipeline with two new `BLOCK_ENTITY_MODELS` entries
  (`"banner_body"`, `"banner_flag"`). Fourteen new tests in that crate pin
  the part hierarchy, the sheet size and — measured through the real baked
  quads, not the literal `addBox` arguments restated — that pole, bar and
  flag stack contiguously along the model's Y-down staff axis.
- **E (resolver).** `lodestone_render::block_entity::banner_ground_placement_matrix`
  (the `T * R * S` composition below, verified against `Transformation.compose`'s
  literal `translation().rotate().scale()` sequence in
  `com.mojang.math.Transformation`, not assumed), `banner_phase`/
  `banner_flag_x_rot` (the sway formulas, unit-tested as exact-value
  predictions — `cos` is exactly `1`/`0`/`-1` at three chosen phases, so
  every intermediate multiply is exact rather than approximate), and
  `BlockEntityModelSet::resolve_banner` returning a new `BannerInstances`
  (`body`/`flag` — opaque, ride the ordinary `plan_block_entities` batcher —
  plus `layers: Vec<BannerLayerDraw>`, the small ordered translucent draw
  list this doc always said the pattern masks would need). Only
  ground/standing; wall is a natural second entry with the same shape (see
  `banner_ground_placement_matrix`'s own doc) and was not built — no more
  reason to build it now than when this doc first named it as a follow-up.
- **F (pixel gate).** `crates/lodestone-render/tests/banner_pattern_layer_pixels.rs`,
  `#[ignore]`d, drawing the **real** `resolve_banner` flag mesh/transform
  through the **real** `EntityPipeline::banner_layer_pipeline`, with
  directly-constructed 1×1 solid-colour fallback textures standing in for
  real pattern-mask sprites (the real banner-pattern atlas — see "The jar
  ships individual sprite PNGs" above — was still not built at the time;
  nothing in that pass claimed otherwise). **A later pass added a third
  test**, `real_creeper_pattern_reaches_pixels_with_its_real_alpha_shape_
  not_a_uniform_rectangle`, once the real `BannerPatternAtlas` landed — see
  "The jar ships individual sprite PNGs" above for the measured counts and
  what that test proves that the two fallback-texture tests below
  structurally could not (a real, spatially-varying mask actually reaching
  the screen, not just a decoded blob). Two *original* tests: submission
  order decides the visible
  colour at full alpha (and each survivor is byte-identical to that colour
  drawn alone — the coincident-depth gate's own anti-vacuity shape); and a
  partial-alpha layer's composite moves monotonically from the layer beneath
  it toward its own colour as alpha rises, landing meaningfully far
  (`> 40` of `255`) from what the *ordinary* opaque/cutout pipeline would
  produce instead. **Verified failing**, not just passing: swapping
  `banner_layer_pipeline` for the ordinary `EntityPipeline`'s pipeline in
  that file and re-running caught exactly one thing — a step-function
  discard-then-overwrite can satisfy every non-strict monotonic inequality,
  which is why the gate also requires the *mid*-alpha sample to differ from
  **both** anchors by more than rounding noise, not just be monotonically
  ordered between them. That control is recorded in the test's own doc
  comment (`the_composite_moves_from_destination_toward_source_as_alpha_rises`),
  not only in this file, so it survives the next person reading the test in
  isolation.

**A real, measured surprise, worth keeping regardless of what built it.**
The first attempt at the pixel gate tried to predict an exact composited
byte from `ALPHA_BLENDING`'s textbook formula (`src·a + dst·(1-a)` in linear
light) using the fragment's raw alpha byte. It was wrong: on this machine's
backend (Metal), a 12-point sweep showed the *effective* linear-space mixing
factor is a real, repeatable, monotonic function of the raw alpha byte, but
not the identity, not `linear_to_srgb(raw)`, and not any single power law
tried against it — raw `0.502` behaves like `~0.76`, raw `0.251` like
`~0.44`, raw `0.031` like `~0.08`. Something in this backend's SRGB-target
blend path reshapes the alpha factor before applying it. The gate that
landed does not depend on knowing that curve's exact shape — see its own
doc comment for why — but whoever next tunes a real antialiased mask edge's
alpha should expect its *effective* coverage in the final image to run
ahead of its raw stored byte, and should re-measure on their own machine
rather than trust either backend-implied number above.

**All three of these are now landed. The list is kept because the shape of
each hop is still the right description of how a banner reaches the screen —
only the "not built" verdicts are stale.**

1. **Landed: the shell installer.** `RenderState::set_banner_source` exists,
   `Sim::banner_source` produces it and `app::redraw` re-installs it every
   frame (it captures the game tick *and* the partial tick, so a stale one
   freezes every banner's sway — `set_bell_source`'s hazard exactly).
   `prepare_block_entities` appends `body`/`flag` into the shared `instances`
   list and returns `layers` as a second, **ordered, unbatched**
   `Vec<BannerLayerDrawBatch>` that `frame.rs` draws through
   `banner_layer_pipeline` right after the opaque block entities.
2. **Landed: the NBT gather.** `block_entities::banner_spawns` reuses the
   `sign_candidates` shape (the second gather in that module that reads
   `be.nbt`), with `standing_banner_colour` taking the base colour off the
   *block name* — sixteen separate banner blocks, not a `color` property, and
   grepping for a property there is the natural mistake that draws every
   banner white — and `banner_patterns` parsing the `"patterns"` list.
   **Both ids are namespace-stripped**: the mask atlas keys on the bare asset
   id (`"creeper"`), so passing `"minecraft:creeper"` through resolves nothing
   and drops the layer silently.
   `*_wall_banner` returns `None` on purpose: its body is
   `createBodyLayer(false)`, a mesh the corpus does not build, so the standing
   rig would hang a full pole in mid-air.
3. **Landed. `lodestone_assets::banner_pattern_atlas::BannerPatternAtlas`**
   loads every real `entity/banner/*.png` mask, discovered through the real
   `atlases/banner_patterns.json` directory-source descriptor (resolved via
   the existing `AtlasDefinition`/`AtlasSource::resolve` machinery — the same
   "ask the thing that actually enumerates it" rule the panorama fix
   established, not a hand-transcribed filename list). Measured directly
   against the real 26.2 jar: **44** real PNGs under `entity/banner/`
   (`banner_base.png`, the plain cloth texture, excluded — not a pattern
   mask) and **43** pattern masks matching all 43
   `data/minecraft/banner_pattern/*.json` registry entries exactly, both
   directions. `banner_pattern_layer_pixels.rs`'s
   `real_creeper_pattern_reaches_pixels_with_its_real_alpha_shape_not_a_
   uniform_rectangle` swaps the real `base`/`creeper` masks in for two of the
   fallback textures and proves the real, spatially-varying alpha actually
   reaches pixels (a hole where alpha is `0`, full coverage where it is
   `255`, a genuine partial blend where it is `191` — creeper.png's real
   measured antialiased-edge value), with the same texture bound as a
   uniform "control" producing **zero** holes (`hole=0, full=2592` exactly,
   verified failing). This is individually addressable `Image`s per pattern
   id, **not** a stitched GPU atlas — the "do not build atlas stitching for
   this" decision below is unaffected; see that section before assuming this
   changes the calculus.

The one join worth a gate, and the one that has one: the sprite key. A
`BannerLayerDraw::sprite` is a full `minecraft:entity/banner/<id>` location
while `BannerPatternAtlas` keys on the bare `<id>`, and if the bridge between
them is wrong every layer is skipped and the banner draws blank white — an
entirely plausible-looking banner. `gpu::tests::banner_masks_resolve_under_
the_key_the_draw_site_derives` checks that bridge on a real two-layer pattern
stack, and also that layer 0 carries the block's dye rather than white.

**Still not built:** wall banners (a second body mesh), and the shield form
(`shield_pattern_layers` exists and still has no consumer — a shield is an item
model in the hand, not a block entity, so it is a different pass entirely).

### The two model classes, decompiled directly

`BannerModel.createBodyLayer(standing: bool)`
(`.cache/mc/26.2/client-src/net/minecraft/client/model/object/banner/BannerModel.java`):

```text
if standing:
  "pole": texOffs(44, 0), addBox(-1, -42, -1,  2, 42, 2), pose ZERO
"bar":   texOffs(0, 42),  addBox(-10, standing? -44 : -20.5, standing? -1 : 9.5,  20, 2, 2), pose ZERO
```

`BannerFlagModel.createFlagLayer(standing: bool)` (same package):

```text
"flag": texOffs(0, 0), addBox(-10, 0, -2,  20, 40, 1),
        pose offset(0, standing? -44 : -20.5, standing? 0 : 10.5)
```

Canvas is `64x64` for both layers (`LayerDefinition.create(mesh, 64, 64)`).
This session's own `BLOCK_ENTITY_MODELS`/`bake_entity_parts` pipeline (see
`lodestone-assets::block_entity_models`, `bell_model` for the shape to
follow) already knows how to turn `addBox`/`texOffs`/`PartPose.offset` into
a `BlockEntityMesh` — a banner needs one more entry there, not a new baker.
Only the **standing** (`standing = true`) variant is this issue's scope —
wall banners are a second entry later, same shape.

**The flag's sway is one per-part rotation, not per-vertex** (corrected
earlier in this doc — re-check "A third prerequisite" above if starting
from an older summary):

```text
flag.xRot = (-0.0125 + 0.01 * cos(2*PI*phase)) * PI
```

`phase`, from `BannerRenderer.extractRenderState`:

```text
phase = (floorMod(blockPos.x*7 + blockPos.y*9 + blockPos.z*13 + gameTime, 100) + partialTicks) / 100.0
```

— i.e. a per-block-position phase offset (so neighbouring banners do not
sway in lockstep) advancing one step per game tick, wrapped every 100
ticks. `BlockEntityMesh::part_transforms`'s `overrides` mechanism (already
used by chest's `lid`/`lock` and bell's `bell_body`) is the right shape for
this: override the `flag` part's `x_rot` per frame, same as
`resolve_bell` overrides `bell_body`.

### The placement transform — the part that needs care

`BannerRenderer.submit` does `poseStack.mulPose(state.transformation)` where
`state.transformation` is (ground-placed banners; wall banners use
`direction.toYRot()` in place of the rotation-segment angle):

```text
MODEL_TRANSLATION = (0.5, 0.0, 0.5)
MODEL_SCALE       = (0.6666667, -0.6666667, -0.6666667)   // note the Y *and* Z flip
Transformation(MODEL_TRANSLATION, Axis.YP.rotationDegrees(-angle), MODEL_SCALE, rightRotation = null)
angle = RotationSegment.convertToDegrees(segment)   // segment * 22.5, ground; already have this helper's shape in skull_ground_placement_matrix
```

A `Transformation(t, r, s, null)` composes as `M = T * R * S` (scale in
local space first, then rotate, then translate) — **not** the
"translate-rotate-about-pivot-then-undo" shape
`block_entity_placement_matrix`/`skull_ground_placement_matrix` already use
in this file. Those two exist because chest/skull geometry is baked
*corner*-anchored (vertices already live inside the block's `0..1` footprint,
so rotating in place needs a pivot). Banner geometry is *not*
corner-anchored — `BannerFlagModel`'s own `PartPose.offset` already
positions it relative to an origin the same way an entity's skeleton does —
so the straight `T * R * S` vanilla itself uses is the right shape here, a
**third** placement convention alongside the two the module doc at the top
of `block_entity.rs` already tables. Concretely, for a ground banner:

```text
world = translate(block_pos)
      * translate(0.5, 0, 0.5)
      * rotate_y(-angle_degrees)
      * scale(2/3, -2/3, -2/3)
      * model_vertex
```

The `2/3` scale and the Y/Z flip both being present is not a typo to
"simplify away" — `BannerModel`/`BannerFlagModel` are shared with the
banner **item**'s GUI/held-item render (`SIZE = 0.6666667` is the same
constant vanilla's item-in-hand code uses elsewhere), so the in-world block
entity path re-applies that same correction on top of otherwise
entity-style baked geometry. Skipping the flip renders the flag upside down
and mirrored; skipping the scale renders it 1.5x too large.

**Landed as `banner_ground_placement_matrix`** (ground only —
`banner_wall_placement_matrix` is still not built, same shape, see "Steps
D–F: landed" above). Untested matrix code is exactly the trap this repo's
own history warns about (a positive-determinant guess shipped an inside-out
block once), so it shipped with `banner_ground_placement_preserves_orientation`
(measuring the determinant's *sign*, not asserting it — the two axis flips
above are individually real and confirmed by
`banner_ground_placement_flips_y_and_z_but_not_x`, and their product is what
keeps the sign positive) rather than only a screenshot.

### Draw order and pipeline routing (step F)

`submitBanner` draws, in order: (1) the banner's own body model (pole+bar)
opaque, sheet `Sheets.BANNER_BASE` — the plain wood/cloth texture, not a
pattern; (2) the flag model, same opaque pass, same sheet; (3)
`submitPatterns`: the base-colour mask (`Sheets.BANNER_PATTERN_BASE`,
`entity/banner/base`) tinted by the banner block's own colour, then up to 16
pattern masks in the item's stored order, each `entity/banner/<pattern
asset id>` tinted by that layer's dye — **all** through
`RenderTypes::bannerPattern`, i.e. `banner_layer_pipeline`. So: 1–2 go
through the ordinary `plan_block_entities` batcher (opaque, this doc's
"send the opaque flag/pole/bar through the existing batcher" conclusion),
and 3 is the small ordered draw list, **N+1 entries long** (base colour
counts as entry 0), each reusing the flag part's own transform (masks paint
over the flag, not the pole/bar).

Real mask sprites need the banner-pattern atlas
(`assets/minecraft/textures/entity/banner/*.png`, 43 real pattern masks —
"The jar ships individual sprite PNGs" above has the loader shape) — **now
landed** as `BannerPatternAtlas`; see "Steps D–F: landed" above. Step F's
*original* pixel gate injects a directly-constructed 1×1 solid-colour
texture per layer instead (the same "an unresolved sheet particle draws
nothing rather than garbage" shape `RenderState::install_particle_sheet_atlas`'s
fallback uses one crate over) and asserts (a) the blend is genuinely
translucent — a partial-alpha composite moves monotonically between the
two layers' own tints as alpha rises, landing far from what the *ordinary*
opaque pipeline would produce instead, not merely "some pixels changed" —
and (b) two layers submitted in opposite orders produce different
composited colour where they overlap, the concrete, measurable form of
"these draws are order-dependent" this doc argued from the start. A later
pass's third test swaps in the real atlas for two of the layers instead —
see "Steps D–F: landed" above. See `banner_pattern_layer_pixels.rs`'s own doc
comment for why the original plan (predict an exact composited byte from
the textbook `ALPHA_BLENDING` formula) had to change: this backend's
effective blend alpha is measurably not the raw fragment alpha byte.
