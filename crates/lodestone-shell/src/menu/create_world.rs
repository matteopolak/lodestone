//! The World Creation screen — vanilla's own world-creation screen,
//! reached from [`super::world_select`]'s "Create New World" button, which
//! was deliberately left present-and-disabled until this screen was built.
//!
//! ## Tabs
//!
//! This screen used to be one flat hand-placed list, with a module doc arguing
//! at length that vanilla's three grid-layout tabs (Game/World/More) were not
//! worth building to hold a handful of fields that get real support. The
//! owner disagreed: the UI needed to match real vanilla's tabbed layout —
//! and by then the tab widget itself had already landed for Statistics
//! (`widget.rs`'s
//! `TAB_SPRITES`/`tab_underline_colour`/`tab_label_dy`, `layout.rs`'s
//! `tab_bar_geometry`/`tab_bar_row_rect`, `render/frame.rs`'s `MenuRow::tab` +
//! [`super::render::TabEntryView`], `render/draw.rs`'s `draw_tab`). This screen
//! is that widget's **second** consumer: one widget, two screens, rather than
//! two bespoke tab strips that could drift apart.
//!
//! Vanilla's three tabs, and where each field landed:
//!
//! - **Game** (`createWorld.tab.game.title`): [`NAME_FIELD`], [`GAME_MODE_ROW`],
//!   [`DIFFICULTY_ROW`], [`ALLOW_CHEATS_ROW`] — vanilla's own Game tab also has an
//!   Experiments button here, but only on an unstable version — absent on a
//!   stable release, which is what this client models, so there is nothing missing
//!   here even though [`EXPERIMENTS_ROW`] now exists (it lives on More,
//!   below, matching vanilla's own always-present copy of the button).
//! - **World** (`createWorld.tab.world.title`): [`SEED_FIELD`],
//!   [`WORLD_TYPE_ROW`], [`STRUCTURES_ROW`], [`BONUS_CHEST_ROW`] — vanilla's
//!   own World tab also has a "Customize Type" button this client has no
//!   preset-editor screen for, left absent the same way. [`WORLD_TYPE_ROW`]
//!   itself is real — cycles all seven bundled
//!   presets and collects the choice — and
//!   selecting `Normal`/`LargeBiomes`/`Amplified` reaches the served world;
//!   the other four remain decorative. See [`WorldTypePreset`]'s own doc for
//!   exactly which is which and why.
//! - **More** (`createWorld.tab.more.title`): vanilla's own More tab is three
//!   buttons, in this order — Game Rules, Experiments, Data Packs. All three
//!   now have real models
//!   ([`GAME_RULES_ROW`]/[`GameRulesEditor`] and
//!   [`DATA_PACKS_ROW`]/[`DataPacksEditor`], and
//!   [`EXPERIMENTS_ROW`]/[`ExperimentsEditor`]).
//!   [`ExperimentsEditor`]'s collected choice reaches disk now too:
//!   [`WorldCreationConfig::experiments`] is written into the freshly created
//!   world's `level.dat` as vanilla's own `enabled_features` field
//!   (`crate::saves::create_world_in`, through
//!   [`lodestone_anvil::level_dat::LevelDat::with_enabled_features`]) — no
//!   `lodestone-server` hook needed, since the shell writes `level.dat`
//!   itself before the server ever opens the directory (see that function's
//!   own doc). What is still unbuilt is the *engine* half: nothing reads
//!   `enabled_features` back to gate the trade-rebalance/redstone/minecart
//!   behaviours themselves, matching vanilla's own scope for a stable-channel
//!   client (the Game tab's copy of this button, per its own note above, is
//!   simply absent — there is no in-game surface for those behaviours to
//!   reach yet either). The tab itself is real regardless: selectable, its
//!   own real [`TabEntryView`](super::render::TabEntryView), never disabled
//!   for having an unbuilt feature under it — unlike Statistics's Items/Mobs,
//!   which vanilla disables **because the underlying list is empty**. Nothing here is
//!   data-driven-empty; disabling the tab for it would misrepresent that as
//!   vanilla's own behaviour.
//! - [`ONLINE_MODE_ROW`] has no vanilla tab at all — see its own doc on why it
//!   exists — and is placed on **World**, after Bonus Chest: it is a
//!   network-exposure setting for the world being created, which is closer in
//!   kind to World's own "how does this world generate/behave" fields than to
//!   Game's account-permission fields.
//!
//! **Not ported: per-tab keyboard focus order.** Vanilla's own tab-bar widget is
//! itself focusable, in tab-order group 0 ahead of the content, so a keyboard
//! player can Tab onto the bar and use Left/Right to switch tabs — the same
//! divergence `stats.rs`'s own focus test already documents for Statistics's
//! bar. This screen's tab bar is fully **clickable** (all three tabs switch
//! content; Statistics only has one live tab to click) but not yet reachable
//! by Tab — [`CreateWorldNav::click_row`] is the only way to switch. A
//! keyboard player can still reach and use everything on the tab that is
//! showing; only the bar itself is mouse-only, matching the scope cut
//! `stats.rs` already made for the same widget.
//!
//! ## What is and is not vanilla geometry
//!
//! `WorldCreationUiState` (326 lines) tracks a world-type preset list, data
//! packs, game rules and a temp save folder on disk that this client has no
//! model for at all — see the per-tab breakdown above for exactly which
//! fields that leaves out. The fields that *do* get real menu-side support
//! (name, seed, game mode, difficulty, structures, bonus chest, cheats) are
//! hand-placed within each tab's own flat column — the same legitimate move
//! [`super::key_binds`] and [`super::social`] already make for their own
//! non-`OptionsList` screens, extended to *within-tab* layout rather than to
//! widget shape.
//!
//! ## Wired vs. decorative
//!
//! - **Wired**: reaching the screen (the "Create New World" button is now
//!   live) and back (Escape/Cancel → [`super::Screen::WorldSelect`]), typing
//!   into the Name/Seed fields (real [`EditBox`]es, the same primitive
//!   [`super::world_select`]'s search field and [`super::nav::EditForm`]
//!   already use), cycling Game Mode/Difficulty and toggling Structures/
//!   Bonus Chest/Allow Cheats (real, in-memory [`WorldCreationConfig`]
//!   state), the Hardcore→Hard difficulty lock (vanilla's own Game-tab
//!   rule: selecting Hardcore forces and disables the difficulty cycle), and
//!   switching between Game/World/More by clicking the tab bar.
//! - **Wired since — the seed.** This section used to say "nothing
//!   downstream reads any field of it yet"; that queued patch landed
//!   (`72cb451`, `d65d593`). `apply_create_world` turns
//!   [`CreateWorldOutcome::Create`] into `MenuAction::Singleplayer(Some(config))`,
//!   and `app.rs`'s `begin_singleplayer` resolves `config.seed` through
//!   `resolve_launch_seed`/`parse_seed` — vanilla's own
//!   seed-parsing rule (trim, a valid `i64` literal
//!   used verbatim, free text hashed with a string hash, empty
//!   means fresh random) — into the `i64`
//!   `lodestone_server::worldgen_data::overworld_chunk_source(seed)` wants,
//!   in place of `BUNDLED_WORLD.seed`.
//! - **Wired since — Online Mode.** Not vanilla (no vanilla screen
//!   control ties online-mode to a per-world setting in the real game), and
//!   the one field on this struct that is not merely collected: `true` makes
//!   `begin_singleplayer` open the new world to LAN immediately, with the
//!   real RSA/AES handshake and session-server ownership check
//!   (`lodestone_server::OnlineModeConfig`) running on every connection —
//!   see [`WorldCreationConfig::online_mode`]'s own doc for what it does and
//!   does not cover.
//! - **Decorative — game mode, difficulty, structures, bonus chest and
//!   allow-cheats.** Collected in `WorldCreationConfig` and cycled/toggled for
//!   real, but nothing downstream reads any of them: they need deeper
//!   session-setup wiring (server-side initial state) than the seed's
//!   one-parameter threading, and are left as documented follow-up.
//! - **Wired — all seven world types.**
//!   Cycles all seven bundled presets for real (the world-generator half
//!   landed all seven; this is their UI), and choosing any of them now
//!   reaches the served world. `Normal`/`LargeBiomes`/`Amplified` go through
//!   `WorldTypePreset::backend_world_type`; `SingleBiomeSurface`/`Flat`/
//!   `FlatAllDimensions`/`DebugAllBlockStates` go through `net.rs`'s
//!   `preset_chunk_source`, once `crates/lodestone-server/src/lib.rs`
//!   re-exported their entry points. `begin_singleplayer` (`app/session.rs`) reads the
//!   chosen [`WorldTypePreset`] from `WorldCreationConfig` for a
//!   **`Created`** launch only (same rule as `seed`), threads the preset
//!   itself through `launch_singleplayer`/`launch_open_to_lan_online`
//!   (`app/launch.rs`) into `NetClient::open_singleplayer`/`open_to_lan`, and
//!   `net.rs`'s `Origin::Integrated` carries it to the one construction site
//!   that used to hardcode `overworld_chunk_source(seed)`. `SingleBiomeSurface`
//!   always uses the bundled default biome (`minecraft:plains`) rather than a
//!   player choice — no UI for that yet, a disclosed simplification, not a
//!   fallback to `Overworld`.
//! - **Wired — Game Rules.** [`GameRulesEditor`]'s per-rule diff
//!   (`WorldCreationConfig::game_rules`) reaches the server for real:
//!   `begin_singleplayer` sends it as
//!   [`lodestone_model::action::ClientAction::SetGameRules`] once the session
//!   reaches Play.
//! - **Decorative — Data Packs.** [`DataPacksEditor`]'s selection
//!   (`WorldCreationConfig::data_packs`) is collected for real — a genuine
//!   directory scan ([`crate::resources::data_packs_dir`]), a real
//!   selectable list — but nothing downstream reads it: this crate has no
//!   data-pack loader at all, unlike Game Rules, which only needed a
//!   `ClientAction` this client already had a producer and consumer for. See
//!   [`DataPacksEditor`]'s own module doc.
//! - **Decorative — the world name and the "will be saved in" folder.**
//!   There is still no `LevelStorageSource` (`world_select`'s own module
//!   docs, unchanged by this issue), so a name is collected and shown but
//!   nothing is ever written to a folder of that name.
//!
//! ## Two index spaces, and why there are two
//!
//! [`CreateWorldWidgets`]' focus ids (`NAME_FIELD == 0` through `CANCEL_ROW ==
//! 10`) are **stable** — [`super::focus::FocusSet`] and every method that takes
//! a "row" by that name (`click_focus` in the tests below, [`activate`]) means
//! one of these. [`CreateWorldNav::click_row`]/[`CreateWorldNav::hover_row`]
//! take a **different** number: the index into [`frame`]'s own `MenuFrame::
//! rows`, which is what `app.rs`'s `menu_row_at`/`render::menu_row_under` hit-
//! test against and what `nav.rs`'s `Screen::CreateWorld` arms forward
//! verbatim. The two coincided by construction before tabs existed (every
//! focus id had exactly one row, in order); now `rows` is three tab rows, then
//! whichever tab's own content rows, then the two footer rows — a length and
//! an offset that both depend on [`CreateWorldNav::active_tab`]. Confusing the
//! two is the exact island shape `CLAUDE.md` warns about: a row that resolves
//! to a real, focusable, testable widget by focus id and reaches no pixels (or
//! the *wrong* pixels) because the click routing was handed a focus id instead
//! of a frame row, or vice versa. [`CreateWorldNav::frame_row_for_focus_id`]/
//! [`CreateWorldNav::focus_id_for_frame_row`] are the one pair of functions
//! that convert between them; nothing else should restate the arithmetic.

use super::edit_box::EditBox;
use super::focus::{FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::layout;
use super::nav::MenuKey;
use super::options;
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot, TabEntryView};
use super::widget::Widget;
use lodestone_server::game_rules::{GAME_RULES, GameRuleValue};

// -- vanilla captions, verbatim from en_us.json --------------------------

/// `selectWorld.enterName`.
pub const NAME_LABEL: &str = "World Name";
/// `selectWorld.newWorld` — the default value, not a hint.
pub const DEFAULT_NAME: &str = "New World";
/// `selectWorld.enterSeed` — the seed field's own visible label, drawn above
/// it exactly like [`NAME_LABEL`], through vanilla's own labeled-element
/// layout helper.
pub const SEED_LABEL: &str = "Seed for the world generator";
/// `selectWorld.seedInfo` — the seed field's own hint ghost text,
/// shown only while the box is empty and unfocused.
/// Not a second permanent label; see
/// [`frame`]'s own doc on the notice this constant used to also feed.
pub const SEED_INFO: &str = "Leave blank for a random seed";
/// `selectWorld.gameMode` / `selectWorld.mapFeatures` / `options.difficulty`
/// / `selectWorld.bonusItems` / `selectWorld.allowCommands`.
pub const GAME_MODE_LABEL: &str = "Game Mode";
pub const DIFFICULTY_LABEL: &str = "Difficulty";
pub const STRUCTURES_LABEL: &str = "Generate Structures";
pub const BONUS_CHEST_LABEL: &str = "Bonus Chest";
pub const ALLOW_CHEATS_LABEL: &str = "Allow Cheats";
/// Not a vanilla caption — there is no vanilla `CreateWorldScreen` control for
/// this, because real Minecraft ties online-mode to the account you are
/// signed in with rather than to a per-world creation setting. See
/// [`WorldCreationConfig::online_mode`] for what this actually does.
pub const ONLINE_MODE_LABEL: &str = "Online Mode (Open to LAN)";
/// `selectWorld.create`, reused verbatim for this screen's own submit button
/// — vanilla uses the same string for both.
pub const CREATE_LABEL: &str = "Create New World";
pub const CANCEL_LABEL: &str = "Cancel";
/// `selectWorld.mapType` — vanilla's own label for the World Type cycle
/// button.
pub const WORLD_TYPE_LABEL: &str = "World Type";
/// `createWorld.customize.gameRules.title`-adjacent — vanilla's More tab
/// button that opens `WorldCreationGameRulesScreen`.
pub const GAME_RULES_BUTTON_LABEL: &str = "Game Rules...";
/// `dataPack.title`-adjacent — vanilla's More tab button that opens a
/// `PackSelectionScreen` scoped to data packs.
pub const DATA_PACKS_BUTTON_LABEL: &str = "Data Packs...";
/// `selectWorld.experiments`, verbatim from `en_us.json` — vanilla's More tab
/// button that opens `ExperimentsScreen`.
pub const EXPERIMENTS_BUTTON_LABEL: &str = "Experiments...";

/// `createWorld.tab.game.title`/`.world.title`/`.more.title`, verbatim from
/// `en_us.json` — this screen's own tab bar, built from the same
/// shared widget [`super::stats::TAB_LABELS`] uses.
pub const TAB_LABELS: [&str; 3] = ["Game", "World", "More"];
pub const GAME_TAB: usize = 0;
pub const WORLD_TAB: usize = 1;
pub const MORE_TAB: usize = 2;

/// The pixel rect of tab `index` (into [`TAB_LABELS`]) at canvas `width` —
/// resolves through the same shared [`layout::tab_bar_row_rect`]
/// `stats::tab_row_rect` does, so the two screens' tab bars cannot drift
/// apart on geometry. See [`super::render::TabEntryView::index`]'s own doc on
/// why a `Slot` cannot express this row's *width*, let alone its `x`.
#[must_use]
pub fn tab_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    layout::tab_bar_row_rect(index, TAB_LABELS.len(), width)
}

/// The fixed focus ids that live on tab `tab`, in the order they appear top to
/// bottom within it. [`MORE_TAB`] has none — see the module docs on why that
/// is honest rather than a shortcut. The one definition [`frame`]'s row
/// construction, [`CreateWorldNav::switch_tab`]'s initial-focus pick, and the
/// frame-row/focus-id conversion below all read from, so a field moved to a
/// different tab only has to move here.
#[must_use]
fn content_rows_for_tab(tab: usize) -> &'static [usize] {
    match tab {
        GAME_TAB => &[NAME_FIELD, GAME_MODE_ROW, DIFFICULTY_ROW, ALLOW_CHEATS_ROW],
        WORLD_TAB => &[
            SEED_FIELD,
            WORLD_TYPE_ROW,
            STRUCTURES_ROW,
            BONUS_CHEST_ROW,
            ONLINE_MODE_ROW,
        ],
        // Vanilla's own More-tab button order:
        // Game Rules, Experiments, **then** Data Packs.
        MORE_TAB => &[GAME_RULES_ROW, EXPERIMENTS_ROW, DATA_PACKS_ROW],
        _ => &[],
    }
}

/// Vanilla's own world-type entry/preset list, narrowed to the seven
/// bundled `world_preset/*.json` documents (generator half) —
/// vanilla's own preset list has a customizable "Buffet"/`FLAT`-family branch
/// this client does not model, so this enum is the seven fixed presets rather
/// than an open list.
///
/// ## Backend wiring — all seven, now
///
/// [`Self::caption`] is real for all seven (`generator.minecraft.*`,
/// verbatim), and **selecting any of the seven now reaches a real, distinct
/// generator** — see `docs/worldgen-world-type-selection.md`'s own "How to
/// change it" table for exactly which entry point each preset needs.
///
/// - [`Self::Normal`]/[`Self::LargeBiomes`]/[`Self::Amplified`] go through
///   [`Self::backend_world_type`] and `overworld_chunk_source_of_type` — see
///   that method's own doc.
/// - [`Self::SingleBiomeSurface`]/[`Self::Flat`]/[`Self::FlatAllDimensions`]/
///   [`Self::DebugAllBlockStates`] go through `net.rs`'s
///   `preset_chunk_source`, which calls `single_biome_chunk_source`/
///   `flat_chunk_source`/`debug_chunk_source` directly, once
///   `crates/lodestone-server/src/lib.rs` re-exported those
///   entry points. `SingleBiomeSurface` always uses
///   `world_preset_single_biome_default_biome()` (`minecraft:plains`) rather
///   than a player-chosen biome — there is no UI for that choice yet, and
///   vanilla's own World tab does not expose it directly either.
///
/// In every case, `begin_singleplayer` (`app/session.rs`) reads the chosen
/// [`WorldTypePreset`] from `WorldCreationConfig` for a **`Created`** launch
/// and threads the preset itself (not just a `lodestone_server::WorldType`
/// projection of it) through `launch_singleplayer`/`launch_open_to_lan_online`
/// (`app/launch.rs`) into `NetClient::open_singleplayer`/`open_to_lan`, and
/// `net.rs`'s `Origin::Integrated` carries it to `preset_chunk_source`, which
/// used to hardcode `overworld_chunk_source(seed)` (i.e. `Self::Normal`)
/// unconditionally. The `Normal`/`LargeBiomes`/`Amplified` leg is verified end
/// to end by `tests/singleplayer_terrain_arrives.rs`'s
/// `a_singleplayer_world_honours_the_selected_world_type_end_to_end`, which
/// reuses `lodestone-server/tests/world_type_selection.rs`'s own measured
/// 64/130 top-of-world heights at seed 4242 rather than re-deriving them —
/// a real `NetClient::open_singleplayer` session selecting `Amplified` must
/// serve the 130 figure over the wire, not the 64 the Overworld default
/// would give the identical column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorldTypePreset {
    #[default]
    Normal,
    LargeBiomes,
    Amplified,
    SingleBiomeSurface,
    Flat,
    FlatAllDimensions,
    DebugAllBlockStates,
}

impl WorldTypePreset {
    /// `generator.minecraft.<id>`, verbatim from `en_us.json`.
    #[must_use]
    pub fn caption(self) -> &'static str {
        match self {
            WorldTypePreset::Normal => "Default",
            WorldTypePreset::LargeBiomes => "Large Biomes",
            WorldTypePreset::Amplified => "AMPLIFIED",
            WorldTypePreset::SingleBiomeSurface => "Single Biome",
            WorldTypePreset::Flat => "Superflat",
            WorldTypePreset::FlatAllDimensions => "Flat All Dimensions",
            WorldTypePreset::DebugAllBlockStates => "Debug Mode",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            WorldTypePreset::Normal => WorldTypePreset::LargeBiomes,
            WorldTypePreset::LargeBiomes => WorldTypePreset::Amplified,
            WorldTypePreset::Amplified => WorldTypePreset::SingleBiomeSurface,
            WorldTypePreset::SingleBiomeSurface => WorldTypePreset::Flat,
            WorldTypePreset::Flat => WorldTypePreset::FlatAllDimensions,
            WorldTypePreset::FlatAllDimensions => WorldTypePreset::DebugAllBlockStates,
            WorldTypePreset::DebugAllBlockStates => WorldTypePreset::Normal,
        }
    }

    /// Whether the launch path builds this preset's own generator rather than
    /// silently falling back to [`Self::Normal`] — see this type's own doc.
    ///
    /// `true` for all seven, now that `net.rs`'s `preset_chunk_source` covers
    /// every variant (items 1 and 2). Kept as a named predicate
    /// rather than deleted: a caller that wants to warn about a decorative
    /// choice should still ask this rather than assume, so a preset added
    /// later without its own `preset_chunk_source` arm has somewhere to
    /// report `false` from instead of silently falling through to `Normal`.
    #[must_use]
    pub fn is_backend_wired(self) -> bool {
        true
    }

    /// The `lodestone_server::WorldType` this preset resolves to, for the
    /// three presets shaped as one — `net.rs`'s `preset_chunk_source` is the
    /// full seven-way mapping now; this is the narrower
    /// three-way piece it delegates to for
    /// [`Self::Normal`]/[`Self::LargeBiomes`]/[`Self::Amplified`].
    /// [`Self::SingleBiomeSurface`]/[`Self::Flat`]/[`Self::FlatAllDimensions`]/
    /// [`Self::DebugAllBlockStates`] have no `lodestone_server::WorldType` of
    /// their own — they go through `single_biome_chunk_source`/
    /// `flat_chunk_source`/`debug_chunk_source` directly instead, so this
    /// falls back to [`lodestone_server::WorldType::Overworld`] for them; a
    /// caller building a chunk source from a full [`WorldTypePreset`] should
    /// call `net.rs`'s `preset_chunk_source`, not this method.
    #[must_use]
    pub fn backend_world_type(self) -> lodestone_server::WorldType {
        match self {
            WorldTypePreset::LargeBiomes => lodestone_server::WorldType::LargeBiomes,
            WorldTypePreset::Amplified => lodestone_server::WorldType::Amplified,
            WorldTypePreset::Normal
            | WorldTypePreset::SingleBiomeSurface
            | WorldTypePreset::Flat
            | WorldTypePreset::FlatAllDimensions
            | WorldTypePreset::DebugAllBlockStates => lodestone_server::WorldType::Overworld,
        }
    }
}

/// Vanilla's own selected-game-mode set, narrowed to the three a player
/// actually picks from this button (vanilla's own Game-tab cycle — `DEBUG`,
/// vanilla's fourth value, is not offered here; its own caption for it is
/// literally "spectator", which is not a serious
/// creation-time choice and this button does not cycle to it, matching the
/// real client).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorldGameMode {
    #[default]
    Survival,
    Creative,
    Hardcore,
}

impl WorldGameMode {
    #[must_use]
    pub fn caption(self) -> &'static str {
        match self {
            WorldGameMode::Survival => "Survival",
            WorldGameMode::Creative => "Creative",
            WorldGameMode::Hardcore => "Hardcore",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            WorldGameMode::Survival => WorldGameMode::Creative,
            WorldGameMode::Creative => WorldGameMode::Hardcore,
            WorldGameMode::Hardcore => WorldGameMode::Survival,
        }
    }
}

/// Reuses [`lodestone_model::common::Difficulty`] directly rather than a
/// narrowed local copy (unlike [`WorldGameMode`]) — every vanilla difficulty
/// is a legitimate creation-time choice, so there is nothing to narrow.
pub use lodestone_model::common::Difficulty as WorldDifficulty;

#[must_use]
fn difficulty_caption(d: WorldDifficulty) -> &'static str {
    match d {
        WorldDifficulty::Peaceful => "Peaceful",
        WorldDifficulty::Easy => "Easy",
        WorldDifficulty::Normal => "Normal",
        WorldDifficulty::Hard => "Hard",
    }
}

#[must_use]
fn next_difficulty(d: WorldDifficulty) -> WorldDifficulty {
    match d {
        WorldDifficulty::Peaceful => WorldDifficulty::Easy,
        WorldDifficulty::Easy => WorldDifficulty::Normal,
        WorldDifficulty::Normal => WorldDifficulty::Hard,
        WorldDifficulty::Hard => WorldDifficulty::Peaceful,
    }
}

/// The fields this screen collects — [`CreateWorldOutcome::Create`]'s
/// payload. Not yet consumed by anything downstream; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldCreationConfig {
    pub name: String,
    /// The typed seed text, verbatim — empty means "random", matching
    /// vanilla's own random-seed default branch
    /// (`selectWorld.seedInfo`). Parsing an empty/non-numeric seed into an
    /// actual `i64` is the consuming patch's job (see the module docs), not
    /// this screen's: vanilla itself accepts non-numeric seed text and
    /// hashes it with its own seed-parsing routine, which this menu
    /// layer has no reason to reimplement ahead of a consumer that needs it.
    pub seed: String,
    /// **Decorative for every value** — see [`WorldTypePreset`]'s own doc for
    /// exactly which three of the seven presets already have a real,
    /// reachable generator and which hop is still missing for them.
    pub world_type: WorldTypePreset,
    pub game_mode: WorldGameMode,
    pub difficulty: WorldDifficulty,
    pub generate_structures: bool,
    pub bonus_chest: bool,
    pub allow_cheats: bool,
    /// **Wired, not decorative** (shell-side control) — unlike
    /// every other field on this struct: `true` makes `app.rs`'s
    /// `begin_singleplayer` open this world to LAN on an OS-assigned port
    /// *immediately*, with the real RSA/AES handshake and session-server
    /// ownership check running on every connection it accepts (including the
    /// host's own loopback join — see `net.rs`'s `open_lan_world` match arm on
    /// `(None, Some(address))`), instead of the ordinary in-memory
    /// singleplayer session `open_singleplayer` starts for every other world.
    ///
    /// `false` — the default — changes nothing: the world opens exactly as it
    /// always has, with no listener and no network call.
    ///
    /// Only reachable from **Create New World**, not **Play Selected World**:
    /// `SingleplayerLaunch::Open` carries no `WorldCreationConfig` to hold
    /// this on. A world created without it can still be published later
    /// through the pause menu's existing Open to LAN button, just not with
    /// online mode — that path calls `IntegratedServer::publish`, which has
    /// no online-mode parameter.
    pub online_mode: bool,
    /// Game rules that differ from their vanilla default (More
    /// tab), collected from [`CreateWorldNav`]'s own [`GameRulesEditor`] when
    /// Create is pressed. Sent to the freshly-created singleplayer server as
    /// [`lodestone_model::action::ClientAction::SetGameRules`] once the
    /// session reaches Play — see `app/session.rs`'s `begin_singleplayer`.
    /// Empty for a world whose rules were never touched, which sends nothing
    /// rather than a no-op packet.
    pub game_rules: Vec<(lodestone_model::ResourceKey, String)>,
    /// Extra data packs selected beyond the always-active Vanilla one (the
    /// More tab), as the ids [`DataPacksEditor::selected_ids`] reports
    /// — collected from [`CreateWorldNav`]'s own [`DataPacksEditor`] when
    /// Create is pressed. **Fully decorative**: this crate has no data-pack
    /// loader at all (no recipe/loot-table/tag override machinery to apply
    /// one), so nothing downstream reads this yet — see
    /// [`DataPacksEditor`]'s own module doc. Kept anyway, the same
    /// disclosed-but-collected shape `game_mode`/`difficulty`/
    /// `generate_structures`/`bonus_chest`/`allow_cheats` already use on this
    /// struct, so a future loader has somewhere real to read the player's
    /// choice from rather than needing this screen touched again. Empty for a
    /// world that never had any extra pack selected, same "send nothing
    /// rather than a no-op" rule [`Self::game_rules`] uses.
    pub data_packs: Vec<String>,
    /// Feature flags the player turned on (Experiments half), as
    /// [`ExperimentFlag::id`] strings — collected from [`CreateWorldNav`]'s
    /// own [`ExperimentsEditor`] when Create is pressed, the exact
    /// `Vec<String>` shape [`Self::data_packs`] takes. Unlike that field,
    /// this one reaches disk: vanilla's own
    /// enabled-features field is written into a freshly
    /// created world's `level.dat` at creation time — it is never a network
    /// packet, unlike [`Self::game_rules`] — and `crate::saves::create_world_in`
    /// writes it there, through
    /// [`lodestone_anvil::level_dat::LevelDat::with_enabled_features`], from
    /// the same layer that already writes the world's name and game type. No
    /// `lodestone-server` hook is needed for *that* much: the shell owns
    /// `level.dat`'s creation, and the server only ever reads it back
    /// afterwards (`region_source::LevelDatHandle::open_or_create`'s
    /// existing-file branch), preserving whatever this crate wrote. Still
    /// unbuilt: nothing downstream reads `enabled_features` back to make the
    /// three flags actually change engine behaviour — that is real gameplay
    /// work, not a wiring gap. Empty for a world that never had any
    /// experiment turned on, matching vanilla's own default (empty) feature
    /// set, and writes nothing extra to `level.dat` in that case.
    pub experiments: Vec<String>,
}

impl Default for WorldCreationConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_string(),
            seed: String::new(),
            world_type: WorldTypePreset::default(),
            game_mode: WorldGameMode::default(),
            // Vanilla's own default difficulty.
            difficulty: WorldDifficulty::Normal,
            // Vanilla's own world-options defaults — generate-structures true,
            // generate-bonus-chest false.
            generate_structures: true,
            bonus_chest: false,
            allow_cheats: false,
            online_mode: false,
            game_rules: Vec::new(),
            data_packs: Vec::new(),
            experiments: Vec::new(),
        }
    }
}

// -- focus row ids -----------------------------------------------------------

pub const NAME_FIELD: usize = 0;
pub const SEED_FIELD: usize = 1;
pub const GAME_MODE_ROW: usize = 2;
pub const DIFFICULTY_ROW: usize = 3;
pub const STRUCTURES_ROW: usize = 4;
pub const BONUS_CHEST_ROW: usize = 5;
pub const ALLOW_CHEATS_ROW: usize = 6;
pub const ONLINE_MODE_ROW: usize = 7;
pub const WORLD_TYPE_ROW: usize = 8;
pub const CREATE_ROW: usize = 9;
pub const CANCEL_ROW: usize = 10;
/// More tab's first row (Game Rules half): opens the
/// scrollable rule editor — see [`CreateWorldMode::GameRules`].
pub const GAME_RULES_ROW: usize = 11;
/// More tab's second row (Data Packs half): opens the pack
/// selector — see [`CreateWorldMode::DataPacks`].
pub const DATA_PACKS_ROW: usize = 12;
/// More tab's third row (Experiments half): opens the
/// feature-flag toggle list — see [`CreateWorldMode::Experiments`].
pub const EXPERIMENTS_ROW: usize = 13;
const ROW_COUNT: usize = 14;

const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// Every row's rect, hand-placed within its own tab (see the module docs).
/// Two text fields, seven button-shaped rows, a two-button footer —
/// [`row_slot`] is the single definition every one of `Self::new`'s seeded
/// rects, [`super::render`]'s draw and `app.rs`'s hit-test all read, so they
/// cannot drift apart the way a restated constant could.
///
/// Rows that share a *local* position within their own tab (Name and Seed are
/// both their tab's first row; Game Mode and Structures are both second, and
/// so on — see [`content_rows_for_tab`]) share one `Slot` here, because the
/// two tabs are never on screen at once: [`frame`] only ever builds rows for
/// the *active* tab, so there is exactly one live consumer of any given `dy`
/// at a time.
#[must_use]
pub fn row_slot(row: usize) -> Slot {
    const FIELD_W: f32 = 200.0;
    const X: f32 = -(FIELD_W / 2.0);
    // Clear of the tab bar's own underline (`layout::TAB_BAR_HEIGHT`, 24 px)
    // plus a little breathing room, the same margin `stats.rs`'s `HEADER_
    // HEIGHT` reasoning uses for why the *default* header height put a label
    // crossing the divider.
    const TOP: f32 = layout::TAB_BAR_HEIGHT + 16.0;
    const ROW_H: f32 = 24.0;
    // World now has one more row than Game (five vs. four, since the
    // world-type selector landed on World only), so the two tabs' local
    // indices no longer line up 1:1 the way they did when every row paired
    // with exactly one sibling. Named per-tab instead of paired, still one
    // `match` arm per **local row position** so the two tabs' rows that do
    // share a `dy` (never shown at once, so sharing costs nothing) stay
    // visibly paired rather than restated at the same number twice.
    match row {
        // Local row 0: Name / Seed / More's one row (Game Rules).
        NAME_FIELD | SEED_FIELD | GAME_RULES_ROW => {
            Slot { origin: Origin::ScreenTop, dx: X, dy: TOP, w: FIELD_W, h: super::render::EDIT_BOX_H }
        }
        // Local row 1: Game Mode / World Type / More's second row (Experiments).
        GAME_MODE_ROW | WORLD_TYPE_ROW | EXPERIMENTS_ROW => Slot {
            origin: Origin::ScreenTop,
            dx: X,
            dy: TOP + ROW_H,
            w: FIELD_W,
            h: super::render::EDIT_BOX_H,
        },
        // Local row 2: Difficulty / Structures / More's third row (Data Packs).
        DIFFICULTY_ROW | STRUCTURES_ROW | DATA_PACKS_ROW => Slot {
            origin: Origin::ScreenTop,
            dx: X,
            dy: TOP + ROW_H * 2.0,
            w: FIELD_W,
            h: super::render::EDIT_BOX_H,
        },
        // Local row 3: Allow Cheats / Bonus Chest.
        ALLOW_CHEATS_ROW | BONUS_CHEST_ROW => Slot {
            origin: Origin::ScreenTop,
            dx: X,
            dy: TOP + ROW_H * 3.0,
            w: FIELD_W,
            h: super::render::EDIT_BOX_H,
        },
        // Local row 4: World-only — Online Mode. Game's four rows end at
        // local row 3, so this has no Game-side sibling to pair with.
        ONLINE_MODE_ROW => Slot {
            origin: Origin::ScreenTop,
            dx: X,
            dy: TOP + ROW_H * 4.0,
            w: FIELD_W,
            h: super::render::EDIT_BOX_H,
        },
        CREATE_ROW => Slot {
            origin: Origin::Settings(super::options::Placement::Footer { index: 0, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: super::options::SMALL_BUTTON_WIDTH,
            h: super::options::WIDGET_H,
        },
        _ => Slot {
            origin: Origin::Settings(super::options::Placement::Footer { index: 1, count: 2 }),
            dx: 0.0,
            dy: 0.0,
            w: super::options::SMALL_BUTTON_WIDTH,
            h: super::options::WIDGET_H,
        },
    }
}

/// The two [`EditBox`]es plus the five cycle/toggle/submit [`Widget`]s, one
/// struct so [`FocusSet`] can borrow them while [`CreateWorldNav`] borrows
/// the set — the same split [`super::nav::FormFields`] and
/// [`super::world_select::WorldSelectWidgets`] already use, for the same
/// reason (`FocusSet`'s methods take `&mut dyn FocusChildren`).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateWorldWidgets {
    pub name: EditBox,
    pub seed: EditBox,
    pub game_mode: Widget,
    pub difficulty: Widget,
    pub structures: Widget,
    pub bonus_chest: Widget,
    pub allow_cheats: Widget,
    pub online_mode: Widget,
    pub world_type: Widget,
    pub create: Widget,
    pub cancel: Widget,
    /// More tab's Game Rules button — opens
    /// [`CreateWorldMode::GameRules`].
    pub game_rules: Widget,
    /// More tab's Data Packs button — opens
    /// [`CreateWorldMode::DataPacks`].
    pub data_packs: Widget,
    /// More tab's Experiments button — opens
    /// [`CreateWorldMode::Experiments`].
    pub experiments: Widget,
}

impl FocusChildren for CreateWorldWidgets {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        Some(match id {
            NAME_FIELD => &self.name as &dyn FocusTarget,
            SEED_FIELD => &self.seed as &dyn FocusTarget,
            GAME_MODE_ROW => &self.game_mode as &dyn FocusTarget,
            DIFFICULTY_ROW => &self.difficulty as &dyn FocusTarget,
            STRUCTURES_ROW => &self.structures as &dyn FocusTarget,
            BONUS_CHEST_ROW => &self.bonus_chest as &dyn FocusTarget,
            ALLOW_CHEATS_ROW => &self.allow_cheats as &dyn FocusTarget,
            ONLINE_MODE_ROW => &self.online_mode as &dyn FocusTarget,
            WORLD_TYPE_ROW => &self.world_type as &dyn FocusTarget,
            CREATE_ROW => &self.create as &dyn FocusTarget,
            CANCEL_ROW => &self.cancel as &dyn FocusTarget,
            GAME_RULES_ROW => &self.game_rules as &dyn FocusTarget,
            DATA_PACKS_ROW => &self.data_packs as &dyn FocusTarget,
            EXPERIMENTS_ROW => &self.experiments as &dyn FocusTarget,
            _ => return None,
        })
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        Some(match id {
            NAME_FIELD => &mut self.name as &mut dyn FocusTarget,
            SEED_FIELD => &mut self.seed as &mut dyn FocusTarget,
            GAME_MODE_ROW => &mut self.game_mode as &mut dyn FocusTarget,
            DIFFICULTY_ROW => &mut self.difficulty as &mut dyn FocusTarget,
            STRUCTURES_ROW => &mut self.structures as &mut dyn FocusTarget,
            BONUS_CHEST_ROW => &mut self.bonus_chest as &mut dyn FocusTarget,
            ALLOW_CHEATS_ROW => &mut self.allow_cheats as &mut dyn FocusTarget,
            ONLINE_MODE_ROW => &mut self.online_mode as &mut dyn FocusTarget,
            WORLD_TYPE_ROW => &mut self.world_type as &mut dyn FocusTarget,
            CREATE_ROW => &mut self.create as &mut dyn FocusTarget,
            CANCEL_ROW => &mut self.cancel as &mut dyn FocusTarget,
            GAME_RULES_ROW => &mut self.game_rules as &mut dyn FocusTarget,
            DATA_PACKS_ROW => &mut self.data_packs as &mut dyn FocusTarget,
            EXPERIMENTS_ROW => &mut self.experiments as &mut dyn FocusTarget,
            _ => return None,
        })
    }
}

/// What one key or click did to the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateWorldOutcome {
    Handled,
    Cancel,
    /// Pressing Create — carries the [`WorldCreationConfig`] the player
    /// collected, for `menu/nav.rs`'s `apply_create_world` to hand to
    /// `MenuAction::Singleplayer`. Not `Copy` any more:
    /// `WorldCreationConfig` carries a `String` (the world name and the
    /// typed seed text), which `Handled`/`Cancel` never needed.
    Create(WorldCreationConfig),
}

/// This screen's live state: its widgets, its focus, its config, and which
/// tab is showing.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateWorldNav {
    pub widgets: CreateWorldWidgets,
    focus: FocusSet,
    config: WorldCreationConfig,
    /// Which of [`TAB_LABELS`] is currently showing. Starts at
    /// [`GAME_TAB`] — vanilla's own Game/World/More tab-bar order, and the
    /// tab vanilla's own initial-focus routine would land on if it ran (it
    /// does not — see the module docs on why keyboard tab-order is not
    /// ported).
    active_tab: usize,
    /// Which button row the mouse is over, if any — separate from keyboard
    /// focus for [`super::nav::EditForm::hovered`]'s exact reason (this
    /// screen has the same shape: two [`EditBox`]es plus five button rows).
    /// Carries a **focus id**, not a frame-row index — see the module docs'
    /// "two index spaces" section.
    ///
    /// **This is the whole of reported hover bug**, fixed before
    /// the tabs half: `CreateWorld` already reaches
    /// [`super::render::stamp_canvas_facts`] through `render::frame_for`'s own
    /// `frame.map` — every screen's frame does, unconditionally, unless its
    /// arm returns `None` the way in-world `Settings` deliberately does — so
    /// `cursor`/`gui_scale`/`panorama_speed`/`list` were never the gap here,
    /// unlike the two earlier instances of this defect shape. What was
    /// missing was this field: nothing on this screen ever recorded *which*
    /// row the cursor was over, so [`frame`]'s `MenuFrame::hovered` stayed
    /// `None` unconditionally and `widget.hovered` (`render::draw_widget`)
    /// was `false` for every row, every frame — no outline, ever, regardless
    /// of where the mouse was.
    hovered: Option<usize>,
    /// Which sub-screen is showing (Game Rules half) — `Tabs` is
    /// the ordinary Game/World/More view this whole module used to be;
    /// `GameRules` replaces it with [`GAME_RULES_ROW`]'s own scrollable rule
    /// list. Not part of [`Self::active_tab`]: switching tabs while the rule
    /// editor is open is not possible (there is no tab bar drawn in that
    /// mode), so the two are orthogonal rather than one being a special case
    /// of the other.
    mode: CreateWorldMode,
    /// The rule editor's own live state — see [`GameRulesEditor`]'s doc.
    game_rules: GameRulesEditor,
    /// The pack selector's own live state — see [`DataPacksEditor`]'s doc.
    data_packs: DataPacksEditor,
    /// The feature-flag toggle list's own live state — see
    /// [`ExperimentsEditor`]'s doc.
    experiments: ExperimentsEditor,
}

/// See [`CreateWorldNav::mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CreateWorldMode {
    #[default]
    Tabs,
    GameRules,
    DataPacks,
    Experiments,
}

fn button(row: usize, label: impl Into<String>) -> Widget {
    let (x, y, w, h) = row_slot(row).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
    Widget::new(x, y, w, h, label)
}

impl CreateWorldNav {
    /// A fresh screen at vanilla's own defaults — called every time the
    /// screen is opened (vanilla's own "open fresh" state, not
    /// resumed from a previous visit), matching every other "reset on entry"
    /// screen in this tree.
    #[must_use]
    pub fn new() -> Self {
        let config = WorldCreationConfig::default();
        let (nx, ny, nw, nh) = row_slot(NAME_FIELD).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
        let (sx, sy, sw, sh) = row_slot(SEED_FIELD).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
        let mut name = EditBox::new(nx, ny, nw, nh, NAME_LABEL);
        name.set_value(&config.name);
        let mut seed = EditBox::new(sx, sy, sw, sh, SEED_LABEL);
        seed.hint = Some(SEED_INFO.to_string());
        let mut widgets = CreateWorldWidgets {
            name,
            seed,
            game_mode: button(GAME_MODE_ROW, cycle_label(GAME_MODE_LABEL, config.game_mode.caption())),
            difficulty: button(
                DIFFICULTY_ROW,
                cycle_label(DIFFICULTY_LABEL, difficulty_caption(config.difficulty)),
            ),
            structures: button(STRUCTURES_ROW, toggle_label(STRUCTURES_LABEL, config.generate_structures)),
            bonus_chest: button(BONUS_CHEST_ROW, toggle_label(BONUS_CHEST_LABEL, config.bonus_chest)),
            allow_cheats: button(ALLOW_CHEATS_ROW, toggle_label(ALLOW_CHEATS_LABEL, config.allow_cheats)),
            online_mode: button(ONLINE_MODE_ROW, toggle_label(ONLINE_MODE_LABEL, config.online_mode)),
            world_type: button(WORLD_TYPE_ROW, cycle_label(WORLD_TYPE_LABEL, config.world_type.caption())),
            create: button(CREATE_ROW, CREATE_LABEL),
            cancel: button(CANCEL_ROW, CANCEL_LABEL),
            game_rules: button(GAME_RULES_ROW, GAME_RULES_BUTTON_LABEL),
            data_packs: button(DATA_PACKS_ROW, DATA_PACKS_BUTTON_LABEL),
            experiments: button(EXPERIMENTS_ROW, EXPERIMENTS_BUTTON_LABEL),
        };
        let mut focus = FocusSet::new();
        for row in 0..ROW_COUNT {
            focus.add_renderable_widget(row);
        }
        focus.set_initial_focus(&mut widgets, NAME_FIELD);
        let mut nav = Self {
            widgets,
            focus,
            config,
            active_tab: GAME_TAB,
            hovered: None,
            mode: CreateWorldMode::Tabs,
            game_rules: GameRulesEditor::new(),
            data_packs: DataPacksEditor::new(),
            experiments: ExperimentsEditor::new(),
        };
        // Game is active from the start, so this deactivates World's four
        // fields (Seed/Structures/Bonus Chest/Online Mode) — matching what a
        // fresh vanilla screen shows (only the current tab's controls can
        // take focus) — and folds in the (inactive, at the default game mode)
        // hardcore lock.
        nav.sync_tab_visibility();
        nav.apply_hardcore_lock();
        nav
    }

    #[must_use]
    pub fn config(&self) -> &WorldCreationConfig {
        &self.config
    }

    #[must_use]
    pub fn focused(&self) -> Option<usize> {
        self.focus.focused()
    }

    #[must_use]
    pub fn active_tab(&self) -> usize {
        self.active_tab
    }

    /// Whether the Game Rules sub-screen is open — the gate
    /// [`super::nav::MenuNav::active_list`]/[`super::nav::MenuNav::scroll_active_list`]
    /// use to know this screen has a scrollbar right now.
    #[must_use]
    pub fn game_rules_open(&self) -> bool {
        self.mode == CreateWorldMode::GameRules
    }

    /// The rule editor's own scroll offset, in pixels — only meaningful while
    /// [`Self::game_rules_open`].
    #[must_use]
    pub fn game_rules_scroll(&self) -> f32 {
        self.game_rules.scroll()
    }

    /// Scrolls the rule editor by `notches` of mouse wheel, at a
    /// `canvas_height`-tall canvas.
    pub fn scroll_game_rules_by(&mut self, notches: f32, canvas_height: f32) {
        self.game_rules.scroll_by(notches, canvas_height);
    }

    /// Whether the Data Packs sub-screen is open — mirrors
    /// [`Self::game_rules_open`].
    #[must_use]
    pub fn data_packs_open(&self) -> bool {
        self.mode == CreateWorldMode::DataPacks
    }

    /// The pack selector's own scroll offset, in pixels — only meaningful
    /// while [`Self::data_packs_open`].
    #[must_use]
    pub fn data_packs_scroll(&self) -> f32 {
        self.data_packs.scroll()
    }

    /// Whether the Experiments sub-screen is open — mirrors
    /// [`Self::game_rules_open`]/[`Self::data_packs_open`]. No scroll
    /// accessor beside it: [`ExperimentsEditor`]'s own doc explains why a
    /// fixed four rows never needs one.
    #[must_use]
    pub fn experiments_open(&self) -> bool {
        self.mode == CreateWorldMode::Experiments
    }

    /// How many rows the pack selector currently has (the always-present
    /// Vanilla row plus whatever [`DataPacksEditor::refresh`] most recently
    /// found) — `nav.rs`'s own [`super::widget::ListSpec`] needs this, unlike
    /// [`GAME_RULES`]'s fixed length, because a real scan is variable.
    #[must_use]
    pub fn data_packs_len(&self) -> usize {
        self.data_packs.len()
    }

    /// Scrolls the pack selector by `notches` of mouse wheel, at a
    /// `canvas_height`-tall canvas.
    pub fn scroll_data_packs_by(&mut self, notches: f32, canvas_height: f32) {
        self.data_packs.scroll_by(notches, canvas_height);
    }

    /// Sets every widget's `active` flag from [`Self::active_tab`] alone —
    /// the difficulty row is the one exception, folded into
    /// [`Self::apply_hardcore_lock`] instead, since it has a *second* gate
    /// (the hardcore lock) that must combine with tab membership rather than
    /// override it. `FocusTarget::takes_focus` reads `is_active()`, so this
    /// is what keeps Tab traversal inside the showing tab without touching
    /// [`FocusSet`]'s own registries at all — an inactive widget is simply
    /// never offered.
    fn sync_tab_visibility(&mut self) {
        let game = self.active_tab == GAME_TAB;
        let world = self.active_tab == WORLD_TAB;
        self.widgets.name.widget.active = game;
        self.widgets.game_mode.active = game;
        self.widgets.allow_cheats.active = game;
        self.widgets.seed.widget.active = world;
        self.widgets.structures.active = world;
        self.widgets.bonus_chest.active = world;
        self.widgets.online_mode.active = world;
        self.widgets.world_type.active = world;
        let more = self.active_tab == MORE_TAB;
        self.widgets.game_rules.active = more;
        self.widgets.data_packs.active = more;
    }

    /// Difficulty is locked to Hard and its own row inactive while Hardcore
    /// is selected — vanilla's own Game-tab rule (selecting Hardcore forces
    /// and disables the difficulty cycle; every other mode leaves it live) —
    /// **and** while a tab other than Game is showing, folded into the same
    /// flag rather than a second field: both are "can this row take focus or
    /// a click right now", so they combine with `&&` instead of one silently
    /// overriding the other whichever was applied last.
    fn apply_hardcore_lock(&mut self) {
        let hardcore = self.config.game_mode == WorldGameMode::Hardcore;
        if hardcore {
            self.config.difficulty = WorldDifficulty::Hard;
        }
        self.widgets.difficulty.active = self.active_tab == GAME_TAB && !hardcore;
        self.widgets.difficulty.message =
            cycle_label(DIFFICULTY_LABEL, difficulty_caption(self.config.difficulty));
    }

    fn refresh_labels(&mut self) {
        self.widgets.game_mode.message = cycle_label(GAME_MODE_LABEL, self.config.game_mode.caption());
        self.widgets.structures.message = toggle_label(STRUCTURES_LABEL, self.config.generate_structures);
        self.widgets.bonus_chest.message = toggle_label(BONUS_CHEST_LABEL, self.config.bonus_chest);
        self.widgets.allow_cheats.message = toggle_label(ALLOW_CHEATS_LABEL, self.config.allow_cheats);
        self.widgets.online_mode.message = toggle_label(ONLINE_MODE_LABEL, self.config.online_mode);
        self.widgets.world_type.message = cycle_label(WORLD_TYPE_LABEL, self.config.world_type.caption());
        self.apply_hardcore_lock();
    }

    /// Switches [`Self::active_tab`] to `tab` — a no-op for the tab already
    /// showing or an out-of-range index (`TAB_LABELS.len()` is 3; a click
    /// hit-testing onto a row beyond the tab bar never reaches this, but a
    /// direct call should still be inert rather than panic). Clears any
    /// button hover (the hovered row is about to disappear from the frame)
    /// and moves keyboard focus onto the new tab's first field, mirroring
    /// vanilla's own tab switch (which sets initial focus on the new tab) —
    /// or clears focus entirely on
    /// [`MORE_TAB`], which has nothing to focus.
    fn switch_tab(&mut self, tab: usize) {
        if tab >= TAB_LABELS.len() || tab == self.active_tab {
            return;
        }
        self.active_tab = tab;
        self.hovered = None;
        self.sync_tab_visibility();
        self.apply_hardcore_lock();
        match content_rows_for_tab(tab).first() {
            Some(&first) => self.focus.set_initial_focus(&mut self.widgets, first),
            None => self.focus.clear_focus(&mut self.widgets),
        }
    }

    /// The index into [`frame`]'s `MenuFrame::rows` that focus id `id`
    /// currently resolves to, given [`Self::active_tab`] — `None` if `id`
    /// belongs to a tab that is not showing. The inverse of
    /// [`Self::focus_id_for_frame_row`]; see the module docs' "two index
    /// spaces" section for why both exist.
    ///
    /// `pub` (rather than test-only) because `nav.rs`'s own integration tests
    /// drive this screen the way the app does — through frame-row clicks —
    /// and need this same conversion to name a row without hand-deriving the
    /// tab-count offset a second time.
    #[must_use]
    pub fn frame_row_for_focus_id(&self, id: usize) -> Option<usize> {
        let content = content_rows_for_tab(self.active_tab);
        if id == CREATE_ROW {
            return Some(TAB_LABELS.len() + content.len());
        }
        if id == CANCEL_ROW {
            return Some(TAB_LABELS.len() + content.len() + 1);
        }
        content
            .iter()
            .position(|&x| x == id)
            .map(|local| TAB_LABELS.len() + local)
    }

    /// The focus id row `frame_row` (an index into [`frame`]'s own
    /// `MenuFrame::rows`) currently means, given [`Self::active_tab`] —
    /// `None` for a tab-bar row (`frame_row < TAB_LABELS.len()`, handled by
    /// the caller instead — see [`Self::click_row`]) or an index past the
    /// footer. The inverse of [`Self::frame_row_for_focus_id`].
    #[must_use]
    fn focus_id_for_frame_row(&self, frame_row: usize) -> Option<usize> {
        let local = frame_row.checked_sub(TAB_LABELS.len())?;
        let content = content_rows_for_tab(self.active_tab);
        if local < content.len() {
            return Some(content[local]);
        }
        match local - content.len() {
            0 => Some(CREATE_ROW),
            1 => Some(CANCEL_ROW),
            _ => None,
        }
    }

    /// The mouse moved over frame row `row` (an index into `frame(..).rows`
    /// — see the module docs' "two index spaces" section). A tab-bar row
    /// records nothing here: [`super::render::draw`]'s own `MenuRow::tab` arm
    /// derives tab hover straight from `MenuFrame::cursor` and the tab's own
    /// rect (the same thing that already made Statistics's tab bar highlight
    /// correctly with no `Screen::Statistics` arm in `MenuNav::hover` at
    /// all), so recording it a second way here would be a second, and
    /// possibly disagreeing, source of truth.
    ///
    /// A field row (`NAME_FIELD`/`SEED_FIELD`) does nothing here either —
    /// mirrors [`super::nav::EditForm::hover_row`] exactly, including its
    /// reason: hovering the Seed field while typing in Name cannot steal the
    /// caret out from under the player (vanilla's `ContainerEventHandler`
    /// moves focus only from a *click* or Tab traversal, never from hover —
    /// `EditBox` itself has no hover highlight at all). Every other row
    /// records only [`Self::hovered`], which is what lets the mouse travel to
    /// Create without touching whichever field currently has the keyboard.
    pub fn hover_row(&mut self, row: usize) {
        if self.mode == CreateWorldMode::GameRules {
            self.game_rules.hover_row(row);
            return;
        }
        if self.mode == CreateWorldMode::DataPacks {
            self.data_packs.hover_row(row);
            return;
        }
        if self.mode == CreateWorldMode::Experiments {
            self.experiments.hover_row(row);
            return;
        }
        if row < TAB_LABELS.len() {
            return;
        }
        let Some(focus_id) = self.focus_id_for_frame_row(row) else {
            return;
        };
        match focus_id {
            NAME_FIELD | SEED_FIELD => {}
            _ => self.hovered = Some(focus_id),
        }
    }

    /// The focus id the mouse is over, for [`super::render::MenuFrame::hovered`]
    /// (via [`frame`]'s own conversion through [`Self::frame_row_for_focus_id`]).
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    fn activate(&mut self, row: usize) -> CreateWorldOutcome {
        match row {
            GAME_MODE_ROW => {
                self.config.game_mode = self.config.game_mode.next();
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            DIFFICULTY_ROW => {
                if self.widgets.difficulty.active {
                    self.config.difficulty = next_difficulty(self.config.difficulty);
                    self.refresh_labels();
                }
                CreateWorldOutcome::Handled
            }
            STRUCTURES_ROW => {
                self.config.generate_structures = !self.config.generate_structures;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            BONUS_CHEST_ROW => {
                self.config.bonus_chest = !self.config.bonus_chest;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            ALLOW_CHEATS_ROW => {
                self.config.allow_cheats = !self.config.allow_cheats;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            ONLINE_MODE_ROW => {
                self.config.online_mode = !self.config.online_mode;
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            WORLD_TYPE_ROW => {
                self.config.world_type = self.config.world_type.next();
                self.refresh_labels();
                CreateWorldOutcome::Handled
            }
            GAME_RULES_ROW => {
                self.mode = CreateWorldMode::GameRules;
                CreateWorldOutcome::Handled
            }
            DATA_PACKS_ROW => {
                self.mode = CreateWorldMode::DataPacks;
                // Scan on entering the sub-screen, not on every visit to the
                // wider Create New World screen — the same trigger
                // `menu::packs::PacksNav::reset` uses for the Resource Packs
                // screen, so a pack dropped into the folder mid-session is
                // picked up the next time this row is clicked.
                self.data_packs.refresh();
                CreateWorldOutcome::Handled
            }
            EXPERIMENTS_ROW => {
                self.mode = CreateWorldMode::Experiments;
                CreateWorldOutcome::Handled
            }
            CREATE_ROW => {
                self.config.name = self.widgets.name.value().to_string();
                self.config.seed = self.widgets.seed.value().to_string();
                self.config.game_rules = self.game_rules.changed_entries();
                self.config.data_packs = self.data_packs.selected_ids();
                self.config.experiments = self.experiments.enabled_ids();
                CreateWorldOutcome::Create(self.config.clone())
            }
            CANCEL_ROW => CreateWorldOutcome::Cancel,
            _ => CreateWorldOutcome::Handled,
        }
    }

    /// A click on frame row `row` (an index into `frame(..).rows` — see the
    /// module docs). Mirrors
    /// [`super::world_select::WorldSelectNav::click_row`]'s own reasoning: a
    /// click focuses a field, presses a button, or switches the active tab,
    /// and none of those is "hover then Enter".
    pub fn click_row(&mut self, row: usize) -> CreateWorldOutcome {
        if self.mode == CreateWorldMode::GameRules {
            if self.game_rules.click_row(row) {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if self.mode == CreateWorldMode::DataPacks {
            if self.data_packs.click_row(row) {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if self.mode == CreateWorldMode::Experiments {
            if self.experiments.click_row(row) {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if row < TAB_LABELS.len() {
            self.switch_tab(row);
            return CreateWorldOutcome::Handled;
        }
        let Some(focus_id) = self.focus_id_for_frame_row(row) else {
            return CreateWorldOutcome::Handled;
        };
        if focus_id == NAME_FIELD || focus_id == SEED_FIELD {
            self.focus.set_focused(&mut self.widgets, Some(focus_id));
            return CreateWorldOutcome::Handled;
        }
        let active = self
            .widgets
            .get(focus_id)
            .is_some_and(super::focus::FocusTarget::is_active);
        if !active {
            return CreateWorldOutcome::Handled;
        }
        self.focus.set_focused(&mut self.widgets, Some(focus_id));
        self.activate(focus_id)
    }

    /// One key, routed through the same `Escape` → field → navigation →
    /// screen order [`super::nav::EditForm::handle_key`] already documents
    /// and cites vanilla's own key-handling order for. Tab traversal stays
    /// within the showing tab's own fields plus the always-active footer —
    /// see [`Self::sync_tab_visibility`]'s own doc on why that needs no
    /// special case here: [`FocusSet`] already skips an inactive widget.
    pub fn handle_key(&mut self, key: MenuKey) -> CreateWorldOutcome {
        // Escape from the rule editor returns to the More tab rather than
        // discarding the whole screen — the same "Escape unwinds one level"
        // shape [`super::options::SettingsNav`]'s page graph already uses,
        // narrowed to this screen's one extra level. Edits already live on
        // `self.game_rules` the moment a button is clicked (there is no
        // separate discard-on-cancel path here, unlike vanilla's own
        // game-rules screen's close-and-discard routine — a disclosed
        // simplification, not a missed case).
        if self.mode == CreateWorldMode::GameRules {
            if key == MenuKey::Escape {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if self.mode == CreateWorldMode::DataPacks {
            if key == MenuKey::Escape {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if self.mode == CreateWorldMode::Experiments {
            if key == MenuKey::Escape {
                self.mode = CreateWorldMode::Tabs;
            }
            return CreateWorldOutcome::Handled;
        }
        if key == MenuKey::Escape {
            return CreateWorldOutcome::Cancel;
        }
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.widgets, ch);
            return CreateWorldOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return CreateWorldOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.widgets, event) {
            KeyOutcome::Close => CreateWorldOutcome::Cancel,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => CreateWorldOutcome::Handled,
            KeyOutcome::Declined if key == MenuKey::Enter => {
                match self.focus.focused() {
                    Some(row) => self.activate(row),
                    None => CreateWorldOutcome::Handled,
                }
            }
            KeyOutcome::Declined => CreateWorldOutcome::Handled,
        }
    }
}

impl Default for CreateWorldNav {
    fn default() -> Self {
        Self::new()
    }
}

fn cycle_label(caption: &str, value: &str) -> String {
    format!("{caption}: {value}")
}

fn toggle_label(caption: &str, on: bool) -> String {
    format!("{caption}: {}", if on { "ON" } else { "OFF" })
}

/// Builds the whole World Creation frame: the tab bar plus whichever tab's
/// own rows are active, plus the always-present Create/Cancel footer.
///
/// ## No separate title label — the same call `stats.rs` already made
///
/// Vanilla draws no heading above this screen's tab bar either — the
/// bar's own background *is* the
/// header, exactly as `stats.rs`'s own doc explains for the same widget. This
/// used to draw `"Create New World"` as a centred label at `dy: 12`, which is
/// the vanilla string for the *button* that opens this screen
/// (`selectWorld.create`), not a real vanilla heading on the screen itself.
#[must_use]
pub fn frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
    if nav.mode == CreateWorldMode::GameRules {
        return game_rules_frame(nav);
    }
    if nav.mode == CreateWorldMode::DataPacks {
        return data_packs_frame(nav);
    }
    if nav.mode == CreateWorldMode::Experiments {
        return experiments_frame(nav);
    }
    let focused = nav.focused();
    let widget_row = |w: &Widget, row: usize| MenuRow {
        label: w.message.clone(),
        enabled: w.active,
        slot: Some(row_slot(row)),
        ..Default::default()
    };
    // Mirrors `Screen::ServerEdit`'s own field rows (`render.rs`'s
    // `manage_server_slot` arm): a live `EditBox` clone, `field: true`, and
    // `slot` rather than the generic stack — `draw_edit_box` reads the box's
    // own state (value, caret, selection, hint) and decides nothing here.
    let field_row = |edit: &EditBox, row: usize| MenuRow {
        label: edit.value().to_string(),
        enabled: true,
        field: true,
        edit: Some(edit.clone()),
        slot: Some(row_slot(row)),
        ..Default::default()
    };

    let active_tab = nav.active_tab();
    let content = content_rows_for_tab(active_tab);
    let mut rows = Vec::with_capacity(TAB_LABELS.len() + content.len() + 2);
    rows.extend(TAB_LABELS.iter().enumerate().map(|(index, &label)| MenuRow {
        label: label.to_string(),
        // Every tab is real and clickable — unlike Statistics's Items/Mobs,
        // nothing here is data-driven-empty; see the module docs on why More
        // is `enabled` even though it has no rows of its own.
        enabled: true,
        tab: Some(TabEntryView {
            index,
            count: TAB_LABELS.len(),
            selected: index == active_tab,
        }),
        ..Default::default()
    }));
    for &id in content {
        rows.push(match id {
            NAME_FIELD => field_row(&nav.widgets.name, NAME_FIELD),
            SEED_FIELD => field_row(&nav.widgets.seed, SEED_FIELD),
            GAME_MODE_ROW => widget_row(&nav.widgets.game_mode, GAME_MODE_ROW),
            DIFFICULTY_ROW => widget_row(&nav.widgets.difficulty, DIFFICULTY_ROW),
            STRUCTURES_ROW => widget_row(&nav.widgets.structures, STRUCTURES_ROW),
            BONUS_CHEST_ROW => widget_row(&nav.widgets.bonus_chest, BONUS_CHEST_ROW),
            ALLOW_CHEATS_ROW => widget_row(&nav.widgets.allow_cheats, ALLOW_CHEATS_ROW),
            ONLINE_MODE_ROW => widget_row(&nav.widgets.online_mode, ONLINE_MODE_ROW),
            WORLD_TYPE_ROW => widget_row(&nav.widgets.world_type, WORLD_TYPE_ROW),
            GAME_RULES_ROW => widget_row(&nav.widgets.game_rules, GAME_RULES_ROW),
            DATA_PACKS_ROW => widget_row(&nav.widgets.data_packs, DATA_PACKS_ROW),
            EXPERIMENTS_ROW => widget_row(&nav.widgets.experiments, EXPERIMENTS_ROW),
            // `content_rows_for_tab` is the only producer of these ids; a new
            // entry there needs a matching arm here, and an out-of-sync pair
            // is a compile-time `unreachable!()` away from being caught the
            // first time a test actually visits the new row rather than
            // silently drawing a blank one.
            _ => unreachable!("content_rows_for_tab produced an id `frame` has no arm for: {id}"),
        });
    }
    rows.push(widget_row(&nav.widgets.create, CREATE_ROW));
    rows.push(widget_row(&nav.widgets.cancel, CANCEL_ROW));

    let selected = focused
        .and_then(|id| nav.frame_row_for_focus_id(id))
        .unwrap_or(usize::MAX);
    let hovered = nav.hovered().and_then(|id| nav.frame_row_for_focus_id(id));

    let mut labels = Vec::new();
    // Vanilla's own labeled-element layout helper draws a real, visible label
    // above each field — only the active tab's own
    // field label(s) are emitted, matching the row itself only being emitted
    // for the active tab.
    if active_tab == GAME_TAB {
        labels.push(MenuLabel {
            text: NAME_LABEL.to_string(),
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: row_slot(NAME_FIELD).dy - 10.0,
            align: Align::Left,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }
    if active_tab == WORLD_TAB {
        // The seed field's own visible label — `SEED_LABEL`
        // (`selectWorld.enterSeed`, "Seed for the world generator"). This
        // used to be missing entirely: only the *hint* text
        // (`SEED_INFO`/`selectWorld.seedInfo`) was drawn, and as a permanent
        // notice rather than vanilla's own hint ghost text — see the
        // `notice` doc below.
        labels.push(MenuLabel {
            text: SEED_LABEL.to_string(),
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: row_slot(SEED_FIELD).dy - 10.0,
            align: Align::Left,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        selected,
        // This used to be left at its `..Default::default()` of
        // `None` unconditionally — nothing on this screen ever recorded which
        // row the mouse was over (see `CreateWorldNav::hovered`'s own doc) —
        // so `render::draw_widget`'s `widget.hovered` was `false` for every
        // row, every frame, and no button ever drew its hover outline.
        hovered,
        vanilla: true,
        labels,
        // No `notice` here. Vanilla shows `SEED_INFO` in exactly one place —
        // its own seed-field hint —
        // ghost text drawn only while the box is empty and unfocused.
        // `CreateWorldNav::new` already sets `seed.hint`, so a permanent
        // notice here would draw the same string vanilla only ever shows
        // conditionally — a duplicate, not a second real label.
        ..Default::default()
    }
}

// -- Game Rules sub-screen (More tab) ---------------------------
//
// Vanilla's `WorldCreationGameRulesScreen` is a per-type widget (a checkbox
// for a boolean rule, a free-text `EditBox` for an integer one) over a
// two-column `AbstractSelectionList`. This is a narrower shape: **every** row,
// boolean or integer, is a `-`/`+` step pair plus a `"name: value"` label —
// one geometry for both types rather than two, and a real, working control
// for each (an integer rule needs to go *down* from its default as often as
// up, which a single "cycle" button cannot do without an impractically long
// wraparound for an unbounded rule like `max_command_forks`). A disclosed
// simplification, not a missing feature: every one of vanilla's 60 rules is
// listed, live, and reaches the wire.
//
// Edits apply to [`GameRulesEditor::values`] immediately on click — there is
// no separate "Done commits, Cancel discards" distinction the way vanilla's
// screen has, because nothing downstream reads the values until Create is
// pressed (`CreateWorldNav::activate`'s `CREATE_ROW` arm reads
// [`GameRulesEditor::changed_entries`] at that point, not before) — so
// leaving the rule editor via Done, Escape or the tab bar are all the same
// "keep what's here" operation.

/// One rule's identity plus its live value — [`GameRulesEditor`]'s per-row
/// state, parallel to [`GAME_RULES`] by index.
#[derive(Debug, Clone, PartialEq)]
pub struct GameRulesEditor {
    values: Vec<GameRuleValue>,
    scroll: f32,
    cursor: usize,
}

impl GameRulesEditor {
    fn new() -> Self {
        Self {
            values: GAME_RULES.iter().map(|rule| rule.default).collect(),
            scroll: 0.0,
            cursor: 0,
        }
    }

    /// The scroll offset, in pixels.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// Rule `index`'s live value, or its default if `index` is out of range
    /// (never true in practice — `values` is seeded 1:1 from [`GAME_RULES`]
    /// and never resized).
    #[must_use]
    fn value(&self, index: usize) -> GameRuleValue {
        self.values
            .get(index)
            .copied()
            .unwrap_or_else(|| GAME_RULES[index.min(GAME_RULES.len().saturating_sub(1))].default)
    }

    /// Every rule whose live value differs from its vanilla default, as the
    /// `(namespaced key, wire-form string)` pairs
    /// [`lodestone_model::action::ClientAction::SetGameRules`] carries.
    /// Sending only the changed subset (rather than vanilla's own "always
    /// re-apply everything" shape) is a byte-count optimisation, not a
    /// correctness difference: an unset rule already reads the server's own
    /// identical default.
    #[must_use]
    pub fn changed_entries(&self) -> Vec<(lodestone_model::ResourceKey, String)> {
        GAME_RULES
            .iter()
            .zip(&self.values)
            .filter(|(spec, value)| **value != spec.default)
            .map(|(spec, value)| {
                (
                    // `GAME_RULES` names are bare identifiers ("no
                    // `minecraft:` namespace" — `lodestone_server::game_rules`'s
                    // own doc); the wire needs the full resource key.
                    lodestone_model::Identifier::new("minecraft", spec.name)
                        .expect("every GAME_RULES name is a valid identifier path"),
                    value.serialize(),
                )
            })
            .collect()
    }

    fn step(&mut self, index: usize, increase: bool) {
        let (Some(spec), Some(value)) = (GAME_RULES.get(index), self.values.get_mut(index)) else {
            return;
        };
        *value = match (spec.default, *value) {
            // A boolean rule has no meaningful "step" — `-`/`+` are simply
            // Off/On, matching vanilla's own two-state checkbox.
            (GameRuleValue::Bool(_), GameRuleValue::Bool(_)) => GameRuleValue::Bool(increase),
            (GameRuleValue::Int(_), GameRuleValue::Int(current)) => {
                let min = spec.min.unwrap_or(i32::MIN);
                let max = spec.max.unwrap_or(i32::MAX);
                let delta = if increase { 1 } else { -1 };
                GameRuleValue::Int(current.saturating_add(delta).clamp(min, max))
            }
            // `GAME_RULES` never mixes the two default shapes for one entry.
            (_, other) => other,
        };
    }

    /// The live [`super::widget::ScrollList`] at this canvas height, or
    /// `None` when there is nothing to scroll — mirrors
    /// [`super::key_binds::KeyBindsNav::model`].
    fn model(&self, canvas_height: f32) -> Option<super::widget::ScrollList> {
        game_rules_list_spec(self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the shared primitive.
    pub fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    #[must_use]
    fn visible(&self) -> Vec<GameRuleControlView> {
        game_rule_controls(self.scroll)
    }

    /// The cursor's position within [`Self::visible`], for the highlight —
    /// mirrors [`super::key_binds::KeyBindsNav::selected_row`].
    #[must_use]
    fn selected_row(&self) -> Option<usize> {
        let all = all_game_rule_controls();
        let control = *all.get(self.cursor)?;
        self.visible().iter().position(|c| c.control == control)
    }

    /// The mouse moved over visible row `row` — moves the cursor there, the
    /// same "hover moves the cursor" shape
    /// [`super::key_binds::KeyBindsNav::hover_row`] uses.
    fn hover_row(&mut self, row: usize) {
        let visible = self.visible();
        let Some(view) = visible.get(row).copied() else {
            return;
        };
        let all = all_game_rule_controls();
        if let Some(i) = all.iter().position(|&c| c == view.control) {
            self.cursor = i;
        }
    }

    /// A click on visible row `row`. Returns `true` when Done was pressed —
    /// [`CreateWorldNav::click_row`]'s cue to leave the editor.
    fn click_row(&mut self, row: usize) -> bool {
        self.hover_row(row);
        let visible = self.visible();
        let Some(view) = visible.get(row).copied() else {
            return false;
        };
        match view.control {
            GameRuleControl::Minus(index) => {
                self.step(index, false);
                false
            }
            GameRuleControl::Plus(index) => {
                self.step(index, true);
                false
            }
            GameRuleControl::Done => true,
        }
    }
}

/// One clickable control of the Game Rules screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GameRuleControl {
    /// Step rule `usize` (an index into [`GAME_RULES`]) down.
    Minus(usize),
    /// Step rule `usize` up.
    Plus(usize),
    /// Leave the editor, keeping every edit made so far.
    Done,
}

/// One flattened, focusable control plus its already-resolved [`Slot`] —
/// mirrors [`super::key_binds::KeyControlView`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct GameRuleControlView {
    control: GameRuleControl,
    slot: Slot,
}

/// Every control, ignoring scroll — mirrors
/// [`super::key_binds::all_controls`].
#[must_use]
fn all_game_rule_controls() -> Vec<GameRuleControl> {
    let mut out = Vec::with_capacity(GAME_RULES.len() * 2 + 1);
    for index in 0..GAME_RULES.len() {
        out.push(GameRuleControl::Minus(index));
        out.push(GameRuleControl::Plus(index));
    }
    out.push(GameRuleControl::Done);
    out
}

/// `-`/`+` step button width, and the gap between them and the row's right
/// edge / each other.
const STEP_BUTTON_W: f32 = 20.0;
const STEP_GAP: f32 = 4.0;
/// The rule list's row band, centred like [`super::key_binds::ROW_WIDTH`] but
/// narrower — this screen has one label and two small buttons per row, not
/// `KeyBindsList`'s name-plus-two-75/50-px-buttons.
pub const GAME_RULE_ROW_WIDTH: f32 = 280.0;
pub const GAME_RULE_ROW_H: f32 = 20.0;

#[must_use]
pub fn game_rule_row_left(width: f32) -> f32 {
    width * 0.5 - GAME_RULE_ROW_WIDTH * 0.5
}

#[must_use]
pub fn game_rule_row_right(width: f32) -> f32 {
    game_rule_row_left(width) + GAME_RULE_ROW_WIDTH
}

#[must_use]
pub fn game_rule_plus_x(width: f32) -> f32 {
    game_rule_row_right(width) - STEP_BUTTON_W
}

#[must_use]
pub fn game_rule_minus_x(width: f32) -> f32 {
    game_rule_plus_x(width) - STEP_GAP - STEP_BUTTON_W
}

#[must_use]
pub fn game_rule_name_x(width: f32) -> f32 {
    game_rule_row_left(width) + 4.0
}

/// Budget of list pixels, mirroring [`super::key_binds::LIST_WINDOW_PX`] —
/// same fixed-budget reasoning (no GPU scissor here, see that constant's own
/// doc), reusing the identical header/footer constants so this screen's band
/// agrees with every other settings-shaped list in this tree.
pub const GAME_RULES_LIST_WINDOW_PX: f32 = crate::config::MIN_SCALED_HEIGHT as f32
    - options::SUB_HEADER_HEIGHT
    - options::FOOTER_HEIGHT
    - options::LIST_TOP_INSET;

/// This screen's list, as the generic [`super::widget::ListSpec`] the
/// scrollbar draw and the mouse wheel both go through — mirrors
/// [`super::key_binds::list_spec`].
#[must_use]
pub fn game_rules_list_spec(scroll: f32) -> super::widget::ListSpec {
    super::widget::ListSpec::uniform(
        GAME_RULE_ROW_H,
        options::SUB_HEADER_HEIGHT,
        options::FOOTER_HEIGHT,
        GAME_RULES.len(),
        GAME_RULE_ROW_WIDTH,
    )
    .at(scroll)
}

/// Where one [`GameRuleControl`] (other than [`GameRuleControl::Done`], which
/// reuses [`Origin::Settings`]'s footer) sits — [`Origin::CreateWorldGameRules`]'s
/// whole body. Mirrors [`super::key_binds::KeyPlacement`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameRulePlacement {
    Minus { row: u16, scroll: f32 },
    Plus { row: u16, scroll: f32 },
    /// The `"name: value"` label — a [`MenuLabel`], not a [`GameRuleControl`].
    Name { row: u16, scroll: f32 },
}

impl GameRulePlacement {
    fn row_scroll(self) -> (u16, f32) {
        match self {
            GameRulePlacement::Minus { row, scroll }
            | GameRulePlacement::Plus { row, scroll }
            | GameRulePlacement::Name { row, scroll } => (row, scroll),
        }
    }
}

/// The top-left of the widget a [`GameRulePlacement`] names — mirrors
/// [`super::key_binds::placement_anchor`].
#[must_use]
pub fn game_rule_placement_anchor(placement: GameRulePlacement, width: f32, _height: f32) -> (f32, f32) {
    let (row, scroll) = placement.row_scroll();
    let row_y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(row) * GAME_RULE_ROW_H
        - scroll.floor();
    match placement {
        GameRulePlacement::Minus { .. } => (game_rule_minus_x(width), row_y),
        GameRulePlacement::Plus { .. } => (game_rule_plus_x(width), row_y),
        GameRulePlacement::Name { .. } => (game_rule_name_x(width), row_y + 6.0),
    }
}

/// **Every** control at scroll offset `scroll`, then Done — mirrors
/// [`super::key_binds::controls`]: absolute indices into [`all_game_rule_controls`],
/// not a windowed slice, since [`super::render::draw`] clips a row to the band
/// itself.
#[must_use]
fn game_rule_controls(scroll: f32) -> Vec<GameRuleControlView> {
    let mut out = Vec::with_capacity(GAME_RULES.len() * 2 + 1);
    for row in 0..GAME_RULES.len() {
        out.push(GameRuleControlView {
            control: GameRuleControl::Minus(row),
            slot: Slot {
                origin: Origin::CreateWorldGameRules(GameRulePlacement::Minus {
                    row: row as u16,
                    scroll,
                }),
                dx: 0.0,
                dy: 0.0,
                w: STEP_BUTTON_W,
                h: GAME_RULE_ROW_H,
            },
        });
        out.push(GameRuleControlView {
            control: GameRuleControl::Plus(row),
            slot: Slot {
                origin: Origin::CreateWorldGameRules(GameRulePlacement::Plus {
                    row: row as u16,
                    scroll,
                }),
                dx: 0.0,
                dy: 0.0,
                w: STEP_BUTTON_W,
                h: GAME_RULE_ROW_H,
            },
        });
    }
    out.push(GameRuleControlView {
        control: GameRuleControl::Done,
        slot: Slot {
            origin: Origin::Settings(options::Placement::Footer { index: 0, count: 1 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
    });
    out
}

/// Builds the Game Rules sub-screen's whole frame — [`frame`]'s
/// [`CreateWorldMode::GameRules`] branch.
#[must_use]
fn game_rules_frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
    let editor = &nav.game_rules;
    let visible = editor.visible();
    let selected = editor.selected_row();

    let rows: Vec<MenuRow> = visible
        .iter()
        .map(|view| MenuRow {
            label: match view.control {
                GameRuleControl::Minus(_) => "-".to_string(),
                GameRuleControl::Plus(_) => "+".to_string(),
                GameRuleControl::Done => "Done".to_string(),
            },
            enabled: true,
            slot: Some(view.slot),
            ..Default::default()
        })
        .collect();

    // `list_labels`, not `labels` — these scroll with the band and must be
    // clipped to it, the same split `key_binds::frame` documents for its own
    // Name/Category labels.
    let mut list_labels = Vec::with_capacity(GAME_RULES.len());
    for (row, spec) in GAME_RULES.iter().enumerate() {
        let value = editor.value(row);
        list_labels.push(MenuLabel {
            text: format!("{}: {}", spec.name, value.serialize()),
            origin: Origin::CreateWorldGameRules(GameRulePlacement::Name {
                row: row as u16,
                scroll: editor.scroll(),
            }),
            dx: 0.0,
            dy: 0.0,
            align: Align::Left,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        });
    }

    MenuFrame {
        title: "Game Rules",
        rows,
        selected: selected.unwrap_or(usize::MAX),
        vanilla: true,
        labels: vec![MenuLabel {
            text: "Game Rules".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 12.0,
            align: Align::Centre,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        }],
        list_labels,
        // **Deliberately not set here** — `render::dispatch` stamps
        // `f.list = nav.active_list(ui)` on every frame, matching
        // `key_binds::frame`'s own comment on why setting it twice invites
        // the two declarations to disagree.
        ..Default::default()
    }
}

// -- Data Packs sub-screen (More tab) ---------------------------
//
// Vanilla's Data Packs button opens the same `PackSelectionScreen` widget as
// the standalone Resource Packs screen (`super::packs`), pointed at a
// world-scoped pack repository instead of the global one. This module does
// not reuse `super::packs::PacksNav` directly: that type is wired straight to
// the *global* resource-pack config (`crate::config::SelectedPacks`) and the
// live texture stack (`crate::resources::set_selected_packs`,
// `Sim::reload_resource_pack_atlas`) — repurposing it here would make
// selecting a *data* pack while creating a world also reshuffle the running
// client's *textures*, which is not what either screen does in vanilla.
//
// What **is** reused: `crate::resources::scan_resource_packs_in` and
// `crate::resources::DiscoveredPack`. A data pack and a resource pack are the
// identical on-disk shape to a scanner that only ever reads `pack.mcmeta` and
// `pack.png` — this screen never opens a pack's actual contents (there is no
// data-pack loader in this crate to hand them to; see
// [`WorldCreationConfig::data_packs`]'s own doc) — so only the directory
// differs ([`crate::resources::data_packs_dir`] instead of
// [`crate::resources::resource_packs_dir`]).
//
// A further, disclosed simplification on top of that reuse: vanilla's screen
// is two columns (Available/Selected) with reorder buttons, mirroring
// `super::packs`'s own shape. This is one scrollable list with a per-row
// toggle instead — the same kind of flattening `GameRulesEditor`'s own module
// doc already argues for (one control shape instead of vanilla's several),
// and for the same underlying reason: data-pack *priority order* has nothing
// downstream to affect it, since nothing here ever loads a pack's contents.
// The built-in "Vanilla" row is always present, always selected, and — like
// `super::packs::PackRow::builtin` — never a transfer target, matched by
// construction ([`DataPacksEditor::rebuild`] always seeds it first) rather
// than by a flag.

/// One row of the Data Packs list — the built-in Vanilla entry (`id: None`)
/// plus one entry per pack [`crate::resources::scan_resource_packs_in`] finds
/// under [`crate::resources::data_packs_dir`]. Carries only what this screen
/// draws (a title and whether it is selected), not the full
/// [`crate::resources::DiscoveredPack`]: this screen never reads a pack's
/// icon or its declared `pack_format`, so pulling the whole struct in would
/// drag `lodestone_assets::Image`'s no-`PartialEq` shape into this module's
/// own derive list for nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DataPackRow {
    /// [`crate::resources::DiscoveredPack::id`], or `None` for the built-in
    /// Vanilla row — it is not a file on disk and can never be deselected
    /// (see [`DataPacksEditor::click_row`]).
    id: Option<String>,
    title: String,
    selected: bool,
}

/// One clickable control of the Data Packs screen — mirrors
/// [`GameRuleControl`], narrowed to this screen's single per-row action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataPackControl {
    /// Toggle row `usize` (an index into [`DataPacksEditor::rows`]) between
    /// Available and Selected. A no-op for index `0` (Vanilla).
    Toggle(usize),
    /// Leave the editor, keeping every selection made so far — the same
    /// "no separate discard path" rule [`GameRulesEditor`]'s own doc records.
    Done,
}

/// One flattened, focusable control plus its already-resolved [`Slot`] —
/// mirrors [`GameRuleControlView`].
#[derive(Debug, Clone, Copy, PartialEq)]
struct DataPackControlView {
    control: DataPackControl,
    slot: Slot,
}

/// Every control for a list of `len` rows, ignoring scroll — mirrors
/// [`all_game_rule_controls`].
#[must_use]
fn all_data_pack_controls(len: usize) -> Vec<DataPackControl> {
    let mut out = Vec::with_capacity(len + 1);
    for index in 0..len {
        out.push(DataPackControl::Toggle(index));
    }
    out.push(DataPackControl::Done);
    out
}

/// This screen's row band — narrower than [`GAME_RULE_ROW_WIDTH`] because a
/// row here has no `-`/`+` step pair riding on its right edge, just the one
/// full-width toggle button.
pub const DATA_PACK_ROW_WIDTH: f32 = 280.0;
pub const DATA_PACK_ROW_H: f32 = 20.0;

#[must_use]
fn data_pack_row_left(width: f32) -> f32 {
    width * 0.5 - DATA_PACK_ROW_WIDTH * 0.5
}

/// This screen's list, as the generic [`super::widget::ListSpec`] the
/// scrollbar draw and the mouse wheel both go through — mirrors
/// [`game_rules_list_spec`]. Takes `len` explicitly, unlike that function:
/// [`GAME_RULES`] is a fixed-length table, but a real scan's row count
/// varies, so nothing here can close over a constant the way the Game Rules
/// sibling does.
#[must_use]
pub fn data_packs_list_spec(len: usize, scroll: f32) -> super::widget::ListSpec {
    super::widget::ListSpec::uniform(
        DATA_PACK_ROW_H,
        options::SUB_HEADER_HEIGHT,
        options::FOOTER_HEIGHT,
        len,
        DATA_PACK_ROW_WIDTH,
    )
    .at(scroll)
}

/// Where one Data Packs row sits — [`Origin::CreateWorldDataPacks`]'s whole
/// body. Mirrors [`GameRulePlacement`], narrowed to the one shape this
/// screen's rows need (Done reuses [`Origin::Settings`]'s footer, same as
/// [`GameRulePlacement`]'s own Done).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DataPackPlacement {
    Row { row: u16, scroll: f32 },
}

/// The top-left of the widget a [`DataPackPlacement`] names — mirrors
/// [`game_rule_placement_anchor`].
#[must_use]
pub fn data_pack_placement_anchor(placement: DataPackPlacement, width: f32, _height: f32) -> (f32, f32) {
    match placement {
        DataPackPlacement::Row { row, scroll } => {
            let y = options::SUB_HEADER_HEIGHT + options::LIST_TOP_INSET + f32::from(row) * DATA_PACK_ROW_H
                - scroll.floor();
            (data_pack_row_left(width), y)
        }
    }
}

/// **Every** control at scroll offset `scroll`, then Done — mirrors
/// [`game_rule_controls`]: absolute indices into `rows`, not a windowed
/// slice, since [`super::render::draw`] clips a row to the band itself.
#[must_use]
fn data_pack_controls(rows: &[DataPackRow], scroll: f32) -> Vec<DataPackControlView> {
    let mut out = Vec::with_capacity(rows.len() + 1);
    for row in 0..rows.len() {
        out.push(DataPackControlView {
            control: DataPackControl::Toggle(row),
            slot: Slot {
                origin: Origin::CreateWorldDataPacks(DataPackPlacement::Row { row: row as u16, scroll }),
                dx: 0.0,
                dy: 0.0,
                w: DATA_PACK_ROW_WIDTH,
                h: DATA_PACK_ROW_H,
            },
        });
    }
    out.push(DataPackControlView {
        control: DataPackControl::Done,
        slot: Slot {
            origin: Origin::Settings(options::Placement::Footer { index: 0, count: 1 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        },
    });
    out
}

/// The Data Packs sub-screen's own live state — mirrors [`GameRulesEditor`].
#[derive(Debug, Clone, PartialEq)]
pub struct DataPacksEditor {
    rows: Vec<DataPackRow>,
    scroll: f32,
    cursor: usize,
}

impl DataPacksEditor {
    /// Vanilla alone — no scan yet. The real scan happens only when the
    /// player actually opens this sub-screen
    /// ([`CreateWorldNav::activate`]'s [`DATA_PACKS_ROW`] arm calls
    /// [`Self::refresh`]), not on every visit to the wider Create New World
    /// screen — the same "scan on entering that specific screen" trigger
    /// [`super::packs::PacksNav::reset`] uses, rather than doing it in
    /// [`super::options::SettingsNav`]'s own construction.
    fn new() -> Self {
        Self {
            rows: vec![DataPackRow { id: None, title: "Vanilla".to_string(), selected: true }],
            scroll: 0.0,
            cursor: 0,
        }
    }

    /// Rebuilds `rows` from `discovered` — the pure half [`Self::refresh`]
    /// calls with a live scan, and what tests call directly with a fixture
    /// list instead of touching the real filesystem (mirrors
    /// [`crate::resources::scan_resource_packs_in`]'s own directory-argument
    /// split, one level up). A previously-selected pack keeps its selection
    /// across a rebuild if it is still present (matched by id); one that
    /// disappeared from the folder is simply dropped, mirroring
    /// [`super::packs::PacksNav::rebuild`]'s own "no longer on disk" rule.
    /// Resets scroll and cursor: a rebuild only ever happens on (re-)opening
    /// this sub-screen, never mid-scroll.
    fn rebuild(&mut self, discovered: Vec<crate::resources::DiscoveredPack>) {
        let previously_selected: std::collections::HashSet<String> =
            self.rows.iter().filter(|r| r.selected).filter_map(|r| r.id.clone()).collect();
        let mut rows = vec![DataPackRow { id: None, title: "Vanilla".to_string(), selected: true }];
        for pack in discovered {
            rows.push(DataPackRow {
                selected: previously_selected.contains(&pack.id),
                id: Some(pack.id),
                title: pack.title,
            });
        }
        self.rows = rows;
        self.scroll = 0.0;
        self.cursor = 0;
    }

    /// Scans [`crate::resources::data_packs_dir`] for real and rebuilds from
    /// it.
    fn refresh(&mut self) {
        self.rebuild(crate::resources::scan_resource_packs_in(&crate::resources::data_packs_dir()));
    }

    /// The scroll offset, in pixels.
    #[must_use]
    fn scroll(&self) -> f32 {
        self.scroll
    }

    /// How many rows are currently listed, Vanilla included.
    #[must_use]
    fn len(&self) -> usize {
        self.rows.len()
    }

    /// Extra pack ids the player selected, in scan order — mirrors
    /// [`GameRulesEditor::changed_entries`]'s "only report what changed"
    /// shape: the always-on Vanilla row is never included, since there is
    /// nothing to send that is not already every world's own default.
    #[must_use]
    pub fn selected_ids(&self) -> Vec<String> {
        self.rows.iter().filter(|r| r.selected).filter_map(|r| r.id.clone()).collect()
    }

    /// The live [`super::widget::ScrollList`] at this canvas height, or
    /// `None` when there is nothing to scroll — mirrors
    /// [`GameRulesEditor::model`].
    fn model(&self, canvas_height: f32) -> Option<super::widget::ScrollList> {
        data_packs_list_spec(self.rows.len(), self.scroll).model(canvas_height)
    }

    /// One mouse-wheel notch, through the shared primitive.
    fn scroll_by(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) = self.model(canvas_height) else {
            return;
        };
        list.mouse_scrolled(notches);
        self.scroll = list.scroll();
    }

    #[must_use]
    fn visible(&self) -> Vec<DataPackControlView> {
        data_pack_controls(&self.rows, self.scroll)
    }

    /// The cursor's position within [`Self::visible`], for the highlight —
    /// mirrors [`GameRulesEditor::selected_row`].
    #[must_use]
    fn selected_row(&self) -> Option<usize> {
        let all = all_data_pack_controls(self.rows.len());
        let control = *all.get(self.cursor)?;
        self.visible().iter().position(|c| c.control == control)
    }

    /// The mouse moved over visible row `row` — moves the cursor there.
    fn hover_row(&mut self, row: usize) {
        let visible = self.visible();
        let Some(view) = visible.get(row).copied() else {
            return;
        };
        let all = all_data_pack_controls(self.rows.len());
        if let Some(i) = all.iter().position(|&c| c == view.control) {
            self.cursor = i;
        }
    }

    /// A click on visible row `row`. Returns `true` when Done was pressed —
    /// [`CreateWorldNav::click_row`]'s cue to leave the editor.
    fn click_row(&mut self, row: usize) -> bool {
        self.hover_row(row);
        let visible = self.visible();
        let Some(view) = visible.get(row).copied() else {
            return false;
        };
        match view.control {
            DataPackControl::Toggle(index) => {
                if let Some(r) = self.rows.get_mut(index) {
                    // Vanilla (`id: None`) is always active — the row exists
                    // so a player can see it is on, not so they can turn it
                    // off.
                    if r.id.is_some() {
                        r.selected = !r.selected;
                    }
                }
                false
            }
            DataPackControl::Done => true,
        }
    }
}

/// Builds the Data Packs sub-screen's whole frame — [`frame`]'s
/// [`CreateWorldMode::DataPacks`] branch. Mirrors [`game_rules_frame`], minus
/// the separate `list_labels` pass: a Data Packs row is a single toggle
/// button whose own label already carries the pack's name and state, unlike
/// a Game Rules row's `-`/`+` pair plus a *separate* name label.
#[must_use]
fn data_packs_frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
    let editor = &nav.data_packs;
    let visible = editor.visible();
    let selected = editor.selected_row();

    let rows: Vec<MenuRow> = visible
        .iter()
        .map(|view| MenuRow {
            label: match view.control {
                DataPackControl::Toggle(index) => editor
                    .rows
                    .get(index)
                    .map_or_else(String::new, |r| toggle_label(&r.title, r.selected)),
                DataPackControl::Done => "Done".to_string(),
            },
            enabled: true,
            slot: Some(view.slot),
            ..Default::default()
        })
        .collect();

    MenuFrame {
        title: "Data Packs",
        rows,
        selected: selected.unwrap_or(usize::MAX),
        vanilla: true,
        labels: vec![MenuLabel {
            text: "Data Packs".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 12.0,
            align: Align::Centre,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        }],
        // Deliberately not set here — mirrors `game_rules_frame`'s own
        // comment: `render::dispatch` stamps `f.list` on every frame.
        ..Default::default()
    }
}

/// The three real vanilla feature flags a 26.2 world can enable
/// (vanilla's own three registered experimental flags, beyond the always-on
/// `vanilla` flag, which carries no UI at all). Vanilla's own
/// enabled-features field is exactly this set, written into a freshly
/// created world's `level.dat`, never sent over any network packet — see
/// [`WorldCreationConfig::experiments`]'s own doc for where that write
/// actually happens and what is still unbuilt past it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperimentFlag {
    TradeRebalance,
    RedstoneExperiments,
    MinecartImprovements,
}

impl ExperimentFlag {
    pub const ALL: [ExperimentFlag; 3] =
        [Self::TradeRebalance, Self::RedstoneExperiments, Self::MinecartImprovements];

    /// The bare registration id (vanilla's own flag-registration argument,
    /// no `minecraft:` namespace) —
    /// vanilla's own wire/NBT shape for `enabled_features`.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::TradeRebalance => "trade_rebalance",
            Self::RedstoneExperiments => "redstone_experiments",
            Self::MinecartImprovements => "minecart_improvements",
        }
    }

    /// `dataPack.<id>.name`, verbatim from `en_us.json` — vanilla's real
    /// `ExperimentsScreen` is a `PackSelectionScreen` over these three
    /// specifically as "feature flag" packs, so it borrows the data-pack
    /// translation keys rather than having its own.
    #[must_use]
    pub fn caption(self) -> &'static str {
        match self {
            Self::TradeRebalance => "Villager Trade Rebalance",
            Self::RedstoneExperiments => "Redstone Experiments",
            Self::MinecartImprovements => "Minecart Improvements",
        }
    }
}

/// One flag's live toggle state, on the Experiments sub-screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExperimentRow {
    flag: ExperimentFlag,
    enabled: bool,
}

/// The Experiments sub-screen's own live state — three fixed
/// toggle rows plus Done, the shape [`DataPacksEditor`] takes for a scanned
/// list, simplified: the flag set is fixed
/// ([`ExperimentFlag::ALL`]), so there is no scan, and — four rows total,
/// always fewer than fit on screen — no scroll state either, unlike
/// [`DataPacksEditor`]/[`GameRulesEditor`]. Every flag defaults off,
/// matching vanilla's own default feature set (no experimental
/// flag is on by default).
#[derive(Debug, Clone, PartialEq)]
pub struct ExperimentsEditor {
    rows: [ExperimentRow; 3],
    cursor: usize,
}

impl ExperimentsEditor {
    fn new() -> Self {
        Self {
            rows: ExperimentFlag::ALL.map(|flag| ExperimentRow { flag, enabled: false }),
            cursor: 0,
        }
    }

    /// Every flag the player turned on, in [`ExperimentFlag::ALL`] order —
    /// mirrors [`DataPacksEditor::selected_ids`]'s "only report what's on"
    /// shape: an untouched screen sends nothing, matching
    /// [`WorldCreationConfig::game_rules`]'s own "send nothing rather than a
    /// no-op" rule.
    #[must_use]
    fn enabled_ids(&self) -> Vec<String> {
        self.rows.iter().filter(|r| r.enabled).map(|r| r.flag.id().to_string()).collect()
    }

    /// The mouse moved over visible row `row` — moves the cursor there.
    fn hover_row(&mut self, row: usize) {
        if row < ALL_EXPERIMENT_CONTROLS.len() {
            self.cursor = row;
        }
    }

    /// A click on visible row `row`. Returns `true` when Done was pressed —
    /// [`CreateWorldNav::click_row`]'s cue to leave the editor.
    fn click_row(&mut self, row: usize) -> bool {
        self.hover_row(row);
        match ALL_EXPERIMENT_CONTROLS.get(row) {
            Some(ExperimentControl::Toggle(index)) => {
                if let Some(r) = self.rows.get_mut(*index) {
                    r.enabled = !r.enabled;
                }
                false
            }
            Some(ExperimentControl::Done) => true,
            None => false,
        }
    }
}

/// One control on the Experiments sub-screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExperimentControl {
    /// Toggle flag `usize` (an index into [`ExperimentFlag::ALL`]).
    Toggle(usize),
    /// Leave the editor, keeping every toggle made so far.
    Done,
}

/// Every control, in display order — mirrors [`all_data_pack_controls`],
/// fixed-length since [`ExperimentsEditor`] never scans anything.
const ALL_EXPERIMENT_CONTROLS: [ExperimentControl; 4] = [
    ExperimentControl::Toggle(0),
    ExperimentControl::Toggle(1),
    ExperimentControl::Toggle(2),
    ExperimentControl::Done,
];

/// This screen's row height — matches [`DATA_PACK_ROW_H`]/[`GAME_RULE_ROW_H`].
const EXPERIMENT_ROW_H: f32 = 24.0;
/// This screen's row band — matches [`DATA_PACK_ROW_WIDTH`]/[`GAME_RULE_ROW_WIDTH`].
const EXPERIMENT_ROW_W: f32 = 280.0;

/// One control's `Slot`, stacked top-to-bottom with no scroll — the same
/// `Origin::ScreenTop` + fixed `dy` shape [`row_slot`] uses for the main
/// tabs' own rows, simplified further since every row here shares one `dx`
/// (centred) rather than pairing two tabs' worth of fields at each `dy`. No
/// [`super::widget::ListSpec`]/scrollbar involved at all — see
/// [`ExperimentsEditor`]'s own doc for why a fixed four rows never needs one.
#[must_use]
fn experiment_control_slot(index: usize) -> Slot {
    const TOP: f32 = layout::TAB_BAR_HEIGHT + 32.0;
    Slot {
        origin: Origin::ScreenTop,
        dx: -(EXPERIMENT_ROW_W / 2.0),
        dy: TOP + EXPERIMENT_ROW_H * index as f32,
        w: EXPERIMENT_ROW_W,
        h: super::render::EDIT_BOX_H,
    }
}

/// Builds the Experiments sub-screen's whole frame — [`frame`]'s
/// [`CreateWorldMode::Experiments`] branch. Mirrors [`data_packs_frame`],
/// simplified to the fixed row count [`ExperimentsEditor`]'s own doc
/// explains: every control's `Slot` comes straight from
/// [`experiment_control_slot`] rather than through a scrollable list model.
#[must_use]
fn experiments_frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
    let editor = &nav.experiments;
    let rows: Vec<MenuRow> = ALL_EXPERIMENT_CONTROLS
        .iter()
        .enumerate()
        .map(|(i, control)| MenuRow {
            label: match control {
                ExperimentControl::Toggle(index) => editor
                    .rows
                    .get(*index)
                    .map_or_else(String::new, |r| toggle_label(r.flag.caption(), r.enabled)),
                ExperimentControl::Done => "Done".to_string(),
            },
            enabled: true,
            slot: Some(experiment_control_slot(i)),
            ..Default::default()
        })
        .collect();

    MenuFrame {
        title: "Experiments",
        rows,
        selected: editor.cursor,
        vanilla: true,
        labels: vec![MenuLabel {
            text: "Experiments".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 12.0,
            align: Align::Centre,
            colour: super::widget::ACTIVE_LABEL,
            scale: 1.0,
        }],
        // Deliberately not set here — mirrors `data_packs_frame`'s own
        // comment: `render::dispatch` stamps `f.list` on every frame.
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test convenience: activate focus id `id` the way a player does —
    /// switch to whichever tab holds it (a no-op if it is already showing,
    /// and a no-op for `CREATE_ROW`/`CANCEL_ROW`, which belong to no tab),
    /// then click its resolved frame row. Keeps the tests below reading by
    /// **focus id**, which is what they are actually about, without hand-
    /// deriving a frame-row index at every call site — see the module docs'
    /// "two index spaces" section for why the two differ at all.
    impl CreateWorldNav {
        fn click_focus(&mut self, id: usize) -> CreateWorldOutcome {
            if let Some(tab) = (0..TAB_LABELS.len()).find(|&t| content_rows_for_tab(t).contains(&id)) {
                self.switch_tab(tab);
            }
            let row = self
                .frame_row_for_focus_id(id)
                .unwrap_or_else(|| panic!("focus id {id} has no frame row on tab {}", self.active_tab));
            self.click_row(row)
        }

        fn hover_focus(&mut self, id: usize) {
            if let Some(tab) = (0..TAB_LABELS.len()).find(|&t| content_rows_for_tab(t).contains(&id)) {
                self.switch_tab(tab);
            }
            let row = self
                .frame_row_for_focus_id(id)
                .unwrap_or_else(|| panic!("focus id {id} has no frame row on tab {}", self.active_tab));
            self.hover_row(row);
        }
    }

    #[test]
    fn defaults_match_vanillas_own() {
        let config = WorldCreationConfig::default();
        assert_eq!(config.name, "New World");
        assert_eq!(config.seed, "");
        assert_eq!(config.world_type, WorldTypePreset::Normal);
        assert_eq!(config.game_mode, WorldGameMode::Survival);
        assert_eq!(config.difficulty, WorldDifficulty::Normal);
        assert!(config.generate_structures);
        assert!(!config.bonus_chest);
        assert!(!config.allow_cheats);
        assert!(!config.online_mode);
    }

    #[test]
    fn a_fresh_nav_starts_on_the_game_tab_focused_on_the_name_field() {
        let nav = CreateWorldNav::new();
        assert_eq!(nav.active_tab(), GAME_TAB);
        assert_eq!(nav.focused(), Some(NAME_FIELD));
        assert_eq!(nav.widgets.name.value(), "New World");
        assert_eq!(nav.widgets.seed.value(), "");
    }

    #[test]
    fn typing_reaches_the_focused_field_and_the_seed_field_lives_on_the_world_tab() {
        let mut nav = CreateWorldNav::new();
        // Clear the default and type a real name, on the Game tab.
        for _ in 0.."New World".len() {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "My World".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(nav.widgets.name.value(), "My World");

        // Tab from Name must **not** land on Seed — the two are on different
        // tabs now (a real vanilla divergence: `GameTab` and `WorldTab` are
        // different `Screen` children in the real client too, so a keyboard
        // Tab never crossed between them there either). It lands on this
        // tab's next control instead.
        nav.handle_key(MenuKey::Tab);
        assert_eq!(
            nav.focused(),
            Some(GAME_MODE_ROW),
            "Tab must stay within the Game tab's own fields"
        );
        assert_eq!(nav.widgets.seed.value(), "", "Seed must be untouched — it was never reached");

        // Reaching Seed is a tab click, then a field click — exactly what a
        // player does.
        nav.click_focus(SEED_FIELD);
        assert_eq!(nav.active_tab(), WORLD_TAB);
        for ch in "12345".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(nav.widgets.seed.value(), "12345");
        // The name field must be untouched by typing into the seed field.
        assert_eq!(nav.widgets.name.value(), "My World");
    }

    #[test]
    fn game_mode_cycles_through_all_three_and_wraps() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.config().game_mode, WorldGameMode::Survival);
        nav.click_focus(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Creative);
        nav.click_focus(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Hardcore);
        nav.click_focus(GAME_MODE_ROW);
        assert_eq!(nav.config().game_mode, WorldGameMode::Survival, "wraps");
    }

    #[test]
    fn selecting_hardcore_locks_difficulty_to_hard_and_disables_its_row() {
        let mut nav = CreateWorldNav::new();
        // Move difficulty to a value that is *not* Hard first, so the
        // "forced" assertion below is meaningful — it would fail if
        // `apply_hardcore_lock` did nothing, rather than passing by
        // coincidence because the default already happened to be Hard.
        nav.click_focus(DIFFICULTY_ROW); // Normal -> Hard
        nav.click_focus(DIFFICULTY_ROW); // Hard -> Peaceful (wraps)
        nav.click_focus(DIFFICULTY_ROW); // Peaceful -> Easy
        assert_eq!(nav.config().difficulty, WorldDifficulty::Easy);

        nav.click_focus(GAME_MODE_ROW); // Survival -> Creative
        nav.click_focus(GAME_MODE_ROW); // Creative -> Hardcore
        assert_eq!(nav.config().difficulty, WorldDifficulty::Hard, "forced");
        assert!(!nav.widgets.difficulty.active, "row must be inactive while locked");

        // Clicking a disabled row does nothing — the same rule every other
        // present-and-disabled control in this tree follows.
        nav.click_focus(DIFFICULTY_ROW);
        assert_eq!(nav.config().difficulty, WorldDifficulty::Hard, "unchanged");

        // Leaving Hardcore unlocks it again, at whatever it was left on.
        nav.click_focus(GAME_MODE_ROW); // Hardcore -> Survival
        assert!(nav.widgets.difficulty.active, "unlocked outside Hardcore");
    }

    #[test]
    fn the_three_toggles_flip_independently_across_two_tabs() {
        let mut nav = CreateWorldNav::new();
        assert!(nav.config().generate_structures);
        nav.click_focus(STRUCTURES_ROW); // World tab
        assert!(!nav.config().generate_structures);
        assert!(!nav.config().bonus_chest, "untouched");
        assert!(!nav.config().allow_cheats, "untouched");

        nav.click_focus(BONUS_CHEST_ROW); // World tab
        assert!(nav.config().bonus_chest);
        nav.click_focus(ALLOW_CHEATS_ROW); // Game tab — crosses back
        assert!(nav.config().allow_cheats);
        assert!(!nav.config().generate_structures, "still off from the first click");
    }

    /// Same shape as `the_three_toggles_flip_independently_across_two_tabs`,
    /// kept separate because `online_mode` is wired (see its own doc) rather
    /// than decorative like the other three — this is the pair the toggle's
    /// own gate needs: the default stays off, and clicking flips only this
    /// field.
    #[test]
    fn online_mode_defaults_off_and_toggles_independently() {
        let mut nav = CreateWorldNav::new();
        assert!(!nav.config().online_mode);
        nav.click_focus(ONLINE_MODE_ROW);
        assert!(nav.config().online_mode);
        assert!(!nav.config().allow_cheats, "neighbour untouched");
        assert!(!nav.config().bonus_chest, "neighbour untouched");
        assert!(nav.config().generate_structures, "neighbour untouched (default on)");

        nav.click_focus(ONLINE_MODE_ROW);
        assert!(!nav.config().online_mode, "toggles back off");
    }

    #[test]
    fn create_carries_the_typed_name_and_seed() {
        let mut nav = CreateWorldNav::new();
        for _ in 0.."New World".len() {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "Overworld".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        nav.click_focus(SEED_FIELD);
        for ch in "42".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected CreateWorldOutcome::Create, got {outcome:?}");
        };
        assert_eq!(config.name, "Overworld");
        assert_eq!(config.seed, "42");
        assert_eq!(nav.config().name, "Overworld");
        assert_eq!(nav.config().seed, "42");
    }

    #[test]
    fn cancel_and_escape_both_ask_to_leave() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.click_focus(CANCEL_ROW), CreateWorldOutcome::Cancel);

        let mut nav2 = CreateWorldNav::new();
        assert_eq!(nav2.handle_key(MenuKey::Escape), CreateWorldOutcome::Cancel);
    }

    #[test]
    fn a_click_acts_on_the_row_it_landed_on_and_nothing_else() {
        // The same failure shape as a prior click-routing defect, on this
        // screen too: clicking Structures must not touch Bonus Chest.
        let mut nav = CreateWorldNav::new();
        nav.click_focus(STRUCTURES_ROW);
        assert!(!nav.config().generate_structures);
        assert!(!nav.config().bonus_chest, "neighbour untouched");
    }

    #[test]
    fn the_name_and_seed_fields_reach_the_frame_as_real_edit_boxes() {
        // The island this fixes: `frame()` used to build its `rows` starting
        // at `GAME_MODE_ROW`, so neither field ever carried an `edit` and
        // `draw_edit_box` never ran for either of them — the boxes were
        // focusable and typeable in every test above, and invisible on
        // screen. Positive assertion on the fields, negative control on a
        // button row right next to them, per `CLAUDE.md`'s "a gate that only
        // checks the border exists would have passed while this bug shipped"
        // — the control here is the thing that would have caught it.
        let mut nav = CreateWorldNav::new();
        nav.widgets.seed.set_value("1234");
        let f = frame(&nav);
        // Game tab: three tab rows, then Name/GameMode/Difficulty/AllowCheats,
        // then Create/Cancel.
        assert_eq!(f.rows.len(), TAB_LABELS.len() + 4 + 2);
        let name_row = TAB_LABELS.len();
        assert!(f.rows[name_row].field, "the Name row is a text field");
        let name_edit = f.rows[name_row]
            .edit
            .as_ref()
            .expect("the name row must carry its EditBox, or nothing draws");
        assert_eq!(name_edit.value(), "New World");
        // The control: a button row must not spuriously carry one too, or
        // this assertion would be vacuously satisfied by every row.
        let game_mode_row = name_row + 1;
        assert!(
            f.rows[game_mode_row].edit.is_none(),
            "a button row must not carry an EditBox"
        );
        assert!(!f.rows[game_mode_row].field);

        // Seed lives on the World tab.
        nav.click_focus(SEED_FIELD);
        let f = frame(&nav);
        let seed_row = TAB_LABELS.len();
        assert!(f.rows[seed_row].field, "the Seed row is a text field");
        let seed_edit = f.rows[seed_row]
            .edit
            .as_ref()
            .expect("the seed row must carry its EditBox, or nothing draws");
        assert_eq!(seed_edit.value(), "1234");
    }

    #[test]
    fn both_fields_get_their_own_vanilla_label_on_their_own_tab_and_the_seed_hint_is_not_duplicated() {
        // Vanilla's own screen wraps each field in its labeled-element
        // layout helper — a real, drawn label, not
        // narration. Each is present on its own tab, and **absent** on the
        // other — the control that catches a label emitted unconditionally
        // regardless of which tab is showing.
        let mut nav = CreateWorldNav::new();
        let f = frame(&nav);
        assert!(
            f.labels.iter().any(|l| l.text == NAME_LABEL),
            "missing the name field's own label on the Game tab"
        );
        assert!(
            !f.labels.iter().any(|l| l.text == SEED_LABEL),
            "the seed label must not appear while the Game tab is showing"
        );

        nav.click_focus(SEED_FIELD);
        let f = frame(&nav);
        assert!(
            f.labels.iter().any(|l| l.text == SEED_LABEL),
            "missing the seed field's own label on the World tab — this used \
             to be absent entirely, with only the *hint* text drawn"
        );
        assert!(
            !f.labels.iter().any(|l| l.text == NAME_LABEL),
            "the name label must not appear while the World tab is showing"
        );
        // `SEED_INFO` must appear as the box's own hint, and *not* also as a
        // second, permanent label/notice — vanilla shows it in exactly one
        // place (its own hint field, conditional on empty+unfocused).
        assert_eq!(nav.widgets.seed.hint.as_deref(), Some(SEED_INFO));
        assert!(
            !f.labels.iter().any(|l| l.text == SEED_INFO),
            "the hint text must not also be drawn as a permanent label"
        );
        assert!(
            f.notice.is_none(),
            "no permanent notice either — that was the pre-fix duplicate"
        );
    }

    #[test]
    fn every_row_resolves_on_screen_at_the_smallest_canvas_on_every_tab() {
        let (w, h) = (
            crate::config::MIN_SCALED_WIDTH as f32,
            crate::config::MIN_SCALED_HEIGHT as f32,
        );
        // Collected across all three tabs and asserted once, per `CLAUDE.md`'s
        // "collect mismatches, do not assert inside the loop" — an `assert!`
        // per row would report only the *first* off-screen row, not every one.
        let mut offenders = Vec::new();
        for tab in 0..TAB_LABELS.len() {
            let mut nav = CreateWorldNav::new();
            nav.switch_tab(tab);
            for &row in content_rows_for_tab(tab)
                .iter()
                .chain([&CREATE_ROW, &CANCEL_ROW])
            {
                let (x, y, rw, rh) = row_slot(row).resolve(w, h);
                if !(x >= 0.0 && y >= 0.0 && x + rw <= w && y + rh <= h) {
                    offenders.push(format!(
                        "tab {tab} row {row} at ({x}, {y}) size {rw}x{rh} on {w}x{h}"
                    ));
                }
            }
        }
        assert!(offenders.is_empty(), "off-screen rows: {offenders:#?}");
    }

    #[test]
    fn the_footer_buttons_do_not_overlap_the_content_rows() {
        let (w, h) = (854.0, 480.0);
        // `ONLINE_MODE_ROW` is the deepest row of either tab (World's local
        // row 4; Game's four rows end one row shallower) — the one to check
        // against, since it is closest to the footer.
        let (_, content_bottom, _, ch) = row_slot(ONLINE_MODE_ROW).resolve(w, h);
        let (_, footer_y, _, _) = row_slot(CREATE_ROW).resolve(w, h);
        assert!(
            footer_y >= content_bottom + ch,
            "footer at {footer_y} must sit at or below the last content row's bottom {}",
            content_bottom + ch
        );
    }

    // -- the tab bar --------------------------------------

    #[test]
    fn the_frame_carries_three_real_clickable_tab_rows() {
        let nav = CreateWorldNav::new();
        let f = frame(&nav);
        for (index, &label) in TAB_LABELS.iter().enumerate() {
            let row = &f.rows[index];
            assert_eq!(row.label, label);
            let view = row.tab.expect("a tab-bar row must carry a TabEntryView");
            assert_eq!(view.index, index);
            assert_eq!(view.count, TAB_LABELS.len());
            assert_eq!(view.selected, index == GAME_TAB);
            // Unlike Statistics's Items/Mobs, every tab here is real — see
            // the module docs on why More is enabled with nothing under it.
            assert!(row.enabled, "{label} must be a real, clickable tab");
        }
    }

    #[test]
    fn clicking_a_tab_switches_active_tab_and_the_frames_content() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.click_row(WORLD_TAB), CreateWorldOutcome::Handled);
        assert_eq!(nav.active_tab(), WORLD_TAB);
        let f = frame(&nav);
        // World's first content row is Seed, a field — Game's own first row
        // (Name) must not still be present anywhere in `rows`.
        let first_content = &f.rows[TAB_LABELS.len()];
        assert!(first_content.field, "World's first row is the Seed field");
        assert!(
            !f.rows.iter().any(|r| r.label == NAME_LABEL),
            "Game's Name field must not appear while World is showing"
        );

        assert_eq!(nav.click_row(MORE_TAB), CreateWorldOutcome::Handled);
        assert_eq!(nav.active_tab(), MORE_TAB);
        let f = frame(&nav);
        assert_eq!(
            f.rows.len(),
            TAB_LABELS.len() + 5,
            "More has three content rows (Game Rules, Data Packs and \
             Experiments) plus the tab bar and the footer"
        );

        // Clicking the tab already showing is a no-op, not a crash and not a
        // focus reset.
        nav.click_row(GAME_TAB);
        nav.click_focus(GAME_MODE_ROW);
        let before = nav.focused();
        assert_eq!(nav.click_row(GAME_TAB), CreateWorldOutcome::Handled);
        assert_eq!(nav.focused(), before, "re-clicking the active tab must not move focus");
    }

    #[test]
    fn switching_tabs_moves_keyboard_focus_onto_the_new_tabs_first_field_or_clears_it() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.focused(), Some(NAME_FIELD), "premise");

        nav.click_row(WORLD_TAB);
        assert_eq!(nav.focused(), Some(SEED_FIELD), "World's first field takes focus");

        nav.click_row(MORE_TAB);
        assert_eq!(
            nav.focused(),
            Some(GAME_RULES_ROW),
            "More's first (and only) field, the Game Rules button, takes focus \
             (this tab used to have nothing at all)"
        );

        nav.click_row(GAME_TAB);
        assert_eq!(nav.focused(), Some(NAME_FIELD), "back to Game's first field");
    }

    #[test]
    fn a_field_inactive_on_another_tab_cannot_be_reached_by_tab_traversal() {
        // The control for `sync_tab_visibility`: with the Game tab showing,
        // repeatedly pressing Tab must never land on a World-tab-only id.
        let mut nav = CreateWorldNav::new();
        let world_only = [
            SEED_FIELD,
            WORLD_TYPE_ROW,
            STRUCTURES_ROW,
            BONUS_CHEST_ROW,
            ONLINE_MODE_ROW,
        ];
        for _ in 0..8 {
            nav.handle_key(MenuKey::Tab);
            if let Some(focused) = nav.focused() {
                assert!(
                    !world_only.contains(&focused),
                    "Tab traversal reached World-only id {focused} while the Game tab was showing"
                );
            }
        }
    }

    #[test]
    fn hovering_a_tab_row_records_nothing_hover_is_derived_from_the_cursor_at_draw_time() {
        // See `hover_row`'s own doc: a tab-bar row's hover comes from
        // `MenuFrame::cursor` at draw time, not from this bookkeeping — so
        // this must be a true no-op, not merely "does not crash".
        let mut nav = CreateWorldNav::new();
        nav.hover_row(GAME_TAB);
        assert_eq!(nav.hovered(), None);
        nav.hover_row(WORLD_TAB);
        assert_eq!(nav.hovered(), None);
    }

    // -- hover --------------------------------------------------

    #[test]
    fn a_fresh_screen_has_nothing_hovered_so_the_frame_carries_none() {
        let nav = CreateWorldNav::new();
        assert_eq!(nav.hovered(), None);
        assert_eq!(
            frame(&nav).hovered,
            None,
            "the frame must reflect the nav's own hover state, not invent one"
        );
    }

    #[test]
    fn hovering_a_button_row_reaches_the_frame() {
        let mut nav = CreateWorldNav::new();
        nav.hover_focus(CREATE_ROW);
        assert_eq!(nav.hovered(), Some(CREATE_ROW));
        let create_frame_row = nav.frame_row_for_focus_id(CREATE_ROW).unwrap();
        assert_eq!(
            frame(&nav).hovered,
            Some(create_frame_row),
            "MenuFrame::hovered must carry the row render::draw_widget outlines"
        );

        // A second hover replaces the first — only one row highlights at once.
        nav.hover_focus(CANCEL_ROW);
        assert_eq!(nav.hovered(), Some(CANCEL_ROW));
    }

    /// The control: hovering a **field** must not be recorded as a button
    /// hover, mirroring `EditForm::hover_row`'s exact reason — a text field
    /// has no hover highlight in vanilla, and if a field row were treated as
    /// a hoverable button the mouse passing over Name while Seed is focused
    /// would draw a spurious outline on a row that is not a button at all.
    #[test]
    fn hovering_a_field_row_does_nothing() {
        let mut nav = CreateWorldNav::new();
        nav.hover_focus(NAME_FIELD);
        assert_eq!(nav.hovered(), None, "a field row is not a hoverable button");

        nav.switch_tab(WORLD_TAB);
        nav.hover_focus(SEED_FIELD);
        assert_eq!(nav.hovered(), None);

        // And it must not clobber a real hover already recorded, the same
        // property `hover_row`'s own doc argues protects the *focused* field
        // from a stray mouse move — here it is the *hover* state's turn not
        // to be reset by passing over a field. Deliberately kept on **one**
        // tab throughout: switching tabs legitimately clears hover (the
        // previously-hovered row leaves `frame.rows` entirely — see
        // `switch_tab`'s own doc), which is a different property from the one
        // this test names, and `hover_focus`'s tab-following would otherwise
        // exercise that instead.
        nav.hover_focus(STRUCTURES_ROW);
        nav.hover_focus(SEED_FIELD);
        assert_eq!(
            nav.hovered(),
            Some(STRUCTURES_ROW),
            "moving the mouse back over a field must not clear a button's hover"
        );
    }

    /// Every button row (not the two fields) must be able to report hover —
    /// a gap here is exactly how this shipped once before: `hover_row` existed on
    /// `EditForm` and on other screens, but `CreateWorldNav` had no such
    /// method at all, so no row on this screen could ever be hovered.
    /// Collected across all seven and asserted once (not `assert!` inside the
    /// loop), so a single broken row is not the only one this test can ever
    /// report.
    #[test]
    fn every_button_row_can_be_hovered() {
        let mut offenders = Vec::new();
        for row in [
            GAME_MODE_ROW,
            DIFFICULTY_ROW,
            STRUCTURES_ROW,
            BONUS_CHEST_ROW,
            ALLOW_CHEATS_ROW,
            ONLINE_MODE_ROW,
            WORLD_TYPE_ROW,
            CREATE_ROW,
            CANCEL_ROW,
        ] {
            let mut nav = CreateWorldNav::new();
            nav.hover_focus(row);
            if nav.hovered() != Some(row) {
                offenders.push(row);
            }
        }
        assert!(offenders.is_empty(), "rows that did not record hover: {offenders:?}");
    }

    // -- world type (UI half) -----------------------------------

    #[test]
    fn world_type_defaults_to_normal_and_cycles_through_all_seven() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.config().world_type, WorldTypePreset::Normal);
        let order = [
            WorldTypePreset::LargeBiomes,
            WorldTypePreset::Amplified,
            WorldTypePreset::SingleBiomeSurface,
            WorldTypePreset::Flat,
            WorldTypePreset::FlatAllDimensions,
            WorldTypePreset::DebugAllBlockStates,
            WorldTypePreset::Normal, // wraps
        ];
        for expect in order {
            nav.click_focus(WORLD_TYPE_ROW);
            assert_eq!(nav.config().world_type, expect);
        }
    }

    #[test]
    fn every_preset_now_reports_wired() {
        // Used to assert exactly the three presets needing no
        // `lodestone-server` change (`overworld_chunk_source_of_type` already
        // `pub` at that crate's root) reported true and the other four
        // reported false. A later change closed that gap: `lib.rs` now
        // re-exports `single_biome_chunk_source`/`flat_chunk_source`/
        // `debug_chunk_source` and `net.rs`'s `preset_chunk_source` covers
        // all seven, so `is_backend_wired` is unconditionally `true` — see
        // its own doc for why the predicate is kept rather than deleted.
        // Collected, not asserted inside the loop, so a regression is still
        // fully reported rather than stopping at the first wrong preset.
        let unwired: Vec<WorldTypePreset> = [
            WorldTypePreset::Normal,
            WorldTypePreset::LargeBiomes,
            WorldTypePreset::Amplified,
            WorldTypePreset::SingleBiomeSurface,
            WorldTypePreset::Flat,
            WorldTypePreset::FlatAllDimensions,
            WorldTypePreset::DebugAllBlockStates,
        ]
        .into_iter()
        .filter(|p| !p.is_backend_wired())
        .collect();
        assert!(unwired.is_empty(), "preset(s) reporting unwired: {unwired:?}");
    }

    /// [`WorldTypePreset::backend_world_type`]'s exact mapping — the value a
    /// caller passes once [`WorldTypePreset::is_backend_wired`] says yes.
    /// Predicted per-variant rather than merely checking the two wired,
    /// non-`Overworld` arms differ from the fallback: the four unwired
    /// presets falling back to `Overworld` is as load-bearing as the three
    /// wired arms landing on their own variant — a caller that skipped the
    /// `is_backend_wired` check must still get a real, playable world
    /// (`Overworld`) rather than a panic or a nonsense generator.
    #[test]
    fn backend_world_type_maps_each_preset_to_its_measured_generator() {
        use lodestone_server::WorldType;
        let cases = [
            (WorldTypePreset::Normal, WorldType::Overworld),
            (WorldTypePreset::LargeBiomes, WorldType::LargeBiomes),
            (WorldTypePreset::Amplified, WorldType::Amplified),
            (WorldTypePreset::SingleBiomeSurface, WorldType::Overworld),
            (WorldTypePreset::Flat, WorldType::Overworld),
            (WorldTypePreset::FlatAllDimensions, WorldType::Overworld),
            (WorldTypePreset::DebugAllBlockStates, WorldType::Overworld),
        ];
        let wrong: Vec<(WorldTypePreset, WorldType)> = cases
            .into_iter()
            .filter(|&(preset, expected)| preset.backend_world_type() != expected)
            .collect();
        assert!(wrong.is_empty(), "wrong mapping(s): {wrong:?}");
    }

    #[test]
    fn world_type_only_reachable_on_the_world_tab() {
        let mut nav = CreateWorldNav::new();
        assert_eq!(nav.active_tab(), GAME_TAB, "premise");
        nav.click_focus(WORLD_TYPE_ROW);
        assert_eq!(
            nav.active_tab(),
            WORLD_TAB,
            "clicking World Type must switch to the tab that holds it"
        );
    }

    // -- Game Rules sub-screen (More tab) -----------------------

    fn rule_index(name: &str) -> usize {
        GAME_RULES
            .iter()
            .position(|r| r.name == name)
            .unwrap_or_else(|| panic!("{name} is not in GAME_RULES"))
    }

    #[test]
    fn game_rules_button_opens_the_editor_and_lists_every_rule() {
        let mut nav = CreateWorldNav::new();
        assert!(!nav.game_rules_open(), "premise: starts closed");
        assert_eq!(nav.click_focus(GAME_RULES_ROW), CreateWorldOutcome::Handled);
        assert!(nav.game_rules_open(), "clicking Game Rules must open the editor");
        let f = frame(&nav);
        assert_eq!(
            f.rows.len(),
            GAME_RULES.len() * 2 + 1,
            "a -/+ pair per rule plus one Done button — a shorter list would \
             mean a rule GAME_RULES carries never reached a control"
        );
        assert_eq!(
            f.list_labels.len(),
            GAME_RULES.len(),
            "one name label per rule"
        );
    }

    #[test]
    fn clicking_plus_then_minus_on_a_boolean_rule_round_trips_through_changed_entries() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(GAME_RULES_ROW);
        let index = rule_index("keep_inventory"); // default false
        assert!(
            nav.game_rules.changed_entries().is_empty(),
            "premise: nothing touched yet"
        );
        let plus_row = nav
            .game_rules
            .visible()
            .iter()
            .position(|v| v.control == GameRuleControl::Plus(index))
            .expect("keep_inventory's Plus button is visible at scroll 0");
        assert_eq!(nav.click_row(plus_row), CreateWorldOutcome::Handled);
        assert_eq!(
            nav.game_rules.changed_entries(),
            vec![(
                lodestone_model::Identifier::new("minecraft", "keep_inventory").unwrap(),
                "true".to_string()
            )],
            "flipping the rule to non-default must appear in the diff, keyed \
             on its namespaced identifier"
        );
        let minus_row = nav
            .game_rules
            .visible()
            .iter()
            .position(|v| v.control == GameRuleControl::Minus(index))
            .expect("keep_inventory's Minus button is visible at scroll 0");
        nav.click_row(minus_row);
        assert!(
            nav.game_rules.changed_entries().is_empty(),
            "back at the default, the diff must go empty again rather than \
             recording a no-op override"
        );
    }

    #[test]
    fn an_integer_rule_clamps_at_its_declared_minimum_rather_than_wrapping_negative() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(GAME_RULES_ROW);
        let index = rule_index("random_tick_speed"); // default 3, min 0
        for _ in 0..10 {
            let row = nav
                .game_rules
                .visible()
                .iter()
                .position(|v| v.control == GameRuleControl::Minus(index))
                .unwrap();
            nav.click_row(row);
        }
        assert_eq!(
            nav.game_rules.value(index),
            GameRuleValue::Int(0),
            "ten decrements past the default of 3 must clamp at the declared \
             minimum (0), never go negative or wrap"
        );
    }

    #[test]
    fn escape_from_the_game_rules_editor_returns_to_more_tab_not_a_full_cancel() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(GAME_RULES_ROW);
        assert!(nav.game_rules_open(), "premise");
        assert_eq!(
            nav.handle_key(MenuKey::Escape),
            CreateWorldOutcome::Handled,
            "must not unwind the whole Create New World screen"
        );
        assert!(
            !nav.game_rules_open(),
            "escape must close only the editor, back to the tabs"
        );
        assert_eq!(nav.active_tab(), MORE_TAB, "left on the tab it was opened from");
    }

    #[test]
    fn create_after_editing_game_rules_carries_exactly_the_changed_entries() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(GAME_RULES_ROW);
        let index = rule_index("keep_inventory");
        let plus_row = nav
            .game_rules
            .visible()
            .iter()
            .position(|v| v.control == GameRuleControl::Plus(index))
            .unwrap();
        nav.click_row(plus_row);
        // Done is always the last control.
        let done_row = nav.game_rules.visible().len() - 1;
        assert_eq!(nav.click_row(done_row), CreateWorldOutcome::Handled);
        assert!(!nav.game_rules_open(), "Done must leave the editor");
        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected Create, got {outcome:?}");
        };
        assert_eq!(
            config.game_rules,
            vec![(
                lodestone_model::Identifier::new("minecraft", "keep_inventory").unwrap(),
                "true".to_string()
            )],
            "Create must capture the editor's diff at press time, not an \
             empty default"
        );
    }

    // -- Data Packs sub-screen (More tab) -----------------------

    /// A [`crate::resources::DiscoveredPack`] fixture — mirrors
    /// `packs::tests::pack`, so a test never touches the real filesystem: the
    /// path is deliberately nonexistent.
    fn discovered_pack(name: &str) -> crate::resources::DiscoveredPack {
        crate::resources::DiscoveredPack {
            id: format!("file/{name}"),
            title: name.to_string(),
            description: String::new(),
            pack_format: 64,
            icon: None,
            path: std::path::PathBuf::from("/nonexistent").join(name),
            kind: crate::resources::PackKind::Directory,
        }
    }

    #[test]
    fn opening_the_editor_scans_for_real_and_lists_vanilla_alone_with_nothing_installed() {
        // Exercises the real `DATA_PACKS_ROW` -> `activate` -> `refresh` path
        // (a live, empty-safe scan of `crate::resources::data_packs_dir` —
        // nothing this dev/CI machine's own data directory would ever
        // contain), not `rebuild` directly, so the wiring from click to scan
        // is what is under test here — the fixture-injection tests below
        // exercise the pure list logic instead.
        let mut nav = CreateWorldNav::new();
        assert!(!nav.data_packs_open(), "premise: starts closed");
        assert_eq!(nav.click_focus(DATA_PACKS_ROW), CreateWorldOutcome::Handled);
        assert!(nav.data_packs_open(), "clicking Data Packs must open the editor");
        assert_eq!(nav.data_packs.rows.len(), 1, "Vanilla alone, nothing installed");
        assert_eq!(nav.data_packs.rows[0].title, "Vanilla");
        assert!(nav.data_packs.rows[0].selected, "Vanilla starts selected");
        assert!(
            nav.data_packs.selected_ids().is_empty(),
            "Vanilla itself is never reported as a selected id — there is \
             nothing to send that is not already every world's own default"
        );
    }

    #[test]
    fn a_discovered_pack_starts_unselected_and_toggles_on_click() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        nav.data_packs.rebuild(vec![discovered_pack("cool_pack")]);
        assert_eq!(nav.data_packs.rows.len(), 2, "Vanilla plus the one fixture");
        assert!(nav.data_packs.selected_ids().is_empty(), "premise: not yet selected");

        let row = nav
            .data_packs
            .visible()
            .iter()
            .position(|v| v.control == DataPackControl::Toggle(1))
            .expect("the discovered pack's row is visible");
        assert_eq!(nav.click_row(row), CreateWorldOutcome::Handled);
        assert_eq!(nav.data_packs.selected_ids(), vec!["file/cool_pack".to_string()]);

        // And back off — the same row, re-resolved (a selection does not
        // reorder or remove the row in this single-list shape).
        let row = nav
            .data_packs
            .visible()
            .iter()
            .position(|v| v.control == DataPackControl::Toggle(1))
            .unwrap();
        nav.click_row(row);
        assert!(nav.data_packs.selected_ids().is_empty(), "toggles back off");
    }

    #[test]
    fn vanillas_row_cannot_be_deselected() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        let row = nav
            .data_packs
            .visible()
            .iter()
            .position(|v| v.control == DataPackControl::Toggle(0))
            .expect("Vanilla's own row is visible");
        assert_eq!(nav.click_row(row), CreateWorldOutcome::Handled);
        assert!(
            nav.data_packs.rows[0].selected,
            "clicking Vanilla's row must not deselect it"
        );
    }

    #[test]
    fn a_rebuild_keeps_a_still_present_selection_and_drops_a_removed_one() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        nav.data_packs.rebuild(vec![discovered_pack("alpha"), discovered_pack("bravo")]);
        let alpha_row = nav
            .data_packs
            .visible()
            .iter()
            .position(|v| v.control == DataPackControl::Toggle(1))
            .unwrap();
        nav.click_row(alpha_row);
        assert_eq!(nav.data_packs.selected_ids(), vec!["file/alpha".to_string()]);

        // A re-scan that still has `alpha` (still selected) but drops
        // `bravo` (never selected, and now gone).
        nav.data_packs.rebuild(vec![discovered_pack("alpha")]);
        assert_eq!(
            nav.data_packs.selected_ids(),
            vec!["file/alpha".to_string()],
            "a still-present selection must survive a rebuild"
        );
        assert_eq!(nav.data_packs.rows.len(), 2, "Vanilla plus the one remaining pack");
    }

    #[test]
    fn escape_from_the_data_packs_editor_returns_to_more_tab_not_a_full_cancel() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        assert!(nav.data_packs_open(), "premise");
        assert_eq!(
            nav.handle_key(MenuKey::Escape),
            CreateWorldOutcome::Handled,
            "must not unwind the whole Create New World screen"
        );
        assert!(!nav.data_packs_open(), "escape must close only the editor, back to the tabs");
        assert_eq!(nav.active_tab(), MORE_TAB, "left on the tab it was opened from");
    }

    #[test]
    fn create_after_selecting_a_data_pack_carries_exactly_the_selected_extra_ids() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        nav.data_packs.rebuild(vec![discovered_pack("cool_pack")]);
        let row = nav
            .data_packs
            .visible()
            .iter()
            .position(|v| v.control == DataPackControl::Toggle(1))
            .unwrap();
        nav.click_row(row);
        // Done is always the last control.
        let done_row = nav.data_packs.visible().len() - 1;
        assert_eq!(nav.click_row(done_row), CreateWorldOutcome::Handled);
        assert!(!nav.data_packs_open(), "Done must leave the editor");
        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected Create, got {outcome:?}");
        };
        assert_eq!(
            config.data_packs,
            vec!["file/cool_pack".to_string()],
            "Create must capture the editor's selection at press time, not \
             an empty default"
        );
    }

    /// The control for `frame`'s dispatch: the Data Packs frame must
    /// actually carry rows drawn from the editor's own state, not a stale or
    /// empty placeholder — the exact shape `draw_tab` shipped as dead code
    /// for (`CLAUDE.md`'s own recorded incident): a frame that looks right in
    /// isolation but that nothing routes real rows into.
    #[test]
    fn the_data_packs_frame_carries_one_row_per_pack_plus_done() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(DATA_PACKS_ROW);
        nav.data_packs.rebuild(vec![discovered_pack("alpha"), discovered_pack("bravo")]);
        let f = frame(&nav);
        assert_eq!(f.rows.len(), 3 + 1, "Vanilla + two packs + Done");
        assert_eq!(f.title, "Data Packs");
        assert!(
            f.rows.iter().any(|r| r.label.starts_with("Vanilla")),
            "Vanilla's row must reach the frame"
        );
        assert!(
            f.rows.iter().any(|r| r.label.starts_with("alpha")),
            "a discovered pack's row must reach the frame"
        );
    }

    // -- Experiments --------------------------------------------

    #[test]
    fn an_untouched_experiments_screen_sends_nothing_on_create() {
        let mut nav = CreateWorldNav::new();
        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected Create, got {outcome:?}");
        };
        assert!(
            config.experiments.is_empty(),
            "no experiment was ever opened or toggled, so nothing should be sent"
        );
    }

    #[test]
    fn create_after_toggling_an_experiment_carries_exactly_the_enabled_flag_id() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(EXPERIMENTS_ROW);
        assert!(nav.experiments_open(), "premise");
        // Toggle(1) == RedstoneExperiments (see `ExperimentFlag::ALL`).
        nav.click_row(1);
        // Done is always the last control.
        let done_row = ALL_EXPERIMENT_CONTROLS.len() - 1;
        assert_eq!(nav.click_row(done_row), CreateWorldOutcome::Handled);
        assert!(!nav.experiments_open(), "Done must leave the editor");

        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected Create, got {outcome:?}");
        };
        assert_eq!(
            config.experiments,
            vec!["redstone_experiments".to_string()],
            "Create must capture exactly the one toggled flag, not all three \
             and not none"
        );
    }

    /// Toggling twice returns to the vanilla default (off) and sends nothing
    /// — the discriminating control for the toggle above: a resolver that
    /// always sent *something* once the screen was opened would fail this.
    #[test]
    fn toggling_an_experiment_twice_sends_nothing() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(EXPERIMENTS_ROW);
        nav.click_row(0);
        nav.click_row(0);
        let done_row = ALL_EXPERIMENT_CONTROLS.len() - 1;
        nav.click_row(done_row);
        let outcome = nav.click_focus(CREATE_ROW);
        let CreateWorldOutcome::Create(config) = outcome else {
            panic!("expected Create, got {outcome:?}");
        };
        assert!(config.experiments.is_empty(), "back to off must send nothing, same as never opened");
    }

    #[test]
    fn escape_from_the_experiments_editor_returns_to_more_tab_not_a_full_cancel() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(EXPERIMENTS_ROW);
        assert!(nav.experiments_open(), "premise");
        assert_eq!(
            nav.handle_key(MenuKey::Escape),
            CreateWorldOutcome::Handled,
            "must not unwind the whole Create New World screen"
        );
        assert!(!nav.experiments_open(), "escape must close only the editor, back to the tabs");
        assert_eq!(nav.active_tab(), MORE_TAB, "left on the tab it was opened from");
    }

    /// The control for `frame`'s dispatch — same reasoning
    /// `the_data_packs_frame_carries_one_row_per_pack_plus_done` gives: a
    /// frame that looks right in isolation but that nothing routes real rows
    /// into is exactly `CLAUDE.md`'s recorded `draw_tab` incident shape.
    #[test]
    fn the_experiments_frame_carries_one_row_per_flag_plus_done() {
        let mut nav = CreateWorldNav::new();
        nav.click_focus(EXPERIMENTS_ROW);
        nav.click_row(1);
        let f = frame(&nav);
        assert_eq!(f.rows.len(), 3 + 1, "three flags plus Done");
        assert_eq!(f.title, "Experiments");
        assert!(
            f.rows.iter().any(|r| r.label == "Villager Trade Rebalance: OFF"),
            "an untouched flag's row must read OFF: {:?}",
            f.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
        );
        assert!(
            f.rows.iter().any(|r| r.label == "Redstone Experiments: ON"),
            "the toggled flag's row must read ON: {:?}",
            f.rows.iter().map(|r| &r.label).collect::<Vec<_>>()
        );
    }
}
