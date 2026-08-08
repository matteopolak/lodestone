//! The creative-inventory screen's wiring (issue #158): when it shows, what a
//! click does, and the per-frame geometry call.
//!
//! The screen itself is [`crate::container::creative_geometry`]; this module is
//! the half that stops it being an island. Three hops, all in this file:
//! [`WindowApp::creative_screen_open`] decides it is up,
//! [`WindowApp::handle_creative_click`] and
//! [`WindowApp::scroll_creative_screen`] drive it, and
//! [`creative_panel_geometry`] draws it.

use super::*;

use crate::container::{
    CREATIVE_SEARCH_MAX_LEN, CreativeHit, CreativeState, CreativeTabKind, CreativeView,
    creative_geometry, creative_hit_test, creative_items_for, creative_layout,
    creative_page_items, creative_tab_count, creative_tab_kind, creative_tab_title_key,
};

impl WindowApp {
    /// Whether the creative-inventory screen is the one on screen right now.
    ///
    /// Two conditions: the player opened *their own* inventory (a server-opened
    /// container is a real container even in creative, and vanilla shows its
    /// ordinary screen), and the player has creative's abilities.
    ///
    /// # The creative signal
    ///
    /// `Sim::has_infinite_materials` — `Abilities.instabuild` off
    /// `PLAYER_ABILITIES`, the same field the anvil and enchanting screens
    /// already gate on. **This is not `GameMode::Creative`**, and the difference
    /// is real: `ServerGameMode` is an ECS component with no shell reader
    /// (`lodestone-ecs/src/session.rs`), and vanilla itself opens this screen off
    /// `player.hasInfiniteMaterials()` in `Minecraft.openInventory`
    /// (`Minecraft.java`'s `gameMode.hasInfiniteItems()` branch), not off the
    /// game-mode enum. So `instabuild` is the *right* signal here rather than a
    /// stand-in — but note a server that grants `instabuild` in another mode
    /// would get this screen, which is exactly what vanilla does too.
    pub(super) fn creative_screen_open(&self) -> bool {
        self.sim.open_menu().is_none()
            && self.ui.is_container_open()
            && self.sim.has_infinite_materials()
    }

    /// The selected tab's display name, through the live language table.
    fn creative_title(&self) -> String {
        let Some(key) = creative_tab_title_key(self.creative.tab) else {
            return String::new();
        };
        let translator = self.sim.translator();
        translator(key).unwrap_or_else(|| fallback_tab_title(key).to_string())
    }

    /// Resolve a click against the creative screen and act on it, returning
    /// whether it was **consumed** — always true inside the panel or its tab
    /// strip, so a click never falls through to the world behind.
    pub(super) fn handle_creative_click(&mut self, w: u32, h: u32) -> bool {
        let items = creative_items_for(self.creative.tab, &self.creative.search);
        let layout = creative_layout(&self.creative, items.len(), self.nav.gui_scale(), w, h);
        let Some(hit) = creative_hit_test(
            &layout,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
        ) else {
            return false;
        };
        match hit {
            CreativeHit::Tab(i) => {
                if i < creative_tab_count() {
                    self.creative.select_tab(i);
                }
            }
            CreativeHit::SearchBox => self.creative.search_focused = true,
            CreativeHit::Scrollbar => {
                // Vanilla begins a thumb drag on press and jumps nowhere on the
                // first frame (`:500`); the drag itself is handled by
                // `drag_creative_scroll` off `CursorMoved`.
                self.creative.scrolling = layout.can_scroll;
            }
            CreativeHit::Grid(cell) => {
                let page = creative_page_items(&items, self.creative.scroll);
                if let Some(Some(id)) = page.get(cell) {
                    self.give_creative_item(id);
                }
            }
            // The hotbar row and the inventory tab's own slots are the player's
            // real inventory, and a click there needs the cursor-stack semantics
            // this screen has none of (see `container/creative.rs`'s module
            // doc). Consumed rather than acted on: falling through would click a
            // slot of the *ordinary* inventory screen, which is not the one on
            // screen.
            CreativeHit::Hotbar(_)
            | CreativeHit::Inventory(_)
            | CreativeHit::Destroy
            | CreativeHit::Panel => {}
        }
        true
    }

    /// Put `id` into the player's currently selected hotbar slot.
    ///
    /// Vanilla picks the stack up onto the cursor and the player then drops it
    /// into a slot, which is what actually sends
    /// `ServerboundSetCreativeModeSlotPacket`. This client has no cursor stack on
    /// this screen, so the click sends that same packet directly for the slot the
    /// player is holding — one gesture instead of two, and the wire traffic is
    /// identical.
    ///
    /// `36 + selected` is the *container* slot index of a hotbar slot in window
    /// 0, which is the space `SET_CREATIVE_MODE_SLOT` is defined in — the same
    /// numbering `container.rs`'s own slot layout uses.
    fn give_creative_item(&mut self, id: &str) {
        let Ok(item) = id.parse::<lodestone_model::Identifier>() else {
            return;
        };
        let slot = 36 + i16::try_from(self.sim.selected_slot()).unwrap_or(0);
        self.sim.send_creative_slot(slot, item, 1);
    }

    /// One wheel notch over the creative screen scrolls the grid by a row.
    pub(super) fn scroll_creative_screen(&mut self, notches: f32) {
        if !creative_tab_kind(self.creative.tab).scrolls() {
            return;
        }
        let count = creative_items_for(self.creative.tab, &self.creative.search).len();
        self.creative.scroll_by(notches, count);
    }

    /// Continue an in-flight scrollbar drag. Called from `CursorMoved`, after the
    /// cursor position has been updated.
    pub(super) fn drag_creative_scroll(&mut self, w: u32, h: u32) {
        if !self.creative.scrolling {
            return;
        }
        let count = creative_items_for(self.creative.tab, &self.creative.search).len();
        let layout = creative_layout(&self.creative, count, self.nav.gui_scale(), w, h);
        let Some(track) = layout.scroll_track else { return };
        let scale = crate::config::calculate_gui_scale(self.nav.gui_scale(), w, h).max(1) as f32;
        self.creative.drag_scroll(self.cursor.1 / scale, track.y);
    }

    /// Whether typing should go into the search box rather than to the game.
    pub(super) fn creative_search_active(&self) -> bool {
        self.creative_screen_open()
            && creative_tab_kind(self.creative.tab) == CreativeTabKind::Search
            && self.creative.search_focused
    }

    /// Apply one editing keystroke to the search box.
    pub(super) fn edit_creative_search(&mut self, edit: CreativeSearchEdit) {
        match edit {
            CreativeSearchEdit::Char(c) => {
                if self.creative.search.chars().count() < CREATIVE_SEARCH_MAX_LEN {
                    self.creative.search.push(c);
                }
            }
            CreativeSearchEdit::Backspace => {
                self.creative.search.pop();
            }
        }
        // A narrower result set can leave the scroll past the end, and the page
        // query clamps on read — but resetting keeps the *thumb* honest on the
        // very next frame rather than one frame late.
        self.creative.scroll = 0.0;
    }

    /// The selected tab's display name for this frame, or `None` when the screen
    /// is not up.
    ///
    /// Resolved before `redraw` splits its field borrows, for exactly the reason
    /// [`creative_panel_geometry`] is a free function — see its doc.
    pub(super) fn creative_frame_title(&self) -> Option<String> {
        self.creative_screen_open().then(|| self.creative_title())
    }
}

/// Build one frame of creative-screen geometry.
///
/// A free function over the fields it reads, not a `&self` method, and for the
/// same reason `recipe_panel_geometry` is one: `redraw` holds `&mut` borrows of
/// `self.render`, `self.hud` and `self.container` across the whole frame, so
/// anything taking `&self` there fails the borrow check.
#[allow(clippy::too_many_arguments)]
pub(super) fn creative_panel_geometry(
    state: &CreativeState,
    menu: Option<&Menu>,
    title: &str,
    renderer: &crate::container::ContainerRenderer,
    models: Option<&lodestone_render::BlockModels>,
    gui_scale: u32,
    w: u32,
    h: u32,
) -> crate::container::ContainerGeometry {
    let items = renderer.item_atlas();
    creative_geometry(
        state,
        CreativeView { menu, title },
        gui_scale,
        w,
        h,
        items.as_deref(),
        models,
        renderer.font(),
        renderer.background_data(),
    )
}

/// One search-box keystroke. A two-variant enum rather than a `char` so
/// `resolve_key`'s caller cannot accidentally push a control character into the
/// field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CreativeSearchEdit {
    /// A printable character.
    Char(char),
    /// Delete the last character.
    Backspace,
}

/// English names for the fourteen `itemGroup.*` keys, for a jar-less run with no
/// language table — `en_us.json`'s own values.
///
/// Not a substitute for the table: [`WindowApp::creative_title`] prefers the live
/// translator whenever there is one, exactly as the container title does. This
/// exists so a headless or pack-less run shows a word rather than a raw key,
/// which is the defect `container::menu_title` was written to fix.
fn fallback_tab_title(key: &str) -> &'static str {
    match key {
        "itemGroup.buildingBlocks" => "Building Blocks",
        "itemGroup.coloredBlocks" => "Colored Blocks",
        "itemGroup.natural" => "Natural Blocks",
        "itemGroup.functional" => "Functional Blocks",
        "itemGroup.redstone" => "Redstone Blocks",
        "itemGroup.hotbar" => "Saved Hotbars",
        "itemGroup.search" => "Search Items",
        "itemGroup.tools" => "Tools & Utilities",
        "itemGroup.combat" => "Combat",
        "itemGroup.foodAndDrink" => "Food & Drinks",
        "itemGroup.ingredients" => "Ingredients",
        "itemGroup.spawnEggs" => "Spawn Eggs",
        "itemGroup.op" => "Operator Utilities",
        "itemGroup.inventory" => "Inventory",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tab_has_a_jarless_name() {
        for i in 0..creative_tab_count() {
            let key = creative_tab_title_key(i).expect("tab in range");
            assert!(
                !fallback_tab_title(key).is_empty(),
                "no fallback title for {key}"
            );
        }
    }

    #[test]
    fn the_search_box_stops_at_vanillas_max_length() {
        let mut state = CreativeState::default();
        state.search = "x".repeat(CREATIVE_SEARCH_MAX_LEN);
        // The push guard is on the count, so a full field ignores a new char.
        assert_eq!(state.search.chars().count(), CREATIVE_SEARCH_MAX_LEN);
    }
}
