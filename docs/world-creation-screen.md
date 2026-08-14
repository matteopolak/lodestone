# World Creation screen

## What it is

`Screen::CreateWorld` (issue #190): vanilla's `CreateWorldScreen`, reached
from the world list's "Create New World" button
(`crates/lodestone-shell/src/menu/world_select.rs`'s
`WorldSelectButton::Create`, now live — issue #397 left it
present-and-disabled deliberately for this issue). Collects a world name,
seed, world type, game mode, difficulty, three toggles (generate structures,
bonus chest, allow cheats), and online mode, across vanilla's own three tabs
(issue #567).

> **Update (issue #567): this screen has real tabs now, not one flat list.**
> The section below titled "Not vanilla geometry, on purpose" is kept as the
> record of the reasoning that held until the owner asked for tabs
> explicitly ("we need it to match the real vanilla UI for it — which has
> tabs, etc."). See [Tabs](#tabs-issue-567) for the current shape.

## How it works

- `menu/create_world.rs` — the whole model: `WorldCreationConfig` (the
  collected fields), `WorldGameMode`/`WorldDifficulty`/`WorldTypePreset` (the
  cycle values), `CreateWorldWidgets` (two real `EditBox`es — Name, Seed —
  plus seven `Widget` rows for the cycles/toggles/footer, one
  `FocusChildren` struct mirroring `menu/nav.rs`'s `FormFields` and
  `world_select.rs`'s `WorldSelectWidgets`), `CreateWorldNav` (the live
  state: widgets, focus, config, and which tab is active), and `frame` (the
  whole screen).
- `menu.rs` — the `Screen::CreateWorld` variant, `UiState::open_create_world`/
  `close_create_world`.
- `menu/nav.rs` — `MenuNav::create_world`, `key_create_world`/
  `apply_create_world`, and `WorldSelectOutcome::CreateWorld` threaded
  through `apply_world_select` (which is now a method rather than an
  associated function, so it can reset `create_world` on entry).
- `menu/world_select.rs` — `WorldSelectButton::Create.enabled()` flipped to
  `true`, `WorldSelectOutcome::CreateWorld` added, `press` routes to it.

## Tabs (issue #567)

Vanilla's `CreateWorldScreen` is three `GridLayoutTab`s (Game/World/More)
inside a `MenuTabBar`. This screen now has the same three, built from the
[shared tab widget](./statistics-screen.md#the-tab-widget-issue-564) issue
#564 built for the Statistics screen — this is that widget's second
consumer, exactly as the owner asked for ("the same tabs that the Create New
World UI uses").

**Building this second consumer is what surfaced a real, pre-existing bug in
the first one.** `draw`'s tab-row branch had been nested inside a
`row.slot.is_some()` gate since issue #564 landed it, and a tab row never
carries a `slot` — so `draw_tab` was dead code from day one, and every tab
row on *both* screens silently drew as a plain button instead. Wiring this
screen's own tabs and gating them with a render-level test (not just a
`create_world.rs`/`stats.rs` frame-construction one) is what caught it. See
[Statistics's own tab-widget section](./statistics-screen.md#the-tab-widget-issue-564)
for the full account — the fix lives entirely in `draw.rs`, so nothing here
had to change once it landed.

| tab | fields |
|---|---|
| **Game** (`createWorld.tab.game.title`) | Name, Game Mode, Difficulty, Allow Cheats |
| **World** (`createWorld.tab.world.title`) | Seed, World Type, Structures, Bonus Chest, Online Mode |
| **More** (`createWorld.tab.more.title`) | nothing |

Vanilla's `GameTab` also has an Experiments button and `WorldTab` a
"Customize Type" button this client has no experiments/preset-editor screen
for — left absent rather than drawn inert, the same call every field in this
tree already makes for a control with no backing model. `MoreTab` is three
buttons (Game Rules, Experiments, Data Packs) and none of the three models
exist here (no game-rule table, no experiments screen, no data-pack loader —
`world_select.rs`'s own module docs on the missing `LevelStorageSource`
still apply). **More is still selectable and enabled**, not disabled: unlike
Statistics's Items/Mobs — which vanilla itself disables *because the
underlying list is empty* (`StatsScreen.setTabActiveStateAndTooltip`) —
nothing about More is data-driven-empty. It is feature-not-yet-built, and
disabling the tab would misrepresent that as vanilla's own behaviour.

`ONLINE_MODE_ROW` has no vanilla tab at all (see its own doc below) and lives
on World, after Bonus Chest — a network-exposure setting for the world being
created, closer in kind to World's "how does this world generate/behave"
fields than to Game's account-permission ones.

**Two index spaces.** `CreateWorldWidgets`' focus ids (`NAME_FIELD` through
`CANCEL_ROW`) are stable identifiers `FocusSet` and `activate` key off;
`CreateWorldNav::click_row`/`hover_row` take a *different* number — the index
into `frame`'s own `MenuFrame::rows`, which changes shape with the active tab
(three tab-bar rows, then whichever tab's own content, then the two footer
rows). The two coincided by construction before tabs existed; now
`CreateWorldNav::frame_row_for_focus_id`/`focus_id_for_frame_row` are the one
pair of functions that convert between them — see `create_world.rs`'s own
module doc for the reasoning in full.

**Tab switching sets each widget's own `active` flag** rather than rebuilding
`FocusSet`'s registries (`CreateWorldNav::sync_tab_visibility`): a field not
on the showing tab becomes inactive, and `FocusTarget::takes_focus` already
reads `is_active()`, so Tab traversal skips it with no special case. The
difficulty row folds the tab-membership check into its existing hardcore
lock rather than adding a second field, since both are "can this row take
focus or a click right now" and need to combine, not override one another.

**Not ported: per-tab keyboard focus order.** Vanilla's `MenuTabBar` is
itself focusable, in tab-order group 0 ahead of the footer — the same
divergence the [Statistics screen's own focus test](./statistics-screen.md)
already documents for the same widget. This screen's bar is fully
**clickable** (all three tabs switch content; Statistics only has one live
tab to click), but not yet reachable by Tab.

### Not vanilla geometry, on purpose (the record this replaced)

This is kept as the record of the reasoning that held from issue #190
through #564, and is why the fields *within* each tab are still a hand-placed
flat column rather than vanilla's `GridLayout` arithmetic:

> `WorldCreationUiState` (326 lines) tracks a world-type preset list, data
> packs, game rules and a temp save folder on disk. None of that fits this
> pipeline or this client: there is still no `LevelStorageSource`, no
> data-pack loader, and no game-rule model. Building the full preset/data-pack
> machinery to hold a handful of fields that do get real support would be
> geometry in service of nothing.

What changed since: the owner's #567 report treated *lacking tabs at all* as
the defect, independent of how complete the fields under them are — the same
call [Statistics](./statistics-screen.md) already made (its own Items/Mobs
are present-and-inactive, not absent). The within-tab layout is still hand-
arithmetic (`docs/ui-framework.md` already names this legitimate — even
`TitleScreen` itself uses no layout class), just now arranged per tab instead
of in one flat column.

## World type (issue #519's UI half)

`WorldTypePreset` cycles all seven bundled `world_preset/*.json` documents
(`generator.minecraft.*` captions, verbatim) and is collected on
`WorldCreationConfig::world_type` — real state, real cycling, on the World
tab. **Decorative for all seven today**, and unevenly so:

| preset | generator | reachable from `lodestone-shell` without a `lodestone-server` change? |
|---|---|---|
| `Normal` | `overworld_chunk_source` | yes |
| `LargeBiomes` | `overworld_chunk_source_of_type(seed, WorldType::LargeBiomes)` | yes |
| `Amplified` | `overworld_chunk_source_of_type(seed, WorldType::Amplified)` | yes |
| `SingleBiomeSurface` | `single_biome_chunk_source` | no — not yet re-exported from `lodestone-server`'s root |
| `Flat` | `flat_chunk_source` | no |
| `FlatAllDimensions` | `flat_chunk_source` (all-dimensions settings) | no |
| `DebugAllBlockStates` | `debug_chunk_source` | no |

See [`worldgen-world-type-selection.md`](./worldgen-world-type-selection.md)
for the full entry-point table and what each generator actually does — that
doc is the authority; do not re-derive the names here.

**The remaining hop for the first three is one call site, and it is
deliberately untouched by this pass.** `net.rs`'s `Origin::Integrated`
construction calls `lodestone_server::overworld_chunk_source(seed)` exactly
once; making it read `WorldCreationConfig::world_type` means adding a field
to `Origin::Integrated` and a parameter to `NetClient::open_singleplayer`/
`open_to_lan`, threaded from `session.rs`'s `begin_singleplayer`. Left open
because `net.rs` had a live, unrelated concurrent edit in flight the session
this landed (`net::NetUpdate::LanPublishError`), and a new field on a shared
struct under concurrent edit is exactly the collision shape `CLAUDE.md`
warns about. The other four presets are additionally blocked on
`crates/lodestone-server/src/lib.rs` re-exporting their entry points, which
is a change to a crate outside this screen's ownership. Both are tracked in
the Create New World option-port follow-up issue in the tracker.

## Hover outline (issue #567)

Every button row on this screen used to draw with no hover outline at all,
ever, regardless of where the mouse was — reported as "the third instance"
of the frame-built-without-the-shared-canvas-stamp bug (in-world Settings,
then this issue's own opening report). **It was not a third instance of that
bug.** `Screen::CreateWorld`'s arm in `render::frame_for` returns `Some`
unconditionally (no `settings_in_world()`-style deferral to a second draw
path), so it already reaches `render::stamp_canvas_facts` through
`frame_for`'s own unconditional `frame.map(...)` — `cursor`/`gui_scale`/
`panorama_speed`/`list` were never the gap.

The real cause: `CreateWorldNav` had no field to record which row the mouse
was over, and `MenuNav::hover`'s match in `menu/nav.rs` had no
`Screen::CreateWorld` arm, so `frame`'s `MenuFrame::hovered` stayed `None`
unconditionally and `render::draw_widget`'s `widget.hovered` was `false`
every frame. Fixed the same shape `menu/nav.rs`'s `EditForm::hover_row`
already uses (a field row does nothing, a button row records `Self::
hovered`) — see `CreateWorldNav::hover_row`/`hovered` — plus the one-line
`menu/nav.rs` match arm `Screen::CreateWorld => self.create_world.hover_row(
row)`, in `MenuNav::hover`. `hover_row`/`hovered` carry a **focus id**, not a
frame-row index — `frame` converts through `frame_row_for_focus_id` when
building `MenuFrame::hovered`, since the row a given field is *drawn at*
depends on which tab is active. A tab-bar row itself needs no such
bookkeeping: its hover is derived straight from `MenuFrame::cursor` and its
own rect at draw time (`render/draw.rs`'s `draw_tab`), which is also why
Statistics's own tab bar highlighted correctly with no `Screen::Statistics`
arm in `MenuNav::hover` at all — only its Done button needed one, a second,
separate bug found auditing the sibling consumer of this same widget (see
[Statistics screen](./statistics-screen.md#dones-own-hover-outline-issue-567s-audit)).

**The mechanical check** the issue asked for lives in `menu/render/tests.rs`'s
`owns_frame_agrees_with_frame_for_on_every_screen`: every screen `frame_for`
returns `Some` for now also asserts its frame's `cursor`/`gui_scale`/
`panorama_speed`/`list` equal `stamp_canvas_facts`'s own inputs, with a real
(non-`None`) cursor position set first so the comparison cannot pass
vacuously. It is a tripwire for the *architectural* shape (a screen arm that
stops going through `frame_for`'s stamp, or sets a conflicting value of its
own) — not a substitute for the existing `frame_for_defers_to_an_overlay_
for_in_world_settings` test, which is what covers the one screen that
deliberately reaches the draw through a *second* path this loop never
visits.

## Wired vs. decorative

- **Wired**: reaching the screen (the button is live) and back (Escape/
  Cancel → `Screen::WorldSelect`), typing into Name/Seed (real `EditBox`es,
  the same primitive `world_select.rs`'s own search field and
  `menu/nav.rs`'s `EditForm` already use), cycling Game Mode/Difficulty/World
  Type and toggling Structures/Bonus Chest/Allow Cheats (real, in-memory
  config state), the Hardcore→Hard difficulty lock (`GameTab.java`'s own
  rule: selecting Hardcore forces and disables the difficulty cycle), and
  switching between Game/World/More by clicking the tab bar.
- **Wired since — the collected config's effect on the launched seed.** This
  section used to say "nothing downstream reads any field of it yet" and
  pointed at a queued patch; that patch has landed (`72cb451`, `d65d593`).
  `nav.rs`'s `apply_create_world` **creates the world directory** (issue #468's
  reading 2 — it is the layer that knows where `saves/` is) and turns
  `CreateWorldOutcome::Create` into
  `MenuAction::Singleplayer(SingleplayerLaunch::Created { world_dir, config })`,
  and `app.rs`'s
  `begin_singleplayer`/`resolve_launch_seed`/`parse_seed` resolve
  `config.seed` (vanilla's own `WorldOptions.parseSeed`/`randomSeed` rule —
  trim, a valid `i64` literal verbatim, free text hashed with Java's
  `String.hashCode`, empty means fresh random) into the `i64`
  `lodestone_server::worldgen_data::overworld_chunk_source(seed)` wants,
  replacing `world_select::BUNDLED_WORLD`'s hardcoded seed. The typed seed is
  honoured because the directory is **new** and therefore has no
  `world_gen_settings.dat` yet — which is precisely why creating a fresh directory
  is the right fix for #468's wart rather than forcing a seed onto an existing
  world. Proved end to end,
  not just at the `i64` level: `app.rs`'s
  `resolved_seeds_from_different_world_creation_configs_generate_different_terrain`
  resolves two typed seeds through the *production* path and asserts
  different real terrain at the same coordinate in the same column, plus
  byte-identical reproduction of the same seed.
- **Wired since — the world name.** This section used to say "decorative: there
  is still no `LevelStorageSource`, so a name is collected and shown but nothing
  is ever written to a folder of that name". There is one now
  (`crate::saves`, issue #468's reading 2): the typed name is sanitised into a
  folder name by `FileUtil.sanitizeName`'s own rules, de-duplicated with a
  ` (N)` counter, and written into the new world's `level.dat` as `LevelName` —
  so the world-select list shows what the player typed even when the folder had
  to be sanitised into something else. `saves.rs` is the spec.
- **Wired since — the game mode, partly.** It reaches `level.dat`'s `GameType`,
  so a world created as Creative is *listed* as Creative. **Hardcore is written
  as Survival**, because `LevelDat::for_new_world` writes `hardcore: 0` and this
  layer has no business hand-editing that compound; that is the same gap as the
  four fields below, one field narrower.
- **Decorative — world type.** Cycles all seven presets for real; three have
  a reachable generator and are only missing the `net.rs` threading hop
  (above), four more are additionally blocked on a `lodestone-server`
  re-export.
- **Decorative — difficulty, structures, bonus chest and allow-cheats.**
  Collected and cycled/toggled for real, but nothing downstream reads any of
  them — see "What is still queued" below.

## What is still queued

The seed and the name reach disk, and the game mode partly does (see above).
Difficulty, structures, bonus chest, allow-cheats and the hardcore flag need
deeper session-setup wiring (an ECS/server-side initial state, not just a
menu-side constant) than the seed's one-parameter threading, and are left as
documented follow-up — along with world type's own `net.rs` hop and the
four presets blocked on a `lodestone-server` re-export (both above).

## How to change it

- **Adding a field**: extend `WorldCreationConfig`, add a row constant, add
  it to `content_rows_for_tab` for whichever tab it belongs on, add a slot in
  `row_slot`, wire it into `CreateWorldWidgets`/`FocusChildren`/`activate`/
  `sync_tab_visibility`/`refresh_labels`. See `create_world.rs`'s own module
  doc for the full checklist and the "two index spaces" trap.
- **Row geometry** is asserted on-canvas at `MIN_SCALED_HEIGHT`, per tab, and
  checked not to overlap the footer (`create_world.rs`'s own tests) rather
  than hand-derived from a vanilla screenshot, since the *within-tab*
  geometry is not vanilla's to begin with (only the tab bar itself is).
- **Adding a tab-bar visual check**: point-sample with `colour_at`/
  `coverage_of` in `render/tests.rs`, not a vertex-in-rect probe — a tab
  bar's merge fill and flanking separators are exactly the kind of
  wide/enclosing paint a `band_coverage`-style probe reads as zero coverage.
  See the two gates named in [Statistics's own tab-widget
  section](./statistics-screen.md#the-tab-widget-issue-564).

## Configuration

None — this screen has no persisted state of its own (a created "world" is
never written to disk except the fields named "Wired" above).

## Dependencies

- `menu/edit_box.rs` — `EditBox`, the same primitive `world_select.rs`'s
  search field and `menu/nav.rs`'s `EditForm` already use.
- `menu/focus.rs` — `FocusSet`/`FocusChildren`/`FocusTarget`, the same
  mechanism `world_select.rs`'s `WorldSelectWidgets` already uses.
- `menu/options.rs` — `Placement::Footer`, `SMALL_BUTTON_WIDTH`, `WIDGET_H`
  — reused for this screen's two-button footer.
- `menu/layout.rs` — `TAB_BAR_HEIGHT`, `tab_bar_geometry`, `tab_bar_row_rect`
  — the [shared tab widget](./statistics-screen.md#the-tab-widget-issue-564).
- `menu/widget.rs` — `TAB_SPRITES`, `tab_underline_colour`, `tab_label_dy`.
- `lodestone_model::common::Difficulty` — reused directly rather than a
  local copy, since every vanilla difficulty is a legitimate creation-time
  choice.
- `docs/worldgen-world-type-selection.md` — the world-type entry-point
  table `WorldTypePreset::is_backend_wired` and this doc's own table above
  are both derived from.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`selectWorld.*`, `options.difficulty*`, `createWorld.tab.*`,
  `generator.minecraft.*`).

## See also

- [World select](./main-menu.md) — `Screen::WorldSelect`, this screen's
  entry point and its "Back"/"Escape" destination.
- [Statistics screen](./statistics-screen.md) — the tab widget's other
  consumer, and the sibling screen whose "reduce vanilla's structure, keep
  the real fields" call this one still makes for the fields within a tab.
- [World preset generator selection](./worldgen-world-type-selection.md) —
  the backend half of the World Type row.
