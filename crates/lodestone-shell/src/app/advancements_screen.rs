//! The Advancements screen's wiring: clicks, panning, and the
//! per-frame geometry call.
//!
//! The screen itself is [`crate::menu::advancements`]; this module is the half
//! that stops it being an island. It mirrors
//! [`super::creative_screen`]'s shape exactly, and for the same reason: both
//! screens build a [`ContainerGeometry`](crate::container::ContainerGeometry) and
//! hand it to `ContainerRenderer::render_geometry_scaled`, so neither can be a
//! `menu::render` frame and neither can be a `&self` method inside `redraw`'s
//! borrow split.

use super::*;

use crate::menu::advancements::{
    AdvancementProgress, AdvancementToastQueue, AdvancementsHit, AdvancementsState,
    AdvancementsView, advancements_geometry, advancements_hit_test, advancements_layout,
    advancement_tabs,
};

/// How stale a cached [`AdvancementProgress`] may get while the screen is shut.
///
/// Reading `SessionAdvancements` clones a 126-entry store of owned criterion
/// names, which is not something to do sixty times a second for a toast that
/// nobody can perceive arriving a quarter-second late. While the screen *is*
/// open the read happens every frame, so panning and hovering never show stale
/// frames.
const PROGRESS_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// The live advancement progress plus the completion-toast queue.
///
/// One field on [`WindowApp`] rather than three, because `app.rs` is a contended
/// file and the three parts have exactly one lifetime between them.
#[derive(Debug, Default)]
pub(crate) struct AdvancementsFeed {
    progress: AdvancementProgress,
    toasts: AdvancementToastQueue,
    polled_at: Option<crate::platform::Instant>,
}

impl WindowApp {
    /// This frame's advancement progress, refreshed from `SessionAdvancements`
    /// when the screen is open or [`PROGRESS_POLL`] has elapsed.
    ///
    /// Returns an owned snapshot rather than a borrow: every caller here also
    /// needs `self.nav.advancements_mut()` in the same statement, and `redraw`
    /// carries the value across its field-borrow split.
    /// [`AdvancementProgress`] is a map of 126 `Copy` records, so the clone is
    /// cheaper than the read it saves.
    pub(super) fn advancement_progress(&mut self) -> AdvancementProgress {
        let now = crate::platform::Instant::now();
        let stale = self
            .advancement_feed
            .polled_at
            .is_none_or(|at| now.duration_since(at) >= PROGRESS_POLL);
        if self.ui.is_advancements() || stale {
            let store = self.sim.advancements();
            self.advancement_feed.progress = AdvancementProgress::from_store(&store);
            self.advancement_feed.polled_at = Some(now);
            self.advancement_feed
                .toasts
                .observe(&self.advancement_feed.progress);
        }
        self.advancement_feed.progress.clone()
    }

    /// The advancement whose completion toast belongs on screen this frame.
    pub(super) fn advancement_toast(
        &mut self,
        now_ms: u64,
    ) -> Option<&'static crate::menu::advancement_data::Advancement> {
        self.advancement_feed.toasts.current(now_ms)
    }

    /// Resolve a click against the Advancements screen, returning whether it was
    /// consumed.
    ///
    /// A click on a widget selects nothing: vanilla's advancement widgets are not
    /// buttons — the only thing a click does there is nothing at all, and the
    /// *hover* is what shows the title. Consumed anyway so it does not fall
    /// through to the paused world behind.
    pub(super) fn handle_advancements_click(&mut self, w: u32, h: u32) -> bool {
        let gui_scale = self.nav.gui_scale();
        let (cx, cy) = self.cursor;
        let progress = self.advancement_progress();
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, &progress, gui_scale, w, h) else {
            return false;
        };
        let Some(hit) = advancements_hit_test(&layout, gui_scale, w, h, cx, cy) else {
            return false;
        };
        match hit {
            AdvancementsHit::Tab(i) => {
                if i < advancement_tabs().len() {
                    state.select_tab(i);
                }
            }
            AdvancementsHit::Viewport => self.advancements_drag = Some((cx, cy)),
            AdvancementsHit::Widget(_) | AdvancementsHit::Window => {}
        }
        true
    }

    /// Continue a viewport drag — vanilla pans the tree by the pointer delta
    /// (`AdvancementsScreen.mouseDragged` forwards to `AdvancementTab.scroll`).
    pub(super) fn drag_advancements(&mut self, w: u32, h: u32) {
        let Some((px, py)) = self.advancements_drag else {
            return;
        };
        let gui_scale = self.nav.gui_scale();
        let scale = crate::config::calculate_gui_scale(gui_scale, w, h).max(1) as f32;
        let (dx, dy) = ((self.cursor.0 - px) / scale, (self.cursor.1 - py) / scale);
        self.advancements_drag = Some(self.cursor);
        let progress = self.advancement_progress();
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, &progress, gui_scale, w, h) else {
            return;
        };
        let tree = layout.tree;
        state.pan(&tree, dx, dy);
    }

    /// One wheel notch, at vanilla's `SCROLL_SPEED`.
    pub(super) fn scroll_advancements(&mut self, notches: f32, w: u32, h: u32) {
        let gui_scale = self.nav.gui_scale();
        let progress = self.advancement_progress();
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, &progress, gui_scale, w, h) else {
            return;
        };
        let tree = layout.tree;
        state.scroll_by(&tree, notches);
    }

    /// The hovered widget's index and resolved title/description for this frame,
    /// plus this frame's hover fade.
    ///
    /// Resolved before `redraw` splits its field borrows — the same constraint
    /// [`creative_frame_title`](Self::creative_frame_title) works around. **This
    /// is also where the fade is advanced**, because it is the one per-frame call
    /// that already knows whether anything is hovered; a second walk of the
    /// layout just to tick it would be the same work twice.
    pub(super) fn advancements_hover(&mut self, w: u32, h: u32) -> AdvancementsHoverFrame {
        if !self.ui.is_advancements() {
            return AdvancementsHoverFrame::default();
        }
        let gui_scale = self.nav.gui_scale();
        let (cx, cy) = self.cursor;
        let progress = self.advancement_progress();
        let translate = self.sim.translator();
        let state = self.nav.advancements_mut();
        let hovered = advancements_layout(state, &progress, gui_scale, w, h)
            .and_then(|layout| {
                let hit = advancements_hit_test(&layout, gui_scale, w, h, cx, cy)?;
                let AdvancementsHit::Widget(i) = hit else {
                    return None;
                };
                let advancement = layout.tree.nodes.get(i)?.advancement;
                let resolve = |key: &str, fallback: &str| {
                    translate(key).unwrap_or_else(|| fallback.to_string())
                };
                Some((
                    i,
                    resolve(advancement.title, advancement.title_en),
                    resolve(advancement.description, advancement.description_en),
                ))
            });
        let fade = state.tick_fade(hovered.is_some());
        AdvancementsHoverFrame { hovered, fade }
    }
}

/// One frame's hover resolution: what is under the pointer, and how far the
/// viewport dim has ramped.
#[derive(Debug, Clone, Default)]
pub(super) struct AdvancementsHoverFrame {
    /// `(node index, title, description)`.
    pub(super) hovered: Option<(usize, String, String)>,
    /// `AdvancementTab::fade`.
    pub(super) fade: f32,
}

/// Build one frame of Advancements-screen geometry.
///
/// A free function for [`creative_panel_geometry`](super::creative_panel_geometry)'s
/// reason: `redraw` holds `&mut` borrows of several fields across the frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn advancements_panel_geometry(
    state: &mut AdvancementsState,
    hover: &AdvancementsHoverFrame,
    progress: &AdvancementProgress,
    title: &str,
    renderer: &crate::container::ContainerRenderer,
    models: Option<&lodestone_render::BlockModels>,
    gui_scale: u32,
    w: u32,
    h: u32,
) -> Option<crate::container::ContainerGeometry> {
    let layout = advancements_layout(state, progress, gui_scale, w, h)?;
    let items = renderer.item_atlas();
    Some(advancements_geometry(
        &layout,
        AdvancementsView {
            title,
            hovered: hover.hovered.as_ref().map(|(i, ..)| *i),
            hovered_title: hover.hovered.as_ref().map_or("", |(_, t, _)| t.as_str()),
            hovered_description: hover.hovered.as_ref().map_or("", |(.., d)| d.as_str()),
            progress,
            fade: hover.fade,
        },
        gui_scale,
        w,
        h,
        items.as_deref(),
        models,
        renderer.font(),
        renderer.background_data(),
    ))
}

/// The HUD view for one completion toast.
///
/// A free function for [`advancements_panel_geometry`]'s reason: `redraw` holds
/// `&mut` borrows across the frame, so this takes the `&'static Advancement` and
/// a translator rather than `&self`.
pub(super) fn advancement_toast_view(
    advancement: &'static crate::menu::advancement_data::Advancement,
    translate: &dyn Fn(&str) -> Option<String>,
) -> crate::hud::AdvancementToastView {
    let (heading_key, heading_en, colour) =
        crate::menu::advancements::toast_heading(advancement.frame);
    let resolve = |key: &str, fallback: &str| translate(key).unwrap_or_else(|| fallback.to_string());
    crate::hud::AdvancementToastView {
        heading: resolve(heading_key, heading_en),
        heading_colour: colour,
        title: resolve(advancement.title, advancement.title_en),
        icon: ResourceLocation::parse(advancement.icon)
            .ok()
            .map(|item| crate::hud::HotbarSlot {
                item,
                count: 1,
                damage: None,
                max_damage: None,
                enchanted: false,
            }),
        visible_portion: 1.0,
    }
}

/// The selected tab's title, through the language table with the data pack's own
/// `en_us` value as the fallback.
pub(super) fn advancements_title(
    state: &AdvancementsState,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    let tabs = advancement_tabs();
    let Some(root) = tabs.get(state.tab.min(tabs.len().saturating_sub(1))) else {
        return String::new();
    };
    translate(root.title).unwrap_or_else(|| root.title_en.to_string())
}
