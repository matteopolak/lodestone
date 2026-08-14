# Scoreboard sidebar

## What it is

The right-edge panel that lists a scoreboard objective's scores —
`Hud.displayScoreboardSidebar`, ported into `HudGeometry::build_inner`
(`crates/lodestone-shell/src/hud.rs`) from the folded state
`crate::scoreboard::sidebar_from` (`crates/lodestone-shell/src/scoreboard.rs`)
builds out of `lodestone_game::scoreboard::Scoreboard`.

## How it works

Two halves:

- **`scoreboard.rs`** resolves the server's folded scoreboard into a
  `crate::overlay::Sidebar` — a title plus per-row `label`/`score` spans —
  applying the `translate` table and each row's `NumberFormat`
  (`Blank`/`Fixed`/`Styled`/`Default`). This half is not the sizing bug and did
  not change.
- **`HudGeometry::build_inner`**'s sidebar block lays that `Sidebar` out at
  vanilla's own metrics (`SIDEBAR_LINE_H = 9.0`, `SIDEBAR_TEXT_SCALE = 1.0`,
  both in `crates/lodestone-shell/src/hud.rs`) — **not** this function's
  ambient `scale`/`line_h`, which is `HUD_TEXT_SCALE` (`2.0`) and this HUD's
  own 18px stride. Using the ambient pitch was the bug: it rendered the panel
  at twice vanilla's size, reported as "the scoreboard is way too big".

Ported directly from `Hud.displayScoreboardSidebar`
(`.cache/mc/26.2/client-src/net/minecraft/client/gui/Hud.java`):

```
width  = max(titleWidth, max over rows of labelWidth + (scoreWidth > 0 ? spacerWidth + scoreWidth : 0))
height = entries * 9
bottom = guiHeight() / 2 + height / 3      // a deliberate top bias, not h/2
left   = guiWidth() - width - 3
right  = guiWidth() - 3 + 2
headerY = bottom - height
```

The header plate spans `(left-2, headerY-10)` to `(right, headerY-1)`, 9px
tall; the body plate spans `(left-2, headerY-1)` to `(right, bottom)`. Title is
centred at `left + width/2 - titleWidth/2, headerY - 9`; each row's label sits
at `left`, its score right-aligned at `right - scoreWidth`, stepping upward by
9px per row from `bottom`.

Colours: title and label both draw in vanilla's `-1` (white) base; the score
column's base is `ChatFormatting.RED` (`0xFF5555`,
`StyledFormat.SIDEBAR_DEFAULT`) — a server's `NumberFormat::Styled` colour
overrides it per span, exactly as `scoreboard.rs`'s own doc records. Plate
alpha is `Options.getBackgroundColor(0.4F)` (header) and `(0.3F)` (body), both
black, at the option's default (`backgroundForChatOnly` on).

## How to change it

- **`bottom = h/2 + height/3` is vanilla's own formula, not a bug to
  "correct" to `h/2`.** It puts the panel slightly above true centre on
  purpose; porting it as a symmetric centring is a plausible-looking
  regression.
- **The `": "` spacer only counts when the row has a score.** A `Blank`
  format's zero-width score means no spacer either — vanilla's
  `scoreWidth > 0 ? spacerWidth + scoreWidth : 0`, not an unconditional gap.
- Every measurement goes through `Builder::spans_width`/`text_width` at
  `SIDEBAR_TEXT_SCALE`, never the ambient `scale` — a restated width would
  desync from the draw the moment a vanilla font is or is not attached.
- See `crate::hud::TAB_LINE_H` for the identical exemption already made for
  the tab-list overlay, and `docs/hud-text-scale.md` for the family of sites
  that still use the ambient 2x pitch (chat, at time of writing).

## Configuration

None. Vanilla exposes no size option for the sidebar; `guiScale` (the existing
`crate::menu::render::logical_canvas` divisor) is the only control.

## Dependencies

`crate::overlay::Sidebar`/`SidebarLine`, `crate::scoreboard::sidebar_from`,
`crate::hud::{SIDEBAR_LINE_H, SIDEBAR_TEXT_SCALE, SIDEBAR_EDGE_MARGIN,
SIDEBAR_BODY_BG_ALPHA, SIDEBAR_HEADER_BG_ALPHA, SIDEBAR_SCORE_DEFAULT}`.
Decompiled reference: `net.minecraft.client.gui.Hud.displayScoreboardSidebar`
and `net.minecraft.network.chat.numbers.StyledFormat` under
`.cache/mc/26.2/client-src`.
