# Language screen

## What it is

`SettingsPage::Language` (issue #415): vanilla's `LanguageSelectScreen`, reached
from the root settings screen's own "Language..." grid button (`ROOT_GRID` in
`crates/lodestone-shell/src/menu/options.rs`, now live). It is the first of
the three settings sub-screens #392's plan always said would need a
*different* list widget than this tree's other two (`OptionsList`,
`KeyBindsList`) — vanilla's `ObjectSelectionList` — and this issue builds that
third kind.

## How it works

- `crates/lodestone-shell/src/menu/language.rs` — the whole model:
  `LanguageEntry` (code + display name), `LANGUAGES` (this client's own real
  language table — see "Why the list has exactly one entry" below),
  `filtered` (the search predicate), `LanguageNav` (the live state: a real
  search `EditBox`, a cursor over the filtered rows plus the footer, and the
  scroll window), and `frame` (the whole screen).
- `crates/lodestone-shell/src/menu/options.rs` — `SettingsPage::Language`,
  `SettingsNav::language`/`language()`/`language_mut()`/`leave_language`, and
  `settings_frame`'s early-return branch (the same shape
  `SettingsPage::KeyBinds` already established, since this page is not an
  `OptionsList` page either).
- `crates/lodestone-shell/src/menu/nav.rs` — `MenuNav::key_language`,
  `apply_language`, and the `hover`/`click`/`key_settings` guards that route
  to them whenever `settings.page() == SettingsPage::Language`.
- `crates/lodestone-shell/src/menu/render.rs` — `Origin::Language`, carrying
  `menu::language::LanguagePlacement`.

### Why the list has exactly one entry, and why that is not a placeholder

This client parses no `languages.json` — `resources.rs`'s `language` field
loads exactly one table, `assets/minecraft/lang/en_us.json`. `LANGUAGES`
therefore has one `LanguageEntry` (`en_us`, "English (US)"), always selected.
Selecting it (the only thing there is to select) changes nothing, matching
`world_select.rs`'s own precedent — that screen's search box is documented as
"filters the list — of nothing, today" — one screen over. A list mechanism
that correctly handles `N` entries and is fed `N = 1` today is not a stub.

`en_us`'s display name is transcribed from vanilla's well-known
`languages.json` entry rather than read out of this repo's own jar snapshot:
that jar ships no `languages.json` at all
(`unzip -l .cache/mc/26.2/client.jar | /usr/bin/grep -i lang/` returns only
`en_us.json`), so this one string is public vanilla knowledge, not
jar-verified — flagged in `language.rs`'s own module docs rather than
presented as if it were.

## Wired vs. decorative

- **Wired**: reaching the screen (the root's "Language..." button is live)
  and back (Escape/Done → Root), a real search `EditBox` (typing filters
  `LANGUAGES` by name — the same mechanism vanilla's `filterEntries` runs,
  just fed one entry, routed end to end through `app.rs`'s `menu_key_for` →
  `MenuNav::key_language`'s `MenuKey::Char`/`MenuKey::Backspace` arms), moving
  the list+footer cursor, and selecting the one real entry.
- **Decorative — the selection's effect.** Vanilla's `onDone` calls
  `languageManager.setSelected` and `minecraft.reloadResourcePacks()` when the
  selected code differs from the current one. It never can here: the one
  entry *is* the current language, so that guard is always false. Nothing is
  faked to look otherwise.
- **Present-and-inactive**: the footer's "Font Settings..." button
  (`FontOptionsScreen`, vanilla's own next hop) — out of scope for this pass,
  the same `no_screen`-shaped placeholder every other unbuilt destination in
  `SettingsPage` uses.

## What is deliberately not built

Vanilla's `AbstractSelectionList` draws a selected/hovered row with a 1 px
outline and a darker fill, not a button sprite. Building that second
selection-highlight primitive for a list with exactly one real entry is
geometry in service of nothing, so each row instead draws through the
existing `widget/button*` path every other settings row already uses. See
`language.rs`'s own module docs ("The deliberate departure: rows draw as
buttons") for the full reasoning and the one seam (`Origin::Language`) a later
pass would touch to change it.

Resource Packs (`PackSelectionScreen`) and Telemetry (`TelemetryInfoScreen`)
remain unbuilt, per issue #415's own suggested split — each needs
substantially more than a list widget:

- `PackSelectionScreen` needs a `PackRepository` model, filesystem watching, a
  zip/directory pack detector, `pack.png` icon loading, and **two**
  drag-between `ObjectSelectionList`s — this client has none of that.
- `TelemetryInfoScreen` needs a live `TelemetryEventWidget` (a scrollable log
  of pending telemetry events) plus external links — this client collects no
  telemetry at all, so there is nothing to show even once the widget exists.

## Geometry

Every number is transcribed from `.cache/mc/26.2/client-src`'s
`LanguageSelectScreen.java`, cited file:line in `language.rs` itself — see
that module's own "Geometry, transcribed" section for the full derivation,
including the one vanilla quirk worth keeping: the list's real header height
(36, from the search box) is **not** the constructor's literal `y = 33` —
`repositionElements` overwrites it before a frame is ever drawn.

## Configuration

None — this screen has no persisted state of its own.

## Dependencies

- `menu/edit_box.rs` — `EditBox`, the same primitive `world_select.rs`'s
  search field and `menu/create_world.rs`'s fields already use.
- `menu/layout.rs` — `HeaderAndFooterLayout`, `LinearLayout`, `widget_rects`
  — the header/footer widget columns are a real arranged tree, not restated
  arithmetic.
- `menu/options.rs` — `LIST_TOP_INSET`, `HEADER_LINE_HEIGHT`, `WIDGET_H`,
  `SMALL_BUTTON_WIDTH`, reused for this screen's footer buttons.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`options.language.title`, `options.language`,
  `gui.language.search`, `options.languageAccuracyWarning`, `options.font`,
  `gui.done`).

## See also

- [The settings tree](./settings-screen.md) — the root page this screen is
  reached from, and the census this page's nav button moves.
- [Menu UI framework](./ui-framework.md) — the epic's plan, and why this is
  the third list-widget kind.
- [Key Binds](./keybindings.md) — the second list-widget kind, whose
  `SettingsNav` integration shape this page mirrors.
