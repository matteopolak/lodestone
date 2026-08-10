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
  goes in `render::draw` beside `MenuFrame::backdrop`, which is already the
  in-world/out-of-world bit this pass carries.

The header bar is light-over-dark and the footer bar its mirror; the mirroring is the
point, since each bevel then faces the content.

### The full stack, out of a world and in one

Out of a world: panorama → whole-screen `menu_background` (this pass applies it as
`panorama::dim_for_screen` inside the panorama shader) → band tint → rows → bars.
In a world: `OVERLAY_BG` → band tint → rows → bars. The band therefore sits two
washes deep over the panorama and one over the world, which is vanilla's own
arithmetic.

### The in-world stack above was the plan, and for a while the code did none of it

Worth keeping as the clearest local instance of *a doc that transcribes vanilla
correctly is not evidence the code does*. The paragraph above was written when the
chrome landed and it was right about vanilla — and in-world Options drew **no chrome
at all** for as long as it stood, which the owner reported on 2026-08-09: *"the main
menu settings have the header/footer, but if i go in game and open settings it doesnt
have it."*

Nothing about the screen differed. There is one `Screen::Settings`, one
`options::settings_frame` that takes no in-world flag, and a `MenuNav::active_list`
arm keyed only on the page. What differed is **which function built the frame**:

| path | builder | `MenuFrame::list` |
|---|---|---|
| from the title | `render::frame_for` | `Some` — chrome draws |
| from the pause menu | `options::settings_frame`, raw | `None` — chrome skipped |

`frame_for` stamps the **canvas facts** onto everything it returns and answers `None`
for the overlay screens deliberately, so the overlay path never reached the stamp.
Measured on `SettingsPage::Sound` at 320×240, the page and canvas the report names:
`frame_for` yields a chrome rect of `(0, 33, 320, 174)` and the raw call yields
`None`, so the tint and all four separator rows are skipped with nothing red.

Three hypotheses were checked and **ruled out** before this one, and they are worth
recording because each is the plausible answer:

- *a regression* — no. The main-menu gate passes at every commit since the chrome
  landed; the overlay path never had it.
- *the draw path* — no. `MenuRenderer::render` and `render_overlay` share one `draw`
  body and differ only in the pass's load op, so both emit the chrome identically.
- *grid versus list* — no. `active_list` does answer `None` for a widget-grid page
  like `Root`, but Sound is a list page with non-empty entries, so it answers `Some`
  in both contexts and the page is not the variable.

The fix is `nav::settings_overlay_frame`: one expression, used by `app/redraw.rs`'s
overlay draw *and* `nav::on_screen_frame`'s hit-test, which stamps the canvas facts
through the same `render::stamp_canvas_facts` that `frame_for` uses. **No condition
was widened** — `ListChrome::None` and the empty-`entries()` arm are untouched, so
the deliberate no-chrome cases stay no-chrome. Adding a second chrome draw on the
overlay path would have worked and then drifted.

Two things the same raw call was also dropping, found by looking at the whole stamp
rather than the one field in the report:

- **`MenuFrame::cursor`**, so in-world Options had no hover affordances and none of
  the per-option tooltips.
- **`MenuFrame::backdrop`**, which defaults to `Panorama` — so in-world Options was
  painting the panorama over the paused world, the *2026-08-04* report that made this
  screen an overlay in the first place. Routing the frame to `render_overlay` changed
  the load op and not the frame's own backdrop declaration, so half that fix had
  quietly never applied. `settings_overlay_frame` sets `Dim`, which is what
  `pause_frame` and `death_frame` already did by hand.

`gui_scale` was dropped too, but only reaches pixels when the option is non-default.

The one thing that stays context-dependent on purpose is the root page's `World
Options...` row — vanilla's own `inWorld` header fork. That is about *rows*, not
chrome, and unifying the chrome did not touch it.

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
- **Gotcha: a screen drawn as an overlay does not go through `render::frame_for`, so
  it does not get the canvas facts unless someone stamps them.** That is what hid the
  in-world bug above for a whole session. If you add an overlay screen, build its
  frame in **one** function that calls `render::stamp_canvas_facts`, and have both the
  draw (`app/redraw.rs`) and the hit-test (`nav::on_screen_frame`) call that function
  — `nav::settings_overlay_frame` and `nav::command_block_overlay_frame` are the two
  worked examples. A frame built inline at the draw site is the defect shape.
- **Gotcha: every render test in `menu/render/tests.rs` reached the chrome through
  `frame_for`.** The blind spot was the *fixture set*, not any one assertion — a whole
  corpus sharing one construction path. `in_world_settings_carries_the_same_band_
  chrome_as_the_main_menu` is the gate that goes the other way, and its executed
  control is the pre-fix raw `settings_frame`, which must produce no chrome at all.

## Configuration

None. No option gates the chrome; vanilla has none either. The only inputs are the
canvas size and the active screen's `ListSpec`.

## Dependencies

- `menu::widget::{ListSpec, ListChrome, ScrollList}` — the declaration and the band
  arithmetic; see [`scrollable-list.md`](./scrollable-list.md).
- `menu::render::draw` — the three draws, and the row clip they must agree with.
- `menu::render::stamp_canvas_facts` — what puts a `ListSpec` on a frame in the first
  place, and therefore what every chrome draw is downstream of. `render::frame_for`
  applies it to the full-screen screens; `nav::settings_overlay_frame` applies it to
  the in-world settings overlay.
- `menu::render`'s `LIST_BAND_TINT` / `SEPARATOR_LIGHT` / `SEPARATOR_DARK` /
  `SEPARATOR_H` — the decoded constants.
- `menu::panorama` — the whole-screen wash this sits on top of; see
  [`menu-panorama.md`](./menu-panorama.md).
- The 26.2 `client.jar` under `.cache/mc/26.2/`, as the outside source for every
  colour above.
