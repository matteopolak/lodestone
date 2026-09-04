# The menu UI framework

## What it is

The shared substrate every out-of-world (and paused-in-world) menu screen is built from: a `Widget`
type with a vanilla-faithful disabled state, layout containers that arrange those widgets, a
`Screen`-level focus/tab/click dispatch layer, list chrome and a pixel-accurate scrollable list, the
overlay-vs-full-frame distinction that lets a screen draw over a live world, and the spinning
panorama backdrop behind the title and other menu screens. All of it is a port of vanilla's
`gui/layouts`, `gui/components`, `gui/navigation` and `Screen`/`Hud` background packages, kept
faithful to the jar rather than redesigned.

## How it works

### Screens and the widget lifecycle

A menu screen is built in three phases, mirroring vanilla exactly:

1. **Build.** The screen creates `Widget`s and containers, wrapping each child with a
   `LayoutSettings` (four paddings plus an `x`/`y` alignment in `0.0..=1.0`).
2. **Arrange.** One `arrange_elements()` pass walks the tree bottom-up — nested containers size
   themselves from their own children first — and writes absolute positions into the leaves.
3. **Visit.** `visit_widgets` hands the arranged leaves to the screen for drawing and hit-testing.
   This is the only route from a tree to a draw, which is why a spacer element's `visit_widgets` is a
   deliberate no-op: it takes part in every measurement and is never drawn.

Most rows here are still rebuilt fresh every frame (cheap, and correct for anything without
persistent state); a container is arranged once and cached, since arranging is canvas-independent —
only the final placement in the screen depends on the current size.

Vanilla's own render side went through the same split this codebase already had: `Renderable` is a
one-method interface that appends render-state records into a list, and a later pass walks and draws
them, matching this shell's extract/frame separation. Treat a screen's `extract`-shaped step and its
`frame`/draw-shaped step as two different passes for the same reason vanilla does.

### Layout containers

Four containers cover real usage: `LinearLayout` (a single row or column, wrapping a one-axis
`GridLayout`), `GridLayout` (row/column counts are *derived* from the highest occupied row/column,
never declared), `FrameLayout` (children default to centre alignment, sized to
`max(min size, largest padded child)`), and `HeaderAndFooterLayout` (pins a header at the top, a
footer at the screen bottom, and clamps the content band so it can never overlap the footer). A
container screen (inventories, chests) uses none of this — its slot geometry comes from the
game-logic menu classes as constructor arithmetic, not from a layout tree, and that boundary should
stay: arranging a container screen with these containers would invent geometry vanilla never had.

The alignment formula is not `(available - width) / 2`; it is a padding-aware lerp between the
leading and trailing padding. `x` truncates and `y` rounds — a real, asymmetric vanilla quirk, not a
bug to "fix" — so a child centred in an odd-sized cell can land off by a pixel on one axis and not
the other. A spanning grid cell splits its size with Mojang's integer `Divisor` (Bresenham-style, so
the parts always sum back exactly) and only ever *grows* a row/column that is smaller than the span
needs. Whole-tree placement goes through `align_in_rectangle`; centred placement is the same call
with `0.5` on both axes, so it does not have a second convenience entry point to keep in sync.
`FrameLayout` minimum bounds are likewise changed through the per-axis `set_min_width` and
`set_min_height` methods; callers that need both set both explicitly.
`LayoutSettings` keeps the general `align` builder and the live horizontal-centre shorthand;
direction-specific forwarding aliases are omitted until a screen actually needs one.
`SpacerElement` keeps its general two-axis constructor and its vertical-only shorthand; there is no
horizontal-only forwarding constructor without a consumer.

Using a hand-arithmetic layout instead of a container is legitimate vanilla, too — the title screen,
for one, hand-centres rather than using any layout class. Whether a screen is layout-driven or
hand-placed is a per-screen choice, not a rule.

### Focus, tab order, and input dispatch

Every screen keeps three separate registries of its widgets, and which one a widget lands in decides
what it can do:

| registered with | drawn | receives input |
|---|---|---|
| the interactive list (draw + input) | yes | yes |
| the input-only list | no | yes |
| the render-only list | yes | no |

A widget in the wrong list compiles, is unit-testable, and is simply unclickable or invisible with
nothing failing loudly — this is the most common way a new widget silently does nothing.

`Screen`-level key handling has a strict order: Escape is answered first (if the screen closes on
it), then the **currently focused child alone** gets the key — dispatch never iterates the whole
child list — and only if that returns unhandled does Tab or an arrow key become a focus-navigation
event. This ordering is also what lets a text field coexist with arrow-key focus navigation with no
special-case rule: horizontal-arrow handling is claimed by the field itself and never reaches step
three, while vertical arrows are declined by the field and fall through to navigate focus.

Tab does not wrap by walking off the end of the list and reading a wrapped index — the walk simply
returns "no next child," and a **retry one layer up** clears focus and searches again from the start.
Two consequences: arrow-key navigation is not retried this way, so it does not wrap at all (focus
just stays put at the edge), and because the retry clears focus first, a single-focusable-child
screen re-lands on that same child rather than doing nothing. Tab order itself is stable-sorted
insertion order unless a widget declares an explicit order group.

Arrow navigation is geometric, in two passes: a strict pass keeps only candidates that overlap the
focused widget on the cross axis and lie further along the travel axis, and only if nothing qualifies
does a vaguer pass drop the overlap requirement and pick the nearest candidate by squared distance.
Shipping only the strict pass reads as "Tab works, arrows die at the end of a column" — a real trap
if a new widget layout is added without the vague fallback. `getChildAt`/hit-testing resolves to the
**first** match in registration order, not the topmost by z — two overlapping widgets means the
older one always wins the click, so widgets here (as in vanilla) simply should not overlap.

Because focus paths in vanilla hold live object references, and that shape does not translate
directly, focus here is tracked as a tree of **ids** resolved through a small lookup trait rather than
back-references — a structural difference from vanilla with no behavioural consequence, but worth
knowing before assuming a focus path can be walked as pointers.

### The widget catalogue

`Widget` owns a control's bounds, message and state (`active` / `visible` / `hovered` / `focused`)
and answers the two things every screen otherwise re-derives: which background sprite to draw and
what colour the label is. There is **no separate disabled widget type** — `active = false` is the
entire API, and inventing a second type for it is the mistake this exists to prevent. Setting it
inactive does exactly three things: the background sprite swaps to its disabled variant, the label
turns the flat vanilla grey, and keyboard focus skips the widget entirely (it becomes unreachable by
Tab, not merely unclickable). A disabled control that explains itself with a tooltip — a greyed-out
button under a cursor still showing *why* it's inactive — is vanilla's own idiom for "unsupported but
present," worth copying wherever a feature is intentionally stubbed out.

Sprite selection is a small four-state record (`enabled`, `disabled`, `enabled_focused`,
`disabled_focused`), built through one of three collapsing constructors depending on how much distinct
art a widget has: a single sprite for every state, a two-state form that only distinguishes focus
(this is `EditBox`'s shape), or the full four-state form most buttons use. The predicate feeding the
"focused" side is **not uniform across widget kinds** — a button treats hover and keyboard focus as
the same highlighted state (`hovered || focused`), while a text field's highlighted sprite is driven
by keyboard focus alone; hovering a field does not draw its highlighted background. Checkboxes,
sliders and edit-box borders each have their own small quirks (a checkbox and a slider pick between
two sprites by hand rather than through the four-state record at all; one particular disabled button
sprite has a different nine-slice border width than its siblings) that are easy to silently
"normalize away" if a widget is re-derived from a sibling instead of read from its own art.

`EditBox` is the one widget with real internal state: a caret, a selection, horizontal scroll and a
length cap (32 characters by default). Its uneditable text colour is keyed on whether the field is
*editable*, not whether it is *active* — a field can be focusable and clickable while still refusing
text input, and that is a different flag than the disabled-widget mechanism above. Because typing
holds state that must survive a frame, a screen with a live text field cannot rebuild its widgets from
scratch every frame the way a row of buttons can; it must reposition the existing widget instead.
Platform matters here too: the "clipboard/select-all" modifier is the platform shortcut key (Cmd on
macOS, Ctrl elsewhere), not a hardcoded Ctrl — every cut/copy/paste/select-all check needs to go
through that platform switch or it silently only works on some machines.

### List chrome

A scrolling menu list draws three things behind and around its rows, in a fixed order: a translucent
band tint drawn *before* the rows (so the rows paint over it), then a light-over-dark separator bar
above the band and its mirrored dark-over-light counterpart below it, both drawn *after* the rows so
they cleanly cap a row that scrolls flush to the band's edge. The tinted rect spans the **whole
canvas width**, not just the narrower column the rows themselves occupy — a settings list's rows sit
in a centred column, but the background and bars behind them reach the screen edges, and tinting only
the row column would leave the canvas margins looking wrong. A list that is genuinely narrower than
the canvas (two side-by-side lists sharing one clip and one scrollbar, for instance) opts out of this
whole-canvas chrome rather than trying to force the shared model to draw two separate bands.

The chrome's colours and geometry are all decoded directly from the game's own textures rather than
picked by eye, and every one of them is a flat single colour per texture row — there is nothing to
tile, so a flat quad per row is exact, not an approximation.

### The scrollable list

The shared list mechanism tracks a scroll offset **in pixels**, not in whole rows — this is the one
representation choice the whole thing hinges on. A row-index offset structurally cannot express a
partial scroll (one wheel notch is half a row's height, not a whole row), and no amount of animation
on top of a row counter can produce that intermediate position, because the information simply is not
there. Vanilla itself has no scroll animation at all — smoothness is entirely a side effect of the
offset being pixel-granular — so nothing here should add easing on top of it.

Hover and selection are two genuinely separate pieces of state: hover is recomputed from the mouse
every frame and only ever *read*, never written back into what's selected; selection only moves on a
click, a keyboard press, or an explicit focus change. Conflating the two — letting a hover write the
"selected" field — silently lets moving the mouse re-aim a Select/Remove-style action pointed at
whatever the model calls "selected."

Because the render pipeline underneath has no GPU scissor rect, every clip here (list rows, sprites,
text) is done on the CPU by intersecting geometry against the visible band before upload, rather than
issuing a hardware scissor per range. That decision is what let the scroll offset move to pixels in
the first place — a row that only partially fits the band gets partially drawn instead of skipped
whole.

Clipping the *draw* is only half the picture — the **hit-test** has to reject clicks and hover outside
a list's own band the same way vanilla's list widgets bound the cursor against the list box before
ever walking its entries. A screen that puts click-handling geometry into the same flat list as its
list rows without asking "is this actually inside the list's band" lets an overhanging list row steal
clicks (and hover) meant for whatever sits below the list, footer buttons included. Every screen that
adopts the shared list declares a small canvas-independent spec — row height, top offset, footer
height, entry count, and how its row column relates to the canvas width (centred at a fixed width, or
full-width with an explicit inset) — and that one declaration is what both the scrollbar/wheel input
and the pixel draw read, so a screen cannot have a working scrollbar without also getting the correct
click bound, or vice versa.

### Overlay screens and the panorama backdrop

Not every menu screen owns the whole frame. A screen shown over a live world — the pause menu, the
death screen, a command-block editor opened in-world — is drawn as a **backdrop-aware overlay**
rather than a full standalone frame: it composites over the game world (dimmed, not covered by the
panorama) instead of replacing it. This is a three-way choice per screen (panorama backdrop, a dim
overlay over the world, or nothing) rather than a single boolean, because a screen can legitimately
want a translucent wash drawn **over** the live game world without suppressing everything else that a
full-frame screen gets — canvas scale, cursor/hover state, the list chrome above, and so on.

That "everything else" is the load-bearing rule: **every screen's frame must be built through one
shared function that stamps the canvas facts onto it**, the same one full-frame screens use, rather
than being assembled by hand at the draw site. A screen that skips this and builds its frame raw
still compiles and looks plausible, but silently loses hover, tooltips, the correct backdrop, and list
chrome, because each of those is a field on the shared frame stamp rather than something the draw call
derives itself. When adding a new overlay-style screen, build its frame through that one shared
stamping function and have both the draw path and the hit-test path call it — never assemble a frame
inline at either call site.

The panorama itself is vanilla's spinning cubemap sky, shown full-strength (undimmed) only on the
title screen and dimmed under the translucent menu-background wash everywhere else out of a world; it
is not drawn at all over a live world. It spins at a fixed, slow rate (a multi-minute revolution), so
"the sky looks static" over a short observation is expected, not broken. One thing worth knowing
before debugging a flat grey sky: the six cubemap face textures are **not** shipped as real art in the
game's own jar — the jar deliberately carries 1×1 grey placeholder stubs, and the real 1024²-pixel
faces are delivered through the separate asset object store the game's asset index points at. A grey,
non-varying panorama does not mean the loader is broken; it means the object store wasn't populated,
and the fix is fetching those objects, not touching the panorama code. The dim wash over the panorama
is applied as a single blend factor rather than a second quad — cheaper, and correct only because the
dim happens to be equivalent to a straight multiply in either linear or gamma target space.

## How to change it

- **A new widget or screen: pick the right registry.** Draw-and-input, input-only, and draw-only are
  three different lists; the wrong one compiles and renders correctly in isolation while silently
  never receiving a click or never drawing.
- **A new disabled state: never invent a second widget type.** Toggle `active`, and if the widget has
  no dedicated disabled art (checkboxes, sliders), let it fall back to the grey label plus blocked
  input — do not invent art vanilla never had.
- **A widget with any internal state (a text field, a scroll position) cannot be rebuilt from scratch
  every frame.** Reposition the existing instance instead, or its state resets every frame it's drawn.
- **Do not add scroll animation or a GPU scissor to the list.** Neither exists in vanilla; the
  pixel-granular offset already produces the "smooth" feel, and CPU clipping is what makes partial rows
  and clipped text both work without a second mechanism.
- **A list's hit-test bound and its draw clip must come from the same declaration.** If a screen's
  list rows are hit-tested through anything other than the shared band declaration, an overhanging row
  will steal clicks from whatever sits below the list.
- **An overlay-style screen's frame must go through the same canvas-stamping function every full-frame
  screen uses.** Assembling a frame by hand at the draw site is the single most common way an overlay
  screen quietly loses hover, tooltips, chrome, or the correct backdrop.
- **Read the real arrange/layout logic before trusting a screen that merely uses it.** A screen whose
  padding happens to leave no room for alignment to matter cannot tell you what the alignment formula
  actually does.
- **Sort out platform key modifiers explicitly.** Clipboard-style shortcuts use the platform's own
  modifier key, not a hardcoded one; getting this wrong only breaks on the platforms nobody tested on.

## Configuration

- `crates/lodestone-shell/src/config.rs` — `gui_scale` sets the logical canvas every layout and widget
  bound is expressed in.
- `crates/lodestone-shell/src/resources.rs` — loads the menu GUI sprite atlas (widget/list-chrome
  sprites) and the panorama faces; both are fail-open, so a missing pack asset degrades to a flat
  fallback rather than failing startup.
- The asset object store (resolved alongside the game's normal asset root) is where the real panorama
  faces live; an unpopulated store is a soft failure, not an error.

## Dependencies

- `lodestone-assets` — `.mcmeta`/nine-slice parsing for widget and list sprites, and the plain image
  loader the panorama faces use.
- `lodestone-render` — the GUI sprite atlas and its nine-slice geometry decomposition.
- `lodestone-shell::menu` — the widget, layout, focus, list, and panorama modules themselves, plus the
  screen-specific code that declares each screen's layout and list spec.
- The decompiled game client source — behavioural reference only, never transliterated; every ported
  constant here traces back to a real vanilla class rather than to a sibling port.

## See also

- [Menu screens](./menu-screens.md) — the screen catalogue built from these primitives.
- [Container screens](./container-screens.md) — the neighbouring subsystem that deliberately does
  not use these layout containers.
- [Keybindings](./keybindings.md) — the action/input table behind the key events this dispatch layer
  consumes.
