//! F3 debug overlay text, geometry caching and vanilla format strings.

use super::*;

#[test]
fn debug_geometry_refresh_is_bounded_but_open_and_layout_changes_are_immediate() {
    let t0 = Instant::now();
    let stamp = DebugGeometryStamp {
        width: 3024,
        height: 1964,
        gui_scale: 0,
        font_revision: 7,
    };
    let mut refresh = DebugGeometryRefresh::default();

    assert!(refresh.should_refresh(t0, true, stamp));
    assert!(!refresh.should_refresh(
        t0 + std::time::Duration::from_millis(99),
        true,
        stamp
    ));
    assert!(refresh.should_refresh(
        t0 + std::time::Duration::from_millis(100),
        true,
        stamp
    ));
    assert!(!refresh.should_refresh(
        t0 + std::time::Duration::from_millis(101),
        false,
        stamp
    ));
    assert!(
        refresh.should_refresh(t0 + std::time::Duration::from_millis(102), true, stamp),
        "reopening F3 must not show the last hidden snapshot"
    );
    assert!(refresh.should_refresh(
        t0 + std::time::Duration::from_millis(103),
        true,
        DebugGeometryStamp { width: 1512, ..stamp }
    ));
}

#[test]
fn cached_debug_layer_matches_the_inline_geometry_exactly() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    let inline = HudGeometry::build(&frame, 1280, 720);
    let cached = build_debug_vertices(
        &frame,
        1280,
        720,
        crate::config::AUTO_GUI_SCALE,
        None,
    );
    assert_eq!(cached, inline.verts);
}

#[test]
fn rss_is_observable() {
    // The memory gauge must read a real, non-zero RSS on the host running
    // the tests. A zero here is exactly the broken-gauge regression the fix
    // addressed: a HUD field that reads 0 gets believed. A live process
    // always has a resident set, so >1 MiB is a safe, non-vacuous floor.
    let rss = process_rss_bytes();
    assert!(
        rss > 1 << 20,
        "process RSS should be observable (>1 MiB), got {rss} bytes — memory gauge is broken"
    );
}

#[test]
fn facing_from_yaw() {
    let mut s = DebugStats {
        yaw: 0.0,
        ..Default::default()
    };
    assert_eq!(s.facing(), "south (+Z)");
    s.yaw = 90.0;
    assert_eq!(s.facing(), "west (-X)");
    s.yaw = 180.0;
    assert_eq!(s.facing(), "north (-Z)");
    s.yaw = 270.0;
    assert_eq!(s.facing(), "east (+X)");
    s.yaw = -90.0;
    assert_eq!(s.facing(), "east (+X)");
}

/// `ServerDifficulty` reaches a real, tested ECS fold (`lodestone-client`'s
/// `apply_routes_difficulty_changed_through_the_real_path`) but the F3
/// overlay once drew nothing for it. This pins the exact text so a
/// regression back to "no line at all" or a swapped lock state is visible
/// in a diff, not just "some line changed somewhere".
///
/// **Re-derived** when the overlay was reformatted against vanilla's own
/// strings: the prefix was `DIFFICULTY` and the names were shouted, and both
/// are now vanilla's lowercase serialized keys (`Difficulty`'s
/// `PEACEFUL(0, "peaceful")` …). Every assertion below changed for that
/// reason and for no other — the *shape* (a line always present, `-` before
/// the first report, a lock suffix, all four names) is unchanged.
#[test]
fn debug_overlay_shows_difficulty_and_lock_state() {
    // Found by content, not position: `lines()` is a growing list of
    // independent facts, so pinning an index here would make this test
    // brittle to an unrelated line being added or reordered, which is
    // exactly the kind of accidental coupling `CLAUDE.md` warns a gate
    // should not have.
    fn difficulty_line(stats: &DebugStats) -> String {
        stats
            .lines()
            .into_iter()
            .find(|l| l.starts_with("Difficulty:"))
            .expect("the F3 overlay must always carry a Difficulty line")
    }

    let no_report = DebugStats::default();
    assert_eq!(
        difficulty_line(&no_report),
        "Difficulty: -",
        "before the server's first report, the line must say so plainly rather \
         than defaulting to a difficulty the server never sent"
    );

    let unlocked = DebugStats {
        difficulty: Some((lodestone_model::Difficulty::Easy, false)),
        ..Default::default()
    };
    assert_eq!(difficulty_line(&unlocked), "Difficulty: easy");

    let locked = DebugStats {
        difficulty: Some((lodestone_model::Difficulty::Hard, true)),
        ..Default::default()
    };
    assert_eq!(difficulty_line(&locked), "Difficulty: hard (locked)");

    // Every variant name, so a mis-mapped match arm (e.g. Peaceful reading
    // as Easy) cannot hide behind only testing one value.
    for (d, name) in [
        (lodestone_model::Difficulty::Peaceful, "peaceful"),
        (lodestone_model::Difficulty::Easy, "easy"),
        (lodestone_model::Difficulty::Normal, "normal"),
        (lodestone_model::Difficulty::Hard, "hard"),
    ] {
        let stats = DebugStats {
            difficulty: Some((d, false)),
            ..Default::default()
        };
        assert_eq!(difficulty_line(&stats), format!("Difficulty: {name}"));
    }
}

/// The server's simulation distance has a precise visible destination on
/// the F3 entity line. A missing report must not be fabricated from the
/// render distance, while a reported value must survive alongside a
/// pairwise-distinct entity count.
#[test]
fn debug_overlay_shows_only_reported_simulation_distance() {
    fn entity_line(stats: &DebugStats) -> String {
        stats
            .right_lines()
            .into_iter()
            .find(|line| line.starts_with("E:"))
            .expect("the F3 overlay must carry an entity line")
    }

    let absent = DebugStats {
        entities_drawn: 3,
        ..Default::default()
    };
    assert_eq!(
        entity_line(&absent),
        "E: 3",
        "control: no server report must not invent an SD value"
    );

    let reported = DebugStats {
        entities_drawn: 3,
        simulation_distance: Some(11),
        ..Default::default()
    };
    assert_eq!(entity_line(&reported), "E: 3, SD: 11");
}

/// A server-data packet has an actual HUD destination. The absent case is
/// separate from an empty string: F3 must not claim a server announced a
/// message before that packet exists.
#[test]
fn debug_overlay_shows_only_reported_server_motd() {
    fn motd_line(stats: &DebugStats) -> Option<String> {
        stats
            .right_lines()
            .into_iter()
            .find(|line| line.starts_with("MOTD:"))
    }

    assert_eq!(
        motd_line(&DebugStats::default()),
        None,
        "control: no server-data packet must draw no MOTD line"
    );
    assert_eq!(
        motd_line(&DebugStats {
            server_motd: Some("Copper Canyon".to_owned()),
            ..Default::default()
        }),
        Some("MOTD: Copper Canyon".to_owned())
    );
}

/// Combat state has a visible F3 consumer. The absence control matters: an
/// idle-looking session is not evidence that the server sent an end packet.
#[test]
fn debug_overlay_shows_only_reported_combat_session() {
    fn combat_line(stats: &DebugStats) -> Option<String> {
        stats
            .right_lines()
            .into_iter()
            .find(|line| line.starts_with("Combat:"))
    }

    assert_eq!(combat_line(&DebugStats::default()), None);
    assert_eq!(
        combat_line(&DebugStats {
            combat_session: Some(lodestone_ecs::CombatSession::Active),
            ..Default::default()
        }),
        Some("Combat: active".to_owned())
    );
    assert_eq!(
        combat_line(&DebugStats {
            combat_session: Some(lodestone_ecs::CombatSession::Ended {
                duration_ticks: 240,
            }),
            ..Default::default()
        }),
        Some("Combat: ended (240 ticks)".to_owned())
    );
}

/// `RenderState::weather_columns`/`weather_rain_columns` reach the F3
/// overlay. Pairwise-distinct values (not e.g. `4, 4`), so a transposed
/// assignment at the `app::redraw` call site cannot survive this test.
#[test]
fn debug_overlay_shows_weather_columns() {
    fn weather_line(stats: &DebugStats) -> String {
        stats
            .lines()
            .into_iter()
            .find(|l| l.starts_with("Weather cols:"))
            .expect("the F3 overlay must always carry a Weather cols line")
    }

    assert_eq!(
        weather_line(&DebugStats::default()),
        "Weather cols: 0, rain: 0",
        "clear weather (or no pass installed) must read as zero, not absent"
    );

    let stats = DebugStats {
        weather_columns: 11,
        weather_rain_columns: 4,
        ..Default::default()
    };
    assert_eq!(weather_line(&stats), "Weather cols: 11, rain: 4");
}

/// The F3 overlay's plate, ink and pitch, against the literals in
/// The debug screen overlay.
///
/// # Where the expected values come from
///
/// `extractLines` is four numbers: `int height = 9`, the two margins spent as
/// `left = alignLeft ? 2 : gui_width() - 2 - width` and `top = 2 + height * i`,
/// `graphics.fill(…, -1873784752)` and `graphics.text(…, -2039584, false)`.
/// The two colours below are those **signed Java `int`s, transcribed as
/// written and unpacked here** rather than restated as four floats — a
/// channel swap or a dropped alpha then fails, which is the failure a
/// hand-copied `[0x50/255.0, …]` array cannot see because it *is* the
/// hypothesis.
#[test]
fn debug_overlay_plate_and_ink_match_vanillas_fill_literals() {
    /// The debug screen overlay's extract lines' `graphics.fill(…, -1873784752)`.
    const VANILLA_PLATE_ARGB: i32 = -1_873_784_752;
    /// Its `graphics.text(…, -2039584, false)`.
    const VANILLA_INK_ARGB: i32 = -2_039_584;

    fn unpack_argb(argb: i32) -> [f32; 4] {
        let bits = argb as u32;
        let channel = |shift: u32| ((bits >> shift) & 0xFF) as f32 / 255.0;
        [channel(16), channel(8), channel(0), channel(24)]
    }

    // Collected, not asserted in place: a colour that is wrong in three
    // channels should report three channels, not the first one.
    let mut mismatches: Vec<String> = Vec::new();
    for (label, expected, actual) in [
        ("plate", unpack_argb(VANILLA_PLATE_ARGB), DEBUG_LINE_BG),
        ("ink", unpack_argb(VANILLA_INK_ARGB), DEBUG_LINE_INK),
    ] {
        for (channel, (e, a)) in expected.iter().zip(actual.iter()).enumerate() {
            if (e - a).abs() > f32::EPSILON {
                mismatches.push(format!(
                    "{label} channel {channel}: expected {e}, got {a}"
                ));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "F3 overlay colours diverged from DebugScreenOverlay's own literals: {mismatches:?}"
    );

    // The plate must be opaque enough to read over snow and translucent
    // enough to see terrain through, which is the whole reason it is
    // `0x90` and not `0xFF` or `0x40`. Stated as the byte so a future
    // "make it darker" cannot pass by rounding.
    assert!(
        (DEBUG_LINE_BG[3] - 0x90 as f32 / 255.0).abs() < f32::EPSILON,
        "the plate alpha is vanilla's 0x90, not {}",
        DEBUG_LINE_BG[3]
    );

    assert_eq!(
        DEBUG_LINE_H, 9.0,
        "vanilla's `int height = 9` is both the line pitch and the plate height"
    );
    assert_eq!(DEBUG_MARGIN, 2.0, "MARGIN_LEFT/RIGHT/TOP are all 2");
}

/// Every ported line of the F3 overlay, character for character, against the
/// format strings in the debug entry position, the debug entry section position,
/// The debug entry light and the debug entry looking at's block state info.
///
/// # Why this position
///
/// `[-0.5, 70.25, 88.75]`. Each component is doing work, and an origin-ish
/// position would have measured nothing:
///
/// | component | what it discriminates |
/// |---|---|
/// | `x = -0.5` | **floor vs truncate.** Vanilla's own block-position accessor is `Mth.floor`, so this is block `-1` in chunk `-1`; an `as i64` cast gives block `0` in chunk `0`. `0 0 0` cannot tell those apart, and the truncating version shipped. |
/// | `x` negative | the region hint's arithmetic shift and mask (`-1 & 31 == 31`, `-1 >> 5 == -1`) and the `%02d` section-relative pad (`-1 & 15 == 15`) |
/// | fractional `y` and `z` | vanilla's asymmetric `%.3f / %.5f / %.3f` — a uniform `%.2f`, or space separators, differ visibly |
/// | `y = 70.25` | section Y is `70 >> 4 == 4`, not the block Y the `Chunk:` line used to print |
/// | `yaw = 405` | Mth's wrap degrees — prints `45.0`, not `405.0` |
/// | `the_nether`, not `overworld` | a hardcoded dimension default, which is what this line read before it was wired to `ServerDimension` |
/// | hitboxes **on**, borders **off** | the two `Debug overlays:` states are deliberately *different*. Equal booleans are the one input a transposed pair survives, and they are adjacent same-typed fields — the cheapest bug in the file |
///
/// Each expectation is paired with the value the **superseded** formatting
/// produced, and the gate fails if the two ever coincide: an input where
/// both hypotheses agree is not a test.
#[test]
fn debug_overlay_ported_lines_match_vanillas_format_strings() {
    let stats = DebugStats {
        position: [-0.5, 70.25, 88.75],
        yaw: 405.0,
        pitch: 12.34,
        light: Some((4, 11)),
        target: Some([-1, 70, 87]),
        dimension: Some("minecraft:the_nether".to_string()),
        hitboxes_shown: true,
        chunk_borders_shown: false,
        ..Default::default()
    };
    let lines = stats.lines();

    // (what it is, vanilla's format applied by hand, what the old format
    // produced for the same input). The third column is the wrong
    // hypothesis, present so the gate can prove the input separates them.
    let cases = [
        (
            "XYZ",
            "XYZ: -0.500 / 70.25000 / 88.750",
            "XYZ -0.50 70.25 88.75",
        ),
        // No old counterpart existed for `Block:`; the truncating cast is
        // the wrong hypothesis instead.
        ("Block", "Block: -1 70 88", "Block: 0 70 88"),
        (
            "Chunk",
            "Chunk: -1 4 5 [31 5 in r.-1.0.mca]",
            "CHUNK 0 70 5",
        ),
        (
            "Facing",
            "Facing: west (Towards negative X) (45.0 / 12.3)",
            "FACING west (-X) (405.0/12.3)",
        ),
        (
            "Section-relative",
            "Section-relative: 15 06 08",
            "Section-relative: -1 6 8",
        ),
        (
            "Client Light",
            "Client Light: 11 (4 sky, 11 block)",
            "LIGHT 11 (4 SKY, 11 BLOCK)",
        ),
        // The identifier half of vanilla's last `position`-group line. The
        // wrong hypothesis is the overworld default this used to be absent
        // for entirely — a line that reads a constant is the defect class,
        // not the missing line.
        (
            "minecraft:the_nether",
            "minecraft:the_nether",
            "minecraft:overworld",
        ),
        // `formatChart`'s shape, carrying the two toggles that exist here.
        // The wrong hypothesis is the *transposed* pair, which is why the
        // fixture sets the two booleans differently.
        (
            "Debug overlays",
            "Debug overlays: [F3+B] Hitboxes visible; [F3+G] Chunk borders hidden",
            "Debug overlays: [F3+B] Hitboxes hidden; [F3+G] Chunk borders visible",
        ),
        (
            "Targeted Block",
            "Targeted Block: -1, 70, 87",
            "TARGET -1 70 87",
        ),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (label, expected, superseded) in cases {
        if expected == superseded {
            failures.push(format!(
                "{label}: the chosen input cannot separate the two \
                 hypotheses — both read {expected:?}"
            ));
            continue;
        }
        if !lines.iter().any(|l| l == expected) {
            let got = lines
                .iter()
                .find(|l| {
                    l.split(&[':', ' '][..]).next() == expected.split(&[':', ' '][..]).next()
                })
                .cloned()
                .unwrap_or_else(|| "<no line with that prefix>".to_string());
            failures.push(format!("{label}: expected {expected:?}, got {got:?}"));
        }
        if lines.iter().any(|l| l == superseded) {
            failures.push(format!(
                "{label}: still drawing the superseded format {superseded:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} ported lines are wrong:\n  {}\nall lines: {:#?}",
        failures.len(),
        cases.len(),
        failures.join("\n  "),
        lines
    );

    // Before login there is no dimension, and vanilla's whole `position`
    // group is absent in that state — so the line goes rather than becoming
    // a placeholder. The assertion above is this one's control: the same
    // detector found the line present with `Some`, so a `false` here is
    // absence and not a broken search.
    let pre_login = DebugStats::default();
    assert!(
        !pre_login
            .lines()
            .iter()
            .any(|l| l.contains("minecraft:") || l == "-"),
        "with no dimension reported the overlay must draw no dimension line \
         at all, got {:#?}",
        pre_login.lines()
    );

    // And the toggle line survives the pre-login state reading `hidden` for
    // both — the default, and the only value that could hide a wire that
    // never runs. Asserted so the line's *presence* is not conditional on
    // state the way the dimension's is.
    assert!(
        pre_login.lines().iter().any(|l| l
            == "Debug overlays: [F3+B] Hitboxes hidden; [F3+G] Chunk borders hidden"),
        "the Debug overlays line is unconditional, got {:#?}",
        pre_login.lines()
    );
}

/// The column structure: vanilla's own category blocks, separated by the
/// `""` spacers `extractRenderState` inserts, and `lines()` still the exact
/// concatenation of every block.
///
/// The concatenation property is the reason `lines()` exists — it is what
/// stops a line being added to one block and silently missing from every
/// consumer of the flat list — so it is asserted directly rather than
/// assumed. The fixture carries a `frame_profile` for exactly that reason:
/// the profile block moved out of the right column, and a block that draws
/// but is not in `lines()` is the island this assertion exists to catch.
#[test]
fn debug_overlay_columns_carry_vanillas_spacers_and_concatenate() {
    let stats = DebugStats {
        status: "local world".into(),
        adapter: vec!["Apple M5".into(), "Metal".into()],
        frame_profile: vec!["setup: 0.12/0.30/0.41 ms (240/240, 0 skip)".into()],
        ..Default::default()
    };
    let left = stats.left_lines();
    let right = stats.right_lines();
    let profile = stats.profile_lines();

    let mut expected = left.clone();
    expected.extend(right.clone());
    expected.extend(profile.clone());
    assert_eq!(
        stats.lines(),
        expected,
        "`lines()` must stay the concatenation of every block, or a line \
         added to one goes missing from every flat-list consumer"
    );
    assert!(
        profile.iter().any(|l| l == "Frame profile:")
            && profile
                .iter()
                .any(|l| l.starts_with("setup: 0.12/0.30/0.41 ms")),
        "the profile block is its heading plus the producer's own lines, \
         verbatim: {profile:#?}"
    );
    assert!(
        !right.iter().any(|l| l.starts_with("setup: ")),
        "the profile lines must no longer sit in the right column: {right:#?}"
    );
    assert!(
        DebugStats::default().profile_lines().is_empty(),
        "no reading yet must draw no heading and no spacer, not a placeholder"
    );

    // Vanilla's first priority line goes left and the second goes right,
    // because `addPriorityLine` fills whichever column is shorter and both
    // start empty. So the fps line heads the left column and the version
    // line heads the right one — not the other way round.
    assert!(
        left[0].ends_with("ms work)") && left[0].contains(" fps "),
        "the fps line must head the left column, got {:?}",
        left[0]
    );
    assert!(
        right[0].starts_with("Lodestone "),
        "the version line must head the right column, got {:?}",
        right[0]
    );

    // A spacer between category blocks, in both columns — the visible
    // difference between vanilla's grouped layout and one dense stack. A
    // count with a verdict on the count, not an eyeball.
    for (name, column) in [("left", &left), ("right", &right)] {
        let spacers = column.iter().filter(|l| l.is_empty()).count();
        assert!(
            spacers >= 2,
            "the {name} column needs at least two group spacers, found {spacers} in {column:#?}"
        );
        assert!(
            !column.last().expect("a non-empty column").is_empty(),
            "a trailing spacer draws nothing and only pads `lines()` — the \
             {name} column must not end with one"
        );
    }

    // The adapter block is the `system` group: a spacer, then the lines.
    let adapter_start = right
        .iter()
        .position(|l| l == "Apple M5")
        .expect("the adapter lines must reach the right column");
    assert_eq!(
        right[adapter_start - 1], "",
        "the adapter block must open with a spacer so it reads as its own group"
    );
}

#[test]
fn completed_ping_reply_reaches_the_debug_text() {
    let stats = DebugStats {
        ping_rtt_ms: Some(37),
        ..Default::default()
    };
    assert!(
        stats.left_lines().iter().any(|line| line == "Ping: 37 ms"),
        "the response measurement must reach a visible F3 line"
    );
    assert!(
        DebugStats::default()
            .left_lines()
            .iter()
            .any(|line| line == "Ping: -"),
        "the line must distinguish no completed reply from a zero-millisecond reply"
    );
}

#[test]
fn hiding_the_debug_overlay_removes_its_geometry() {
    let stats = DebugStats {
        status: "local world".into(),
        ..Default::default()
    };
    let mut frame = HudFrame::new(&stats);
    let with = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.show_debug = false;
    let without = HudGeometry::build(&frame, 640, 480).vertex_count();
    // Only the crosshair (2 quads = 12 verts) should remain.
    assert!(without < with, "F3 off must drop the overlay glyphs");
    assert_eq!(without, 12, "just the crosshair survives");
}
