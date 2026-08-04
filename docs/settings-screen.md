# The settings tree, with unsupported controls disabled

## What it is

`crates/lodestone-shell/src/menu/options.rs` — vanilla's `OptionsScreen` tree as
a **table plus arithmetic**: nine `OptionsList` pages, **143 controls**, of
which **28 or 29 work** (see below) **and the rest are present and greyed
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
**29/28**. None of the three is part of the 143-control census, for the same
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

**Seven options**, as of #200/#202/#203 (up from the two #55 landed with):

| option | page | field |
|---|---|---|
| `guiScale` | Video, under *Display* | `config::Options::gui_scale` |
| `bobView` | Accessibility | `config::Options::view_bobbing` |
| `toggleCrouch` | Controls, as "Sneak" | `config::Options::toggle_sneak` |
| `toggleSprint` | Controls, as "Sprint" | `config::Options::toggle_sprint` |
| `mouseWheelSensitivity` | Mouse, as "Scroll Sensitivity" | `config::Options::mouse_wheel_sensitivity` |
| `invertMouseX` | Mouse | `config::Options::invert_mouse_x` |
| `invertMouseY` | Mouse | `config::Options::invert_mouse_y` |

Plus nine `Done` buttons (one per page, always live) and either thirteen or
twelve working nav buttons, depending on `in_world` (these counts are still
only the 143-control `OptionsList` census — Key Binds', Language's,
Telemetry's and Resource Packs' own counts are counted separately, above,
and are not part of either number):

- **Outside a world** (opened from the title): Skin, Sound, Video, Controls,
  Chat, Accessibility (the root grid), Accessibility → Controls, Controls →
  Mouse, Controls → **Key Binds**, the root → **Online**, and the root →
  **Language**/**Telemetry**/**Resource Packs** (issue #415 — every root
  grid button now opens something). **29 live, 114 inactive.**
- **Inside a world** (opened from the pause menu): the same twelve minus the
  root's Online link (it is the inactive World Options placeholder instead)
  — Key Binds', Language's, Telemetry's and Resource Packs' own nav buttons
  are unconditionally live either way. **28 live, 115 inactive.**

Both counts are asserted directly —
`options::tests::the_disabled_majority_is_the_point_and_it_is_measured` (29,
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
`true` (`OptionInstance.java:213-216`). So no live option in this client is a
slider, which is why `MenuRow::slider` is a `bool` and not a value.

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

**2. An inactive slider draws its track and no handle.** The handle's position
*is* the value (`AbstractSliderButton.extractWidgetRenderState`), so putting one
at 0 is departure 1 in pixels instead of text. This is the one place where the
absence of a component is the honest render; it is not "disabled art", which
`menu-widgets.md` correctly forbids for this widget family. `Widget::slider`'s
doc says where the handle goes when a slider does go live.

**3. The scroll snaps to whole entries, and the visible window is a fixed pixel
budget.** `AbstractSelectionList` scrolls continuously and scissors the band;
this menu pipeline has no scissor, so a row that overran the band would paint over
the footer. `LIST_WINDOW_PX` is therefore derived from the **shortest** content
band any `gui_scale` can produce (`config::MIN_SCALED_HEIGHT`, vanilla's
`Window.java:453`), which makes it correct at every canvas and *conservative* at
large ones — seven 25 px rows where vanilla would show eleven or twelve.
`menu/accounts.rs`'s `VISIBLE_ROWS` is the existing precedent for the same trade.
This is the departure most worth revisiting: a scissor in the menu pipeline, or a
`&mut MenuNav` in `frame_for`, would delete it.

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
  seven live settings (see [`LiveOption`] in `options.rs`). `MIN_SCALED_HEIGHT`
  is public because `LIST_WINDOW_PX` is derived from it.
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
