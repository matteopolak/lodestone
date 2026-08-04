# World Creation screen

## What it is

`Screen::CreateWorld` (issue #190): vanilla's `CreateWorldScreen`, reached
from the world list's "Create New World" button
(`crates/lodestone-shell/src/menu/world_select.rs`'s
`WorldSelectButton::Create`, now live — issue #397 left it
present-and-disabled deliberately for this issue). Collects a world name,
seed, game mode, difficulty, and three toggles (generate structures, bonus
chest, allow cheats).

## How it works

- `menu/create_world.rs` — the whole model: `WorldCreationConfig` (the
  collected fields), `WorldGameMode`/`WorldDifficulty` (the two cycle
  values), `CreateWorldWidgets` (two real `EditBox`es — Name, Seed — plus
  five `Widget` rows for the cycles/toggles/footer, one `FocusChildren`
  struct mirroring `menu/nav.rs`'s `FormFields` and `world_select.rs`'s
  `WorldSelectWidgets`), `CreateWorldNav` (the live state: widgets, focus,
  config), and `frame` (the whole screen).
- `menu.rs` — the `Screen::CreateWorld` variant, `UiState::open_create_world`/
  `close_create_world`.
- `menu/nav.rs` — `MenuNav::create_world`, `key_create_world`/
  `apply_create_world`, and `WorldSelectOutcome::CreateWorld` threaded
  through `apply_world_select` (which is now a method rather than an
  associated function, so it can reset `create_world` on entry).
- `menu/world_select.rs` — `WorldSelectButton::Create.enabled()` flipped to
  `true`, `WorldSelectOutcome::CreateWorld` added, `press` routes to it.

### Not vanilla geometry, on purpose

`CreateWorldScreen` is 828 lines with three `GridLayoutTab`s (Game/World/
More) inside a `MenuTabBar`, and `WorldCreationUiState` (326 lines) tracks a
world-type preset list, data packs, game rules and a temp save folder on
disk. None of that fits this pipeline or this client: there is still no
`LevelStorageSource`, no data-pack loader, and no game-rule model (see
`world_select.rs`'s own module docs, unchanged by this issue). Building the
tab/preset machinery to hold a handful of fields that do get real support
would be geometry in service of nothing, so this screen is **one flat
hand-placed list** instead of vanilla's tabs — the same move
`menu/key_binds.rs` and `menu/social.rs` already made for their own
non-`OptionsList` screens, extended one step further (skipping a
sub-structure rather than a widget shape). `docs/ui-framework.md` already
names hand-arithmetic layout as legitimate vanilla (`TitleScreen` itself uses
no layout class); this is the same call at a coarser grain.

## Wired vs. decorative

- **Wired**: reaching the screen (the button is live) and back (Escape/
  Cancel → `Screen::WorldSelect`), typing into Name/Seed (real `EditBox`es,
  the same primitive `world_select.rs`'s own search field and
  `menu/nav.rs`'s `EditForm` already use), cycling Game Mode/Difficulty and
  toggling Structures/Bonus Chest/Allow Cheats (real, in-memory config
  state), and the Hardcore→Hard difficulty lock (`GameTab.java`'s own rule:
  selecting Hardcore forces and disables the difficulty cycle).
- **Decorative — the collected config's effect on the launched world.**
  Pressing Create returns a real `WorldCreationConfig`, but **nothing
  downstream reads any field of it yet**. `app.rs`'s `launch_singleplayer`
  hardcodes `world_select::BUNDLED_WORLD`'s seed regardless of what this
  screen collected. See "The queued patch" below.
- **Decorative — the world name and the "will be saved in" folder.** There
  is still no `LevelStorageSource`, so a name is collected and shown but
  nothing is ever written to a folder of that name.

## The queued patch

The one line most worth doing first: `lodestone_server::worldgen_data::
overworld_chunk_source(seed: i64)` already takes a seed as a plain parameter
(verified — it is not buried behind anything singleplayer-specific), so
threading `WorldCreationConfig::seed` through to `launch_singleplayer` is a
real, scoped change to `app.rs` (brokered for this batch):

1. `MenuAction::Singleplayer` (currently a bare variant) needs to carry the
   config the player collected, or `app.rs` needs a new variant/field for
   "create and play" distinct from "play the bundled world" (`nav.rs`'s
   `apply_create_world` is where the Create button's outcome is decided
   today — it returns `MenuAction::None`, matching the fact that nothing
   consumes the config yet).
2. `launch_singleplayer` parses `config.seed` (empty → random, non-empty →
   vanilla's own seed-hashing rule, `WorldOptions.java`'s `parseSeed`, not
   yet implemented anywhere in this menu layer) into the `i64`
   `overworld_chunk_source` wants, instead of reading `BUNDLED_WORLD.seed`.

Game mode, difficulty, structures, bonus chest and allow-cheats need deeper
session-setup wiring (an ECS/ server-side initial state, not just a menu-side
constant) than this one patch can specify and are left as documented
follow-up.

## How to change it

- **Adding a field**: extend `WorldCreationConfig`, add a row constant and a
  slot in `row_slot`, wire it into `CreateWorldWidgets`/`FocusChildren`/
  `activate`. The 9-row layout is intentionally flat — see "Not vanilla
  geometry" above before reaching for a tab.
- **Row geometry** is asserted on-canvas at `MIN_SCALED_HEIGHT` and checked
  not to overlap the footer (`create_world.rs`'s own tests) rather than
  hand-derived from a vanilla screenshot, since this geometry is not
  vanilla's to begin with.

## Configuration

None — this screen has no persisted state of its own (a created "world" is
never written to disk, per the module docs above).

## Dependencies

- `menu/edit_box.rs` — `EditBox`, the same primitive `world_select.rs`'s
  search field and `menu/nav.rs`'s `EditForm` already use.
- `menu/focus.rs` — `FocusSet`/`FocusChildren`/`FocusTarget`, the same
  mechanism `world_select.rs`'s `WorldSelectWidgets` already uses.
- `menu/options.rs` — `Placement::Footer`, `SMALL_BUTTON_WIDTH`, `WIDGET_H`
  — reused for this screen's two-button footer.
- `lodestone_model::common::Difficulty` — reused directly rather than a
  local copy, since every vanilla difficulty is a legitimate creation-time
  choice.
- The 26.2 jar's `assets/minecraft/lang/en_us.json` for every caption
  verbatim (`selectWorld.*`, `options.difficulty*`).

## See also

- [World select](./main-menu.md) — `Screen::WorldSelect`, this screen's
  entry point and its "Back"/"Escape" destination.
- [Social Interactions screen](./social-interactions.md),
  [Statistics screen](./statistics-screen.md) — the sibling screens whose
  "reduce vanilla's structure, keep the real fields" call this one repeats.
