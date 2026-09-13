use super::*


/// The loading screen's progress bar reaches geometry, at vanilla's
/// `LevelLoadingScreen` rect, and its **green fill tracks the real fraction**.
///
/// The fill width is the assertion, not the bar's presence: a bar that drew at a
/// fixed width would pass a presence check while telling the player nothing. The
/// expected width is derived from `PROGRESS_BAR_W` and the fraction the frame was
/// built from, the same two terms the draw multiplies.
#[test]
fn the_loading_bar_fill_tracks_the_real_column_count() {
    use crate::menu::loading::TerrainProgress;

    let quarter = TerrainProgress { loaded: 100, expected: 400 };
    let f = loading_frame_with_progress("Loading terrain...", quarter);
    let v = geometry(&f, V_W, V_H);

    let track = colour_bounds(&v, V_W, V_H, PROGRESS_BAR_BG)
        .expect("the black track must reach the colour stream");
    let fill = colour_bounds(&v, V_W, V_H, PROGRESS_BAR_FG)
        .expect("the green fill must reach the colour stream");

    // The track is vanilla's 200x2, horizontally centred.
    assert!(
        (track.2 - PROGRESS_BAR_W).abs() < 0.5,
        "track width {} should be {PROGRESS_BAR_W}",
        track.2
    );
    assert!(
        (track.3 - PROGRESS_BAR_H).abs() < 0.5,
        "track height {} should be {PROGRESS_BAR_H}",
        track.3
    );
    assert!(
        (track.0 + track.2 * 0.5 - V_W * 0.5).abs() < 1.0,
        "track should be centred, got x0={} w={}",
        track.0,
        track.2
    );

    // A quarter-loaded view fills a quarter of the bar, from the same left edge.
    let want = (quarter.fraction() * PROGRESS_BAR_W).round();
    assert!(
        (fill.2 - want).abs() < 0.5,
        "fill width {} should be {want} for {}/{}",
        fill.2,
        quarter.loaded,
        quarter.expected
    );
    assert!((fill.0 - track.0).abs() < 0.5, "fill must start at the track's left edge");

    // The negative control that matters here: an empty view must draw the track
    // and *no* fill at all, so a full-width fill cannot masquerade as progress.
    let empty = TerrainProgress { loaded: 0, expected: 400 };
    let v0 = geometry(&loading_frame_with_progress("Loading terrain...", empty), V_W, V_H);
    assert!(
        colour_bounds(&v0, V_W, V_H, PROGRESS_BAR_BG).is_some(),
        "the track still draws at zero"
    );
    assert!(
        colour_bounds(&v0, V_W, V_H, PROGRESS_BAR_FG).is_none(),
        "a zero-column view must draw no green fill"
    );
}
/// The loading screen's chunk-status grid reaches geometry as
/// **real per-cell colour**, not a uniform block that merely proves the grid
/// drew *something*.
///
/// One cell is `Full` and the other eight of a 3x3 (`radius = 1`) grid are
/// `Empty`. `colour_bounds` finds each vanilla status colour's own quad
/// bounds by colour, not by a vertex-inside-a-fixed-rect probe (`CLAUDE.md`'s
/// warning about the coverage helper enclosing-quad trap does not apply
/// here: each cell is 2x2 px, far smaller than any probe rect could be, and
/// this asks where the colour landed rather than whether a fixed rect is
/// covered). The `Full` corner's box is predicted **exactly**, from the same
/// `chunk_cell_origin` expression `draw::build` calls — a restated constant
/// could drift from the draw and still pass, an evaluated call to the same
/// function cannot. The `Empty` colour's box is the rest of the grid: the two
/// boxes are at different positions, which is the discriminating property —
/// a solid-colour grid (the negative control below) cannot produce this.
#[test]
fn the_chunk_grid_draws_two_real_statuses_at_two_different_cells() {
    use crate::menu::loading::{ChunkCellStatus, TerrainChunkGrid, TerrainProgress};

    let radius = 1u32;
    let diameter = TerrainChunkGrid::diameter(radius);
    assert_eq!(diameter, 3, "premise: a 3x3 grid, so 'one corner' is unambiguous");

    let mut cells = vec![ChunkCellStatus::Empty; diameter * diameter];
    cells[0] = ChunkCellStatus::Full; // (x=0, z=0), row-major x-fastest
    let grid = TerrainChunkGrid {
        radius,
        center: (0, 0),
        cells,
    };

    let progress = TerrainProgress { loaded: 1, expected: 9 };
    let frame = loading_frame_with_progress_and_grid("Loading terrain...", progress, Some(grid));
    let v = geometry(&frame, V_W, V_H);

    // The exact prediction: the same expression `draw::build` evaluates.
    let dy = chunk_grid_dy(radius);
    let center_x = V_W * 0.5;
    let center_y = (V_H * 0.5 + dy).floor();
    let (want_x, want_y) = chunk_cell_origin(center_x, center_y, diameter, 0, 0);

    // Vanilla's `FULL` colour is opaque white, the same colour every plain
    // label draws with under the jar-less debug font — see
    // `colour_bounds_in_band`'s doc. Restricting to the grid's own predicted
    // vertical band (well above the phase label, by `CHUNK_GRID_GAP`) is what
    // lets this ask about the grid specifically rather than about "white
    // anywhere on the frame".
    let total = diameter as f32 * CHUNK_CELL_SIZE;
    let band = (center_y - total * 0.5 - 0.5, center_y + total * 0.5 + 0.5);
    let full = colour_bounds_in_band(&v, V_W, V_H, CHUNK_CELL_FULL, band)
        .expect("the one Full cell must reach the colour stream");
    let empty = colour_bounds_in_band(&v, V_W, V_H, CHUNK_CELL_EMPTY, band)
        .expect("the eight Empty cells must reach the colour stream");

    assert!(
        (full.0 - want_x).abs() < 0.5 && (full.1 - want_y).abs() < 0.5,
        "Full cell at {:?}, expected top-left ({want_x}, {want_y})",
        full
    );
    assert!(
        (full.2 - CHUNK_CELL_SIZE).abs() < 0.5 && (full.3 - CHUNK_CELL_SIZE).abs() < 0.5,
        "Full cell should be exactly one {CHUNK_CELL_SIZE}x{CHUNK_CELL_SIZE} px cell, got {:?}",
        full
    );

    // The two colours must occupy different **extents** — the discriminator
    // this gate exists to check. Their bounding boxes can share a corner (the
    // excluded cell sits at the grid's own corner, so its neighbours' boxes
    // touch the same point) without sharing a *size*: a grid that drew one
    // colour everywhere could not produce one cell-sized box and one
    // grid-sized box at once.
    assert!(
        (full.2 - empty.2).abs() > 0.5 || (full.3 - empty.3).abs() > 0.5,
        "Full at {full:?} and Empty at {empty:?} must not have the same extent"
    );
    // The Empty colour spans the rest of the 3x3 block: every row and column
    // except the excluded corner still has an Empty cell reaching every edge,
    // so its bounds equal the whole grid's — strictly larger than one cell.
    let total = diameter as f32 * CHUNK_CELL_SIZE;
    assert!(
        (empty.2 - total).abs() < 0.5 && (empty.3 - total).abs() < 0.5,
        "Empty should span the full {total}x{total} px grid minus one corner, got {:?}",
        empty
    );

    // The negative control: a grid with only one status present must draw
    // only that status's colour — proving the split above is conditional on
    // real per-cell data, not a fixed two-colour pattern.
    let uniform = TerrainChunkGrid {
        radius,
        center: (0, 0),
        cells: vec![ChunkCellStatus::Full; diameter * diameter],
    };
    let uniform_frame =
        loading_frame_with_progress_and_grid("Loading terrain...", progress, Some(uniform));
    let uv = geometry(&uniform_frame, V_W, V_H);
    assert!(
        colour_bounds(&uv, V_W, V_H, CHUNK_CELL_FULL).is_some(),
        "an all-loaded grid must still draw the Full colour"
    );
    assert!(
        colour_bounds(&uv, V_W, V_H, CHUNK_CELL_EMPTY).is_none(),
        "an all-loaded grid must draw no Empty cell at all"
    );

    // And the frame-level negative control: no grid at all draws no Empty
    // cell — `Full`'s own colour is skipped here because it coincides with
    // vanilla's plain white label text (see `colour_bounds_in_band`'s doc),
    // which the phase label draws with or without a grid, so it carries no
    // information about the grid either way. `Empty`'s grey has no such
    // coincidence in this frame, so its absence is the real control.
    let none_frame = loading_frame_with_progress("Loading terrain...", progress);
    let nv = geometry(&none_frame, V_W, V_H);
    assert!(
        colour_bounds(&nv, V_W, V_H, CHUNK_CELL_EMPTY).is_none(),
        "no grid at all must draw no Empty cell"
    );
}

/// The grid must fit the smallest canvas at the largest selectable render
/// distance. The layout keeps the square centred and moves the label/bar above
/// it, so a 32-radius selection should show the complete 65x65 cell square
/// rather than silently cropping it to the middle.
#[test]
fn the_chunk_grid_fits_the_smallest_canvas_at_every_render_distance() {
    use crate::menu::loading::{MAX_GRID_RADIUS, TerrainChunkGrid};

    /// `config::MIN_SCALED_HEIGHT` — the shortest canvas that can be presented.
    const FLOOR_H: f32 = 240.0;

    /// The top edge of the grid a session streaming `view_radius` draws, in
    /// canvas coordinates on a `height`-tall canvas.
    fn grid_top(view_radius: u32, height: f32) -> f32 {
        let radius = TerrainChunkGrid::view_radius(view_radius);
        let side = TerrainChunkGrid::diameter(radius) as f32 * CHUNK_CELL_SIZE;
        (height * 0.5 + chunk_grid_dy(radius)).floor() - side * 0.5
    }

    fn grid_bottom(view_radius: u32, height: f32) -> f32 {
        let radius = TerrainChunkGrid::view_radius(view_radius);
        let side = TerrainChunkGrid::diameter(radius) as f32 * CHUNK_CELL_SIZE;
        grid_top(view_radius, height) + side
    }

    // The selected maximum, a larger request (which must be clamped), and the
    // ordinary default all need to stay inside the 240-pixel canvas floor.
    let mut bad: Vec<String> = Vec::new();
    for view_radius in [8u32, 32, 64, 1024] {
        let top = grid_top(view_radius, FLOOR_H);
        if top < 0.0 {
            bad.push(format!(
                "view_radius {view_radius}: top edge {top} on the {FLOOR_H}px canvas floor \
                 — off the top of the screen"
            ));
        }
        if grid_bottom(view_radius, FLOOR_H) > FLOOR_H {
            bad.push(format!(
                "view_radius {view_radius}: bottom edge {} is below the {FLOOR_H}px canvas",
                grid_bottom(view_radius, FLOOR_H)
            ));
        }
    }
    assert!(bad.is_empty(), "the grid does not fit the canvas floor: {bad:#?}");

    // The player's selected distance is preserved up to the supported maximum;
    // a server/fixture asking for more cannot make the square grow off-screen.
    assert_eq!(TerrainChunkGrid::view_radius(8), 8, "a small view is drawn whole");
    assert_eq!(TerrainChunkGrid::view_radius(32), 32, "the selected maximum is drawn whole");
    assert_eq!(TerrainChunkGrid::view_radius(64), MAX_GRID_RADIUS);
}

/// Every generation status has its own palette entry. A single grey/white
/// fallback would make the grid look like a scalar counter even though each
/// cell carries a typed status.
#[test]
fn the_chunk_status_palette_keeps_all_twelve_colours_distinct() {
    use crate::menu::loading::ChunkCellStatus;

    let statuses = [
        ChunkCellStatus::Empty,
        ChunkCellStatus::StructureStarts,
        ChunkCellStatus::StructureReferences,
        ChunkCellStatus::Biomes,
        ChunkCellStatus::Noise,
        ChunkCellStatus::Surface,
        ChunkCellStatus::Carvers,
        ChunkCellStatus::Features,
        ChunkCellStatus::InitializeLight,
        ChunkCellStatus::Light,
        ChunkCellStatus::Spawn,
        ChunkCellStatus::Full,
    ];
    let colours = statuses.map(chunk_cell_colour);
    for (index, colour) in colours.iter().enumerate() {
        assert!(
            colours[index + 1..].iter().all(|other| other != colour),
            "status palette entry {index} must not collapse into a neighbouring status"
        );
    }
}
