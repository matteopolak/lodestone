//! Survival HUD vitals: armour, hearts, hunger, air, XP, and hotbar rendering.
//!
//! The parent HUD owns frame assembly and GPU submission; this module owns the
//! cohesive survival-vitals projection and sprite draw path. The public icon
//! projection helpers are re-exported from crate::hud for compatibility.

use super::{
    bubble_position, bubble_row, vitals_line_base, Builder, HudFrame, HOTBAR_MARGIN, BUBBLE_SIZE,
    VITALS_ROW_PITCH,
};
use super::{anim, locator};

/// Vanilla's own client-side can-hurt-player check, the predicate
/// [`HudFrame::can_hurt_player`] carries.
///
/// Its body is `localPlayerMode.isSurvival()`, and vanilla's own is-survival
/// check on its game-type enum is
/// `this == SURVIVAL || this == ADVENTURE` — so **both** creative and spectator are
/// false. Naming a mode instead (`mode == Creative`) is the tempting wrong version:
/// it agrees on three of the four values and leaves a spectator with a heart row
/// vanilla never draws.
///
/// `None` — no live connection, or a login whose game mode has not arrived — reads
/// as `true`, matching the pre-connect HUD and [`HudFrame::new`]'s own default.
#[must_use]
pub fn can_hurt_player(mode: Option<lodestone_model::GameMode>) -> bool {
    use lodestone_model::GameMode;
    match mode {
        Some(GameMode::Creative | GameMode::Spectator) => false,
        Some(GameMode::Survival | GameMode::Adventure) | None => true,
    }
}

/// Which of the three armour sprites one of the ten armour-row icons shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmourIcon {
    /// `hud/armor_full` — two whole armour points.
    Full,
    /// `hud/armor_half` — the one odd point at the frontier.
    Half,
    /// `hud/armor_empty` — the dark backing past the frontier.
    Empty,
}

impl ArmourIcon {
    /// The GUI sprite id, so the draw and any gate name the sprite once.
    #[must_use]
    pub fn sprite_id(self) -> &'static str {
        match self {
            Self::Full => "hud/armor_full",
            Self::Half => "hud/armor_half",
            Self::Empty => "hud/armor_empty",
        }
    }
}

/// Vanilla's `Hud.extractArmor` icon choice for icon `i` of ten, at `armour` points.
///
/// Transcribed from the three sibling `if`s rather than restated as arithmetic,
/// because the tempting restatement is wrong and the wrong version agrees with this
/// one on every *even* input:
///
/// ```text
/// if (i * 2 + 1 <  armor) FULL
/// if (i * 2 + 1 == armor) HALF
/// if (i * 2 + 1 >  armor) EMPTY
/// ```
///
/// The frontier is the **odd** threshold `2i + 1`, so at `armour = 15` this yields 7
/// full, 1 half, 2 empty, while the plausible `full = ceil(armour / 2)` reading — or
/// the off-by-one `i * 2 < armour` — yields 8 full and **no half at all**. An even
/// input cannot tell those apart, which is why the gate for this drives odd values.
///
/// `i` past nine and an `armour` above 20 both saturate: vanilla's loop is a fixed
/// `0..10` and the registry clamps `minecraft:armor` to `0..=30`, so a value over 20
/// fills the row rather than growing it.
#[must_use]
pub fn armour_icon(i: usize, armour: i32) -> ArmourIcon {
    let threshold = i as i32 * 2 + 1;
    if threshold < armour {
        ArmourIcon::Full
    } else if threshold == armour {
        ArmourIcon::Half
    } else {
        ArmourIcon::Empty
    }
}

/// Which heart sprite one of the ten heart containers shows **over** its backing,
/// or `None` for a container left empty.
///
/// A separate type from [`ArmourIcon`] because the empty case really is different:
/// an unfilled heart draws the container sprite and nothing on top of it, where an
/// unfilled armour icon draws its own `hud/armor_empty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartFill {
    /// `hud/heart/full` — both halves of this container.
    Full,
    /// `hud/heart/half` — the odd half at the frontier.
    Half,
}

impl HeartFill {
    /// The GUI sprite id, so the draw and any gate name the sprite once.
    #[must_use]
    pub fn sprite_id(self) -> &'static str {
        match self {
            Self::Full => "hud/heart/full",
            Self::Half => "hud/heart/half",
        }
    }
}

/// Vanilla's `Hud.extractHearts` fill choice for heart `i` of ten, at `health`
/// **hit points** (not halves) — `None` for a container with nothing drawn over it.
///
/// # The `ceil` is the whole function, and it is why this is a named symbol
///
/// Vanilla never compares the raw float. `extractPlayerHealth` computes
/// `currentHealth = Mth.ceil(player.getHealth())` **once**, hands that `int` to
/// `extractHearts`, and the fill is two integer comparisons against it:
///
/// ```text
/// int halves = containerIndex * 2;
/// if (halves < currentHealth) {
///    boolean halfHeart = halves + 1 == currentHealth;
///    extractHeart(type, …, halfHeart);
/// }
/// ```
///
/// So the composition — ceil, then an integer frontier — is the thing that has to be
/// right, and it had no name here before, which is exactly how the two halves came
/// apart: the ghost-overlay row of the same draw loop already used the
/// `halves + 1 ==` shape against an integer, while the fill row compared
/// `health - 2i` against `2.0`/`1.0` as floats. Both readings agree on every **even**
/// hit point and diverge at every odd half, in both directions:
///
/// | health | vanilla | the float reading |
/// |---|---|---|
/// | 0.5 | `ceil` 1 → one **half** heart | nothing at all — an empty bar while alive |
/// | 1.5 | `ceil` 2 → one **full** heart | a half heart |
/// | 19.5 | `ceil` 20 → **ten full** hearts | nine full and a half |
/// | 2.0, 20.0 | full hearts | identical |
///
/// The first row is the live player report ("sometimes i get to 0 hearts but im still
/// alive"): under the ceiling an empty bar is reachable only at *exactly* 0, which is
/// death. Any gate written at an integer health measures only that this function runs.
///
/// `health` is clamped at zero rather than trusted, because `hurt` overshoot can
/// report a small negative and `Mth.ceil` of that would light a heart.
#[must_use]
pub fn heart_fill(i: usize, health: f32) -> Option<HeartFill> {
    let current = health.max(0.0).ceil() as i32;
    let halves = i as i32 * 2;
    if halves >= current {
        return None;
    }
    Some(if halves + 1 == current {
        HeartFill::Half
    } else {
        HeartFill::Full
    })
}

/// Number of ten-container rows required by the reported maximum health.
///
/// The default is one row before an attribute packet arrives. The clamp is the
/// attribute's own legal `1..=1024` range, so malformed network data cannot
/// make the HUD allocate an unbounded geometry row count.
#[must_use]
pub fn heart_rows(max_health: Option<f32>) -> usize {
    let halves = max_health.unwrap_or(20.0).clamp(1.0, 1024.0).ceil() as usize;
    (halves + 19) / 20
}

/// Draw the item icons into the nine hotbar cells. Mirrors the slot geometry of
/// both hotbar-draw paths (real GUI atlas at scale 2, or the procedural 22px
/// cells) so icons land centred in the wells either way. A no-op without an item
/// atlas or `hotbar_items`, so headless / jar-less runs are unaffected.
pub(super) fn draw_hotbar_items(b: &mut Builder, frame: &HudFrame, anim: &HudAnim) {
    let Some(slots) = frame.hotbar_items else {
        return;
    };
    let cx = b.w * 0.5;
    // (first icon origin x, icon origin y, cell pitch, icon size) for the active
    // hotbar layout. Vanilla insets the 16px icon 3px into each 20px native slot.
    let (icon0_x, icon_y, pitch, size) = if b.gui.is_some() {
        // Native sprite pixels, laid straight into the already-scale-divided
        // canvas — see the "GUI Scale" note on `sprite_vitals`, which this
        // mirrors exactly (same hotbar rect, same reasoning for dropping the
        // old hardcoded ×2).
        let hw = 182.0;
        let hh = 22.0;
        let hx = cx - hw * 0.5;
        let hy = b.h - hh - HOTBAR_MARGIN;
        (hx + 3.0, hy + 3.0, 20.0, 16.0)
    } else {
        let cell = 22.0;
        let hw = 9.0 * cell;
        let hx = cx - hw * 0.5;
        let hy = b.h - HOTBAR_MARGIN - cell;
        (hx + 3.0, hy + 3.0, cell, 16.0)
    };
    for (i, slot) in slots.iter().enumerate().take(9) {
        if let Some(item) = slot {
            let x = icon0_x + i as f32 * pitch;
            let pop = anim.hotbar_pop.get(i).copied().unwrap_or(0.0);
            b.item_icon_popped(item, x, icon_y, size, pop);
        }
    }
}

/// Draw the server-authoritative item-use cooldown veil over occupied hotbar
/// icons. This shares the icon rectangles rather than the 20px wells: the
/// cooldown belongs to the item, not to an empty slot or the selection chrome.
pub(super) fn draw_hotbar_cooldowns(b: &mut Builder, frame: &HudFrame) {
    let Some(slots) = frame.hotbar_items else {
        return;
    };
    let cx = b.w * 0.5;
    let (icon0_x, icon_y, pitch, size) = if b.gui.is_some() {
        let hw = 182.0;
        let hh = 22.0;
        let hx = cx - hw * 0.5;
        let hy = b.h - hh - HOTBAR_MARGIN;
        (hx + 3.0, hy + 3.0, 20.0, 16.0)
    } else {
        let cell = 22.0;
        let hw = 9.0 * cell;
        let hx = cx - hw * 0.5;
        let hy = b.h - HOTBAR_MARGIN - cell;
        (hx + 3.0, hy + 3.0, cell, 16.0)
    };
    for (i, slot) in slots.iter().enumerate().take(9) {
        if slot.is_none() {
            continue;
        }
        let fraction = frame.hotbar_cooldowns.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        if fraction > 0.0 {
            let height = size * fraction;
            b.rect_px(
                icon0_x + i as f32 * pitch,
                icon_y + size - height,
                size,
                height,
                [0.0, 0.0, 0.0, 0.5],
            );
        }
    }
}

/// The per-frame vitals-cluster animation phases [`HudGeometry::build_inner`]
/// draws with — heart blink/jitter, the hunger wobble and the hotbar pop.
/// See `hud/anim.rs` for the vanilla citations and `docs/hud-animations.md`
/// for the port notes.
///
/// [`HudAnim::NONE`] is idle (every field at its settled value) and is what
/// [`HudGeometry::build`]/[`HudGeometry::build_with_font`]/
/// [`HudGeometry::build_with_gui`] pass — the pure, jar-less, deterministic
/// entry points every pre-existing geometry test calls — so none of those
/// three grow a wall-clock dependency, and every one of them keeps drawing
/// pixel-identically to before this type existed. Only
/// [`HudRenderer::render_with_item_models`] threads a live value in, computed
/// from [`HudRenderer`]'s own cross-frame animation state.
#[derive(Debug, Clone, Copy)]
pub(super) struct HudAnim {
    /// Vanilla's heart-row `blink`.
    pub(super) heart_blink: bool,
    /// Vanilla's own displayed-health field — the "ghost" heart
    /// overlay's total. Equal to the current health while idle.
    pub(super) display_health: i32,
    /// The wall-tick index this frame resolved to (see `hud/anim::wall_tick`)
    /// — the input the pure per-container/per-pip jitter functions need.
    pub(super) tick: i64,
    /// The health-container index lifted by the active regeneration wave.
    /// `None` is the settled position when regeneration is absent.
    pub(super) regeneration_heart_index: Option<usize>,
    /// Per-hotbar-slot pop amount, vanilla's `5.0 → 0.0` scale, `0.0` =
    /// settled/idle (see `hud/anim::HotbarPop`).
    pub(super) hotbar_pop: [f32; 9],
    /// Level-up flash strength: `1.0` at the moment of the gain, decaying to
    /// `0.0` (see `hud/anim::XpFlash`).
    ///
    /// **This is a client-local effect, not a 26.2 XP-bar parity value** — that
    /// bar has no corresponding flash in the target protocol family.
    pub(super) xp_flash: f32,
}

/// Whether the HUD's active-effect projection contains regeneration. The
/// projection is already the production fold used by the top-right effect
/// overlay, so the heart wave shares the same identity and visibility gate.
pub(super) fn regeneration_active(effects: Option<&[crate::effects::HudEffectIcon]>) -> bool {
    effects.is_some_and(|effects| {
        effects
            .iter()
            .any(|effect| effect.icon == "mob_effect/regeneration")
    })
}

impl HudAnim {
    pub(super) const NONE: Self = Self {
        heart_blink: false,
        display_health: i32::MIN, // unused while `heart_blink` is false and jitter is skipped
        tick: 0,
        regeneration_heart_index: None,
        hotbar_pop: [0.0; 9],
        xp_flash: 0.0,
    };
}

/// Draw the survival vitals cluster — hotbar frame, selection highlight, XP bar
/// (background + progress), hearts, and hunger — from the vanilla GUI atlas.
/// Returns `bars_y`, the top of the hearts/hunger row, which the action bar sits
/// above. Layout mirrors the procedural fallback closely so toggling the atlas
/// on or off does not visibly jump the HUD. A no-op-safe: [`Builder::sprite`]
/// draws nothing for a missing sprite, so a partial atlas degrades gracefully.
pub(super) fn sprite_vitals(b: &mut Builder, frame: &HudFrame, anim: &HudAnim) -> f32 {
    // Native sprite pixels, laid straight into `b.w`/`b.h` — the
    // already-scale-divided logical canvas `HudGeometry::build_inner` computes
    // via `logical_canvas`. This used to hardcode a ×2 ("vanilla GUI Scale 2")
    // on every sprite dimension here, from before there was any real scale
    // computation; now that the canvas itself is divided by the *actual*
    // effective scale, that hardcode would double-apply it — sprites here would
    // render at 2× the size of the hotbar cells and text around them, which are
    // laid out in plain logical pixels with no such multiplier. Dropping it is
    // what keeps this cluster at the same visual size as everything else at any
    // scale, not just the one this used to assume. At an integer scale the atlas
    // sampler's Nearest magnification still replicates texels exactly, so
    // on-screen pixels equal jar pixels — which the GPU gate checks.
    let white = [1.0, 1.0, 1.0, 1.0];
    let cx = b.w * 0.5;

    // Hotbar (182x22 native), centred at the bottom, with the 24x23 selection
    // sprite over the chosen slot.
    let hw = 182.0;
    let hh = 22.0;
    let hx = cx - hw * 0.5;
    let hy = b.h - hh - HOTBAR_MARGIN;
    if let Some(sel) = frame.hotbar {
        b.sprite("hud/hotbar", hx, hy, hw, hh, white);
        // Vanilla draws the selection at native offset (slot*20 - 1, -1) from the
        // hotbar origin; the sprite is 24x23 so it overhangs the 20px slot pitch.
        //
        // **The vertical asymmetry is vanilla's, and a report that the bottom edge
        // is "cut off" is a faithful absence rather than a defect.** Both blits
        // from `Hud.extractItemHotbar`, verbatim: the bar at
        // `(centre - 91, guiHeight - 22, 182, 22)` and the selection at
        // `(centre - 91 - 1 + slot * 20, guiHeight - 22 - 1, 24, 23)`. PNG headers
        // read out of the 26.2 jar agree — `hud/hotbar` is 182x22 and
        // `hud/hotbar_selection` is 24x23. So the bar occupies rows `H-22..H-1` and
        // the selection `H-23..H-1`: **one pixel of overhang at the top and none at
        // the bottom, because 23 = 22 + 1 and the offset is -1 on the top only.**
        // There is no bottom overhang to lose, at any GUI scale, and this holds in
        // exact integer arithmetic — so neither rasterisation rounding nor a clip
        // rect can be blamed for it. The relationship below (`sh = 23.0` at
        // `hy - 1.0` over an `hh = 22.0` bar at `hy`) is the same one.
        //
        // The bottom margin that used to float this 6 px up is now
        // [`HOTBAR_MARGIN`] — zero, matching the blits quoted above. That is a
        // separate fix from this asymmetry and the two should not be conflated:
        // the asymmetry is vanilla's and stays.
        let sel = sel.min(8) as f32;
        let sw = 24.0;
        let sh = 23.0;
        let sx = hx + sel * 20.0 - 1.0;
        let sy = hy - 1.0;
        b.sprite("hud/hotbar_selection", sx, sy, sw, sh, white);
    }

    // XP bar (182x5), just above the hotbar: full background, then the progress
    // sprite cropped left-to-right to its filled fraction.
    //
    // The gap above the hotbar is vanilla's own arithmetic, not a guess:
    // `ContextualBar.MARGIN_BOTTOM` (24) is the hotbar's 22px height plus a 2px
    // gap, and `ContextualBar.top` is `guiScaledHeight - MARGIN_BOTTOM - HEIGHT`
    // — i.e. the bar sits *2px* above the
    // hotbar sprite, not 4. `hy` is already this cluster's hotbar-top in the
    // same logical-pixel space vanilla's own GUI-height value is in, so subtracting from
    // it (rather than restating an absolute `b.h`-based constant) is what keeps
    // this correct if the cluster's own bottom margin ever changes — the same
    // "derive from the expression the draw uses" rule the XP number below now
    // follows too.
    let bar_w = 182.0;
    let bar_h = 5.0;
    // With [`HOTBAR_MARGIN`] at vanilla's zero this resolves to
    // `guiHeight - 22 - 5 - 2 == guiHeight - 29`, which is exactly
    // `ContextualBar.top`'s `guiScaledHeight - 24 - 5`. It was 6 px off before,
    // for the single reason that `hy` was.
    //
    // The bar no longer feeds anything else's placement. It used to raise a
    // `cluster_top` that the hearts row stacked off, which made the hearts move
    // depending on whether the player had XP — vanilla's own vitals-cluster baseline is a
    // constant (see [`VITALS_LINE_BASE_FROM_BOTTOM`]) and takes no such branch.
    //
    // This is `ContextualBar::top(window)` — `guiScaledHeight - 24 - 5` —
    // shared by every bar that can occupy this slot (`ContextualBar` is a
    // single mutually-exclusive slot in vanilla: XP, locator, or the
    // jumpable-vehicle bar, never more than one at once). Unconditional now
    // (it used to live behind `frame.xp.map`) because the locator bar needs
    // the same position on a session with no XP to show at all.
    let bar_top = hy - bar_h - 2.0;
    // Contextual-info priority is reduced to the two bars this build models:
    // the locator bar wins whenever there is at least one waypoint to show,
    // and the XP bar draws only when the locator bar has nothing. The target
    // client also gives a recently gained XP flash priority for a few seconds
    // and supports a jumpable-vehicle bar; neither is modelled here. A player
    // with waypoints who just levelled up therefore sees the locator bar
    // immediately.
    if !frame.locator.is_empty() {
        b.sprite("hud/locator_bar_background", hx, bar_top, bar_w, bar_h, white);
        // `Mth.ceil((graphics.guiWidth() - 9) / 2.0F)` — vanilla's own locator-bar rendering.
        // Not `guiWidth/2 - 9/2`: the `ceil` sits around the whole
        // subtraction, and the two only agree when `guiWidth` is even.
        let dot = locator::dot_size() as f32;
        let screen_middle = ((b.w - dot) / 2.0).ceil();
        let dy = bar_top - 2.0;
        for dot_info in frame.locator {
            let dx = screen_middle + dot_info.offset as f32;
            b.sprite(locator::DEFAULT_DOT_SPRITE, dx, dy, dot, dot, dot_info.color);
        }
    }
    // `nextContextualInfoState` reaches `ContextualInfo.EXPERIENCE` only when
    // `gameMode.hasExperience()`, so creative and spectator draw neither the bar nor
    // the level number — see [`HudFrame::can_hurt_player`].
    else if let (true, Some((level, progress))) = (frame.can_hurt_player, frame.xp) {
        let by = bar_top;
        b.sprite("hud/experience_bar_background", hx, by, bar_w, bar_h, white);
        let p = progress.clamp(0.0, 1.0);
        if p > 0.0 {
            // Crop by shrinking both the destination width and the sampled UV
            // span, so the bar reveals its pattern instead of squashing it.
            //
            // The level-up flash rides the *fill*'s vertex tint. The
            // sprite is already near-white, so the visible part of the effect is
            // the level number below; brightening the fill too is what stops the
            // number looking like it flashed on its own. `white` unchanged at
            // `xp_flash == 0.0`, so an idle frame is byte-identical to before.
            let fill = anim::flash_toward_white(white, anim.xp_flash);
            for mut q in b.gui_geometry("hud/experience_bar_progress", hx, by, bar_w, bar_h) {
                let span = q.uv_max[0] - q.uv_min[0];
                q.dst[2] *= p;
                q.uv_max[0] = q.uv_min[0] + span * p;
                b.push_sprite_quad(q, fill);
            }
        }
        // The level number (vanilla green), centred above the bar.
        //
        // Player report: "the xp bar number is too big and too high." Both
        // were real, and both were this block:
        //
        // * **Too big** — `scale` was `2.0`. This function already draws in
        //   the scale-divided logical canvas (see the doc comment atop
        //   `sprite_vitals`), the same space the 182px-wide bar itself is laid
        //   out in, so a ×2 on the text alone made it twice vanilla's size
        //   relative to everything around it. Vanilla's own draw
        //   (`ContextualBar.extractExperienceLevel`, below) never scales the
        //   font at all.
        // * **Too high** — `by - line_h` used a *font-metrics* gap
        //   (`(GLYPH_H + 2) * scale`, i.e. 20px at the old scale of 2), not
        //   vanilla's real one. `ContextualBar.extractExperienceLevel`
        //   places the text at
        //   `y = guiHeight - 24 - 9 - 2`, and the bar itself sits at
        //   `guiHeight - 24 - 5`: the text's top is exactly `6` logical px
        //   above the bar's top, full stop — not a value derived from glyph
        //   height. Written as `by - 6.0` here for the same reason the bar
        //   gap above is written from `hy` rather than restated: it is the one
        //   expression that cannot drift out of sync with where the bar
        //   itself actually landed.
        //
        // Vanilla also does not use its usual single-shadow text path here: it
        // calls `graphics.text(font, str, x, y, colour, false)` — shadow
        // `false` — **five** times: four unshadowed black copies offset ±1px
        // on each axis (the outline), then one unshadowed copy in
        // `0x80FF20`. `Builder::text` would add
        // its own automatic drop shadow on top of a hand-rolled outline, so
        // this uses [`Builder::text_plain`] for all five passes, matching
        // vanilla's `shadow = false` exactly.
        if level > 0 {
            let s = level.to_string();
            let tw = b.text_width(&s, 1.0);
            let tx = cx - tw * 0.5;
            let ty = by - 6.0;
            let black = [0.0, 0.0, 0.0, 1.0];
            // `0x80FF20` is the base green in raw ARGB bytes; it is brightened
            // toward white for the level-up flash's duration. The mix is kept
            // in this raw-byte space; see `anim::flash_toward_white`.
            let green = anim::flash_toward_white([128.0 / 255.0, 1.0, 32.0 / 255.0, 1.0], anim.xp_flash);
            b.text_plain(&s, tx + 1.0, ty, 1.0, black);
            b.text_plain(&s, tx - 1.0, ty, 1.0, black);
            b.text_plain(&s, tx, ty + 1.0, 1.0, black);
            b.text_plain(&s, tx, ty - 1.0, 1.0, black);
            b.text_plain(&s, tx, ty, 1.0, green);
        }
    }

    // Hearts (health) left, hunger right, one row above the cluster. Each icon
    // is 9x9 native, stepped 8px (vanilla spacing); a container/empty backing is
    // drawn first, then a full or half overlay per two points.
    // All three rows sit behind vanilla's own single can-hurt-player gate on
    // its player-health extraction — hearts, hunger and the bubble row are drawn by that one
    // call, so creative and spectator show none of them. See
    // [`HudFrame::can_hurt_player`].
    let icon = 9.0;
    let step = 8.0;
    // The vitals-cluster baseline, from vanilla's own expression rather than by stacking upward
    // from the hotbar. See [`vitals_line_base`]: this used to be
    // `cluster_top - icon - 4.0`, and `cluster_top` moved with the XP bar, so the
    // hearts landed on two different rows depending on the player's game mode and
    // on neither of vanilla's.
    let row_y = vitals_line_base(b.h);
    let health_rows = heart_rows(frame.max_health);

    // The armour row, one 10px line **above** the hearts and sharing their left
    // anchor — vanilla's own armour-row x-coordinate stepping identically to the
    // hearts' own left anchor, and its y-coordinate subtracting a further row's
    // worth of pitch (times however many health rows there are) plus ten from the
    // baseline.
    //
    // `numHealthRows` is `ceil(maxHealth / 2 / 10)` while absorption is absent.
    // [`heart_rows`] obtains that number from the local attribute snapshot, so
    // Health Boost moves armour above the complete heart stack. [`VITALS_ROW_PITCH`]
    // is shared with the air row so the two cannot drift apart.
    //
    // Drawn *before* the hearts because vanilla's own armour extraction call precedes
    // its hearts extraction, and left as a separate `if` rather than folded into the
    // hearts' block because vanilla gates it on armour being positive alone — a player with
    // armour and no health packet yet still has an armour row.
    if frame.can_hurt_player
        && let Some(armour) = frame.armour
        && armour > 0
    {
        let armour_row_y = row_y - health_rows as f32 * VITALS_ROW_PITCH;
        for i in 0..10 {
            let x = hx + i as f32 * step;
            b.sprite(
                armour_icon(i, armour).sprite_id(),
                x,
                armour_row_y,
                icon,
                icon,
                white,
            );
        }
    }

    if frame.can_hurt_player && let Some(hp) = frame.health {
        let hp = hp.max(0.0);
        let current = hp.ceil() as i32;
        // The container background flashes to the "_blinking" sprite variant
        // for the same alternating windows the ghost overlay below uses —
        // vanilla draws it for *every* container regardless of that
        // container's own fill state.
        let container = if anim.heart_blink {
            "hud/heart/container_blinking"
        } else {
            "hud/heart/container"
        };
        // Critical-health y-jitter: `currentHealth +
        // absorption <= 4`. Absorption is not modelled in `HudFrame` yet, so
        // this gates on health alone — a documented narrowing, not a silent
        // one.
        let critical = current <= 4;
        for row in 0..health_rows {
            let row_y = row_y - row as f32 * VITALS_ROW_PITCH;
            for column in 0..10 {
                let i = row * 10 + column;
                let x = hx + column as f32 * step;
                let y = (if critical {
                    row_y + anim::heart_jitter(anim.tick, i)
                } else {
                    row_y
                }) - if anim.regeneration_heart_index == Some(i) {
                    2.0
                } else {
                    0.0
                };
                b.sprite(container, x, y, icon, icon, white);
                // The "ghost" of health about to be lost uses the same global
                // container index as the fill, so it crosses row boundaries.
                let halves = i * 2;
                if anim.heart_blink && (halves as i32) < anim.display_health {
                    let half = (halves as i32 + 1) == anim.display_health;
                    let ghost = if half {
                        "hud/heart/half_blinking"
                    } else {
                        "hud/heart/full_blinking"
                    };
                    b.sprite(ghost, x, y, icon, icon, white);
                }
                if let Some(fill) = heart_fill(i, hp) {
                    b.sprite(fill.sprite_id(), x, y, icon, icon, white);
                }
            }
        }
    }
    if frame.can_hurt_player && let Some(food) = frame.food {
        let food_f = food.max(0) as f32;
        // Hunger-empty wobble: `frame.saturation` is
        // `None` off a build that has not wired it through yet (see
        // `HudFrame::saturation`'s doc) — treated as "not empty", so the row
        // stays flush rather than guessing.
        let saturation = frame.saturation.unwrap_or(1.0);
        for i in 0..10 {
            // Hunger fills right-to-left in vanilla.
            let x = hx + hw - icon - i as f32 * step;
            let y = row_y + anim::hunger_wobble(anim.tick, food, saturation, i);
            b.sprite("hud/food_empty", x, y, icon, icon, white);
            let units = food_f - i as f32 * 2.0;
            if units >= 2.0 {
                b.sprite("hud/food_full", x, y, icon, icon, white);
            } else if units >= 1.0 {
                b.sprite("hud/food_half", x, y, icon, icon, white);
            }
        }
    }

    // Air bubbles, one row above hearts/hunger, on the same right edge
    // (`hx + hw`) the hunger row uses — vanilla's own shared right anchor.
    //
    // # The baseline minus ten is the whole answer, and reading only
    // # vanilla's player-health extraction alone says otherwise
    //
    // Three terms, and the third cancels the second. Hand-expanded from the
    // 26.2 source for a player with no mounted vehicle, `H == guiHeight`:
    //
    // | step | in | out |
    // |---|---|---|
    // | vanilla's own player-health extraction sets the air row's y-coordinate to the baseline minus ten | `H-39` | `H-49` |
    // | when there is no vehicle, its food extraction runs and the air row's y-coordinate steps back a further ten | `H-49` | `H-59` |
    // | vanilla's own air-bubble extraction then resolves the final y-line from a zero vehicle-heart count and that value | `H-59` | **`H-49`** |
    //
    // The last row is the one that is easy to miss: that final resolution computes
    // a row offset as one less than however many vehicle-heart rows are visible, and
    // zero vehicle hearts round up to zero such rows, so the offset is
    // **-1** and subtracting ten times a negative offset *adds* the ten straight back. The
    // second subtraction is real but unobservable without a vehicle; its purpose
    // is the mounted case, where no food row draws (a real vehicle heart count) and a
    // 20-heart mount gives an offset of one, moving the bubbles up to `H-59` to
    // clear the vehicle-health row that replaced the food.
    //
    // So the bubbles share a line with the armour row — armour on the left,
    // bubbles on the right — which is what vanilla looks
    // like. A "correction" to the baseline minus twenty reads as obviously right from
    // vanilla's player-health extraction alone and is wrong; this table is here so the next
    // person re-derives it rather than re-deciding it.
    //
    // Mounted vehicles are not modelled (`HudFrame` carries no vehicle), so the
    // `rowOffset >= 1` branch has nothing to drive it — a documented narrowing.
    if frame.can_hurt_player && let Some((air, max_air, eye_in_water)) = frame.air {
        let air_row_y = row_y - health_rows as f32 * VITALS_ROW_PITCH;
        // `wobble` is vanilla's `tickCount % 2 == 0` (a 0/1px jitter vanilla
        // applies to a fully-empty row's last bubble) — no per-frame tick
        // parity is piped into `HudFrame` yet, so this always reads `false`.
        // Purely cosmetic, deliberately left unwired rather than approximated.
        let wobble = false;
        for (i, slot) in bubble_row(air, max_air, eye_in_water, wobble)
            .into_iter()
            .enumerate()
        {
            let Some(sprite_id) = slot.sprite_id() else {
                continue;
            };
            let (x, y) = bubble_position(i, hx + hw, air_row_y);
            b.sprite(sprite_id, x, y, BUBBLE_SIZE, BUBBLE_SIZE, white);
        }
    }

    row_y
}
