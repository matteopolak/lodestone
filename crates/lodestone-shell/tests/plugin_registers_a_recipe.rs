//! Issue #148: a recipe a **plugin** registered reaches the crafting screen.
//!
//! # Why this test exists and the unit tests are not enough
//!
//! `lodestone_ecs::recipes`' own unit tests register through the public API and
//! then ask `RecipeBook::match_grid` whether it matched. That is a closed loop:
//! it proves the registry and the matcher agree, and would pass unchanged if
//! nothing in the shell ever read the registry — which is precisely the island
//! `CLAUDE.md`'s rule 1 describes, and how this repo has shipped nine of them.
//!
//! So this test drives the seam end to end, in the order the real process does:
//!
//! 1. compose the client's plugin set with [`Sim::client_app`], exactly as
//!    `docs/plugin-api.md` tells a third-party author to;
//! 2. add a plugin that registers a recipe from inside `Plugin::build`, through
//!    `RecipeRegistryExt::add_recipe` and nothing else — no privileged access, no
//!    hand-built resource;
//! 3. build a [`Sim`] around it, which is what `WindowApp` does;
//! 4. adopt a corpus, the way `WindowApp::adopt_recipe_corpus` does at GPU
//!    bring-up — *after* every plugin has already built;
//! 5. run the **real container geometry** over a crafting menu holding the
//!    plugin's ingredients, and assert the ghost-preview result actually draws.
//!
//! # What is asserted, and why it needs no `client.jar`
//!
//! The ghost preview draws the predicted stack's icon *and* a dim quad over it
//! (`container/geometry.rs`, the `craft_layout` block). The icon needs an
//! `ItemAtlas` and therefore a jar; the dim quad does not — it is a plain colour
//! quad, and its colour `[0.05, 0.05, 0.05, 0.55]` occurs at exactly **one**
//! place in the whole shell, that block. It is therefore an atlas-free,
//! unambiguous detector for "the ghost preview fired".
//!
//! Per `CLAUDE.md`'s "measure by location, never by frame average", the
//! assertion is not merely that such a quad exists: its bounding box is compared
//! against the result slot's own rect, taken from the same `slot_layout` lookup
//! the draw itself uses, so a quad drawn anywhere else would fail.
//!
//! # The control
//!
//! `control_without_the_plugin_the_ghost_preview_never_fires` is byte-identical
//! except that it does not add the plugin. It must find **zero** dim quads. Run
//! it and watch it fail if you break the detector — a passing gate with a
//! passing control proves nothing.

use lodestone::config::Config;
use lodestone::container::{ContainerFrame, ContainerGeometry, panel_origin, slot_layout};
use lodestone::sim::Sim;
use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::recipes::{RecipeRegistry, RecipeRegistryExt};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::Menu;
use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, RecipeRegistration, ShapelessRecipe};
use lodestone_model::Identifier;

/// The exact colour the ghost-preview dim quad is drawn with
/// (`container/geometry.rs`). Restated here rather than imported because it is
/// not public — and because a test that read the constant from the code under
/// test could not notice the code changing it.
const GHOST_DIM: [f32; 4] = [0.05, 0.05, 0.05, 0.55];

/// Floats per vertex on `ContainerGeometry::verts`: `[x, y, r, g, b, a]`.
const FLOATS_PER_VERTEX: usize = 6;

fn id(s: &str) -> Identifier {
    s.parse().expect("test id parses")
}

/// The plugin under test — a third-party plugin as `docs/plugin-api.md`
/// describes one, with no access this crate does not give every consumer.
struct SparkleStickPlugin;

impl Plugin for SparkleStickPlugin {
    fn build(&self, app: &mut App) {
        let recipe = Recipe::Shapeless(ShapelessRecipe::new(
            vec![
                Ingredient::Item(id("minecraft:stick")),
                Ingredient::Item(id("minecraft:diamond")),
            ],
            ItemStack::new(id("minecraft:blaze_rod"), 1),
        ));
        app.add_recipe(RecipeRegistration::new(
            id("sparkle:sparkle_stick"),
            recipe,
        ))
        .expect("a fresh id outside the minecraft: namespace registers");
    }
}

/// The corpus `client.jar` would have supplied. Deliberately holds a recipe that
/// is *not* grid-matchable, so nothing in it can satisfy the assertion by
/// accident: if a ghost preview fires, only the plugin's recipe can have caused
/// it.
fn stand_in_vanilla_corpus() -> RecipeBook {
    let mut book = RecipeBook::new();
    book.insert(
        id("minecraft:some_special_recipe"),
        Recipe::Special("crafting_special_firework_rocket".to_owned()),
    );
    book
}

/// Runs steps 1-4 of the module doc and returns the merged corpus the shell
/// would have cached, or `None` if the registry never got installed.
///
/// `with_plugin` is the single bit the test and its control differ by.
fn corpus_after_startup(with_plugin: bool) -> Option<RecipeBook> {
    let mut app = Sim::client_app();
    if with_plugin {
        // The one line a plugin author writes. Note it runs *now*, before any
        // corpus exists — which is the load-order problem `RecipeRegistry`
        // exists to solve.
        app.add_plugins(SparkleStickPlugin);
    }
    let sim = Sim::from_app(app, Config::default());

    // Exactly what `WindowApp::adopt_recipe_corpus` does at GPU bring-up.
    let book = lodestone_ecs::hold_write(sim.ecs(), |world| {
        let mut registry = world.get_resource_or_insert_with(RecipeRegistry::default);
        registry.adopt_corpus(stand_in_vanilla_corpus());
        registry.snapshot()
    });
    (!book.is_empty()).then_some(book)
}

/// A 3x3 crafting menu whose grid holds the plugin recipe's two ingredients and
/// whose result slot is **empty** — the ghost preview's precondition.
fn crafting_menu_holding_the_ingredients() -> Menu {
    let mut menu = Menu::crafting(3, 3);
    let craft = menu
        .craft_layout()
        .expect("a crafting menu has a craft layout");
    menu.set_slot_item(
        craft.first_input,
        Some(ItemStack::new(id("minecraft:stick"), 1)),
    );
    menu.set_slot_item(
        craft.first_input + 1,
        Some(ItemStack::new(id("minecraft:diamond"), 1)),
    );
    assert!(
        menu.slot_item(craft.result_slot).is_none(),
        "precondition: the server has not sent a result, so the ghost may draw"
    );
    menu
}

/// Every dim-quad vertex's `(x, y)` in NDC.
fn ghost_quad_vertices(verts: &[f32]) -> Vec<(f32, f32)> {
    verts
        .chunks_exact(FLOATS_PER_VERTEX)
        .filter(|v| {
            // Exact equality: these are literal constants copied through the
            // vertex builder, never the result of arithmetic.
            v[2] == GHOST_DIM[0] && v[3] == GHOST_DIM[1] && v[4] == GHOST_DIM[2] && v[5] == GHOST_DIM[3]
        })
        .map(|v| (v[0], v[1]))
        .collect()
}

/// The container geometry for `book`, at a fixed viewport.
fn geometry_for(book: Option<&RecipeBook>, menu: &Menu) -> ContainerGeometry {
    let frame = ContainerFrame::new(Some(menu), "Crafting").with_recipe_book(book);
    ContainerGeometry::build(&frame, VIEW_W, VIEW_H)
}

const VIEW_W: u32 = 1280;
const VIEW_H: u32 = 720;

#[test]
fn a_plugin_registered_recipe_draws_its_ghost_preview_in_the_result_slot() {
    let book = corpus_after_startup(true).expect("the merged corpus is non-empty");
    assert!(
        book.get(&id("sparkle:sparkle_stick")).is_some(),
        "the plugin's registration survived corpus adoption"
    );

    let menu = crafting_menu_holding_the_ingredients();
    let geo = geometry_for(Some(&book), &menu);
    let quad = ghost_quad_vertices(&geo.verts);
    assert!(
        !quad.is_empty(),
        "the ghost preview must draw for a plugin recipe; found no dim quad at all"
    );

    // Location, not just presence. The expected rect is derived from the same
    // `slot_layout`/`panel_origin` pair the draw uses, so this cannot pass by
    // restating a constant that drifted.
    let layout = slot_layout(&menu);
    let (ox, oy) = panel_origin(&layout, VIEW_W, VIEW_H);
    let craft = menu.craft_layout().expect("craft layout");
    let rect = layout
        .slots
        .iter()
        .find(|r| r.menu_index == craft.result_slot)
        .expect("the result slot has a rect");
    // `panel_origin` and `SlotRect` are in *logical canvas* space; the geometry
    // scales that up to physical pixels by the same effective GUI scale
    // `ContainerGeometry::build` resolves from `AUTO_GUI_SCALE`. Deriving the
    // factor from that same function rather than hardcoding "3" is the point:
    // the first version of this assertion restated the unscaled origin and
    // failed at (748, 216) against an expected (249.3, 72), which is exactly a
    // factor of three — a constant restated instead of derived.
    let scale = lodestone::config::calculate_gui_scale(
        lodestone::config::AUTO_GUI_SCALE,
        VIEW_W,
        VIEW_H,
    ) as f32;
    let expect_px_x = (ox + rect.x) * scale;
    let expect_px_y = (oy + rect.y) * scale;

    // NDC -> pixels, inverting the standard GUI transform.
    let to_px = |(x, y): (f32, f32)| {
        (
            (x + 1.0) * 0.5 * VIEW_W as f32,
            (1.0 - y) * 0.5 * VIEW_H as f32,
        )
    };
    let px: Vec<(f32, f32)> = quad.iter().copied().map(to_px).collect();
    let min_x = px.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let min_y = px.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_x = px.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let max_y = px.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);

    // One pixel of slack for the NDC round trip only; the box must be the slot.
    assert!(
        (min_x - expect_px_x).abs() <= 1.0 && (min_y - expect_px_y).abs() <= 1.0,
        "ghost quad starts at ({min_x}, {min_y}), expected the result slot at \
         ({expect_px_x}, {expect_px_y}); bounding box was \
         ({min_x}, {min_y})..({max_x}, {max_y})"
    );
    assert!(
        (max_x - min_x) > 0.0 && (max_y - min_y) > 0.0,
        "degenerate ghost quad: ({min_x}, {min_y})..({max_x}, {max_y})"
    );
}

/// **The control.** Identical to the test above but for the plugin never being
/// added. If this ever passes while finding a quad, the detector above is
/// measuring something other than the plugin's recipe.
#[test]
fn control_without_the_plugin_the_ghost_preview_never_fires() {
    let book = corpus_after_startup(false).expect("the stand-in corpus is non-empty");
    assert!(
        book.get(&id("sparkle:sparkle_stick")).is_none(),
        "control precondition: nothing registered the recipe"
    );

    let menu = crafting_menu_holding_the_ingredients();
    let geo = geometry_for(Some(&book), &menu);
    let quad = ghost_quad_vertices(&geo.verts);
    assert!(
        quad.is_empty(),
        "with no plugin there is nothing to predict, yet {} dim-quad vertices drew",
        quad.len()
    );
}

/// A second control, for the *detector* rather than the feature: the same grid
/// and the same plugin corpus, with the recipe book withheld from the frame,
/// must also draw nothing. This is what proves the quad is sourced from the
/// book and not from the grid contents alone.
#[test]
fn control_a_frame_with_no_recipe_book_draws_no_ghost() {
    let menu = crafting_menu_holding_the_ingredients();
    let geo = geometry_for(None, &menu);
    assert!(
        ghost_quad_vertices(&geo.verts).is_empty(),
        "the ghost must come from the book, not from the grid"
    );
}

/// Registration reaching the *recipe-book panel* as well as the ghost preview.
///
/// A separate consumer with a separate read path (`browse`, not `match_grid`), so
/// a registration visible in one and not the other — craftable but invisible, or
/// listed but uncraftable — fails here rather than being discovered by a player.
#[test]
fn a_plugin_registered_recipe_is_listed_in_the_recipe_book() {
    use lodestone_model::RecipeBookType;

    let book = corpus_after_startup(true).expect("merged corpus");
    let listed = book.browse(RecipeBookType::Crafting, None, "");
    assert!(
        listed.contains(&&id("sparkle:sparkle_stick")),
        "the plugin's recipe must be browsable; the panel listed {listed:?}"
    );
}

/// A plugin registering *after* startup must reach the same book, and must bump
/// the revision so the shell's per-frame `sync_recipe_book` notices.
#[test]
fn a_registration_made_mid_session_bumps_the_revision_the_shell_polls() {
    let app = Sim::client_app();
    let sim = Sim::from_app(app, Config::default());
    let before = lodestone_ecs::hold_write(sim.ecs(), |world| {
        let mut registry = world.get_resource_or_insert_with(RecipeRegistry::default);
        registry.adopt_corpus(stand_in_vanilla_corpus());
        registry.revision()
    });

    let after = lodestone_ecs::hold_write(sim.ecs(), |world| {
        let mut registry = world.resource_mut::<RecipeRegistry>();
        registry
            .register(RecipeRegistration::new(
                id("sparkle:late"),
                Recipe::Shapeless(ShapelessRecipe::new(
                    vec![Ingredient::Item(id("minecraft:stone"))],
                    ItemStack::new(id("minecraft:diamond"), 1),
                )),
            ))
            .expect("registers mid-session");
        registry.revision()
    });

    assert!(
        after > before,
        "a mid-session registration must move the revision the shell polls \
         ({before} -> {after}), or the cached book never refreshes"
    );
    // And `sim` is used past the guards, so the borrow shape above is the real
    // one the shell has rather than a temporary.
    assert!(sim.ecs().try_read().is_some(), "no guard was leaked");
}
