//! Runtime configuration for the game shell, parsed from argv, plus the
//! **persisted** user options ([`Options`]) that survive a restart.
//!
//! Kept tiny and version-free: the shell never names a protocol version. The
//! only network knob is `protocol`, a *number* the shell hands to
//! [`lodestone_registry::adapter_for_protocol`] — the registry decides which
//! version crate (if any) answers it.
//!
//! ## GUI scale
//!
//! [`calculate_gui_scale`] reproduces vanilla's own gui-scale calculation
//! exactly: an integer scale, `0` meaning "auto" (pick the largest scale that
//! still fits a minimum logical size), clamped so the framebuffer is never
//! divided into less than that minimum. See the function's own docs for the
//! one deliberate omission (the legacy `enforceUnicode` even-scale rounding).
//!
//! [`Options`] is the persisted settings model built on top of it — today
//! `gui_scale` plus the [`crate::keybinds`] table. It is written to
//! `options.json` next to `servers.json`, in the
//! **same** platform data directory [`crate::menu::servers::data_dir`] already
//! discovers; see [`options_path`]. That reuse is deliberate — see that
//! module's docs for why the directory lookup lives there and not here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::keybinds::Keybinds;

/// Sentinel `gui_scale` value meaning "auto": the largest integer scale that
/// still fits [`MIN_SCALED_WIDTH`]x[`MIN_SCALED_HEIGHT`] into the framebuffer.
/// Matches vanilla's own auto-gui-scale sentinel.
pub const AUTO_GUI_SCALE: u32 = 0;

/// Default look sensitivity — vanilla's own persisted-options declarations
/// (a unit-interval double, default `0.5`), and already what [`Config::default`] used before
/// [`Options::sensitivity`] existed, so the migration in issue #443 changes
/// nothing for an untouched install.
pub const DEFAULT_SENSITIVITY: f32 = 0.5;

/// Default render distance in chunks.
///
/// **This is 8, and vanilla's is 12.** The difference is
/// deliberate and predates issue #443: `Config::default().render_distance` has
/// been 8 for this client's whole life, so making the persisted default 12 would
/// silently hand every existing install a 2.25× larger chunk load the first time
/// they launched a build with a settings row wired. A migration must not change
/// behaviour it is only supposed to relocate; the *slider's range* is still
/// vanilla's `2..=32` (see `menu::options::INT_RANGE_SLIDERS`), so a player can
/// reach 12 or 32 — this is the starting point, not a ceiling.
pub const DEFAULT_RENDER_DISTANCE: u32 = 8;

/// Vanilla's `renderDistance` minimum (an int range starting at 2 in its own
/// persisted-options declarations). The clamp is load-bearing rather than cosmetic: 0 or 1
/// reaches `sim/build.rs`'s world radius and would generate nothing.
pub const MIN_RENDER_DISTANCE: u32 = 2;

/// Vanilla's `renderDistance` maximum on the `largeDistances` branch
///. See `menu::options::LARGE_DISTANCES_MAX` for why this
/// client takes that branch unconditionally — there is no JVM heap cap to ask.
pub const MAX_RENDER_DISTANCE: u32 = 32;

/// Vanilla's `weatherRadius` bounds and default — a clamped range `3..=10`
/// defaulting to `10`. The same pair
/// `menu::options::INT_RANGE_SLIDERS`' `"weatherRadius"` row places the handle
/// with, so the value a drag can reach and the track it draws on are one fact.
///
/// The default is deliberately named through
/// [`lodestone_render::DEFAULT_WEATHER_RADIUS`] rather than spelled `10` here:
/// that constant is what `app::weather::weather_columns_for_frame` used to pass
/// unconditionally, and a second literal would let the two drift.
pub const MIN_WEATHER_RADIUS: i32 = 3;
/// See [`MIN_WEATHER_RADIUS`].
pub const MAX_WEATHER_RADIUS: i32 = lodestone_render::DEFAULT_WEATHER_RADIUS;

/// Vanilla's `biomeBlendRadius` bounds and default — a clamped range `0..=7`
/// defaulting to `2`. The same pair
/// `menu::options::INT_RANGE_SLIDERS`' `"biomeBlendRadius"` row places the
/// handle with.
///
/// The stored number is the window **radius**; vanilla's label shows the
/// *width* `2r + 1` ("5x5 (Normal)" at the default `2`), so the two are not
/// interchangeable and a label written against the radius would be off by a
/// factor of two and a bit at every value.
///
/// The maximum is named through [`lodestone_assets::tint::MAX_BLEND_RADIUS`],
/// which is the width of `BlendRowCursor`'s ring buffer, rather than spelled
/// `7` here: vanilla's option maximum and this client's cursor capacity have to
/// be the same number, and a second literal is how they would stop being one.
pub const MIN_BIOME_BLEND_RADIUS: i32 = 0;
/// See [`MIN_BIOME_BLEND_RADIUS`].
pub const MAX_BIOME_BLEND_RADIUS: i32 = lodestone_assets::tint::MAX_BLEND_RADIUS;
/// See [`MIN_BIOME_BLEND_RADIUS`] — vanilla's shipped default, `5x5 (Normal)`.
pub const DEFAULT_BIOME_BLEND_RADIUS: i32 = lodestone_render::biome_tint::BLEND_RADIUS;

/// Vanilla's `menuBackgroundBlurriness` bounds and default —
/// a clamped range `0..=10` defaulting to `BLURRINESS_DEFAULT_VALUE = 5`
///. The same pair
/// `menu::options::INT_RANGE_SLIDERS`' `"menuBackgroundBlurriness"` row places
/// the handle with.
///
/// `0` is a real, reachable value rather than a degenerate one: vanilla's
/// `Screen.extractBlurredBackground` only asks for the pass at
/// `blurRadius >= 1.0F`, so zero means "no blur", which is why the label is
/// `genericValueOrOffLabel` and reads **OFF**.
pub const MIN_MENU_BACKGROUND_BLURRINESS: u32 = 0;
/// See [`MIN_MENU_BACKGROUND_BLURRINESS`].
pub const MAX_MENU_BACKGROUND_BLURRINESS: u32 = 10;
/// See [`MIN_MENU_BACKGROUND_BLURRINESS`] — vanilla's own
/// shipped default.
pub const DEFAULT_MENU_BACKGROUND_BLURRINESS: u32 = 5;

/// Vanilla clamps the auto-picked (and any manual) scale so the resulting
/// *scaled* GUI resolution never drops below this many logical pixels wide —
/// its own gui-scale calculation's `>= 320` check.
pub const MIN_SCALED_WIDTH: u32 = 320;
/// As [`MIN_SCALED_WIDTH`], vertical — vanilla's own gui-scale calculation's `>= 240`.
///
/// Public because it is a **floor on the logical canvas**, not just an input to
/// [`calculate_gui_scale`]: a screen that has to fit its content into the band
/// that survives at every scale needs the number, and the settings tree's
/// visible-list window is derived from it (`menu::options::LIST_WINDOW_PX`)
/// rather than picked.
pub const MIN_SCALED_HEIGHT: u32 = 240;

/// Highest `gui_scale` the settings screen will manually cycle to. Vanilla's
/// own ceiling is effectively unbounded (its own persisted-options
/// declarations put the manual maximum at `2147483646`) and its slider's *dynamic* max is
/// `calculate_gui_scale(AUTO_GUI_SCALE, ..)` for the live window
/// — but that means threading the live framebuffer
/// size into the menu's pure, GPU-free navigation layer just to bound a
/// cycle. [`calculate_gui_scale`] still clamps the *effective* scale to
/// whatever the framebuffer actually fits regardless of what is requested, so
/// a manual value above what the window can show just saturates rather than
/// doing anything unsafe or even visible.
pub const MAX_MANUAL_GUI_SCALE: u32 = 8;

/// Vanilla's `fov` bounds and default (its own persisted-options
/// declarations: an int range 30 to 110, default `70`).
///
/// **An int-valued range, not a unit-interval double** — the trap this triple exists to close.
/// A persistence-only codec sits between the stored int and the double the
/// slider's value-set actually walks, so reading the value-set's transform as
/// the persistence one puts the value at `70 * 40 + 70`. The same pair is in
/// `menu::options::INT_RANGE_SLIDERS` under the `"fov"` accessor, which is where
/// the slider handle comes from.
pub const MIN_FOV: u32 = 30;
/// See [`MIN_FOV`].
pub const MAX_FOV: u32 = 110;
/// See [`MIN_FOV`] — vanilla's shipped "Normal" FOV, and the same number
/// `camera_rig::FOV_Y_DEGREES` used to pin the camera to unconditionally.
pub const DEFAULT_FOV: u32 = 70;

/// Vanilla's `CloudStatus.getSerializedName` — the string
/// `options.json` stores [`Options::cloud_status`] as.
///
/// A name rather than the enum's ordinal, because the file is hand-editable and a
/// bare `1` is both unguessable and silently remapped by any future variant
/// insertion. Vanilla serialises the same three strings in its own `options.txt`.
#[must_use]
pub fn cloud_status_name(status: lodestone_render::CloudStatus) -> &'static str {
    match status {
        lodestone_render::CloudStatus::Off => "off",
        lodestone_render::CloudStatus::Fast => "fast",
        lodestone_render::CloudStatus::Fancy => "fancy",
    }
}

/// The inverse of [`cloud_status_name`]. `None` for anything else, which
/// [`Options::from_json`] turns into vanilla's default rather than an error — the
/// same rule every other key in that function follows.
///
/// Vanilla additionally accepts its **legacy** boolean spellings here
/// (`"true"` → FANCY, `"false"` → OFF, `CloudStatus.byName`), and so does this: a
/// player copying a value out of an old `options.txt` should not silently get
/// FANCY where they asked for off.
#[must_use]
pub fn cloud_status_from_name(name: &str) -> Option<lodestone_render::CloudStatus> {
    match name {
        "off" | "false" => Some(lodestone_render::CloudStatus::Off),
        "fast" => Some(lodestone_render::CloudStatus::Fast),
        "fancy" | "true" => Some(lodestone_render::CloudStatus::Fancy),
        _ => None,
    }
}

/// Vanilla's `ParticleStatus`, the
/// `options.particles` cycle.
///
/// Three states in declaration order, which is also its own cycle-button's visiting order
/// and the order of the enum's ids `0, 1, 2`: `ALL`, `DECREASED`, `MINIMAL`.
/// `ALL` is vanilla's default and is what this client did unconditionally
/// before the row went live.
///
/// **This is a probabilistic filter, not three fixed budgets.**
/// `ClientLevel.calculateParticleLevel` folds the option down per spawn:
/// `DECREASED` becomes `MINIMAL` one time in three, and `MINIMAL` is lifted
/// back to `DECREASED` one time in ten *for an always-show particle*.
/// `ClientLevel.doAddParticle` then drops the spawn whenever the folded level
/// is `MINIMAL`. So `DECREASED` keeps roughly two thirds of eligible spawns and
/// `MINIMAL` keeps roughly none. See
/// `crate::particles::Particles::particle_level_permits`, which is where that
/// fold lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticleLevel {
    /// `options.particles.all` — vanilla's default: every eligible spawn.
    #[default]
    All,
    /// `options.particles.decreased` — roughly two spawns in three.
    Decreased,
    /// `options.particles.minimal` — effectively none.
    Minimal,
}

/// Vanilla's `ParticleStatus` serialized name — the string `options.json`
/// stores [`Options::particles`] as. A name rather than vanilla's integer id,
/// [`cloud_status_name`]'s reasoning.
#[must_use]
pub fn particle_level_name(value: ParticleLevel) -> &'static str {
    match value {
        ParticleLevel::All => "all",
        ParticleLevel::Decreased => "decreased",
        ParticleLevel::Minimal => "minimal",
    }
}

/// The inverse of [`particle_level_name`]. `None` for anything else, which
/// [`Options::from_json`] turns into vanilla's `All` default.
#[must_use]
pub fn particle_level_from_name(name: &str) -> Option<ParticleLevel> {
    match name {
        "all" => Some(ParticleLevel::All),
        "decreased" => Some(ParticleLevel::Decreased),
        "minimal" => Some(ParticleLevel::Minimal),
        _ => None,
    }
}

/// Vanilla's `AttackIndicatorStatus`, the
/// `options.attackIndicator` cycle.
///
/// Three states in **declaration order**, which is also its own cycle-button's visiting
/// order and the order of the enum's own ids `0, 1, 2`:
/// `OFF`, `CROSSHAIR`, `HOTBAR`. `CROSSHAIR` is vanilla's default and is what
/// this client drew unconditionally before the row went live.
///
/// The two live states draw the **same** attack-strength value in two different
/// places, never both: `Hud.extractCrosshair` gates its 16x4 bar under the
/// crosshair on `CROSSHAIR`, and the hotbar section gates its 18x18 gauge beside
/// the hotbar on `HOTBAR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AttackIndicator {
    /// `options.off` — no indicator anywhere.
    Off,
    /// `options.attack.crosshair` — vanilla's default: the bar under the
    /// crosshair.
    #[default]
    Crosshair,
    /// `options.attack.hotbar` — the gauge beside the hotbar.
    Hotbar,
}

/// Vanilla's `AttackIndicatorStatus` serialized name — the string
/// `options.json` stores [`Options::attack_indicator`] as. A name rather than
/// vanilla's own integer id, [`cloud_status_name`]'s reasoning: the file stays
/// hand-editable and a future variant insertion cannot silently renumber it.
#[must_use]
pub fn attack_indicator_name(value: AttackIndicator) -> &'static str {
    match value {
        AttackIndicator::Off => "off",
        AttackIndicator::Crosshair => "crosshair",
        AttackIndicator::Hotbar => "hotbar",
    }
}

/// The inverse of [`attack_indicator_name`]. `None` for anything else, which
/// [`Options::from_json`] turns into vanilla's `Crosshair` default.
#[must_use]
pub fn attack_indicator_from_name(name: &str) -> Option<AttackIndicator> {
    match name {
        "off" => Some(AttackIndicator::Off),
        "crosshair" => Some(AttackIndicator::Crosshair),
        "hotbar" => Some(AttackIndicator::Hotbar),
        _ => None,
    }
}

/// Vanilla's `InactivityFpsLimit`, the `options.
/// inactivityFpsLimit` cycle — "Reduce FPS when" `Minimized`/`AFK`.
///
/// `Minimized` reduces the frame rate only while the OS reports the window
/// iconified; `Afk` additionally runs vanilla's own idle clock
/// (`FramerateLimitTracker`'s `SHORT_AFK`/`LONG_AFK`, 30 fps after a minute of
/// no input and 10 after ten). This client's window already throttles an
/// unfocused/occluded window unconditionally (`app::pacing::FramePacer`'s
/// table, which predates this option), so what this field actually gates is
/// the AFK half — see [`crate::app::pacing::effective_target_fps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InactivityFpsLimit {
    /// `options.inactivityFpsLimit.minimized` — reduce only when iconified.
    Minimized,
    /// `options.inactivityFpsLimit.afk` — vanilla's own default.
    #[default]
    Afk,
}

/// Vanilla's `InactivityFpsLimit.getSerializedName` — the string `options.json`
/// stores [`Options::inactivity_fps_limit`] as. Same reasoning as
/// [`cloud_status_name`]: a name, not an ordinal, so the file stays
/// hand-editable and immune to a future variant insertion.
#[must_use]
pub fn inactivity_fps_limit_name(value: InactivityFpsLimit) -> &'static str {
    match value {
        InactivityFpsLimit::Minimized => "minimized",
        InactivityFpsLimit::Afk => "afk",
    }
}

/// The inverse of [`inactivity_fps_limit_name`]. `None` for anything else,
/// which [`Options::from_json`] turns into vanilla's `Afk` default.
#[must_use]
pub fn inactivity_fps_limit_from_name(name: &str) -> Option<InactivityFpsLimit> {
    match name {
        "minimized" => Some(InactivityFpsLimit::Minimized),
        "afk" => Some(InactivityFpsLimit::Afk),
        _ => None,
    }
}

/// Vanilla's own unlimited-framerate cutoff: the
/// stored `framerateLimit` value at and above which vanilla's own per-tick
/// loop never applies its own frame-rate limiter
/// at all — "Unlimited" is a *sentinel value*, not a special-cased "no limit"
/// state, and `260` is chosen so the row's own bucket range `1..=26`, scaled by `*10`,
/// makes it the slider's last bucket.
pub const UNLIMITED_FRAMERATE_CUTOFF: u32 = 260;

/// Vanilla's `framerateLimit` floor — its own persisted-options declarations
/// clamp the stored value to 10..=260. The slider's own pre-image floor
/// (bucket `1` of 26) maps to this through the `*10` transform.
pub const MIN_FRAMERATE_LIMIT: u32 = 10;

/// Vanilla's shipped default `framerateLimit`, `120` —
/// bucket `12` of `26` through the same xmap
/// [`menu::options::INT_RANGE_SLIDERS`](crate::menu::options::INT_RANGE_SLIDERS)
/// already carried for the inactive row.
pub const DEFAULT_FRAMERATE_LIMIT: u32 = 120;

/// Vanilla's `GraphicsPreset` — the "Quality &
/// Performance" preset slider: `Fast`, `Fancy`, `Fabulous`, `Custom`, in that
/// declaration order (the order [`crate::menu::options::LiveOption::
/// GraphicsPreset`]'s slider visits, matching `SliderableEnum.toSliderValue`'s
/// `values.indexOf`).
///
/// `GraphicsPreset::apply` writes **seventeen**
/// vanilla quality options; this client only has real consumers for three of
/// them ([`Options::render_distance`], [`Options::cloud_status`],
/// [`Options::cutout_leaves`]), so [`crate::menu::nav::MenuNav::apply_graphics_preset`]
/// writes those three and no others — see that function's doc for the numbers
/// and for why `Custom` writes nothing (vanilla's own preset selector has no
/// arm for it — it is a client-local marker meaning "none of the presets
/// match anymore", not a preset vanilla itself ever switches to).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsPreset {
    /// The fastest preset.
    Fast,
    /// The middle preset — vanilla's shipped default.
    Fancy,
    /// The most detailed preset.
    Fabulous,
    /// The state every individually-changed quality
    /// option should settle into (vanilla's own preset-to-custom transition).
    /// Nothing in this client writes it automatically yet; see
    /// [`Options::graphics_preset`]'s doc for the gap.
    Custom,
}

impl Default for GraphicsPreset {
    fn default() -> Self {
        GraphicsPreset::Fancy
    }
}

impl GraphicsPreset {
    /// The four variants in vanilla's own declaration order —
    /// the order both the slider and [`graphics_preset_name`]/
    /// [`graphics_preset_from_name`] key off.
    pub const ORDER: [GraphicsPreset; 4] = [
        GraphicsPreset::Fast,
        GraphicsPreset::Fancy,
        GraphicsPreset::Fabulous,
        GraphicsPreset::Custom,
    ];
}

/// `GraphicsPreset.getSerializedName` — the string `options.json` stores
/// [`Options::graphics_preset`] as. A name, not an ordinal, for
/// [`cloud_status_name`]'s reason.
#[must_use]
pub fn graphics_preset_name(value: GraphicsPreset) -> &'static str {
    match value {
        GraphicsPreset::Fast => "fast",
        GraphicsPreset::Fancy => "fancy",
        GraphicsPreset::Fabulous => "fabulous",
        GraphicsPreset::Custom => "custom",
    }
}

/// The inverse of [`graphics_preset_name`]. `None` for anything else, which
/// [`Options::from_json`] turns into vanilla's `Fancy` default.
#[must_use]
pub fn graphics_preset_from_name(name: &str) -> Option<GraphicsPreset> {
    match name {
        "fast" => Some(GraphicsPreset::Fast),
        "fancy" => Some(GraphicsPreset::Fancy),
        "fabulous" => Some(GraphicsPreset::Fabulous),
        "custom" => Some(GraphicsPreset::Custom),
        _ => None,
    }
}

/// Vanilla's eleven sound-category names, in its own declaration order —
/// the strings its own sound-category name lookup returns.
///
/// Three different things are keyed off this one list, which is why it is a
/// constant rather than three literals: the `options.soundSource.<name>`
/// captions the settings tree renders, the `sound_volume_<name>` keys in
/// `options.json`, and the index into [`Options::sound_volumes`].
///
/// **Indexed by `lodestone_model::event::SoundCategory::ordinal`**, which is the
/// same bridge `lodestone_sound`'s own `map_category` crosses to reach a mixer
/// bus — so the persisted key, the caption and the bus cannot drift apart
/// without the ordinal itself changing. Note `Records`/`Blocks`/`Players` are
/// *plural* in the enums and **singular** on the wire and in the file; that
/// asymmetry is vanilla's, and it is the reason this list exists at all instead
/// of a lowercased `Debug` impl.
pub const SOUND_CATEGORY_NAMES: [&str; 11] = [
    "master", "music", "record", "weather", "block", "hostile", "neutral", "player", "ambient",
    "voice", "ui",
];

/// Vanilla's `mouseWheelSensitivity` slider bounds:
/// its own log-mapped sensitivity at slider positions `-200` and `100`, i.e. `10^(-200/100)` and
/// `10^(100/100)`.
pub const MIN_MOUSE_WHEEL_SENSITIVITY: f32 = 0.01;
/// See [`MIN_MOUSE_WHEEL_SENSITIVITY`].
pub const MAX_MOUSE_WHEEL_SENSITIVITY: f32 = 10.0;
/// The step one settings-row click moves `mouse_wheel_sensitivity` by.
///
/// Not vanilla's own granularity — the slider drags continuously through 300
/// log-mapped integer steps, which is meaningless translated to "one click".
/// Chosen so the whole `0.01..=10.0` range takes a reasonable number of
/// clicks to traverse rather than either one click (a de facto on/off switch)
/// or hundreds.
pub const MOUSE_WHEEL_SENSITIVITY_STEP: f32 = 0.25;

/// The step one settings-row click moves a unit-interval-double option
/// by — every chat and text-background option is one of these.
///
/// As with [`MOUSE_WHEEL_SENSITIVITY_STEP`] this is not vanilla's own
/// granularity: vanilla's own version of the option is a *drag*, continuous over `[0, 1]`, and this
/// client's settings rows activate as a click rather than a drag (see
/// `menu::options::SettingsOutcome::Cycle`). `0.1` is chosen to match the
/// granularity vanilla's own percent-value label displays — it prints
/// the value scaled to whole percent, so a tenth is a visible 10-percentage-point move and
/// the whole range is ten clicks.
pub const UNIT_DOUBLE_STEP: f32 = 0.1;

/// Advances a unit-interval double option one click, wrapping past the top back to `0`.
///
/// Additive on the **continuous** value rather than a round-trip through a
/// quantized step index, for exactly the reason
/// `MenuNav::cycle_mouse_wheel_sensitivity` documents: `chat_height_unfocused`
/// boots at `70.0 / 160.0 = 0.4375`, which is not a multiple of
/// [`UNIT_DOUBLE_STEP`] away from `0.0`, so snapping to the nearest grid point
/// would silently move a value the user never touched.
///
/// The period is `1.0 + step` rather than `1.0` so that `1.0` is a *reachable,
/// resting* value instead of being skipped straight to `0.0` — the same shape
/// (and the same reason) as the mouse-wheel wrap.
///
/// A non-finite or out-of-range input is pulled back into `[0, 1]` rather than
/// propagated: `options.json` is hand-editable, and a corrupt value must not
/// put a slider handle off its track.
#[must_use]
pub fn step_unit_double(value: f32, delta: i32) -> f32 {
    let base = if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let period = 1.0 + UNIT_DOUBLE_STEP;
    let wrapped = (base + delta as f32 * UNIT_DOUBLE_STEP).rem_euclid(period);
    // `rem_euclid` can land in `(1.0, 1.1)` — the deliberate rest-at-max
    // window above — so clamp rather than let a handle draw off the track.
    wrapped.clamp(0.0, 1.0)
}

/// Reproduces vanilla's own gui-scale calculation exactly, **minus** the
/// legacy even-scale rounding it applies for its old unicode-font mode:
/// Lodestone has no unicode-font mode to enforce (that option exists in
/// vanilla only for a legacy glyph-page font), so the branch is dropped
/// rather than wired to a setting that would always read `false`.
///
/// `desired` is the persisted `gui_scale` option: [`AUTO_GUI_SCALE`] (`0`)
/// means "pick the largest scale that fits" (vanilla passes `maxScale = 0` for
/// auto and relies on `scale` — which only ever counts up from `1` — never
/// equalling it; reproduced here with an unreachable ceiling rather than a
/// literal `0` so the same loop serves both cases without a divide-by-zero).
/// Any other value is a hard upper bound that itself gets reduced if the
/// framebuffer is too small for it.
///
/// `framebuffer_width`/`framebuffer_height` must be **physical** pixels — what
/// winit calls `PhysicalSize`, i.e. already DPI-scaled — matching vanilla's
/// own physical framebuffer dimensions. That is the only place a display's DPI
/// factor enters this model: there is no separate "DPI scale" input, because
/// on a Retina/HiDPI display the physical framebuffer size already *is* the
/// logical window size times the OS scale factor. Dividing the framebuffer by
/// the returned integer scale (what vanilla calls its own scaled logical
/// dimensions) is what turns a fixed-pixel-sized menu layout into
/// the right *visual* size instead of half-size on a 2x display.
#[must_use]
pub fn calculate_gui_scale(desired: u32, framebuffer_width: u32, framebuffer_height: u32) -> u32 {
    // Vanilla's `scale != maxScale` loop guard never fires for `maxScale == 0`
    // because `scale` starts at 1 and only increases — `i32::MAX` reproduces
    // that "unreachable ceiling" behaviour for `desired == AUTO_GUI_SCALE`.
    let ceiling = if desired == AUTO_GUI_SCALE {
        i32::MAX
    } else {
        desired as i32
    };
    let fb_w = framebuffer_width as i32;
    let fb_h = framebuffer_height as i32;
    let mut scale: i32 = 1;
    while scale != ceiling
        && scale < fb_w
        && scale < fb_h
        && fb_w / (scale + 1) >= MIN_SCALED_WIDTH as i32
        && fb_h / (scale + 1) >= MIN_SCALED_HEIGHT as i32
    {
        scale += 1;
    }
    // Vanilla can return a `0` framebuffer-sized scale (e.g. an iconified
    // window reporting 0x0); a menu that divides by that would be a fresh
    // crash, so the effective scale is floored at 1 here rather than in every
    // caller.
    scale.max(1) as u32
}

/// Persisted user settings that must survive a restart — distinct from
/// [`Config`], which is parsed fresh from argv every run and never written
/// back. Add fields here as more settings need to persist, following
/// [`crate::menu::servers::ServerList`]'s rule that a missing or corrupt file
/// is the default, never an error.
///
/// **Not `Eq`** since #203 added `mouse_wheel_sensitivity: f32` — `f32` has no
/// `Eq` impl (`NaN != NaN`), so the struct can no longer derive it. Nothing
/// depended on `Options: Eq` before this (checked: no `HashSet`/`BTreeMap`
/// keyed on it), only `PartialEq`, which every test here already uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// The user's chosen `gui_scale`: [`AUTO_GUI_SCALE`] or an explicit
    /// ceiling. This is fed to [`calculate_gui_scale`] against the live
    /// framebuffer size — never used directly as a pixel count.
    pub gui_scale: u32,
    /// The rebindable action → input table (`docs/keybindings.md`).
    ///
    /// [`Keybinds`] is deliberately `Copy` (a fixed array, not a map) so this
    /// struct stays `Copy` and the menu layer that reads it by value does not
    /// have to change.
    pub keybinds: Keybinds,
    /// Vanilla's **View Bobbing** option (`options.viewBobbing`,
    /// vanilla's own persisted-options declarations, a boolean option defaulting to on), which
    /// lives on the Accessibility screen in 26.2.
    ///
    /// Gates the walking bob only. Vanilla's own per-frame render pass
    /// applies its damage-tilt bob **unconditionally** and only the walking bob behind this flag,
    /// so a player who turns bobbing off still gets the damage tilt — that is
    /// vanilla's split, not an oversight here. The damage tilt has its own
    /// separate accessibility option — [`Self::damage_tilt_strength`].
    ///
    /// Default **on**, matching vanilla. See `docs/view-bobbing.md`.
    pub view_bobbing: bool,
    /// Vanilla's **Damage Tilt** accessibility option (its own persisted-options
    /// declarations name it `damageTiltStrength`,
    /// a `0.0..=1.0` unit double defaulting to `1.0`), which scales
    /// vanilla's own damage-hit tilt linearly.
    ///
    /// `0.0` must genuinely disable the tilt rather than merely shrink it — that is
    /// the accessibility contract, and it is what
    /// `crate::camera_rig::BobFrame::hurt_roll_degrees` multiplies by. It does
    /// **not** disable the death roll, which vanilla applies unscaled.
    ///
    /// Clamped on load: a mangled file must not be able to produce a tilt larger
    /// than vanilla can, and a negative value would tilt the camera the wrong way.
    pub damage_tilt_strength: f32,
    /// Vanilla's **Panorama Scroll Speed** accessibility option
    /// (its own persisted-options declarations name it `panoramaSpeed`, a `0.0..=1.0` unit double defaulting to `1.0`
    /// with the plain percent-value label), which scales the title screen's
    /// cubemap yaw rate.
    ///
    /// `0.0` is a *stationary* panorama, which is the point of the option — the
    /// spin is what makes the title screen unusable for some players — so unlike
    /// [`Self::damage_tilt_strength`] zero is not an "off" state and the label
    /// reads `0%`.
    ///
    /// Consumed by `crate::menu::panorama::PanoramaRenderer::set_speed`, reached
    /// through `crate::menu::render::MenuFrame::panorama_speed`. Clamped on load
    /// for `damage_tilt_strength`'s reason: vanilla cannot produce a rate outside
    /// this range, and a negative one would spin the sky backwards.
    pub panorama_speed: f32,
    /// Vanilla's `key.sneak` toggle option (its own persisted-options
    /// declarations name it `toggleCrouch`, issue #202): sneak is hold-to-activate when
    /// `false` (vanilla's own default) and press-to-toggle when `true`. Fed to
    /// [`lodestone_controller::InputState::set_toggle_modes`].
    pub toggle_sneak: bool,
    /// As [`Self::toggle_sneak`], for `key.sprint` (vanilla's own `toggleSprint`).
    pub toggle_sprint: bool,
    /// As [`Self::toggle_sneak`], for `key.attack` (vanilla's own `toggleAttack`).
    pub toggle_attack: bool,
    /// As [`Self::toggle_sneak`], for `key.use` (vanilla's own `toggleUse`).
    pub toggle_use: bool,
    /// Vanilla's `options.autoJump`, default `false`.
    pub auto_jump: bool,
    /// Vanilla's `options.sprintWindow`, an
    /// a clamped range `0..=10` with default `7`. `0` disables double-tap sprint.
    pub sprint_window_ticks: u8,
    /// Vanilla's `options.invertMouseX`, default `false`.
    /// Fed to [`lodestone_controller::apply_look_inverted`].
    pub invert_mouse_x: bool,
    /// Vanilla's `options.invertMouseY`, default `false`.
    pub invert_mouse_y: bool,
    /// Vanilla's `options.discreteMouseScroll`, default
    /// `false`: collapse a scroll delta to its **sign** before
    /// [`Self::mouse_wheel_sensitivity`] scales it, so a high-resolution trackpad
    /// moves one notch's worth per gesture instead of a proportional amount.
    ///
    /// Applied at the input boundary in `app/lifecycle.rs`, because that is where
    /// vanilla applies it: `MouseHandler.onScroll` computes
    /// `(discreteScroll ? signum(yoffset) : yoffset) * scrollSensitivity` **once**
    /// and hands the result to both
    /// `screen().mouseScrolled(..)` and the hotbar's `ScrollWheelHandler`. It is
    /// therefore not a list-specific or hotbar-specific transform — it is what a
    /// wheel notch *is* once the options are honoured, which is why it wraps the
    /// raw delta rather than living inside either consumer.
    pub discrete_mouse_scroll: bool,
    /// Vanilla's `options.mouseWheelSensitivity`: a
    /// multiplier on the raw scroll delta before it reaches slot selection
    ///. Default `1.0` — vanilla's own default is
    /// `logMouse(0) == 10^(0/100) == 1.0`, i.e. no
    /// scaling, which is why `1.0` (not `0.0`) is what an absent/corrupt key
    /// degrades to.
    pub mouse_wheel_sensitivity: f32,
    /// Vanilla's `options.chat.scale`, `0.0..=1.0`,
    /// default `1.0`. Read by [`crate::hud::ChatDisplayOptions::scale`] as a
    /// pose-scale multiplier on the chat log and input line.
    pub chat_scale: f32,
    /// Vanilla's `options.chat.width`, `0.0..=1.0`,
    /// default `1.0`. Fed through vanilla's own chat-width calculation, reproduced as
    /// `crate::hud::chat_width_px`, to size the chat box.
    pub chat_width: f32,
    /// Vanilla's `options.chat.height.unfocused`,
    /// `0.0..=1.0`, default vanilla's own unfocused-height fraction =
    /// `70.0/160.0` — how tall the scrollback
    /// is while the chat box is **closed**.
    pub chat_height_unfocused: f32,
    /// As [`Self::chat_height_unfocused`], while the chat box is **open**
    /// (`options.chat.height.focused`, vanilla's own persisted-options declarations). Default `1.0`.
    pub chat_height_focused: f32,
    /// Vanilla's `options.chat.line_spacing`,
    /// `0.0..=1.0`, default `0.0`. Extra fraction of a line's height inserted
    /// between chat rows.
    pub chat_line_spacing: f32,
    /// Vanilla's `options.chat.opacity`, `0.0..=1.0`,
    /// default `1.0`. Chat **text** alpha is `chat_opacity * 0.9 + 0.1`
    /// — never fully transparent, matching vanilla.
    pub chat_opacity: f32,
    /// Vanilla's `options.accessibility.text_background_opacity`
    ///, `0.0..=1.0`, default `0.5`. Vanilla shares
    /// this one knob between chat and several other translucent panels; this
    /// client only has a consumer for the chat background so far
    ///, hence the chat-scoped name here.
    pub chat_background_opacity: f32,
    /// Vanilla's `options.chat.color`, default `true`.
    /// `false` strips every legacy `§` code before drawing a scrollback line
    /// (matching vanilla's own color-code-stripping helper) —
    /// it does not affect the input line, which never carries codes.
    pub chat_colors: bool,
    /// Vanilla's own subtitle-caption toggle, default `false` —
    /// the accessibility toggle for the sound-subtitle caption overlay.
    /// Vanilla exposes it on **two** settings pages, Sound and
    /// Accessibility, both writing this one field.
    pub show_subtitles: bool,
    /// Vanilla's look **sensitivity** (`options.sensitivity`,
    /// vanilla's own persisted-options declarations): a unit-interval double, `0.0..=1.0`, default `0.5`. The
    /// displayed label is `percentValueLabel(caption, 2.0 * value)`, so the
    /// default reads **100%** and the maximum **200%** — the stored number is
    /// half the percentage a player sees.
    ///
    /// **This field is the migration in issue #443.** It used to live only on
    /// [`Config`], parsed from argv every run and never written back, so the
    /// settings row for it had to stay inactive: a row that appeared to set it
    /// would have been fabricated persistence. The consumer already existed
    /// (`sim/step.rs`'s `apply_mouse`), which is what made this the highest-value
    /// remaining migration rather than a new feature.
    ///
    /// `--sensitivity` on argv still wins for that run — see
    /// [`Config::resolve_persisted`].
    pub sensitivity: f32,
    /// Vanilla's `options.renderDistance`:
    /// a clamped range from 2 to either 32 or 16 depending on whether the large-distances
    /// branch is taken, default `12` chunks.
    ///
    /// Vanilla does not apply the value on every frame the slider moves —
    /// it commits this one
    /// **600 ms after the drag stops** rather than per-frame, because each
    /// change reloads chunks. This client applies it on the next launch instead
    /// (see [`Config::resolve_persisted`]) — a real difference from vanilla,
    /// recorded rather than hidden.
    ///
    /// Same migration as [`Self::sensitivity`]; consumers already existed at
    /// `sim/build.rs`'s world radius and `sim/camera.rs`'s fog.
    pub render_distance: u32,
    /// Vanilla's `options.advancedItemTooltips`, toggled by
    /// **F3+H** and by nothing else.
    ///
    /// # Why this is an option and not a debug flag
    ///
    /// F3+H reads like a debug chord alongside F3+B and F3+G, and it is bound the
    /// same way — but the two siblings flip transient render state while this one
    /// flips a *persisted* option. `ItemStack.getTooltipLines` takes a
    /// `TooltipFlag`, and `Minecraft` supplies
    /// `options.advancedItemTooltips ? TooltipFlag.Default.ADVANCED :
    /// TooltipFlag.Default.NORMAL`, so the chord is a *writer* of this field and
    /// the tooltip builder is its only reader. Storing it in an `AtomicBool` on
    /// `WindowApp` (which is what the hitbox and chunk-border chords do) would
    /// lose it on every restart, which vanilla does not.
    ///
    /// Deliberately **no settings row**: vanilla has none either — there is no
    /// `advancedItemTooltips` entry on any `OptionsSubScreen`, so adding a
    /// `LiveOption` for it would be this client inventing a control. The chord is
    /// the whole UI.
    ///
    /// Default `false`, matching vanilla's boot value.
    pub advanced_item_tooltips: bool,
    /// F3+P — vanilla's own pause-on-lost-focus option: whether losing window focus pauses
    /// the game (vanilla's own debug-key handling has a dedicated arm for the chord). Read by
    /// `WindowEvent::Focused(false)` in `app/lifecycle.rs`, which otherwise
    /// pauses unconditionally.
    ///
    /// Default `true`, matching vanilla's boot value — the client pauses on
    /// focus loss unless a player explicitly turns it off with the chord.
    pub pause_on_lost_focus: bool,
    /// Vanilla's eleven `soundSource.*` sliders (vanilla's own persisted-options declarations
    /// build each as a unit-interval double defaulting to `1.0` for
    /// every bus), indexed by
    /// `lodestone_model::event::SoundCategory::ordinal` — see
    /// [`SOUND_CATEGORY_NAMES`] for the order and the file keys.
    ///
    /// One array rather than eleven named fields because the consumer is itself
    /// an array: `lodestone_audio::CategoryVolumes` stores `[f32; 11]` on the
    /// same ordinal, so a per-field struct would only add eleven places for the
    /// two orders to disagree. It also keeps [`Options`] `Copy`.
    ///
    /// Pushed to the mixer every frame by `Sim::set_sound_volumes`; the final
    /// gain a sound is played at is **not** this number — it is
    /// `CategoryVolumes::gain`, which is source volume times master volume for every
    /// bus except `Master` itself (vanilla's own final-volume asymmetry: master
    /// is not squared).
    pub sound_volumes: [f32; 11],
    /// Vanilla's **FOV** option, the vertical field of
    /// view in **degrees** — an a clamped range `30..=110` defaulting to `70`, not a
    /// a unit-interval double. See [`MIN_FOV`].
    ///
    /// Reaches `lodestone_render::Camera::fov_y_degrees` through
    /// `Sim::set_fov_y_degrees` → `camera_rig::build_camera`, which pinned it to
    /// the module constant `camera_rig::FOV_Y_DEGREES` before this field existed.
    /// Pushed per frame, because vanilla applies this one immediately (unlike
    /// [`Self::render_distance`], whose `applyValueImmediately` is `false`).
    ///
    /// It composes with, and is never overwritten by, the spyglass zoom:
    /// `camera_rig::apply_spyglass_fov` multiplies whatever this produced by
    /// `0.1`, so scoping at FOV 30 is a 3° view and at 110 an 11° one — vanilla's
    /// own behaviour, since its modifier is a multiplier on `options.fov` too.
    ///
    /// Clamped on load and again at the setter: a hand-edited `0` would build a
    /// degenerate projection matrix, which blanks the frame rather than looking
    /// wrong.
    pub fov: u32,
    /// Vanilla's **Glint Speed** accessibility option, a
    /// a unit-interval double defaulting to `0.5` — how fast the enchantment shimmer scrolls
    /// across an item.
    ///
    /// The default is not incidental: it is the same number as
    /// `lodestone_render::glint::DEFAULT_SPEED`, because that constant *is*
    /// vanilla's shipped option value. So a stored `0.5` and the frozen constant
    /// are byte-identical, which is exactly why a gate for this option has to pick
    /// something else.
    ///
    /// `0.0` is a legitimate value and a **stationary** shimmer, not an absent
    /// one — the whole point of the accessibility option — so unlike
    /// [`Self::damage_tilt_strength`] zero is not an off state.
    pub glint_speed: f32,
    /// Vanilla's **Glint Strength** accessibility option,
    /// a unit-interval double defaulting to `0.75` — the shimmer's alpha
    /// (`GlintAlpha`), matching `lodestone_render::glint::DEFAULT_STRENGTH`.
    ///
    /// `0.0` removes the shimmer entirely, which is what a player sensitive to it
    /// wants; there is no separate on/off toggle in vanilla either.
    pub glint_strength: f32,
    /// Vanilla's **Clouds** option (vanilla's own persisted-options declarations,
    /// a cycle button over its own cloud-status enum, default `FANCY`) — off, fast, or fancy.
    ///
    /// Persisted as the string `"off"`/`"fast"`/`"fancy"`, matching vanilla's own
    /// serialized-name lookup, rather than as the enum's ordinal: the file
    /// is hand-editable, and a number would make the three states unguessable *and*
    /// silently remap if a variant were ever inserted.
    ///
    /// Reaches `lodestone_render::SkyFrame::with_cloud_status`, which had **zero**
    /// production callers — the sky pass always built `CloudStatus::default()`, so
    /// FAST geometry existed, was pixel-gated, and no player could select it.
    ///
    /// `Off` is a variant of `lodestone_render::CloudStatus` rather than a skip in
    /// the shell's pass; that enum's own doc records why.
    pub cloud_status: lodestone_render::CloudStatus,
    /// Vanilla's **Max Framerate** option (`options.framerateLimit`,
    /// vanilla's own persisted-options declarations): a clamped range `10..=260`, default `120`, where
    /// `260` ([`UNLIMITED_FRAMERATE_CUTOFF`]) means "Unlimited" — a sentinel
    /// value, not a special enum state (vanilla's own per-tick loop gates its
    /// frame-rate limiter on `framerateLimit < 260`).
    ///
    /// This is the **raw stored fps**, not the slider's `1..=26` pre-image —
    /// see `menu::options::INT_RANGE_SLIDERS`'s `"framerateLimit"` row for the
    /// `*10` xmap between the two.
    ///
    /// Consumed by [`crate::app::pacing::effective_target_fps`], which folds
    /// this together with [`Self::inactivity_fps_limit`]'s AFK clock into one
    /// target the frame pacer schedules against — see that function's doc for
    /// why the two compose rather than one overriding the other.
    pub framerate_limit: u32,
    /// Vanilla's **VSync** option (`options.vsync`, vanilla's own persisted-options declarations),
    /// default `true`. Vanilla reacts to a change by invalidating its own
    /// surface configuration; this client's equivalent
    /// is `WindowApp::sync_vsync_present_mode`, which polls this field once per
    /// presented frame and hands it to
    /// `lodestone_render::SurfaceTarget::set_present_mode` — see that method's
    /// doc for why polling a pure field into a GPU setter is safe here (the
    /// equality guard inside it).
    ///
    /// **Composes with [`Self::framerate_limit`], it does not gate it**:
    /// vanilla applies its own frame-rate limiter whenever
    /// `framerateLimit < 260` **unconditionally**, vsync on or off
    /// — vanilla's own client entry point has no vsync check at all — the two are
    /// independent throttles the client is subject to simultaneously, and this
    /// client reproduces that rather than inventing a precedence between them.
    pub enable_vsync: bool,
    /// Vanilla's **Reduce FPS when** option (`options.inactivityFpsLimit`).
    /// See [`InactivityFpsLimit`]'s own doc for what it actually gates here.
    pub inactivity_fps_limit: InactivityFpsLimit,
    /// Vanilla's **Preset** slider (`options.graphics.preset`,
    /// vanilla's own persisted-options declarations), default `Fancy`. See [`GraphicsPreset`]'s doc
    /// for the three fields this client's preset actually writes, and why.
    pub graphics_preset: GraphicsPreset,
    /// Vanilla's **See-Through Leaves** option (`options.cutoutLeaves`,
    /// vanilla's own persisted-options declarations), default `true` (holes visible — vanilla's
    /// FANCY/FABULOUS behaviour).
    ///
    /// `false` is vanilla's FAST behaviour: leaves render through the *solid*
    /// pass, which skips the alpha test entirely, so the same cutout texture's
    /// holes paint solid instead of see-through. Reaches
    /// `lodestone_render::models::ModelVertex::cutout_bypass` through
    /// `mesher::SnapshotModelView::force_opaque_at` — see that field's doc for
    /// why this is a per-vertex render-pass bypass rather than a second
    /// occlusion bake. Changing this forces a remesh of every loaded column
    /// (`Sim::set_cutout_leaves`), matching vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)`.
    pub cutout_leaves: bool,
    /// Vanilla's **Mipmap Levels** option (`options.mipmapLevels`,
    /// vanilla's own persisted-options declarations): a clamped range `0..=4`, default
    /// [`lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS`] — the same number
    /// this field defaults to, so a fresh install's stored value and the
    /// render crate's own fallback constant are one fact, not two.
    ///
    /// The block/model atlas's requested mip depth. Reaches
    /// `crate::resources::set_mipmap_levels` from
    /// `menu::nav::MenuNav`'s slider-drag and click-step writers, which bumps
    /// the same `pack_generation` counter a resource-pack selection change
    /// does — so dragging this slider rebuilds the atlas, remeshes every
    /// loaded column and swaps the GPU bind groups through the identical
    /// live-reload path a resource-pack selection change already built, not a
    /// second one. See `crate::resources::mipmap_levels`'s doc for the read
    /// side.
    pub mipmap_levels: u32,
    /// Vanilla's **Entity Shadows** option (`options.entityShadows`,
    /// vanilla's own persisted-options declarations), default `true`.
    ///
    /// Reaches `RenderState::set_entity_shadows_enabled`, polled every frame
    /// exactly like [`Self::cutout_leaves`] (`app/redraw.rs`, beside
    /// `Sim::set_cutout_leaves`) rather than fired only on toggle — a plain
    /// bool write is cheap enough that the equality-guard trick that field's
    /// own doc explains is not needed here.
    pub entity_shadows: bool,
    /// Vanilla's **Weather Effect Radius** option (`options.weatherRadius`,
    /// vanilla's own persisted-options declarations): a clamped range `3..=10`, default `10`, measured in **blocks**
    /// (`en_us.json`'s `options.blocks` is `"%s Blocks"`, not `"%s Chunks"` —
    /// its neighbour `cloudRange` is the chunk-denominated one).
    ///
    /// The half-width of the square of columns the rain/snow pass walks around
    /// the camera. Reaches `lodestone_render::extract_columns` and
    /// `lodestone_render::column_instance` through
    /// `app::weather::weather_columns_for_frame`, which took
    /// [`lodestone_render::DEFAULT_WEATHER_RADIUS`] as a literal at both call
    /// sites before this field existed — the "correct function fed a constant by
    /// its producer" shape, not a missing consumer. It reaches both, because the
    /// second is what fades a column's alpha out toward the radius: passing the
    /// option to only the extraction would draw fewer columns *and* fade them at
    /// the wrong distance.
    pub weather_radius: i32,
    /// Vanilla's **Menu Background Blur** option
    /// (`options.menuBackgroundBlurriness`, vanilla's own persisted-options declarations): a clamped range `0..=10`,
    /// default `5`. Placed on **two** pages, Video and Accessibility, exactly as
    /// vanilla places it — both rows drive this one field.
    ///
    /// The box-blur half-width for the pass behind an in-game menu. Reaches
    /// `menu::render::MenuRenderer::set_blur_radius` →
    /// `menu::render::blur::MenuBlur::set_radius`, polled once per presented
    /// frame in `app/redraw.rs` beside `MenuRenderer::begin_frame`. The pass
    /// existed, was pixel-gated, and ran at a frozen `BLUR_RADIUS` constant
    /// whose own module doc said wiring the option was "a matter of threading a
    /// radius into `config_h`/`config_v`" — this is that.
    pub menu_background_blurriness: u32,
    /// Vanilla's **Attack Indicator** option (`options.attackIndicator`,
    /// vanilla's own persisted-options declarations), default [`AttackIndicator::Crosshair`].
    ///
    /// Reaches `hud::HudFrame::attack_indicator`, which the crosshair and hotbar
    /// draw sites in `hud::HudGeometry::build_inner` each gate on — vanilla's own
    /// two `if` in `Hud.extractCrosshair` and the hotbar section. Before this
    /// field the crosshair bar drew unconditionally, i.e. the client behaved as
    /// though the option were pinned to `Crosshair`, which the draw site's own
    /// comment said in as many words.
    pub attack_indicator: AttackIndicator,
    /// Vanilla's **Particles** option (`options.particles`, vanilla's own persisted-options declarations),
    /// default [`ParticleLevel::All`].
    ///
    /// Pushed into the sim by `Sim::set_particle_level` once per presented
    /// frame and read at the one place vanilla reads it —
    /// `ClientLevel.doAddParticle`'s equivalent in `sim::net_apply`, which
    /// already transcribed that function's *other* half (the 32-block cutoff
    /// and its `overrideLimiter` bypass) and was missing only the level test.
    pub particles: ParticleLevel,
    /// Vanilla's **Biome Blend** option (`options.biomeBlendRadius`,
    /// vanilla's own persisted-options declarations): a clamped range `0..=7`, default `2`. The stored number is the
    /// window **radius**; the row's label shows the width `2r + 1`.
    ///
    /// The half-width of the square of biomes each tinted vertex averages its
    /// grass/foliage/water colour over. Reaches
    /// `lodestone_render::biome_tint::BlendedTintCursor::new` through
    /// `mesher::mesh_one`, whose three view constructors all took
    /// `lodestone_render::biome_tint::BLEND_RADIUS` — the render crate's own doc
    /// for that constant already said it was the default "unless a caller has
    /// an actual video-settings" value, and no caller did.
    ///
    /// Changing this forces a remesh of every loaded column
    /// (`Sim::set_blend_radius`), matching vanilla's own
    /// `operateOnLevelExtractor(LevelExtractor::allChanged)` — the blend is
    /// baked per vertex, so there is no uniform to update in place.
    pub biome_blend_radius: i32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            gui_scale: AUTO_GUI_SCALE,
            keybinds: Keybinds::new(),
            view_bobbing: true,
            damage_tilt_strength: 1.0,
            panorama_speed: 1.0,
            toggle_sneak: false,
            toggle_sprint: false,
            toggle_attack: false,
            toggle_use: false,
            auto_jump: false,
            sprint_window_ticks: lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS,
            invert_mouse_x: false,
            invert_mouse_y: false,
            discrete_mouse_scroll: false,
            mouse_wheel_sensitivity: 1.0,
            chat_scale: 1.0,
            chat_width: 1.0,
            chat_height_unfocused: 70.0 / 160.0,
            chat_height_focused: 1.0,
            chat_line_spacing: 0.0,
            chat_opacity: 1.0,
            chat_background_opacity: 0.5,
            chat_colors: true,
            show_subtitles: false,
            sensitivity: DEFAULT_SENSITIVITY,
            render_distance: DEFAULT_RENDER_DISTANCE,
            advanced_item_tooltips: false,
            pause_on_lost_focus: true,
            sound_volumes: [1.0; 11],
            fov: DEFAULT_FOV,
            glint_speed: lodestone_render::glint::DEFAULT_SPEED as f32,
            glint_strength: lodestone_render::glint::DEFAULT_STRENGTH,
            // `CloudStatus::default()` is `Fancy`, vanilla's own default — named
            // through `Default` rather than spelled out so the two cannot drift.
            cloud_status: lodestone_render::CloudStatus::default(),
            framerate_limit: DEFAULT_FRAMERATE_LIMIT,
            enable_vsync: true,
            inactivity_fps_limit: InactivityFpsLimit::default(),
            graphics_preset: GraphicsPreset::default(),
            cutout_leaves: true,
            mipmap_levels: lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS,
            entity_shadows: true,
            weather_radius: MAX_WEATHER_RADIUS,
            menu_background_blurriness: DEFAULT_MENU_BACKGROUND_BLURRINESS,
            attack_indicator: AttackIndicator::default(),
            particles: ParticleLevel::default(),
            biome_blend_radius: DEFAULT_BIOME_BLEND_RADIUS,
        }
    }
}

impl Options {
    /// Loads from the real on-disk location ([`options_path`]). Missing or
    /// corrupt is the default, never an error — a broken settings file must
    /// not stop the game from launching, same rule as the server list.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&options_path())
    }

    /// As [`Options::load`], from an explicit path (for tests, so nothing
    /// touches the developer's real settings file).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        // See `Self::save_to` on why this is `crate::platform::store` rather than
        // `std::fs`. Missing *and* refused both fall back to the default here, which
        // is this method's documented contract — the distinction between the two is
        // preserved where it can be acted on, in `save_to`'s `Result`.
        crate::platform::store::read_text(path)
            .map_or_else(|_| Self::default(), |t| Self::from_json(&t))
    }

    fn from_json(text: &str) -> Self {
        let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(text) else {
            return Self::default();
        };
        let gui_scale = obj
            .get("gui_scale")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(AUTO_GUI_SCALE);
        // A missing `keybinds` key is the vanilla defaults, and so is a
        // malformed one — `Keybinds::from_json_value` degrades per-entry and
        // never fails, so one stale binding cannot cost the whole table (let
        // alone `gui_scale`, read above and independent of it).
        let keybinds = obj
            .get("keybinds")
            .map_or_else(Keybinds::new, Keybinds::from_json_value);
        // Absent or malformed is **on**, because that is vanilla's default —
        // note this is the opposite rule from the deleted `unlock_framerate`
        // knob, whose default was off. A degrade-to-`false` here would silently
        // turn a real setting off for anyone whose file got mangled.
        let view_bobbing = obj
            .get("view_bobbing")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        // Absent or malformed is vanilla's `1.0`, for `view_bobbing`'s reason, and
        // **clamped** rather than merely defaulted: a hand-edited `50.0` would
        // otherwise produce a 700-degree camera roll on every hit, and a negative
        // value would roll it the wrong way. NaN fails the `contains` test and
        // therefore falls back too.
        let damage_tilt_strength = obj
            .get("damage_tilt_strength")
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| (0.0..=1.0).contains(v))
            .unwrap_or(1.0);
        // Same shape and same clamp as `damage_tilt_strength` above. NaN fails
        // `contains` and falls back, which matters more here than it looks: a NaN
        // speed would poison `PanoramaRenderer`'s accumulated spin permanently,
        // and a NaN yaw builds a NaN view matrix rather than a wrong one.
        let panorama_speed = obj
            .get("panorama_speed")
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| (0.0..=1.0).contains(v))
            .unwrap_or(1.0);
        // Absent or malformed is `false` for both — vanilla's own default is
        // hold mode, so a mangled file must not silently switch a player onto
        // toggle mode they never asked for.
        let toggle_sneak = obj
            .get("toggle_sneak")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let toggle_sprint = obj
            .get("toggle_sprint")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let toggle_attack = obj
            .get("toggle_attack")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let toggle_use = obj
            .get("toggle_use")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let auto_jump = obj
            .get("auto_jump")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let sprint_window_ticks = obj
            .get("sprint_window_ticks")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v.min(10) as u8)
            .unwrap_or(lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS);
        let invert_mouse_x = obj
            .get("invert_mouse_x")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let invert_mouse_y = obj
            .get("invert_mouse_y")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let discrete_mouse_scroll = obj
            .get("discrete_mouse_scroll")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Degrades to `1.0` (no scaling), not `0.0` — a `0.0` multiplier would
        // silently disable the scroll wheel entirely for anyone whose file
        // got mangled, which is a far worse failure than "sensitivity reset
        // to the default".
        let mouse_wheel_sensitivity = obj
            .get("mouse_wheel_sensitivity")
            .and_then(serde_json::Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(1.0);
        // The five `0.0..=1.0` chat sliders all degrade the same way: a
        // missing, non-finite, or out-of-range value falls back to the
        // vanilla default rather than propagating a value the draw site would
        // have to re-clamp (and risk forgetting to).
        let unit = |key: &str, default: f32| -> f32 {
            obj.get(key)
                .and_then(serde_json::Value::as_f64)
                .map(|v| v as f32)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .unwrap_or(default)
        };
        let chat_scale = unit("chat_scale", 1.0);
        let chat_width = unit("chat_width", 1.0);
        let chat_height_unfocused = unit("chat_height_unfocused", 70.0 / 160.0);
        let chat_height_focused = unit("chat_height_focused", 1.0);
        let chat_line_spacing = unit("chat_line_spacing", 0.0);
        let chat_opacity = unit("chat_opacity", 1.0);
        let chat_background_opacity = unit("chat_background_opacity", 0.5);
        // Absent or malformed is **on** — vanilla's own default — same rule as
        // `view_bobbing`: a mangled file must not silently strip every
        // colour code from chat.
        let chat_colors = obj
            .get("chat_colors")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        // Absent or malformed is **off**, vanilla's own default — the mirror of
        // `chat_colors` above, whose default is on.
        let show_subtitles = obj
            .get("show_subtitles")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // A unit-interval double, so the same `0.0..=1.0` rule as the chat sliders —
        // reusing `unit` rather than restating the range, because a hand-written
        // second copy is how the two would drift.
        let sensitivity = unit("sensitivity", DEFAULT_SENSITIVITY);
        // Clamped to vanilla's own a clamped range `2..=32` rather
        // than merely parsed: an out-of-range value here reaches
        // `sim/build.rs`'s world radius and `sim/camera.rs`'s fog, and a 0 would
        // generate no chunks at all — a hand-edited file must not be able to
        // produce a black screen.
        let render_distance = obj
            .get("render_distance")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| (MIN_RENDER_DISTANCE..=MAX_RENDER_DISTANCE).contains(v))
            .unwrap_or(DEFAULT_RENDER_DISTANCE);
        let advanced_item_tooltips = obj
            .get("advanced_item_tooltips")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        // Absent or malformed is **on** — vanilla's own default
        // (vanilla's own persisted-options declarations's `pauseOnLostFocus = true`).
        let pause_on_lost_focus = obj
            .get("pause_on_lost_focus")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        // The eleven sound sliders, through the same `unit` rule as the chat
        // ones: every bus is a unit-interval double defaulting to `1.0`, so an absent,
        // non-finite or out-of-range value degrades to full volume rather than
        // to silence. Silence is the wrong degradation here for the reason
        // `chat_colors` documents in reverse — a mangled file must not leave a
        // player wondering why the game has no sound.
        let mut sound_volumes = [1.0f32; 11];
        for (slot, name) in sound_volumes.iter_mut().zip(SOUND_CATEGORY_NAMES) {
            *slot = unit(&format!("sound_volume_{name}"), 1.0);
        }
        // Rejected rather than clamped to a neighbour, the same rule
        // `render_distance` follows and for the same reason: landing on the
        // default tells a reader the value was refused, where a clamp to 30 would
        // look like a legitimate choice nobody made. A `0` here would build a
        // degenerate projection and blank the frame.
        let fov = obj
            .get("fov")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| (MIN_FOV..=MAX_FOV).contains(v))
            .unwrap_or(DEFAULT_FOV);
        // Both unit-interval doubles, so the chat sliders' `unit` rule applies. The
        // defaults come from `lodestone_render::glint`'s own constants rather than
        // from literals here, because those constants *are* vanilla's shipped
        // option values — a second copy would be a fact declared twice.
        let glint_speed = unit(
            "glint_speed",
            lodestone_render::glint::DEFAULT_SPEED as f32,
        );
        let glint_strength = unit(
            "glint_strength",
            lodestone_render::glint::DEFAULT_STRENGTH,
        );
        let cloud_status = obj
            .get("cloud_status")
            .and_then(serde_json::Value::as_str)
            .and_then(cloud_status_from_name)
            .unwrap_or_default();
        // Clamped to vanilla's own persisted-options range `10..=260` — a mangled file
        // must not be able to produce a limit below the floor, and rounding to
        // the nearest `*10` bucket keeps a hand-edited value that fell between
        // two buckets from silently teleporting the slider handle.
        let framerate_limit = obj
            .get("framerate_limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .map(|v| (v / 10 * 10).clamp(MIN_FRAMERATE_LIMIT, UNLIMITED_FRAMERATE_CUTOFF))
            .unwrap_or(DEFAULT_FRAMERATE_LIMIT);
        // Absent or malformed is **on** — vanilla's own default
        //, `view_bobbing`'s reason.
        let enable_vsync = obj
            .get("enable_vsync")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let inactivity_fps_limit = obj
            .get("inactivity_fps_limit")
            .and_then(serde_json::Value::as_str)
            .and_then(inactivity_fps_limit_from_name)
            .unwrap_or_default();
        let graphics_preset = obj
            .get("graphics_preset")
            .and_then(serde_json::Value::as_str)
            .and_then(graphics_preset_from_name)
            .unwrap_or_default();
        // Absent or malformed is **on** — vanilla's own default
        //, `view_bobbing`'s reason again: a mangled file
        // must not silently switch a player onto FAST's solid leaves.
        let cutout_leaves = obj
            .get("cutout_leaves")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        // Clamped to vanilla's own a clamped range `0..=4` — the same bound
        // `menu::options::INT_RANGE_SLIDERS`' `"mipmapLevels"` row places the
        // handle with, and the same one `crate::resources::set_mipmap_levels`
        // enforces on the live-write side. A hand-edited out-of-range value
        // would otherwise reach `AtlasBuilder::with_mip_levels` directly.
        let mipmap_levels = obj
            .get("mipmap_levels")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .filter(|v| *v <= lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS)
            .unwrap_or(lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS);
        // Absent or malformed is **on** — vanilla's own default
        //, `cutout_leaves`'s reason again: a mangled file
        // must not silently hide every mob's shadow.
        let entity_shadows = obj
            .get("entity_shadows")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        // Clamped to vanilla's own a clamped range `3..=10` — `mipmap_levels`' reason
        // above. A hand-edited `0` would otherwise reach `extract_columns` and
        // silently stop all precipitation drawing, which reads as a render bug
        // rather than as a settings-file mistake.
        let weather_radius = obj
            .get("weather_radius")
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .map_or(MAX_WEATHER_RADIUS, |v| {
                v.clamp(MIN_WEATHER_RADIUS, MAX_WEATHER_RADIUS)
            });
        // Clamped to vanilla's own a clamped range `0..=10`, `weather_radius`'s reason
        // above — a hand-edited 200 would reach the blur shader's inner loop as
        // a 401-tap filter per pass, six passes deep.
        let menu_background_blurriness = obj
            .get("menu_background_blurriness")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .map_or(DEFAULT_MENU_BACKGROUND_BLURRINESS, |v| {
                v.clamp(
                    MIN_MENU_BACKGROUND_BLURRINESS,
                    MAX_MENU_BACKGROUND_BLURRINESS,
                )
            });
        // Absent or unrecognised is vanilla's own `CROSSHAIR`, which is also the
        // behaviour every build before this field had.
        let attack_indicator = obj
            .get("attack_indicator")
            .and_then(serde_json::Value::as_str)
            .and_then(attack_indicator_from_name)
            .unwrap_or_default();
        // Absent or unrecognised is vanilla's own `ALL`, which is also the
        // behaviour every build before this field had.
        let particles = obj
            .get("particles")
            .and_then(serde_json::Value::as_str)
            .and_then(particle_level_from_name)
            .unwrap_or_default();
        // Clamped to vanilla's own a clamped range `0..=7` — which is also
        // `BlendRowCursor`'s ring capacity, so this clamp is a memory bound as
        // well as a fidelity one. `BlendedTintCursor::new` clamps again on its
        // own side; both are cheap and neither is the other's excuse.
        let biome_blend_radius = obj
            .get("biome_blend_radius")
            .and_then(serde_json::Value::as_i64)
            .and_then(|v| i32::try_from(v).ok())
            .map_or(DEFAULT_BIOME_BLEND_RADIUS, |v| {
                v.clamp(MIN_BIOME_BLEND_RADIUS, MAX_BIOME_BLEND_RADIUS)
            });
        Self {
            gui_scale,
            keybinds,
            view_bobbing,
            damage_tilt_strength,
            panorama_speed,
            toggle_sneak,
            toggle_sprint,
            toggle_attack,
            toggle_use,
            auto_jump,
            sprint_window_ticks,
            invert_mouse_x,
            invert_mouse_y,
            discrete_mouse_scroll,
            mouse_wheel_sensitivity,
            chat_scale,
            chat_width,
            chat_height_unfocused,
            chat_height_focused,
            chat_line_spacing,
            chat_opacity,
            chat_background_opacity,
            chat_colors,
            show_subtitles,
            sensitivity,
            render_distance,
            advanced_item_tooltips,
            pause_on_lost_focus,
            sound_volumes,
            fov,
            glint_speed,
            glint_strength,
            cloud_status,
            framerate_limit,
            enable_vsync,
            inactivity_fps_limit,
            graphics_preset,
            cutout_leaves,
            mipmap_levels,
            entity_shadows,
            weather_radius,
            menu_background_blurriness,
            attack_indicator,
            particles,
            biome_blend_radius,
        }
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&options_path())
    }

    /// As [`Options::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        // `crate::platform::store`, not `std::fs`: a browser has no filesystem, so
        // every read would miss and every write would fail, and the player would lose
        // all 44 live option rows on reload with no error anywhere. The browser arm is
        // `localStorage`, keyed by this same path. See that module.
        let mut obj = serde_json::Map::new();
        obj.insert("gui_scale".into(), self.gui_scale.into());
        // Written only when something was actually rebound, so an untouched
        // install has no `keybinds` key at all rather than a noisy block of
        // defaults — see `Keybinds::to_json_value` for why defaults are omitted.
        let keybinds = self.keybinds.to_json_value();
        if !keybinds.as_object().is_some_and(serde_json::Map::is_empty) {
            obj.insert("keybinds".into(), keybinds);
        }
        // Written only when it differs from the default, same rule as
        // `keybinds`: an untouched install has no key for it.
        if !self.view_bobbing {
            obj.insert("view_bobbing".into(), false.into());
        }
        // Written only when it is not vanilla's default, matching `view_bobbing`
        // above: an untouched config stays free of keys nobody set.
        if self.damage_tilt_strength != 1.0 {
            obj.insert(
                "damage_tilt_strength".into(),
                f64::from(self.damage_tilt_strength).into(),
            );
        }
        if self.panorama_speed != 1.0 {
            obj.insert(
                "panorama_speed".into(),
                f64::from(self.panorama_speed).into(),
            );
        }
        if self.toggle_sneak {
            obj.insert("toggle_sneak".into(), true.into());
        }
        if self.toggle_sprint {
            obj.insert("toggle_sprint".into(), true.into());
        }
        if self.toggle_attack {
            obj.insert("toggle_attack".into(), true.into());
        }
        if self.toggle_use {
            obj.insert("toggle_use".into(), true.into());
        }
        if self.auto_jump {
            obj.insert("auto_jump".into(), true.into());
        }
        if self.sprint_window_ticks != lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS {
            obj.insert("sprint_window_ticks".into(), (self.sprint_window_ticks as u64).into());
        }
        if self.invert_mouse_x {
            obj.insert("invert_mouse_x".into(), true.into());
        }
        if self.invert_mouse_y {
            obj.insert("invert_mouse_y".into(), true.into());
        }
        if self.discrete_mouse_scroll {
            obj.insert("discrete_mouse_scroll".into(), true.into());
        }
        if (self.mouse_wheel_sensitivity - 1.0).abs() > f32::EPSILON {
            obj.insert(
                "mouse_wheel_sensitivity".into(),
                (self.mouse_wheel_sensitivity as f64).into(),
            );
        }
        // The eleven sound sliders. Written per-bus and only when the bus is not
        // at vanilla's `1.0`, so an untouched install has no `sound_volume_*` key
        // at all — the same rule every other option here follows, and the reason
        // it matters more for this group than for any other: eleven keys is over
        // a third of the file.
        //
        // A direct insert rather than `put_unit` below, because the key is
        // composed at runtime and that closure takes a `&'static str`.
        for (index, name) in SOUND_CATEGORY_NAMES.iter().enumerate() {
            let value = self.sound_volumes[index];
            if (value - 1.0).abs() > f32::EPSILON {
                obj.insert(format!("sound_volume_{name}"), f64::from(value).into());
            }
        }
        let default = Self::default();
        let mut put_unit = |key: &'static str, value: f32, default: f32| {
            if (value - default).abs() > f32::EPSILON {
                obj.insert(key.into(), (value as f64).into());
            }
        };
        put_unit("chat_scale", self.chat_scale, default.chat_scale);
        put_unit("chat_width", self.chat_width, default.chat_width);
        put_unit(
            "chat_height_unfocused",
            self.chat_height_unfocused,
            default.chat_height_unfocused,
        );
        put_unit(
            "chat_height_focused",
            self.chat_height_focused,
            default.chat_height_focused,
        );
        put_unit(
            "chat_line_spacing",
            self.chat_line_spacing,
            default.chat_line_spacing,
        );
        put_unit("chat_opacity", self.chat_opacity, default.chat_opacity);
        put_unit(
            "chat_background_opacity",
            self.chat_background_opacity,
            default.chat_background_opacity,
        );
        put_unit("glint_speed", self.glint_speed, default.glint_speed);
        put_unit(
            "glint_strength",
            self.glint_strength,
            default.glint_strength,
        );
        // Before the `chat_colors` insert below, because `put_unit` holds a
        // mutable borrow of `obj` and its last use has to precede any direct
        // insert.
        put_unit("sensitivity", self.sensitivity, default.sensitivity);
        if !self.chat_colors {
            obj.insert("chat_colors".into(), false.into());
        }
        if self.show_subtitles {
            obj.insert("show_subtitles".into(), true.into());
        }
        if self.render_distance != default.render_distance {
            obj.insert("render_distance".into(), self.render_distance.into());
        }
        if self.advanced_item_tooltips != default.advanced_item_tooltips {
            obj.insert(
                "advanced_item_tooltips".into(),
                self.advanced_item_tooltips.into(),
            );
        }
        if self.pause_on_lost_focus != default.pause_on_lost_focus {
            obj.insert(
                "pause_on_lost_focus".into(),
                self.pause_on_lost_focus.into(),
            );
        }
        if self.fov != default.fov {
            obj.insert("fov".into(), self.fov.into());
        }
        if self.cloud_status != default.cloud_status {
            obj.insert(
                "cloud_status".into(),
                cloud_status_name(self.cloud_status).into(),
            );
        }
        if self.framerate_limit != default.framerate_limit {
            obj.insert("framerate_limit".into(), self.framerate_limit.into());
        }
        if !self.enable_vsync {
            obj.insert("enable_vsync".into(), false.into());
        }
        if self.inactivity_fps_limit != default.inactivity_fps_limit {
            obj.insert(
                "inactivity_fps_limit".into(),
                inactivity_fps_limit_name(self.inactivity_fps_limit).into(),
            );
        }
        if self.graphics_preset != default.graphics_preset {
            obj.insert(
                "graphics_preset".into(),
                graphics_preset_name(self.graphics_preset).into(),
            );
        }
        if !self.cutout_leaves {
            obj.insert("cutout_leaves".into(), false.into());
        }
        if self.mipmap_levels != default.mipmap_levels {
            obj.insert("mipmap_levels".into(), self.mipmap_levels.into());
        }
        if !self.entity_shadows {
            obj.insert("entity_shadows".into(), false.into());
        }
        if self.weather_radius != default.weather_radius {
            obj.insert("weather_radius".into(), self.weather_radius.into());
        }
        if self.biome_blend_radius != default.biome_blend_radius {
            obj.insert(
                "biome_blend_radius".into(),
                self.biome_blend_radius.into(),
            );
        }
        if self.particles != default.particles {
            obj.insert("particles".into(), particle_level_name(self.particles).into());
        }
        if self.attack_indicator != default.attack_indicator {
            obj.insert(
                "attack_indicator".into(),
                attack_indicator_name(self.attack_indicator).into(),
            );
        }
        if self.menu_background_blurriness != default.menu_background_blurriness {
            obj.insert(
                "menu_background_blurriness".into(),
                self.menu_background_blurriness.into(),
            );
        }
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .unwrap_or_else(|_| "{}".to_string());
        crate::platform::store::write_text(path, &text)
    }
}

/// Full path to the persisted options file — alongside `servers.json` in the
/// same platform data directory. Deliberately reuses
/// [`crate::menu::servers::data_dir`]'s discovery (and its
/// `LODESTONE_DATA_DIR` override) rather than inventing a second one.
#[must_use]
pub fn options_path() -> PathBuf {
    crate::menu::servers::data_dir().join("options.json")
}

/// The Social Interactions screen's per-player "Hide in Chat"
/// choices, keyed by UUID. **Not** part of [`Options`]: `Options` derives
/// `Copy` deliberately (see its own doc — "the menu layer that reads it by
/// value does not have to change"), and a `Vec` field would take that away
/// from every existing call site that copies an `Options` by value. A second
/// small file, alongside `options.json`/`servers.json`, is the same trade
/// [`crate::menu::servers::ServerList`] and `menu/accounts.rs`'s profile list
/// already made rather than growing one shared struct without bound.
///
/// Persisting a hidden choice cannot be "wrong" the way a cycled option value
/// can — see `docs/social-interactions.md`'s note on self-healing: toggling a
/// player hidden or shown is a deliberate, reversible click each time, with
/// no derived state that could drift from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HiddenPlayers {
    ids: std::collections::BTreeSet<uuid::Uuid>,
}

impl HiddenPlayers {
    /// Loads from the real on-disk location ([`hidden_players_path`]).
    /// Missing or corrupt is empty, never an error — same rule as
    /// [`Options::load`].
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&hidden_players_path())
    }

    /// As [`Self::load`], from an explicit path (for tests).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        // `crate::platform::store` — see `crate::config::Options::save_to`.
        let Ok(text) = crate::platform::store::read_text(path) else {
            return Self::default();
        };
        let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&text) else {
            return Self::default();
        };
        let ids = items
            .into_iter()
            .filter_map(|v| v.as_str().and_then(|s| uuid::Uuid::parse_str(s).ok()))
            .collect();
        Self { ids }
    }

    #[must_use]
    pub fn contains(&self, id: uuid::Uuid) -> bool {
        self.ids.contains(&id)
    }

    /// Flips `id`'s hidden state. Does not persist by itself — see
    /// [`Self::save_to`], called separately so a test can inspect the
    /// in-memory state without touching disk.
    pub fn toggle(&mut self, id: uuid::Uuid) {
        if !self.ids.remove(&id) {
            self.ids.insert(id);
        }
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&hidden_players_path())
    }

    /// As [`Self::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        // `crate::platform::store` — see `crate::config::Options::save_to`.
        let items: Vec<serde_json::Value> = self
            .ids
            .iter()
            .map(|id| serde_json::Value::String(id.to_string()))
            .collect();
        let text = serde_json::to_string_pretty(&serde_json::Value::Array(items))
            .unwrap_or_else(|_| "[]".to_string());
        crate::platform::store::write_text(path, &text)
    }
}

/// Full path to the persisted hidden-players file — same directory
/// discovery as [`options_path`]/`servers_path`.
#[must_use]
pub fn hidden_players_path() -> PathBuf {
    crate::menu::servers::data_dir().join("hidden_players.json")
}

/// The Resource Packs screen's ordered selection, **highest
/// priority first** — the same order the screen's Selected column shows
/// top-to-bottom, and the order
/// [`lodestone_assets::ResourceManager::from_priority_order`] documents.
///
/// A separate file for the same reason [`HiddenPlayers`] is one: [`Options`] is
/// deliberately `Copy` and a `Vec<String>` field would take that away from every
/// call site that copies it by value. Vanilla keeps this in `options.txt` as
/// `resourcePacks:[...]`; this client keeps it beside `options.json` instead,
/// which is the same trade `servers.json` and the profile list already made.
///
/// The **built-in pack is not in this list.** It is pinned to the bottom of the
/// stack by construction in `resources.rs`, exactly as vanilla's own
/// fixed-position `Pack.Position.BOTTOM` built-in pack is
///, so there is no state here that could ever deselect it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectedPacks {
    ids: Vec<String>,
}

impl SelectedPacks {
    /// Loads from the real on-disk location ([`selected_packs_path`]). Missing
    /// or corrupt is empty, never an error — same rule as [`Options::load`].
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&selected_packs_path())
    }

    /// As [`Self::load`], from an explicit path (for tests).
    #[must_use]
    pub fn load_from(path: &Path) -> Self {
        // `crate::platform::store` — see `crate::config::Options::save_to`.
        let Ok(text) = crate::platform::store::read_text(path) else {
            return Self::default();
        };
        let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text)
        else {
            return Self::default();
        };
        let mut ids = Vec::new();
        for value in items {
            if let Some(id) = value.as_str() {
                let id = id.to_string();
                // A duplicate would load the same pack twice at two priorities.
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Self { ids }
    }

    /// Wraps an already-ordered id list, highest priority first.
    #[must_use]
    pub fn from_ids(ids: Vec<String>) -> Self {
        Self { ids }
    }

    /// The ids, highest priority first.
    #[must_use]
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Consumes into the id list, highest priority first.
    #[must_use]
    pub fn into_ids(self) -> Vec<String> {
        self.ids
    }

    /// Writes to the real on-disk location.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&selected_packs_path())
    }

    /// As [`Self::save`], to an explicit path (for tests).
    ///
    /// # Errors
    /// Returns the underlying I/O error if the directory cannot be created or
    /// the file cannot be written.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        // `crate::platform::store` — see `crate::config::Options::save_to`.
        let items: Vec<serde_json::Value> = self
            .ids
            .iter()
            .map(|id| serde_json::Value::String(id.clone()))
            .collect();
        let text = serde_json::to_string_pretty(&serde_json::Value::Array(items))
            .unwrap_or_else(|_| "[]".to_string());
        crate::platform::store::write_text(path, &text)
    }
}

/// Full path to the persisted resource-pack selection — same directory
/// discovery as [`options_path`]/`servers_path`.
#[must_use]
pub fn selected_packs_path() -> PathBuf {
    crate::menu::servers::data_dir().join("resource_packs.json")
}

/// How the binary should run this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Open a window and run the interactive game loop.
    Window,
    /// Run headless: bring up the GPU, render one frame of the local world to an
    /// offscreen target, read the pixels back, and print the debug stats. This
    /// is the evidence path when no window server is reachable.
    Headless,
    /// Connect to the server, stream events for a bounded time, print them, and
    /// exit. Proves the live pipeline end to end without a GPU.
    Connect,
    /// A real, persistent session — ticks, connects, keeps the
    /// event loop alive — with **no** presentation attached at start: no
    /// window, no GPU, no `PresentationSet` systems. Unlike [`Mode::Headless`]
    /// this is not a one-shot evidence path; it stays running until the
    /// process is told to attach a window (`app::AppEvent::AttachPresentation`,
    /// driven by `app::runners::run_headless_session`'s stdin control thread)
    /// or to quit.
    #[cfg(feature = "runtime-presentation")]
    HeadlessSession,
}

/// The live client workload selected by the opt-in frame benchmark driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkWorkload {
    /// Normal generated terrain, with a stationary segment followed by flight.
    Terrain,
    /// A dense Java-authored scene of specialized render paths.
    Showcase,
    /// A large late-season multiplayer save, with dense stationary and flight arms.
    Megaworld,
    /// Stampy's Lovelier World, viewed from an open-air large-build waypoint.
    Lovelier,
}

impl BenchmarkWorkload {
    /// Stable name written into frame-profile labels and result metadata.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Terrain => "terrain",
            Self::Showcase => "showcase",
            Self::Megaworld => "megaworld",
            Self::Lovelier => "lovelier",
        }
    }
}

/// Whether a benchmark includes the F3 text overlay's measurable observer cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkDebugOverlay {
    Closed,
    Open,
}

/// Durations and workload for one deterministic live frame benchmark session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchmarkConfig {
    /// Which Java-backed scene the runner prepared.
    pub workload: BenchmarkWorkload,
    /// Explicit F3 state; result metadata must never infer this from timings.
    pub debug_overlay: BenchmarkDebugOverlay,
    /// Joined-world settling time excluded from reported measurements.
    pub warmup: Duration,
    /// Fixed-view measurement duration.
    pub stationary: Duration,
    /// Terrain-flight or showcase-orbit measurement duration.
    pub moving: Duration,
}

impl BenchmarkConfig {
    /// Physical framebuffer requested for every canonical benchmark run.
    pub const PHYSICAL_SIZE: (u32, u32) = (2560, 1440);

    const DEFAULT_WARMUP: Duration = Duration::from_secs(20);
    const DEFAULT_STATIONARY: Duration = Duration::from_secs(30);
    const DEFAULT_MOVING: Duration = Duration::from_secs(60);
}

/// Parsed shell configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// What to do this run.
    pub mode: Mode,
    /// Server host to connect to (when connecting).
    pub host: String,
    /// Server port.
    pub port: u16,
    /// Protocol *number* to request an adapter for. `776` is vanilla 26.2.
    pub protocol: i32,
    /// Render distance in chunks (drives the camera far plane and worldgen span).
    pub render_distance: u32,
    /// Whether to also open a live connection while the window is up.
    pub connect_in_window: bool,
    /// Whether argv actually named a connection target (`--host` or `--port`),
    /// as opposed to [`Self::host`]/[`Self::port`] merely holding their defaults.
    ///
    /// Recorded as its own flag because the *value* cannot answer the question:
    /// `--host 127.0.0.1 --port 25565` is byte-identical to passing nothing, and
    /// `app::requested_a_connection` used to compare against `Config::default()`
    /// and therefore sent that launch to the main menu instead of the server the
    /// user named. Not set by `--live`, which has its own flag.
    pub address_given: bool,
    /// How long the `Connect` mode streams events before exiting.
    pub connect_for: Duration,
    /// Mouse-look sensitivity as a vanilla `0..1` slider (fed through the cubic
    /// response curve in [`lodestone_controller::sensitivity_factor`]). `0.5` is the
    /// vanilla default and yields `0.15°`/pixel.
    pub sensitivity: f32,
    /// Whether argv actually named `--sensitivity`, as opposed to
    /// [`Self::sensitivity`] merely holding its default.
    ///
    /// Exactly [`Self::address_given`]'s shape and for exactly its reason: the
    /// *value* cannot answer the question, because `--sensitivity 0.5` is
    /// byte-identical to passing nothing. Without this flag
    /// [`Self::resolve_persisted`] could not tell "the user asked for the default
    /// this run" from "the user said nothing", and would overwrite an explicit
    /// flag with `options.json`.
    pub sensitivity_given: bool,
    /// Whether argv actually named `--render-distance`/`--rd`. See
    /// [`Self::sensitivity_given`].
    pub render_distance_given: bool,
    /// Opt-in deterministic live frame benchmark. `None` for ordinary play.
    pub benchmark: Option<BenchmarkConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Window,
            host: "127.0.0.1".into(),
            port: 25565,
            protocol: 776,
            render_distance: DEFAULT_RENDER_DISTANCE,
            connect_in_window: false,
            address_given: false,
            connect_for: Duration::from_secs(15),
            sensitivity: DEFAULT_SENSITIVITY,
            sensitivity_given: false,
            render_distance_given: false,
            benchmark: None,
        }
    }
}

impl Config {
    /// The outcome of parsing argv: either a runnable [`Config`], a request to
    /// print `--help`, or an error for an unrecognised argument. Help and errors
    /// are resolved by `main` **before** any window, GPU, or world init, so the
    /// binary is discoverable and `./lodestone --help` never opens a window.
    ///
    /// Recognised flags:
    /// `--headless`, `--connect`, `--window`, `--host <h>`, `--port <p>`,
    /// `--protocol <n>`, `--render-distance <n>`, `--live` (connect while
    /// windowed), `--seconds <n>`, `--sensitivity <f>`, the benchmark options,
    /// and `--help`/`-h`.
    #[must_use]
    pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> CliOutcome {
        let mut cfg = Config::default();
        let mut it = args.into_iter();
        let mut benchmark_workload = None;
        let mut benchmark_option_seen = false;
        let mut benchmark_debug_overlay_seen = false;
        let mut benchmark_debug_overlay = BenchmarkDebugOverlay::Closed;
        let mut benchmark_warmup = BenchmarkConfig::DEFAULT_WARMUP;
        let mut benchmark_stationary = BenchmarkConfig::DEFAULT_STATIONARY;
        let mut benchmark_moving = BenchmarkConfig::DEFAULT_MOVING;
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--help" | "-h" => return CliOutcome::Help(Self::usage()),
                "--headless" => cfg.mode = Mode::Headless,
                "--connect" => cfg.mode = Mode::Connect,
                #[cfg(feature = "runtime-presentation")]
                "--headless-session" => cfg.mode = Mode::HeadlessSession,
                "--window" => cfg.mode = Mode::Window,
                "--live" => cfg.connect_in_window = true,
                "--host" => {
                    if let Some(v) = it.next() {
                        cfg.host = v;
                        cfg.address_given = true;
                    }
                }
                "--port" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.port = v;
                        cfg.address_given = true;
                    }
                }
                "--protocol" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.protocol = v;
                    }
                }
                "--render-distance" | "--rd" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.render_distance = v;
                        cfg.render_distance_given = true;
                    }
                }
                "--seconds" => {
                    if let Some(v) = it.next().and_then(|v| v.parse::<u64>().ok()) {
                        cfg.connect_for = Duration::from_secs(v);
                    }
                }
                "--sensitivity" => {
                    if let Some(v) = it.next().and_then(|v| v.parse().ok()) {
                        cfg.sensitivity = v;
                        cfg.sensitivity_given = true;
                    }
                }
                "--benchmark" => {
                    benchmark_option_seen = true;
                    let Some(value) = it.next() else {
                        return CliOutcome::Error(
                            "--benchmark requires terrain, showcase, megaworld, or lovelier".into(),
                        );
                    };
                    benchmark_workload = Some(match value.as_str() {
                        "terrain" => BenchmarkWorkload::Terrain,
                        "showcase" => BenchmarkWorkload::Showcase,
                        "megaworld" => BenchmarkWorkload::Megaworld,
                        "lovelier" => BenchmarkWorkload::Lovelier,
                        _ => {
                            return CliOutcome::Error(format!(
                                "--benchmark requires terrain, showcase, megaworld, or lovelier, got {value}"
                            ));
                        }
                    });
                    cfg.mode = Mode::Window;
                    cfg.connect_in_window = true;
                }
                "--benchmark-warmup" => {
                    benchmark_option_seen = true;
                    let Some(value) = it.next() else {
                        return CliOutcome::Error("--benchmark-warmup requires a value in seconds".into());
                    };
                    let Ok(seconds) = value.parse::<u64>() else {
                        return CliOutcome::Error(format!(
                            "--benchmark-warmup requires a value in seconds, got {value}"
                        ));
                    };
                    benchmark_warmup = Duration::from_secs(seconds);
                }
                "--benchmark-stationary" => {
                    benchmark_option_seen = true;
                    let Some(value) = it.next() else {
                        return CliOutcome::Error(
                            "--benchmark-stationary requires a value in seconds".into(),
                        );
                    };
                    let Ok(seconds) = value.parse::<u64>() else {
                        return CliOutcome::Error(format!(
                            "--benchmark-stationary requires a value in seconds, got {value}"
                        ));
                    };
                    benchmark_stationary = Duration::from_secs(seconds);
                }
                "--benchmark-moving" => {
                    benchmark_option_seen = true;
                    let Some(value) = it.next() else {
                        return CliOutcome::Error("--benchmark-moving requires a value in seconds".into());
                    };
                    let Ok(seconds) = value.parse::<u64>() else {
                        return CliOutcome::Error(format!(
                            "--benchmark-moving requires a value in seconds, got {value}"
                        ));
                    };
                    benchmark_moving = Duration::from_secs(seconds);
                }
                "--benchmark-debug-overlay" => {
                    benchmark_option_seen = true;
                    benchmark_debug_overlay_seen = true;
                    let Some(value) = it.next() else {
                        return CliOutcome::Error(
                            "--benchmark-debug-overlay requires open or closed".into(),
                        );
                    };
                    benchmark_debug_overlay = match value.as_str() {
                        "closed" => BenchmarkDebugOverlay::Closed,
                        "open" => BenchmarkDebugOverlay::Open,
                        _ => {
                            return CliOutcome::Error(format!(
                                "--benchmark-debug-overlay requires open or closed, got {value}"
                            ));
                        }
                    };
                }
                other => {
                    return CliOutcome::Error(format!("unrecognised argument: {other}"));
                }
            }
        }
        if benchmark_option_seen {
            let Some(workload) = benchmark_workload else {
                if benchmark_debug_overlay_seen {
                    return CliOutcome::Error(
                        "--benchmark-debug-overlay options require --benchmark".into(),
                    );
                }
                return CliOutcome::Error(
                    "benchmark duration options require --benchmark terrain, showcase, megaworld, or lovelier"
                        .into(),
                );
            };
            cfg.benchmark = Some(BenchmarkConfig {
                workload,
                debug_overlay: benchmark_debug_overlay,
                warmup: benchmark_warmup,
                stationary: benchmark_stationary,
                moving: benchmark_moving,
            });
        }
        CliOutcome::Run(cfg)
    }

    /// Fold the persisted [`Options`] into this argv-parsed [`Config`], for
    /// every setting that now lives in both — migration.
    ///
    /// **Precedence: an explicit flag wins for that run.** `--render-distance 4`
    /// is a debugging and benchmarking affordance, and having `options.json`
    /// silently override it would make every measurement taken with that flag a
    /// lie. A flag that was *not* given loses to the persisted value, which is
    /// the whole point of the migration.
    ///
    /// **Why the resolution is here and not at each consumer.** `sensitivity` is
    /// read by `sim/step.rs`'s `apply_mouse` and `render_distance` by
    /// `sim/build.rs`'s world radius, `sim/camera.rs`'s fog and four `app/*`
    /// call sites. Resolving once, into the fields those seven sites already
    /// read, means the migration adds **no** new consumer and cannot produce the
    /// island where a settings row writes a field nothing reads. The
    /// alternative — teaching each site to consult both structs — is seven
    /// chances to miss one, in files a settings change has no business touching.
    ///
    /// **Known limitation, stated rather than implied:** this runs once, at
    /// launch, so a change made in the settings screen takes effect on the
    /// **next** launch. For `render_distance` that is close to vanilla, which
    /// also defers (`applyValueImmediately = false`, a 600 ms debounce, because
    /// each change reloads chunks). For `sensitivity` it is *not* — vanilla
    /// applies that immediately. Closing that gap means pushing the value into
    /// `Sim` each frame the way `set_mouse_invert` already does from
    /// `app/redraw.rs`, which is a brokered file and deliberately not touched
    /// here.
    pub fn resolve_persisted(&mut self, options: &Options) {
        if !self.sensitivity_given {
            self.sensitivity = options.sensitivity;
        }
        if !self.render_distance_given {
            self.render_distance = options.render_distance;
        }
    }

    /// The `--help` usage text. Kept in one place so the flag list can't drift
    /// from the parser above.
    #[must_use]
    pub fn usage() -> String {
        #[cfg_attr(not(feature = "runtime-presentation"), allow(unused_mut))]
        let mut text = "\
lodestone — a multi-version Minecraft Java client (game shell)

USAGE:
    lodestone [OPTIONS]

MODES (default: --window):
    --window                 Open a window and play the interactive game loop
    --headless               Render one offscreen frame, print debug stats, exit
    --connect                Stream live server events for a bounded time, exit
    --live                   Also open a live connection while windowed

CONNECTION:
    Naming either of these connects on launch and skips the main menu — even when
    the value given is the default one.
    --host <HOST>            Server host (default: 127.0.0.1)
    --port <PORT>            Server port (default: 25565)
    --protocol <N>           Protocol number to request an adapter for
                             (default: 776 = vanilla 26.2). Requires the `live`
                             build feature for an adapter to be compiled in.
    --seconds <N>            How long --connect streams before exiting (default: 15)

RENDER / INPUT:
    --render-distance <N>    Render distance in chunks (default: 8); also --rd
    --sensitivity <F>        Mouse-look sensitivity, 0..1 (default: 0.5)

LIVE FRAME BENCHMARK:
    --benchmark <WORKLOAD>   terrain, showcase, megaworld, or lovelier; forces a windowed run
    --benchmark-debug-overlay <STATE>
                             closed or open (default: closed)
    --benchmark-warmup <N>  Joined-world warm-up seconds (default: 20)
    --benchmark-stationary <N>
                             Fixed-view measurement seconds (default: 30)
    --benchmark-moving <N>  Flight/orbit measurement seconds (default: 60)

    -h, --help               Print this help and exit
"
        .to_string();
        // A separate `push_str` rather than folded into the
        // literal above so the flag's presence in `--help` tracks the same
        // `cfg` that gates parsing it (`Self::from_args`'s `--headless-session`
        // arm) and `Mode::HeadlessSession` itself — advertising a flag that
        // does not exist would be worse than the extra `cfg`.
        #[cfg(feature = "runtime-presentation")]
        text.push_str(
            "\nRUNTIME PRESENTATION:\n    \
             --headless-session       Persistent session, no window until you type `attach`\n",
        );
        text
    }
}

/// The result of parsing command-line arguments.
#[derive(Debug, Clone)]
pub enum CliOutcome {
    /// Parsed successfully; run the shell with this config.
    Run(Config),
    /// `--help`/`-h` was requested; the payload is the usage text to print.
    Help(String),
    /// An argument was not recognised; the payload is the error message.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(a: &[&str]) -> Config {
        match Config::from_args(a.iter().map(|s| (*s).to_string())) {
            CliOutcome::Run(c) => c,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_window_and_262() {
        let c = Config::default();
        assert_eq!(c.mode, Mode::Window);
        assert_eq!(c.protocol, 776);
        assert_eq!(c.port, 25565);
    }

    #[test]
    fn flags_parse() {
        let c = parse(&[
            "--headless",
            "--host",
            "example",
            "--port",
            "1234",
            "--rd",
            "16",
        ]);
        assert_eq!(c.mode, Mode::Headless);
        assert_eq!(c.host, "example");
        assert_eq!(c.port, 1234);
        assert_eq!(c.render_distance, 16);
    }

    #[test]
    fn connect_and_live() {
        let c = parse(&["--connect", "--seconds", "3", "--live"]);
        assert_eq!(c.mode, Mode::Connect);
        assert_eq!(c.connect_for.as_secs(), 3);
        assert!(c.connect_in_window);
    }

    #[test]
    fn benchmark_flags_build_a_live_windowed_run_without_changing_defaults() {
        let normal = parse(&[]);
        assert_eq!(normal.benchmark, None);

        let terrain = parse(&[
            "--benchmark",
            "terrain",
            "--benchmark-warmup",
            "20",
            "--benchmark-stationary",
            "30",
            "--benchmark-moving",
            "60",
        ]);
        assert_eq!(terrain.mode, Mode::Window);
        assert!(
            terrain.connect_in_window,
            "a live benchmark must dial its configured Java oracle"
        );
        assert_eq!(
            terrain.benchmark,
            Some(BenchmarkConfig {
                workload: BenchmarkWorkload::Terrain,
                debug_overlay: BenchmarkDebugOverlay::Closed,
                warmup: Duration::from_secs(20),
                stationary: Duration::from_secs(30),
                moving: Duration::from_secs(60),
            })
        );
    }

    #[test]
    fn benchmark_rejects_unknown_workloads_and_missing_durations() {
        assert!(matches!(
            Config::from_args(["--benchmark".into(), "castle".into()]),
            CliOutcome::Error(message) if message.contains("terrain, showcase, megaworld, or lovelier")
        ));
        assert!(matches!(
            Config::from_args(["--benchmark-warmup".into()]),
            CliOutcome::Error(message) if message.contains("requires a value")
        ));
    }

    #[test]
    fn megaworld_and_explicit_debug_overlay_policy_parse() {
        let open = parse(&[
            "--benchmark",
            "megaworld",
            "--benchmark-debug-overlay",
            "open",
        ]);
        let benchmark = open.benchmark.expect("benchmark config");
        assert_eq!(benchmark.workload, BenchmarkWorkload::Megaworld);
        assert_eq!(benchmark.debug_overlay, BenchmarkDebugOverlay::Open);

        let closed = parse(&[
            "--benchmark",
            "megaworld",
            "--benchmark-debug-overlay",
            "closed",
        ]);
        assert_eq!(
            closed.benchmark.expect("benchmark config").debug_overlay,
            BenchmarkDebugOverlay::Closed
        );
    }

    #[test]
    fn lovelier_large_world_workload_parses() {
        let parsed = parse(&["--benchmark", "lovelier"]);
        assert_eq!(
            parsed.benchmark.expect("benchmark config").workload,
            BenchmarkWorkload::Lovelier
        );
    }

    #[test]
    fn debug_overlay_policy_requires_a_benchmark_and_a_known_value() {
        assert!(matches!(
            Config::from_args([
                "--benchmark-debug-overlay".into(),
                "open".into(),
            ]),
            CliOutcome::Error(message) if message.contains("require --benchmark")
        ));
        assert!(matches!(
            Config::from_args([
                "--benchmark".into(),
                "megaworld".into(),
                "--benchmark-debug-overlay".into(),
                "sometimes".into(),
            ]),
            CliOutcome::Error(message) if message.contains("open or closed")
        ));
    }

    #[test]
    fn spelling_out_the_default_address_still_counts_as_asking_for_a_connection() {
        // The launch behind the two-worlds report: `app::requested_a_connection`
        // compared the *values* against `Config::default()`, so this argv was
        // indistinguishable from passing nothing and landed on the main menu.
        let c = parse(&["--host", "127.0.0.1", "--port", "25565"]);
        assert_eq!(c.host, Config::default().host, "same value as the default");
        assert_eq!(c.port, Config::default().port, "same value as the default");
        assert!(
            c.address_given,
            "the flag was seen, which is the question the menu bypass asks"
        );
        // Control: no address flag at all must stay false, or the field is a
        // constant `true` and cannot distinguish anything.
        assert!(!parse(&["--window"]).address_given);
        assert!(
            !parse(&["--live"]).address_given,
            "--live has its own flag; it must not imply an address was named"
        );
    }

    #[test]
    fn bad_values_keep_defaults() {
        let c = parse(&["--port", "notanumber"]);
        assert_eq!(c.port, 25565);
    }

    #[test]
    fn help_flag_requests_help_before_anything_runs() {
        // Both spellings must short-circuit to Help, never a runnable Config —
        // this is what stops `./lodestone --help` from opening a window.
        for flag in ["--help", "-h"] {
            match Config::from_args([flag.to_string()]) {
                CliOutcome::Help(text) => {
                    // The usage must actually document the flags, not be a stub,
                    // or it's the "green output that isn't evidence" shape.
                    assert!(text.contains("USAGE"), "usage missing header: {text}");
                    assert!(text.contains("--headless"), "usage omits --headless");
                    assert!(text.contains("--host"), "usage omits --host");
                }
                other => panic!("expected Help for {flag}, got {other:?}"),
            }
        }
    }

    #[test]
    fn unrecognised_argument_errors_rather_than_being_ignored() {
        // A stray/unknown token must be an explicit error (resolved before init)
        // rather than silently dropped — otherwise typos launch the default run.
        match Config::from_args(["--frobnicate".to_string()]) {
            CliOutcome::Error(msg) => assert!(msg.contains("--frobnicate"), "msg: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
        match Config::from_args(["stray".to_string()]) {
            CliOutcome::Error(msg) => assert!(msg.contains("stray"), "msg: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -- GUI scale ----------------------------------------------------------
    //
    // Expected values are hand-derived from vanilla's own algebra (the largest
    // integer S with `fb_w/S >= 320` and `fb_h/S >= 240`), not by re-tracing
    // this implementation, so a bug shared between the spec-reading and the
    // port would not cancel out.

    #[test]
    fn auto_scale_matches_vanillas_default_854x480_window() {
        // Vanilla's own default window is 854x480. Height is the binding
        // constraint: the largest S with 480/S >= 240 is S=2 (480/3=160 fails);
        // width allows up to S=2 as well (854/3=284 < 320). So S=2.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 854, 480), 2);
    }

    #[test]
    fn auto_scale_at_1280x720() {
        // Height binds: largest S with 720/S >= 240 is S=3 (720/4=180 fails).
        // Width would allow S=4 (1280/4=320 exactly), so height wins: S=3.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 1280, 720), 3);
    }

    #[test]
    fn auto_scale_at_4k_is_the_retina_style_case() {
        // A framebuffer this large is what a HiDPI/Retina display reports for
        // an ordinary-looking window — this is the case the menu's "half size
        // on Retina" report was about. Height binds: largest S with
        // 2160/S >= 240 is S=9 (2160/10=216 fails); width allows S=12
        // (3840/12=320 exactly), so S=9.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 3840, 2160), 9);
    }

    #[test]
    fn a_manual_scale_is_honoured_when_the_window_is_big_enough() {
        assert_eq!(calculate_gui_scale(2, 1280, 720), 2);
        assert_eq!(calculate_gui_scale(5, 3840, 2160), 5);
    }

    #[test]
    fn a_manual_scale_is_clamped_down_by_a_small_window() {
        // 200/2 = 100 < 320, so even a request for scale 2 cannot be honoured;
        // the window is too small for anything but scale 1.
        assert_eq!(calculate_gui_scale(2, 200, 200), 1);
    }

    #[test]
    fn scale_never_drops_below_one_even_for_a_degenerate_framebuffer() {
        // An iconified/minimised window can report 0x0; the menu must not
        // divide by zero laying itself out against that.
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 0, 0), 1);
        assert_eq!(calculate_gui_scale(AUTO_GUI_SCALE, 1, 1), 1);
    }

    // -- persisted options ---------------------------------------------------

    fn temp_options_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-config-{}-{tag}/options.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    #[test]
    fn options_default_to_auto_scale() {
        assert_eq!(Options::default().gui_scale, AUTO_GUI_SCALE);
    }

    #[test]
    fn options_round_trip_through_a_real_file() {
        let path = temp_options_path("roundtrip");
        let opts = Options {
            gui_scale: 3,
            ..Options::default()
        };
        opts.save_to(&path).expect("save should create parents");
        assert_eq!(Options::load_from(&path), opts);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_unknown_key_in_the_file_is_ignored_rather_than_failing_the_load() {
        // The `unlock_framerate` debug knob used to live here and
        // is now deleted, so an install that toggled it still has the key on
        // disk. A stale key must be *ignored*, not turned into a parse failure
        // that silently resets everything else — which is what would happen if
        // `from_json` ever grew a strict/deny-unknown-fields shape.
        let path = temp_options_path("stale-key");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "{\"gui_scale\": 5, \"unlock_framerate\": true, \"nonsense\": [1,2]}",
        )
        .unwrap();
        assert_eq!(
            Options::load_from(&path).gui_scale,
            5,
            "gui_scale must survive keys this version does not know"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn view_bobbing_defaults_on_and_only_writes_a_key_when_turned_off() {
        // The **opposite** default from the deleted `unlock_framerate` knob, and
        // the asymmetry is the point: vanilla ships `options.viewBobbing` on, so
        // a missing or garbled value must read as ON. Degrading to `false` here
        // would silently disable a real setting for anyone whose file got
        // mangled — the failure mode a debug knob is allowed to have and a
        // shipped option is not.
        let path = temp_options_path("view-bobbing");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(Options::default().view_bobbing);

        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("view_bobbing"),
            "the default writes no key: {text}"
        );
        assert!(Options::load_from(&path).view_bobbing);

        let off = Options {
            view_bobbing: false,
            ..Options::default()
        };
        off.save_to(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("view_bobbing"));
        assert_eq!(Options::load_from(&path), off);

        for bad in ["\"false\"", "0", "[]", "null", "{}"] {
            std::fs::write(
                &path,
                format!("{{\"gui_scale\": 5, \"view_bobbing\": {bad}}}"),
            )
            .unwrap();
            let loaded = Options::load_from(&path);
            assert!(
                loaded.view_bobbing,
                "view_bobbing: {bad} must degrade to ON, not OFF"
            );
            assert_eq!(loaded.gui_scale, 5, "gui_scale must survive {bad}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    // F3+P (`KeyOutcome::TogglePauseOnLostFocus`). Same shape as
    // `view_bobbing_defaults_on_and_only_writes_a_key_when_turned_off` and for
    // the same reason: vanilla's `Options.pauseOnLostFocus` defaults `true`,
    // so a missing or garbled value must read as ON, not silently stop
    // pausing on focus loss for anyone whose file got mangled.
    fn pause_on_lost_focus_defaults_on_and_only_writes_a_key_when_turned_off() {
        let path = temp_options_path("pause-on-lost-focus");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert!(Options::default().pause_on_lost_focus);

        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("pause_on_lost_focus"),
            "the default writes no key: {text}"
        );
        assert!(Options::load_from(&path).pause_on_lost_focus);

        let off = Options {
            pause_on_lost_focus: false,
            ..Options::default()
        };
        off.save_to(&path).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pause_on_lost_focus")
        );
        assert_eq!(Options::load_from(&path), off);

        for bad in ["\"false\"", "0", "[]", "null", "{}"] {
            std::fs::write(
                &path,
                format!("{{\"gui_scale\": 5, \"pause_on_lost_focus\": {bad}}}"),
            )
            .unwrap();
            let loaded = Options::load_from(&path);
            assert!(
                loaded.pause_on_lost_focus,
                "pause_on_lost_focus: {bad} must degrade to ON, not OFF"
            );
            assert_eq!(loaded.gui_scale, 5, "gui_scale must survive {bad}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- toggle sneak/sprint/attack/use, auto-jump, mouse invert/sensitivity
    //    (issues #202/#203/#444) ---

    #[test]
    fn toggle_and_invert_default_off_and_write_no_key_when_untouched() {
        let path = temp_options_path("toggle-invert-defaults");
        assert!(!Options::default().toggle_sneak);
        assert!(!Options::default().toggle_sprint);
        assert!(!Options::default().toggle_attack);
        assert!(!Options::default().toggle_use);
        assert!(!Options::default().auto_jump);
        assert!(!Options::default().invert_mouse_x);
        assert!(!Options::default().invert_mouse_y);

        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for key in [
            "toggle_sneak",
            "toggle_sprint",
            "toggle_attack",
            "toggle_use",
            "auto_jump",
            "sprint_window_ticks",
            "invert_mouse_x",
            "invert_mouse_y",
        ] {
            assert!(!text.contains(key), "the default writes no {key} key: {text}");
        }
        assert_eq!(Options::load_from(&path), Options::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn toggle_and_invert_round_trip_and_degrade_to_off() {
        let path = temp_options_path("toggle-invert-roundtrip");
        let on = Options {
            toggle_sneak: true,
            toggle_sprint: true,
            toggle_attack: true,
            toggle_use: true,
            auto_jump: true,
            invert_mouse_x: true,
            invert_mouse_y: true,
            ..Options::default()
        };
        on.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for key in [
            "toggle_sneak",
            "toggle_sprint",
            "toggle_attack",
            "toggle_use",
            "auto_jump",
            "invert_mouse_x",
            "invert_mouse_y",
        ] {
            assert!(text.contains(key), "an explicit `true` must be written: {text}");
        }
        assert_eq!(Options::load_from(&path), on);

        // A malformed value must degrade to `false` (vanilla's own default),
        // never silently flip a player onto a mode they never chose.
        for bad in ["\"true\"", "1", "null", "[]"] {
            std::fs::write(
                &path,
                format!(
                    "{{\"toggle_sneak\": {bad}, \"toggle_sprint\": {bad}, \
                      \"toggle_attack\": {bad}, \"toggle_use\": {bad}, \
                      \"auto_jump\": {bad}, \
                      \"invert_mouse_x\": {bad}, \"invert_mouse_y\": {bad}}}"
                ),
            )
            .unwrap();
            let loaded = Options::load_from(&path);
            assert!(!loaded.toggle_sneak, "toggle_sneak: {bad} must degrade to OFF");
            assert!(!loaded.toggle_sprint, "toggle_sprint: {bad} must degrade to OFF");
            assert!(!loaded.toggle_attack, "toggle_attack: {bad} must degrade to OFF");
            assert!(!loaded.toggle_use, "toggle_use: {bad} must degrade to OFF");
            assert!(!loaded.auto_jump, "auto_jump: {bad} must degrade to OFF");
            assert!(!loaded.invert_mouse_x, "invert_mouse_x: {bad} must degrade to OFF");
            assert!(!loaded.invert_mouse_y, "invert_mouse_y: {bad} must degrade to OFF");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn sprint_window_default_is_vanish_and_round_trips_clamped_to_ten() {
        let path = temp_options_path("sprint-window");
        // Boots at vanilla's shipped 7 (its own persisted-options declarations, a clamped range 0..=10).
        assert_eq!(
            Options::default().sprint_window_ticks,
            lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS
        );
        // Off is a real state (0) that must be written, unlike the default.
        let off = Options {
            sprint_window_ticks: 0,
            ..Options::default()
        };
        off.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("sprint_window_ticks"), "0 must be written: {text}");
        assert_eq!(Options::load_from(&path), off);

        // A value above the slider max clamps to 10, matching `IntRange`.
        let three = Options {
            sprint_window_ticks: 3,
            ..Options::default()
        };
        three.save_to(&path).unwrap();
        std::fs::write(
            &path,
            "{\"sprint_window_ticks\": 99}",
        )
        .unwrap();
        assert_eq!(Options::load_from(&path).sprint_window_ticks, 10);
        // A malformed value degrades to the default, never to 0.
        std::fs::write(&path, "{\"sprint_window_ticks\": \"high\"}").unwrap();
        assert_eq!(
            Options::load_from(&path).sprint_window_ticks,
            lodestone_controller::SPRINT_TRIGGER_WINDOW_TICKS
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn mouse_wheel_sensitivity_defaults_to_one_and_degrades_to_one() {
        let path = temp_options_path("wheel-sensitivity");
        assert_eq!(Options::default().mouse_wheel_sensitivity, 1.0);

        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("mouse_wheel_sensitivity"),
            "the default (1.0, no scaling) writes no key: {text}"
        );

        let custom = Options {
            mouse_wheel_sensitivity: 2.5,
            ..Options::default()
        };
        custom.save_to(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("mouse_wheel_sensitivity"));
        assert_eq!(Options::load_from(&path).mouse_wheel_sensitivity, 2.5);

        // Zero, negative and non-finite must all degrade to 1.0 — a 0.0
        // multiplier would silently disable the scroll wheel entirely, which
        // is a far worse failure than "the setting reset to default".
        for bad in ["0", "-3.0", "\"nan\"", "null"] {
            std::fs::write(&path, format!("{{\"mouse_wheel_sensitivity\": {bad}}}")).unwrap();
            assert_eq!(
                Options::load_from(&path).mouse_wheel_sensitivity,
                1.0,
                "mouse_wheel_sensitivity: {bad} must degrade to 1.0"
            );
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- the eleven `soundSource.*` sliders -----------------------------------

    /// Each bus round-trips through its **own** key, and an untouched install
    /// writes none of the eleven.
    ///
    /// Every value below is distinct and none is `1.0`, which is what makes this
    /// a test rather than a smoke check: eleven copies of one number would pass
    /// with any two keys transposed, and the default `1.0` would pass with the
    /// whole group unread. The per-bus expectation is looked up by
    /// `SOUND_CATEGORY_NAMES`' own index, so a reordering of the array is a
    /// failure here instead of a silent remap.
    #[test]
    fn sound_volumes_round_trip_per_bus_and_stay_out_of_an_untouched_file() {
        let path = temp_options_path("sound-volumes");
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("sound_volume"),
            "an untouched install must write none of the eleven keys: {text}"
        );
        assert_eq!(Options::load_from(&path).sound_volumes, [1.0; 11]);

        let values = [0.5, 0.4, 0.3, 0.2, 0.1, 0.9, 0.8, 0.7, 0.6, 0.35, 0.0];
        let custom = Options {
            sound_volumes: values,
            ..Options::default()
        };
        custom.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for (index, name) in SOUND_CATEGORY_NAMES.iter().enumerate() {
            let key = format!("sound_volume_{name}");
            assert!(text.contains(&key), "{key} missing from {text}");
            let back = Options::from_json(&format!("{{\"{key}\": {}}}", values[index]));
            let mut expected = [1.0f32; 11];
            expected[index] = values[index];
            assert_eq!(
                back.sound_volumes, expected,
                "{key} must set only bus {index}"
            );
        }
        assert_eq!(Options::load_from(&path).sound_volumes, values);

        // Out of range, non-numeric and null all degrade to **full volume**, not
        // to silence: a mangled file must not leave a player hunting a bug in the
        // audio engine.
        for bad in ["-0.5", "1.5", "\"loud\"", "null"] {
            let json = format!("{{\"sound_volume_master\": {bad}}}");
            assert_eq!(
                Options::from_json(&json).sound_volumes[0], 1.0,
                "sound_volume_master: {bad} must degrade to 1.0"
            );
        }
        // The detector works: an in-range 0.0 really does come through, so the
        // clause above is rejecting bad values rather than rejecting everything.
        assert_eq!(
            Options::from_json("{\"sound_volume_master\": 0.0}").sound_volumes[0],
            0.0
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- `fov` --------------------------------------------------------------

    /// `fov` round-trips, stays out of an untouched file, and rejects rather than
    /// clamps a value outside vanilla's a clamped range `30..=110`.
    ///
    /// Rejecting to the **default** rather than to a nearer endpoint is the same
    /// choice `render_distance` makes, and for the same reason: landing on 70 tells
    /// a reader the file was refused, where landing on 30 looks like a setting
    /// somebody chose. (`camera_rig::build_camera` clamps to the endpoints
    /// instead, because by then the value has a live producer that has already
    /// been range-checked.)
    #[test]
    fn fov_round_trips_and_an_out_of_range_value_degrades_to_the_default() {
        let path = temp_options_path("fov");
        assert_eq!(Options::default().fov, DEFAULT_FOV);
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("fov"), "the default writes no key: {text}");

        let custom = Options {
            fov: 95,
            ..Options::default()
        };
        custom.save_to(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("fov"));
        assert_eq!(Options::load_from(&path).fov, 95);

        for bad in ["0", "29", "111", "999", "-30", "\"wide\"", "null"] {
            let json = format!("{{\"fov\": {bad}}}");
            assert_eq!(
                Options::from_json(&json).fov,
                DEFAULT_FOV,
                "{bad} must be rejected, not clamped to a neighbour"
            );
        }
        // The detector works: both endpoints come through.
        for good in [MIN_FOV, 70, MAX_FOV] {
            let json = format!("{{\"fov\": {good}}}");
            assert_eq!(Options::from_json(&json).fov, good);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- `glintSpeed` / `glintStrength` --------------------------------------

    /// Both glint options round-trip, and both boot at the same numbers
    /// `lodestone_render::glint` holds as its constants.
    ///
    /// That last assertion is the load-bearing one and it looks trivial: those two
    /// constants *are* vanilla's shipped option values, so if the defaults here
    /// ever drifted from them an untouched install would silently start shimmering
    /// differently from vanilla with no row on any screen changed.
    #[test]
    fn glint_options_round_trip_and_default_to_the_render_crates_own_constants() {
        let path = temp_options_path("glint");
        let d = Options::default();
        assert_eq!(
            f64::from(d.glint_speed),
            lodestone_render::glint::DEFAULT_SPEED
        );
        assert_eq!(d.glint_strength, lodestone_render::glint::DEFAULT_STRENGTH);

        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("glint"), "the defaults write no key: {text}");

        let custom = Options {
            glint_speed: 0.0,
            glint_strength: 0.25,
            ..Options::default()
        };
        custom.save_to(&path).unwrap();
        let back = Options::load_from(&path);
        assert_eq!(back.glint_speed, 0.0, "a frozen glint is a legal choice");
        assert_eq!(back.glint_strength, 0.25);

        for bad in ["-0.5", "1.5", "\"fast\"", "null"] {
            let json = format!("{{\"glint_speed\": {bad}, \"glint_strength\": {bad}}}");
            let loaded = Options::from_json(&json);
            assert_eq!(
                f64::from(loaded.glint_speed),
                lodestone_render::glint::DEFAULT_SPEED,
                "glint_speed: {bad} must degrade to the default"
            );
            assert_eq!(
                loaded.glint_strength,
                lodestone_render::glint::DEFAULT_STRENGTH,
                "glint_strength: {bad} must degrade to the default"
            );
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- `cloudStatus` -------------------------------------------------------

    /// All three cloud states round-trip through their vanilla serialised names,
    /// and an untouched install writes no key.
    ///
    /// # Why the name and not the ordinal
    ///
    /// The wrong hypothesis this gate executes is "the ordinal is good enough": the
    /// loop below asserts each name maps to a **distinct** state, and the
    /// `"off"`/`"false"` pair pins the one that would silently invert if the file
    /// carried a number and a variant were ever inserted ahead of `Off`. `Off` is
    /// first in the enum, so its ordinal is `0` — which is also what a missing or
    /// malformed key would deserialise to under an ordinal scheme, making "clouds
    /// off" and "no setting" indistinguishable. Under the name scheme a malformed
    /// key is FANCY, vanilla's default.
    #[test]
    fn cloud_status_round_trips_through_vanillas_own_names() {
        use lodestone_render::CloudStatus;
        let path = temp_options_path("cloud-status");
        assert_eq!(Options::default().cloud_status, CloudStatus::Fancy);
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("cloud_status"),
            "the default writes no key: {text}"
        );

        for status in [CloudStatus::Off, CloudStatus::Fast, CloudStatus::Fancy] {
            let name = cloud_status_name(status);
            assert_eq!(
                cloud_status_from_name(name),
                Some(status),
                "{name} must round-trip"
            );
            let opts = Options {
                cloud_status: status,
                ..Options::default()
            };
            opts.save_to(&path).unwrap();
            assert_eq!(Options::load_from(&path).cloud_status, status);
        }
        // The three names really are three names, so the round trip above is not
        // satisfied by one string mapping to everything.
        assert_eq!(cloud_status_name(CloudStatus::Off), "off");
        assert_eq!(cloud_status_name(CloudStatus::Fast), "fast");
        assert_eq!(cloud_status_name(CloudStatus::Fancy), "fancy");

        // Vanilla's legacy boolean spellings, from `CloudStatus.byName`. `"false"`
        // is the discriminating one: under a naive "anything unknown is the
        // default" read it would become FANCY, the opposite of what it says.
        assert_eq!(cloud_status_from_name("false"), Some(CloudStatus::Off));
        assert_eq!(cloud_status_from_name("true"), Some(CloudStatus::Fancy));

        for bad in ["\"OFF\"", "\"none\"", "1", "true", "null"] {
            let json = format!("{{\"cloud_status\": {bad}}}");
            assert_eq!(
                Options::from_json(&json).cloud_status,
                CloudStatus::Fancy,
                "{bad} must degrade to vanilla's default, not to Off"
            );
        }
        // The detector works: a legal name really does come through.
        assert_eq!(
            Options::from_json("{\"cloud_status\": \"off\"}").cloud_status,
            CloudStatus::Off
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- chat display options (issue: player report on "chat options ...
    // size, etc.") -----------------------------------------------------------

    #[test]
    fn chat_options_default_to_vanillas_own_defaults() {
        let d = Options::default();
        assert_eq!(d.chat_scale, 1.0);
        assert_eq!(d.chat_width, 1.0);
        assert_eq!(d.chat_height_unfocused, 70.0 / 160.0);
        assert_eq!(d.chat_height_focused, 1.0);
        assert_eq!(d.chat_line_spacing, 0.0);
        assert_eq!(d.chat_opacity, 1.0);
        assert_eq!(d.chat_background_opacity, 0.5);
        assert!(d.chat_colors);
    }

    #[test]
    fn chat_options_untouched_write_no_keys_and_round_trip_when_changed() {
        let path = temp_options_path("chat-options-roundtrip");
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for key in [
            "chat_scale",
            "chat_width",
            "chat_height_unfocused",
            "chat_height_focused",
            "chat_line_spacing",
            "chat_opacity",
            "chat_background_opacity",
            "chat_colors",
        ] {
            assert!(!text.contains(key), "the default writes no {key} key: {text}");
        }

        let custom = Options {
            chat_scale: 0.5,
            chat_width: 0.25,
            chat_height_unfocused: 0.1,
            chat_height_focused: 0.75,
            chat_line_spacing: 0.4,
            chat_opacity: 0.3,
            chat_background_opacity: 0.9,
            chat_colors: false,
            ..Options::default()
        };
        custom.save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for key in [
            "chat_scale",
            "chat_width",
            "chat_height_unfocused",
            "chat_height_focused",
            "chat_line_spacing",
            "chat_opacity",
            "chat_background_opacity",
            "chat_colors",
        ] {
            assert!(text.contains(key), "a changed {key} must be written: {text}");
        }
        assert_eq!(Options::load_from(&path), custom);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn chat_options_degrade_to_defaults_on_bad_values() {
        let path = temp_options_path("chat-options-corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for bad in ["\"nope\"", "-1.0", "2.0", "null", "[]"] {
            std::fs::write(
                &path,
                format!(
                    "{{\"chat_scale\": {bad}, \"chat_width\": {bad}, \
                      \"chat_height_unfocused\": {bad}, \"chat_height_focused\": {bad}, \
                      \"chat_line_spacing\": {bad}, \"chat_opacity\": {bad}, \
                      \"chat_background_opacity\": {bad}, \"chat_colors\": {bad}}}"
                ),
            )
            .unwrap();
            let loaded = Options::load_from(&path);
            let d = Options::default();
            assert_eq!(loaded.chat_scale, d.chat_scale, "chat_scale: {bad}");
            assert_eq!(loaded.chat_width, d.chat_width, "chat_width: {bad}");
            assert_eq!(
                loaded.chat_height_unfocused, d.chat_height_unfocused,
                "chat_height_unfocused: {bad}"
            );
            assert_eq!(
                loaded.chat_height_focused, d.chat_height_focused,
                "chat_height_focused: {bad}"
            );
            assert_eq!(
                loaded.chat_line_spacing, d.chat_line_spacing,
                "chat_line_spacing: {bad}"
            );
            assert_eq!(loaded.chat_opacity, d.chat_opacity, "chat_opacity: {bad}");
            assert_eq!(
                loaded.chat_background_opacity, d.chat_background_opacity,
                "chat_background_opacity: {bad}"
            );
            assert!(loaded.chat_colors, "chat_colors: {bad} must degrade to ON");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- persisted keybinds --------------------------------------------------

    #[test]
    fn rebound_keys_survive_a_real_save_and_load() {
        use crate::keybinds::{Binding, InputAction};
        use winit::keyboard::KeyCode;

        let path = temp_options_path("keybinds-roundtrip");
        let mut opts = Options {
            gui_scale: 2,
            ..Options::default()
        };
        opts.keybinds
            .set(InputAction::Inventory, Binding::Key(KeyCode::KeyI));
        opts.keybinds
            .set(InputAction::Jump, Binding::Mouse(winit::event::MouseButton::Middle));
        opts.save_to(&path).unwrap();

        let loaded = Options::load_from(&path);
        assert_eq!(loaded, opts);
        assert!(loaded.keybinds.is(InputAction::Inventory, KeyCode::KeyI));
        assert!(
            !loaded.keybinds.is(InputAction::Inventory, KeyCode::KeyE),
            "the old default must not still fire"
        );
        // The unrelated setting rode along untouched.
        assert_eq!(loaded.gui_scale, 2);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn an_untouched_install_writes_no_keybinds_key_at_all() {
        // The file should show what the user changed. A default table writes
        // nothing, so a fresh `options.json` is exactly as small as it was
        // before the keybinding layer existed.
        let path = temp_options_path("keybinds-absent");
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("keybinds"),
            "defaults should not be written: {text}"
        );
        assert_eq!(Options::load_from(&path), Options::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_corrupt_keybinds_block_costs_neither_the_other_settings_nor_the_launch() {
        // The rule this shares with the server list: a broken settings file must
        // degrade, never fail. A `keybinds` value of the wrong *type* is the
        // worst case — an implementation that indexed into it would panic.
        let path = temp_options_path("keybinds-corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for bad in ["\"nope\"", "[1,2,3]", "null", "17"] {
            std::fs::write(&path, format!("{{\"gui_scale\": 4, \"keybinds\": {bad}}}")).unwrap();
            let loaded = Options::load_from(&path);
            assert_eq!(
                loaded.keybinds,
                crate::keybinds::Keybinds::new(),
                "keybinds: {bad} should degrade to the defaults"
            );
            assert_eq!(loaded.gui_scale, 4, "gui_scale must survive keybinds: {bad}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_or_corrupt_options_file_is_the_default_not_an_error() {
        assert_eq!(
            Options::load_from(Path::new("/nonexistent/options.json")),
            Options::default()
        );
        let path = temp_options_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "}{ not json").unwrap();
        assert_eq!(Options::load_from(&path), Options::default());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_options_path_lives_beside_the_server_list() {
        // Same directory, same discovery — see the module docs on why this
        // must not invent a second config location.
        assert_eq!(
            options_path().parent(),
            crate::menu::servers::servers_path().parent()
        );
        assert_eq!(options_path().file_name().unwrap(), "options.json");
    }

    // -- HiddenPlayers -------------------------------------------

    fn temp_hidden_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-config-{}-{tag}/hidden_players.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    #[test]
    fn a_fresh_hidden_players_list_hides_nobody() {
        let hp = HiddenPlayers::default();
        assert!(!hp.contains(uuid::Uuid::from_u128(1)));
    }

    #[test]
    fn toggle_flips_both_ways_and_leaves_other_ids_alone() {
        let mut hp = HiddenPlayers::default();
        let a = uuid::Uuid::from_u128(1);
        let b = uuid::Uuid::from_u128(2);
        hp.toggle(a);
        assert!(hp.contains(a));
        assert!(!hp.contains(b), "an untouched id must not report hidden");
        hp.toggle(a);
        assert!(!hp.contains(a), "toggling again must self-heal");
    }

    #[test]
    fn save_and_load_round_trip_through_the_real_file() {
        let path = temp_hidden_path("round-trip");
        let mut hp = HiddenPlayers::default();
        let a = uuid::Uuid::from_u128(1);
        let b = uuid::Uuid::from_u128(2);
        hp.toggle(a);
        hp.toggle(b);
        hp.save_to(&path).unwrap();

        let loaded = HiddenPlayers::load_from(&path);
        assert!(loaded.contains(a));
        assert!(loaded.contains(b));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_or_corrupt_hidden_players_file_is_empty_not_an_error() {
        assert_eq!(
            HiddenPlayers::load_from(Path::new("/nonexistent/hidden_players.json")),
            HiddenPlayers::default()
        );
        // A distinct tag from `Options`' own "corrupt" test, deliberately:
        // both helpers build a path from `lodestone-config-{pid}-{tag}/…`, so
        // sharing a tag put both tests' files in the *same* parent directory
        // — and one test's end-of-test `remove_dir_all` could then race the
        // other's write, since `cargo test` runs tests in parallel threads by
        // default. Caught by this test flaking under `--no-fail-fast` even
        // though the logic it exercises was correct.
        let path = temp_hidden_path("hp-corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json at all").unwrap();
        assert_eq!(HiddenPlayers::load_from(&path), HiddenPlayers::default());
        // A well-formed JSON array with a non-UUID entry degrades that one
        // entry rather than the whole file — same "a broken piece must not
        // cost the rest" rule `Keybinds::from_json_value` follows.
        std::fs::write(&path, r#"["not-a-uuid", "00000000-0000-0000-0000-000000000001"]"#).unwrap();
        let loaded = HiddenPlayers::load_from(&path);
        assert!(loaded.contains(uuid::Uuid::from_u128(1)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- issue #443: the argv -> options.json migration ----------------------

    /// A flag that was **given** wins over `options.json` for that run; a flag
    /// that was **not** given loses to it.
    ///
    /// The second half is the migration; the first half is what stops
    /// `--render-distance 4` from silently becoming a lie in a benchmark. Note
    /// both directions are asserted with the *same* persisted value, so the test
    /// cannot pass by the two happening to agree.
    #[test]
    fn an_explicit_flag_beats_options_json_and_an_absent_one_does_not() {
        let mut persisted = Options::default();
        persisted.render_distance = 16;
        persisted.sensitivity = 0.8;

        // Absent flags: the persisted values win.
        let CliOutcome::Run(mut absent) = Config::from_args([]) else {
            panic!("no args must parse")
        };
        assert!(!absent.render_distance_given && !absent.sensitivity_given);
        assert_eq!(absent.render_distance, DEFAULT_RENDER_DISTANCE, "before");
        absent.resolve_persisted(&persisted);
        assert_eq!(absent.render_distance, 16, "options.json wins when unflagged");
        assert!((absent.sensitivity - 0.8).abs() < 1e-6);

        // Given flags: argv wins, against the very same persisted values.
        let CliOutcome::Run(mut given) = Config::from_args(
            ["--render-distance", "4", "--sensitivity", "0.25"]
                .iter()
                .map(|s| (*s).to_string()),
        ) else {
            panic!("flags must parse")
        };
        assert!(given.render_distance_given && given.sensitivity_given);
        given.resolve_persisted(&persisted);
        assert_eq!(given.render_distance, 4, "argv must win for this run");
        assert!((given.sensitivity - 0.25).abs() < 1e-6);
    }

    /// The case the `*_given` flags exist for, and the reason the *value* cannot
    /// answer the question — the same trap `address_given` was added for.
    ///
    /// Passing the default explicitly is byte-identical to passing nothing, so a
    /// resolver that compared against `Config::default()` would overwrite it.
    /// Run as a control: the value-comparison hypothesis is computed here and
    /// shown to give the wrong answer.
    #[test]
    fn passing_the_default_explicitly_is_still_an_explicit_flag() {
        let mut persisted = Options::default();
        persisted.render_distance = 32;

        let CliOutcome::Run(mut cfg) = Config::from_args(
            ["--render-distance", &DEFAULT_RENDER_DISTANCE.to_string()]
                .iter()
                .map(|s| (*s).to_string()),
        ) else {
            panic!("must parse")
        };
        // The wrong hypothesis, executed: "was it given?" answered by comparing
        // the value to the default.
        let value_says_given = cfg.render_distance != Config::default().render_distance;
        assert!(
            !value_says_given,
            "the value-comparison hypothesis must answer FALSE here — that is \
             precisely its bug"
        );
        assert!(
            cfg.render_distance_given,
            "the flag must answer TRUE, because argv really did name it"
        );

        cfg.resolve_persisted(&persisted);
        assert_eq!(
            cfg.render_distance, DEFAULT_RENDER_DISTANCE,
            "an explicit --render-distance 8 must survive a persisted 32; if it \
             does not, the resolver is comparing values instead of reading the flag"
        );
    }

    /// Both new fields survive a save/load round trip, and are omitted from the
    /// file entirely while they hold their defaults — the same rule every other
    /// opt-in field in `save_to` follows.
    #[test]
    fn the_migrated_options_round_trip_and_stay_out_of_an_untouched_file() {
        let path = temp_options_path("migrated-443");
        Options::default().save_to(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("render_distance") && !text.contains("sensitivity"),
            "an untouched install must have no key for either: {text}"
        );

        let mut opts = Options::default();
        opts.render_distance = 24;
        opts.sensitivity = 0.125;
        opts.save_to(&path).unwrap();
        let back = Options::load_from(&path);
        assert_eq!(back.render_distance, 24);
        assert!((back.sensitivity - 0.125).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A hand-edited or corrupt file must not be able to produce a black screen.
    ///
    /// `render_distance` reaches `sim/build.rs`'s world radius, so 0 would
    /// generate no chunks at all. The clamp is to vanilla's own a clamped range `2..=32`
    /// rather than to "something positive", and each rejected value is checked to
    /// land on the **default** rather than on a silently clamped neighbour, which
    /// is what tells a reader the value was rejected rather than adjusted.
    #[test]
    fn an_out_of_range_render_distance_degrades_to_the_default() {
        for bad in ["0", "1", "33", "999999", "-4", "\"twelve\"", "null"] {
            let json = format!("{{\"render_distance\": {bad}}}");
            assert_eq!(
                Options::from_json(&json).render_distance,
                DEFAULT_RENDER_DISTANCE,
                "{bad} must be rejected, not clamped to a neighbour"
            );
        }
        // The detector works: in-range values really do come through, including
        // both endpoints.
        for good in [MIN_RENDER_DISTANCE, 12, MAX_RENDER_DISTANCE] {
            let json = format!("{{\"render_distance\": {good}}}");
            assert_eq!(Options::from_json(&json).render_distance, good);
        }
        // And sensitivity, a unit-interval double, follows the chat sliders' rule.
        for bad in ["-0.5", "1.5", "\"loud\""] {
            let json = format!("{{\"sensitivity\": {bad}}}");
            assert_eq!(
                Options::from_json(&json).sensitivity,
                DEFAULT_SENSITIVITY,
                "{bad} must degrade to the default"
            );
        }
        assert_eq!(Options::from_json("{\"sensitivity\": 0.0}").sensitivity, 0.0);
        assert_eq!(Options::from_json("{\"sensitivity\": 1.0}").sensitivity, 1.0);
    }

    #[test]
    fn the_hidden_players_path_lives_beside_the_server_list() {
        assert_eq!(
            hidden_players_path().parent(),
            crate::menu::servers::servers_path().parent()
        );
        assert_eq!(
            hidden_players_path().file_name().unwrap(),
            "hidden_players.json"
        );
    }
}
