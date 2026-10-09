# The menu UI framework

## What it is

The shared substrate every out-of-world (and paused-in-world) menu screen is built from: a `Widget` type with a faithful disabled state, layout containers, a screen-level focus/tab/click dispatch layer, list chrome and a pixel-accurate scrollable list, the overlay-versus-full-frame distinction, and the spinning panorama backdrop. It ports the original game's GUI layout, component and navigation behaviour, kept faithful to the jar rather than redesigned.

## How it works

### Screen and widget lifecycle

A screen builds in three phases:

1. Build: create `Widget`s and containers, wrapping each child in `LayoutSettings` (four paddings plus `x`/`y` alignment in `0.0..=1.0`).
2. Arrange: one `arrange_elements()` pass walks bottom-up (nested containers size from their children first) and writes absolute positions into leaves.
3. Visit: `visit_widgets` hands arranged leaves to the screen for drawing and hit-testing. It is the only tree-to-draw route, so a spacer's `visit_widgets` is a deliberate no-op.

Most rows are rebuilt each frame; a container is arranged once and cached since arrangement is canvas-independent. Keep a screen's `extract`-shaped step and its `frame`/draw-shaped step as separate passes.

### Layout containers

`LinearLayout` (one row or column over a one-axis `GridLayout`), `GridLayout` (row and column counts derived from the highest occupied cell), `FrameLayout` (children default to centre, sized to `max(min size, largest padded child)`), and `HeaderAndFooterLayout` (header at top, footer at bottom, content band clamped to never overlap the footer). Screens construct it from the current canvas dimensions per layout pass. Container screens use none of this: slot geometry is constructor arithmetic from the menu, and a layout tree would invent geometry the original never had. Hand-placed screens (the title) are equally legitimate.

- Alignment is a padding-aware lerp, not `(available - width) / 2`; `x` truncates and `y` rounds (a real asymmetric quirk), so an odd cell can be off by a pixel on one axis only.
- A spanning grid cell splits its size with an integer Bresenham-style divisor (parts sum exactly) and only grows a row or column smaller than the span needs.
- Whole-tree placement is `align_in_rectangle`; centring is the same call with `0.5` on both axes. `FrameLayout` min bounds use `set_min_width`/`set_min_height`; the centred child baseline comes from `new_child_layout_settings`. Direction-specific aliases are omitted until a screen needs one.

### Focus, tab order, input

A screen keeps three widget registries, and which one a widget lands in decides what it can do:

| registered with | drawn | receives input |
|---|---|---|
| interactive list | yes | yes |
| input-only list | no | yes |
| render-only list | yes | no |

A widget in the wrong list compiles and unit-tests but is unclickable or invisible with no failure: the most common way a new widget does nothing. Screen renderers own draw order, so `FocusSet` exposes no raw render-list accessor; there is no narration list.

Key order: Escape first (if the screen closes on it), then only the focused child gets the key, and only if unhandled does Tab or an arrow become a focus-navigation event. This is how a text field coexists with arrows: horizontal arrows are claimed by the field, vertical ones fall through.

- Tab wraps via a retry one layer up that clears focus and searches from the start, so a single-focusable screen re-lands on the same child. Arrow navigation is not retried and does not wrap. Tab order is stable-sorted insertion order unless an order group is declared.
- Arrow navigation is geometric in two passes: a strict pass (cross-axis overlap and further along the travel axis), then a vague fallback picking the nearest by squared distance. Shipping only the strict pass reads as "arrows die at the end of a column".
- Hit-testing resolves to the first match in registration order, not topmost, so widgets should not overlap.
- Focus is a tree of ids resolved through a lookup trait, not object references.

### Widgets

`Widget` owns bounds, message and state (`active`/`visible`/`hovered`/`focused`) and answers which sprite to draw and what colour the label is. There is no separate disabled type: `active = false` swaps to the disabled sprite, turns the label flat grey and makes the widget unreachable by Tab. A disabled control with an explanatory tooltip is the idiom for "unsupported but present".

Sprites are a four-state record (`enabled`, `disabled`, `enabled_focused`, `disabled_focused`) with three collapsing constructors (one sprite; two-state focus-only, `EditBox`'s shape; full four-state). The focused predicate differs: a button highlights on `hovered || focused`, a text field on keyboard focus alone. Checkboxes and sliders pick sprites by hand, and one disabled button sprite has a different nine-slice border; read each widget's own art instead of normalizing from a sibling.

`EditBox` has real state (caret, selection, horizontal scroll, 32-character cap by default). Its uneditable colour keys on editable, not active. A screen with a live text field must reposition the existing widget rather than rebuild each frame. Clipboard and select-all use the platform modifier (Cmd on macOS, Ctrl elsewhere) at every check.

### List chrome and the scrollable list

Draw order: a translucent band tint before the rows, then a light-over-dark separator above the band and the mirrored dark-over-light below, both after the rows so they cap a row scrolled flush to the edge. The tint spans the whole canvas width, not just the row column; a genuinely narrow list (two side-by-side lists sharing one clip and scrollbar) opts out. Colours and geometry are decoded from the game textures, flat per row, so a flat quad per row is exact.

- The scroll offset is in pixels, not rows: a wheel notch is half a row and a row counter cannot express that. The original has no scroll animation; smoothness is pixel granularity, so add no easing.
- Hover is recomputed from the mouse each frame and only read; selection moves only on click, key press or focus change. Letting hover write "selected" lets mouse movement re-aim a Select/Remove action.
- There is no GPU scissor: every clip (rows, sprites, text) is done on the CPU against the visible band before upload, which lets a partly visible row draw partly.
- The hit-test must reject clicks and hover outside the list's band, or an overhanging row steals clicks from footer buttons. Every adopting screen declares a canvas-independent spec (row height, top offset, footer height, entry count, row column relation to the canvas: centred fixed width or full-width with inset) that the scrollbar, wheel input, draw and click bound all read.

### Overlays and the panorama

A screen over a live world (pause, death, in-world command block editor) is a backdrop-aware overlay: a three-way choice per screen (panorama, dim over the world, or nothing), not a boolean, so a translucent wash does not lose canvas scale, hover state or list chrome.

The load-bearing rule: every screen's frame is built through one shared function that stamps the canvas facts, used by both draw and hit-test paths. A frame assembled inline at either site compiles and looks plausible but silently loses hover, tooltips, backdrop and chrome.

The panorama is a spinning cubemap, full-strength only on the title screen, dimmed under the menu-background wash elsewhere out of world, and not drawn over a live world. It turns multi-minute slow, so a static-looking sky over a short observation is normal. The jar ships 1x1 grey stubs for the six faces; the real 1024x1024 faces come from the asset object store. A flat grey panorama means that store is unpopulated: fetch the objects, do not touch panorama code. The dim is one blend factor rather than a second quad, correct because it equals a straight multiply in linear or gamma space.

## How to change it

- New widget or screen: pick the right registry (draw+input, input-only, draw-only).
- New disabled state: toggle `active`; for widgets without disabled art (checkboxes, sliders) fall back to the grey label and blocked input.
- Stateful widgets are repositioned, never rebuilt per frame.
- No scroll animation or GPU scissor on the list.
- Take a list's hit-test bound and draw clip from the same spec.
- Overlay frames go through the shared stamping function.
- Read the real arrange logic before trusting a screen whose padding leaves no room for alignment to matter.
- Use the platform modifier for clipboard shortcuts.

## Configuration

- `crates/lodestone-shell/src/config.rs`: `gui_scale` sets the logical canvas.
- `crates/lodestone-shell/src/resources.rs`: loads the menu sprite atlas and panorama faces, fail-open to a flat fallback. An unpopulated asset object store is a soft failure.

## Dependencies

`lodestone-assets` (`.mcmeta` and nine-slice parsing, image loader), `lodestone-render` (GUI sprite atlas and nine-slice decomposition), `lodestone-shell::menu` (widget, layout, focus, list, panorama modules and per-screen specs), and the decompiled client as behavioural reference only.

See also [menu screens](./menu-screens.md), [container screens](./container-screens.md) and [keybindings](./keybindings.md).
