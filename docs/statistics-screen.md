# Statistics screen

## What it is

`Screen::Statistics` (issue #188): vanilla's `StatsScreen`, reached from the
pause menu's Statistics button
(`crates/lodestone-shell/src/menu/nav.rs`'s `PauseButton::Statistics`, now
live). Only the **General** tab (vanilla's 77 fixed stats — time, distance,
damage, counters) is a real scrollable list; **Items** and **Mobs** are
present-and-inactive.

> **Update: the server can now answer.** `V770ServerProtocol::encode_award_stats`
> had no override, so the server built a real stats snapshot on
> `ClientCommand(REQUEST_STATS)` and handed it to a seam that dropped it. It now
> emits a real `award_stats` frame — see
> [`map-and-advancement-wire.md`](./map-and-advancement-wire.md) for the per-stat-
> type registry dispatch.
>
> **Second update: the client now decodes it, and the screen now shows it.**
> `award_stats` folds into `lodestone_ecs::SessionStatistics`, and this screen
> reads it — see [Where the numbers come from](#where-the-numbers-come-from).
> The section below that used to be titled "Why every value is zero" is kept as
> the record of the state it described.
>
> **Third update (issue #564): the tab bar is a real widget now, not three
> `MenuLabel`s.** The bug this replaced is kept below in ["What is
> deliberately not built"](#what-is-deliberately-not-built) — its symptom was
> the reported one (tab captions drawn crossing the divider under a
> "Statistics" heading vanilla never draws), and its cause was the *frame*
> gap this section used to describe, not a missing stamp. See
> [The tab widget](#the-tab-widget-issue-564) for what changed.
>
> **Fourth update (issue #567): `draw_tab` had never actually run — an
> island from the moment it landed — plus Done's own hover outline and a real
> vanilla visual audit.** The owner's report ("the tabs are way too big and
> are not designed the same way as vanilla... they are supposed to mesh with
> the border and change when selected") sent an audit that found something
> more basic than a missing separator: `draw`'s tab-row branch
> (`if let Some(tab) = row.tab.as_ref())` was nested **inside**
> `if row.slot.is_some()`, and a tab row never carries a `slot` (its rect
> comes from `row.tab`'s own dedicated `row_rect` arm, not a generic `Slot`)
> — so the branch was unreachable from the moment issue #564 added it, and
> every tab row silently fell through to the un-slotted "centred stack" path
> and drew as a **plain button** (`draw_widget`, vanilla's `widget/button`
> art) sized to the tab's own correctly-resolved rect, with none of
> `draw_tab`'s sprite selection, underline or label-drop. That is a much
> better match for "too big, not designed like vanilla" than a missing
> separator line is, and it is `CLAUDE.md`'s island pattern in its purest
> form: correct, registered, unit-tested green at the frame-construction
> level (`stats.rs`'s/`create_world.rs`'s own tests build a `MenuFrame` and
> never ask `draw`/`geometry` to rasterise it, so neither could see this),
> and reaching zero of its own pixels because the one path that could reach
> it was gated on a field the row never sets. Moved to a top-level sibling
> check (matching `MenuRow::entry`/`account`/`world`, none of which need a
> slot either) — see `draw.rs`'s own comment at the fixed call site.
>
> Two more things landed in the same pass, now that `draw_tab` is actually
> reachable: (1) `frame` never set `MenuFrame::hovered`, so Done — this
> screen's only real button — never drew a hover outline regardless of where
> the mouse was, the same defect shape as #567's own Create-New-World report,
> found here by auditing the sibling consumer of the same widget; (2) the tab
> strip did not "mesh with the border" the way real vanilla's does — a
> full-width divider used to run under the *entire* bar (including the
> selected tab, which vanilla never draws a line under), and the selected
> tab's own bottom edge never merged with the content panel below it
> (`MenuTabButton.renderMenuBackground`, not previously ported). Both are
> fixed in `render/draw.rs`'s `draw_tab` and the `render.rs` band chrome it
> now coordinates with — see [The tab widget](#the-tab-widget-issue-564) for
> the geometry generalisation that made a *second* consumer possible in the
> first place, and for what "meshes" means concretely.
>
> **The discriminator that caught it**: two new point-sampled `render/
> tests.rs` gates (below) built through the real `frame_for` dispatch and
> asked `colour_at` what was actually painted at the flanks and under the
> selected tab — both failed with "got None" at every sample, which is what
> led to the trace rather than a guess. Fixed, both pass; the failing run
> before the fix is the neuter this repo's evidence standards ask for, not a
> constructed one.

## How it works

- `menu/stats.rs` — the whole model: `StatFormat` (vanilla's four
  `StatFormatter`s — `DEFAULT`, `DIVIDE_BY_TEN`, `DISTANCE`, `TIME`,
  transcribed from `StatFormatter.java`), `GENERAL_STATS` (77 entries: id,
  the verbatim `en_us.json` caption, its formatter), `StatsSnapshot` (the
  live values — see "Why every value is zero" below), `general_rows` (sorts
  by caption, matching `StatsScreen.java:170`'s
  `Comparator.comparing(k -> I18n.get(...))` — **not** declaration order),
  `StatsNav` (a scroll cursor, nothing else — the list is not clickable in
  vanilla either), and `frame` (the whole screen).
- `menu/nav.rs` — `MenuNav::stats`, `key_statistics`, and
  `PauseButton::Statistics`'s `enabled()`/Enter arm.
- `menu/render/dispatch.rs` — wired into `owns_frame` and `frame_for`'s match,
  passing `nav.stats_snapshot()`.

## Where the numbers come from

```text
award_stats -> lodestone_game::progress::Statistics   (SessionStatistics component)
  -> Sim::statistics()                                -- a clone, per frame, in-session only
  -> StatsSnapshot::from_statistics                   -- projection onto GENERAL_STATS
  -> MenuNav::refresh_stats                           -- app::session's reconciliation
  -> dispatch: stats::frame(nav.stats(), nav.stats_snapshot())
```

**The load-bearing detail is the key shape.** The screen's ids are bare paths
(`"jump"`, `"sleep_in_bed"`), and the wire key is `StatKey { category:
"minecraft:custom", value: "minecraft:jump" }` — the category is the **stat
type's** registry name (`Stats.CUSTOM`), not the tab's. Get either half wrong and
*every* lookup misses, which is indistinguishable from "the server awarded
nothing" and therefore from the state this screen was stuck in before.
`the_projection_reads_the_custom_category_and_the_namespaced_value` computes both
wrong hypotheses and requires them to miss.

The projection is driven by `GENERAL_STATS` rather than by the store's keys, for
two reasons: the snapshot's keys are `&'static str` and so must come from the
table, and a `minecraft:mined`/`minecraft:killed` counter has nowhere to go on
this screen (those are the Items and Mobs tabs). Such a counter is dropped, not
squeezed onto a General row.

The snapshot lives on `MenuNav` beside `StatsNav`, not inside it: `StatsNav` is
`Copy` and a sparse counter map is not, and the lifetimes differ — the scroll and
focus reset when the screen opens, the counters belong to the session.

### The state this replaced, kept as a record

This section used to be titled *"Why every value is zero"*, and it was right:
nothing decoded `award_stats`, confirmed by grep and by `cargo xtask
connectedness`, so `StatsSnapshot::default()` was not a placeholder standing in
for real data — it was *the* data. A stat reading zero was the **true** state,
unlike a settings row showing a fabricated `ON` for a feature that does not work
(`docs/settings-screen.md`'s departure 1).

The failure mode when that stopped being true is worth keeping: the empty
`StatsSnapshot::default()` was a **literal at the call site**, so the moment the
decode landed the screen kept drawing zeros with nothing anywhere reporting a
problem. An honest zero and an island look identical from the outside.

An empty table is still correct outside a session, and still correct for a world
where nothing has happened yet. It is also still why Items and Mobs are correctly empty rather than approximately
so: vanilla's own `ItemStatisticsList`/`MobsStatisticsList` filter to
non-zero counts, and `StatsScreen.setTabActiveStateAndTooltip` (`:124-133`)
disables a tab whose list is empty. With every stat at zero, an empty list
*is* what vanilla's own filter would produce from the same data — the
disabled tabs are not a scope cut standing in for real tab-switching, they
are the state vanilla itself would show.

## Wired vs. decorative

- **Wired**: reaching the screen from the pause menu and back (Escape/Done),
  the General tab's real structure (77 stats, vanilla's own captions,
  vanilla's own three format rules — tested against known non-zero inputs,
  not only the trivial zero case), and the census (`GENERAL_STATS.len() ==
  77`, matching `Stats.java`'s own count of `makeCustomStat` calls).
- **Decorative**: every value shown, because nothing decodes the packet that
  would populate one. Enabling the pause button reflects that the screen now
  exists and shows the honest (zero) state — issue #188's own scope asks for
  exactly that once the screen exists ("Enable the pause-menu button once the
  screen exists").

## Focus: nothing is focused on open, and that took a player report

A player reported (2026-08-04) that *"the Statistics menu always has the 'Done'
button focused for some reason"*. It did, and the cause was one line:
`stats::frame` set `selected: 0` on a frame whose **only** `MenuRow` is Done, so
the button was drawn focused the instant the screen appeared.

The jar says nothing should be, and says it twice independently:

- `Screen.init` calls `setInitialFocus()` (`Screen.java:328`), whose base body
  (`:161-169`) is wrapped **entirely** in
  `if (this.minecraft.getLastInputType().isKeyboard())`. This screen is reached
  by *clicking* the pause menu's Statistics button, so the last input type is a
  mouse and the whole body is skipped. `StatsScreen` does not override
  `setInitialFocus` — grepping the jar finds it in eight screens, all of them
  text-field screens (chat, anvil, book, command block, language, packs, social,
  telemetry), and this is not one.
- Even opened from the keyboard, Done would still not be the first stop.
  `StatsScreen.init` (`:79-98`) adds the `MenuTabBar` **first** and then puts the
  footer's Done in `setTabOrderGroup(1)`, which sorts it after every
  default-group widget. The first tab stop is the General tab.

So a focused Done is wrong under *both* input types, which is why `StatsNav`
carries a plain `focused: bool` defaulting to `false` rather than a modelled
`lastInputType` — nothing in this shell tracks one and no reachable path would
make it keyboard.

Three consequences, all in `menu/stats.rs` and `menu/nav.rs`:

- `frame`'s `selected` is `usize::MAX` — `MenuFrame::selected`'s own documented
  "highlights nothing" sentinel, the same value `command_block_frame` uses — not
  an arbitrary out-of-range index.
- **Enter is gated on focus.** `nav::MenuNav::key_statistics`'s `Enter` used to
  close unconditionally, which is `Screen.keyPressed` with a focused widget —
  right behaviour from a premise (something is focused) that is false on open.
  Tab (`MenuKey::Tab` → `StatsNav::focus_next`) is what grants focus; with one
  focusable child it is idempotent, because vanilla's Tab wrap is
  `clearFocus()`-then-retry and re-finds the same child. **Escape is deliberately
  not gated** (`shouldCloseOnEsc()` is true and Escape is the screen's own
  handler), so there is always a keyboard way out before the first Tab.
- **`Screen::Statistics` needed its own `click` arm.** The shared fall-through in
  `MenuNav::click` is `hover` + `Enter`, and hover grants no focus anywhere in
  this shell — so once Enter was gated, clicking Done would have done nothing.
  `click_statistics` is `ContainerEventHandler.mouseClicked`: focus the child that
  was hit, then call its `onClick`. This is issue #391's shape one screen further.

Note this is a *different mechanism* from the earlier "hovering should not focus
it" report on the server list, which was hover writing into selection and was
fixed by splitting `hovered` from `selected`. This one is the **initial value** of
the selection, which that split did not touch — so the two had to be found
separately. Worth checking the same smell elsewhere: `error_frame`,
`credits_frame`, and the accounts pending/failed frames all still hard-code
`selected: 0` on a single-button frame (see "What is deliberately not built").

## Done's own hover outline (issue #567's audit)

A second, separate gap from the tab widget itself, found while auditing this
screen as the tab widget's other consumer: `frame` never set `MenuFrame::
hovered`, and `MenuNav::hover` had no `Screen::Statistics` arm at all, so
`draw_widget`'s `widget.hovered` was `false` for Done on every frame
regardless of the cursor — the exact defect shape issue #567 reported and
fixed for Create New World, just never noticed here because nobody moved the
mouse over Done while looking closely. `StatsNav` now carries a `hovered:
Option<usize>` and `hover_row`, wired through `MenuNav::hover`'s new
`Screen::Statistics` arm; the tab bar itself needed no equivalent, since
`draw_tab`'s hover is derived straight from `MenuFrame::cursor` and the tab's
own rect rather than from row-hover bookkeeping (see [the tab
widget](#the-tab-widget-issue-564) below).

## The tab widget (issue #564)

Built once in `menu/widget.rs` (`TAB_SPRITES`, `tab_underline_colour`,
`tab_label_dy`) and `menu/layout.rs` (`TAB_BAR_HEIGHT`, `tab_bar_geometry`,
`round_toward`), for both this screen and [Create New
World](./world-creation-screen.md) to share — issue #567 made Create New
World the second real consumer. `menu/render/draw.rs`'s `draw_tab` is the
draw side; it is selected by `MenuRow::tab: Option<TabEntryView>` (in
`menu/render/frame.rs`) as a **top-level** check in `draw`'s row loop,
alongside `MenuRow::entry`/`account`/`world` — none of the four need a
`slot`, which is exactly why it must not be nested inside `draw`'s
`row.slot.is_some()` branch the way `MenuRow::pack` legitimately is (a pack
row's rect really is its slot; a tab row's is not, it comes from a dedicated
`row_rect` arm keyed on `row.tab` instead). See the next paragraph for what
went wrong when it briefly was nested there.

**It was dead code from the day it landed, and this is worth reading in
full.** Issue #564's original `draw_tab` call was written nested inside
`if row.slot.is_some()`, alongside `MenuRow::pack`'s (legitimately-nested)
check — a plausible place to put it, since both were "tested here for
`MenuRow::pack`'s exact reason". But a tab row's `MenuRow::slot` is always
`None`, so that condition was always `false` for one, and the branch never
ran: every tab row silently fell through to the screen's *un-slotted*
"centred stack" path several dozen lines further down and drew as a plain
`draw_widget` button (vanilla's `widget/button` art) sized to the tab's own
correctly-resolved rect. No test caught it, because every existing test of
this screen's tabs (`stats.rs`'s own `#[cfg(test)]`s) builds a `MenuFrame`
and asserts on its *rows*, never asking `draw`/`geometry` to rasterise it —
a frame-construction test structurally cannot see a draw-dispatch bug.
Found by the two point-sampled `render/tests.rs` gates below, built through
the real `frame_for` production path: both failed with "got None" at every
sample point, which is what led to tracing the dispatch rather than guessing
at a missing separator. Fixed by moving the check to the top level, as
described above.

**The geometry is now screen-agnostic.** `row_rect`'s `MenuRow::tab` arm used
to call `stats::tab_row_rect` *by name* — harmless while this screen was the
only consumer, and a hard-coded dependency the moment Create New World became
a second one. `TabEntryView` now carries its own `count` (how many tabs this
bar has) alongside `index`, and `layout::tab_bar_row_rect(index, count,
width)` is the one function both screens' own `tab_row_rect` wrappers resolve
through — `row_rect` calls it directly, off the row's own fields, knowing
nothing about which screen produced it.

**The bar now meshes with the border, matching real vanilla (issue #567's
visual audit).** Two things `draw_tab` did not port before, both now real:

- **The flanking header separator.** `MenuTabBar.extractWidgetRenderState`
  blits `Screen.HEADER_SEPARATOR` in exactly two places — before the first
  tab and after the last — never under any tab. `draw_tab` draws these two
  segments itself, on the bar's first and last tab respectively (`tab.index
  == 0` / `tab.index + 1 == tab.count`), reusing the same decoded
  `SEPARATOR_LIGHT`/`SEPARATOR_DARK` pair `render.rs`'s generic band chrome
  already uses for the *content* header/footer. That generic chrome's own
  **header** pair is now suppressed whenever the frame has a tab bar at all
  (`draw`'s own `frame.rows.iter().any(|r| r.tab.is_some())` check) — it used
  to run full-width under the entire strip, including the selected tab,
  which is exactly the "not designed the same way as vanilla" the owner
  reported.
- **The selected tab's panel merge.** `MenuTabButton.renderMenuBackground`
  fills the selected tab's own inset body (`x+2, y+2` through `right-2,
  bottom`) with the same surface the content panel below is drawn on, so its
  bottom edge visually disappears into the panel. This client has no tiled
  `menu_background.png`-equivalent texture, so the merge reuses
  `LIST_BAND_TINT` — the *same* flat colour the content band immediately
  below a tab bar is already tinted with, gated on `merges_with_band` (`true`
  only when `frame.list` produced band chrome — Statistics has one, Create
  New World does not, so its tab bar never paints a merge fill with nothing
  to merge into).

Both are point-sampled gates in `render/tests.rs`
(`the_statistics_tab_bar_meshes_selected_tab_only_and_the_flanks_carry_the_header_separator`,
`create_worlds_tab_bar_gets_the_flanking_separators_but_never_the_merge_fill`)
— `colour_at`, not a vertex-in-rect probe, since a large enclosing fill (the
merge panel) is exactly what a `band_coverage`-style probe cannot see (it
counts vertices *inside* a small rect, and an enclosing quad contributes
none). The first gate also asserts the selected tab's own bottom edge
(`layout::TAB_BAR_HEIGHT`) and the content band's own top
(`ListSpec::model(..).top()`) are the same coordinate, derived from both
sides' real expressions rather than two copies of the literal `24.0`.

`frame` now emits one real `MenuRow` per `TAB_LABELS` entry (`["General",
"Items", "Mobs"]`), not a `MenuLabel` each — real `widget/tab*` sprites keyed
by `(selected, hovered)`, the underline under the selected tab, and vanilla's
3 px label drop while unselected. Only General is `enabled`/`selected`;
Items/Mobs are present-and-inactive for the same reason they always were (see
[What is deliberately not built](#what-is-deliberately-not-built)).

**No "Statistics" title label any more, either.** Vanilla's `TITLE` (`gui.
stats`) is passed to `Screen`'s constructor for narration only — nothing in
`StatsScreen` ever draws it, because the header *is* the tab bar. Drawing it
here at `dy: 12` was what put a stray heading over the tab row.

**This screen's header height is [`layout::TAB_BAR_HEIGHT`] (24 px), not
[`options::SUB_HEADER_HEIGHT`] (33 px)** — `stats::HEADER_HEIGHT` now says so
explicitly, and `band_top`/`list_spec`/`LIST_WINDOW_PX` all read it instead.
The wrong (33 px) header height is exactly what put the tab row's own
labels — drawn at `dy: 28` under the old scheme — crossing the divider bar a
33 px header draws at its own bottom edge: 5 px of overlap, the owner's
reported symptom.

**Zebra striping is real now too.** `general_row_colour` (`index % 2 == 0` →
white, odd → `0xFFBABABA`) matches `GeneralStatisticsList.Entry.
extractContent`'s own `color` variable, applied to both the caption and the
value on the same row. Row height is vanilla's real `14` px
(`GeneralStatisticsList`'s `itemHeight`, `StatsScreen.java:177`), not the 20 px
`options::WIDGET_H` this screen used to borrow from every other non-`OptionsList`
list — `stats::ITEMS_ROW_H`/`MOBS_ROW_H` (22/`9*4`) are recorded alongside it
for when Items/Mobs get real lists, expression rather than literal for the
36 px one (four lines of the 9 px font).

## What is deliberately not built

Items and Mobs as real, clickable, sortable `ContainerObjectSelectionList`s —
not because they are hard, but because with zero underlying data they would
always render empty regardless, so building the enumeration machinery now
buys nothing until a decoder exists. The two tabs are present, drawn through
the same real widget General is (issue #564), and inactive — matching
vanilla's own disabled-tab-with-empty-list behaviour exactly, including the
underline's grey (`tab_underline_colour(false)`) rather than white.

**The gap that used to be here is fixed.** All three tabs used to be
`MenuLabel`s, not `MenuRow`s, so none of them was a control at all — with
Done as the only row, tab traversal had exactly one destination and the tab
bar could not be focused, hovered or clicked. That was honest for Items/Mobs
(disabled in vanilla too) but not for General, which vanilla focuses first.
The tabs are real `MenuRow`s now (see [above](#the-tab-widget-issue-564)); a
keyboard-focus stop is still `#188`/`#564` follow-up territory, since only
`StatsNav::focus_next` (one destination: Done) exists today.

Two more single-button frames still hard-code `selected: 0`, so they draw their
one button focused on open the way this screen used to: `error_frame`
("Back to Title Screen"), `credits_frame` ("Done"), and the accounts
pending/failed frames ("Cancel" / "Back to Accounts"). They all live in
`menu/render/` and were out of scope for this pass. Whether vanilla focuses them
is genuinely ambiguous rather than clear-cut — unlike this screen, none is
reached by a *click*, so `getLastInputType()` is whatever it last happened to be
— and the visible consequence is one highlighted button that Enter activates
anyway. Worth deciding, not worth guessing.

## How to change it

- ~~**The real dependency**: a decoder for the statistics packet.~~ **Landed**, and
  the prediction this bullet made was exactly right: the decode arrived in
  `crates/protocol/*` and the fix here was "a `MenuNav` field refreshed from
  `app` each frame", sourced the same way `nav.social()`'s roster is. To add a
  General stat now, the only question is whether its id is a `minecraft:custom`
  path — see [Where the numbers come from](#where-the-numbers-come-from).
- **Items/Mobs**: once real per-item/per-mob counts exist, these need their
  own list model (id, icon, per-column counts, non-zero filter) — a natural
  sibling to `crate::menu::social::SocialNav`'s shape, not a fold into
  `StatsNav`.
- **Adding a stat**: append to `GENERAL_STATS`; `general_rows`'s sort means
  its position in the table does not need to be alphabetical.

## Configuration

None — this screen has no persisted state of its own.

## Dependencies

- `menu/options.rs` — `FOOTER_HEIGHT`, `LIST_TOP_INSET`, `SMALL_BUTTON_WIDTH`,
  `Placement::Footer` — reused for this screen's footer, the same way
  `menu/key_binds.rs` and `menu/social.rs` reuse them for their own
  non-`OptionsList` screens. **Not** `SUB_HEADER_HEIGHT`/`WIDGET_H` any more —
  see [The tab widget](#the-tab-widget-issue-564) for why this screen's own
  `HEADER_HEIGHT`/`ROW_H` replaced them.
- `menu/layout.rs` — `TAB_BAR_HEIGHT`, `tab_bar_geometry`,
  `tab_bar_row_rect`, now genuinely shared with [Create New
  World](./world-creation-screen.md)'s own tab strip (issue #567).
- `menu/widget.rs` — `TAB_SPRITES`, `tab_underline_colour`, `tab_label_dy`.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`stat.minecraft.*`, `gui.stats`).
- `.cache/mc/26.2/client-src/net/minecraft/stats/{Stats,StatFormatter}.java`
  — the 77-stat census and the four format rules.
- `.cache/mc/26.2/client-src/net/minecraft/client/gui/components/tabs/
  MenuTabBar.java` — the tab bar's own geometry and sprite rules.

## See also

- [Social Interactions screen](./social-interactions.md) — the sibling
  pause-menu screen this one's structure mirrors most closely.
