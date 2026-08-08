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
- **Decorative — difficulty, structures, bonus chest and allow-cheats.**
  Collected and cycled/toggled for real, but nothing downstream reads any of
  them — see "What is still queued" below.

## What is still queued

The seed and the name reach disk, and the game mode partly does (see above).
Difficulty, structures, bonus chest, allow-cheats and the hardcore flag need
deeper session-setup wiring (an ECS/server-side initial state, not just a
menu-side constant) than the seed's one-parameter threading, and are left as
documented follow-up.

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
