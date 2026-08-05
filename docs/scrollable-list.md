# The scrollable list primitive

## What it is

`menu::widget::ScrollList` is the shared substrate for every list-shaped menu
screen: a **pixel** scroll offset, a scrollbar, and a `hovered`/`selected` pair that
are two genuinely separate pieces of state. It is a port of vanilla's
`AbstractScrollArea` + `AbstractSelectionList` scroll model, and it exists because
the multiplayer list and the account list had each grown their own — with different
bugs.

It owns no entries. A screen keeps its rows in whatever shape suits it and tells the
list only how many there are, so adopting it never means restructuring a model.

## How it works

Four fields, each one a vanilla field:

| `ScrollList` | vanilla |
|---|---|
| `scroll` | `AbstractScrollArea.scrollAmount` (`AbstractScrollArea.java:18`) |
| `selected` | `AbstractSelectionList.selected` (`AbstractSelectionList.java:40`) |
| `hovered` | `AbstractSelectionList.hovered` (`:41`) |
| `dragging` | `AbstractScrollArea.scrolling` (`:19`) |

Every method names the line it ports. The load-bearing ones:

| method | vanilla |
|---|---|
| `content_height` | `Σ heights + 4` (`AbstractSelectionList.java:198-206`) |
| `max_scroll` | `max(0, contentHeight - height)` (`AbstractScrollArea.java:84-86`) |
| `set_scroll` | `Mth.clamp(v, 0, maxScrollAmount())` (`:67-69`) |
| `scroll_rate` | `defaultEntryHeight / 2` (`AbstractSelectionList.java:44` → `:145-147`) |
| `mouse_scrolled` | `scrollAmount - scrollY * scrollRate()` (`AbstractScrollArea.java:34`) |
| `row_top` | `repositionEntries`' running `y` (`AbstractSelectionList.java:143-152`) |
| `row_visible` | `extractListItems`' overlap test (`:346-352`) |
| `scroll_to_entry` | `scrollToEntry` (`:251-261`) |
| `scroller_height` | `clamp((int)(h²/content), 32, h-8)` (`AbstractScrollArea.java:96-98`) |
| `scrollbar_y` | `scrollBarY()` (`:104-108`) |
| `scrollbar_x` | `getRowRight() + scrollbarWidth() + 2` (`AbstractSelectionList.java:289-291`) |
| `drag_to` | `mouseDragged` (`AbstractScrollArea.java:38-56`) |

### The offset is pixels, and that was the bug

Both of this shell's lists stored a **row index** — `MenuNav::server_scroll: usize`
and `accounts::State::scroll: usize` — so one wheel notch jumped a whole 36 px
entry. Vanilla's is a `double` in pixels and one notch is `defaultEntryHeight / 2`,
which for a 36 px server row is exactly **18 px**.

That is not a smoothing problem to be eased over. A row counter structurally cannot
hold 18, and no amount of animation on top of it could produce the intermediate
state, because the information is not there. The fix is the representation.

`a_row_index_implementation_fails_the_notch_assertion` keeps the old model
**executable** as the negative control, so the predicted-value assertion is a
control and not a description of one. Asserting merely that "the offset changed" is
satisfied identically by both implementations — the documented *magnitude* species
of vacuous test.

### There is no scroll animation in 26.2

Checked, not assumed, because "smooth" invites one:
`smoothScroll` / `scrollAnimation` / `targetScroll` appear **nowhere** in
`client/gui`, and `setScrollAmount` is an immediate `Mth.clamp` with no target, no
velocity and no per-frame approach. Smoothness in vanilla is entirely a consequence
of the offset being pixel-granular. **Do not add easing.** It would be invention,
and it would desynchronise the draw from the hit-test, which read the same `row_top`.

### Hover is not selection

Two fields, and nothing copies between them. `hovered` is recomputed from the mouse
at the top of every extract (`AbstractSelectionList.java:210`) and is only ever
*read*, as a boolean argument to the entry's draw (`:360`). `selected` moves on a
click, on a keyboard arrow, or through `setFocused` (`:299-311`) — never on a hover.

`set_hovered` therefore has **no code path to `selected`**. That is what makes the
account-screen bug structurally impossible rather than a remembered rule: `hover`
there wrote `highlighted` *and* `focus`, and `highlighted` is what Select and Remove
act on, so moving the mouse silently re-aimed them.

Closing that opens the mirror-image gap, so both halves belong together: a click
*does* select in vanilla. On the accounts screen a click arrives as `hover` + `Enter`
(`menu/nav.rs:1724-1725`), so `Enter` moves `highlighted` too.

### Clipping: why the offset could not be pixels before

`set_scissor_rect` appears **nowhere** in this workspace, and for months every list
doc in `docs/` recorded the same consequence: "this pipeline has no scissor", so a
row that straddled the band's edge would paint over the header or footer and had to
be **skipped whole**. That is why the offset was row-quantized in the first place.

`menu::render`'s `Quads` now clips on the CPU:

| primitive | how |
|---|---|
| `rect` (and `outline`, `mosaic`) | rect intersection in pixel space |
| `sprite` | `dst` **and** UV cropped together, both axes |
| `text` | emitted to a scratch buffer, then cut in NDC |

`Quads::with_clip` is the scoped entry point — vanilla's
`enableScissor`/`disableScissor` (`AbstractSelectionList.java:242-249`, `:212-214`).

A GPU scissor was rejected deliberately: the whole menu draws in **one**
`"menu-pass"` with four `pass.draw` calls over two vertex streams, so a scissor would
need `MenuGeometry` to record range breaks and the pass to replay them in order, and
the ordering between the streams is already load-bearing (labels are on the colour
stream and must land *on* their button sprite). CPU clipping costs nothing at draw
time and — the deciding reason — **also clips text**, which nothing cheaper does:
glyphs bottom out in `ColourStream::rect` in `hud/item_icon.rs` as one flat quad per
horizontal ink run, so they are not addressable as sprites.

The sprite crop generalises the horizontal-only UV crop the XP bar already uses
(`hud.rs:1302-1312`). Cropping one axis only would **squash** a favicon instead of
cutting it, which is worth naming because it still looks like a picture.

## How to change it, and the gotchas

- **Keep every formula a citation.** A convenience with no vanilla counterpart
  belongs in the screen, not here.
- **The integer truncations are load-bearing.** `scroller_height` casts to `int`
  before clamping and `scrollbar_y` does
  `(int)scrollAmount * (height - scrollerHeight()) / maxScrollAmount()` in *integer*
  arithmetic. The `floor`s are that arithmetic, not defensive rounding — dropping
  them moves the thumb by up to a pixel and no test would say why.
- **`scroll_rate` uses integer division.** A 25 px entry gives 12, not 12.5.
- **`Mth.clamp`'s bounds can cross.** On a very short band `scroller_height`'s upper
  bound (`height - 8`) is below its lower one (32), and vanilla resolves that to the
  **upper** bound. `max` then `min`, in that order.
- **`visible_range` inverts `row_visible`'s algebra; do not adjust it by hand.** The
  `ceil(d/row_h) - 1` is not interchangeable with a `floor`: when a row's bottom edge
  lands exactly on the band's top it is visible by the `>=`, and `floor` drops it.
  A first version used `floor` plus a compensating `+1` and over-reported the last
  row by one at rest. `visible_range_agrees_with_row_visible_at_every_offset` sweeps
  the whole span and is what caught it.
- **`resize` every frame, before reading geometry.** It is
  `updateSizeAndPosition` (`AbstractSelectionList.java:186-195`) and it ends in the
  re-clamp; a list holding a stale `height` reports a stale `max_scroll`, which is
  how a shrunk window ends up scrolled past its own content and draws empty.
- **`set_selected`'s `keyboard` flag is not cosmetic.** Vanilla scrolls into view
  when the entry is clipped **or** when the last input was the keyboard (`:58`). A
  click passes `false`, which is what stops a click on a partly-visible row from
  yanking the list.
- **Row heights may be uniform or per-entry** — `ScrollList::new_variable` (and
  `resize_variable`) take the heights, and `row_offset`/`row_height` are what
  every other method consults. This is `AbstractSelectionList`'s own
  `addEntry(entry, height)` (`:122-129`), whose `repositionEntries` advances a
  running `y` by each child's height (`:143-152`). **Uniform is the degenerate
  case of the same arithmetic, not a second implementation**, and
  `an_explicit_equal_height_list_is_indistinguishable_from_a_uniform_one` holds
  it to that: it sweeps both modes across the whole span and compares every
  observable, so a prefix-sum off-by-one cannot hide behind a spot check.
- **`row_h` is `defaultEntryHeight`, and once heights are explicit it is *not*
  "the height of a row".** It is only what `scroll_rate` is defined against
  (`:44`). Deriving the rate from `heights.first()` would make the wheel speed
  depend on which entry happens to be first — a settings list declaring 25 px
  scrolls **12** px per notch even when its first row is a 20 px header.
  `the_scroll_rate_ignores_the_entry_heights_entirely` pins it, and
  `one_notch_on_a_variable_list_is_half_the_declared_default_height` separates 12
  from the two rivals (10 = rate taken from the first entry, 20 = a row-index
  model landing on that row's height).
- **`visible_range` is closed-form only when uniform.** A prefix sum has no
  multiply to invert, so the variable case walks — as vanilla's own
  `repositionEntries` does. Both modes are held to agreeing with `row_visible`
  entry by entry across the whole span.
- **A uniform `resize` on a variable list drops to uniform on purpose.** Keeping
  a stale height table behind a new `len` is worse than a wrong count, because
  `row_offset` would keep answering *plausibly*. `resize_variable` is the call
  that keeps them.
- **A non-finite or negative height contributes 0 rather than propagating.** A
  `NaN` in a running sum poisons the entire tail, which would put every *later*
  row off-screen — one bad row must stay one bad row.
- **Nesting `with_clip` replaces rather than intersects**, matching
  `enableScissor`'s absolute bounds. No list nests one today.

## Configuration

None at runtime. The compile-time constants are vanilla's, in `menu::widget`:

| constant | value | vanilla |
|---|---|---|
| `SCROLLBAR_WIDTH` | 6 | `AbstractScrollArea.java:13` |
| `SCROLLBAR_MIN_HEIGHT` | 32 | `:14` |
| `SCROLLBAR_HEIGHT_INSET` | 8 | `:97` |
| `LIST_CONTENT_PADDING` | 2 | `AbstractSelectionList.java:435` |
| `SCROLLER_SPRITE` | `widget/scroller` | `AbstractScrollArea.java:15` |
| `SCROLLER_BACKGROUND_SPRITE` | `widget/scroller_background` | `:16` |

The jar-less scrollbar fallback is **not** a citation. 26.2 draws those two sprites
and nothing else, so there is no vanilla colour for a run with no atlas; the fallback
reuses this shell's own `ROW_OFF`/`LABEL` palette. Do not relabel those as "vanilla's
scrollbar colours".

## Current state, and what is left

| report | state |
|---|---|
| scrollbar on the server list | **done** — `render::draw_scrollbar` |
| hover must not focus (accounts) | **done** — `accounts::AccountsNav::hover` |
| both screens share one primitive | **partly** — the geometry is shared; the two screens still own their offsets |
| pixel-granular ("smooth") scrolling | **done for the server list** (#445) — still row-indexed on the accounts screen |

That last row is now the honest gap, and it is narrower than it was.
`MenuNav::server_scroll` is an `f32` pixel offset and `app.rs`'s wheel handler
passes the real `dy` through to `ScrollList::mouse_scrolled`, so the server list
moves vanilla's 18 px per notch and the thumb is continuous — see
[`server-list.md`](./server-list.md)'s scrolling section for the gates and the
observed control.

**`accounts::State::scroll` is still a `usize` row index**, deliberately. It was
checked rather than assumed while #445's input half landed: the accounts screen
has **no mouse-wheel arm at all** in `app.rs`, so its offset only ever moves by
whole rows anyway (`scroll_to_show`, driven by keyboard cursor-follow), and
converting the field alone would change no pixel while touching a file outside
that change's ownership. The conversion belongs with wiring a wheel arm for that
screen, not before it — see the adoption audit below.

## Adoption audit — the other list screens

Measured, not guessed. **No scrollbar is drawn anywhere else in the shell**, and
mouse-wheel input is wired for exactly *one* list in the whole app; the other seven
scroll only by keyboard cursor-follow.

| screen | offset | hover vs selection | verdict |
|---|---|---|---|
| `key_binds.rs` | `first: usize` | hover sets the cursor | **easy** — the canonical shape, ~60-70 lines, nearly all deletions |
| `social.rs` | `first: usize` | hover sets the cursor | **easy** — ~45-55 lines; needs a "content length changed" clamp its siblings don't exercise |
| `stats.rs` | `first: usize` | none at all | **easy, cleanest win** — `StatsNav` is one `usize`; also the only screen already deriving an explicit `max_first` |
| `language.rs` | `first: usize` into a *filtered* list | hover sets the cursor | **medium** — content length is derived per call, and the search-box row offset lives in `nav.rs` |
| `options.rs` | `first: usize`, **variable** row heights | hover sets the cursor | **medium, and it decides the shape** — cannot adopt a uniform-pitch list |
| `packs.rs` | none | hover sets the cursor | **not applicable** — adopting means *adding* scroll, a feature not a refactor |
| `telemetry.rs` | none | hover sets the cursor | **not applicable** — four fixed controls, no list |
| `world_select.rs` | none | **genuinely separate** (`hovered` + `FocusSet`) | **not a scroll candidate — but it is the hover reference** |

Two things fall out and both are decisions, not details:

- ~~**`options.rs` decides whether `ScrollList` grows variable row heights.**~~
  **Decided (#445): it grows them.** `new_variable` landed, so `options.rs`'s
  mixed header/control heights are expressible and the other four screens are the
  degenerate uniform case of the same arithmetic. This was settled *before*
  converting any screen, deliberately — converting the four against a uniform-only
  primitive and then again against a variable one is the one sequencing mistake
  available here.

  **What is still outstanding is the adoption itself**, and one thing found while
  sizing it is worth writing down because it changes the estimate: the scrollbar
  draw is **not** generic today. `render/draw.rs:145-149` calls
  `server_scroll_list` by name, so it is the *multiplayer list's* scrollbar rather
  than "the active screen's". Adopting the primitive on a second screen therefore
  means adding that hook — the issue's own suggested shape, "one arm that asks the
  active screen for its list" — before any of the four conversions produces a
  single new pixel. Do that first; a screen converted to `ScrollList` while the
  draw still asks for the server list is a textbook island (green tests, no
  scrollbar, no wheel).
- **`world_select.rs` is the model for the hover half, not the six `self.cursor = row`
  screens.** It is the only screen that already keeps hover and focus apart. If the
  primitive's hover concept is copied from any of the others, a mouse-over will steal
  the keyboard out of a search field.

Net: four thin adoptions removing roughly 180-210 lines of near-duplicated
window/clamp/hover code, gated on the variable-height decision.

## Dependencies

`ScrollList` itself needs nothing beyond `core` — pure data and arithmetic. Its
pixels come from `menu::render` (`draw_scrollbar`, `Quads::with_clip`), which resolves
sprite ids through `lodestone_render::GuiAtlas` and draws text through
`crate::hud::VanillaFont`.
