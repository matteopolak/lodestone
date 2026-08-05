//! The recipe-book panel's layout, geometry and unlock toasts (issue #163).
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

/// Persisted recipe-book panel UI state (issue #163) — see
/// [`WindowApp::recipe_panel`].
///
/// `tab` is an index into [`crate::container::RecipeBookPanelLayout::tabs`],
/// which is [`lodestone_game::recipe::RecipeBook::visible_tabs`]'s own order;
/// `None` is the all-categories view. `page` is clamped by
/// [`crate::container::recipe_book_panel_contents`] on read, so a stale page
/// left over from a wider search degrades to the last real page rather than
/// showing an empty grid.
#[derive(Debug, Default, Clone)]
pub(super) struct RecipePanelState {
    /// Whether the panel body is open. The toggle button draws either way.
    pub(super) open: bool,
    /// Current search text (substring match on the result id — see
    /// `RecipeBook::browse`).
    pub(super) search: String,
    /// Selected category tab, or `None` for all categories.
    pub(super) tab: Option<usize>,
    /// Current page within the filtered result set.
    pub(super) page: usize,
    /// Whether the search box has keyboard focus, so typing edits
    /// [`Self::search`] instead of reaching the container's own key handling.
    ///
    /// Vanilla focuses its `EditBox` the same way (a click inside it), and this
    /// flag is what stops `search` being a field nothing ever writes — an
    /// island one layer down.
    pub(super) search_focused: bool,
    /// Vanilla's All/Craftable cycle-button state (`RecipeBookComponent`'s
    /// `filtering`): `true` hides every recipe the player cannot currently
    /// make. Drives both the button art
    /// ([`crate::container::RECIPE_SPRITE_FILTER_ENABLED`]) and the browsed set
    /// ([`crate::container::recipe_book_panel_contents_filtered`]).
    pub(super) filtering: bool,
    /// Whether [`WindowApp::restore_recipe_book_settings`] has already applied
    /// the server's `RECIPE_BOOK_SETTINGS` to this panel for `restored_type`.
    ///
    /// A latch, not a one-shot: the settings are **per book type** while this
    /// panel state is one shared instance (see the type doc), so opening a
    /// furnace after a crafting table must restore again with the furnace's
    /// own values. `None` means nothing has been restored yet this session.
    pub(super) restored_type: Option<lodestone_model::RecipeBookType>,
}

/// Wall-clock milliseconds for the recipe-toast window.
///
/// [`lodestone_game::recipe::RecipeToastQueue`] takes "now" from its caller and
/// only ever compares two of these against each other, so any clock with
/// millisecond resolution works. The epoch clock is used because that is what
/// vanilla's own toast timing is keyed off (`System.currentTimeMillis()`, see
/// `RECIPE_TOAST_DISPLAY_MS`'s doc) — so whoever wires
/// `RecipeToastQueue::push` from the decode reaches for the same function
/// rather than inventing a second, incompatible origin.
pub(super) fn recipe_toast_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// This frame's recipe-unlock toast, if one should be on screen at `now_ms`.
///
/// `now_ms` is injected rather than read here so this is a pure function of the
/// queue plus a timestamp, which is what lets a test drive the toast at an exact
/// point in its 5000ms window without a sleep.
///
/// `visible_portion` is fixed at `1.0` — fully on screen. Vanilla's 600ms slide
/// (`ToastManager.java:229-232`) needs an animation origin, and
/// [`lodestone_game::recipe::RecipeToastQueue`] exposes none (its
/// `last_changed_ms` is private, and it has no notion of a visibility
/// transition). Drawing at rest is the honest subset; whoever lands the decode
/// and gives the queue a real producer is the right person to add the slide, and
/// [`crate::hud::RecipeToastView::visible_portion`] already takes it.
///
/// A free function over the queue rather than a `&self` method: `redraw` holds a
/// `&mut` borrow of `self.render` across the whole frame, so anything taking
/// `&self` there fails the borrow check. Taking the one field it reads keeps the
/// borrows disjoint — and makes it directly unit-testable against a queue with
/// no `WindowApp` in sight.
pub(super) fn recipe_toast_view(
    queue: &lodestone_game::recipe::RecipeToastQueue,
    now_ms: u64,
) -> Option<crate::hud::RecipeToastView> {
    if !queue.visible(now_ms) {
        return None;
    }
    let (station, unlocked) = queue.displayed_entry(now_ms)?;
    Some(crate::hud::RecipeToastView {
        station: toast_icon(station)?,
        unlocked: toast_icon(unlocked)?,
        visible_portion: 1.0,
    })
}

/// The recipe-book panel's own layout, derived from the *same* state and scale
/// the draw uses.
///
/// Shared by the hit-test and draw paths on purpose: `container.rs`'s own
/// `hit_test_with_scale` carries a warning that a layout built with a different
/// `gui_scale` than the frame was drawn with silently mis-resolves every click,
/// and one function used twice is the only way to guarantee they agree.
pub(super) fn recipe_panel_layout(
    panel: &RecipePanelState,
    menu: &Menu,
    gui_scale: u32,
    w: u32,
    h: u32,
    tab_count: usize,
    total_pages: usize,
) -> crate::container::RecipeBookPanelLayout {
    let mut layout = crate::container::recipe_book_panel_layout_with_scale(
        menu,
        gui_scale,
        w,
        h,
        tab_count,
        panel.page > 0,
        panel.page + 1 < total_pages,
    );
    // The geometry layer has no panel state, so the filter art is selected
    // here — see `RecipeBookPanelLayout::filtering`'s own doc. Set in the
    // shared layout builder rather than at the draw site so the hit-test and
    // the draw cannot disagree about which button is on screen.
    layout.filtering = panel.filtering;
    layout
}

/// The Craftable-filter predicate for `menu`: can the player make `id` right
/// now, out of the inventory this menu owns?
///
/// Built from [`lodestone_game::menu::Menu::plan_recipe_auto_fill`] — the same
/// call `auto_fill_recipe` makes when the cell is *clicked*. Sharing the
/// primitive is the point: a Craftable-filtered panel that offered a recipe
/// whose click then did nothing would be worse than no filter at all.
fn craftable_in(
    book: &RecipeBook,
    menu: &Menu,
    id: &lodestone_model::Identifier,
) -> bool {
    book.get(id)
        .and_then(|recipe| menu.plan_recipe_auto_fill(recipe, book.tags()))
        .is_some()
}

/// The panel's contents for one frame as `(tab_count, total_pages, page_ids)`,
/// with the ids **owned** so the borrow of `book` ends before a caller mutates
/// its own panel state.
///
/// Degrades to "no tabs, one empty page" with no corpus loaded (jar-less run),
/// which draws an empty-but-present panel rather than hiding the toggle.
pub(super) fn recipe_panel_contents(
    book: Option<&RecipeBook>,
    panel: &RecipePanelState,
    menu: &Menu,
    book_type: lodestone_model::RecipeBookType,
) -> (usize, usize, Vec<lodestone_model::Identifier>) {
    let Some(book) = book else {
        return (0, 1, Vec::new());
    };
    // `&|_| true` when not filtering, so the per-recipe auto-fill plan — which
    // walks the inventory for every browsed id — is only computed in the state
    // that asked for it.
    let contents = crate::container::recipe_book_panel_contents_filtered(
        book,
        book_type,
        panel.tab,
        &panel.search,
        panel.page,
        &|id| !panel.filtering || craftable_in(book, menu, id),
    );
    (
        contents.tabs.len(),
        contents.total_pages,
        contents.page_ids.into_iter().cloned().collect(),
    )
}

/// Build one frame of recipe-book panel geometry, or `None` when `menu` has no
/// recipe book at all (a chest, an anvil) and the panel is suppressed.
///
/// `items`/`models` are the atlases the icons resolve against; both absent is
/// the jar-less path, which falls back to
/// [`crate::container::recipe_book_panel_geometry`]'s hash-derived colour
/// swatches — the same degradation every other icon in this shell uses, and what
/// lets a headless gate exercise this at all.
///
/// Free rather than a method for the same borrow reason as
/// [`recipe_toast_view`].
#[allow(clippy::too_many_arguments)]
pub(super) fn recipe_panel_geometry(
    book: Option<&RecipeBook>,
    panel: &RecipePanelState,
    menu: &Menu,
    gui_scale: u32,
    items: Option<&lodestone_assets::ItemAtlas>,
    models: Option<&lodestone_render::BlockModels>,
    w: u32,
    h: u32,
) -> Option<crate::container::RecipeBookPanelGeometry> {
    let book_type = recipe_book_type_for(menu)?;
    let (tab_count, total_pages, results) = match book {
        Some(book) => {
            let contents = crate::container::recipe_book_panel_contents_filtered(
                book,
                book_type,
                panel.tab,
                &panel.search,
                panel.page,
                // Same predicate the hit-test path uses (`recipe_panel_contents`
                // above): if these two disagreed, a click would resolve against
                // a different page than the one on screen.
                &|id| !panel.filtering || craftable_in(book, menu, id),
            );
            // `map_while`, not `filter_map`: `page_results[i]` must line up with
            // `layout.recipes[i]`, so a recipe with no result stack has to *end*
            // the slice rather than shift every later icon one cell left.
            // Truncating is the documented "fewer entries than populated cells
            // draws only what is given" behaviour.
            let results: Vec<&lodestone_game::item::ItemStack> = contents
                .page_ids
                .iter()
                .map_while(|id| {
                    book.get(id)
                        .and_then(lodestone_game::recipe::Recipe::result_stack)
                })
                .collect();
            (contents.tabs.len(), contents.total_pages, results)
        }
        None => (0, 1, Vec::new()),
    };
    let layout = recipe_panel_layout(panel, menu, gui_scale, w, h, tab_count, total_pages);
    Some(match items {
        Some(items) => crate::container::recipe_book_panel_geometry_with_icons(
            &layout,
            panel.open,
            panel.tab,
            &results,
            gui_scale,
            w,
            h,
            items,
            models,
        ),
        None => crate::container::recipe_book_panel_geometry(
            &layout,
            panel.open,
            panel.tab,
            &results,
            gui_scale,
            w,
            h,
        ),
    })
}

/// One toast icon: a single-item [`HotbarSlot`] for `id`.
///
/// `None` for an id the [`ResourceLocation`] parser rejects, which suppresses
/// the whole toast rather than drawing half of one.
fn toast_icon(id: &lodestone_model::Identifier) -> Option<HotbarSlot> {
    Some(HotbarSlot {
        item: ResourceLocation::parse(&id.to_string()).ok()?,
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
    })
}

/// Turn an auto-fill plan into the container clicks that realise it.
///
/// # Why this is not "two clicks per step"
///
/// [`lodestone_game::recipe::plan_auto_fill`] emits **one step per grid cell**,
/// each moving a *single* item, and several steps can name the same
/// `source_slot` (one stack of coal supplying three cells). The obvious
/// "pick up from `source_slot`, place into `cell`" pair does not express that,
/// because [`Click::left`] on a slot places the **whole** carried stack
/// (`click.rs`: "pick up whole / place whole") — so a 5-coal stack would land
/// entirely in the first cell and every later cell would be empty.
///
/// The sequence that actually produces one item per cell is vanilla's own
/// manual gesture, grouped by source:
///
/// 1. [`Click::left`] the source slot — pick the whole stack onto the cursor;
/// 2. [`Click::right`] each cell that source supplies — "place one" each;
/// 3. [`Click::left`] the source slot again — return the remainder.
///
/// Step 3 is a no-op when the source was exhausted exactly (left-clicking an
/// empty slot with an empty cursor does nothing), so it needs no guard.
///
/// Grouping is by **first appearance** of each `source_slot`, not by adjacency:
/// steps are ordered by grid cell, so one source's cells need not be
/// consecutive.
pub(super) fn auto_fill_clicks(steps: &[lodestone_game::recipe::PlacementStep]) -> Vec<Click> {
    let mut clicks = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    for step in steps {
        if seen.contains(&step.source_slot) {
            continue;
        }
        seen.push(step.source_slot);
        clicks.push(Click::left(step.source_slot));
        for cell in steps
            .iter()
            .filter(|s| s.source_slot == step.source_slot)
            .map(|s| s.cell)
        {
            clicks.push(Click::right(cell));
        }
        clicks.push(Click::left(step.source_slot));
    }
    clicks
}

/// Which recipe book, if any, a menu shows — the same fork
/// [`lodestone_game::menu::Menu::plan_recipe_auto_fill`] makes internally, kept
/// in one place so the panel's *contents* and its *auto-fill* can never
/// disagree about which book they are in.
///
/// `None` means this menu has no recipe book at all (a chest, an anvil), and
/// the panel is suppressed entirely rather than drawing an empty one.
pub(super) fn recipe_book_type_for(menu: &Menu) -> Option<lodestone_model::RecipeBookType> {
    use lodestone_game::menu::SpecialLayout;
    use lodestone_model::RecipeBookType;
    if menu.craft_layout().is_some() {
        return Some(RecipeBookType::Crafting);
    }
    match menu.special_layout()? {
        SpecialLayout::Furnace => Some(RecipeBookType::Furnace),
        SpecialLayout::BlastFurnace => Some(RecipeBookType::BlastFurnace),
        SpecialLayout::Smoker => Some(RecipeBookType::Smoker),
        _ => None,
    }
}
