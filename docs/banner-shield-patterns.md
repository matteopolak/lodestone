# Banner and shield pattern compositing (issue #174)

## What it is

The shared colour/ordering math behind vanilla's banner and shield pattern
system: a base dye colour plus an ordered list of pattern layers, each drawn
as a masked sprite tinted by its own dye colour. `lodestone_render::banner_pattern`
(`crates/lodestone-render/src/banner_pattern.rs`) is that math: a
[`DyeColor`] enum carrying vanilla's real per-colour tint, and
`banner_pattern_layers`/`shield_pattern_layers`, which turn a base colour
plus a pattern list into the exact ordered draw list vanilla itself builds.

**This module does not draw anything yet.** It is pure, GPU-free colour and
ordering logic — see "What is still missing" below for the two reasons
nothing calls it yet, and why that is a scoping finding, not an oversight.

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

## What is still missing (why this lands unwired)

**Re-checked this session (Batch G, #23 + #174), not assumed stale.** Before
touching anything, `grep -rn "banner_pattern_layers\|shield_pattern_layers"`
across the whole tree was run again: the only hits are `lib.rs`'s re-export
and this module's own `#[cfg(test)]` block. **Confirmed still an island** —
nothing outside this crate calls either function, exactly as this doc
already claimed. That grep is the whole basis for treating #174 as a
last-hop problem rather than a maths problem; see `docs/block-entity-renderers.md`'s
Bell section for what *was* built instead this session and why.

Two prerequisites, both outside this task's file ownership, so neither was
built speculatively:

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
2. **No banner/shield mesh.** Vanilla's flag mesh (`BannerFlagModel`) has
   per-vertex cloth-wave animation this codebase has never ported, and
   building it is explicitly issue #23's scope — the issue that owns this
   one says so directly: "the in-world banner block entity rendering itself
   is #23's scope... land the shared compositing function here and have
   #23's banner work consume it." This doc and module are that hand-off
   point. **Still true**: this session built the bell body/rim instead (a
   single static box vs. an animated cloth mesh with a base layer plus up to
   16 further tinted layers — a materially larger effort, and correctly
   triaged as lower value for the "player survives an hour" bar than closing
   a total hole is), so the mesh remains unbuilt.

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

### A third prerequisite, found while landing bell: no per-instance tint

Not named by the original scoping, because it only becomes visible once you
try to actually wire a consumer rather than read the compositing function's
signature. `lodestone_render::block_entity::BlockEntityInstance`/
`BlockEntityBatch` (the shared batching type chest/skull/bell all go through)
carry **one texture stem and no tint** per instance — batching is keyed on
`(model, texture)` alone, and every instance in a batch shares one draw with
no per-instance colour multiply. That is sufficient for chest (material
changes the *texture*, never a tint) and skull (no tint at all) and bell
(same), but `banner_pattern_layers`/`shield_pattern_layers` hand back a
`Vec<PatternLayer>` where **every layer is the same handful of mask sprites
re-tinted by a different `DyeColor`** — a base layer plus up to sixteen
pattern layers, each needing its *own* gamma-space tint over a shared mask.
Vanilla itself draws this as N separate `submitModel` calls, one per layer,
each with a different `diffuseColor` argument
(`BannerRenderer.submitPatternLayer`) — there is no way to fold sixteen
different tints into the current one-texture-per-instance batch shape.

So a real consumer needs `BlockEntityInstance`/`BlockEntityBatch` widened
with a per-instance tint (parallel to the existing per-instance `light: u8`
that already exists for exactly this reason — see that struct's field),
**and** the GPU-side vertex/instance format `EntityPipeline` uses would need
a matching per-instance colour slot threaded through to the shader's tint
multiply. `EntityPipeline` is shared with every mob and every other block
entity, so this is a wider-blast-radius change than adding one more
`(model, texture)` entry was for bell — it is the real reason a banner
consumer is a bigger lift than "port the mesh and call the function", and it
is worth checking this doc before assuming the compositing function alone is
the missing piece.

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
