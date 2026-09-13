//! Tests for connection loading, window policy, inventory focus, and stonecutter/menu setup.

use super::*;

//! `app`'s unit tests, unwrapped verbatim out of `app.rs`.
//!
//! Kept as a single file on purpose: splitting it would rename every test
//! path (`app::tests::foo` -> `app::tests::input::foo`), and those names are
//! used by diagnostics and documentation across the repo.

use super::*;
use super::session::container_cursor_center;
use crate::menu::Screen;
use lodestone_data::item::Item;

fn benchmark_config(workload: crate::config::BenchmarkWorkload) -> Config {
    Config {
        benchmark: Some(crate::config::BenchmarkConfig {
            workload,
            debug_overlay: crate::config::BenchmarkDebugOverlay::Closed,
            heavyweight: None,
            warmup: Duration::from_secs(20),
            mutation: Duration::ZERO,
            stationary: Duration::from_secs(30),
            moving: Duration::from_secs(60),
        }),
        ..Config::default()
    }
}
/// The connection screen is the first production consumer of singleplayer's
/// terrain observations. Keep a progress-bearing frame on the full-frame path
/// so a fast integrated-server join cannot jump from an empty handshake label
/// straight to gameplay without ever submitting the map geometry.
#[test]
fn connection_loading_frame_carries_the_real_progress_and_grid() {
    use crate::menu::loading::{ChunkCellStatus, TerrainChunkGrid, TerrainProgress};
    use crate::menu::render::{CHUNK_CELL_FULL, geometry};

    let progress = TerrainProgress {
        loaded: 3,
        expected: 9,
    };
    let grid = TerrainChunkGrid {
        radius: 1,
        center: (4, -2),
        cells: vec![ChunkCellStatus::Full; 9],
    };
    let frame = crate::app::menus::connection_loading_frame(
        crate::menu::loading::ConnectPhase::LoadingTerrain,
        Some(progress),
        Some(grid.clone()),
    );

    assert_eq!(frame.labels[0].text, "Loading terrain...");
    assert_eq!(
        frame.progress.map(|bar| bar.fraction),
        Some(progress.fraction())
    );
    assert_eq!(
        frame.chunk_grid.as_ref().map(|view| view.grid.clone()),
        Some(grid)
    );

    // Geometry is the actual path the menu renderer submits. A Full grid cell
    // contributes opaque white quads, proving the helper is not merely carrying
    // data that no production frame can consume.
    let vertices = geometry(&frame, 320.0, 240.0);
    let full_cells = vertices.chunks_exact(6).filter(|vertex| {
        (vertex[2] - CHUNK_CELL_FULL[0]).abs() < f32::EPSILON
            && (vertex[3] - CHUNK_CELL_FULL[1]).abs() < f32::EPSILON
            && (vertex[4] - CHUNK_CELL_FULL[2]).abs() < f32::EPSILON
            && (vertex[5] - CHUNK_CELL_FULL[3]).abs() < f32::EPSILON
    });
    assert!(full_cells.count() >= 6, "the grid must reach menu geometry");

    let bare = crate::app::menus::connection_loading_frame(
        crate::menu::loading::ConnectPhase::Joining,
        None,
        None,
    );
    assert!(
        bare.progress.is_none(),
        "multiplayer/no-denominator stays bar-less"
    );
    assert!(bare.chunk_grid.is_none());
}
#[test]
fn remote_connection_cannot_relabel_its_join_as_initial_terrain_generation() {
    assert_eq!(
        crate::app::menus::connection_phase_for_scope(
            false,
            crate::menu::loading::ConnectPhase::LoadingTerrain,
        ),
        crate::menu::loading::ConnectPhase::Joining,
    );
    assert_eq!(
        crate::app::menus::connection_phase_for_scope(
            true,
            crate::menu::loading::ConnectPhase::LoadingTerrain,
        ),
        crate::menu::loading::ConnectPhase::LoadingTerrain,
    );
}

#[test]
fn benchmark_policy_is_uncapped_unvsynced_and_uses_physical_1440p() {
    let config = benchmark_config(crate::config::BenchmarkWorkload::Terrain);
    assert_eq!(window_physical_size(&config), Some((2560, 1440)));
    assert_eq!(benchmark_target_fps(&config, Some(120)), None);
    assert_eq!(
        benchmark_present_mode(&config, wgpu::PresentMode::Fifo),
        wgpu::PresentMode::AutoNoVsync
    );
    assert!(!should_background_pace(&config));
}

#[test]
fn benchmark_window_selects_only_the_hardware_builtin_monitor() {
    let monitors = [(15_608_u32, false), (2_941_u32, true), (91_003_u32, false)];

    assert_eq!(select_builtin_monitor(monitors), Some(2_941));
    assert_eq!(
        select_builtin_monitor([(15_608_u32, false), (91_003_u32, false)]),
        None
    );
}

#[test]
fn ordinary_policy_remains_persisted_option_driven() {
    let config = Config::default();
    assert_eq!(window_physical_size(&config), None);
    assert_eq!(benchmark_target_fps(&config, Some(120)), Some(120));
    assert_eq!(
        benchmark_present_mode(&config, wgpu::PresentMode::Fifo),
        wgpu::PresentMode::Fifo
    );
    assert!(should_background_pace(&config));
}

#[test]
fn container_focus_uses_the_integer_physical_framebuffer_centre() {
    assert_eq!(container_cursor_center(1280, 720), (640.0, 360.0));
    // The native cursor API takes integer physical pixels. Keep the same
    // truncating half-size for odd dimensions instead of introducing a
    // half-pixel position that the OS cannot represent.
    assert_eq!(container_cursor_center(1279, 719), (639.0, 359.0));
}

#[test]
fn terminal_inventory_toggle_focuses_only_on_the_open_edge() {
    let mut app = WindowApp::new(Config::default());
    app.ui.enter_dev_world();
    app.cursor = (17.0, 23.0);

    // There is no offscreen target on this constructor, so this test uses the
    // state edge itself as the deterministic control: opening is a transition,
    // while a second toggle is the close edge and must not re-open/re-focus it.
    app.terminal_toggle_inventory();
    assert_eq!(app.ui.screen(), Screen::Container);
    app.terminal_toggle_inventory();
    assert_eq!(app.ui.screen(), Screen::Playing);
    assert_eq!(app.cursor, (17.0, 23.0));
}

fn open_test_stonecutter(
    app: &mut WindowApp,
    result_count: usize,
) -> lodestone_client::OpenMenuSnapshot {
    use lodestone_client::ClientEvent;

    const WINDOW_ID: i32 = 17;
    let stone_id = lodestone_model::ItemId::canonical(u32::from(Item::Stone.registry_id()));
    let slab_id = lodestone_model::ItemId::canonical(u32::from(Item::StoneSlab.registry_id()));
    let ingest = |event| {
        app.sim
            .net()
            .expect("the test attached a loopback client")
            .ingest_session_event(event);
    };
    ingest(ClientEvent::ScreenOpened {
        window_id: WINDOW_ID,
        menu_type: "minecraft:stonecutter".parse().unwrap(),
        title: lodestone_model::Text::literal("Stonecutter"),
    });
    let mut items = vec![None; 38];
    items[0] = Some(lodestone_model::ItemStack::new(
        "minecraft:stone".parse().unwrap(),
        1,
    ));
    ingest(ClientEvent::ContainerContent {
        window_id: WINDOW_ID,
        state_id: lodestone_model::ContainerStateId::new(1),
        items,
        carried_item: None,
    });
    ingest(ClientEvent::RecipePropertySetsUpdated {
        item_sets: Vec::new(),
        stonecutter_results: (0..result_count)
            .map(|_| (vec![stone_id], vec![slab_id]))
            .collect(),
    });
    app.sim.open_menu().expect("the server stonecutter opens")
}

fn point_at_stonecutter_index(
    menu: &lodestone_game::menu::Menu,
    index: i32,
    start: i32,
) -> (f32, f32) {
    let width = 1280;
    let height = 720;
    let layout = crate::container::slot_layout(menu);
    let (panel_x, panel_y) = crate::container::panel_origin_with_scale(
        &layout,
        crate::config::AUTO_GUI_SCALE,
        width,
        height,
    );
    let scale = crate::config::calculate_gui_scale(
        crate::config::AUTO_GUI_SCALE,
        width,
        height,
    )
    .max(1) as f32;
    let rect = crate::container::stonecutter::grid_rect(index, start)
        .expect("the requested result is visible");
    ((panel_x + rect.x + 1.0) * scale, (panel_y + rect.y + 1.0) * scale)
}

#[test]
fn stonecutter_scroll_and_click_use_the_visible_server_index_when_local_recipes_are_empty() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions) = NetClient::loopback();
    app.sim.attach_net(net);
    app.recipe_book = Some(lodestone_game::recipe::RecipeBook::new());
    let open = open_test_stonecutter(&mut app, 16);
    app.ui.enter_dev_world();
    app.ui.open_container();

    assert!(
        app.scroll_stonecutter(-1.0),
        "the server's offscreen row consumes the wheel"
    );
    assert_eq!(app.stonecutter_scroll, 1.0);
    app.cursor = point_at_stonecutter_index(&open.menu, 4, 4);
    assert!(app.handle_stonecutter_click(&open.menu, 1280, 720));
    assert_eq!(
        actions.try_recv(),
        Ok(lodestone_model::ClientAction::ContainerButtonClick {
            window_id: 17,
            button_id: 4,
        }),
        "the first cell after scrolling must retain server button id 4"
    );
}

#[test]
fn stonecutter_click_rejects_a_local_recipe_the_server_did_not_offer() {
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, actions) = NetClient::loopback();
    app.sim.attach_net(net);
    let stone = "minecraft:stone".parse().unwrap();
    let mut local = RecipeBook::new();
    local.insert(
        "minecraft:local_only_stonecutting".parse().unwrap(),
        Recipe::Stonecutting {
            ingredient: Ingredient::Item(stone),
            result: ItemStack::new("minecraft:stone_slab".parse().unwrap(), 1),
        },
    );
    app.recipe_book = Some(local);
    let open = open_test_stonecutter(&mut app, 0);
    app.cursor = point_at_stonecutter_index(&open.menu, 0, 0);

    assert!(!app.handle_stonecutter_click(&open.menu, 1280, 720));
    assert!(
        actions.try_recv().is_err(),
        "a local-only recipe must send no button click"
    );
}

/// Java's `String.hashCode()`, computed by hand from the well-known
/// public algorithm — an oracle that lives outside this file, per
/// `CLAUDE.md`'s evidence standard. `"hello"`: `h = 0`, then
/// `104, 3325, 103183, 3198781, 99162322` after `'h','e','l','l','o'`
/// (`h = h*31 + c` each step) — a commonly-cited constant, reproduced
/// here from the formula rather than trusted from memory alone.
#[test]
fn java_string_hash_code_matches_the_known_constant() {
    assert_eq!(java_string_hash_code("hello"), 99_162_322);
    assert_eq!(java_string_hash_code(""), 0);
}
