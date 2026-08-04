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

## What is deliberately not built

Items and Mobs as real, clickable, sortable `ContainerObjectSelectionList`s —
not because they are hard, but because with zero underlying data they would
always render empty regardless, so building the enumeration machinery now
buys nothing until a decoder exists. The two tab labels are drawn, greyed,
matching vanilla's own disabled-tab-with-empty-list behaviour.

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
