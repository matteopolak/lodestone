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

## The tab widget (issue #564)

Built once in `menu/widget.rs` (`TAB_SPRITES`, `tab_underline_colour`,
`tab_label_dy`) and `menu/layout.rs` (`TAB_BAR_HEIGHT`, `tab_bar_geometry`,
`round_toward`), for both this screen and Create New World to share — see
[World Creation screen](./world-creation-screen.md) for why the latter does
not yet use it. `menu/render/draw.rs`'s `draw_tab` is the draw side; it is
selected by `MenuRow::tab: Option<TabEntryView>` (in `menu/render/frame.rs`),
tested the same way `MenuRow::pack` is — before `slot`, because its rect *is*
the slot (`stats::tab_row_rect`, via `render::row_rect`'s own arm; a `Slot`
cannot express a `min(400, width)`-clamped row width).

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
- `menu/layout.rs` — `TAB_BAR_HEIGHT`, `tab_bar_geometry`, shared with Create
  New World's own tab strip once it exists.
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
