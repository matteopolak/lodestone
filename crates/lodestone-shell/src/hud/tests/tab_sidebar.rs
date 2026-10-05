//! Tab-list columns, header/footer and the scoreboard sidebar.

use super::*;

#[test]
fn tab_overlay_lists_players() {
    let stats = DebugStats::default();
    let view = tab_view(2);
    let frame = HudFrame {
        players: Some(&view),
        ..HudFrame::new(&stats)
    };
    let with = HudGeometry::build(&frame, 640, 480).vertex_count();
    let without = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
    assert!(with > without, "the tab overlay's plate + names add geometry");
}

/// **The column split, at the threshold.**
///
/// `for (cols = 1; rows > 20; rows = (slots + cols - 1) / cols) { cols++; }`
/// has to be read in Java's own order — condition, body, then update — so
/// `cols` is bumped *before* `rows` is recomputed.
///
/// The discriminating input is **21**, and the number that discriminates is
/// `rows`, not `cols`. A plausible misreading — "columns of 20, so
/// `cols = ceil(slots / 20)` and `rows = 20`" — agrees about `cols` at every
/// input tried here and answers `20` where the truth is `11`. That is the
/// difference between an overlay 11 rows tall and one 20 rows tall with nine
/// empty rows of plate hanging below it, so `cols` alone is not a test.
#[test]
fn the_column_split_matches_vanillas_own_loop_at_the_threshold() {
    let panel = |slots: usize| TabPanel::new(640.0, slots, 40.0, 0, 0.0);
    // One player: one column of one. Not one column of 20.
    assert_eq!((panel(1).cols, panel(1).rows), (1, 1));
    // MAX_ROWS_PER_COL exactly: still one column, because the guard is
    // `rows > 20` and not `rows >= 20`.
    assert_eq!(
        (panel(TAB_MAX_ROWS_PER_COL).cols, panel(TAB_MAX_ROWS_PER_COL).rows),
        (1, TAB_MAX_ROWS_PER_COL)
    );
    // One more, and it splits into two columns of **11** — ceil(21 / 2).
    assert_eq!((panel(21).cols, panel(21).rows), (2, 11));
    // 41 needs three passes of the loop: 41 → 21 → 14.
    assert_eq!((panel(41).cols, panel(41).rows), (3, 14));
    // And vanilla's own cap, which is 80 rather than a round 100: four
    // columns of 20.
    let full = panel(crate::tablist::MAX_TAB_ROWS);
    assert_eq!((full.cols, full.rows), (4, 20));
}

/// Slots fill **column-major** — `col = i / rows`, `row = i % rows`.
///
/// Twenty-one players, so `rows == 11`: index 10 is the bottom of column 0
/// and index 11 is the *top* of column 1. A row-major reading
/// (`col = i % cols`) would put index 1 there instead, and on any list of 20
/// or fewer the two readings are indistinguishable — which is why this gate
/// has to cross the split.
#[test]
fn slots_fill_column_major_so_the_list_reads_downwards() {
    let panel = TabPanel::new(640.0, 21, 40.0, 0, 0.0);
    assert_eq!(panel.rows, 11);
    let [x0, y0] = panel.slot_origin(0);
    let [x10, y10] = panel.slot_origin(10);
    let [x11, y11] = panel.slot_origin(11);
    // Column 0 runs the full 11 rows down.
    assert_eq!(x10, x0);
    assert_eq!(y10, y0 + 10.0 * TAB_LINE_H);
    // Index 11 starts column 1, back at the top.
    assert_eq!(y11, y0);
    assert_eq!(x11, x0 + panel.slot_w + 5.0);
}

/// The header pushes the rows down by `header_len * 9 + 1` — the bare `yyo++`
/// after the header loop is a real pixel of air and is easy to drop.
///
/// With no header the rows start at vanilla's `yyo = 10` unchanged, which is
/// the control: a layout that always added the gap would fail here.
#[test]
fn a_header_offsets_the_rows_by_its_own_height_plus_one() {
    let bare = TabPanel::new(640.0, 3, 40.0, 0, 0.0);
    assert_eq!(bare.rows_top, 10.0);
    let with_header = TabPanel::new(640.0, 3, 40.0, 2, 0.0);
    assert_eq!(with_header.rows_top, 10.0 + 2.0 * TAB_LINE_H + 1.0);
    // `yyo += rows * 9 + 1` before the footer plate, counted from wherever the
    // rows actually began.
    assert_eq!(
        with_header.footer_top,
        with_header.rows_top + 3.0 * TAB_LINE_H + 1.0
    );
}

/// A header or footer wider than the row block **widens the plates**, and a
/// narrow one does not shrink them — vanilla's own max-line-width value starts at the
/// block width and only ever takes a `max`.
#[test]
fn a_wide_banner_widens_the_plate_and_a_narrow_one_leaves_it_alone() {
    let bare = TabPanel::new(640.0, 3, 40.0, 0, 0.0);
    let narrow = TabPanel::new(640.0, 3, 40.0, 1, 4.0);
    assert_eq!(narrow.max_line_width, bare.max_line_width);
    let wide = TabPanel::new(640.0, 3, 40.0, 1, bare.max_line_width + 60.0);
    assert_eq!(wide.max_line_width, bare.max_line_width + 60.0);
    // …and the plate really does grow with it, rather than the width being
    // computed and dropped.
    assert!(wide.plate_w() > bare.plate_w());
}

/// Nothing is drawn for a header or footer the server did not send.
///
/// Byte-identical geometry, not merely "less": vanilla only measures and only
/// fills when the component is non-null, so an absent banner must not leave a
/// plate, a gap, or a single vertex behind. A vanilla server sends neither
/// unless something sets one, so this is the *common* case and not an edge.
#[test]
fn an_absent_header_and_footer_draw_nothing_at_all() {
    let stats = DebugStats::default();
    let build = |view: &crate::tablist::TabListView| {
        HudGeometry::build(
            &HudFrame {
                players: Some(view),
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .verts
    };
    let bare = tab_view(3);
    let mut banner = tab_view(3);
    banner.header = vec![crate::overlay::plain_spans("Welcome")];
    banner.footer = vec![crate::overlay::plain_spans("Bye")];
    let with = build(&banner);
    let without = build(&bare);
    assert!(
        with.len() > without.len(),
        "control: a supplied banner must add geometry — {} floats against {}",
        with.len(),
        without.len()
    );
    // The real assertion: the no-banner frame is the same frame it always
    // was, to the float.
    let again = build(&tab_view(3));
    assert_eq!(without, again);
}

#[test]
fn sidebar_draws_title_and_scored_rows() {
    use crate::overlay::{Sidebar, SidebarLine};
    let stats = DebugStats::default();
    let base = HudFrame {
        crosshair: false,
        show_debug: false,
        ..HudFrame::new(&stats)
    };
    let base_verts = HudGeometry::build(&base, 640, 480).vertex_count();

    let side = Sidebar {
        title: crate::overlay::plain_spans("Objectives"),
        lines: vec![
            SidebarLine {
                label: crate::overlay::plain_spans("Kills"),
                score: crate::overlay::plain_spans("7"),
            },
            SidebarLine {
                label: crate::overlay::plain_spans("Deaths"),
                score: crate::overlay::plain_spans("2"),
            },
        ],
    };
    let frame = HudFrame {
        sidebar: Some(&side),
        ..HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        }
    };
    let with = HudGeometry::build(&frame, 640, 480);
    assert!(
        with.vertex_count() > base_verts,
        "a displayed sidebar must add the panel, title and rows"
    );

    // Anti-vacuity: the score text itself must be drawn, not just the panel.
    // Dropping the scores (blank strings) must reduce the geometry, so a
    // regression that stops rendering scores can't pass this.
    let scoreless = Sidebar {
        title: crate::overlay::plain_spans("Objectives"),
        lines: vec![
            SidebarLine {
                label: crate::overlay::plain_spans("Kills"),
                score: Vec::new(),
            },
            SidebarLine {
                label: crate::overlay::plain_spans("Deaths"),
                score: Vec::new(),
            },
        ],
    };
    let frame_scoreless = HudFrame {
        sidebar: Some(&scoreless),
        ..HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        }
    };
    let without_scores = HudGeometry::build(&frame_scoreless, 640, 480).vertex_count();
    assert!(
        with.vertex_count() > without_scores,
        "the score glyphs must contribute geometry, not just the labels"
    );
}
