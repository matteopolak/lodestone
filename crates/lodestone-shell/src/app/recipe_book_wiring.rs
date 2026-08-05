//! The recipe-book wiring gates, unwrapped verbatim out of `app.rs`.
//!
//! The module's own doc comment stays on the `mod` declaration in `app.rs`,
//! attached to what it documents.

use super::*;
use lodestone_game::item::ItemStack;
use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe, TagResolver};
use lodestone_model::Identifier;

fn id(name: &str) -> Identifier {
    name.parse().expect("valid identifier")
}

fn stack(name: &str, count: i32) -> ItemStack {
    ItemStack::new(id(name), count)
}

/// A canvas big enough that the panel is *not* pushed against the
/// `RECIPE_PANEL_MIN_X` clamp, so the layout under test is the ordinary one.
const W: u32 = 1280;
const H: u32 = 800;

/// The torch: `1` wide, `2` tall — coal over stick.
///
/// Chosen because its arithmetic is **falsifiable**. Laid row-major into a
/// 3-wide grid the two ingredients occupy cells `0` and `3`, because the
/// stride is the *grid's* width and not the shape's. A hand-count that used
/// the shape's width predicts `0` and `1`, and that prediction is wrong —
/// which is exactly why this recipe is the subject rather than a 1×1 one
/// that cannot tell the two apart.
fn torch() -> Recipe {
    Recipe::Shaped(ShapedRecipe::new(
        1,
        2,
        vec![
            Some(Ingredient::Item(id("minecraft:coal"))),
            Some(Ingredient::Item(id("minecraft:stick"))),
        ],
        stack("minecraft:torch", 4),
    ))
}

fn torch_book() -> RecipeBook {
    let mut book = RecipeBook::new();
    book.insert(id("minecraft:torch"), torch());
    book
}

// -- click-to-fill ---------------------------------------------------

/// The dispatch loop's **resulting slot contents**, not merely that clicks
/// were issued.
///
/// This is the assertion that would have caught the plan this change was
/// briefed with. "Two `ContainerClick`s per step — pick up from
/// `source_slot`, place into `cell`" reads correctly and is wrong:
/// `Click::left` on a slot places the **whole** carried stack, so a 5-coal
/// stack would land entirely in cell 0. See [`auto_fill_clicks`].
#[test]
fn auto_fill_puts_exactly_one_item_in_each_grid_cell() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
    menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
    let book = torch_book();
    let steps = menu
        .plan_recipe_auto_fill(book.get(&id("minecraft:torch")).expect("recipe"), book.tags())
        .expect("the plan must exist — both ingredients are in the inventory");

    // `craft.first_input == 1` for a crafting table, so grid cells 0 and 3
    // are menu slots 1 and 4.
    assert_eq!(
        steps.iter().map(|s| s.cell).collect::<Vec<_>>(),
        vec![1, 4],
        "row-major into a 3-wide grid: cells 0 and 3, offset by first_input"
    );

    for click in auto_fill_clicks(&steps) {
        click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
    }

    assert_eq!(
        menu.slot_item(1).map(|s| (s.item().to_string(), s.count())),
        Some(("minecraft:coal".to_string(), 1)),
        "cell 0 must hold exactly ONE coal, not the whole stack"
    );
    assert_eq!(
        menu.slot_item(4).map(|s| (s.item().to_string(), s.count())),
        Some(("minecraft:stick".to_string(), 1)),
        "cell 3 must hold exactly one stick"
    );
    assert_eq!(
        menu.slot_item(2).map(|s| s.item().to_string()),
        None,
        "cell 1 must stay EMPTY — the 1x2 shape does not occupy it"
    );
    assert_eq!(
        menu.slot_item(12).map(|s| s.count()),
        Some(4),
        "the remainder must be returned to the source slot, not left on the cursor"
    );
    assert_eq!(
        menu.slot_item(20).map(|s| s.count()),
        Some(2),
        "same for the second source"
    );
    assert!(
        menu.carried().is_none(),
        "the cursor must end empty, or the next real click would misbehave"
    );
}

/// The negative control for the gate above, and it is **executed**, not
/// described: the briefed "two clicks per step" sequence, run through the
/// same menu, must fail the same assertion.
///
/// Without this, "one item per cell" is satisfied by any plan that happens
/// to place something, and the magnitude — *how many* — is never under test.
#[test]
fn two_clicks_per_step_would_dump_the_whole_stack_in_one_cell() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
    menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
    let book = torch_book();
    let steps = menu
        .plan_recipe_auto_fill(book.get(&id("minecraft:torch")).expect("recipe"), book.tags())
        .expect("plan");

    // The rejected design: literally two clicks per step, both left.
    let ctx = lodestone_game::click::PlayerCtx::survival();
    for step in &steps {
        Click::left(step.source_slot).apply(&mut menu, ctx);
        Click::left(step.cell).apply(&mut menu, ctx);
    }

    assert_eq!(
        menu.slot_item(1).map(|s| s.count()),
        Some(5),
        "control must observe the WHOLE 5-coal stack in cell 0 — if this ever \
         reads 1, `Click::left` has changed meaning and the real gate above \
         is no longer measuring anything"
    );
    assert_ne!(
        menu.slot_item(1).map(|s| s.count()),
        Some(1),
        "and it must NOT satisfy the real gate's assertion"
    );
}

/// One source stack feeding several cells still leaves one item per cell —
/// the case the "group by source" sequence exists for.
#[test]
fn one_source_stack_can_fill_several_cells() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
    let book = {
        let mut b = RecipeBook::new();
        b.insert(
            id("test:three_coal"),
            Recipe::Shaped(ShapedRecipe::new(
                3,
                1,
                vec![
                    Some(Ingredient::Item(id("minecraft:coal"))),
                    Some(Ingredient::Item(id("minecraft:coal"))),
                    Some(Ingredient::Item(id("minecraft:coal"))),
                ],
                stack("minecraft:coal_block", 1),
            )),
        );
        b
    };
    let steps = menu
        .plan_recipe_auto_fill(book.get(&id("test:three_coal")).expect("recipe"), book.tags())
        .expect("plan");
    for click in auto_fill_clicks(&steps) {
        click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
    }
    for cell in [1usize, 2, 3] {
        assert_eq!(
            menu.slot_item(cell).map(|s| s.count()),
            Some(1),
            "cell {cell} must hold exactly one coal"
        );
    }
    assert_eq!(
        menu.slot_item(12).map(|s| s.count()),
        Some(2),
        "5 coal minus 3 placed = 2 returned to the source"
    );
}

/// The plan is all-or-nothing, so a missing ingredient must issue **no**
/// clicks at all rather than half-filling the grid.
#[test]
fn a_missing_ingredient_issues_no_clicks() {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
    let book = torch_book();
    assert!(
        menu.plan_recipe_auto_fill(
            book.get(&id("minecraft:torch")).expect("recipe"),
            book.tags()
        )
        .is_none(),
        "no stick in the inventory, so there must be no plan"
    );
}

// -- the draw pass reaches the screen --------------------------------

/// Rasterise a colour stream's triangles onto a `res × res` grid in NDC and
/// report `(covered_cells, bounding_box)` restricted to `rect`, an
/// `(x0, y0, x1, y1)` NDC box.
///
/// A CPU rasteriser rather than a GPU gate on purpose: this measures whether
/// the wiring puts geometry **where the panel is**, which is a property of
/// the vertices, and it runs in every `cargo test` instead of behind an
/// `#[ignore]`. The bounding box is returned because a bare fraction cannot
/// tell a uniform-but-wrong frame from a localised blob.
fn coverage(
    verts: &[f32],
    rect: (f32, f32, f32, f32),
    res: usize,
) -> (usize, Option<(f32, f32, f32, f32)>) {
    let (rx0, ry0, rx1, ry1) = rect;
    let mut covered = 0usize;
    let mut bbox: Option<(f32, f32, f32, f32)> = None;
    // Cell centres, in NDC.
    let to_ndc = |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32;
    for gy in 0..res {
        for gx in 0..res {
            let (px, py) = (to_ndc(gx), to_ndc(gy));
            if px < rx0 || px > rx1 || py < ry0 || py > ry1 {
                continue;
            }
            let mut hit = false;
            for tri in verts.chunks_exact(6 * 3) {
                let (ax, ay) = (tri[0], tri[1]);
                let (bx, by) = (tri[6], tri[7]);
                let (cx, cy) = (tri[12], tri[13]);
                let d = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
                if d.abs() < f32::EPSILON {
                    continue;
                }
                let w0 = ((bx - px) * (cy - py) - (cx - px) * (by - py)) / d;
                let w1 = ((cx - px) * (ay - py) - (ax - px) * (cy - py)) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    hit = true;
                    break;
                }
            }
            if hit {
                covered += 1;
                bbox = Some(match bbox {
                    None => (px, py, px, py),
                    Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
                });
            }
        }
    }
    (covered, bbox)
}

/// The panel's own rect in NDC, derived from **the same layout expression the
/// draw uses** — never a restated constant.
///
/// A HUD gate in this repo hardcoded a `cluster_top` the draw computed from a
/// moving anchor and reported 0 px for a row that was rendering perfectly.
/// This calls `recipe_panel_layout` exactly as `recipe_panel_geometry` does.
fn panel_rect_ndc(panel: &RecipePanelState, menu: &Menu, tabs: usize, pages: usize) -> (f32, f32, f32, f32) {
    let layout = recipe_panel_layout(panel, menu, 1, W, H, tabs, pages);
    let (cw, ch) = crate::menu::render::logical_canvas(1, W, H);
    let r = layout.panel;
    (
        2.0 * r.x / cw - 1.0,
        1.0 - 2.0 * (r.y + r.h) / ch,
        2.0 * (r.x + r.w) / cw - 1.0,
        1.0 - 2.0 * r.y / ch,
    )
}

fn open_panel() -> RecipePanelState {
    RecipePanelState {
        open: true,
        ..RecipePanelState::default()
    }
}

/// **Every vertex the draw pass will submit must land inside the `[-1, 1]`
/// NDC clip range.**
///
/// This one sweep catches the entire "geometry exists, nothing is on screen"
/// class, and it is the sweep that found both of the bugs the panel's own
/// author hit: tabs at `bx - 30` going off-canvas, and a
/// `Builder::new(1.0, 1.0, None)` placeholder putting every vertex far
/// outside the visible range.
#[test]
fn every_panel_vertex_lands_inside_the_ndc_clip_range() {
    let menu = Menu::crafting(3, 3);
    let book = torch_book();
    let geo = recipe_panel_geometry(
        Some(&book),
        &open_panel(),
        &menu,
        1,
        None,
        None,
        W,
        H,
    )
    .expect("a crafting table has a recipe book");

    assert!(
        geo.vertex_count() > 0,
        "the open panel must emit geometry at all"
    );
    for (i, v) in geo.verts.chunks_exact(6).enumerate() {
        assert!(
            (-1.0..=1.0).contains(&v[0]) && (-1.0..=1.0).contains(&v[1]),
            "vertex {i} at ({}, {}) is outside the NDC clip range — the panel \
             would have geometry and draw nothing",
            v[0],
            v[1]
        );
    }
}

/// The same sweep at a canvas narrow enough to hit the
/// `RECIPE_PANEL_MIN_X` clamp, which is where the tabs previously escaped
/// off-canvas to `x = -1.1218` NDC.
#[test]
fn panel_vertices_stay_on_canvas_at_the_min_x_clamp() {
    let menu = Menu::crafting(3, 3);
    let book = torch_book();
    let geo = recipe_panel_geometry(
        Some(&book),
        &open_panel(),
        &menu,
        1,
        None,
        None,
        420,
        400,
    )
    .expect("recipe book");
    for (i, v) in geo.verts.chunks_exact(6).enumerate() {
        assert!(
            (-1.0..=1.0).contains(&v[0]) && (-1.0..=1.0).contains(&v[1]),
            "vertex {i} at ({}, {}) escaped the canvas at the clamp",
            v[0],
            v[1]
        );
    }
}

/// **Coverage inside the recipe book's own screen rect.**
///
/// The island this closes could not be seen by any test that only checked
/// the geometry was *built*: `container.rs`'s 75 tests all passed while the
/// panel drew nothing. This asserts the vertices actually cover the rect the
/// layout puts the panel in.
#[test]
fn an_open_panel_covers_its_own_screen_rect() {
    let menu = Menu::crafting(3, 3);
    let book = torch_book();
    let panel = open_panel();
    let (tabs, pages, _) = recipe_panel_contents(
        Some(&book),
        &panel,
        &menu,
        lodestone_model::RecipeBookType::Crafting,
    );
    let rect = panel_rect_ndc(&panel, &menu, tabs, pages);
    let geo = recipe_panel_geometry(Some(&book), &panel, &menu, 1, None, None, W, H)
        .expect("recipe book");

    let res = 128;
    let (covered, bbox) = coverage(&geo.verts, rect, res);
    // Total grid cells whose centre falls inside the rect, so the fraction
    // below is "of the panel", not "of the screen".
    let inside = {
        let (mut n, to_ndc) = (0usize, |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32);
        for gy in 0..res {
            for gx in 0..res {
                let (px, py) = (to_ndc(gx), to_ndc(gy));
                if px >= rect.0 && px <= rect.2 && py >= rect.1 && py <= rect.3 {
                    n += 1;
                }
            }
        }
        n
    };
    assert!(inside > 0, "the panel rect must contain sample points at all");
    let fraction = covered as f32 / inside as f32;
    assert!(
        fraction > 0.9,
        "an open panel must fill its own rect: covered {covered}/{inside} \
         ({fraction:.3}) inside rect {rect:?}, covered bbox {bbox:?}"
    );
}

/// The **executed** negative control for the coverage gate: a *closed*
/// panel draws only its toggle button, which lives on the container's own
/// chrome and is nowhere inside the book panel's rect. It must fail the same
/// assertion.
///
/// This is what distinguishes "the panel is drawn" from "something, anything,
/// emitted vertices" — and note what else already paints here: nothing, since
/// this measures the panel geometry's own stream in isolation rather than a
/// composited frame.
#[test]
fn a_closed_panel_fails_the_coverage_assertion() {
    let menu = Menu::crafting(3, 3);
    let book = torch_book();
    let open = open_panel();
    let closed = RecipePanelState::default();
    let (tabs, pages, _) = recipe_panel_contents(
        Some(&book),
        &open,
        &menu,
        lodestone_model::RecipeBookType::Crafting,
    );
    // The *same* rect the positive gate measures — derived from the open
    // layout, so the control differs only in what was drawn.
    let rect = panel_rect_ndc(&open, &menu, tabs, pages);
    let geo = recipe_panel_geometry(Some(&book), &closed, &menu, 1, None, None, W, H)
        .expect("recipe book");

    let (covered, bbox) = coverage(&geo.verts, rect, 128);
    assert_eq!(
        covered, 0,
        "a closed panel must cover NONE of the book rect (bbox {bbox:?}) — if \
         this ever passes, the positive gate above is measuring something \
         other than the panel body"
    );
    assert!(
        geo.vertex_count() > 0,
        "but it must still emit the toggle button, or the control is vacuous \
         for a different reason: nothing drawn at all"
    );
}

/// A menu with no recipe book at all draws no panel — so a chest does not
/// grow a recipe-book toggle.
#[test]
fn a_menu_without_a_recipe_book_draws_no_panel() {
    let chest = Menu::generic(27);
    assert!(
        recipe_book_type_for(&chest).is_none(),
        "a chest has no recipe book"
    );
    assert!(
        recipe_panel_geometry(None, &open_panel(), &chest, 1, None, None, W, H).is_none(),
        "and therefore emits no geometry"
    );
}

/// The furnace family maps to its own book, not the crafting one — the fork
/// `Menu::plan_recipe_auto_fill` makes internally, kept in agreement.
#[test]
fn book_type_matches_the_menu() {
    assert_eq!(
        recipe_book_type_for(&Menu::crafting(3, 3)),
        Some(lodestone_model::RecipeBookType::Crafting)
    );
}

// -- the toast --------------------------------------------------------

/// The toast reaches [`crate::hud::HudFrame`] from a queue with a real
/// entry, at a timestamp inside the display window.
///
/// Driven through a `RecipeToastQueue` the test fills itself. That is the
/// **test-only injection point** this feature needs, and deliberately not a
/// fake producer in production code: the live producer is the
/// `recipe_book_add` decode, which does not exist yet.
#[test]
fn a_queued_unlock_becomes_a_toast_view() {
    let mut queue = lodestone_game::recipe::RecipeToastQueue::new();
    let now = 1_000_000u64;
    queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), now);

    let view = recipe_toast_view(&queue, now + 100).expect("inside the 5000ms window");
    assert_eq!(view.station.item.to_string(), "minecraft:crafting_table");
    assert_eq!(view.unlocked.item.to_string(), "minecraft:torch");
    assert_eq!(view.visible_portion, 1.0);
}

/// The control for the gate above: past the 5000ms window there is no
/// toast, and an empty queue never produces one. Both must fail the same
/// `expect`.
#[test]
fn the_toast_expires_and_an_empty_queue_never_shows_one() {
    let mut queue = lodestone_game::recipe::RecipeToastQueue::new();
    let now = 1_000_000u64;
    assert!(
        recipe_toast_view(&queue, now).is_none(),
        "an empty queue must not produce a toast — this is the state every \
         real session is in until the decode lands"
    );
    queue.push(id("minecraft:crafting_table"), id("minecraft:torch"), now);
    assert!(
        recipe_toast_view(&queue, now + lodestone_game::recipe::RECIPE_TOAST_DISPLAY_MS).is_none(),
        "and it must expire exactly at DISPLAY_TIME"
    );
}

// -- the All/Craftable filter (issue #436) ----------------------------
//
// `SessionRecipeBookSettings` folded a `filtering` bit that had nothing to
// land in: `RecipePanelState` had no such field, the cycle-button was
// hit-tested into a no-op arm, and the sprite was hardcoded to the
// `filter_disabled` art with a doc comment saying so. These three gates are
// the "something on screen changes" half — the button's art, and the browsed
// set behind it.

/// A crafting menu stocked so the torch **is** craftable: coal and stick in
/// the inventory, which is exactly what `plan_recipe_auto_fill` looks for.
fn stocked_crafting_menu() -> Menu {
    let mut menu = Menu::crafting(3, 3);
    menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
    menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
    menu
}

/// **The filter button draws different art in its two states.**
///
/// A *two-hypothesis* assertion rather than a tolerance or a bare
/// "is not empty": the sprite id at the filter rect is required to equal the
/// enabled art **and** to differ from the disabled art, in the filtering
/// state, with both hypotheses named. Asserting only "some sprite is here"
/// would pass against the hardcoded `RECIPE_SPRITE_FILTER` this replaces —
/// which is the whole defect.
///
/// Measured by **location**: the sprite is found by its `dst` rect matching
/// `layout.filter_button`, not by index into the sprite list, so a reordering
/// of the stream cannot silently point this at the page arrow.
#[test]
fn the_filter_button_swaps_its_art_between_all_and_craftable() {
    let menu = stocked_crafting_menu();
    let book = torch_book();

    let sprite_at_filter_rect = |filtering: bool| -> String {
        let panel = RecipePanelState { open: true, filtering, ..RecipePanelState::default() };
        let (tabs, pages, _) =
            recipe_panel_contents(Some(&book), &panel, &menu, lodestone_model::RecipeBookType::Crafting);
        let layout = recipe_panel_layout(&panel, &menu, 1, W, H, tabs, pages);
        let geo = recipe_panel_geometry(Some(&book), &panel, &menu, 1, None, None, W, H)
            .expect("recipe book");
        let want = layout.filter_button;
        geo.sprites
            .iter()
            .find(|s| {
                (s.dst[0] - want.x).abs() < 0.01
                    && (s.dst[1] - want.y).abs() < 0.01
                    && (s.dst[2] - want.w).abs() < 0.01
            })
            .unwrap_or_else(|| {
                panic!(
                    "no sprite at the filter-button rect \
                     [x={} y={} w={} h={}] — sprites present: {:?}",
                    want.x,
                    want.y,
                    want.w,
                    want.h,
                    geo.sprites.iter().map(|s| (s.id, s.dst)).collect::<Vec<_>>()
                )
            })
            .id
            .to_string()
    };

    let all = sprite_at_filter_rect(false);
    let craftable = sprite_at_filter_rect(true);

    assert_eq!(
        all,
        crate::container::RECIPE_SPRITE_FILTER,
        "the All state must still draw vanilla's filter_disabled art"
    );
    // The two hypotheses, both named: it is the enabled art, and it is *not*
    // the disabled art. The second is what fails against the hardcoded id.
    assert_eq!(
        craftable,
        crate::container::RECIPE_SPRITE_FILTER_ENABLED,
        "the Craftable state must draw vanilla's filter_enabled art"
    );
    assert_ne!(
        craftable, all,
        "the two states must be distinguishable on screen — a single hardcoded \
         sprite id passes every other assertion in this test"
    );
}

/// **The Craftable filter actually narrows the browsed set — an exact count,
/// not a direction.**
///
/// Both hypotheses are computed from outside the code under test: the torch
/// is the only recipe in the corpus, so All must show exactly `1` and
/// Craftable-with-nothing-in-hand exactly `0`. Asserting merely "filtered
/// <= unfiltered" is satisfied by a filter that does nothing.
#[test]
fn the_craftable_filter_hides_a_recipe_the_player_cannot_make() {
    let book = torch_book();
    let empty = Menu::crafting(3, 3);
    let stocked = stocked_crafting_menu();

    let count = |menu: &Menu, filtering: bool| -> usize {
        let panel = RecipePanelState { open: true, filtering, ..RecipePanelState::default() };
        let (_, _, ids) =
            recipe_panel_contents(Some(&book), &panel, menu, lodestone_model::RecipeBookType::Crafting);
        ids.len()
    };

    assert_eq!(count(&empty, false), 1, "All shows the torch regardless of inventory");
    assert_eq!(
        count(&empty, true),
        0,
        "Craftable must hide the torch with no coal and no stick in the inventory"
    );
    // The control that proves the filter is reading the *inventory* and not
    // simply returning nothing whenever `filtering` is set — the premise this
    // gate would otherwise leave untested.
    assert_eq!(
        count(&stocked, true),
        1,
        "Craftable must still show the torch once coal and stick are in hand — \
         a filter that always returns nothing passes the assertion above"
    );
}

/// **The control for both gates above, run and observed.**
///
/// The filtered page must be a strict subset of the unfiltered one, and the
/// panel geometry must still be drawable in the filtering state — a filter
/// that emptied the corpus would break pagination (`total_pages` is
/// `max(1)`, and `page` is clamped against it).
#[test]
fn a_filtered_empty_page_still_paginates_and_draws() {
    let book = torch_book();
    let menu = Menu::crafting(3, 3);
    // A stale page from a wider search, which the clamp must absorb.
    let panel = RecipePanelState {
        open: true,
        filtering: true,
        page: 7,
        ..RecipePanelState::default()
    };
    let (_, pages, ids) =
        recipe_panel_contents(Some(&book), &panel, &menu, lodestone_model::RecipeBookType::Crafting);
    assert_eq!(pages, 1, "an empty filtered set is page 0 of 1, never 0 of 0");
    assert!(ids.is_empty(), "nothing is craftable here");
    let geo = recipe_panel_geometry(Some(&book), &panel, &menu, 1, None, None, W, H)
        .expect("the panel must still be built with an empty filtered page");
    assert!(
        geo.chrome_vertex_count > 0,
        "the panel chrome must still be drawn — an empty result set hides recipes, not the book"
    );
}
