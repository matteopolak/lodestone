# The settings tree, with unsupported controls disabled

## What it is

`crates/lodestone-shell/src/menu/options.rs` — vanilla's `OptionsScreen` tree as
a **table plus arithmetic**: nine `OptionsList` pages, **143 controls**, of
which **39 or 40 work** (see below) **and the rest are present and greyed
out** — plus four more pages that are not `OptionsList` at all and are
counted separately (see below): Key Binds, and, since issue #415, Language,
Telemetry and Resource Packs. Reached from the title screen's Options button
and from the pause menu's, on `Screen::Settings`.

**As of issue #415, every one of the root grid's ten nav buttons opens a
real screen.** The three that used to be permanently inactive placeholders
(Language, Telemetry, Resource Packs) are all built now — two of them
(Telemetry, Resource Packs) as deliberately reduced shapes rather than
vanilla's own, with the reduction declared rather than silent; see
[`language-screen.md`](./language-screen.md), [`telemetry-screen.md`](./telemetry-screen.md)
and [`resource-packs-screen.md`](./resource-packs-screen.md).

**Updated since #55 landed:** #200/#202/#203 (`docs/input-options.md`) made five
more Controls/Mouse-page rows live — `toggleCrouch`/`toggleSprint`/
`invertMouseX`/`invertMouseY`/`mouseWheelSensitivity` — without adding or
removing any row, so the 135-control census that stood at the time was
unchanged by that pass; only the live/inactive split moved.

**Updated again for the Online settings page (task `task_036bd7b9`):** the
ninth `OptionsList` page, `SettingsPage::Online`, is now built — vanilla's
`OnlineOptionsScreen`, 8 controls including its own Done. The census is now
**143**, and the live count became **context-dependent for the first time**:
the root's second header button (`Placement::Root(2)`) is a live link to
Online when the screen was opened from the title (`!in_world`) and the same
inactive "World Options..." placeholder it always was when opened from the
pause menu (`in_world`).

**Updated again for Key Binds (issue #15):** `SettingsPage::KeyBinds` is
built — vanilla's `KeyBindsScreen`/`KeyBindsList`, the second list-widget kind
this tree needed and the one #392's plan always said was coming. It is
reached from the *Controls* page's own "Key Binds..." button (not the root
grid), and it is not an `OptionsList` page, so it is not part of the
143-control census — see "The Key Binds page" below for its own count. Making
its nav button live add a tenth working nav button unconditionally (it does
not depend on `in_world`), so the live counts above move from 25/24 to
26/25.

**Updated again for issue #415's three screens — Language, Telemetry,
Resource Packs, landed together.** Each is reached from the **root grid**
directly, unlike Key Binds — the three cells that used to be permanently
inactive `no_screen` placeholders (`Language...`, `Telemetry Data...`,
`Resource Packs...`) are all real `nav(..., SettingsPage::…)` cells now, so
the root grid has **no unbuilt nav button left at all** (the one remaining
inactive `Cell::Nav` in the whole tree is the root's own header button
*inside* a world, where it is the World Options placeholder — see "The
root's Online button" below, unchanged by this update). This adds three
unconditional working nav buttons, moving the live counts from 26/25 to
**29/28** (the chat batch later took them to **40/39** — see "What is actually
live" below). None of the three is part of the 143-control census, for the same
reason Key Binds is not:

- [`language-screen.md`](./language-screen.md) — the *third* list-widget
  kind #392's plan always said this tree would eventually need (a
  scrollable, single-select `ObjectSelectionList`).
- [`telemetry-screen.md`](./telemetry-screen.md) — no new widget kind at
  all; an honest prose screen once the event log and opt-in state this
  client structurally cannot have are recognised as absent rather than
  unbuilt.
- [`resource-packs-screen.md`](./resource-packs-screen.md) — a
  **deliberately reduced** selection list rather than vanilla's own
  drag-between-two-lists shape, which this client's asset-loading layer has
  no analogue for at all.

The rest of this doc's per-page tables were not re-audited for any of these
updates; see "What is actually live" below for the current authoritative
counts.

This is issue #55, the settings branch of the menu-framework epic #392.
[`ui-framework.md`](./ui-framework.md) is the plan of record;
[`menu-widgets.md`](./menu-widgets.md) (#393) is the leaf,
[`menu-layout.md`](./menu-layout.md) (#394) the containers,
[`menu-focus.md`](./menu-focus.md) (#395) the input layer.

**The disabled majority is the deliverable.** A greyed row in vanilla's own
position makes the gap between this client and vanilla *visible*; a missing row
silently changes the screen's shape and the gap becomes something you have to
already know about. Vanilla disables its own controls for exactly this reason —
the narrator button (`OptionsSubScreen.java:43-46`), the anisotropy slider
(`VideoSettingsScreen.java:166-167`), telemetry (`OptionsScreen.java:88-92`) — so
this is copying an idiom rather than inventing one.

## How it works

### One mechanism, not thirteen screens

`Options.java` declares 94 `OptionInstance` fields with 93 accessors, and every
settings sub-screen is the same three lines: a `HeaderAndFooterLayout`, an
`OptionsList`, and an `addOptions()` that calls `addBig` / `addSmall` /
`addHeader`. So a settings screen is a **list of options**, and the census is the
call sites — which is why the whole tree here is `static` tables of `Entry` and
`Cell` rather than 135 hand-placed widgets. Adding a screen is adding a `static`.

| type | vanilla |
|---|---|
| `Entry::Header` / `Big` / `Small` | `OptionsList.addHeader` / `addBig` / `addSmall` |
| `Cell::Option(OptionSpec)` | an `OptionInstance` through `createButton` |
| `Cell::Nav` | a `Button` that calls `setScreen` |
| `Cell::Act` | `Done`, the accessibility guide link, Credits |
| `Placement` | *where* a widget is, resolved at draw time |

### The nine `OptionsList` pages, plus Key Binds, and the three that are not here

| page | controls | vanilla |
|---|---|---|
| Root | 13 | `OptionsScreen` — the only page that is **not** an `OptionsSubScreen` |
| Video | 32 | `VideoSettingsScreen`, three headers |
| Accessibility | 27 | `AccessibilityOptionsScreen`, two footer buttons |
| Chat | 19 | `ChatOptionsScreen` |
| Sound | 17 | `SoundOptionsScreen` |
| Controls | 10 | `controls/ControlsScreen` |
| Skin | 9 | `SkinCustomizationScreen` |
| Mouse | 8 | `MouseSettingsScreen` |
| Online | 8 | `OnlineOptionsScreen`, three headers |

Counts include each page's own footer, which is how #55's census counted the
root's Done. Total: **143** (`13+32+10+8+17+19+27+9+8`). Key Binds, Language,
Telemetry and Resource Packs are not in this table — none of the four is an
`OptionsList` page, so "controls" does not mean the same thing for any of
them; see "The Key Binds page" below for its own count (56) and
[`language-screen.md`](./language-screen.md)/[`telemetry-screen.md`](./telemetry-screen.md)/
[`resource-packs-screen.md`](./resource-packs-screen.md) for the other three.

**As of issue #415, every vanilla screen this root grid links to is built** —
the table that used to live here listed which ones were not; it is kept
below as a record of what each needed, since there is nothing left to list
as absent.

| screen | what it needed |
|---|---|
| `LanguageSelectScreen` | the third list-widget kind (`ObjectSelectionList`) — see [`language-screen.md`](./language-screen.md). |
| `TelemetryInfoScreen` | no new widget at all, once the event log and opt-in state this client structurally cannot have are recognised as absent rather than unbuilt — see [`telemetry-screen.md`](./telemetry-screen.md). |
| `PackSelectionScreen` | vanilla's own shape needs two drag-between `ObjectSelectionList`s over a `PackRepository`, a filesystem watcher and a pack detector — this client has none of that. Landed as a declared reduction instead (one always-empty list, one always-one-entry list, no transfer controls) rather than left absent — see [`resource-packs-screen.md`](./resource-packs-screen.md). |

`OnlineOptionsScreen` and `KeyBindsScreen` **used to be in this table** too.
`OnlineOptionsScreen` needed no new list widget at all — it is the cheap win
the Online-settings follow-up landed: the only reason it was absent was that
the root's own header button (`Placement::Root(2)`) was permanently
inactive regardless of context (see "The root's Online button, and
`in_world`" below). `KeyBindsScreen` **did** need a
different list widget —
`KeyBindsList`, not `OptionsList`: `getRowWidth()` 340 (not 310), a flat 20 px
row height with no header-padding rule, two right-anchored buttons per action
row (a 75 px bind button and a 50 px reset button, 5 px apart) instead of
`OptionsList`'s left-anchored columns, and a live key-capture mode. Issue #15
built it — see "The Key Binds page" below and
[`crates/lodestone-shell/src/menu/key_binds.rs`](../crates/lodestone-shell/src/menu/key_binds.rs)'s
own module docs for the full geometry derivation.

### The `OnlineOptionsScreen` page, and what is wired on it

`SettingsPage::Online` (`OnlineOptionsScreen.java:85-116`): a Friends List
header (a small pair — Friends List, Allow Requests), a second small pair
(In-Game Notification, Visibility), a big Xbox Settings link, a Servers header
with Allow Server Listings, and a Realms header with Realms' "News & Invites"
(`options.realmsNotifications.button`, **not** the `options.realmsNotifications`
string — easy to get backwards reading the accessor name alone).

**Every one of those seven is decorative.** This client has no
`PlayerSocialManager`, no Realms client and no Xbox link to send any of them
to, so all seven are `cycle`/`unsupported` with `live: None` — the same
`live: None` majority every other page has. Only two things about this page
are **wired**: the page's own existence (reachable, and its Done button
returns to the root) and the root's Online nav button that reaches it — see
below.

### The root's Online button, and `in_world`

`Placement::Root(2)` is vanilla's `inWorld` fork (`OptionsScreen.java:56-66`):
`Online...` outside a world, `World Options...` inside one. Both used to be
permanently inactive (`no_screen`, `Cell::Nav { page: None, .. }`) — the label
already swapped correctly (`super::render::frame_for` matched on
`(Placement::Root(2), in_world)`), but neither branch went anywhere, which is
what made this the cheap win: only the `!in_world` branch needed a page behind
it.

`options::online_cell(in_world: bool) -> Cell` is now the single place that
decides both the label *and* the liveness — `nav("Online...", SettingsPage::Online)`
outside a world, `no_screen("World Options...")` inside one (`WorldOptionsScreen`
is not built and is out of scope here; that branch is unchanged from before).
`settings_frame` no longer carries a second, draw-only copy of this fork: it
used to override the label for `(Placement::Root(2), true)` independently of
whatever cell `nav.visible()` produced, which is exactly the two-places-agree-
by-luck shape the module docs' departure (1) warns about. Deleting it means
the label and the click can no longer disagree.

**The fact has to reach three places, and now it does through one field.**
`SettingsNav::in_world` is set once, at `SettingsNav::reset(in_world)`, from
[`UiState::settings_in_world`](../crates/lodestone-shell/src/menu.rs) — the
same fact the draw path already had. `MenuNav`'s two Options entry points
(`MainButton::Options`, `PauseButton::Options`) call `reset` in the same
statement that calls `ui.open_settings()`/`ui.open_settings_from_pause()`, so
the two cannot drift apart for the screen's lifetime. Before this change,
`in_world` reached the **label** (draw) but never the **click or the
keyboard** (`SettingsNav::activate`/`enter`/`click_row`) — the row was drawn
correctly and did nothing either way, so the gap was invisible until something
was actually put behind it.

### Two player reports, fixed 2026-08-04

**The root title overlapped the FOV/Online row.** `settings_frame`'s title
`MenuLabel` used `origin: Origin::Settings(Placement::Root(0))` — already the
real, arranged, **absolute** y `root_widget_rects` puts the title at (`12`,
per `the_title_sits_in_its_band_on_every_page`) — and then added `title_y(Root)`
(also `12.0`) as `dy` on top, because the render loop computes
`y = anchor.1 + label.dy`. That drew the title at absolute `y = 24`, four
pixels into the FOV/Online row's own `y = 29`
(`the_root_title_is_centred_on_the_header_block`'s sibling rects give that row
directly). No existing test caught it: `the_title_sits_in_its_band_on_every_page`
checks `title_y` in isolation, and `the_root_title_is_centred_on_the_header_
block` checks only the *x* centring — neither exercises the composed
`anchor + dy` the draw actually uses. Fixed by making the root's `dy` `0.0`
(the anchor already carries the whole offset there; every other page's
`Origin::ScreenTop` anchor is `0.0`, so `dy` still has to carry all of theirs).
`options::tests::no_settings_title_ever_overlaps_a_widget` is the regression
gate — it walks `settings_frame`'s own labels and rows rather than a
hand-derived rect, at two canvases and, since this client carries no second
locale, a synthetic long title standing in for "more than one language string
width".

**In-world Options showed the main-menu panorama instead of the paused
world.** `render::frame_for`'s `Screen::Settings` arm returned `Some` from
`settings_frame` unconditionally, which routes through `app.rs`'s `draw_menu`
Clear pass — the same pass `Screen::MainMenu` uses, and exactly what
`owns_frame`'s own doc says `Screen::Paused` must *not* go through, for the
same reason: it stops the world (and its HUD/container passes) rendering
behind whatever is up. Fixed on the `render.rs` side by returning `None` for
`Screen::Settings` when
[`UiState::settings_in_world`](../crates/lodestone-shell/src/menu.rs) is
`true`, deferring to a `MenuRenderer::render_overlay` block over the
still-rendering world — the same shape `Screen::Paused`/`Screen::Death`
already use. `owns_frame(Screen::Settings)` is deliberately left `true`
regardless of `in_world`, because every other caller of it is about input
routing (mouse/keyboard treated as menu rows), which is true either way; only
`frame_for`, the render decision, has the exception. **This is only the
render-side half** — the `app.rs` overlay block that must draw the deferred
frame is brokered (`app.rs` is not this module's file) and may not have
landed yet; `frame_for_defers_to_an_overlay_for_in_world_settings` is the
regression gate for the half that has.

### A third player report: that panorama fix made in-world Options unclickable

**"i cant click anything in the options menu"** (2026-08-04). Not the geometry —
the *frame source*.

The paragraph above ends with the reasoning that broke it, and it is worth
quoting because it reads as airtight:

> `owns_frame(Screen::Settings)` is deliberately left `true` regardless of
> `in_world`, because every other caller of it is about input routing
> (mouse/keyboard treated as menu rows), which is true either way; only
> `frame_for`, the render decision, has the exception.

`owns_frame` and `frame_for` are not independent. `owns_frame` decides whether a
click on this screen is *routed as a menu row at all*; `frame_for` is where the
rows it routes to come from. `app.rs`'s `menu_row_at` opened with

```rust
let frame = if self.ui.is_paused() { pause_frame(..) }
            else if self.ui.is_death() { death_frame(..) }
            else { frame_for(..)? };          // <-- `?`
```

so making `frame_for` answer `None` in-world left the screen live to the mouse
with **no rows to hit-test**: every click returned at that `?` before reaching
one. Pause and death had each been given a branch there when *they* became
overlay screens; in-world Options became the third overlay screen and did not.

Three things generalise, and the third is the fix:

- **The identical rows on the title screen worked throughout.** That is what made
  the obvious hypothesis wrong: this screen keeps its own entry-index window
  (`LIST_WINDOW_PX`, `visible_entries`, `Placement::ListCell`'s `first`) and never
  adopted the shared pixel-scrolled `ScrollList`, so a units mismatch between the
  two — row index vs. pixels — was the natural suspect after `29d9f88`/`ed2aadc`.
  It is not one. `nav::tests::clicking_an_options_row_at_its_own_coordinates_
  activates_that_row` measures the title-screen path at real coordinates and it
  resolves exactly. When a screen is unclickable, check whether it has a frame
  before checking where its rows are.
- **No `cargo check` and no existing test could see this.** The break is a
  runtime `None`, the render half had its own passing gate
  (`frame_for_defers_to_an_overlay_for_in_world_settings` asserts the `None`
  *is* returned), and every settings click test in the crate clicks by **row
  index** — which is downstream of the hit-test that had already failed. A gate
  that starts from a row index cannot see a hit-test that never produces one.
- **The fix is not a fourth `if`.** Three branches inlined in a private
  `app.rs` method cannot be enumerated from anywhere, which is exactly why the
  third could go missing silently — the same island shape `CLAUDE.md`'s
  "every terminal `_ =>` arm in an event router is an island factory" describes,
  reached without an enum. The branch set now lives in
  `menu::nav::on_screen_frame`, one function answering "which frame is on
  screen", and `nav::tests::every_mouse_routable_screen_has_a_frame_to_hit_test`
  asserts that every screen `owns_frame`/`is_paused`/`is_death` routes the mouse
  to has one. `menu_row_at` calls it instead of `frame_for`.

`nav::tests::in_world_options_clicks_reach_the_row_they_land_on` is the
behavioural gate: it opens Options from the pause menu, derives a click
coordinate from the **GUI Scale** row's own `row_rect`, and requires *that*
option to have cycled 0 → 1. It keeps the pre-fix observation as an in-test
control — `frame_for` must still answer `None` here, or the overlay draw in
`app/redraw.rs` is drawing the screen twice.

### The Key Binds page (issue #15)

`SettingsPage::KeyBinds` — vanilla's `KeyBindsScreen`/`KeyBindsList`. Reached
from the *Controls* page's "Key Binds..." button, not the root grid (vanilla's
own wiring: `ControlsScreen.java:36`).

**Structure is a pure function of the model, not of any live binding.**
[`Category::SORT_ORDER`](../crates/lodestone-shell/src/keybinds.rs) walked
through [`Keybinds::in_category`] gives the row order; only the *label* and
whether Reset/Reset Keys are clickable depend on the live table. Six of
vanilla's eight categories appear — **Creative and Spectator never do**,
because this client has zero `InputAction`s in either (no consumer, so no
table row, per the module's own "absent rather than listed and dead" rule) —
so a header over zero rows never gets a chance to exist.

| what | count |
|---|---|
| categories (headers) | 6 |
| actions | 27 |
| controls per action | 2 (bind, reset) |
| footer | 2 (Reset Keys, Done) |
| **total focusable controls** | **56** (`27×2 + 2`) |

**Wired**: reaching the page and back (Escape/Done → Controls), every one of
the 56 controls, per-row Reset, Reset Keys, and *starting* a rebind (a bind
button click always works and always latches capture — see
[`docs/keybindings.md`](./keybindings.md)). **Decorative until one more hop
lands**: *finishing* a rebind needs a raw key/mouse event `app.rs` does not
yet forward to `MenuNav::capture_binding` — see that doc's "Wiring the
Controls menu" section for the exact two-arm patch. Nothing about this page
persists a stale or self-inflicted bad value: a rebind either completes
through `capture_binding` (which persists immediately) or the capture is
still pending when the player leaves the page, in which case nothing was ever
written and the old binding is exactly what shows next time — there is no
partially-applied state to worry about self-healing.

The `Pause`-unbind hazard this doc's sibling already named ("nothing enforces
that yet") is now enforced, in `capture_binding`: capturing `Unbound` for
`InputAction::Pause` is refused, because that action is the only gameplay
route to the pause screen and so to Quit to Title, and it is not a real
vanilla `KeyMapping` vanilla itself has any equivalent guard for. Escape while
capturing *any* action cancels the capture without changing its binding at
all (not even to `Unbound`) — a deliberate divergence from vanilla's own
`keyPressed`, which sets `InputConstants.UNKNOWN` unconditionally
(`KeyBindsScreen.java:73-74`); see `capture_binding`'s own doc for why.

### What is actually live — and a correction to the census

**Fifteen options** on **eighteen rows**, as of #200/#202/#203 and the chat
batch (up from the two #55 landed with):

| option | page | field |
|---|---|---|
| `guiScale` | Video, under *Display* | `config::Options::gui_scale` |
| `bobView` | Accessibility | `config::Options::view_bobbing` |
| `toggleCrouch` | Controls, as "Sneak" | `config::Options::toggle_sneak` |
| `toggleSprint` | Controls, as "Sprint" | `config::Options::toggle_sprint` |
| `mouseWheelSensitivity` | Mouse, as "Scroll Sensitivity" | `config::Options::mouse_wheel_sensitivity` |
| `invertMouseX` | Mouse | `config::Options::invert_mouse_x` |
| `invertMouseY` | Mouse | `config::Options::invert_mouse_y` |
| `chatColors` | Chat, as "Colors" | `config::Options::chat_colors` |
| `chatScale` | Chat, as "Chat Text Size" | `config::Options::chat_scale` |
| `chatWidth` | Chat, as "Width" | `config::Options::chat_width` |
| `chatHeightFocused` | Chat | `config::Options::chat_height_focused` |
| `chatHeightUnfocused` | Chat | `config::Options::chat_height_unfocused` |
| `chatLineSpacing` | Chat **and** Accessibility | `config::Options::chat_line_spacing` |
| `chatOpacity` | Chat **and** Accessibility | `config::Options::chat_opacity` |
| `textBackgroundOpacity` | Chat **and** Accessibility | `config::Options::chat_background_opacity` |

The last three sit on **two pages each**, which is vanilla's own shape (one
`OptionInstance` placed on both `ChatOptionsScreen` and
`AccessibilityOptionsScreen`), so fifteen options occupy eighteen rows and
editing either row moves the other's label too. See
[`options-consumption-census.md`](./options-consumption-census.md) for what the
remaining greyed rows are each waiting on.

Plus nine `Done` buttons (one per page, always live) and either thirteen or
twelve working nav buttons, depending on `in_world` (these counts are still
only the 143-control `OptionsList` census — Key Binds', Language's,
Telemetry's and Resource Packs' own counts are counted separately, above,
and are not part of either number):

- **Outside a world** (opened from the title): Skin, Sound, Video, Controls,
  Chat, Accessibility (the root grid), Accessibility → Controls, Controls →
  Mouse, Controls → **Key Binds**, the root → **Online**, and the root →
  **Language**/**Telemetry**/**Resource Packs** (issue #415 — every root
  grid button now opens something). **40 live, 103 inactive.**
- **Inside a world** (opened from the pause menu): the same twelve minus the
  root's Online link (it is the inactive World Options placeholder instead)
  — Key Binds', Language's, Telemetry's and Resource Packs' own nav buttons
  are unconditionally live either way. **39 live, 104 inactive.**

Both counts are asserted directly —
`options::tests::the_disabled_majority_is_the_point_and_it_is_measured` (40,
outside a world, the canonical/title-screen baseline) and
`options::tests::the_root_online_button_is_the_one_row_that_changes_with_in_world`
(both, and that the delta between them is exactly the Online row — Key
Binds' and the other two #415 pages' own buttons contribute to both equally,
so none of them is the row that test is about).

#55's census comment and `ui-framework.md` both say **"4 of 93"**, listing
`render_distance` and `sensitivity` alongside those two. That is wrong, and it is
wrong in the way `CLAUDE.md`'s rule 2 describes — it was written by counting
`config.rs`'s public fields and it counted **two structs**:

- `config::Options` is the *persisted* struct (`options.json`). It has three
  fields: `gui_scale`, `keybinds`, `view_bobbing`. Only two are vanilla
  `OptionInstance`s.
- `config::Config` is *argv*, "parsed fresh from argv every run and never written
  back" in its own doc comment. `render_distance` and `sensitivity` live there.

So a Render Distance or Sensitivity row that appeared to work would be fabricated
persistence: the value would be honoured for the rest of the session by accident
of the CLI default and lost on restart. Both are rendered inactive. Making them
live is real work — a field on `Options`, a JSON key, and a consumer in `app.rs`
that prefers it over the flag — and `sensitivity` additionally cannot be an `f32`
without dropping `Options`' `Eq`.

### Geometry: `OptionsList`, transcribed

Row positions come from four vanilla expressions and nothing else:

```text
list.updateSize(width, layout)  -> position (0, layout.getHeaderHeight())
getFirstEntryY()               -> list.getY() + 2
entry y                        -> firstEntryY + sum(previous entry heights)
Entry.getContentY()            -> entry.y + 2
Entry.extractContent x         -> screen.width / 2 - 155 + column * 160
```

Entry heights are the list's `itemHeight` of 25, except a header, which is
`paddingTop + 9 + 4` where `paddingTop` is **0 for the first entry in the list
and 18 after** (`OptionsList.java:56-60`). That first-header case is why
`entry_height` takes an index rather than being a method on `Entry`.

The root page is different: it has no list. Its header is a vertical
`LinearLayout` (title + a horizontal FOV/Online pair) in a 61 px band, its
content a 2×5 `GridLayout` of 150 px nav buttons, its footer a 200 px Done. That
tree is **built and arranged for real** — `root_widget_rects` is
`HeaderAndFooterLayout`'s first production consumer (#394 landed it with
arithmetic-only gates and a note saying no screen used it yet).

### Where a row's rect comes from

`render::frame_for` runs **before the canvas is known**, so every
canvas-dependent term has to live behind an `Origin`. `Origin::Settings` is the
only variant that carries data, and it has to: a row's position depends on the
page, the entry, *and how far the list is scrolled*.

```text
MenuRow.slot = Slot { origin: Origin::Settings(Placement), dx: 0, dy: 0, w, h }
Origin::anchor(w, h) -> options::placement_anchor(placement, w, h)
```

`row_rect` is still the single definition of where a row is, which is why the
draw, the mouse hover and `app.rs`'s hit-test cannot drift apart.

Unlike `Origin::PauseGrid`, this one **runs a layout** rather than evaluating an
expression against a cached arrangement: `HeaderAndFooterLayout` places its
content band from the canvas *height*, so there is no canvas-independent
arrangement to cache. It is ~15 small boxes per resolution on a screen with no
world behind it.

### Sliders draw a different widget

`OptionInstance.createButton` dispatches on the `ValueSet`: a `CycleableValueSet`
gets a `CycleButton` (an `AbstractButton`, so `widget/button*`), a
`SliderableValueSet` gets an `OptionInstanceSliderButton` (an
`AbstractSliderButton`, so `widget/slider*`). `MenuRow::slider` carries that, and
`widget::SLIDER_SPRITES` is the sprite set.

**`guiScale` is a cycle button, not a slider**, and getting this backwards would
draw a slider track under the one option on the Video page that works: its
`ValueSet` is a `ClampingLazyMaxIntRange`, whose `createCycleButton()` returns
`true` (`OptionInstance.java:213-216`).

This section used to say "no live option in this client is a slider" and
conclude that `MenuRow::slider` was a `bool` and not a value for that reason.
That stopped being true when issue #203 gave `mouseWheelSensitivity` a real
live value — it kept a slider *widget*, unlike `guiScale`. A player report
(2026-08-04, "no sliders for sound — they are buttons") is what caught the gap
that stale claim had left uncovered: nothing ever drew a handle for it either,
because the field it would have needed did not exist. `MenuRow` now also
carries `slider_value: Option<f32>` — see departure 2, rewritten below.

## The four deliberate departures from vanilla

Each is a judgement call rather than a shortcut, so each is written down with what
the alternative would have cost.

**1. An inactive option shows its caption alone.** Vanilla shows
`genericValueLabel(caption, value)` — `"%s: %s"` (`Options.java:1974-1976`). We
hold no value for an option we do not honour, and printing one would be the
fabricated persistence this issue exists to avoid: a row reading
`Entity Shadows: ON` beside a client that draws no shadows is a lie a screenshot
cannot tell from a working feature. The two live options *do* use
`genericValueLabel`.

**2. A slider draws a handle only where a real fraction is known — rewritten
2026-08-04.** This used to say every slider draws its track and *no* handle at
all, because putting one anywhere would be departure 1 in pixels instead of
text — but that reasoning only holds when the fraction is genuinely unknown,
and it is not, for two kinds of slider:

- `mouseWheelSensitivity` (issue #203) is live, so its fraction is the real,
  persisted config value, run through the same `unlogMouse` +
  `IntRangeBase.toSliderValue` math the jar uses
  (`Cell::slider_fraction`/`mouse_wheel_slider_fraction`, `options.rs`).
- Every other slider is still inactive, but for one built on
  `OptionInstance.UnitDouble.INSTANCE` (all eleven `SoundSource` volumes among
  them), vanilla's own `OptionInstance` boots with a concrete default double,
  and `UnitDouble.toSliderValue` is the identity — so that default *is* the
  slider fraction, not a guess. `UNIT_DOUBLE_DEFAULTS` in `options.rs` is that
  table, one entry per accessor, cited to the `Options.java` line it boots
  from.

- Every `IntRange`-family slider now has its real bounds ported too (issue
  #424): `SliderRange` in `options.rs` plus the `INT_RANGE_SLIDERS` table, one
  row per accessor, each citing the `Options.java` line its `(min, max)` and
  default are read from. `SliderRange::to_slider_value` is
  `IntRangeBase.toSliderValue` (`OptionInstance.java:295-301`) transcribed.
- `graphicsPreset` is an `OptionInstance.SliderableEnum`, a third family whose
  divisor is `size - 1` rather than a bucket width (`:486-492`), so it has its
  own one-line function instead of borrowing `SliderRange`.

**Three things about that port are easy to get wrong, and each is gated.** The
`+ 0.5` and `max + 1` are there because an `IntRange` slider selects a *bucket*
(`fromSliderValue` floors, `:303-309`), so the handle marks a bucket's centre;
the naive `(v - min) / (max - min)` is a different function. The two endpoint
special cases are not an optimisation — without them a maxed-out slider draws
its handle short of the end, and `mipmapLevels` (whose shipped default *is* its
max) sits at 0.9. And `fov`'s `Codec.DOUBLE.xmap` is a **persistence** codec,
not a `ValueSet::xmap`, so it must not touch the slider at all.

Observed, not described: with the naive formula
`every_int_range_slider_lands_on_vanillas_own_fraction` fails at
`framerateLimit` (0.44 against vanilla's 0.4423), and with the endpoint cases
removed it fails at `mipmapLevels` (0.9 against 1.0). Both rival formulas are
kept **executable** in `mod rival`, and
`the_naive_endpoint_span_hypothesis_is_measurably_wrong` additionally asserts
that its three chosen rows are still *far enough apart to discriminate* — it
fails loudly if a range change ever makes the control vacuous, which is what it
did when the naive formula was installed. `fov`,
`menuBackgroundBlurriness` and `maxAnisotropyBit` are recorded as rows where the
two formulas coincide algebraically, so a gate built only from those would prove
nothing.

`renderDistance`/`simulationDistance` are the one runtime-decided bound:
vanilla's max is `largeDistances ? 32 : 16`, and `largeDistances` asks whether
the **JVM's `-Xmx` heap cap** is at least 1 GB (`Options.java:1469`). There is no
JVM here and no equivalent ceiling, so `LARGE_DISTANCES_MAX = 32` is a documented
**decision**, not a citation, and is named as a constant so it stays visible.

`fullscreenResolution` still draws no handle, and that is the honest answer
rather than a gap: its value set is the monitor's real video-mode list, so it has
neither a range nor a default int. `every_slider_the_tree_renders_can_place_its_handle`
sweeps every slider row on every page in both in-world states and asserts that
it is the *only* one, in both directions — a ported range that no row renders
fails the same test as a row with no range.

`Widget::slider_handle_sprite` draws the handle sprite itself;
`MenuRow::slider_value: Option<f32>` is what carries the fraction from
`Cell::slider_fraction` to the draw.

**There is no JVM runtime on this machine**, so vanilla's `toSliderValue` could
not be executed to produce an oracle. The expectations are the jar's formula
applied by hand to the jar's own transcribed numbers — two independent paths to
each value — plus the two executed rival formulas above. If a JVM becomes
available, running `OptionInstance`'s own method over these accessors is the
stronger check and should replace the hand arithmetic.

**3. The keyboard's scroll-into-view runs against the shortest canvas.**

This departure used to say something else, and it is worth keeping what it said
because the staleness is the lesson: *"the scroll snaps to whole entries, and the
visible window is a fixed pixel budget … this menu pipeline has no scissor, so a
row that overran the band would paint over the footer … This is the departure most
worth revisiting: a scissor in the menu pipeline would delete it."*

**Both halves of that were already false.** Issue #445 converted this screen to a
continuous pixel offset *and* gave the pipeline a real CPU scissor
(`render::draw`'s `Quads::with_clip`) — but the prose survived, and it was still
being read as the reason a limitation existed. It cost a player two real defects,
reported 2026-08-07 as *"some text overlaps, is in the wrong place, and when I
scroll it doesn't reach the end"*:

- `with_clip` reached the three screens whose rows are list **entries**
  (`MenuRow::entry`/`account`/`world`) and **not** the settings tree, whose rows
  are slotted widgets — so a settings row scrolled past the band still painted
  over the footer's Done button. `Origin::is_scrolling_list_row` is the predicate
  that fixes it, and it deliberately excludes the footer, the title and
  `OptionsScreen`'s own grid: clipping *those* to the band would erase them.
- `SettingsNav::scroll_to_cursor` runs without a canvas (a keypress has none), so
  it clamps against `config::MIN_SCALED_HEIGHT`, where the Video page's
  `maxScrollAmount` is **330**. At 854×480 the real maximum is **90**. The rows
  were placed from the raw 330 while the scrollbar — which goes through
  `ListSpec::model` — was placed from the clamped 90: two readers, two numbers,
  and the list drawn 240 px past its own end with its top rows behind the header.

What is left of the departure is only the first bullet's cause: the keyboard
cannot know the canvas. `options::drawn_scroll` re-clamps where the canvas is
first known — vanilla's own `refreshScrollAmount`, which `updateSizeAndPosition`
calls after every resize — so the rows, the bar and the clip are three readers of
one expression. The residue is that arrowing to the bottom of a long page can
reach the end slightly earlier than it strictly had to at a tall canvas. Never the
other way round: the cursor's row is always inside the band, and
`arrowing_to_the_end_of_the_video_page_reaches_its_last_control_at_every_canvas`
is the gate, swept over four canvases with the control count asserted as a
precondition off the page's own control list.

**Re-measured 2026-08-08, on the wheel arm and on the *draw*, and the list does
reach its end.** The gate above drives the keyboard and reads `list_cell_origin`;
`wheeling_to_the_clamp_puts_the_last_row_at_the_end_of_the_band` drives 200 wheel
notches on every page at four canvases and reads `render::row_rect` off a real
`settings_frame`, against the band `list_spec` hands the clip. That distinction is
why it was worth re-running: the resource-pack screen had a fully green suite
while drawing the wrong thing, because every test asserted on frame data and
nothing asserted on the draw. What it found:

| page/canvas | result |
|---|---|
| Video 240 / 318 / 480 | wheel lands on `max_scroll` exactly (330 / 252 / 90) |
| Key Binds 240 / 318 / 480 / 720 | the same, at 530 / 452 / 290 / 50 |
| every scrollable page, every canvas | the last entry box ends exactly `LIST_CONTENT_PADDING` (2 px) above the band's bottom — vanilla's own trailing padding |

So the remaining suspicion — that `list_cell_origin`'s
`LIST_TOP_INSET + entry_offset + ENTRY_CONTENT_INSET` walk and
`ScrollList::row_top`'s `first_entry_y + row_offset` are **two expressions for one
quantity**, unlike every other list in this tree — is real as a *risk* and is not
a defect: `LIST_TOP_INSET` and `LIST_CONTENT_PADDING` are both 2 px and
`entry_offset` is the same sum `with_heights` was handed. The same gate now asserts
`list_cell_origin(entry) == row_top(entry) + ENTRY_CONTENT_INSET` for **every**
entry of every page at four canvases, which is what keeps them agreeing: a drift
between them presents as "scrolling does not reach the end" with nothing visibly
wrong at either site on its own.

Language could not be measured hermetically — a default `LanguageNav` has one
entry, so its list does not scroll and the measurement is a vacuous
*precondition* rather than a pass. Its geometry is Key Binds' algebra exactly
(a uniform `ListSpec`, `row_y = first_entry_y + row * ROW_H - scroll`), and that
one is measured.

**The parity half of the same report — *"I don't see options in the same places as
26.2 vanilla"* and *"I don't see the render distance option"* — was audited and
found to be the same defect, not a composition one.** Every page's entry order
was re-read against the jar's `addOptions` and matches exactly, root grid
included; Render Distance is on Video's Quality & Performance section paired with
Biome Blend, and live since #443. Three traps were checked rather than assumed:

| trap | what a summary of the call site would have produced |
|---|---|
| `AccessibilityOptionsScreen` calls `addSmall(widget, OptionInstance, widget)` | a **duplicated Narrator row** — that middle argument is `findOption` metadata, not a third widget (`OptionsList.java:52`) |
| `SoundSource` has **eleven** values, `UI` last | a Sound page one row short |
| Video's `qualityOptions` has **seventeen** entries | eight pairs and no `weatherRadius` |

**4. Up/Down move the cursor over *every* control, including inactive ones.**
`AbstractWidget.nextFocusPath` returns `null` when `!isActive()`
(`AbstractWidget.java:152-158`), and `MenuNav`'s `step_enabled` skips inactive
rows on the title and pause screens. On a screen whose *content* is the inactive
majority, skipping them would leave most of the tree's 143 controls unreachable
**and unscrollable** — i.e. invisible, which defeats the whole issue. The vanilla
predicate still governs *activation*: Enter on an inactive row does nothing, and
`WidgetSprites::get(false, true)` keeps it drawing `widget/button_disabled` under
the cursor exactly as vanilla does.

## How to change it

- **Adding a control is a table edit.** `static VIDEO`, `static CHAT`, … in
  `options.rs`, in vanilla's own array order — `addSmall` walks an array two at a
  time (`OptionsList.java:37-42`), so the two columns of a row are *consecutive
  entries of vanilla's array* and the last is alone when the count is odd. Reorder
  a table and `the_per_screen_control_counts_are_the_censused_ones` plus
  `the_settings_rows_are_in_the_order_click_assumes` are what catch it.
- **Making an option live is three edits and one of them is not here.** A
  `LiveOption` variant, `live: Some(..)` on the spec, an arm in
  `SettingsNav::activate`'s caller (`MenuNav::apply_settings`) — and a **consumer**
  that actually honours the field. Without the fourth, the row is an island: it
  will cycle, persist and change nothing on screen.
- **Never label an option you do not honour with a value.** See departure 1. The
  test that holds the line is
  `an_inactive_option_shows_its_caption_and_a_live_one_shows_its_value`, which
  asserts both directions.
- **A page's control order is one index space**, shared by the keyboard cursor,
  the mouse hover, `app.rs`'s hit-test and `SettingsNav::activate` — exactly as
  `MAIN_BUTTONS`' order is on the title screen. If you add a widget that is drawn
  but not focusable (a header `StringWidget` is one), it must reach the frame as a
  `MenuLabel` and **not** enter `controls()`, or every row index after it is wrong.
- **Escape unwinds a history stack, not a `parent()`.** The tree is a *graph*:
  Accessibility links to Controls, which the root also links to. "Where did I come
  from" is history.
- **`SettingsNav::reset(in_world)` on every entry to the screen.** Vanilla
  builds a new `OptionsScreen` each time, so re-entering Options must not
  resume three pages deep — and, since the Online page, must also re-derive
  the root's Online/World Options fork from the entry point actually used
  rather than carrying over the previous visit's. `MenuNav`'s two
  `PauseButton::Options`/`MainButton::Options` arms call it with `true`/`false`
  respectively, in the same statement that calls
  `ui.open_settings_from_pause()`/`ui.open_settings()`.

## How it is proved

`options.rs`'s own tests, each with the vanilla line cited and, where an absence
is claimed, a control:

- **The census**, per screen and in total, against #55's `addBig`/`addSmall`
  call-site counts — an expected value from outside the code under test.
- **The live/inactive ratio**, asserted as an exact list of `LiveOption`s, so
  quietly enabling a row has to say so here. Its control is `renderDistance`,
  which must report itself inactive while `guiScale` reports live through the same
  predicate.
- **Label composition**, both directions: `genericValueLabel` for a live row,
  caption alone for an inactive one.
- **`guiScale` is not a slider**, with `renderDistance` as the control.
- **Header heights**, both branches of the first-entry `paddingTop` rule, with the
  two asserted *unequal* so an implementation that ignored the index fails.
- **Row geometry** against hand-derived numbers (`480 / 2 - 155 = 85`, the
  `+160` column, `33 + 2 + 2 = 37`), plus Java integer division on an odd width.
- **The visible window never overruns the footer at
  `MIN_SCALED_HEIGHT`** — swept over every page at every scroll position, with an
  executed control: the first entry the window *rejects* must be one that genuinely
  would not have fitted.
- **Scrolling reaches every entry on the longest page**, by stepping the cursor
  through all 32 of Video's controls and requiring the union of the windows to be
  every entry. This is departure 4's anti-island gate: it is what would fail if the
  cursor skipped inactive rows.
- **`the_settings_rows_are_in_the_order_click_assumes`**, swept over every page at
  every scroll position: the frame's rows and the control list must agree in
  length, label, `enabled` **and placement**. This is #391's guard.
- **`a_click_acts_on_the_row_it_landed_on_and_nothing_else`** is #391's shape
  directly: click GUI Scale and it cycles; click its inactive left-hand neighbour
  and *nothing happens*; click past the end of the frame and nothing happens.
- **`hover_and_the_cursor_agree_on_every_visible_row`** — hovering row *n* must
  select row *n*, for every row of every page at every scroll position.
- **The root layout** against hand-derived rects for all fourteen widgets, and
  the content band's `Math.min` clamp asserted on both sides (a canvas with room
  and one without).
- **`every_placement_resolves_to_a_rect_on_screen`** is the anti-island assertion
  at this layer: a `Placement` whose index ran past its arranged tree resolves to
  an off-canvas sentinel, so it fails here rather than drawing nothing and looking
  like a table that was never wired. Swept at both `in_world` values, since the
  Online page changed which cell `Placement::Root(2)` resolves to.
- **The root's Online/World Options fork reaches activation, not only the
  label.** `the_root_header_button_follows_vanillas_in_world_fork` now asserts
  `enabled` in both directions (it used to assert only the label);
  `navigation_walks_the_tree_and_escape_unwinds_it` drives an actual `enter()`
  into `SettingsPage::Online` outside a world and asserts the same row is inert
  inside one; `the_root_online_button_is_the_one_row_that_changes_with_in_world`
  is the census-level version — the live count differs by exactly one row
  between the two contexts, named.

`nav.rs` re-checks the two that cross a file boundary through the **real**
`frame_for`, which is the path `app.rs` uses, and the four rewritten
settings tests drive the tree with nothing but the keys a player has — reaching
GUI Scale by pressing Down is what proves the row is reachable at all.

**Key Binds (issue #15)** has its own test file section, `key_binds.rs`'s own
`mod tests` plus `nav.rs`'s integration tests, following the same rules:
`six_categories_carry_all_twenty_seven_actions` and
`actions_walk_registration_order_not_declaration_order` are the census and
ordering guards (the latter's control is deliberately named: walking
`InputAction::ALL` directly instead of `Category::SORT_ORDER` passes every
*other* test in that file and still renders the categories in the wrong
relative order); `a_click_acts_on_the_row_it_landed_on_and_nothing_else` is
#391's shape again, one screen further; and `nav.rs`'s
`clicking_a_bind_button_then_capturing_a_key_rebinds_and_persists`/
`escape_while_capturing_cancels_without_changing_the_binding`/
`capturing_pause_refuses_to_leave_it_unbound` drive `MenuNav::capture_binding`
directly — the same call `app.rs`'s still-outstanding patch is specified to
make — so persistence, cancellation and the `Pause` hazard are all proved on
this crate's side of that hop before the patch exists to close it.

`widget.rs`'s `a_slider_has_a_track_but_no_disabled_track` earned its place: the
first version of `SLIDER_SPRITES` used the 2-argument collapse, which puts the
*focused* sprite in `disabledFocused`, and it reported
`widget/slider_highlighted` for a greyed-out slider under the cursor. Vanilla's
predicate is a **conjunction** (`isActive() && isFocused() && !canChangeValue`),
which is the 3-argument collapse with `enabled == disabled`. The two collapses
are not interchangeable, and the difference is only observable on a
disabled-and-focused widget — a state vanilla never reaches, because
`nextFocusPath` refuses focus to an inactive widget, and this shell reaches on
purpose (departure 4).

## Configuration

- `crates/lodestone-shell/src/config.rs` — `Options` (`options.json`): the
  fifteen live settings (see [`LiveOption`] in `options.rs`), plus
  `UNIT_DOUBLE_STEP`, how far one click moves a `UnitDouble` slider.
  `MIN_SCALED_HEIGHT` is public because `LIST_WINDOW_PX` is derived from it.
- `crates/lodestone-shell/src/resources.rs` — `load_menu_gui_atlas()` supplies
  `widget/button*` and `widget/slider*`. Without a pack the rows fall back to flat
  fills, which is what the jar-less and headless paths see.

## Dependencies

- `menu/layout.rs` — `HeaderAndFooterLayout` (first production consumer),
  `GridLayout`, `LinearLayout`, `LayoutSettings`, `widget_rects`, `ipx`.
- `menu/widget.rs` — `Widget`, `WidgetSprites`, `BUTTON_SPRITES`,
  `SLIDER_SPRITES`, the grey `-6250336`.
- `menu/render.rs` — `Origin::Settings`, `Origin::KeyBinds`, `MenuRow::slider`,
  `draw_widget`.
- `menu/nav.rs` — `MenuNav::settings`, `key_settings`, `apply_settings`,
  `key_key_binds`, `apply_key_binds`, `awaiting_key_capture`,
  `capture_binding`, and the eager persistence rule.
- `menu/key_binds.rs` (issue #15) — the Key Binds page's own model, geometry
  and `KeyBindsNav`; see that module's own docs and
  [`docs/keybindings.md`](./keybindings.md) for the layer underneath it.
- The 26.2 jar at `.cache/mc/26.2/{client-src,client.jar}` — behavioural reference
  only, and `assets/minecraft/lang/en_us.json` for every caption verbatim.

## See also

- [Menu UI framework](./ui-framework.md) — the epic's plan and the census.
- [Menu widgets](./menu-widgets.md) — the disabled path, and the slider sprite
  correction above.
- [Menu layout containers](./menu-layout.md) — `HeaderAndFooterLayout`.
- [Main menu](./main-menu.md), [Pause menu](./pause-menu.md) — the two entry
  points.
- [View bobbing](./view-bobbing.md) — one of the two live options, and #391.
- [Keybindings](./keybindings.md) — the model Key Binds (#15) is a screen over,
  and the one raw-input hop `app.rs` still needs to patch.
