# Menu list band chrome

## What it is

The **band chrome**: the tinted background a scrolling menu list draws behind its
rows, plus the two 2 px horizontal bars that fence that band off from the header
above it and the footer below it. It is what makes a settings screen read as three
sections — title, content, buttons — rather than one field of widgets, and it is
vanilla's `AbstractSelectionList.extractListBackground` and
`extractListSeparators` ported as three flat quad rows.

Reported missing by the owner on 2026-08-09: *"the button(s) anchored at the bottom
have their own section, with a horizontal bar separating it … the middle section has
a bit of a black filter in the background that tints the panorama a bit."* Both
halves were genuinely absent — the four separator textures had **zero** references
anywhere in the tree.

## How it works

`widget::ListSpec` gains a `chrome: ListChrome` declaration and a
`chrome_rect(&ScrollList, canvas_width)` accessor. `render::draw` asks for that rect
once and draws three things from it, in vanilla's own order:

| order | what | where |
|---|---|---|
| before the rows | the band tint | the whole chrome rect |
| after the rows | the header bar | `chrome.y - 2`, two 1 px rows |
| after the rows | the footer bar | `chrome.y + chrome.h`, two 1 px rows |

The tint is before the rows and the bars after them because that is where
`extractListBackground` and `extractListSeparators` sit around `enableScissor` in
`AbstractSelectionList.extractWidgetRenderState`. The bars being *after* the rows is
what caps a row cut flush at the band edge, which is the visible half of the report.

The rect's `y`/`h` come from the very `ScrollList` the row clip is built from
(`ListSpec::model`), so the tint, the two bars and the scissor are three readers of
one expression rather than three expressions that agree today.

### The rect is *not* the row column

`ListChrome::Canvas` spans the whole canvas width, and that is not a shortcut.
`AbstractSelectionList`'s constructor is `super(0, y, width, height, …)` and every
screen here hands it `this.width`, so the list **widget** is canvas-wide while its
`getRowLeft()`/`getRowRight()` column is 310 px on a settings page. Two different
rectangles: `RowBand` is the second, `ListChrome` the first. Tinting only `row_w`
would leave the canvas margins untinted, which is not what vanilla looks like — and
it is why the accounts scrollbar gate now asserts its gutter is *uniformly the tint*
rather than empty.

### The colours are decoded, not chosen

Every value was read out of `.cache/mc/26.2/client.jar` rather than eyeballed. All
six textures are greyscale+alpha with one flat colour per row:

| texture | size | rows, top to bottom |
|---|---|---|
| `gui/header_separator.png` | 32×2 | grey 255 @ alpha 51, then grey 0 @ alpha 191 |
| `gui/footer_separator.png` | 32×2 | grey 0 @ alpha 191, then grey 255 @ alpha 51 |
| `gui/menu_list_background.png` | 16×16 | grey 0 @ alpha 112, uniform |
| `gui/inworld_header_separator.png` | 32×2 | identical to `header_separator` |
| `gui/inworld_footer_separator.png` | 32×2 | identical to `footer_separator` |
| `gui/inworld_menu_list_background.png` | 16×16 | identical to `menu_list_background` |

Two consequences worth knowing before anyone "fixes" them:

- **There is nothing to tile.** A flat quad per texture row is exact, not an
  approximation — the same argument `render.rs`'s `OVERLAY_BG` already makes for the
  whole-screen `menu_background.png` (grey 0 @ alpha 64).
- **The in-world fork is a no-op in 26.2.** Vanilla picks `inworld_*` whenever
  `minecraft.level != null`, and all three pairs are pixel-identical, so one constant
  each is faithful to *both* arms. A `ListChrome` variant for in-world would be dead
  code that looks like fidelity. If a future version makes the pairs differ, the fork
  goes in `render::draw` beside `MenuFrame::overlay`, which is already the
  in-world/out-of-world bit this pass carries.

The header bar is light-over-dark and the footer bar its mirror; the mirroring is the
point, since each bevel then faces the content.

### The full stack, out of a world and in one

Out of a world: panorama → whole-screen `menu_background` (this pass applies it as
`panorama::dim_for_screen` inside the panorama shader) → band tint → rows → bars.
In a world: `OVERLAY_BG` → band tint → rows → bars. The band therefore sits two
washes deep over the panorama and one over the world, which is vanilla's own
arithmetic.

## How to change it

- **Adding a screen with a list.** Nothing to do: `ListSpec::uniform` defaults to
  `ListChrome::Canvas`, which is right for every list here whose vanilla constructor
  takes the screen width.
- **A list that is genuinely narrower than the canvas** declares
  `.without_chrome()`. The one caller is `menu::packs::list_spec`, and its reason is
  in `ListChrome::None`'s own doc: vanilla runs *two* 200 px lists side by side there,
  each with its own background and bar pair, and this crate models the pair as one
  band so a single clip and a single scrollbar can serve both. A canvas-wide tint
  would paint the gutter vanilla leaves clear. The fix, if anyone wants it, is a
  per-column chrome rect — not a wider one.
- **Gotcha: a gate asserting the footer gap is empty will now fail, correctly.** The
  footer bar owns the first 2 px below the band by vanilla's own construction, so any
  strip measured from `band_bottom` has to start at `band_bottom + SEPARATOR_H`
  instead. Two gates were re-derived for exactly this when the chrome landed
  (`an_account_row_straddling_the_band_is_clipped_not_drawn_over_the_footer` and
  `the_accounts_screen_draws_the_same_scrollbar_the_server_list_does`); both carry a
  comment recording the old bound and why it was the wrong question rather than a
  loosened one.
- **Gotcha: `band_coverage` in the render tests samples *vertices*, not coverage.** A
  canvas-wide quad has no vertex inside a narrow probe rect, so it is invisible to
  that helper. Use `coverage` / `coverage_of` (real rect containment) for anything
  that has to see the chrome.

## Configuration

None. No option gates the chrome; vanilla has none either. The only inputs are the
canvas size and the active screen's `ListSpec`.

## Dependencies

- `menu::widget::{ListSpec, ListChrome, ScrollList}` — the declaration and the band
  arithmetic; see [`scrollable-list.md`](./scrollable-list.md).
- `menu::render::draw` — the three draws, and the row clip they must agree with.
- `menu::render`'s `LIST_BAND_TINT` / `SEPARATOR_LIGHT` / `SEPARATOR_DARK` /
  `SEPARATOR_H` — the decoded constants.
- `menu::panorama` — the whole-screen wash this sits on top of; see
  [`menu-panorama.md`](./menu-panorama.md).
- The 26.2 `client.jar` under `.cache/mc/26.2/`, as the outside source for every
  colour above.
