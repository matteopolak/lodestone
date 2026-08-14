# HUD text scale

## What it is

The pose scale applied to the HUD's own text surfaces, and the record of an ad-hoc, HUD-wide 2× pitch (`HUD_TEXT_SCALE`/`hud_line_h`) that used to double several of them against vanilla. It also records the answer to a recurring design question: vanilla exposes **no** size option for the title/subtitle/action-bar trio, and we should not invent one.

**Status: fixed, and the ambient pitch is gone.** Every element `HudGeometry::build_inner` sizes now uses vanilla's own metrics: the title/subtitle/action bar (`ff132e02`, below), the F3 overlay, the tab list, the XP bar/level number, hearts/hunger/armor/hotbar, the held-item name, the scoreboard sidebar, the boss bar, and — as of this pass — chat. `HUD_TEXT_SCALE` and `hud_line_h` have been deleted from `crates/lodestone-shell/src/hud.rs`; there is no ad-hoc pitch left for a future element to pick up by mistake.

Measured 2026-08-04 from a player report ("the action bar text, title bar title and subtitle were too big"). The diagnosis is exact, not approximate: each surface was off by a factor of precisely 2.000.

## How it works

### Vanilla

All three live in `Hud.java` (`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`). Vanilla sizes them with a **pose transform**, never a font size, and the title and subtitle use different factors:

| surface | method | pose scale | cite |
|---|---|---|---|
| title | `extractTitle` | `4.0` | `Hud.java` |
| subtitle | `extractTitle` | `2.0` | `Hud.java` |
| action bar / overlay message | `extractOverlayMessage` | **none → 1.0** | `Hud.java` |
| held-item name (reference) | `extractSelectedItemName` | **none → 1.0** | `Hud.java` |

Position comes from the same pose. `extractTitle` translates once to the screen centre (`Hud.java`) and then draws each string at an offset **inside** its own scale, so the offsets multiply out:

```
title    top y = h/2 + (-10 * 4.0) = h/2 - 40     (Hud.java:376, 378, 381)
subtitle top y = h/2 + (  5 * 2.0) = h/2 + 10     (Hud.java:376, 385, 387)
action bar top y = (h - 68) + (-4) = h - 72       (Hud.java:341, 349)
```

Note the horizontal centring is *not* a bug in vanilla despite looking like one: the title is drawn at `-titleWidth / 2` where `titleWidth` is the **unscaled** `font.width`, inside `scale(4)`. Both the offset and the width scale together, so `-titleWidth/2 * 4` equals half the rendered width and the string lands centred. Copying the `/2` without the enclosing scale would not.

### Ours, and the double-apply

Two independent scale factors are in play in `HudGeometry::build_inner`, and they multiply:

1. `HudGeometry::build_inner`'s call to `logical_canvas(gui_scale, width, height)` divides the **physical** framebuffer down by the resolved GUI scale (`menu/render/measure.rs`, `logical_canvas`). Every pixel constant below it is therefore already laid into vanilla's own GUI pixel unit. This is correct and is the fix for "the HUD draws half-size on Retina".
2. `HUD_TEXT_SCALE` (`crates/lodestone-shell/src/hud.rs`) — a fixed "legibility factor" of `2.0` applied on top.

The three draw sites then multiply vanilla's factor by that `scale`:

| surface | site | ours | vanilla | ratio |
|---|---|---|---|---|
| action bar | `HudGeometry::build_inner` | `scale` = 2.0 | 1.0 | **2×** |
| title | `HudGeometry::build_inner` | `scale * 4.0` = 8.0 | 4.0 | **2×** |
| subtitle | `HudGeometry::build_inner` | `scale * 2.0` = 4.0 | 2.0 | **2×** |

**The error is a flat 2× at every GUI scale, not a scale-dependent one.** This is worth stating because the obvious hypothesis is the opposite. `logical_canvas` divides by the GUI scale and `scale` is a constant, so the *ratio* to vanilla is scale-invariant; what grows with GUI scale is only the absolute error in physical pixels (2× of a larger number). A bug report of "worse at higher GUI scale" would therefore point somewhere else.

### Why `scale = 2.0` exists

History, not intent. `let scale = 2.0;` dates to the repo's initial commit (`32fb577`), when the HUD drew a fixed 5×7 procedural font straight into **physical** framebuffer pixels and 2× was a plain legibility multiplier. `logical_canvas` entered `hud.rs` much later (`c5c0b49`, `572e8ec`), dividing the canvas down by the GUI scale — and the 2× was never removed. The same latent double-apply has already been fixed twice at other sites, both by dropping to `1.0`:

* the XP level number, whose regression gate (`xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar` in `crates/lodestone-shell/src/hud.rs`) explicitly asserts `digit_width < font.width("5", 2.0)` with the message *"the old `let scale = 2.0;` bug is back"*;
* the held-item name, in `HudGeometry::build_inner` (`crates/lodestone-shell/src/hud.rs`), whose comment warns the next reader not to reintroduce `scale` there.

So this is the third instance of one root cause, and the two earlier fixes are the precedent for the shape of this one.

### Measured evidence

`crates/lodestone-shell/tests/hud_text_scale.rs` measures the ink bounding box of each surface in logical-canvas pixels, using the **held-item name as a scale-1.0 reference** rather than restating a font metric — vanilla draws the held-item name and the action bar through the identical unscaled path, so their rendered heights must be equal, the title exactly 4× and the subtitle exactly 2×. Expressing the gate as ratios also makes it immune to a font change.

At a 1280×720 framebuffer (auto GUI scale 3 → a 426.67×240 logical canvas):

| surface | measured | vanilla predicts | double-apply predicts | lands on |
|---|---|---|---|---|
| reference (held item, 1.0) | 7.00 px | — | — | — |
| action bar | 14.00 px | 7.00 | 14.00 | **double-apply** |
| subtitle | 28.00 px | 14.00 | 28.00 | **double-apply** |
| title | 56.00 px | 28.00 | 56.00 | **double-apply** |
| title top edge | 96.00 px | 80.00 (`h/2 - 40`) | — | neither |
| subtitle top edge | 168.00 px | 130.00 (`h/2 + 10`) | — | neither |

Every ratio is exactly 2.000. The two hypotheses differ by 1× the reference glyph box, and the gate's tolerance is 0.25×, so no font padding can straddle them.

Three controls pass, and they are what make the numbers mean anything:

* `a_quiet_hud_frame_paints_nothing` — with `show_debug` and `crosshair` off (both default `true` in `HudFrame::new`, `crates/lodestone-shell/src/hud.rs`) the buffer is empty, so each measurement really is the surface under test and not something that already paints there.
* `a_blank_title_paints_nothing` — a single-space title is inkless, which is what lets the subtitle be isolated (title and subtitle share one `Option` and one draw block).
* `vertex_layout_is_still_six_floats` — guards the NDC stride the gate inverts.

The two assertion gates ran `#[ignore]`d while the patch was outstanding; both are unconditional now that it has landed:

```bash
cargo test -p lodestone-shell --test hud_text_scale -- --nocapture
```

## How to change it

`HUD_TEXT_SCALE` and `hud_line_h` (`crates/lodestone-shell/src/hud.rs`) **no longer exist.** They used to be a single ad-hoc "legibility factor" shared by the debug overlay, the title/subtitle/action bar, and the chat box's own stride — which is exactly why the fix could never be "just move the constant": moving it would have changed every unrelated surface at once. Each surface was instead ported to vanilla's own absolute factors one at a time, and once chat (the last consumer) was ported too, the shared constant had no remaining caller and was deleted outright rather than left as a trap for the next new element to reach for.

Gotchas, in order of how easy each was to get wrong (kept for the next surface that needs this kind of fix, even though every one named below is now resolved):

* **The subtitle's position used to depend on the title's scale.** `HudGeometry::build_inner` (`crates/lodestone-shell/src/hud.rs`) stacked it at `ty + ts * 9.0`, so correcting `ts` from 8.0 to 4.0 alone would have silently *moved* the subtitle. The title block was rebuilt on vanilla's own pose translate (`h/2`, then `-10 * 4.0` and `+5 * 2.0`) instead of patching two constants at a time.
* **The action bar's size and position were separable.** Its anchor was `bars_y - line_h - 6.0`, where `line_h` was the debug-font stride and carried no dependency on the action bar's own scale — so changing the scale alone would have left the position untouched. The position was *also* wrong (measured 198 px against vanilla's `h - 72` = 168 px on the procedural, atlas-free path), but it hung off the vitals cluster's anchor rather than off a constant, so it was a separate change with its own reasoning, not folded in blind.
* **The now-deleted `hud_line_h` was never vanilla's line height.** It was `(GLYPH_H + 2) * HUD_TEXT_SCALE` = 18, this HUD's own 5×7 debug font's analogue; vanilla's chat line height is 9 (`ChatComponent`'s `messageHeight`). Any future per-surface line height needs its own vanilla-derived constant, not a shared ambient one.
* **Chat had the same double-apply, and it is what this doc originally recorded as the one remaining site.** `chat_pose_scale` (`crates/lodestone-shell/src/hud.rs`) used to be `HUD_TEXT_SCALE * opts.scale`, which was `2.0` at the default `chatScale` of `1.0`, where vanilla's chat pose scale is `chatScale` **alone** (`ChatComponent.getScale`). **Fixed now**: `chat_pose_scale(opts)` is `opts.scale.max(0.0)`, with no HUD-side factor. The ripple this doc warned about was real — `chat_line_h`, `chat_width_px`/`chat_height_px`, the input strip, and `suggestion_layout` all read `chat_pose_scale` — but `chat_width_px`/`chat_height_px` were already vanilla's own `getWidth`/`getHeight` formulas and did not depend on the pose scale at all, so only `chat_pose_scale` itself, and everything computed *from* it, needed to change. `suggestion_layout` is also called from `HudRenderer::suggestion_layout` outside `build_inner`, for pointer hit-testing (`WindowApp::suggestion_row_under_cursor`, `crates/lodestone-shell/src/app/menus.rs`) — both call sites resolve through the one `chat_pose_scale` free function, and `build_inner` now calls it too (it used to recompute the same formula inline), so a future edit to the formula cannot update one call site and miss the other.
* **Not yet fixed, found while re-deriving chat: `ChatScreen`'s own widgets are not chat-scaled in vanilla at all, and this HUD still scales them.** `ChatComponent.extractRenderState` (the public overload `ChatScreen.extractRenderState` calls) brackets its *entire* body — including the `pose.scale(scale, scale)` call — in `graphics.pose().pushMatrix()` / `graphics.pose().popMatrix()`. So the chat-scale pose is popped, and the base (unscaled) pose restored, **before** `ChatScreen.extractRenderState` goes on to call `super.extractRenderState` (which renders the `EditBox` widget, added via `addRenderableWidget` in `ChatScreen.init` at its own literal, unscaled pixel rect: `new EditBox(font, 4, height - 12, width - 4, 12, …)`) and `this.commandSuggestions.extractRenderState(…)`. The input line's background band is likewise a bare `graphics.fill(2, height - 14, width - 2, height - 2, …)` call, outside any scale. **Vanilla's `chatScale` therefore affects only the scrollback log — never the input line, its caret, the inline suggestion ghost, or the `CommandSuggestions` dropdown.** This crate's `chat_pose_scale` (`crates/lodestone-shell/src/hud.rs`) is still used for all of those in `HudGeometry::build_inner` (`input_y`, `INPUT_STRIP_PAD`, the input text, the ghost, the caret) and in `suggestion_layout`/`HudRenderer::suggestion_layout`, so at any `chatScale` away from the vanilla-legal default of `1.0` those widgets are still off by the `chatScale` factor — a real but *separate* divergence from the reported "chat draws at 2× vanilla" defect (which was a flat, scale-independent 2× and is fully fixed) and not folded into that fix, for the same reason the boss bar's position bug below was not folded into its size bug: it needs its own reasoning and its own gate, not a multiplier patched in place. A fix would introduce a second, fixed pose constant (vanilla effectively draws these at `1.0` in the already-`gui_scale`-divided logical canvas, exactly like the debug overlay, tab list and sidebar) used at every one of those call sites **and** at `HudRenderer::suggestion_layout`, so the drawn popup and its hit-test rect cannot drift apart the same way the scrollback fix above was careful not to.
* **The boss bar had the same two bugs at once, found by the scoreboard-sidebar audit.** `BossHealthOverlay` (`.cache/mc/26.2/client-src`) draws a fixed **182×5** native bar and an unscaled title; this crate's boss-bar block used `bar_w = b.w * 0.4` (a canvas fraction, not a vanilla constant) *and* the ambient `scale`/`line_h` for the title. Both are fixed now — see `BOSS_BAR_WIDTH`/`BOSS_BAR_HEIGHT`/`BOSS_BAR_TOP`/`BOSS_BAR_STEP`/`BOSS_BAR_TEXT_SCALE` in `crates/lodestone-shell/src/hud.rs`, ported from `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` and `extractRenderState`'s `yOffset` stepping (`yOffset += 10 + 9`, breaking once `yOffset >= guiHeight() / 3`). Not ported: vanilla's per-colour/overlay sprite art (`BAR_BACKGROUND_SPRITES` etc.) — this still draws a flat tinted rect, a separate, pre-existing simplification from the sizing bug.

## Configuration

**Vanilla has no title, subtitle or action-bar size option, and we should not add one.** Verified against the jar rather than from memory:

* Title size is the literal `4.0F` in `Hud.extractTitle`; subtitle size the literal `2.0F` in the same method; the action bar has no scale call at all (`Hud.extractOverlayMessage`). Neither method reads `Options`, and neither do their callees — `Hud.getFont` is a bare field return, and `GuiGraphicsExtractor.textWithBackdrop` touches only `getBackgroundColor`, which is backdrop opacity, not size. The title state itself (`Hud.setTitle`/`Hud.setSubtitle`/`Hud.setTimes`; `Hud.setOverlayMessage`) carries text and timing only, no size field.
* No `titleScale`, `subtitleScale`, `hudScale`, `textScale`, `fontScale` or `actionBarScale` exists anywhere in the client source.
* No row on any options screen sizes them: `OptionsScreen.init` registers only `fov`; `VideoSettingsScreen.displayOptions` registers `guiScale` among the display options; `ChatOptionsScreen.options` registers `chatScale`; `AccessibilityOptionsScreen.options` registers none. `showSubtitles` (`AccessibilityOptionsScreen.options`) is the **sound**-caption overlay (`Hud.extractSubtitleOverlay` → `SubtitleOverlay`), not the title-packet subtitle — an easy and costly confusion.

So the honest answer to "should these have sizing options?" is: **vanilla scales chat but not titles, and `guiScale` is the only control for these three.** `chatScale` is real — the `Options.chatScale` field, `UnitDouble` 0.0–1.0, default 1.0, surfaced at `ChatOptionsScreen.options` and applied at `ChatComponent.extractRenderState` via `ChatComponent.getScale` — and it is consumed by the chat box and nothing else (three call sites tree-wide, none in the title or overlay-message path). `guiScale` is the `Options.guiScale` field, default `0` = auto, and it scales the entire GUI coordinate space uniformly.

Adding a non-vanilla per-surface size option would be a parity divergence with no upstream counterpart, and — more practically — it would paper over this bug rather than fix it: a user would "solve" oversized titles by turning a knob to 0.5, which is exactly the factor the double-apply introduced.

## Dependencies

* `crates/lodestone-shell/src/hud.rs` — `HudGeometry::build_inner`, its title/subtitle/action-bar draw sites, and `chat_pose_scale`.
* `crates/lodestone-shell/src/menu/render/measure.rs` — `logical_canvas`, shared with the menu screens so the two cannot disagree. Still re-exported as `menu::render::logical_canvas`; only the file it lives in changed.
* `crates/lodestone-shell/src/hud/item_icon.rs` — `ColourStream::rect`'s NDC mapping, which the gate inverts.
* `crates/lodestone-shell/tests/hud_text_scale.rs` — the gate.
* `docs/options-consumption-census.md` — the wider options picture, including the eight live chat options.
