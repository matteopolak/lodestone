//! The creative-inventory screen's wiring: when it shows, what a
//! click does, and the per-frame geometry call.
//!
//! The screen itself is [`crate::container::creative_geometry`]; this module is
//! the half that stops it being an island. Three hops, all in this file:
//! [`WindowApp::creative_screen_open`] decides it is up,
//! [`WindowApp::handle_creative_click`] and
//! [`WindowApp::scroll_creative_screen`] drive it, and
//! [`creative_panel_geometry`] draws it.

use super::*;

use lodestone_game::click::ContainerInput;

use crate::container::{
    CREATIVE_SEARCH_MAX_LEN, CreativeEffect, CreativeHit, CreativeState, CreativeTabKind,
    CreativeView, creative_click, creative_geometry, creative_hit_test, creative_items_for,
    creative_layout, creative_page_items, creative_tab_count, creative_tab_kind,
    creative_tab_title_key,
};

/// Every window-0 menu slot the player's own inventory occupies: `5..=8` armour,
/// `9..=35` main, `36..=44` hotbar, `45` off-hand. Slots `0..=4` are the 2×2 crafting
/// grid and its result, which vanilla's trash-slot clear does not touch because they
/// are not `Inventory` slots.
const PLAYER_SECTION_SLOTS: std::ops::RangeInclusive<usize> = 5..=45;

impl WindowApp {
    /// Whether the creative-inventory screen is the one on screen right now.
    ///
    /// Two conditions: the player opened *their own* inventory (a server-opened
    /// container is a real container even in creative, and vanilla shows its
    /// ordinary screen), and the player has creative's abilities.
    ///
    /// # The creative signal
    ///
    /// `Sim::has_infinite_materials` — vanilla's own creative-abilities flag off
    /// `PLAYER_ABILITIES`, the same field the anvil and enchanting screens
    /// already gate on. **This is not `GameMode::Creative`**, and the difference
    /// is real: `ServerGameMode` is an ECS component with no shell reader
    /// (`lodestone-ecs/src/session.rs`), and vanilla itself opens this screen off
    /// its own infinite-materials check in its own open-inventory routine
    /// (its own game-mode has-infinite-items branch), not off the
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
    ///
    /// `button` and `input` are the raw click, so this one entry point serves the
    /// left/right/middle mouse buttons, shift-click, the hotbar number keys, and the
    /// drop key: vanilla's `slotClicked` is likewise one override for all of them.
    pub(super) fn handle_creative_click(
        &mut self,
        button: i32,
        input: ContainerInput,
        w: u32,
        h: u32,
    ) -> bool {
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
            // Outside the panel and its tab strip with a loaded cursor is vanilla's
            // `hasClickedOutside` drop: left throws the whole stack, right one item.
            // With an empty cursor nothing happens and the click is not consumed, so
            // it still closes nothing and reaches nothing.
            return self.drop_creative_cursor(button);
        };
        match hit {
            // Vanilla's `mouseClicked` gates the tab strip and the scrollbar on
            // `event.button() == 0` and lets any other button fall through to the slot
            // path, where those regions hit nothing.
            CreativeHit::Tab(i) => {
                if button == 0 && i < creative_tab_count() {
                    self.creative.select_tab(i);
                }
            }
            CreativeHit::SearchBox => self.creative.search_focused = true,
            CreativeHit::Scrollbar if button == 0 => {
                // Vanilla begins a thumb drag on press and jumps nowhere on the
                // first frame; the drag itself is handled by
                // `drag_creative_scroll` off `CursorMoved`.
                self.creative.scrolling = layout.can_scroll;
            }
            // Everything that touches a stack — the item list, the hotbar row, the
            // inventory tab's slots and the trash slot — goes through the one ported
            // click matrix. Both halves of Matthew's "like a chest, except copied"
            // live in there, and neither is a copy flag.
            CreativeHit::Grid(_)
            | CreativeHit::Hotbar(_)
            | CreativeHit::Inventory(_)
            | CreativeHit::Destroy => {
                let page = creative_page_items(&items, self.creative.scroll);
                let effects = creative_click(
                    hit,
                    input,
                    button,
                    creative_tab_kind(self.creative.tab),
                    &page,
                    &self.sim.player_menu(),
                );
                self.apply_creative_effects(effects);
            }
            CreativeHit::Scrollbar | CreativeHit::Panel => {}
        }
        true
    }

    /// The keyboard route into [`Self::handle_creative_click`] — the hotbar number
    /// keys, the off-hand key, the drop key and the pick-item key, all of which
    /// vanilla funnels into the same `slotClicked` override the mouse uses.
    ///
    /// Returns whether the creative screen took the key. `false` when it is not up, so
    /// a caller can fall through to the ordinary container path.
    pub(super) fn handle_creative_key(&mut self, button: i32, input: ContainerInput) -> bool {
        if !self.creative_screen_open() {
            return false;
        }
        let Some((w, h)) = self.target.as_ref().map(RenderTarget::size) else {
            return false;
        };
        self.handle_creative_click(button, input, w, h);
        true
    }

    /// A click outside the panel with a loaded cursor — vanilla's `slot == null &&
    /// hasClickedOutside` arm, which is a *throw* rather than a slot interaction:
    /// button 0 drops the whole stack, button 1 splits one off it.
    ///
    /// Returns whether the click was consumed; an empty cursor consumes nothing.
    fn drop_creative_cursor(&mut self, button: i32) -> bool {
        let Some(carried) = self.sim.player_menu().carried().cloned() else {
            return false;
        };
        if button == 0 {
            self.apply_creative_effects(vec![
                CreativeEffect::Drop(carried),
                CreativeEffect::SetCarried(None),
            ]);
        } else {
            let mut one = carried.clone();
            one.set_count(1);
            let mut rest = carried;
            rest.shrink(1);
            self.apply_creative_effects(vec![
                CreativeEffect::Drop(one),
                CreativeEffect::SetCarried(lodestone_game::item::normalize(rest)),
            ]);
        }
        true
    }

    /// Apply the resolved effects, in order.
    ///
    /// Order matters and is the resolver's, not this function's: a click that both
    /// writes a slot and changes the cursor must land the slot first, because the
    /// cursor write is what the *next* click reads.
    fn apply_creative_effects(&mut self, effects: Vec<CreativeEffect>) {
        for effect in effects {
            match effect {
                CreativeEffect::SetCarried(item) => self.sim.set_local_carried(item),
                CreativeEffect::SetSlot { menu_index, item } => {
                    self.sim.apply_creative_slot(menu_index, item);
                }
                // `handleCreativeModeItemDrop` — `SET_CREATIVE_MODE_SLOT` with
                // vanilla's `-1` slot, which the server reads as "throw this into the
                // world". Nothing local to predict: the stack is leaving.
                CreativeEffect::Drop(stack) => {
                    let count = u32::try_from(stack.count()).unwrap_or(1);
                    self.sim
                        .send_creative_slot(-1, stack.item().clone(), count);
                }
                // Vanilla loops `inventoryMenu.getItems()` and reports every slot,
                // so the whole 41-slot player section is cleared one write at a time
                // rather than with a bulk verb that does not exist on the wire.
                CreativeEffect::ClearInventory => {
                    for menu_index in PLAYER_SECTION_SLOTS {
                        self.sim.apply_creative_slot(menu_index, None);
                    }
                }
            }
        }
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
    cursor: Option<[f32; 2]>,
    tooltips: Option<bool>,
    renderer: &crate::container::ContainerRenderer,
    models: Option<&lodestone_render::BlockModels>,
    gui_scale: u32,
    w: u32,
    h: u32,
) -> crate::container::ContainerGeometry {
    let items = renderer.item_atlas();
    creative_geometry(
        state,
        CreativeView {
            menu,
            title,
            cursor,
            tooltips,
        },
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
}
