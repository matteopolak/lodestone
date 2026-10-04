//! The heads-up display: a crosshair and an F3-style debug overlay.
//!
//! The overlay is the shell's instrument panel — position, facing, FPS, frame
//! time, chunk/section/quad counts, VRAM and process memory — so it is the first
//! thing that reveals whether the pipeline is actually fast and the first thing
//! that shows a regression. The same [`DebugStats`] is printed by the explicit
//! headless evidence runner; ordinary windowed play stays quiet.
//!
//! Rendering has two streams. Text, the crosshair, and overlay chrome are
//! emitted as solid-colour quads in one dynamic vertex buffer (positions in NDC,
//! RGBA per vertex). The survival vitals — hotbar, XP bar, hearts, hunger — draw
//! from the vanilla GUI sprite atlas once [`HudRenderer::attach_gui`] supplies
//! one, via a second textured vertex stream; without an atlas (jar-less or
//! headless runs) they fall back to procedural quads on the colour stream. Both
//! streams are flat `Vec<f32>`s so they need no `bytemuck::Pod` derive (which the
//! workspace's `deny(unsafe_code)` would reject) and draw in a `Load` pass over
//! the terrain with no depth.

mod anim;
pub mod debug_overlay;
mod font;
pub(crate) mod item_icon;
pub mod locator;
mod tab_panel;
mod toasts;
mod vitals;
mod spectator;
pub mod vanilla_font;

mod builder;
mod chat_layout;
mod chat_suggestions;
mod chat_tooltip;
mod chat_wrap;
mod debug_draw;
mod debug_stats;
mod frame;
mod geometry;
mod renderer;
mod subtitles;
mod toast_views;
#[cfg(test)]
mod tests;

pub use chat_layout::*;
pub use chat_tooltip::*;
pub use chat_wrap::*;
pub use debug_stats::*;
pub use frame::*;
pub use geometry::*;
pub use renderer::*;
pub use toast_views::*;
use builder::*;
use chat_suggestions::*;
use debug_draw::*;
use subtitles::*;
#[cfg(test)]
use tests::coverage;

pub use font::glyph_rows;
pub use tab_panel::TabPanel;
use tab_panel::{
    TAB_HEAD_W, TAB_INK, TAB_INK_SPECTATOR, TAB_PING_H, TAB_PING_INSET, TAB_PING_W, TAB_PLATE,
    TAB_ROW_FILL,
};
pub use vanilla_font::VanillaFont;
pub use vitals::{
    armour_icon, can_hurt_player, heart_fill, heart_rows, ArmourIcon, HeartFill,
};
use vitals::{
    draw_hotbar_cooldowns, draw_hotbar_items, regeneration_active, sprite_vitals, HudAnim,
};
use toasts::{draw_advancement_toast, draw_friends_toast, draw_recipe_toast};
/// The hotbar's per-slot draw record. The container screen builds the same
/// record for every menu slot, so the type itself lives in [`item_icon`]; this
/// is the name the hotbar has always used for it.
pub use item_icon::ItemIcon as HotbarSlot;

use std::sync::Arc;
use std::time::Duration;
use crate::platform::Instant;

use lodestone_render::{
    BUBBLE_SIZE, BlockModels, GpuAtlas, GuiAtlas, GuiSpriteQuad, ModelVertex, bubble_position,
    bubble_row,
};

use lodestone_assets::ItemAtlas;
use lodestone_model::text::{TextColor, TextSpan, TextStyle};

use item_icon::{ColourStream, IconAssets, IconRenderer, IconSink, SpecialIconDraw};

use crate::effects;
use crate::overlay::{BossBarView, Sidebar};

/// Padding between a HUD panel's edge and its content.
pub(crate) const HUD_MARGIN: f32 = 6.0;

/// The gap between the hotbar's bottom edge and the bottom of the screen.
///
/// **Zero, because vanilla's hotbar is flush.** `Hud.extractItemHotbar` blits it
/// at `(guiWidth/2 - 91, guiHeight - 22, 182, 22)` and the selection at
/// `(…, guiHeight - 23, 24, 23)`; there is no bottom margin anywhere in that
/// method. This was [`HUD_MARGIN`] (6), which floated the whole cluster 6 px up.
///
/// **Not [`HUD_MARGIN`], and not a shared constant with it.** They answer
/// different questions — that one is the chat/debug text inset, this one is a
/// vanilla blit coordinate — and folding them together is what made correcting
/// one look like it would move the other. A named zero rather than a deleted
/// term so the next reader can see the decision was made rather than forgotten.
///
/// `pub` because the two hotbar item-icon pixel gates derive their read-back
/// rects from it. Each of them restated a `6.0` of its own and both went red the
/// moment this changed — which is the argument for one name rather than three.
pub const HOTBAR_MARGIN: f32 = 0.0;

/// Vanilla's own vitals-cluster baseline y-coordinate, as a distance up from the bottom of
/// the screen: it is the canvas height minus 39.
///
/// This is the hearts row's top *and* the hunger row's top (vanilla's own
/// food-row extraction is
/// passed that baseline unchanged), and every other row in the cluster is derived
/// from it — see [`vitals_line_base`].
///
/// **It is unconditional in vanilla.** It does not move for the XP bar, the game
/// mode, or anything else. This used to be computed by stacking upward from a
/// `cluster_top` that *did* move with the XP bar, which put the hearts 3 px too
/// high with an XP bar and 4 px too low without one — two different wrong
/// answers, neither of them 39.
const VITALS_LINE_BASE_FROM_BOTTOM: f32 = 39.0;

/// The vitals cluster's baseline row (hearts and hunger) for a canvas `canvas_h`
/// logical pixels tall — vanilla's own vitals-cluster baseline y-coordinate.
///
/// Public because the air-row pixel gate derives its screen rect from it. That
/// gate's own history is why: it hardcoded `lh - 39.0` once, which silently
/// assumed a stack shape the fixture did not have, and reported 0 px for a row
/// that was drawing perfectly. One expression, both callers.
#[must_use]
pub fn vitals_line_base(canvas_h: f32) -> f32 {
    canvas_h - VITALS_LINE_BASE_FROM_BOTTOM
}

/// The vertical pitch between two rows of the vitals cluster — the `10` vanilla's
/// own armour-row and air-row y-coordinates each subtract from the baseline
/// (the armour row also subtracting further rows' worth of pitch above that).
///
/// Written as the 9 px icon plus a 1 px gap, which is what it is, so the icon
/// size and the pitch cannot drift apart.
const VITALS_ROW_PITCH: f32 = 10.0;

/// Padding above and below the chat input's text inside its background strip,
/// in unscaled logical pixels.
///
/// Vanilla's input band is `fill(2, height - 14, width - 2, height - 2, …)`
/// around an `EditBox` whose text sits at `height - 12`
/// (`:56`) — 2px above the text and 2px below it. It is scaled by the chat pose
/// scale at every use, alongside the glyph height, so the strip stays wrapped
/// around the text at any chat scale.
const INPUT_STRIP_PAD: f32 = 2.0;

/// The chat column's left text inset, in chat-pose-scaled pixels: how far the
/// first glyph of a chat line sits from the left edge of the screen.
///
/// **Four, not [`HUD_MARGIN`]'s six.** Both chat surfaces used the shared HUD
/// margin, which is a different quantity that happens to live nearby, and the
/// owner reported the result directly: *"all of the text in the chat window
/// (and my own chat bar) are offset to the right … theres a bigger gap than
/// there should be on the left"*. Both were wrong together because both read
/// one wrong constant, not because two mistakes coincided.
///
/// Two things follow from it being an inset rather than a margin, and both were
/// wrong too:
///
/// - **The plate is anchored at `0`, not at the inset**, and it is wider than
///   the text column by this inset on the left and twice it on the right — see
///   [`CHAT_PLATE_PAD_PX`]. The plate used to be exactly the text column's own
///   width starting at `0`, so a full-width wrapped line ran off the right end
///   of its own background: the second half of the same report.
/// - **It scales with the chat pose scale.** The scrollback's inset is applied
///   inside chat's own scaled space, so at half chat scale it is two screen
///   pixels, not four. The input bar's is a flat four in vanilla, because that
///   screen is not chat-scaled at all; this shell *does* draw the input at the
///   chat pose scale (see [`chat_input_top`]), so scaling its inset too is what
///   keeps the input's first glyph in the same column as the scrollback's. At
///   the default chat scale of 1.0 the two readings coincide.
///
/// See `docs/chat.md` for the derivation and the source it came from.
pub(crate) const CHAT_TEXT_INSET: f32 = 4.0;

/// Extra width the chat scrollback's plate carries beyond the text column, in
/// chat-pose-scaled pixels: [`CHAT_TEXT_INSET`] of left padding plus twice that
/// on the right.
///
/// The plate starts at screen `x = 0` and the text starts at
/// [`CHAT_TEXT_INSET`], so the padding is asymmetric by construction — this is
/// the total, and the plate's own width is the text column's width plus this.
/// Without it a wrapped line at the configured box width overhangs its own
/// background. See `docs/chat.md`.
pub(crate) const CHAT_PLATE_PAD_PX: f32 = 12.0;

/// Where the scrollback's scroll indicator sits, measured from the right edge
/// of the text column, in chat-pose-scaled pixels — clear of the widest
/// possible wrapped line rather than on top of it. See `docs/chat.md`.
const CHAT_SCROLLBAR_GAP: f32 = 8.0;

/// `DebugScreenOverlay.MARGIN_LEFT`/`MARGIN_RIGHT`/`MARGIN_TOP`, all `2`.
///
/// `extractLines` spends them as `left = alignLeft ? 2 : guiWidth() - 2 - width`
/// and `top = 2 + height * i`, so the same `2` is the left inset, the right
/// inset and the top inset.
///
/// **Not [`HUD_MARGIN`]**: the F3 overlay is vanilla's own screen with vanilla's
/// own metrics, and it draws in the already-`gui_scale`-divided logical canvas,
/// so it needs no HUD-side scaling of any kind.
pub const DEBUG_MARGIN: f32 = 2.0;

/// The F3 overlay's line pitch — vanilla's literal `int height = 9` in
/// `DebugScreenOverlay.extractLines`.
///
/// It is both the pitch (`top = 2 + height * i`) and the plate's own height
/// (`fill(…, top - 1, …, top + height - 1, …)` spans exactly `height` rows), so
/// consecutive plates tile with no seam and no overlap.
///
/// The overlay used to draw at an ad-hoc HUD-wide pitch of double this, which is
/// what "the text is way too big" was: exactly the mistake the XP level number's
/// own comment records, one screen over. `docs/hud-text-scale.md` has the fuller
/// history; the ad-hoc pitch itself is gone now that chat (its last consumer)
/// draws at vanilla's own metrics too.
pub const DEBUG_LINE_H: f32 = 9.0;

/// The plate behind each F3 overlay line — vanilla's
/// `fill(left - 1, top - 1, left + width + 1, top + height - 1, -1873784752)`,
/// i.e. `0x90505050` (`DebugScreenOverlay.extractLines`) — mid grey at 56%
/// alpha. Without it the overlay is unreadable over bright terrain, which is
/// what the shell shipped before it had one.
pub(crate) const DEBUG_LINE_BG: [f32; 4] = [
    0x50 as f32 / 255.0,
    0x50 as f32 / 255.0,
    0x50 as f32 / 255.0,
    0x90 as f32 / 255.0,
];

/// The F3 overlay's ink — vanilla's `-2039584`, i.e. `0xFFE0E0E0`, drawn
/// **without** a shadow (`extractLines` passes `shadow = false`).
pub(crate) const DEBUG_LINE_INK: [f32; 4] =
    [0xE0 as f32 / 255.0, 0xE0 as f32 / 255.0, 0xE0 as f32 / 255.0, 1.0];

/// The Tab player-list overlay's line pitch — vanilla's literal `9`
/// (`PlayerTabOverlay.extractRenderState`, which advances `yo` by `9` per row and
/// fills each slot `8` tall inside it).
///
/// **Vanilla's own metrics, not an ad-hoc HUD-wide pitch.** The tab overlay is a
/// vanilla *screen-space* draw in the already-`gui_scale`-divided logical canvas,
/// exactly like the F3 overlay above, so it uses vanilla's own metrics at scale
/// `1.0`. Drawing it at double that pitch is what "the text is way too big"
/// means, one screen over.
pub(crate) const TAB_LINE_H: f32 = 9.0;

/// The tab overlay's text scale — vanilla metrics, so `1.0`. See [`TAB_LINE_H`].
pub(crate) const TAB_TEXT_SCALE: f32 = 1.0;

/// The scoreboard sidebar's line pitch — `Hud.displayScoreboardSidebar`'s
/// literal `9` (`int height = entriesCount * 9;`, and each row's `y` advances
/// by that same `9` walking backwards from `bottom`).
///
/// **Vanilla's own metrics, not an ad-hoc HUD-wide pitch.** Exactly the same
/// exemption as [`TAB_LINE_H`]: this draws in the `gui_scale`-divided logical
/// canvas at vanilla's own font metrics — drawing it at double that pitch is
/// what made the sidebar panel twice vanilla's size.
pub(crate) const SIDEBAR_LINE_H: f32 = 9.0;

/// The sidebar's text scale — vanilla metrics, so `1.0`. See [`SIDEBAR_LINE_H`].
pub(crate) const SIDEBAR_TEXT_SCALE: f32 = 1.0;

/// The sidebar's edge inset — vanilla's own display-scoreboard-sidebar's literal `3` in
/// `int left = guiWidth() - width - 3;` and `int right = guiWidth() - 3 + 2;`.
const SIDEBAR_EDGE_MARGIN: f32 = 3.0;

/// Vanilla's own get-background-color accessor at `0.3F`'s body-plate alpha, with
/// `backgroundForChatOnly` at its default (so the passed `0.3F` default is what
/// renders, not the user's chat background opacity option).
const SIDEBAR_BODY_BG_ALPHA: f32 = 0.3;

/// Vanilla's own get-background-color accessor at `0.4F`'s header-plate alpha — see
/// [`SIDEBAR_BODY_BG_ALPHA`].
const SIDEBAR_HEADER_BG_ALPHA: f32 = 0.4;

/// `ChatFormatting.RED` (`0xFF5555`) — `StyledFormat.SIDEBAR_DEFAULT`'s colour,
/// the score column's default when a server sends no per-entry
/// [`lodestone_game::scoreboard::NumberFormat::Styled`] override.
const SIDEBAR_SCORE_DEFAULT: [f32; 3] = [1.0, 0x55 as f32 / 255.0, 0x55 as f32 / 255.0];

/// `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` — every boss bar is this fixed
/// native size, never a fraction of the canvas width.
const BOSS_BAR_WIDTH: f32 = 182.0;
const BOSS_BAR_HEIGHT: f32 = 5.0;

/// `BossHealthOverlay.extractRenderState`'s `int yOffset = 12;` — the first
/// bar's top.
const BOSS_BAR_TOP: f32 = 12.0;

/// `BossHealthOverlay.extractRenderState`'s per-bar stride: `yOffset += 10 +
/// 9;` — 10 for the bar's own row pitch, 9 for the title above it.
const BOSS_BAR_STEP: f32 = 19.0;

/// The boss bar title's text scale — vanilla's `graphics.text(font, msg, x,
/// y, -1)` takes no pose scale at all, exactly like the action bar and the
/// held-item name (see `docs/hud-text-scale.md`).
const BOSS_BAR_TEXT_SCALE: f32 = 1.0;

/// Bytes per HUD vertex: 2 position floats + 4 colour floats.
const FLOATS_PER_VERTEX: usize = 6;

/// Floats per textured-sprite vertex: position (x, y in NDC), atlas UV (u, v),
/// and an RGBA tint. The GUI sprite stream is separate from the colour stream
/// so the existing colour pipeline is untouched.
pub(crate) const SPRITE_FLOATS_PER_VERTEX: usize = 8;

const HUD_WGSL: &str = include_str!("../shaders/hud.wgsl");

const HUD_SPRITE_WGSL: &str = include_str!("../shaders/hud_sprite.wgsl");

/// The 2-D GUI enchantment glint. Shares `hud_sprite.wgsl`'s vertex
/// layout — see `item_icon::GuiGlint` for why it cannot share
/// `lodestone_render`'s own glint pipeline.
const HUD_GLINT_WGSL: &str = include_str!("../shaders/hud_glint.wgsl");
