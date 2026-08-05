# Statistics screen

## What it is

`Screen::Statistics` (issue #188): vanilla's `StatsScreen`, reached from the
pause menu's Statistics button
(`crates/lodestone-shell/src/menu/nav.rs`'s `PauseButton::Statistics`, now
live). Only the **General** tab (vanilla's 77 fixed stats — time, distance,
damage, counters) is a real scrollable list; **Items** and **Mobs** are
present-and-inactive.

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
- `menu/render.rs` — wired into `owns_frame` and `frame_for`'s match, passing
  `StatsSnapshot::default()` — see below for why that is not a stand-in.

## Why every value is zero

**Nothing in this workspace decodes the `award_stats`/statistics packet.**
Confirmed by grep (`/usr/bin/grep -rln 'award_stats\|AwardStats\|ClientboundAwardStatsPacket'`
over `crates/` — nothing) and by `cargo xtask connectedness`, which names no
stat packet either. Decoding it is `crates/protocol/*` work, out of this
batch's file ownership.

So `StatsSnapshot::default()` — an empty table, `StatsSnapshot::get`
returning `0` for everything — is not a placeholder standing in for real
data; it is *the* data, because nothing has ever populated anything else.
This is a different situation from a settings row showing a fabricated `ON`
for a feature that does not work (`docs/settings-screen.md`'s departure 1): a
stat reading zero is the **true** state of "nothing has been decoded yet",
the same way a freshly created vanilla world's own Statistics screen reads
zero for everything a player has not yet done. Nothing here claims a stat is
tracked that is not — no stat is tracked yet, uniformly and honestly.

This is also why Items and Mobs are correctly empty rather than approximately
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

## What is deliberately not built

Items and Mobs as real, clickable, sortable `ContainerObjectSelectionList`s —
not because they are hard, but because with zero underlying data they would
always render empty regardless, so building the enumeration machinery now
buys nothing until a decoder exists. The two tab labels are drawn, greyed,
matching vanilla's own disabled-tab-with-empty-list behaviour.

**A second, separate gap, surfaced while fixing the focus default:** all three
tabs are drawn as `MenuLabel`s, not `MenuRow`s, so **none of them is a control**.
In vanilla they are real focusable widgets and — per the focus section above —
the General tab is the screen's *first* tab stop. Here Done is the only row, so
tab traversal has exactly one destination and the tab bar cannot be focused,
hovered or clicked at all. That is honest for Items/Mobs (they are disabled in
vanilla too, with zero data) but not for General, which vanilla focuses first.
Building them is #188 territory and deliberately not done here.

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

- **The real dependency**: a decoder for the statistics packet in
  `crates/protocol/*` (out of this batch's ownership) that produces a
  populated `StatsSnapshot` (or an equivalent this module can be handed).
  Once one exists, `render.rs`'s `Screen::Statistics` arm is the one line to
  change — swap `StatsSnapshot::default()` for the real snapshot, sourced the
  same way `nav.social()`'s roster would eventually be (a `MenuNav` field
  refreshed from `app.rs` each frame, or on each `award_stats` packet).
- **Items/Mobs**: once real per-item/per-mob counts exist, these need their
  own list model (id, icon, per-column counts, non-zero filter) — a natural
  sibling to `crate::menu::social::SocialNav`'s shape, not a fold into
  `StatsNav`.
- **Adding a stat**: append to `GENERAL_STATS`; `general_rows`'s sort means
  its position in the table does not need to be alphabetical.

## Configuration

None — this screen has no persisted state of its own.

## Dependencies

- `menu/options.rs` — `SUB_HEADER_HEIGHT`, `FOOTER_HEIGHT`, `LIST_TOP_INSET`,
  `WIDGET_H`, `SMALL_BUTTON_WIDTH`, `Placement::Footer` — reused for this
  screen's footer, the same way `menu/key_binds.rs` and `menu/social.rs`
  reuse them for their own non-`OptionsList` screens.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`stat.minecraft.*`, `gui.stats`).
- `.cache/mc/26.2/client-src/net/minecraft/stats/{Stats,StatFormatter}.java`
  — the 77-stat census and the four format rules.

## See also

- [Social Interactions screen](./social-interactions.md) — the sibling
  pause-menu screen this one's structure mirrors most closely.
