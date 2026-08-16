//! The World Creation screen (issue #190) — vanilla's `CreateWorldScreen`,
//! reached from [`super::world_select`]'s "Create New World" button, which
//! issue #397 deliberately left present-and-disabled for this issue to build.
//!
//! ## Tabs (issue #564/#567)
//!
//! This screen used to be one flat hand-placed list, with a module doc arguing
//! at length that vanilla's three `GridLayoutTab`s (`GameTab`/`WorldTab`/
//! `MoreTab`, `CreateWorldScreen.java`) were not worth building to hold a
//! handful of fields that get real support. Issue #567 is the owner saying
//! otherwise: *"the UI is wrong — we need it to match the real vanilla UI for
//! it (which has tabs, etc.)"* — and by the time that issue was filed, #564 had
//! already landed the tab widget itself for Statistics (`widget.rs`'s
//! `TAB_SPRITES`/`tab_underline_colour`/`tab_label_dy`, `layout.rs`'s
//! `tab_bar_geometry`/`tab_bar_row_rect`, `render/frame.rs`'s `MenuRow::tab` +
//! [`super::render::TabEntryView`], `render/draw.rs`'s `draw_tab`). This screen
//! is that widget's **second** consumer, exactly as #564 asked for: one widget,
//! two screens, rather than two bespoke tab strips that could drift apart.
//!
//! Vanilla's three tabs, and where each field landed:
//!
//! - **Game** (`createWorld.tab.game.title`): [`NAME_FIELD`], [`GAME_MODE_ROW`],
//!   [`DIFFICULTY_ROW`], [`ALLOW_CHEATS_ROW`] — vanilla's `GameTab` also has an
//!   Experiments button this client has no experiments screen for, left absent
//!   rather than drawn inert.
//! - **World** (`createWorld.tab.world.title`): [`SEED_FIELD`],
//!   [`WORLD_TYPE_ROW`], [`STRUCTURES_ROW`], [`BONUS_CHEST_ROW`] — vanilla's
//!   `WorldTab` also has a "Customize Type" button this client has no
//!   preset-editor screen for, left absent the same way. [`WORLD_TYPE_ROW`]
//!   itself (issue #519's UI half) is real — cycles all seven bundled
//!   presets and collects the choice — and since issue #592's item 1,
//!   selecting `Normal`/`LargeBiomes`/`Amplified` reaches the served world;
//!   the other four remain decorative. See [`WorldTypePreset`]'s own doc for
//!   exactly which is which and why.
//! - **More** (`createWorld.tab.more.title`): nothing. Vanilla's `MoreTab` is
//!   three buttons (Game Rules, Experiments, Data Packs), and none of the three
//!   models exist here — no game-rule table, no experiments screen, no
//!   data-pack loader. The tab itself is still real: selectable, its own real
//!   [`TabEntryView`](super::render::TabEntryView), just empty. That is the
//!   honest state rather than a lie — vanilla's own More tab is never disabled
//!   for having nothing built under it, unlike Statistics's Items/Mobs, which
//!   vanilla disables **because the underlying list is empty**
//!   (`StatsScreen.setTabActiveStateAndTooltip`). Nothing here is
//!   data-driven-empty; it is feature-not-yet-built, and disabling the tab
//!   would misrepresent that as vanilla's own behaviour.
//! - [`ONLINE_MODE_ROW`] has no vanilla tab at all — see its own doc on why it
//!   exists — and is placed on **World**, after Bonus Chest: it is a
//!   network-exposure setting for the world being created, which is closer in
//!   kind to World's own "how does this world generate/behave" fields than to
//!   Game's account-permission fields.
//!
//! **Not ported: per-tab keyboard focus order.** Vanilla's `MenuTabBar` is
//! itself focusable, in tab-order group 0 ahead of the content
//! (`CreateWorldScreen`'s own `GROUP_BOTTOM = 1` on the footer), so a keyboard
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
//!   state), the Hardcore→Hard difficulty lock (`GameTab.java`'s own
//!   rule: selecting Hardcore forces and disables the difficulty cycle), and
//!   switching between Game/World/More by clicking the tab bar.
//! - **Wired since — the seed.** This section used to say "nothing
//!   downstream reads any field of it yet"; that queued patch landed
//!   (`72cb451`, `d65d593`). `apply_create_world` turns
//!   [`CreateWorldOutcome::Create`] into `MenuAction::Singleplayer(Some(config))`,
//!   and `app.rs`'s `begin_singleplayer` resolves `config.seed` through
//!   `resolve_launch_seed`/`parse_seed` — vanilla's own
//!   `WorldOptions.parseSeed`/`randomSeed` rule (trim, a valid `i64` literal
//!   used verbatim, free text hashed with Java's `String.hashCode`, empty
//!   means fresh random) — into the `i64`
//!   `lodestone_server::worldgen_data::overworld_chunk_source(seed)` wants,
//!   in place of `BUNDLED_WORLD.seed`.
//! - **Wired since — Online Mode.** Not vanilla (no `CreateWorldScreen`
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
//! - **Wired since issue #592's item 1 — three of seven world types.** Cycles
//!   all seven bundled presets for real (issue #519's generator half landed
//!   all seven; this is their UI), and choosing `Normal`/`LargeBiomes`/
//!   `Amplified` now reaches the served world:
//!   `WorldTypePreset::backend_world_type` converts the UI choice,
//!   `begin_singleplayer` (`app/session.rs`) reads it from
//!   `WorldCreationConfig` for a **`Created`** launch only (same rule as
//!   `seed`), threads it through `launch_singleplayer`/
//!   `launch_open_to_lan_online` (`app/launch.rs`) into
//!   `NetClient::open_singleplayer`/`open_to_lan`, and `net.rs`'s
//!   `Origin::Integrated` carries it to the one construction site that used
//!   to hardcode `overworld_chunk_source(seed)`. **Still decorative — the
//!   other four** (`SingleBiomeSurface`/`Flat`/`FlatAllDimensions`/
//!   `DebugAllBlockStates`): their entry points are blocked on a
//!   `lodestone-server` re-export this crate cannot make (see
//!   [`WorldTypePreset::backend_world_type`]'s own doc). Selecting one of the
//!   four falls back to `Overworld` rather than erroring, the same policy
//!   `is_backend_wired` names.
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
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot, TabEntryView};
use super::widget::Widget;

// -- vanilla captions, verbatim from en_us.json --------------------------

/// `selectWorld.enterName`.
pub const NAME_LABEL: &str = "World Name";
/// `selectWorld.newWorld` — the default value, not a hint.
pub const DEFAULT_NAME: &str = "New World";
/// `selectWorld.enterSeed` — the seed field's own visible label, drawn above
/// it exactly like [`NAME_LABEL`] (`CreateWorldScreen.java`, via
/// `CommonLayouts.labeledElement`).
pub const SEED_LABEL: &str = "Seed for the world generator";
/// `selectWorld.seedInfo` — the seed field's `EditBox.hint` ghost text,
/// shown only while the box is empty and unfocused
/// (`CreateWorldScreen.java`). Not a second permanent label; see
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
/// — vanilla uses the same string for both (`CreateWorldScreen.java`'s
/// `createButton`).
pub const CREATE_LABEL: &str = "Create New World";
pub const CANCEL_LABEL: &str = "Cancel";
/// `selectWorld.mapType` — vanilla's own label for the World Type cycle
/// button (`WorldTab.java`'s `typeButton`, `CreateWorldScreen.java`).
pub const WORLD_TYPE_LABEL: &str = "World Type";

/// `createWorld.tab.game.title`/`.world.title`/`.more.title`, verbatim from
/// `en_us.json` — this screen's own tab bar (issue #567), built from the same
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
        _ => &[],
    }
}

/// `WorldCreationUiState.WorldTypeEntry`/`WorldPreset`, narrowed to the seven
/// bundled `world_preset/*.json` documents (issue #519's generator half) —
/// vanilla's own preset list has a customizable "Buffet"/`FLAT`-family branch
/// this client does not model, so this enum is the seven fixed presets rather
/// than an open list.
///
/// ## Backend wiring — three of seven, and which three
///
/// [`Self::caption`] is real for all seven (`generator.minecraft.*`,
/// verbatim). **Selecting one is only decorative for four of the seven now**
/// — see `docs/worldgen-world-type-selection.md`'s own "How to change it"
/// table for exactly which entry point each preset needs:
///
/// - [`Self::Normal`]/[`Self::LargeBiomes`]/[`Self::Amplified`] are **wired,
///   end to end.** [`Self::backend_world_type`] converts the choice,
///   `begin_singleplayer` (`app/session.rs`) reads it from
///   `WorldCreationConfig` for a **`Created`** launch and threads it through
///   `launch_singleplayer`/`launch_open_to_lan_online` (`app/launch.rs`) into
///   `NetClient::open_singleplayer`/`open_to_lan`, and `net.rs`'s
///   `Origin::Integrated` carries it to the `overworld_chunk_source_of_type`
///   call that used to hardcode `overworld_chunk_source(seed)` (i.e.
///   `WorldType::Overworld`) unconditionally. Verified end to end by
///   `tests/singleplayer_terrain_arrives.rs`'s
///   `a_singleplayer_world_honours_the_selected_world_type_end_to_end`, which
///   reuses `lodestone-server/tests/world_type_selection.rs`'s own measured
///   64/130 top-of-world heights at seed 4242 rather than re-deriving them —
///   a real `NetClient::open_singleplayer` session selecting `Amplified` must
///   serve the 130 figure over the wire, not the 64 the Overworld default
///   would give the identical column.
/// - [`Self::SingleBiomeSurface`]/[`Self::Flat`]/[`Self::FlatAllDimensions`]/
///   [`Self::DebugAllBlockStates`] are **still blocked on a
///   `lodestone-server` change**: their entry points
///   (`single_biome_chunk_source`/`flat_chunk_source`/`debug_chunk_source`)
///   are real and individually verified, but not yet re-exported from that
///   crate's root — `crates/lodestone-server/src/lib.rs` is off limits to
///   this agent (another is live in it), so this cannot be closed from here.
///   Selecting one of these four does not error; [`Self::backend_world_type`]
///   falls back to [`lodestone_server::WorldType::Overworld`], same as
///   selecting [`Self::Normal`] — see that method's own doc.
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
    #[must_use]
    pub fn is_backend_wired(self) -> bool {
        matches!(
            self,
            WorldTypePreset::Normal | WorldTypePreset::LargeBiomes | WorldTypePreset::Amplified
        )
    }

    /// The `lodestone_server::WorldType` this preset resolves to, now that the
    /// `net.rs` threading hop (issue #592's item 1) exists.
    ///
    /// [`Self::is_backend_wired`] is the caller-facing "does this do anything"
    /// question; this is the *value* to pass once the answer is yes. The four
    /// presets [`Self::is_backend_wired`] reports `false` for have no
    /// `lodestone_server::WorldType` variant at all yet — they need
    /// `single_biome_chunk_source`/`flat_chunk_source`/`debug_chunk_source`,
    /// not this enum, once those are re-exported (issue #592's item 2) — so
    /// they fall back to [`lodestone_server::WorldType::Overworld`] here,
    /// same as selecting [`Self::Normal`]. That fallback is exactly what
    /// [`Self::is_backend_wired`] exists to let a caller warn about instead of
    /// silently accepting.
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

/// `WorldCreationUiState.SelectedGameMode`, narrowed to the three a player
/// actually picks from this button (`GameTab.java`'s own cycle — `DEBUG`,
/// vanilla's fourth value, is not offered here; `SelectedGameMode.java`'s
/// own caption for it is literally "spectator", which is not a serious
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
    /// vanilla's own `WorldOptions.defaultWithRandomSeed()` branch
    /// (`selectWorld.seedInfo`). Parsing an empty/non-numeric seed into an
    /// actual `i64` is the consuming patch's job (see the module docs), not
    /// this screen's: vanilla itself accepts non-numeric seed text and
    /// hashes it (`WorldOptions.java`'s own `parseSeed`), which this menu
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
    /// **Wired, not decorative** (issue #273's shell-side control) — unlike
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
}

impl Default for WorldCreationConfig {
    fn default() -> Self {
        Self {
            name: DEFAULT_NAME.to_string(),
            seed: String::new(),
            world_type: WorldTypePreset::default(),
            game_mode: WorldGameMode::default(),
            // `Difficulty.NORMAL` — `WorldCreationUiState.java`.
            difficulty: WorldDifficulty::Normal,
            // `WorldOptions`' own defaults — `generateStructures` true,
            // `generateBonusChest` false (`WorldCreationUiState.java`
            // reads these off `settings.options()`, whose defaults are
            // vanilla's `WorldOptions.defaultWithRandomSeed()`).
            generate_structures: true,
            bonus_chest: false,
            allow_cheats: false,
            online_mode: false,
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
const ROW_COUNT: usize = 11;

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
    // World now has one more row than Game (five vs. four, since #519's
    // world-type selector landed on World only), so the two tabs' local
    // indices no longer line up 1:1 the way they did when every row paired
    // with exactly one sibling. Named per-tab instead of paired, still one
    // `match` arm per **local row position** so the two tabs' rows that do
    // share a `dy` (never shown at once, so sharing costs nothing) stay
    // visibly paired rather than restated at the same number twice.
    match row {
        // Local row 0: Name / Seed.
        NAME_FIELD | SEED_FIELD => {
            Slot { origin: Origin::ScreenTop, dx: X, dy: TOP, w: FIELD_W, h: super::render::EDIT_BOX_H }
        }
        // Local row 1: Game Mode / World Type.
        GAME_MODE_ROW | WORLD_TYPE_ROW => Slot {
            origin: Origin::ScreenTop,
            dx: X,
            dy: TOP + ROW_H,
            w: FIELD_W,
            h: super::render::EDIT_BOX_H,
        },
        // Local row 2: Difficulty / Structures.
        DIFFICULTY_ROW | STRUCTURES_ROW => Slot {
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
    /// `MenuAction::Singleplayer` (issue #190). Not `Copy` any more:
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
    /// Which of [`TAB_LABELS`] is currently showing (issue #567). Starts at
    /// [`GAME_TAB`] — vanilla's own `MenuTabBar.builder(..).addTabs(GameTab,
    /// WorldTab, MoreTab)` order, and the tab `CreateWorldScreen.
    /// setInitialFocus` would land on if it ran (it does not — see the
    /// module docs on why keyboard tab-order is not ported).
    active_tab: usize,
    /// Which button row the mouse is over, if any — separate from keyboard
    /// focus for [`super::nav::EditForm::hovered`]'s exact reason (this
    /// screen has the same shape: two [`EditBox`]es plus five button rows).
    /// Carries a **focus id**, not a frame-row index — see the module docs'
    /// "two index spaces" section.
    ///
    /// **This is the whole of issue #567's reported hover bug**, fixed before
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
}

fn button(row: usize, label: impl Into<String>) -> Widget {
    let (x, y, w, h) = row_slot(row).resolve(SEED_CANVAS.0, SEED_CANVAS.1);
    Widget::new(x, y, w, h, label)
}

impl CreateWorldNav {
    /// A fresh screen at vanilla's own defaults — called every time the
    /// screen is opened (`CreateWorldScreen.openFresh`'s own state, not
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
    }

    /// Difficulty is locked to Hard and its own row inactive while Hardcore
    /// is selected — `GameTab.java`'s own rule (selecting Hardcore forces
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
    /// vanilla's own tab switch (`TabManager.setCurrentTab`, which calls
    /// `setInitialFocus` on the new tab) — or clears focus entirely on
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
            CREATE_ROW => {
                self.config.name = self.widgets.name.value().to_string();
                self.config.seed = self.widgets.seed.value().to_string();
                CreateWorldOutcome::Create(self.config.clone())
            }
            CANCEL_ROW => CreateWorldOutcome::Cancel,
            _ => CreateWorldOutcome::Handled,
        }
    }

    /// A click on frame row `row` (an index into `frame(..).rows` — see the
    /// module docs). Mirrors
    /// [`super::world_select::WorldSelectNav::click_row`]'s own reasoning
    /// (#391's shape): a click focuses a field, presses a button, or — new
    /// for issue #567 — switches the active tab, and none of those is "hover
    /// then Enter".
    pub fn click_row(&mut self, row: usize) -> CreateWorldOutcome {
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
    /// and cites `Screen.keyPressed`'s own order for. Tab traversal stays
    /// within the showing tab's own fields plus the always-active footer —
    /// see [`Self::sync_tab_visibility`]'s own doc on why that needs no
    /// special case here: [`FocusSet`] already skips an inactive widget.
    pub fn handle_key(&mut self, key: MenuKey) -> CreateWorldOutcome {
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
/// Vanilla draws no heading above `CreateWorldScreen`'s tab bar either — the
/// bar's own background (`CreateWorldScreen.TAB_HEADER_BACKGROUND`) *is* the
/// header, exactly as `stats.rs`'s own doc explains for the same widget. This
/// used to draw `"Create New World"` as a centred label at `dy: 12`, which is
/// the vanilla string for the *button* that opens this screen
/// (`selectWorld.create`), not a real vanilla heading on the screen itself.
#[must_use]
pub fn frame(nav: &CreateWorldNav) -> MenuFrame<'static> {
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
    // `CommonLayouts.labeledElement` draws a real, visible label above each
    // field in vanilla (`CreateWorldScreen.java`) — only the active tab's own
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
        // notice rather than vanilla's `EditBox.hint` ghost text — see the
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
        // Issue #567: this used to be left at its `..Default::default()` of
        // `None` unconditionally — nothing on this screen ever recorded which
        // row the mouse was over (see `CreateWorldNav::hovered`'s own doc) —
        // so `render::draw_widget`'s `widget.hovered` was `false` for every
        // row, every frame, and no button ever drew its hover outline.
        hovered,
        vanilla: true,
        labels,
        // No `notice` here. Vanilla shows `SEED_INFO` in exactly one place —
        // `seedEdit.setHint(SEED_EMPTY_HINT)` (`CreateWorldScreen.java`),
        // ghost text drawn only while the box is empty and unfocused.
        // `CreateWorldNav::new` already sets `seed.hint`, so a permanent
        // notice here would draw the same string vanilla only ever shows
        // conditionally — a duplicate, not a second real label.
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
        // #391's shape, on this screen too: clicking Structures must not
        // touch Bonus Chest.
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
        // `CreateWorldScreen.java` wraps each field in
        // `CommonLayouts.labeledElement` — a real, drawn label, not
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
        // place (`EditBox.hint`, conditional on empty+unfocused).
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

    // -- the tab bar (issues #564/#567) --------------------------------------

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
            TAB_LABELS.len() + 2,
            "More has no content rows, only the tab bar and the footer"
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
        assert_eq!(nav.focused(), None, "More has nothing to focus");

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

    // -- hover (issue #567) --------------------------------------------------

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
    /// a gap here is exactly how issue #567 shipped: `hover_row` existed on
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

    // -- world type (issue #519's UI half) -----------------------------------

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
    fn every_preset_caption_is_vanillas_own_generator_string() {
        // `generator.minecraft.<id>`, verbatim from `en_us.json` — not
        // re-derived here, quoted from the jar's own strings so a typo cannot
        // silently pass by looking plausible.
        assert_eq!(WorldTypePreset::Normal.caption(), "Default");
        assert_eq!(WorldTypePreset::LargeBiomes.caption(), "Large Biomes");
        assert_eq!(WorldTypePreset::Amplified.caption(), "AMPLIFIED");
        assert_eq!(WorldTypePreset::SingleBiomeSurface.caption(), "Single Biome");
        assert_eq!(WorldTypePreset::Flat.caption(), "Superflat");
        assert_eq!(WorldTypePreset::FlatAllDimensions.caption(), "Flat All Dimensions");
        assert_eq!(WorldTypePreset::DebugAllBlockStates.caption(), "Debug Mode");
    }

    #[test]
    fn exactly_the_three_backend_ready_presets_report_wired() {
        // The control this needs: not "some report true", but *exactly* the
        // three `docs/worldgen-world-type-selection.md` names as reachable
        // with no `lodestone-server` change (`overworld_chunk_source_of_type`
        // already `pub` at that crate's root) — collected, not asserted
        // inside the loop, so a wrong-by-one set is still fully reported.
        let wired: Vec<WorldTypePreset> = [
            WorldTypePreset::Normal,
            WorldTypePreset::LargeBiomes,
            WorldTypePreset::Amplified,
            WorldTypePreset::SingleBiomeSurface,
            WorldTypePreset::Flat,
            WorldTypePreset::FlatAllDimensions,
            WorldTypePreset::DebugAllBlockStates,
        ]
        .into_iter()
        .filter(|p| p.is_backend_wired())
        .collect();
        assert_eq!(
            wired,
            vec![
                WorldTypePreset::Normal,
                WorldTypePreset::LargeBiomes,
                WorldTypePreset::Amplified,
            ]
        );
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
}
