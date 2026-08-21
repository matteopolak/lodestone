# The Tab player-list overlay

## What it is

The list of online players the game shows while Tab is held, ported from vanilla's
`PlayerTabOverlay`: a translucent plate, one striped row per player carrying that
player's display name and a five-band ping icon, split into columns past twenty
players, with the server's header above and footer below when it sent them.

## How it works

Three layers, and the split is deliberate — each answers a question the others
cannot.

| layer | lives in | question |
|---|---|---|
| state | `lodestone_game::tablist::TabList` | who is online, and what does the server say about them |
| projection | `lodestone_shell::tablist::tab_list_view` | what does one frame of the overlay contain |
| geometry | `lodestone_shell::hud::TabPanel` | where does each of those pieces go |

The wire end was never the problem. `PLAYER_INFO_UPDATE` and `PLAYER_INFO_REMOVE`
are decoded by the `v770` adapter (our own server encodes all three sites),
`TAB_LIST` emits `ClientEvent::TabListChanged` and routes through the `SESSION`
router into `TabList::apply`, and `SessionTabList` is what `Sim::tab_list_view`
reads. **The defect was in the draw**: `HudFrame::players` used to be
`Option<&[String]>` — a flat list of pre-formatted `"NAME  30ms"` rows — plus a
`"PLAYERS (n)"` caption vanilla has no equivalent of. Every link was green, so
`cargo xtask connectedness` reported the same output before and after; what the
flattening destroyed was the game mode, the styled display name and the latency
*band*. This is the "fully-connected wire carrying the wrong value" shape, and the
instrument is silent on it rather than wrong.

### The projection — `tablist.rs`

`tab_list_view` does what the fold deliberately does not:

- resolves each entry's `display_name` (else its plain profile name, run through
  its scoreboard team — see below) into **styled spans**, so a server that
  colours a name gets a coloured row — `PlayerTabOverlay.getNameForDisplay`;
- applies vanilla's `limit(80)` **after** sorting, so a 200-player server shows
  the alphabetically-and-by-order first 80 rather than 200 rows off the bottom;
- turns the raw latency into one of six sprite ids via `ping_sprite`.

**An explicit `display_name` was never the only source of a coloured row, and
for a while this projection only checked that one.** `getNameForDisplay` reads
`info.getTabListDisplayName() != null ? decorate(explicit) : decorate(team-
formatted(plain name))` — a player with **no** explicit display name still gets
coloured by their scoreboard team (`PlayerTeam.formatNameForTeam`), which is
the more common case in practice: a server that runs `/team modify <team>
color` never sends a display-name component at all. `tab_list_view` now takes
the session's `Scoreboard` and, when an entry carries no explicit name, runs it
through `Scoreboard::display_name_of` — the same team-decoration function
`Sim::sidebar` already used for the scoreboard sidebar, so this closes the gap
by *reusing* a fold that existed, not writing a new one. An explicit
`display_name` still wins outright, matching `getNameForDisplay`'s own
short-circuit.

**Per-player hex colour on an explicit `display_name` was correct end to end
all along** — `tab_list_view` has resolved through styled spans since the day
it was rewritten, and the draw uses `text_spans`/`draw_spans`, which handles
`TextColor::Rgb` the same way chat does. The wire hop that actually dropped it:
`v770`'s `player_info.rs` decoded `UPDATE_DISPLAY_NAME` through
`plain_text_from_nbt_component` (a name only, no style at all) rather than
`Text::from_nbt` (the decoder chat's own adapter already uses for the same wire
shape), so a component tree with real style never survived past that one
packet — a protocol-crate fix, reported for brokering rather than made here
(`crates/protocol/**` is off limits to this doc's own author). Nothing about
the shell-side chain needed changing for that half once it lands.

Sort order is `TabList::ordered`, which is `PLAYER_COMPARATOR`: descending
`list_order`, then spectators last, then team name, then profile name
case-insensitively. Team membership belongs to the scoreboard, so the default
omits that key and `ordered_by` takes it from a caller that has one.

Only *listed* players appear (`getListedOnlinePlayers()`). Tab-completion in chat
reads the **unfiltered** set (`getOnlinePlayers()`), which is why the two must not
share a projection — see `docs/chat.md`.

### The geometry — `hud.rs`

`TabPanel::new` is `PlayerTabOverlay.extractRenderState`'s arithmetic. Read it
knowing three things:

- **Vanilla's metrics, not the HUD's.** The overlay draws at `TAB_TEXT_SCALE`
  (`1.0`) with a `TAB_LINE_H` of `9` in the already-`gui_scale`-divided logical
  canvas, exactly like the F3 overlay. The rest of `build_inner` runs at the HUD's
  2× text scale; drawing this overlay there is what "the text is way too big"
  means.
- **The column loop has to be read in Java's own order.**
  `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
  evaluates condition, body, *then* update, so `cols` is bumped before `rows` is
  recomputed. 20 players stay in one column of 20 (the guard is `> 20`, not
  `>= 20`); 21 become **two columns of 11**, not two columns of 20 with nine empty
  rows of plate hanging below; 41 become three of 14.
- **Slots fill column-major** — `col = i / rows`, `row = i % rows`. A row-major
  reading produces a list that reads across instead of down, and on any list of 20
  or fewer the two are indistinguishable.

Every division is vanilla's **integer** division and is floored for that reason;
`slot_w` in particular is `min(estimate, screenWidth - 50) / cols`, and letting it
stay fractional puts column 1 half a pixel off at most widths.

Colours are transcribed, and the second one is the one that gets guessed wrong:
the plates are `Integer.MIN_VALUE` (`0x80000000`, black at alpha 128) but the
per-row fill is `getBackgroundColor(553648127)` = `0x20FFFFFF` — **white** at
alpha 32. Reading it as another black wash makes the rows read as one flat block
instead of a striped list. A spectator's name is `0x90FFFFFF` rather than opaque
white.

**The `0x20FFFFFF` constant itself is correct, and the "too bright" report is
real anyway — check the blend, not the number.** `TAB_ROW_FILL`'s numeric value
matches vanilla exactly (confirmed against `Options.getBackgroundColor`'s real
default), and vanilla blends it directly on raw gamma bytes — it is not
colour-managed. This HUD's colour-quad pipeline (`hud.wgsl`) writes the same raw
value straight through with no gamma correction, but the render target it lands
on is an `Srgb`-format view — native `wgpu-core`'s `Surface::get_default_config`
sorts sRGB formats first, so the swapchain format has been `Bgra8UnormSrgb` on
native since before this fix existed. Writing/blending onto an `Srgb` view
happens in
**linear** light — the hardware decodes the stored byte, blends, then
re-encodes — so a low-alpha white overlay composites brighter than the same
nominal blend does on vanilla's raw bytes. Computed and measured (a headless
`Rgba8UnormSrgb` render, compared against both the gamma-blend and the
linear-blend hypotheses): the divergence is background-dependent and large
against a dark background (tens of `/255`), shrinking toward zero as the
background approaches white — exactly the "fixed point at black and white,
diverges most in between" shape a gamma/linear blend mismatch produces, and
exactly why the black `TAB_PLATE` never looked wrong while the white row fill
does (black is a fixed point of the sRGB transfer curve; blending it is
identical in both spaces).

The fix is not a constant change — CLAUDE.md's colour-space rule is explicit
about that trap. It needs the HUD's flat-colour draws to blend on a **non-sRGB**
view of the same swapchain texture (source *textures* — the font, GUI atlas,
item icons — can stay sRGB-sampled regardless, since sampling gamma-correctness
is independent of the blend target's format), which is a `gpu.rs`/render-target
change outside this crate's ownership boundary. Reported for brokering rather
than made here.

**Landed: the render-target-level primitive.** `RenderTarget::raw_view_format`
(default `= format()`, correct when the base format is already non-sRGB) plus
`AcquiredFrame::create_view(format)` now exist in `lodestone_render::target`,
implemented for both `HeadlessTarget` and `SurfaceTarget`. Both declare *both*
the sRGB and non-sRGB counterparts of their configured format in
`view_formats` up front, so a caller can legally request either view of the
identical texture at any time. This is symmetric with the existing
corrected-view mechanism (`choose_view_format`) that fixed the wasm-darkness
bug, and does not change what `RenderTarget::format()` reports anywhere — only
adds the second, raw accessor. **Wiring `hud.rs`'s flat-colour pipeline
(`self.pipeline`, `hud.wgsl`) to actually draw through it is still open** —
`render_with_item_models`'s `hud-colour-pass` needs a second `raw_view`
parameter threaded from `app/redraw.rs`'s `frame.create_view(target.raw_view_format())`,
and `HudRenderer::new`'s `color_format` argument (which only ever feeds
`self.pipeline` — the `attach_gui`/`attach_items`/`attach_glint`/
`attach_item_models` calls already take their own separate `color_format` and
should keep using the corrected one) needs to become the raw format at its
`app/lifecycle.rs` call site. Reported verbatim for brokering; see the
`lodestone-shell`-owning agent's dispatch record for the exact patch.

**A GPU-verified measurement, and a correction to this doc's own "fixed
point at black and white" phrasing.** A headless gate
(`hud_flat_colour_blend_matches_vanilla_gamma_on_a_raw_target`,
`crates/lodestone-shell/src/gpu/pixel_gates.rs`) builds the real `hud.wgsl`
pipeline directly and sweeps the background from black to white. Re-deriving
the arithmetic independently (never trust a doc's prose over the transfer
function) shows the divergence for *this specific colour pair* —
`TAB_ROW_FILL`'s foreground is white, itself a fixed point of the sRGB curve
in both directions — is **not** a symmetric hump. It is monotonically
decreasing: largest against a dark background (≈67/255 at `bg=0`) and
shrinking smoothly to exactly `0` only at `bg=255`, where foreground and
background coincide. That is the shape the *other* clause of this doc's own
sentence already said ("large against a dark background..., shrinking toward
zero as the background approaches white") — the "fixed point at black and
white" phrasing describes a different, incorrect shape for this pair and
should be read as superseded by that clause.

**Landed: the flat-colour pass now draws on the raw view.** The wiring the
paragraphs above described as open is done. Three pieces, and the third is what
makes it survivable:

- `app/lifecycle.rs` builds the HUD with `HudRenderer::new(device,
  target.raw_view_format())`. That argument feeds `self.pipeline` and nothing
  else; the `attach_gui`/`attach_items`/`attach_glint`/`attach_item_models`
  calls keep taking `target.format()`, because their pipelines draw into `view`.
- `app/redraw.rs` obtains the pass's attachment from
  `HudRenderer::flat_colour_view(&frame)`.
- `HudRenderer` stores the format it compiled that pipeline against and hands
  out the matching view itself. The previous attempt failed because those two
  facts lived in two files and drifted apart; `flat_colour_view` is that
  agreement made structural rather than remembered.

**Why this did not reproduce the earlier revert.** That attempt changed
`HudRenderer::new` while the flat-colour verts were still drawn in the *same*
render pass as the sprite/glint/model pipelines. A `wgpu` pass fixes one
attachment format for every pipeline drawn into it, so the item pipelines could
not draw at all and inventory icons and air bubbles disappeared. `HudRenderer`
has since split the flat-colour stream into its own pass in both entry points
(`hud-colour-pass` in `render_with_item_models`, and the chrome/count passes in
`render_recipe_book_panel`), which is exactly what lets the two formats coexist.
The textured passes were re-run after the change and are unmoved:
`container_item_pixels` 5/5, `container_item_pixels_scaled` 2/2,
`hotbar_special_item_pixels` 9/9, `hotbar_block_item_pixels` 1/1,
`air_bubble_pixels` 1/1, `container_background_pixels` 1/1.

**A real `wgpu` validation failure the same wiring closed.**
`the_recipe_book_draws_under_the_carried_stack` aborted with *"the RenderPass
uses textures with formats [Some(Rgba8Unorm)] but the RenderPipeline with
'hud-pipeline' label uses attachments with formats [Some(Rgba8UnormSrgb)]"* —
a gate that had already threaded a raw view while its renderer was still built
for the corrected one. Production dodged the identical mismatch only because
`app/redraw.rs` was passing `target.format()` there, which made the "raw" view a
second corrected view. That is the drift `flat_colour_view` removes.

**What is verified, and what is not.** The blend itself is measured by
`hud_flat_colour_blend_matches_vanilla_gamma_on_a_raw_target` (still passing,
sweep unchanged: at `bg=0` the RAW target reads 32 against a predicted 32.00
while the corrected target reads 99, 67/255 too light, collapsing to 0 at
`bg=255`), and the *pairing* by
`hud::tests::the_flat_colour_pass_blends_on_gamma_bytes_at_the_surface_format`,
which drives a real `HudRenderer` at native's own `Bgra8UnormSrgb` and carries
a wrong-wiring control. Both build their own `HeadlessTarget`. **Neither reaches
a real swapchain**, so what is proven is that the renderer and the wiring rule
agree; that `SurfaceTarget` reports the format pair those gates assume is a
separate claim, carried by `target.rs`'s own `view_formats` declaration.

## How to change it, and the gotchas

**There is no player head, and on every server we can host that is vanilla's own
behaviour.** `extractRenderState` gates the 8×8 face on
`showHead = connection.onlineMode()`, which comes from the LOGIN packet's
`onlineMode` field. Our server writes `false` there (`v770`'s
`server_protocol`), so vanilla joined to it draws no head either, and
`TabListView::show_head` being `false` reproduces exactly that layout — the 9 px
is reserved only when the flag is set. Turning heads on for an online-mode server
needs **two** things, and neither is in this subsystem:

1. the client-side decode of `onlineMode` off the LOGIN packet (it is currently
   write-only, in the server encoder);
2. a texture path in the HUD render pass. The HUD has a colour pipeline and one
   GUI-atlas sprite pipeline; a per-player skin is a third, with a bind group per
   distinct texture. `remote_skins.rs` already fetches and decodes the sheets for
   the *world* entity pass, so the data is there — what is missing is the pass.

**Do not fabricate a header or footer.** A vanilla server sends neither unless a
plugin or a datapack sets one, which is why vanilla's own tab list shows neither.
`banner_lines` returns an empty `Vec` for absent, empty and whitespace-only
banners, and the draw skips the whole plate — not just the text — so an absent
banner leaves byte-identical geometry.

**The scoreboard score column is not modelled.** Vanilla's `widthForScore` needs a
display objective on the `PLAYER_LIST` slot, which this overlay is not given;
`TabPanel` therefore hardcodes it to `0`. Adding it means threading the objective
plus a `Scoreboard` in, and the hearts render type brings its own blink-state
machine (`PlayerTabOverlay.HealthState`). The sidebar is a separate surface and is
unaffected — see `overlay.rs`.

**Italics are not modelled.** `decorateName` italicises a spectator's name as well
as dimming it; this font has no italic variant and a fabricated slant would be
worse than the dimming alone.

Adding a row field means touching `TabListRow`, the projection, and the draw. The
layout is a separate type on purpose: the draw constructs one `TabPanel` and draws
from it, and a gate constructs one from the same inputs and measures against it, so
a gate cannot keep passing against an overlay that has moved.

## Configuration

None of its own. It follows the GUI scale through `menu::render::logical_canvas`,
and `MAX_TAB_ROWS` (80) and `TAB_MAX_ROWS_PER_COL` (20) are vanilla constants
rather than settings.

## Dependencies

- `lodestone_game::tablist` for the folded state, and `lodestone_game::text` for
  `translate` resolution in display names and banners.
- `lodestone_ecs::SessionTabList`, the one fold, read by `Sim::tab_list_view`.
- The `gui/sprites/icon/ping_*` sprites out of `lodestone_render::GuiAtlas`, bound
  by `HudRenderer::attach_gui`. With no atlas attached (a jar-less run) the bars
  draw nothing and the rest of the overlay is unaffected.
- `crate::overlay::plain_spans`/`spans_text` for the span vocabulary the rows share
  with the scoreboard sidebar.
