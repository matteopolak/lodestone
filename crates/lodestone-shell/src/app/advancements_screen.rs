//! The Advancements screen's wiring (issue #167): clicks, panning, and the
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
    AdvancementsHit, AdvancementsState, AdvancementsView, advancements_geometry,
    advancements_hit_test, advancements_layout, advancement_tabs,
};

impl WindowApp {
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
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, gui_scale, w, h) else {
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
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, gui_scale, w, h) else {
            return;
        };
        let tree = layout.tree;
        state.pan(&tree, dx, dy);
    }

    /// One wheel notch, at vanilla's `SCROLL_SPEED`.
    pub(super) fn scroll_advancements(&mut self, notches: f32, w: u32, h: u32) {
        let gui_scale = self.nav.gui_scale();
        let state = self.nav.advancements_mut();
        let Some(layout) = advancements_layout(state, gui_scale, w, h) else {
            return;
        };
        let tree = layout.tree;
        state.scroll_by(&tree, notches);
    }

    /// The hovered widget's index and resolved title for this frame, or `None`.
    ///
    /// Resolved before `redraw` splits its field borrows — the same constraint
    /// [`creative_frame_title`](Self::creative_frame_title) works around.
    pub(super) fn advancements_hover(&mut self, w: u32, h: u32) -> Option<(usize, String)> {
        if !self.ui.is_advancements() {
            return None;
        }
        let gui_scale = self.nav.gui_scale();
        let (cx, cy) = self.cursor;
        let translate = self.sim.translator();
        let state = self.nav.advancements_mut();
        let layout = advancements_layout(state, gui_scale, w, h)?;
        match advancements_hit_test(&layout, gui_scale, w, h, cx, cy)? {
            AdvancementsHit::Widget(i) => {
                let advancement = layout.tree.nodes.get(i)?.advancement;
                let title = translate(advancement.title)
                    .unwrap_or_else(|| advancement.title_en.to_string());
                Some((i, title))
            }
            _ => None,
        }
    }
}

/// Build one frame of Advancements-screen geometry.
///
/// A free function for [`creative_panel_geometry`](super::creative_panel_geometry)'s
/// reason: `redraw` holds `&mut` borrows of several fields across the frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn advancements_panel_geometry(
    state: &mut AdvancementsState,
    hovered: Option<(usize, &str)>,
    title: &str,
    renderer: &crate::container::ContainerRenderer,
    models: Option<&lodestone_render::BlockModels>,
    gui_scale: u32,
    w: u32,
    h: u32,
) -> Option<crate::container::ContainerGeometry> {
    let layout = advancements_layout(state, gui_scale, w, h)?;
    let items = renderer.item_atlas();
    Some(advancements_geometry(
        &layout,
        AdvancementsView {
            title,
            hovered: hovered.map(|(i, _)| i),
            hovered_title: hovered.map_or("", |(_, t)| t),
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
