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
  this is the plain tab-list name.
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

`see_through` is `Entity.isDiscrete()` (`isShiftKeyDown()`, bit `0x02` of the
shared flags byte, `Entity.java:262`/`:2703`) — sneaking suppresses the
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
  (`SubmitNodeCollection.java:104`).
- **Anchor**: `feet.y + base_height * scale + 0.5` — `base_height` from
  `lodestone_data::entity_dimensions::base_dimensions` (the real jar-derived
  hitbox census), not a guessed constant. This is vanilla's
  `EntityAttachment.NAME_TAG` fallback (`AT_HEIGHT`, `EntityAttachment.
  java:9`/`:25`) plus `SubmitNodeCollection.java:103`'s `+0.5`. Per-type
  attachment overrides (a sitting cat, a sleeping villager) are not ported —
  every entity uses the fallback, which the overwhelming majority (players,
  every standard mob) genuinely get.
- **Distance cutoff**: `64.0` blocks, squared-distance from the camera to
  the entity's **feet** (`EntityRenderer.java:246`/`:252`), not the tag
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

The shadow copy is a quarter-brightness version of the **glyph's own**
resolved colour (`Font.java::getShadowColor`'s no-explicit-colour branch,
`ARGB.scaleRGB(textColor, 0.25F)`), not a flat grey constant — a coloured
name's shadow is a dim version of that same colour.

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
| normal | `DepthStencilState.DEFAULT` = `(GREATER_THAN_OR_EQUAL, writeDepth=true)` (`DepthStencilState.java:6`) | `CompareFunction::LessEqual`, `depth_write_enabled: true` |
| see-through | `Optional.empty()` — no depth attachment at all (`RenderPipelines.java:507`) | `CompareFunction::Always`, `depth_write_enabled: false` |

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

Colours (`SubmitNodeCollection.java:113`/`:117`): normal is opaque white
(`-1`); see-through is `0x81_FFFFFF` — white at alpha `129/255 ≈ 0.506`. Both
use plain alpha blending; with the normal pass's alpha at `1.0` the blend is
a no-op there, so draw order between the two passes does not affect the
final pixel wherever both would cover the same texel.

Both passes are drawn last in `gpu.rs`'s single "block pass", after
`debug_lines`, so they read the same depth buffer terrain and every entity
already wrote this frame — see `RenderState::render_inner`.

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
  `SubmitNodeCollection.java:104`'s call order first.
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
  deliberately not baked into the cached geometry — the normal pass, the
  shadow copy and the see-through pass all draw the *same* cached layout at
  three different alphas, and `gpu/display_text.rs` draws it at a
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

- **The background plate.** Vanilla draws a translucent black quad behind
  the glyphs, sized from the `chatOpacity` game option
  (`SubmitNodeCollection.java:108`). Not in the issue's scope checklist and
  not required for legibility (the drop shadow already separates text from
  background) — a genuine gap, not an oversight.
- **Per-frame packed-light modulation.** Vanilla forces near-full brightness
  for the normal pass specifically so a tag stays legible in the dark
  (`LightCoordsUtil.lightCoordsWithEmission(lightCoords, 2)`); this renderer
  draws plain full-bright white unconditionally, which is a close
  approximation of that override rather than a divergence from it.
- **`EntityAttachment` per-type overrides**, the crosshair-look-at override
  to `shouldShowName` (`EntityRenderer.java:113`), scoreboard team
  colouring/prefixes, and the `belowName` scoreboard line — all explicitly
  out of scope per the issue.
- **The local third-person body's own nametag.** `ThirdPersonBodyState::
  into_draw` always sets `name_tag: None` — the camera never needs to read
  its own name off its own head.
- **`§k` (obfuscated) styling** — see the "Style" section above.
- **Colour/bold/italic/underline/strikethrough reaching a *player's* or a
  *mob's* actual on-screen tag** — the draw pass is ready
  (`layout_styled_ink_runs`), but `entities.rs::resolve_entity_facts` and
  `crates/protocol/v770`'s metadata decode both still flatten the source
  `Text` component to plain text before this module ever sees it. See the
  "Style" section's own doc for the exact symbols.

## Verification

- `crates/lodestone-shell/src/gpu/nametag.rs`'s own unit tests (`cargo test
  -p lodestone-shell --lib gpu::nametag`) — the distance cutoff and empty-name
  cases, driven directly against `push_entity_quads` with no GPU.
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

```text
cargo test -p lodestone-shell --lib gpu::nametag
cargo test -p lodestone-shell --lib entities::tests::name_tag
cargo test -p lodestone-shell --test nametag_pixels -- --ignored --nocapture
```
