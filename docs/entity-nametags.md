# Entity and player nametags

## What it is

Billboarded text above every entity with a visible custom name, and above
every other player (issue #100). Before this landed, `EntityView` already
decoded `DATA_CUSTOM_NAME`/`DATA_CUSTOM_NAME_VISIBLE` in full
(`crates/protocol/v770/src/packets/metadata.rs`, indices 2/3), folded them
into real ECS components (`lodestone_ecs::entity::CustomName`/
`CustomNameVisible`, wired into `apply_entity_metadata`'s routing switch),
and reconstituted them onto `EntityView` — but `net::entity_snapshot`, the
one place that lowers `EntityView` into the version-free `EntitySnapshot`
the renderer actually consumes, dropped both fields on the floor. The data
was fully decoded and simply never read past that boundary — the exact
"island" shape this repo's other entity-metadata fixes (velocity, equipment,
variant) already had.

## How it works

### The two different rules

A player's tag and every other entity's tag are resolved by genuinely
different vanilla predicates, both applied once, in
`entities.rs::resolve_entity_facts` (`crates/lodestone-shell/src/entities.rs`)
— **not** at `net::entity_snapshot`, which issue #36 deleted along with
`EntitySnapshot` itself; `resolve_entity_facts` is the bevy-ECS successor this
doc's earlier draft predates (`docs/bevy-migration.md` §4.4 is the wider
record of that move):

- **A player's tag is always its tab-list display name.**
  `Player.shouldShowName()` returns `true` unconditionally, overriding the
  base `Entity.shouldShowName() = isCustomNameVisible()` every other entity
  uses. The name comes from the `tab_list: &TabList` parameter
  `resolve_entity_facts` already threads through for the player-skin lookup
  right below it (`crate::tablist`), matched by the entity's `EntityUuid`
  component. Scoreboard team colouring/prefixes are genuinely part of
  vanilla's `getDisplayName()` for a player and are **out of scope here** —
  this is the plain tab-list name; team visibility is applied separately
  below.
  - **The lookup survives a missing tab-list entry once resolved.** A
    player-type NPC spawned by a server plugin is almost always a fake
    player entity whose profile carries no `CustomName` at all, and whose
    plugin commonly adds a tab-list entry (to declare a skin/name) and then
    removes it shortly after, to keep the fake player out of the visible
    player list while the entity stays spawned — the exact shape a real
    disconnect's `player_info_remove` also takes. Without a fallback, that
    miss silently drops the tag every frame afterwards even though nothing
    about the entity changed — the reported symptom was "NPCs don't have a
    name rendered ... their skins do show up perfectly though," and the skin
    half already survived this (`crate::remote_skins`'s `LAST_KNOWN` cache).
    `remote_skins::remember_name`/`last_known_name` are the name half of the
    identical fix, keyed by uuid the same way.
- **Every other entity's tag is its `CUSTOM_NAME`**, gated on
  `CUSTOM_NAME_VISIBLE` (`LivingEntity.shouldShowName() =
  isCustomNameVisible()`). An entity with `CUSTOM_NAME_VISIBLE=true` but no
  custom name shows nothing here — vanilla would fall back to drawing the
  entity's translated type name (`Entity.getDisplayName()`'s default), which
  is **deliberately not reproduced** (out of scope: the issue's own
  checklist says "a non-empty custom name").

Both resolve to one `crate::entities::NameTag { text, see_through }`, carried
as `EntitySnapshot::name_tag` → `RenderNameTag` (a `bevy_ecs` component,
folded/updated in `entities.rs::spawn_track`/`update_track` exactly like
`RenderEquipment`/`RenderWool`) → `EntityDraw::name_tag`, the same
extract-to-plain-POD boundary every other render-side entity field crosses
(`docs/bevy-migration.md` §4.4).

Before either tag reaches that common output, the client applies the target
team's `name_tag_visibility` rule from the folded `SessionScoreboard`. The
target is looked up by its profile name for player entities (or UUID string
for other entities); the viewer is the local connection profile. `Never`
suppresses a helper's tab-list name, while the two team-relative modes and
`see_friendly_invisibles` follow `LivingEntityRenderer.shouldShowName`.
Invisible armour stands remain the vanilla hologram exception: their renderer
uses only `isCustomNameVisible()`.

`see_through` is `Entity.isDiscrete()` (`isShiftKeyDown()`, bit `0x02` of the
shared flags byte, `Entity.setSharedFlag`/`Entity.isDiscrete`) — sneaking suppresses the
depth-testless pass, resolved once at the same boundary.

### The draw path: `crates/lodestone-shell/src/gpu/nametag.rs`

A new `gpu/` submodule (`docs/gpu-module-layout.md`), following the same
"small, self-contained, world-space colour pipeline" shape
`gpu/debug_lines.rs`/`gpu/outline.rs` already use — one `view_proj` uniform,
no texture, one bind group, well under the model shader's 4-bind-group
floor.

- **Font data**: [`lodestone_assets::font::RasterFont`] loaded directly (the
  same jar-sourced `FontLoader::load_raster("minecraft:default", …)` call
  `hud/vanilla_font.rs::VanillaFont::load` makes), not `VanillaFont` itself —
  its glyph rasteriser is private and its public draw methods target a 2-D
  screen-space `ColourStream` in `hud/item_icon.rs`; both files belong to a
  different area of the render stack. `layout_styled_ink_runs` re-derives the
  same run-length ink walk `VanillaFont::glyph` does, emitting local rects
  instead of screen quads.
- **Billboarding**: `lodestone_render::entity::camera_orientation(camera.
  view_matrix())`, the same already-verified camera→world rotation
  `thrown_item_matrix` uses for projectiles — not a hand-derived
  `cross(forward, up)`, which `docs/thrown-projectiles.md` records getting
  wrong three times running. Every nametag this frame shares one basis
  (`orientation.x_axis`/`.y_axis`), matching vanilla's single
  `camera.orientation` applied identically per tag
  (`SubmitNodeCollection.submitNameTag`).
- **Anchor**: `feet.y + base_height * scale + 0.5` — `base_height` from
  `lodestone_data::entity_dimensions::base_dimensions` (the real jar-derived
  hitbox census), not a guessed constant. This is vanilla's
  `EntityAttachment.NAME_TAG` fallback (`AT_HEIGHT`, `EntityAttachment`) plus `SubmitNodeCollection.submitNameTag`'s `+0.5`. Per-type
  attachment overrides (a sitting cat, a sleeping villager) are not ported —
  every entity uses the fallback, which the overwhelming majority (players,
  every standard mob) genuinely get.
- **Distance cutoff**: `64.0` blocks, squared-distance from the camera to
  the entity's **feet** (`EntityRenderer.extractNameTags`), not the tag
  anchor.

### Style: colour, bold, italic, underline, strikethrough

`gpu/nametag.rs::layout_styled_ink_runs` is the single world-space ink walk:
it takes a fully-inherited `Vec<lodestone_model::text::TextSpan>`
(`crate::entities::NameTag::text.to_spans()` — `NameTag::text` is a real
`lodestone_model::Text`, read directly with no legacy-string bridge in
between) instead of a bare string, and every emitted `StyledRect` carries
its own resolved RGBA colour — matching `Font.java::getTextColor`, which uses
a span's own `TextColor` when set and falls back to the pass's own base tint
(opaque white for both nametag passes) otherwise. Bold draws each ink run
twice, offset by `Font::bold_offset` — the same "redraw the glyph shifted"
technique `Font.java`/`BakedSheetGlyph.renderChar` use, not a font-weight
variant — and widens the *advance* too (`GlyphInfo.getAdvance(bold)`), which
matters for centring multi-line blocks (see `docs/display-entity-orientation.md`,
where this bites `text_display`, the pass that shares this walk). Italic
shears each texel row by `ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v`; underline/
strikethrough are emitted per glyph, matching `Font.java::accept`'s own
unconditional per-glyph effect bar.

**A nametag has no drop shadow** — see "What each submission carries" below
for the record, and for what this pass drew instead before the plate landed.

**`§k` (obfuscated) is not implemented.** It needs per-frame resampling state
neither this renderer nor `gpu/display_text.rs` keeps (unlike
`hud/vanilla_font.rs`, which has one for the 2-D HUD path) — a disclosed gap,
not a silent one.

**The upstream gap is closed.** `entities.rs::resolve_entity_facts` resolves
a player's tag via `TabListEntry::effective_name()` (a `Text`) and a mob's
via the metadata-decoded `CustomName` (also a `Text`) — neither flattens
before building `NameTag`, so `NameTag::text` carries the full component tree
straight through to `push_entity_quads`, which calls `Text::to_spans()` on it
directly. Colour — a hex `TextColor::Rgb` included — bold, italic, underline
and strikethrough all survive: a hex colour is the one thing a
`to_legacy_string`/`Text::from_legacy` round trip could never carry (legacy
`§` codes are a fixed 16-entry palette with no hex form), which is why this
module no longer bridges through one.

### The two depth passes

Reconciled against `.cache/mc/26.2/client-src`'s
`RenderPipelines.java`/`RenderTypes.java`, not guessed:

| | vanilla (jar) | here (`wgpu`) |
|---|---|---|
| normal | `DepthStencilState.DEFAULT` = `(GREATER_THAN_OR_EQUAL, writeDepth=true)` (`DepthStencilState.DEFAULT`) | `CompareFunction::LessEqual`, `depth_write_enabled: true` |
| see-through | `Optional.empty()` — no depth attachment at all (`RenderPipelines.TEXT_SEE_THROUGH`) | `CompareFunction::Always`, `depth_write_enabled: false` |

The normal-pass sign flip is the same one every other depth-tested pass in
this codebase applies: our depth is `[0,1]` DirectX-style, not vanilla's
reversed-Z, so "closer or equal" flips from `GREATER_THAN_OR_EQUAL` to
`LessEqual`.

**The see-through row is not a straight port.** Vanilla's abstraction lets a
pipeline declare *no* depth-stencil state at all; `wgpu` does not have an
equivalent for "this pipeline ignores the pass's depth attachment" while
sharing a render pass that has one — every pipeline drawn inside such a pass
must declare a matching-format depth-stencil state of its own. This was
found the hard way, not reasoned out in advance: an initial
`depth_stencil: None` pipeline validation-errored at draw time
(`Incompatible depth-stencil attachment format: … Some(Depth32Float) but the
RenderPipeline … uses an attachment with format None`), it did not silently
no-op or fall back to something plausible. `CompareFunction::Always` (every
fragment passes — no comparison operator, hence no sign to get backwards)
with `depth_write_enabled: false` is the equivalent-in-effect substitute:
functionally "no depth interaction", expressed the only way `wgpu` allows it
within a single shared pass.

### What each submission carries — and why the plate is in only one

`SubmitNodeCollection.submitNameTag` branches on `!isDiscrete` and the two
arms differ in **three** things at once: glyph colour, background colour, and
which of the two groups they land in. Read the method, not the pipeline
table — the pipelines are identical between the arms and carry none of this:

| | not discrete (the usual case) | discrete (sneaking) |
|---|---|---|
| `nameTags` (depth-tested) | colour `-1` (opaque white), background `0` — **no plate** | colour `-2130706433`, background `getBackgroundOpacity` — **plate** |
| `seeThroughNameTags` | colour `-2130706433` (`0x81_FFFFFF`, white at `129/255 ≈ 0.506`), background `getBackgroundOpacity` — **plate** | *(no submission)* |

So a tag draws its plate exactly once, in whichever group draws **last**, and
that group deliberately paints over the copy the other one already laid down:
`SubmitNodeCollection`'s own phase list orders `nameTags` before
`seeThroughNameTags`, which `NameTagRenderer::draw` matches. The composite
for an ordinary named mob or player is glyph ink at `1 - 0.25·(1 - 0.506)` of
full white over a backdrop the plate has darkened to `0.75` of itself — the
familiar bright-name-on-a-dark-slab look.

Two things here were wrong in this pass until recently, and both came from
reading the *pipeline* table above as if it were the whole record:

- **A sneaking tag's glyphs were drawn opaque.** Vanilla's discrete arm uses
  `-2130706433` in the depth-tested group; the alpha is not a property of the
  see-through pipeline.
- **There is no drop shadow.** `NameTagFeatureRenderer.prepareText` calls
  `Font.prepareText(..., drawShadow = false, ...)`, and
  `Font.PreparedTextBuilder.getShadowColor` returns a fully transparent `0`
  for a style with no explicit shadow colour when that flag is clear — so
  vanilla emits no shadow renderable for a nametag at all. This pass used to
  draw a quarter-brightness shadow copy *instead of* the plate, on the stated
  reasoning that the shadow made the plate unnecessary. That is what made a
  named mob read as bare floating text.

### The plate

Geometry comes from `Font.PreparedTextBuilder.markBackground`, which seeds
the rect at the pen as `(x - 1, y - 1) .. (x + 0, y + 9)` and grows its right
edge by each glyph's advance. `layout_styled_ink_runs` starts its pen at local
`(0, 0)`, so the finished rect is `(-1, -1) .. (total_width, 9)` in logical
font pixels — **asymmetric**: one pixel of padding on the left and top only,
none on the right or bottom. `Font.PreparedTextBuilder.visit` emits it before
any glyph, which is what `push_entity_quads` does within each group's buffer.

Vanilla additionally pushes the plate `-0.01` along local `z`. Nothing here
does and nothing needs to: a nametag billboard's plane is perpendicular to
the view axis, so every vertex in it shares one window depth and the depth
test cannot separate plate from glyphs regardless. Submission order does it,
and `LessEqual` passes the resulting tie. Porting the offset faithfully would
also be inert — through the `0.025` text scale it is `0.00025` blocks, which
is 0–2 ULP of `Depth32Float` at any real viewing distance under this
project's forward `[0,1]` projection (see `docs/shaders.md`).

The colour is black at `Options.getBackgroundOpacity(0.25F)`. That accessor
returns its **fallback** `0.25` unless `backgroundForChatOnly` is turned off,
an option vanilla defaults *on* and this crate does not model — so
`0x40000000` is the faithful value for an unconfigured client, and
`Options::chat_background_opacity` (this crate's chat-HUD slider) is
deliberately **not** threaded in: vanilla's chat-only default means it would
not feed this value either. `gpu/display_text.rs`'s `DEFAULT_BACKGROUND_ARGB`
records the same reasoning for the same accessor, and lands one step darker
(`0x3F000000`) only because `Display.TextDisplay`'s own default truncates
where `ARGB.color(float, int)` rounds.

**The plate's on-screen alpha used to diverge from vanilla, and the constant
was never the cause.** Vanilla composites it on raw gamma bytes; every pipeline
in this pass targeted the swapchain's **sRGB** view, so the hardware blended in
linear light — the same colour-space mismatch `docs/tab-list.md` records for
the HUD's flat-colour stream. For a pure-black source the two agree only at a
black backdrop (white is *not* a second fixed point here, unlike the tab-list
case) and diverge monotonically toward it; re-derived from the sRGB transfer
function, `0.75·bg` (vanilla) against `encode(0.75·decode(bg))` (the bug) is
`0` at `bg = 0`, ≈+7/255 at `bg = 64`, ≈+16/255 at `bg = 128` and ≈+33/255 at
`bg = 255`, so the plate read **too weak against a bright backdrop** (sky) and
close to right against a dark one.

**Fixed structurally, not by tuning the constant** — see
`docs/world-text-gamma-blend.md`. The three flat-colour world-text passes
(nametags, sign text, `text_display`) now draw into a **raw** (non-sRGB) view
of the same colour texture, which because a `wgpu` render pass fixes one
attachment format for every pipeline in it means two extra render passes rather
than a different pipeline in the existing one. `RenderState::set_world_text_view`
installs the view and re-points the pipelines from one expression, and
`app/redraw.rs` calls it once per frame. Measured live at `Bgra8UnormSrgb`
against a sky backdrop of 181: the plate now reads **136**, exactly vanilla's
`181 × 0.749`, where the sRGB pairing gave **159**
(`tests/world_text_gamma_blend_pixels.rs`).

The nametag pass is drawn last of the world's colour work, after `debug_lines`,
so it reads the same depth buffer terrain and every entity already wrote this
frame — see `RenderState::render_inner`. It is its own render pass now rather
than the tail of the "block pass", but nothing about its ordering changed: both
its pipelines still draw in one pass, in the same order, so the plate still
paints over the opaque normal-pass glyphs the way `SubmitNodeCollection`'s
phase list does.

## How to change it, and the gotchas

- **The two rules live at `entities.rs::resolve_entity_facts`, not in
  `gpu/nametag.rs`.** If a scoreboard-team-prefixed player name, or vanilla's
  translated-type-name fallback, is ever wanted, that is a change to the
  *resolution* logic in `entities.rs`, not to the draw pass — the draw pass
  only ever sees an already-resolved `Option<NameTag>` and does not know
  which rule produced it.
- **The player-name fallback cache is per-uuid, not per-frame.**
  `remote_skins::remember_name`/`last_known_name` persist across frames the
  same way `remember`/`last_known` (the skin cache) already do; a fresh uuid
  that was never resolved through the tab list still correctly shows no tag,
  and only a uuid that *was* resolved once inherits its own remembered name
  on a later miss — see `remote_skins.rs`'s own doc on `NAME_LAST_KNOWN` for
  why this needs to be a second, separate map rather than folded into the
  skin one.
- **`wgpu`'s depth-stencil constraint is per render *pass*, not per
  pipeline in isolation.** Any future pass that wants "no depth interaction"
  while sharing this crate's single monolithic block pass needs the same
  `Always`/`write: false` substitute this module uses — a bare
  `depth_stencil: None` will validation-error the moment it shares a pass
  with anything that has a depth attachment, which every pass in
  `render_inner` does.
- **The billboard basis is per-frame, not per-entity.** `NameTagRenderer::
  prepare` computes `right`/`up` once from the camera and reuses it for
  every entity's tag — this is deliberate (it matches vanilla) and also
  the only realistic way to keep this a single small vertex upload; do not
  "fix" it into a per-entity look-at without re-reading
  `SubmitNodeCollection.submitNameTag`'s call order first.
- **`entity_base_height` falls back to `1.8`** for a type path the
  jar-derived census cannot resolve (an unregistered/synthetic type path,
  or the rare `0`-height marker types) — not a crash, not a `0`-height tag
  glued to the entity's feet.
- **`layout_ink_runs`/`layout_styled_ink_runs` are cached, and per-frame
  callers must go through `InkLayoutCache`/`StyledInkLayoutCache`
  respectively** (issue #527 (b)). The ink walk probes `cell_width *
  cell_height` texels per character and its output is pure local space, so
  the unstyled cache depends only on `(text, RasterFont)` and the styled one
  only on `(spans, RasterFont)` — the anchor and the billboard basis are
  applied afterwards. `NameTagRenderer` (styled) and `SignTextRenderer`
  (unstyled — sign lines have no per-run style to carry) each own one cache
  of the kind they need; `gpu/display_text.rs::DisplayTextRenderer` also owns
  a `StyledInkLayoutCache`. **The font is not part of either key**: each
  renderer owns one `RasterFont` for its whole lifetime, so if a renderer
  ever swaps fonts in place, its cache must be cleared at that point or it
  will keep serving the old font's rects.
- **A `StyledRect`'s colour is always opaque (`alpha == 1.0`).** Alpha is
  deliberately not baked into the cached geometry — the normal pass and the
  see-through pass draw the *same* cached layout at two different alphas
  (and which alpha the normal pass takes depends on discreteness, not on the
  pipeline), and `gpu/display_text.rs` draws it at a
  per-entity `textOpacity`-derived alpha. Every caller multiplies its own
  alpha onto the rect's existing `1.0` when building vertices; do not bake a
  pass-specific alpha into `layout_styled_ink_runs` itself.

## Configuration

None. The font, like every other jar-sourced asset in this crate, is
discovered via `LODESTONE_ASSETS` or the highest-sorting `.cache/mc/<ver>`
under an ancestor of the working directory (see `jar_manager`/`pack_root` in
`gpu/nametag.rs` — a deliberate duplicate of `hud/vanilla_font.rs`'s own
discovery snippet, for the same reason that module duplicates it from
`crate::resources` rather than the `#[cfg(test)]`-gated original: see that
module's doc).

## Dependencies

- `lodestone-assets` (`font::{FontLoader, FontOptions, RasterFont, metrics}`)
  — the jar-sourced glyph data.
- `lodestone-data` (`entity_dimensions`, `entity_types`) — new direct
  dependency of `lodestone-shell`, added for this feature; the jar-derived
  per-entity-type hitbox census used for the tag's vertical anchor.
- `lodestone-render` (`Camera`, `DEPTH_FORMAT`, `entity::camera_orientation`)
  — the camera math and the shared depth format.
- `lodestone-model` (`text::{Text, TextColor, TextSpan}`) — the version-free
  chat-component model `layout_styled_ink_runs` resolves style from; see the
  "Style" section above.
- `lodestone-game::tablist::TabList` — already-folded tab-list state, for
  player names.
- `crate::remote_skins` — `remember_name`/`last_known_name`, the per-uuid
  fallback cache for a player-type entity's name surviving a missing
  tab-list entry (see the bullet above), alongside the pre-existing skin
  cache it shares the module with.
- `wgpu` — the pass's own pipeline/shader, ~200 lines, no shared bind groups
  with the model/entity pipelines.

## What is deliberately not built

- **Per-frame packed-light modulation.** Vanilla forces near-full brightness
  for the normal pass specifically so a tag stays legible in the dark
  (`LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)`); this renderer
  draws plain full-bright white unconditionally, which is a close
  approximation of that override rather than a divergence from it.
- **`EntityAttachment` per-type overrides**, the crosshair-look-at override
  to `shouldShowName` (`EntityRenderer.extractNameTags`), scoreboard team
  colouring/prefixes, and the `belowName` scoreboard line — all explicitly
  out of scope per the issue.
- **`backgroundForChatOnly`.** The accessibility toggle that makes
  `getBackgroundOpacity` return the real `text_background_opacity` slider
  instead of its `0.25` fallback. Unmodelled, so the plate is always the
  fallback shade — see "The plate" above.
- **The local third-person body's own nametag.** `ThirdPersonBodyState::
  into_draw` always sets `name_tag: None` — the camera never needs to read
  its own name off its own head.
- **`§k` (obfuscated) styling** — see the "Style" section above.

(A bullet here used to claim that colour/bold/italic/underline/strikethrough
never reached a real player's or mob's tag, because
`entities.rs::resolve_entity_facts` flattened the source `Text` first. That
was true when written and is not any more — the "Style" section's own
"The upstream gap is closed" paragraph, in this same file, records the fix.
A status annotation in a doc is the highest-decay claim in it; check the tree
before inheriting one.)

## Verification

- `crates/lodestone-shell/src/gpu/nametag.rs`'s own unit tests (`cargo test
  -p lodestone-shell --lib gpu::nametag`) — the distance cutoff, the
  empty-name case, the style walk, and the plate, all driven directly against
  `push_entity_quads` with no GPU. The plate gates are
  `a_visible_nametag_draws_its_plate_in_the_see_through_pass_only` (which
  pass carries it, and that the other carries **none** — vanilla's
  `backgroundColor = 0` arm),
  `the_plate_rect_is_vanillas_asymmetric_one_not_the_ink_bounds` (all four
  edges collected and reported together, against the plausible wrong
  hypothesis of an ink-bounds-sized plate), and
  `a_sneaking_nametag_carries_its_plate_in_the_normal_pass`. Neutered by
  deleting both `plate_quad` calls, 3 of 3 failed and the rect gate named all
  four wrong edges; the fourth plate gate (`an_empty_name_emits_no_plate`)
  correctly stays green under that neuter, since its subject returns before
  either call.
- `crates/lodestone-shell/src/entities.rs`'s `tests::name_tag` module — the
  two resolution rules (player-vs-mob, visibility gating, sneaking) pinned
  against `resolve_entity_facts` in isolation, plus the per-uuid fallback
  (`a_players_name_tag_survives_a_missing_tab_list_entry_once_resolved`),
  the name-cache sibling of the pre-existing skin-fallback test right next
  to it.
- `crates/lodestone-shell/tests/nametag_pixels.rs` — the real pixel gates,
  through `RenderState::render`:
  - `a_named_entity_draws_text_pixels_above_it`: a tagged entity against an
    otherwise-identical untagged control, with the pixel-diff bounding box
    checked against an *analytically projected* anchor point (derived from
    the same `lodestone_data` census the render code reads, not a
    remembered literal).
  - `occlusion`: a giant, close occluder entity (real depth-tested-and-written
    geometry via the ordinary entity pass — no terrain harness needed) placed
    between the camera and a distant tagged entity. A sneaking tag (no
    see-through pass) is pixel-identical to the no-tag baseline — this is
    also the control proving the occluder genuinely blocks the depth-tested
    normal pass, per `CLAUDE.md`'s "a control's premise can be false before
    the feature under test existed". A standing tag in the same occluded
    position still contributes real, faded pixels.
- The plate's own **rasterised** A/B, measured through that same
  `a_named_entity_draws_text_pixels_above_it` gate by deleting both
  `plate_quad` calls and re-running: with the plate, the tagged-vs-untagged
  diff is **189 px** filling a **21×9** bounding box exactly (a solid rect);
  without it, **42 px** scattered inside a 20×6 box. The plate is 147 of
  those pixels, and it extends one pixel further left and two further
  vertically than the ink — the asymmetric `-1` pad and the 9-px line height
  reaching real pixels, not just real vertices.

```text
cargo test -p lodestone-shell --lib gpu::nametag
cargo test -p lodestone-shell --lib entities::tests::name_tag
cargo test -p lodestone-shell --test nametag_pixels -- --ignored --nocapture
```
