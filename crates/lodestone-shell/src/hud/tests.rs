use super::*;
use lodestone_assets::ResourceLocation;
use super::tab_panel::TAB_MAX_ROWS_PER_COL;

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
/// `DebugScreenOverlay`.
///
/// # Where the expected values come from
///
/// `extractLines` is four numbers: `int height = 9`, the two margins spent as
/// `left = alignLeft ? 2 : guiWidth() - 2 - width` and `top = 2 + height * i`,
/// `graphics.fill(…, -1873784752)` and `graphics.text(…, -2039584, false)`.
/// The two colours below are those **signed Java `int`s, transcribed as
/// written and unpacked here** rather than restated as four floats — a
/// channel swap or a dropped alpha then fails, which is the failure a
/// hand-copied `[0x50/255.0, …]` array cannot see because it *is* the
/// hypothesis.
#[test]
fn debug_overlay_plate_and_ink_match_vanillas_fill_literals() {
    /// `DebugScreenOverlay.extractLines`' `graphics.fill(…, -1873784752)`.
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
/// format strings in `DebugEntryPosition`, `DebugEntrySectionPosition`,
/// `DebugEntryLight` and `DebugEntryLookingAt.BlockStateInfo`.
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
/// | `yaw = 405` | `Mth.wrapDegrees` — prints `45.0`, not `405.0` |
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
fn geometry_has_crosshair_and_text() {
    let stats = DebugStats {
        position: [1.0, 64.0, 2.0],
        status: "local world".into(),
        ..Default::default()
    };
    let geo = HudGeometry::build(&HudFrame::new(&stats), 320, 240);
    // Crosshair alone is 2 quads = 12 verts; text adds far more.
    assert!(geo.vertex_count() > 100, "expected glyphs + crosshair");
    assert_eq!(geo.verts.len() % FLOATS_PER_VERTEX, 0);
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
fn empty_string_advances_without_panicking() {
    let stats = DebugStats::default();
    let _ = HudGeometry::build(&HudFrame::new(&stats), 1, 1);
}

#[test]
fn hotbar_items_draw_count_on_colour_stream_without_atlas() {
    // With `hotbar_items` populated but no item atlas attached, the flat
    // icons cannot draw (item_verts stays empty), but the stack-count number
    // still renders to the colour stream and nothing panics.
    let stats = DebugStats::default();
    let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();

    let slots = [
        Some(HotbarSlot {
            item: ResourceLocation::parse("minecraft:stone").unwrap(),
            count: 64,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    let mut frame = HudFrame::new(&stats);
    frame.hotbar = Some(0);
    frame.hotbar_items = Some(&slots);
    let geo = HudGeometry::build(&frame, 640, 480);

    assert!(
        geo.item_verts.is_empty(),
        "no item atlas attached, so no item-sprite geometry"
    );
    assert!(
        geo.vertex_count() > base,
        "the '64' stack count must add colour-stream verts"
    );
}

#[test]
fn item_cooldown_veil_draws_only_for_an_occupied_positive_fraction_slot() {
    let stats = DebugStats::default();
    let slots = [
        Some(HotbarSlot {
            item: ResourceLocation::parse("minecraft:ender_pearl").unwrap(),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    let mut frame = HudFrame::new(&stats);
    frame.hotbar = Some(0);
    frame.hotbar_items = Some(&slots);

    let zero_cooldown = [0.0];
    let half_cooldown = [0.5];
    let full_cooldown = [1.0];
    let no_cooldowns: [f32; 0] = [];
    frame.hotbar_cooldowns = &zero_cooldown;
    let zero = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &half_cooldown;
    let half = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &full_cooldown;
    let full = HudGeometry::build(&frame, 640, 480).vertex_count();

    assert_eq!(half, zero + 6, "one positive cooldown must add one veil quad");
    assert_eq!(full, half, "fraction changes veil height, not its quad count");

    // Negative control: the same positive fraction must not paint an empty
    // slot merely because the parallel cooldown slice names its index.
    let empty_slots: [Option<HotbarSlot>; 9] = Default::default();
    frame.hotbar_items = Some(&empty_slots);
    let empty = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &no_cooldowns;
    let empty_control = HudGeometry::build(&frame, 640, 480).vertex_count();
    assert_eq!(empty, empty_control, "an empty slot must not receive a cooldown veil");
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

/// Decodes a colour-stream vertex buffer's NDC positions back to the pixel
/// space `ColourStream::rect` built them from, returning `(min_x, max_x,
/// min_y, max_y)`. The exact inverse of `to_ndc` in
/// `hud/item_icon.rs`'s `ColourStream::rect`.
fn ndc_bounds(verts: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for v in verts.chunks_exact(FLOATS_PER_VERTEX) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        min_x = min_x.min(px);
        max_x = max_x.max(px);
        min_y = min_y.min(py);
        max_y = max_y.max(py);
    }
    (min_x, max_x, min_y, max_y)
}

/// Pins the crosshair to vanilla's real ink, not its sprite's bounding box —
/// the draw site's own doc has the pixel-by-pixel read of
/// `hud/crosshair.png`. Two hypotheses, computed from outside constants
/// rather than guessed: vanilla's real 9px-long, 1px-thick "+" (correct), and
/// this draw's own pre-fix 16px/2px-thick bar (the bug this test would have
/// caught). At `gui_scale == 1` — the floor `320x240` this repo already uses
/// elsewhere for that reason — physical and logical pixels coincide, so the
/// measured span is the real on-screen footprint, not a scaled derivative.
#[test]
fn crosshair_span_matches_vanillas_real_ink_not_the_old_wrong_hypothesis() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.show_debug = false;
    let (w, h) = (320.0_f32, 240.0_f32);
    let geo = HudGeometry::build(&frame, w as u32, h as u32);
    assert_eq!(geo.vertex_count(), 12, "precondition: only the crosshair draws");

    let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
    let (span_x, span_y) = (max_x - min_x, max_y - min_y);
    let (correct, wrong) = (9.0_f32, 16.0_f32);
    let mut mismatches = Vec::new();
    if (span_x - correct).abs() >= 0.01 {
        mismatches.push(format!("horizontal span {span_x} (want {correct})"));
    }
    if (span_y - correct).abs() >= 0.01 {
        mismatches.push(format!("vertical span {span_y} (want {correct})"));
    }
    assert!(
        mismatches.is_empty(),
        "crosshair does not match vanilla's real ink: {mismatches:?} — \
         note the wrong hypothesis this used to draw was {wrong}px"
    );
    // The wrong hypothesis is a real, distinct number — if `correct` and
    // `wrong` ever coincided this assertion would be measuring nothing.
    assert!((correct - wrong).abs() > 1.0);

    // Still centred on the canvas — only the size should have changed.
    let (cx, cy) = ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
    assert!((cx - w * 0.5).abs() < 0.01, "crosshair should stay x-centred");
    assert!((cy - h * 0.5).abs() < 0.01, "crosshair should stay y-centred");
}

/// The same shape one GUI-scale step up: vanilla's own scale-calculation picks 3
/// for a 1280x720 framebuffer at its automatic GUI-scale setting (height-bound:
/// `720/(3+1) < 240`, `720/(3+0) >= 240` at the previous step — see
/// `calculate_gui_scale`'s own doc), so every logical-pixel constant this
/// draw site uses should come out scaled by exactly 3, including the
/// crosshair's real 9px span becoming 27.
#[test]
fn crosshair_span_scales_with_gui_scale_not_just_at_the_floor() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.show_debug = false;
    let (w, h) = (1280.0_f32, 720.0_f32);
    assert_eq!(
        crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, w as u32, h as u32),
        3,
        "precondition: this framebuffer must resolve to gui_scale 3"
    );
    let geo = HudGeometry::build(&frame, w as u32, h as u32);
    let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
    let (span_x, span_y) = (max_x - min_x, max_y - min_y);
    assert!(
        (span_x - 27.0).abs() < 0.05,
        "crosshair horizontal span should scale to 27px at gui_scale 3, got {span_x}"
    );
    assert!(
        (span_y - 27.0).abs() < 0.05,
        "crosshair vertical span should scale to 27px at gui_scale 3, got {span_y}"
    );
}

/// Twelve candidates named so their widths are equal, so the layout
/// arithmetic below is not also measuring a proportional font.
fn popup_candidates(n: usize) -> Vec<crate::chat::Candidate> {
    (0..n)
        .map(|i| crate::chat::Candidate {
            text: format!("cand{i:02}"),
            tooltip: None,
        })
        .collect()
}

/// The dropdown reaches pixels, and does so **inside its own rect** — the
/// island check for the whole widget.
///
/// Counting vertices alone would pass for a popup drawn off-screen or on top
/// of the hotbar, which is the failure this repo keeps hitting. So the
/// assertion is on *where*: every quad the popup adds must have its corners
/// inside the rect `suggestion_layout` resolved (plus the 1px scroll-hint
/// gutters, which are outside the rows by construction). Mismatches are
/// collected and asserted as a set, so one stray quad does not hide the
/// others.
///
/// The negative control is the same frame with `chat_suggestions: None`, run
/// and compared, not described.
#[test]
fn the_suggestion_popup_draws_inside_the_rect_the_layout_resolved() {
    let stats = DebugStats::default();
    let candidates = popup_candidates(12);
    let (w, h) = (640u32, 480u32);
    let base_frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("ca"),
        chat_caret_visible: false,
        ..HudFrame::new(&stats)
    };
    // The control: identical frame, no popup. Run, not asserted about.
    let control = HudGeometry::build(&base_frame, w, h);

    let popup = SuggestionPopup {
        line: "ca",
        start: 0,
        candidates: &candidates,
        selected: 0,
        offset: 0,
        cursor: None,
    };
    let with = HudGeometry::build(
        &HudFrame {
            chat_suggestions: Some(popup),
            ..base_frame
        },
        w,
        h,
    );
    assert!(
        with.vertex_count() > control.vertex_count(),
        "the popup must add geometry — {} vs {}",
        with.vertex_count(),
        control.vertex_count()
    );

    // Re-derive the rect from the same function the draw called, with the
    // same measure: no font attached here, so `item_icon::text_w`.
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let layout = suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));
    assert_eq!(
        layout.rows,
        crate::chat::SUGGESTION_LINE_LIMIT,
        "12 candidates must be windowed to 10 rows — otherwise the cap is untested"
    );

    // Every vertex the popup added, in canvas pixels. `verts` is NDC over the
    // logical canvas, so undo that rather than restating a pixel formula.
    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    let gutter = pose.max(1.0);
    let mut outside = Vec::new();
    for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
        let (x, y) = (px(chunk[0]), py(chunk[1]));
        let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
        let inside_y =
            y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
        if !(inside_x && inside_y) {
            outside.push((x, y));
        }
    }
    assert!(
        outside.is_empty(),
        "{} of the popup's own vertices landed outside its rect \
         (x {}..{}, y {}..{}): {:?}",
        outside.len(),
        layout.x,
        layout.x + layout.w,
        layout.y - gutter,
        layout.y + layout.h + gutter,
        &outside[..outside.len().min(8)]
    );

    // And the rect really is above the input line rather than over it — the
    // `anchorToBottom` placement, which a sign error would invert.
    assert!(
        layout.y + layout.h <= chat_input_top(ch, pose),
        "the popup's bottom ({}) must sit at or above the input line's top ({})",
        layout.y + layout.h,
        chat_input_top(ch, pose)
    );
}

/// The regression this chat-scale fix can specifically introduce: the draw
/// (`HudGeometry::build_inner`) and the pointer hit-test
/// (`HudRenderer::suggestion_layout`, exercised headlessly here through the
/// same free functions it calls — a GPU-free `wgpu::Device` cannot be
/// constructed in this test, so this is the identical code path minus the
/// device handle) both resolve `chat_pose_scale`. Before this fix
/// `HudGeometry::build_inner` recomputed `HUD_TEXT_SCALE * opts.scale`
/// inline instead of calling [`chat_pose_scale`], so the two *could* have
/// drifted apart the moment either copy changed; now `build_inner` calls
/// the same function the hit-test does, structurally.
///
/// Run at **two** non-coincident chat scales — `1.0` (default) and `0.5`
/// — because a bug that only shows up away from the default (say, a stray
/// `HUD_TEXT_SCALE` reintroduced on one side only) would pass at `1.0` if
/// the two formulas happened to agree there by construction and diverge
/// everywhere else.
#[test]
fn the_hit_test_rect_and_the_drawn_popup_agree_at_two_different_chat_scales() {
    let stats = DebugStats::default();
    let candidates = popup_candidates(12);
    let (w, h) = (640u32, 480u32);

    for chat_scale in [1.0_f32, 0.5] {
        let opts = ChatDisplayOptions {
            scale: chat_scale,
            ..ChatDisplayOptions::default()
        };
        let base_frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some("ca"),
            chat_caret_visible: false,
            chat_options: opts,
            ..HudFrame::new(&stats)
        };
        let control = HudGeometry::build(&base_frame, w, h);

        let popup = SuggestionPopup {
            line: "ca",
            start: 0,
            candidates: &candidates,
            selected: 0,
            offset: 0,
            cursor: None,
        };
        let with = HudGeometry::build(
            &HudFrame {
                chat_suggestions: Some(popup),
                ..base_frame
            },
            w,
            h,
        );

        // The "hit-test region": exactly what `HudRenderer::suggestion_layout`
        // computes (`logical_canvas` → `chat_pose_scale(opts)` →
        // `suggestion_layout`), not a restatement.
        let (cw, ch) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let pose = chat_pose_scale(opts);
        let layout =
            suggestion_layout(cw, ch, pose, &popup, |s| measure_text(None, s, pose));

        // The "drawn region": every vertex the popup actually added.
        let px = |x: f32| (x + 1.0) * 0.5 * cw;
        let py = |y: f32| (1.0 - y) * 0.5 * ch;
        let gutter = pose.max(1.0);
        let mut outside = Vec::new();
        for chunk in with.verts[control.verts.len()..].chunks(FLOATS_PER_VERTEX) {
            let (x, y) = (px(chunk[0]), py(chunk[1]));
            let inside_x = x >= layout.x - 0.5 && x <= layout.x + layout.w + 0.5;
            let inside_y =
                y >= layout.y - gutter - 0.5 && y <= layout.y + layout.h + gutter + 0.5;
            if !(inside_x && inside_y) {
                outside.push((x, y));
            }
        }
        assert!(
            outside.is_empty(),
            "chat_scale {chat_scale}: {} of the popup's own vertices landed \
             outside the hit-test rect (x {}..{}, y {}..{}): {:?}",
            outside.len(),
            layout.x,
            layout.x + layout.w,
            layout.y - gutter,
            layout.y + layout.h + gutter,
            &outside[..outside.len().min(8)]
        );
    }
}

/// `row_at` maps a pointer to the candidate the player is looking at.
///
/// The inputs are chosen so the two plausible readings disagree: with
/// `offset == 2` a hit on the **first visible row** must report candidate
/// `2`, not `0`, and the last visible row must report `11` rather than `9`.
/// An implementation that forgot `+ offset` agrees with the truth only at
/// `offset == 0`, which is why the scrolled case is the one asserted.
#[test]
fn a_pointer_resolves_to_the_candidate_under_it_including_when_scrolled() {
    let candidates = popup_candidates(12);
    let popup = SuggestionPopup {
        line: "ca",
        start: 0,
        candidates: &candidates,
        selected: 0,
        offset: 2,
        cursor: None,
    };
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let layout = suggestion_layout(640.0, 480.0, pose, &popup, |s| measure_text(None, s, pose));
    let mid_x = layout.x + layout.w * 0.5;

    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 0.5, 2, 12),
        Some(2),
        "the first visible row is candidate 2 once the window has scrolled"
    );
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 1.5, 2, 12),
        Some(3)
    );
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.row_h * 9.5, 2, 12),
        Some(11),
        "and the last visible row is the last candidate"
    );
    // Outside, on each of the four edges.
    assert_eq!(layout.row_at(mid_x, layout.y - 1.0, 2, 12), None);
    assert_eq!(
        layout.row_at(mid_x, layout.y + layout.h + 1.0, 2, 12),
        None
    );
    assert_eq!(
        layout.row_at(layout.x - 1.0, layout.y + layout.row_h * 0.5, 2, 12),
        None
    );
    assert_eq!(
        layout.row_at(
            layout.x + layout.w + 1.0,
            layout.y + layout.row_h * 0.5,
            2,
            12
        ),
        None
    );
}

#[test]
fn chat_input_and_log_add_geometry() {
    let stats = DebugStats::default();
    let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
    let chat = [("<a> hi", 0.0_f32), ("<b> yo", 0.0)];
    let frame = HudFrame {
        chat: &chat,
        chat_input: Some("hello"),
        ..HudFrame::new(&stats)
    };
    let with_chat = HudGeometry::build(&frame, 640, 480).vertex_count();
    assert!(with_chat > base, "chat log + input line must add geometry");
}

/// A chat selection is modelled in character positions, but its rectangle
/// must follow the exact rendered glyph advances (including UTF-8 input),
/// land behind the glyphs, and stay inside the input strip.
#[test]
fn chat_input_selection_draws_a_clipped_glyph_aligned_rect() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let input = "aébc";
    let base = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        w,
        h,
    );
    let selected = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_selection: Some((1, 3)),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        w,
        h,
    );

    let (logical_w, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;
    let to_px_y = |ndc_y: f32| (1.0 - ndc_y) * 0.5 * logical_h;

    let selection = selected
        .verts
        .chunks(FLOATS_PER_VERTEX)
        .filter(|vertex| vertex[2..6] == [0.0, 0.0, 1.0, 1.0])
        .map(|vertex| (to_px_x(vertex[0]), to_px_y(vertex[1])))
        .collect::<Vec<_>>();
    assert!(
        !selection.is_empty(),
        "the selected character range must add a blue selection rectangle"
    );

    let (min_x, max_x, min_y, max_y) = selection.iter().fold(
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
        |(min_x, max_x, min_y, max_y), &(x, y)| {
            (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y))
        },
    );
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let left = CHAT_TEXT_INSET * pose + measure_text(None, "a", pose);
    let right = CHAT_TEXT_INSET * pose + measure_text(None, "aéb", pose);
    assert!((min_x - left).abs() < 0.01, "selection begins at its first glyph");
    assert!((max_x - right).abs() < 0.01, "selection ends at its last glyph");
    assert!(
        min_y >= chat_input_top(logical_h, pose) - 0.01
            && max_y <= chat_input_top(logical_h, pose) + font::GLYPH_H as f32 * pose + 0.01,
        "selection stays in the input glyph row"
    );
    assert!(
        selected.vertex_count() > base.vertex_count(),
        "the selection must be a render-model addition, not editor-only state"
    );
}

/// Regression gate for the player report's third defect: `hud.rs` used to
/// draw `format!("> {input}_")` unconditionally, so a `>` prompt appeared
/// that vanilla's own chat-screen input widget never draws. An empty input
/// with the caret off must therefore draw *nothing* beyond its background
/// strip, and turning the caret on must add exactly one `_` glyph — a
/// negative control (caret off) plus a positive one (caret on) rather than
/// eyeballing a vertex-count increase.
#[test]
fn no_stray_prompt_prefix_and_caret_blinks() {
    let stats = DebugStats::default();
    let caret_off = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(""),
            chat_caret_visible: false,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // Only the input row's own translucent background rect (one quad =
    // 6 vertices) may be here — no `>` , no space, nothing.
    assert_eq!(
        caret_off.vertex_count(),
        6,
        "an empty input with the caret off must draw only its background strip"
    );

    let caret_on = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(""),
            chat_caret_visible: true,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // `_`'s bitmap (`font::glyph_rows('_')`) lights only its bottom row's
    // 5 bits — exactly 5 quads = 30 vertices, not a guess.
    assert_eq!(
        caret_on.vertex_count(),
        caret_off.vertex_count() + 30,
        "chat_caret_visible must toggle exactly one `_` glyph (5 lit pixels)"
    );
}

/// Owner report: "the actual insertion point moves with Left/Right but the
/// flashing underscore stays at the end." Two separate defects behind one
/// symptom, and this gate has to see both — the caret's **x** was measured
/// from the width of the whole line rather than of the text before the
/// caret, and its **shape** was the appended `_` unconditionally where
/// vanilla switches to a 1 px insert bar the moment the caret is not at the
/// end (`EditBox.extractWidgetRenderState`'s `insert` predicate choosing
/// between `TextCursorUtils.extractInsertCursor` and `extractAppendCursor`).
///
/// The input and cursor are chosen so the two hypotheses disagree on both
/// axes: `"abcd"` with the caret at 2 is neither position 0 (where an empty
/// line's append and insert x coincide) nor the end (where the *whole*
/// bug is invisible, since "width of the line" and "width of the text
/// before the caret" are the same number). Each assertion below therefore
/// carries the value the buggy code would have produced as well as the
/// right one, so it fails rather than merely being satisfiable.
#[test]
fn chat_caret_follows_the_cursor_and_becomes_a_bar_mid_string() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let input = "abcd";
    let pose = chat_pose_scale(ChatDisplayOptions::default());

    let build = |cursor: Option<usize>, caret: bool| {
        HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat_input: Some(input),
                chat_cursor: cursor,
                chat_caret_visible: caret,
                ..HudFrame::new(&stats)
            },
            w,
            h,
        )
    };

    // The caret is drawn last, and the text and background before it are
    // identical across all three builds, so everything past the caret-off
    // build's vertex count *is* the caret. Nothing else in this frame draws
    // after the input row — the sibling gate above pins that by asserting an
    // empty input with the caret off emits exactly its 6 background
    // vertices.
    let off = build(Some(2), false);
    let mid = build(Some(2), true);
    let end = build(None, true);

    // Shape, as a count rather than an eyeball. `_`'s bitmap lights 5 pixels
    // (5 quads, 30 vertices); the insert bar is a single rect (1 quad, 6).
    // These are the two hypotheses, and they cannot coincide.
    assert_eq!(
        mid.verts.len(),
        off.verts.len() + 6 * FLOATS_PER_VERTEX,
        "a caret inside the line must be the 1 px insert bar (one quad), not \
         the 5-quad `_` glyph"
    );
    assert_eq!(
        end.verts.len(),
        off.verts.len() + 30 * FLOATS_PER_VERTEX,
        "a caret at the end of the line must still be the appended `_`"
    );

    let (logical_w, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;
    let to_px_y = |ndc_y: f32| (1.0 - ndc_y) * 0.5 * logical_h;
    let caret_box = |g: &HudGeometry| {
        g.verts[off.verts.len()..].chunks(FLOATS_PER_VERTEX).fold(
            (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY),
            |(min_x, max_x, min_y, max_y), v| {
                let (x, y) = (to_px_x(v[0]), to_px_y(v[1]));
                (min_x.min(x), max_x.max(x), min_y.min(y), max_y.max(y))
            },
        )
    };

    // Position. `cursorX = textX + width(before) + 1`, then `cursorX--` in
    // insert mode, so the bar's left edge is `textX + width("ab")` exactly.
    // The old code's answer — `textX + width("abcd") + 1`, the append form
    // measured against the whole line — is the `wrong` value below, and the
    // assertion has to land on one of them.
    let inset = CHAT_TEXT_INSET * pose;
    let (mid_left, ..) = caret_box(&mid);
    let right = inset + measure_text(None, "ab", pose);
    let wrong = inset + measure_text(None, input, pose) + pose;
    assert!(
        (wrong - right) > 1.0,
        "the fixture must separate the two hypotheses: correct {right}, buggy \
         {wrong}"
    );
    assert!(
        (mid_left - right).abs() < 0.01,
        "the insert bar sits after the text before the caret ({right}), not at \
         the end of the line ({wrong}); measured {mid_left}"
    );

    // And the append arm still lands where it always did, so the fix moved
    // the mid-string caret rather than shifting every caret left.
    let (end_left, ..) = caret_box(&end);
    assert!(
        (end_left - wrong).abs() < 0.01,
        "the appended `_` keeps vanilla's `textX + width(value) + 1`; expected \
         {wrong}, measured {end_left}"
    );

    // The bar's rect: vanilla's `fill(x, y - 1, x + 1, y + lineHeight, …)`
    // spans one pixel above the glyph box to one below it, and is one pixel
    // wide. Both in this surface's scaled pixels.
    let (bar_l, bar_r, bar_t, bar_b) = caret_box(&mid);
    let top = chat_input_top(logical_h, pose);
    assert!(
        (bar_r - bar_l - pose).abs() < 0.01,
        "the insert bar is one (scaled) pixel wide, measured {}",
        bar_r - bar_l
    );
    assert!(
        (bar_t - (top - pose)).abs() < 0.01
            && (bar_b - (top + (font::GLYPH_H as f32 + 1.0) * pose)).abs() < 0.01,
        "the insert bar spans the glyph row plus one pixel either side; got \
         {bar_t}..{bar_b} against a glyph row starting at {top}"
    );
}

/// Owner report: "the inline autocomplete suggestion gets offset by the
/// ticking underscore, when it should not move." The ghost's pen used to
/// be measured from `{input}{caret}`'s live width, and `caret` is `""`
/// half of every blink cycle — so the ghost's x shifted by the caret
/// glyph's own advance every ~300ms. Fixed by always measuring against
/// `{input}_` regardless of the actual blink state.
///
/// This predicts the *old* (buggy) pens from first principles — via the
/// same jar-less `measure_text` the popup gate above uses — and asserts
/// they really would have differed, which is what makes the "now equal"
/// assertion below a discriminating regression rather than a vacuous one
/// (a font where `_` measured zero-width could satisfy equality by
/// accident either way).
#[test]
fn suggestion_ghost_pen_does_not_move_with_the_caret_blink() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let underscore_w = measure_text(None, "_", pose);
    assert!(
        underscore_w > 0.0,
        "the fallback font must give `_` a real width, or this test cannot \
         discriminate the fix from the bug it replaces"
    );

    let ghost_min_x = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(chunk[0]);
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    let frame = |caret_visible: bool| HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: caret_visible,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let on = HudGeometry::build(&frame(true), w, h);
    let off = HudGeometry::build(&frame(false), w, h);

    // What the pre-fix formula (`margin + text_width({input}{caret})`)
    // would have produced: the two pens differ by exactly `_`'s own
    // advance, since that is the only difference between the two
    // measured strings.
    let old_pen_on = measure_text(None, "he_", pose);
    let old_pen_off = measure_text(None, "he", pose);
    assert!(
        (old_pen_on - old_pen_off - underscore_w).abs() < 1e-4,
        "sanity check on the reproduction itself: the old pens must differ \
         by exactly the caret glyph's width"
    );

    assert_eq!(
        ghost_min_x(&on),
        ghost_min_x(&off),
        "the suggestion ghost's x must not move when the caret blinks"
    );
}

/// The landed blink-invariance fix above made the ghost's pen *stable*,
/// but stable at the wrong x: one whole underscore-width too far right,
/// permanently. **The discriminating assertion is the absolute x, not
/// stability** — a gate that only re-runs the blink-invariance check
/// above would pass on the regression this predicts and rejects.
///
/// `HudGeometry`'s `verts` are in **NDC** (`ColourStream::rect`'s own doc:
/// "positions in NDC"), not pixels, so the pixel-space prediction below is
/// converted through the same `to_ndc` vanilla-canvas math the draw uses
/// — via [`crate::menu::render::logical_canvas`], the one function that
/// resolves a framebuffer size to the logical canvas every layout site
/// (including this draw) measures against.
#[test]
fn suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (logical_w, _) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_ndc_x = |px: f32| 2.0 * px / logical_w - 1.0;

    let ghost_min_x = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(chunk[0]);
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    // The ghost text starts with `A`, not `llo` as the other gates in this
    // module use — deliberately: this fallback font's `l` glyph has a
    // **blank leading column** (`font::glyph_rows('l')`'s column 0 is
    // unlit in all seven rows), so "leftmost lit pixel" would measure one
    // glyph-column right of the real pen for any string starting with
    // `l`. `A` lights column 0 on at least one row, so its own leftmost
    // lit pixel *is* the pen position — which is what this test needs to
    // assert an exact x. The other gates in this module only compare
    // *relative* ghost positions, where that per-glyph offset cancels out
    // and does not matter.
    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("Allo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);

    // Vanilla's `cursorX - 1`, `cursorX` being `EditBox
    // .extractWidgetRenderState`'s `drawX` *after* `drawX +=
    // font.width(charSequence) + 1;` — the typed text's width, no caret
    // glyph folded in (unlike the older, already-fixed `font.width("he_")`
    // bug), **plus vanilla's own reserved pixel**, which the `- 1` then
    // exactly cancels: `(font.width("he") + 1) - 1 == font.width("he")`.
    // So the *correct* ghost position is flush with the raw text width —
    // no further arithmetic on top of it, which is exactly the
    // discriminating case: a formula that forgets vanilla's `+ 1` (this
    // draw site's own bug until now) computes `font.width("he") - 1`
    // instead, landing the ghost one pixel *short*, overlapping into the
    // text's last glyph rather than sitting flush against it.
    let expected = to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he", pose));
    let missing_caret_width_hypothesis =
        to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he_", pose) - pose);
    let missing_plus_one_hypothesis =
        to_ndc_x(CHAT_TEXT_INSET * pose + measure_text(None, "he", pose) - pose);
    for (name, hypothesis) in [
        ("the caret-width bug", missing_caret_width_hypothesis),
        ("the missing `+ 1` bug", missing_plus_one_hypothesis),
    ] {
        assert!(
            (expected - hypothesis).abs() > 1e-3,
            "sanity check on the reproduction: {name}'s hypothesis must be \
             discriminably far from the correct one, or a coincidence could \
             pass either way"
        );
    }
    assert!(
        (ghost_min_x(&geo) - expected).abs() < 1e-4,
        "ghost x (NDC) = {}, expected cursorX - 1 = {expected} (the \
         caret-width bug would have placed it at \
         {missing_caret_width_hypothesis}, the missing-`+ 1` bug at \
         {missing_plus_one_hypothesis})",
        ghost_min_x(&geo)
    );
}

/// **The owner's report**: "the inline completion (grey text) is missing
/// the pixel gap after the last character... it touches the regular text
/// which is wrong." Established by direct comparison against
/// `crates/lodestone-shell/src/menu/edit_box.rs`'s `draw_state_with`,
/// which already carries vanilla's `+ 1.0`
/// (`EditBox.extractWidgetRenderState`'s `drawX += font.width
/// (charSequence) + 1;`) — this draw site did not.
///
/// **Why the assertion is against the text's own right edge, not the
/// caret.** A first attempt at this gate measured `caret_x - ghost_x` and
/// found it passed under a deliberate re-neuter of the `+ 1` fix —
/// because both the ghost (`cursor_x - pose`) and the caret (`cursor_x`)
/// move together with `cursor_x`, so the gap *between them* is `pose`
/// regardless of whether `cursor_x` itself carries vanilla's `+ 1`. The
/// bug is a shift of the whole `{ghost, caret}` pair relative to the
/// *text*, not a change in their separation from each other — so only a
/// measurement against the text's own (independently computed) right edge
/// can see it. Before the fix, `ghost_x` sat a full `pose` *before* the
/// text's right edge (overlapping the last glyph, `font.width(value) - 1`
/// instead of vanilla's `(font.width(value) + 1) - 1 ==
/// font.width(value)`); after it, `ghost_x` sits flush with the text's
/// right edge, matching vanilla's own edit-box widget's own cancellation exactly — not a
/// visible pixel of daylight, but no longer overlapping into the glyph
/// either, which is the actual "touches" the report named.
#[test]
fn the_ghost_sits_flush_with_the_text_not_overlapping_its_last_glyph() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (logical_w, _) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let to_px_x = |ndc_x: f32| (ndc_x + 1.0) * 0.5 * logical_w;

    let ghost_min_px = |g: &HudGeometry| -> f32 {
        let mut min_x = f32::INFINITY;
        for chunk in g.verts.chunks(FLOATS_PER_VERTEX) {
            if chunk[2..6] == SUGGESTION_GHOST {
                min_x = min_x.min(to_px_x(chunk[0]));
            }
        }
        assert!(min_x.is_finite(), "no SUGGESTION_GHOST-coloured vertex found");
        min_x
    };

    // Two pairwise-distinct inputs, different lengths, so a formula that
    // (incorrectly) makes the offset depend on the text's own width
    // cannot pass by coincidence at a single length.
    for input in ["he", "cats"] {
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            chat_input: Some(input),
            chat_caret_visible: true,
            chat_suggestion_ghost: Some("Allo"),
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build(&frame, w, h);
        let ghost_x = ghost_min_px(&geo);

        // The text's own right edge, computed from the font's advance
        // metric alone (`measure_text`) — not through `cursor_x`, the
        // value the draw itself derives the ghost from, so this cannot
        // pass by restating the code under test.
        let text_right_edge = CHAT_TEXT_INSET * pose + measure_text(None, input, pose);
        let offset = ghost_x - text_right_edge;
        // The bug this replaces: `font.width(value) - 1` (the missing
        // `+ 1` never reserved, so the ghost lands one pixel *inside* the
        // text's last glyph instead of flush with its advance edge). A
        // constant, not a second measurement, so the two hypotheses can
        // never coincide by construction (`0.0 - (-pose)` is always
        // `pose`, well past the tolerance below).
        let overlap_hypothesis = -pose;

        assert!(
            offset.abs() < pose * 0.25,
            "input {input:?}: the ghost must sit flush with the text's own \
             right edge (vanilla's `(font.width(value) + 1) - 1 == \
             font.width(value)`), not offset from it: measured {offset:.3}px"
        );
        assert!(
            (offset - overlap_hypothesis).abs() > pose * 0.5,
            "input {input:?}: measured offset {offset:.3}px is too close to \
             the missing-`+ 1` bug's prediction of overlapping the text's \
             last glyph by {overlap_hypothesis:.3}px to discriminate the \
             fix from the bug it replaces"
        );
    }
}

/// The other half of the fix: the caret must draw **after** (on top of)
/// the suggestion, not before — vanilla's own edit-box widget's render order is text →
/// hint → suggestion → highlight → cursor. `HudGeometry::build` appends
/// vertices in draw order, so "after" is observable as "later in `verts`".
#[test]
fn caret_draws_after_the_suggestion_so_it_composites_on_top() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);

    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some("he"),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);

    let last_index_with_color = |target: [f32; 4]| -> Option<usize> {
        geo.verts
            .chunks(FLOATS_PER_VERTEX)
            .enumerate()
            .filter(|(_, chunk)| chunk[2..6] == target)
            .map(|(i, _)| i)
            .max()
    };
    let ghost_last = last_index_with_color(SUGGESTION_GHOST)
        .expect("the ghost must draw when chat_suggestion_ghost is Some");
    // The caret shares the input text's own white and the same input
    // row, so identify it as a white quad **inside that row's glyph box**
    // appearing after the ghost — restricted to the row so an unrelated
    // white element elsewhere in the frame (this test does not disable
    // every HUD element) cannot produce a false pass. The box spans the
    // full glyph height, not just `input_y` exactly: `_`'s own bitmap
    // (`font::glyph_rows('_')`) only lights the bottom row, so its quad's
    // y sits `6 * pose` px below `input_y`, not at it. `input_y` and the
    // span are converted to NDC the same way
    // [`suggestion_ghost_sits_at_cursor_x_minus_one_not_after_the_caret`]
    // converts x, using the logical (not raw framebuffer) canvas height
    // `chat_input_top` itself is measured against.
    let pose = chat_pose_scale(ChatDisplayOptions::default());
    let (_, logical_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let input_y_ndc = 1.0 - 2.0 * chat_input_top(logical_h, pose) / logical_h;
    let glyph_h_ndc = 2.0 * (font::GLYPH_H as f32 * pose) / logical_h;
    let white = [1.0_f32, 1.0, 1.0, 1.0];
    let caret_after_ghost = geo.verts.chunks(FLOATS_PER_VERTEX).enumerate().skip(ghost_last + 1).any(
        |(_, chunk)| {
            chunk[2..6] == white
                && chunk[1] <= input_y_ndc + 1e-3
                && chunk[1] >= input_y_ndc - glyph_h_ndc - 1e-3
        },
    );
    assert!(
        caret_after_ghost,
        "the caret's white quad, on the input's own row, must appear \
         after the ghost's grey quad in draw order, so it composites on \
         top"
    );
}

/// Vanilla's `!insert` gate: a full line (256 chars, `ChatInput::push_char`'s
/// own cap) suppresses the suggestion entirely, matching
/// `EditBox`'s own `insert = cursorPos < value.length() || value.length()
/// >= maxLength` — this shell's chat caret is always at the end (see the
/// draw's own comment), so only the length half of that disjunction can
/// ever apply here.
#[test]
fn suggestion_is_suppressed_once_the_chat_line_is_full() {
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let full_line: String = "x".repeat(256);

    let frame = HudFrame {
        crosshair: false,
        show_debug: false,
        chat_input: Some(full_line.as_str()),
        chat_caret_visible: true,
        chat_suggestion_ghost: Some("llo"),
        ..HudFrame::new(&stats)
    };
    let geo = HudGeometry::build(&frame, w, h);
    let has_ghost = geo
        .verts
        .chunks(FLOATS_PER_VERTEX)
        .any(|chunk| chunk[2..6] == SUGGESTION_GHOST);
    assert!(!has_ghost, "a full line must draw no suggestion ghost");
}

/// Predicts the exact geometry of a hard-wrapped chat line from first
/// principles (box width, the fixed fallback font's per-char advance, and
/// `a`'s own lit-pixel count), rather than merely asserting "it wrapped" —
/// CLAUDE.md's *magnitude* species of vacuous test is a predicate that
/// would pass for any wrap width; this one would fail for a wrong one.
#[test]
fn a_long_line_with_no_spaces_hard_wraps_at_the_predicted_row_count() {
    let stats = DebugStats::default();
    let line = "a".repeat(70);
    let chat = [(line.as_str(), 0.0_f32)];
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    // The default chat box is `chat_width_px(1.0) == 320`px wide (capped
    // at `b.w == 640`, so uncapped here). With no `VanillaFont` attached,
    // `Builder::legacy_width` falls back to `item_icon::text_w`:
    // `(GLYPH_W + 1) * scale` per char, and the chat pose scale is
    // vanilla's own chat-scale option alone (`chat_pose_scale`,
    // vanilla's own scale getter), `1.0` at the default, so each `a` costs
    // `6 * 1.0 == 6`px. `floor(320 / 6) == 53` fit the first row; the
    // remaining `70 - 53 == 17` spill to a second — two rows.
    //
    // 70 chars, not 30: at this HUD's now-deleted ad-hoc 2× pitch each
    // `a` would have cost `12`px (`floor(320 / 12) == 26` per row), which
    // wraps 70 chars into **three** rows (26 + 26 + 18), not two — a
    // whole extra row, not a rounding-sized difference, so this input
    // cannot coincide between the two hypotheses the way a shorter line
    // could.
    //
    // `a`'s bitmap (`font::glyph_rows('a')`) lights `0+0+3+1+4+2+4 == 14`
    // pixels; each lit pixel is one quad (`ColourStream::glyph`)
    // of 6 vertices, so all 70 `a`s cost
    // `70 * 14 * 6 == 5880` vertices regardless of how they are split
    // across rows — the row *count* shows up only in the background
    // strips, one 6-vertex rect each.
    assert_eq!(
        geo.vertex_count(),
        5880 + 2 * 6,
        "expected exactly two wrapped rows' worth of geometry at vanilla's \
         chat-scale-only pose (one row would be 5880 + 6, three — the \
         deleted ad-hoc 2× pitch's prediction — would be 5880 + 18)"
    );
}

/// Direct, GPU-free gate on [`wrap_legacy_with`]'s wrap *decision*, using a
/// hand-specified width table rather than the fixed 5×7 fallback — the
/// fallback is itself fixed-advance, so it cannot exercise the
/// variable-width case the real vanilla font (attached only when a jar is
/// present) actually draws with. `i`/`W`'s widths below are vanilla's own,
/// documented in `crate::hud::vanilla_font`'s module doc ("`i` is 2 px
/// wide … `W` and `M` are 6"); the competing "flat character count"
/// hypothesis uses this shell's own real fixed-advance constant
/// (`(font::GLYPH_W + 1) * 1.0 == 6`) rather than an invented
/// number, so both sides of the comparison are real, citable code.
#[test]
fn wrap_uses_real_per_glyph_widths_not_a_flat_character_count() {
    let real_width = |s: &str| -> f32 {
        s.chars()
            .map(|c| match c {
                'i' => 2.0,
                'W' => 6.0,
                _ => 0.0,
            })
            .sum()
    };
    let flat_count_width = |s: &str| -> f32 { s.chars().count() as f32 * 6.0 };

    // Five narrow glyphs then five wide ones, no spaces, so the wrap is a
    // pure hard-break character-index decision with no word-boundary
    // logic muddying which hypothesis "wins".
    let s = "iiiiiWWWWW";
    let max_width_px = 20.0;

    // Real cumulative widths: 2,4,6,8,10 (the five `i`s), then 16, 22 …
    // for the `W`s — the largest prefix at or under 20px is "iiiiiW"
    // (16px); the next `W` would make 22px.
    let real_rows = wrap_legacy_with(real_width, s, max_width_px);
    assert_eq!(
        real_rows.first().map(String::as_str),
        Some("iiiiiW"),
        "real per-glyph widths must break after the 6th character: {real_rows:?}"
    );

    // The flat hypothesis charges every character 6px regardless of
    // glyph, so only `floor(20 / 6) == 3` fit before the 4th overflows —
    // three characters, not six.
    let flat_rows = wrap_legacy_with(flat_count_width, s, max_width_px);
    assert_eq!(
        flat_rows.first().map(String::as_str),
        Some("iii"),
        "a flat character-count hypothesis must break after the 3rd character: {flat_rows:?}"
    );

    let real_break = real_rows[0].chars().count();
    let flat_break = flat_rows[0].chars().count();
    assert_eq!(real_break, 6, "predicted real-width break index");
    assert_eq!(flat_break, 3, "predicted flat character-count break index");
    assert_eq!(
        real_break - flat_break,
        3,
        "the two hypotheses must diverge by a real, non-zero margin, or this test \
         cannot tell a real-width wrap from a character-count one"
    );
}

/// **The bug report**: a chat message carrying a literal `\n` (a
/// multi-line system message, or a pasted multi-line player message)
/// rendered the control character as a missing-glyph box instead of
/// breaking the line, because [`wrap_legacy_with`] only ever split on
/// `' '`. A width table that charges `\n` a large, easy-to-notice cost
/// makes the point unambiguously: if the character survived into a row
/// unbroken, the row's measured width would blow way past `max_width_px`
/// and this test's own `real_width` closure would report it — the
/// control is built into the fixture rather than bolted on afterward.
#[test]
fn wrap_legacy_with_splits_on_a_literal_newline() {
    let width = |s: &str| -> f32 {
        s.chars()
            .map(|c| if c == '\n' { 1000.0 } else { 1.0 })
            .sum()
    };
    let rows = wrap_legacy_with(width, "first line\nsecond line", 200.0);
    assert_eq!(
        rows,
        vec!["first line".to_string(), "second line".to_string()],
        "a literal \\n must start a new row, not survive as a character in the \
         middle of one: {rows:?}"
    );
    assert!(
        rows.iter().all(|r| !r.contains('\n')),
        "no returned row may still carry the control character: {rows:?}"
    );
}

/// [`wrap_measured`]'s own precedent (`menu::render::draw`): a blank line
/// in the source is a line, not something that collapses when its
/// neighbours are pulled together. `wrap_legacy_paragraph("")`'s
/// documented "never empty" guarantee is what makes this fall out for
/// free from the `\n` split alone — this test is what proves that
/// guarantee actually reaches the outer function rather than being an
/// unused promise on the inner one.
#[test]
fn wrap_legacy_with_treats_a_blank_paragraph_as_a_line() {
    let width = |s: &str| -> f32 { s.chars().count() as f32 };
    let rows = wrap_legacy_with(width, "a\n\nb", 200.0);
    assert_eq!(
        rows,
        vec!["a".to_string(), String::new(), "b".to_string()],
        "a blank line between two real ones must survive as its own empty row: {rows:?}"
    );
}

/// The [`TextSpan`] sibling of [`wrap_uses_real_per_glyph_widths_not_a_flat_character_count`]:
/// same real-per-glyph-width table over the same `"iiiiiWWWWW"` input, so
/// [`wrap_spans_with`] is proven against a real width hypothesis rather
/// than the fixed-advance fallback. Sharpened past that test in the one
/// way a span list can be: the ten glyphs are split across two
/// *differently-styled* runs, so the wrap point falls **inside** the
/// styled boundary — the case `wrap_legacy_with` cannot even express,
/// since a `§` code and the run it colours are just characters to it.
#[test]
fn wrap_spans_breaks_by_real_width_and_keeps_style_across_the_break() {
    let real_width = |spans: &[TextSpan]| -> f32 {
        spans
            .iter()
            .flat_map(|s| s.text.chars())
            .map(|c| match c {
                'i' => 2.0,
                'W' => 6.0,
                _ => 0.0,
            })
            .sum()
    };
    let max_width_px = 20.0;
    let red = TextSpan {
        text: "iiiii".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Red),
            ..TextStyle::default()
        },
    };
    let blue = TextSpan {
        text: "WWWWW".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Blue),
            ..TextStyle::default()
        },
    };
    let spans = [red, blue];

    let rows = wrap_spans_with(real_width, &spans, max_width_px);
    let first_row = &rows[0];
    let joined: String = first_row.iter().map(|s| s.text.as_str()).collect();
    assert_eq!(
        joined, "iiiiiW",
        "must break after the 6th glyph (16px), same as the legacy gate's real-width \
         hypothesis: {rows:?}"
    );
    assert_eq!(
        first_row.len(),
        2,
        "the wrap point falls mid-style, so the row must keep two separate runs \
         rather than merging across the boundary: {first_row:?}"
    );
    assert_eq!(
        first_row[0].style.color,
        Some(TextColor::Red),
        "the `i`s before the break must keep their colour"
    );
    assert_eq!(
        first_row[1].style.color,
        Some(TextColor::Blue),
        "the lone `W` carried onto this row must keep *its own* colour, not the \
         red run's — a single-pending-style model (one colour per row) would get \
         this wrong on a row that starts one style and ends in another"
    );
}

/// The [`TextSpan`] sibling of [`wrap_legacy_with_splits_on_a_literal_newline`],
/// sharpened past it the one way a span list can be: the `\n` sits
/// **inside** a single styled run rather than between two plain `&str`s,
/// which is exactly the case [`split_span_paragraphs`] exists for — a
/// plain `&str::split('\n')` has no style to preserve, but a `TextSpan`'s
/// run does, on both sides of the break.
#[test]
fn wrap_spans_with_splits_on_a_literal_newline_mid_span() {
    let width = |spans: &[TextSpan]| -> f32 {
        spans
            .iter()
            .flat_map(|s| s.text.chars())
            .map(|c| if c == '\n' { 1000.0 } else { 1.0 })
            .sum()
    };
    let styled = TextSpan {
        text: "before\nafter".to_string(),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Red),
            ..TextStyle::default()
        },
    };
    let rows = wrap_spans_with(width, std::slice::from_ref(&styled), 200.0);
    let joined: Vec<String> = rows
        .iter()
        .map(|row| row.iter().map(|s| s.text.as_str()).collect())
        .collect();
    assert_eq!(
        joined,
        vec!["before".to_string(), "after".to_string()],
        "a \\n sitting inside one styled run must still split into two rows: {joined:?}"
    );
    assert!(
        rows.iter().all(|row| row.iter().all(|s| s.style.color == Some(TextColor::Red))),
        "both sides of the split must keep the run's original colour: {rows:?}"
    );
}

/// Proves `chat_options.colors` is read, not merely stored. `§c` is
/// zero-width whether it recolours or is stripped, so the two frames'
/// vertex *counts* are equal by construction — the option's whole effect
/// is on colour, so the control that actually matters is `verts`
/// (positions **and** colours) differing.
#[test]
fn chat_colors_option_strips_legacy_codes_when_off() {
    let stats = DebugStats::default();
    let coded = [("\u{00a7}chi", 0.0_f32)];
    let frame = |colors: bool| HudFrame {
        crosshair: false,
        show_debug: false,
        chat: &coded,
        chat_options: ChatDisplayOptions {
            colors,
            ..ChatDisplayOptions::default()
        },
        ..HudFrame::new(&stats)
    };
    let with_colors = HudGeometry::build(&frame(true), 640, 480);
    let without_colors = HudGeometry::build(&frame(false), 640, 480);
    assert_eq!(
        with_colors.vertex_count(),
        without_colors.vertex_count(),
        "the code is zero-width either way, so geometry *count* must match"
    );
    assert_ne!(
        with_colors.verts, without_colors.verts,
        "chat_colors=false must actually strip the colour, not just round-trip the option"
    );
}

/// Proves `chat_options.background_opacity` is read with the right
/// *magnitude*, not merely that changing it changes something — the
/// species of vacuous test CLAUDE.md calls out (a hurt-overlay gate once
/// passed 3440/3440 while only checking the *sign* of a change, not how
/// much). Row 0's background rect is emitted before any of its text
/// glyphs, so its first vertex's alpha channel is `verts[5]` — no
/// filtering, no averaging, the exact float the draw call passed in.
#[test]
fn chat_background_opacity_sets_the_exact_row_alpha() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    for bg in [0.1_f32, 0.5, 1.0] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    background_opacity: bg,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let alpha = geo.verts[5];
        assert!(
            (alpha - bg).abs() < 1e-5,
            "row background alpha must equal chat_background_opacity ({bg}), got {alpha}"
        );
    }
}

/// As [`chat_background_opacity_sets_the_exact_row_alpha`], for
/// `chat_options.text_opacity`: `hi` carries no `§` code, so its colour
/// stays `base` throughout `Builder::text_legacy`'s fallback path and
/// every glyph pixel's alpha is exactly the `alpha` parameter passed in —
/// here, `text_opacity * 0.9 + 0.1` at a fresh
/// line's fade of `1.0`.
#[test]
fn chat_text_opacity_sets_the_exact_glyph_alpha() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    for op in [0.0_f32, 0.5, 1.0] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    text_opacity: op,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let expected = op.mul_add(0.9, 0.1);
        // `verts[0..36)` is row 0's background rect (6 vertices); `h`'s
        // bitmap (`font::glyph_rows('h')`) lights bit 0 of its very top
        // row, so the next quad emitted is that pixel — its alpha is
        // `verts[41]` (the 6th float of the 2nd vertex block).
        let alpha = geo.verts[41];
        assert!(
            (alpha - expected).abs() < 1e-5,
            "text_opacity {op}: expected glyph alpha {expected}, got {alpha}"
        );
    }
}

/// As the two magnitude gates above, for `chat_options.width_pct`, via
/// vanilla's own `ChatComponent.getWidth` algebra
/// (`pct * 280.0 + 40.0`, floored) computed independently here rather
/// than by calling [`chat_width_px`] — so a bug shared between the two
/// could not cancel out.
#[test]
fn chat_width_option_sizes_the_box_to_the_predicted_pixel_width() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    // `b.w == 320` at this canvas size:
    // `logical_canvas(AUTO_GUI_SCALE, 640, 480) == (320, 240)` (height
    // binds at `calculate_gui_scale(0, 640, 480) == 2`).
    const CANVAS_W: f32 = 320.0;
    for (pct, expected_px) in [(1.0_f32, 320.0_f32), (0.5, 180.0), (0.0, 40.0)] {
        let geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &chat,
                chat_options: ChatDisplayOptions {
                    width_pct: pct,
                    ..ChatDisplayOptions::default()
                },
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        // Row 0's background rect starts at `x == 0`, so its second
        // vertex `(x + w, y)` (`ColourStream::rect`) converted to NDC is
        // `2 * w / b.w - 1` — `verts[6]`.
        //
        // That rect is the *plate*, which is deliberately wider than the
        // text column it sits behind — see [`CHAT_PLATE_PAD_PX`], and note
        // the pad is the only shared term here: the discriminating part,
        // the `pct * 280 + 40` slope, is still derived independently of
        // `chat_width_px`, so a wrong slope cannot cancel out.
        let pad = CHAT_PLATE_PAD_PX * chat_pose_scale(ChatDisplayOptions::default());
        let x1_ndc = geo.verts[6];
        let expected_ndc = 2.0 * (expected_px + pad) / CANVAS_W - 1.0;
        assert!(
            (x1_ndc - expected_ndc).abs() < 1e-4,
            "pct {pct}: expected box width {expected_px}px plus {pad}px of plate \
             padding (x1 {expected_ndc}), got x1 {x1_ndc}"
        );
    }
}

/// As the width gate above, for `chat_options.scale`: it must exactly
/// double the on-screen row height when set to `2.0`, not merely change
/// it by some amount.
#[test]
fn chat_scale_option_doubles_the_row_height_exactly() {
    let stats = DebugStats::default();
    let chat = [("hi", 0.0_f32)];
    let frame = |scale: f32| HudFrame {
        crosshair: false,
        show_debug: false,
        chat: &chat,
        chat_options: ChatDisplayOptions {
            scale,
            ..ChatDisplayOptions::default()
        },
        ..HudFrame::new(&stats)
    };
    let default_geo = HudGeometry::build(&frame(1.0), 640, 480);
    let doubled_geo = HudGeometry::build(&frame(2.0), 640, 480);
    // Row 0's rect vertex 0 (`y0`) and vertex 2 (`y1`, the 3rd vertex —
    // floats 12..18) give its height in NDC: `verts[1] - verts[13]`.
    let height = |g: &HudGeometry| g.verts[1] - g.verts[13];
    let default_h = height(&default_geo);
    let doubled_h = height(&doubled_geo);
    assert!(default_h > 0.0, "sanity: the rect must have positive height");
    assert!(
        (doubled_h - 2.0 * default_h).abs() < 1e-5,
        "chat_scale=2.0 must exactly double the row height: default {default_h}, doubled {doubled_h}"
    );
}

/// Proves `chat_options.height_pct_unfocused` is read as a genuine *cap*
/// on visible rows, not just stored: at `0.0` (`chat_height_px(0.0) ==
/// 20`px against a `9`px vanilla-metrics default row — vanilla's own
/// `messageHeight`, vanilla's own chat-component rendering) exactly two rows fit
/// (`floor(20 / 9) == 2`), so a five-line log must render identically to
/// a two-line log, not five.
#[test]
fn chat_height_option_caps_the_number_of_visible_rows() {
    let stats = DebugStats::default();
    let chat = [
        ("a", 0.0_f32),
        ("b", 0.0),
        ("c", 0.0),
        ("d", 0.0),
        ("e", 0.0),
    ];
    let capped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            chat_options: ChatDisplayOptions {
                height_pct_unfocused: 0.0,
                ..ChatDisplayOptions::default()
            },
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let two_lines_uncapped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat[3..],
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert_eq!(
        capped.vertex_count(),
        two_lines_uncapped.vertex_count(),
        "height_pct_unfocused == 0.0 must cap the scrollback to exactly two rows \
         at vanilla's 9px row height"
    );
    let uncapped = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        uncapped.vertex_count() > capped.vertex_count(),
        "the default (uncapped-enough-for-5-lines) height must show more than the capped one"
    );
}

#[test]
fn chat_colour_codes_are_zero_width_and_recolour_runs() {
    let stats = DebugStats::default();
    // A `§c` prefix must not add glyph geometry (codes are 2 chars / 0 width):
    // "§chi" and "hi" draw the same number of lit pixels.
    let plain = [("hi", 0.0_f32)];
    let coded = [("\u{00a7}chi", 0.0_f32)];
    let plain_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &plain,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let coded_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &coded,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert_eq!(
        plain_geo.vertex_count(),
        coded_geo.vertex_count(),
        "a colour code must draw no glyphs of its own"
    );
    // …but the pixels must be a different colour, so the code isn't ignored.
    assert_ne!(
        plain_geo.verts, coded_geo.verts,
        "a colour code must recolour the run, not merely be stripped"
    );
}

#[test]
fn chat_lines_fade_out_with_age_when_closed() {
    let stats = DebugStats::default();
    // A fresh line draws; a line older than the visible window draws nothing.
    let fresh = [("hello", 0.0_f32)];
    let stale = [("hello", 30.0_f32)];
    let fresh_n = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &fresh,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    let stale_n = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &stale,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    assert!(fresh_n > 0, "a fresh chat line must be visible");
    assert_eq!(
        stale_n, 0,
        "a line past its lifetime must vanish when closed"
    );

    // Opening the box (a chat_input present) resurrects the stale line.
    let opened = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &stale,
            chat_input: Some(""),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    )
    .vertex_count();
    assert!(
        opened > 0,
        "an open chat box shows history regardless of age"
    );
}

/// The discriminating fixture for the whole chat-hex bug: one line
/// carrying all three colour conventions a real server mixes — a modern
/// `TextColor::Rgb` component style, a legacy-named component style, and
/// a `§` code embedded *inside* a literal (the shape the owner's report
/// actually showed: `"§f§lG§r§f §r§8| §rLumberjack..."` is entirely this
/// third convention). A fixture using only named colours cannot tell
/// "hex survives" from "everything survives", because a named colour
/// gets through the lossy `chat: &[(&str, f32)]` path too — see the
/// control below, which proves that path really does lose it.
#[test]
fn chat_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices() {
    use lodestone_model::text::Text;

    let hex = Text {
        content: lodestone_model::text::TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: no component-level colour at all, the `§c`
    // lives inside the literal text itself, exactly as a plugin server
    // (or the owner's server) embeds one.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: lodestone_model::text::TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let root = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };
    let spans = root.resolve(&|_| None).to_spans();
    assert_eq!(
        spans.len(),
        3,
        "sanity: three runs in, three runs out — {spans:?}"
    );

    let stats = DebugStats::default();
    let chat_spans = [(spans.as_slice(), 0.0_f32)];
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_spans: &chat_spans,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the line must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |rgb: (u8, u8, u8)| {
        geo.verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    // Pairwise-distinct RGB triples, per this repo's own fixture rule —
    // a transposition or a fallback-to-base cannot hide behind a shared
    // value. Vanilla's own named `red` and `gray` chat colours are
    // hand-transcribed from their packed RGB integers (16733525 and
    // 11184810 respectively) rather than read back through
    // `TextColor::rgb()`, which would make this `decode(encode(x)) == x`.
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(*rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same three-way message, but through the lossy
    // `chat: &[(&str, f32)]` path (`Text::to_legacy_string`, which has
    // no representation for `TextColor::Rgb`). This must show the loss —
    // if it did not, the assertion above would be proving nothing about
    // which path actually carries the colour.
    let flattened = root.resolve(&|_| None).to_legacy_string();
    let chat_legacy = [(flattened.as_str(), 0.0_f32)];
    let legacy_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat: &chat_legacy,
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let legacy_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (legacy_byte(v[2]), legacy_byte(v[3]), legacy_byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy-string path was expected to lose the hex colour \
         (that is the bug), but it drew it anyway — this test's premise is wrong"
    );
}

/// The held-item name highlight's own version of the chat gate just
/// above: `lodestone_game::item::styled_hover_name_spans` feeding
/// `HudFrame::held_item_spans` must reach three pairwise-distinct vertex
/// RGBs, and the legacy `HudFrame::held_item` path built from the same
/// tree via `styled_hover_name`/`to_legacy_string` must lose the hex —
/// the control that proves the assertion above is measuring the right
/// path, not a coincidence.
#[test]
fn held_item_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices() {
    use lodestone_model::text::Text;

    let hex = Text {
        content: lodestone_model::text::TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    // The inline convention: a server-authored item name whose colour
    // lives inside the literal text as a `§c` code rather than as a
    // component-level style — the second clause `Text::to_spans` handles
    // and `styled_hover_name_spans` inherits for free.
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: lodestone_model::text::TextContent::Literal("Gray".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Gray),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let root = Text {
        extra: vec![hex, inline_legacy, named],
        ..Text::default()
    };
    let spans = root.resolve(&|_| None).to_spans();
    assert_eq!(
        spans.len(),
        3,
        "sanity: three runs in, three runs out — {spans:?}"
    );

    let stats = DebugStats::default();
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            held_item_spans: Some((spans.clone(), 1.0)),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    assert!(
        geo.vertex_count() > 0,
        "sanity: the label must draw something at all"
    );

    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let has_colour = |rgb: (u8, u8, u8)| {
        geo.verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    let expected = [
        ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
        ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
        ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
    ];
    let missing: Vec<&str> = expected
        .iter()
        .filter(|(_, rgb)| !has_colour(*rgb))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the same three-way name, but through the lossy
    // `held_item: Option<(String, f32)>` path — `styled_hover_name`'s
    // `Text::to_legacy_string`, which has no representation for
    // `TextColor::Rgb`. This must show the loss, or the assertion above
    // proves nothing about which field actually carries the colour.
    let flattened = root.resolve(&|_| None).to_legacy_string();
    let legacy_geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            held_item: Some((flattened, 1.0)),
            ..HudFrame::new(&stats)
        },
        640,
        480,
    );
    let legacy_byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (legacy_byte(v[2]), legacy_byte(v[3]), legacy_byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy-string path was expected to lose the hex colour \
         (that is the bug), but it drew it anyway — this test's premise is wrong"
    );
}

#[test]
fn health_pips_scale_with_value() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;
    frame.health = Some(0.0);
    let empty = HudGeometry::build(&frame, 640, 480);
    frame.health = Some(20.0);
    let full = HudGeometry::build(&frame, 640, 480);
    // Ten pips are always drawn (lit or dark), so the *count* is identical —
    // the gauge width reads regardless of value. This guards that a zero-HP
    // frame still renders the empty slots rather than nothing (which would
    // read as "no HUD" instead of "no health").
    assert_eq!(
        empty.vertex_count(),
        full.vertex_count(),
        "ten pip quads regardless of value"
    );
    assert_eq!(empty.vertex_count(), 10 * 6, "10 pips × 6 verts each");
    // …but the *colours* must differ: a full bar's lit pips can't share the
    // empty bar's dark colour, or the gauge would never actually read HP.
    assert_ne!(
        empty.verts, full.verts,
        "full vs empty must recolour the pips, not just redraw them"
    );
}

#[test]
fn regeneration_effect_projection_arms_only_the_wave() {
    let ordinary = crate::effects::HudEffectIcon {
        icon: "mob_effect/speed".to_owned(),
        background: crate::effects::HUD_EFFECT_BACKGROUND_SPRITE,
        alpha: 1.0,
        beneficial: true,
    };
    let regeneration = crate::effects::HudEffectIcon {
        icon: "mob_effect/regeneration".to_owned(),
        ..ordinary.clone()
    };
    assert!(!regeneration_active(Some(&[ordinary])));
    assert!(regeneration_active(Some(&[regeneration])));
    assert!(!regeneration_active(None));
}

#[test]
fn max_health_adds_a_second_heart_row_without_inferring_it_from_current_health() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;
    // A hurt Health-Boosted player is below 20 but still needs 20 heart
    // containers. This distinguishes attribute-driven row count from a
    // tempting `health > 20` branch.
    frame.health = Some(19.0);
    frame.max_health = Some(24.0);
    let boosted = HudGeometry::build(&frame, 640, 480);

    frame.max_health = Some(20.0);
    let ordinary = HudGeometry::build(&frame, 640, 480);
    assert_eq!(heart_rows(Some(24.0)), 2);
    assert_eq!(heart_rows(Some(20.0)), 1);
    assert_eq!(boosted.vertex_count() - ordinary.vertex_count(), 10 * 6);

    // Malformed data cannot create an arbitrary number of rows.
    assert_eq!(heart_rows(Some(f32::INFINITY)), 52);
}

/// The discriminating input for the two readings of "hide the hearts in
/// creative": vanilla's own is-survival check on its game-type enum is `SURVIVAL || ADVENTURE`, so **spectator**
/// is the value where the mode-naming hypothesis (`mode == Creative`) and the real
/// predicate disagree. Adventure is the second such value in the other direction.
#[test]
fn can_hurt_player_is_survival_and_not_creative_test() {
    use lodestone_model::GameMode;
    assert!(can_hurt_player(Some(GameMode::Survival)));
    // `isSurvival()` returns true for ADVENTURE too — an adventure-mode player is
    // hurtable and keeps the whole column.
    assert!(can_hurt_player(Some(GameMode::Adventure)));
    assert!(!can_hurt_player(Some(GameMode::Creative)));
    // The one that separates the hypotheses.
    assert!(!can_hurt_player(Some(GameMode::Spectator)));
    // Pre-connect / pre-login: the survival layout, matching `HudFrame::new`.
    assert!(can_hurt_player(None));
}

/// [`armour_icon`] against `Hud.extractArmor`'s three `if`s, at inputs where the
/// **wrong** reading gives a different answer.
///
/// The wrong reading is the one anybody would write from the screenshot rather
/// than the record: `full = ceil(armour / 2)` with a half only on an odd
/// remainder — or equivalently the off-by-one `i * 2 < armour`. It agrees with
/// the real predicate on **every even input**, so a gate at 8 or 20 measures that
/// the code runs. The discriminating inputs are odd, and every one below is
/// checked against both hypotheses: at 15 the truth is 7 full + 1 half + 2 empty
/// and the wrong reading says 8 full + 0 half + 2 empty.
///
/// Asserted as the full ten-icon **sequence**, not as counts, because counts
/// alone cannot see a half drawn at the wrong index — and mismatches are
/// collected rather than asserted inside the loop, so a neuter reports every arm
/// instead of the first.
#[test]
fn armour_icons_follow_extract_armor_at_odd_values() {
    use ArmourIcon::{Empty, Full, Half};
    // Hand-expanded from `if (i * 2 + 1 </==/> armor)`, one row per input.
    // `armour = 1` is the smallest drawn row; 30 is the registry's clamp ceiling
    // and must saturate at ten rather than grow.
    let cases: [(i32, [ArmourIcon; 10]); 6] = [
        (1, [Half, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty]),
        (7, [Full, Full, Full, Half, Empty, Empty, Empty, Empty, Empty, Empty]),
        (15, [Full, Full, Full, Full, Full, Full, Full, Half, Empty, Empty]),
        (19, [Full, Full, Full, Full, Full, Full, Full, Full, Full, Half]),
        (20, [Full; 10]),
        (30, [Full; 10]),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (armour, expected) in cases {
        let got: Vec<ArmourIcon> = (0..10).map(|i| armour_icon(i, armour)).collect();
        if got != expected {
            mismatches.push(format!("armour {armour}: expected {expected:?}, got {got:?}"));
        }
        // The wrong hypothesis, evaluated at the same input so the test records
        // that the two really do differ here rather than asserting they do.
        let wrong: Vec<ArmourIcon> = (0..10)
            .map(|i| {
                if (i as i32) * 2 < armour {
                    Full
                } else {
                    Empty
                }
            })
            .collect();
        if armour % 2 == 1 && wrong == expected {
            mismatches.push(format!(
                "armour {armour} is not a discriminating input: the ceil()/off-by-one \
                 reading gives the same ten icons, so this row measures only that \
                 the function runs"
            ));
        }
    }
    // Zero draws nothing at the call site (`armour > 0`), but the predicate itself
    // must still be total — an all-empty row, never a panic or a stray half.
    if (0..10).map(|i| armour_icon(i, 0)).any(|c| c != Empty) {
        mismatches.push("armour 0 must be ten empty icons".to_string());
    }
    assert!(
        mismatches.is_empty(),
        "armour icon selection diverges from Hud.extractArmor:\n  {}",
        mismatches.join("\n  ")
    );
}

/// [`heart_fill`] against `Hud.extractHearts`, at the **half** healths where the
/// `Mth.ceil` reading and the float reading give different sprites.
///
/// Live player report: *"sometimes i get to 0 hearts but im still alive - im
/// assuming vanilla maybe rounds up while we just round either way"*. He was
/// right, and 0.5 is the input that proves it — but it is not the only one, which
/// is why this drives four healths rather than that one. Vanilla ceils **up**, so
/// the divergence runs in both directions: 0.5 gains a half heart the float
/// reading never drew, while 1.5 and 19.5 promote a *half* to a **full**.
///
/// Every expectation below is hand-expanded from `currentHealth = Mth.ceil(health)`
/// and `halves < currentHealth` / `halves + 1 == currentHealth`, and the float
/// reading this replaced is evaluated at the same input so each row *records* that
/// the two really differ instead of asserting they do. The two integer healths are
/// deliberately included as the coincident controls: they must agree, and a gate
/// written at 1.0 or 20.0 alone would measure only that the function runs.
///
/// Asserted as the ten-sprite **sequence** and collected rather than asserted
/// inside the loop, so a neuter reports every arm instead of the first — a count
/// of filled hearts cannot see 19.5, where both readings draw ten sprites and only
/// the tenth one's identity differs.
#[test]
fn heart_fill_follows_extract_hearts_at_half_healths() {
    use HeartFill::{Full, Half};
    // `None` is a container with nothing over it. Rows hand-expanded from the
    // record, not from this module.
    let cases: [(f32, [Option<HeartFill>; 10]); 6] = [
        // Dead: the only health at which the bar is legitimately empty.
        (0.0, [None; 10]),
        // The report. `ceil(0.5) == 1`, so `halves + 1 == 1` at i = 0: a half.
        (0.5, [Some(Half), None, None, None, None, None, None, None, None, None]),
        // Coincident control — both readings say one half heart.
        (1.0, [Some(Half), None, None, None, None, None, None, None, None, None]),
        // `ceil(1.5) == 2`, so i = 0 is `halves + 1 == 1 != 2`: a **full** heart.
        (1.5, [Some(Full), None, None, None, None, None, None, None, None, None]),
        // The top of the bar. `ceil(19.5) == 20`: ten full, no half at i = 9.
        (19.5, [Some(Full); 10]),
        // Coincident control at the top.
        (20.0, [Some(Full); 10]),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (health, expected) in cases {
        let got: Vec<Option<HeartFill>> = (0..10).map(|i| heart_fill(i, health)).collect();
        if got != expected {
            mismatches.push(format!("health {health}: expected {expected:?}, got {got:?}"));
        }
        // The reading this replaced, evaluated here so a row that stops
        // discriminating says so rather than passing quietly.
        let float_reading: Vec<Option<HeartFill>> = (0..10)
            .map(|i| {
                let units = health.max(0.0) - i as f32 * 2.0;
                if units >= 2.0 {
                    Some(Full)
                } else if units >= 1.0 {
                    Some(Half)
                } else {
                    None
                }
            })
            .collect();
        let half_health = (health.fract() - 0.5).abs() < 1e-6;
        if half_health && float_reading == expected {
            mismatches.push(format!(
                "health {health} is not a discriminating input: the float \
                 `health - 2i` reading gives the same ten sprites, so this row \
                 measures only that the function runs"
            ));
        }
        if !half_health && float_reading != expected {
            mismatches.push(format!(
                "health {health} was chosen as a coincident control but the two \
                 readings disagree ({float_reading:?} vs {expected:?}) — the \
                 control's premise is false"
            ));
        }
    }
    // A negative health (hurt overshoot) must read as dead, not ceil to a heart.
    if heart_fill(0, -0.5).is_some() {
        mismatches.push("a negative health must fill no heart".to_string());
    }
    assert!(
        mismatches.is_empty(),
        "heart fill diverges from Hud.extractHearts:\n  {}",
        mismatches.join("\n  ")
    );
}

/// The armour row is gated by the **same** `canHurtPlayer()` flag as the hearts,
/// hunger and bubble rows — vanilla reaches all four through one
/// `extractPlayerHealth` call — and by vanilla's own `armor > 0`, which draws
/// **no** row rather than ten empty icons.
///
/// Counts are predicted from `Builder::pips`' own shape, which
/// `health_pips_scale_with_value` independently pins at ten quads of six vertices:
/// so one armour row is `10 * 6 = 60` vertices on top of an otherwise identical
/// frame. The three arms that must be identical to the baseline are the ones a
/// direction-only assertion would miss — `Some(0)` in particular, where the
/// tempting "draw the empty backing anyway" reading adds 60 and vanilla adds 0.
#[test]
fn the_armour_row_costs_one_pip_row_and_only_when_worn() {
    let stats = DebugStats::default();
    let build = |can_hurt: bool, armour: Option<i32>| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.health = Some(20.0);
        frame.food = Some(20);
        frame.armour = armour;
        HudGeometry::build(&frame, 640, 480).vertex_count()
    };
    let baseline = build(true, None);
    let mut mismatches: Vec<String> = Vec::new();
    // `None` (never wired), `Some(0)` (live, wearing nothing) and creative all
    // draw exactly the baseline; a worn value adds one row and nothing else.
    for (label, can_hurt, armour, expected) in [
        ("not wired", true, None, baseline),
        ("live, unarmoured", true, Some(0), baseline),
        ("full diamond", true, Some(20), baseline + 60),
        ("half icon at 15", true, Some(15), baseline + 60),
        ("creative, armoured", false, Some(20), build(false, None)),
    ] {
        let got = build(can_hurt, armour);
        if got != expected {
            mismatches.push(format!(
                "{label}: expected {expected} vertices, got {got} (baseline {baseline})"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "armour row wiring diverges:\n  {}",
        mismatches.join("\n  ")
    );
}

/// Vanilla's gate is `canHurtPlayer()` — `SURVIVAL || ADVENTURE` — and one call
/// to `extractPlayerHealth` behind it draws hearts, hunger and the bubble row,
/// while `hasExperience()` (the same body) gates the XP bar.
///
/// The counts here are predicted, not observed: on the procedural branch (no GUI
/// atlas attached) `Builder::pips` emits exactly ten quads per row at six
/// vertices each, which `health_pips_scale_with_value` above independently pins at
/// `10 * 6`. So a survival frame carrying both rows is `2 * 10 * 6` and a creative
/// frame is `0`. The wrong hypothesis — "gate on `GameMode::Creative`" — would
/// leave the spectator arm at 120, so the spectator case below is the one that
/// separates the two readings; a creative-only test passes under either.
#[test]
fn creative_and_spectator_hide_the_whole_vitals_column() {
    let stats = DebugStats::default();
    // `HudFrame` is not `Copy` (it carries owned strings), so each arm builds its
    // own rather than cloning one.
    let build = |can_hurt: bool, vitals: bool, hotbar: Option<usize>| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.xp = Some((7, 0.5));
        frame.hotbar = hotbar;
        if vitals {
            frame.health = Some(20.0);
            frame.food = Some(20);
        }
        HudGeometry::build(&frame, 640, 480).vertex_count()
    };

    // Survival / adventure: two pip rows on top of whatever the XP bar itself
    // costs — asserted by *subtracting* an XP-only frame rather than by predicting
    // the bar's own vertex budget, which is not what this test is about.
    assert_eq!(
        build(true, true, None) - build(true, false, None),
        2 * 10 * 6,
        "ten health pips and ten hunger pips, six vertices each"
    );

    // `can_hurt_player == false` must take the XP bar and its level number with
    // it, not just the two pip rows — so the whole cluster is gone, not merely
    // shortened. With no hotbar, zero is the honest total.
    assert_eq!(
        build(false, true, None),
        0,
        "no hearts, no hunger, no XP bar when the player cannot be hurt"
    );

    // The control: a `0` above would also be what a frame drawing nothing at all
    // reports, so prove the hotbar — which vanilla draws in every game mode, and
    // which this gate must not touch — still lands.
    assert!(
        build(false, true, Some(0)) > 0,
        "the hotbar is not behind canHurtPlayer(); vanilla draws it in creative"
    );
}

/// `extractSelectedItemName` places the held-item label at `guiHeight - 59`, then
/// `y += 14` when `!canHurtPlayer()`. Predicted as an exact delta rather than
/// "it moved": at 480 px tall with GUI scale forced to 1 the logical canvas is the
/// physical one, so the two y values are `421` and `435`.
#[test]
fn the_held_item_label_drops_exactly_fourteen_pixels_in_creative() {
    let stats = DebugStats::default();
    let lowest_y = |can_hurt: bool| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.held_item = Some(("Diamond Sword".to_string(), 1.0));
        HudGeometry::build(&frame, 640, 480)
            .verts
            .chunks(FLOATS_PER_VERTEX)
            .map(|v| v[1])
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let survival = lowest_y(true);
    let creative = lowest_y(false);
    // `verts` are clip space, y **up**, so a label lower on screen has the smaller
    // value. The expected delta is derived from the same `logical_canvas` the draw
    // lays out in rather than from a hardcoded scale: 14 logical px over a canvas
    // `h` tall spans `2 * 14 / h` of the `-1..=1` range.
    let (_, canvas_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, 640, 480);
    let expected = 2.0 * 14.0 / canvas_h;
    assert!(
        (survival - creative - expected).abs() < 1e-4,
        "expected a 14px drop ({expected} in clip space), got {survival} -> {creative}"
    );
}

#[test]
fn hotbar_draws_and_selection_moves_the_highlight() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;

    // No hotbar → no hotbar geometry.
    frame.hotbar = None;
    let none = HudGeometry::build(&frame, 640, 480).vertex_count();

    // A hotbar adds a real run of geometry (panel + 9 cells + a 4-edge ring).
    frame.hotbar = Some(0);
    let sel0 = HudGeometry::build(&frame, 640, 480);
    assert!(
        sel0.vertex_count() > none,
        "an on-screen hotbar must add geometry, got {} vs {none}",
        sel0.vertex_count()
    );

    // Moving the selection keeps the vertex *count* identical (same panel,
    // 9 cells, 4-edge ring) but must move the ring — so the bytes differ. A
    // selection that never relocates the highlight would render as a hotbar
    // that ignores the held slot.
    frame.hotbar = Some(4);
    let sel4 = HudGeometry::build(&frame, 640, 480);
    assert_eq!(
        sel0.vertex_count(),
        sel4.vertex_count(),
        "selecting a different slot must not change the vertex count"
    );
    assert_ne!(
        sel0.verts, sel4.verts,
        "the selection ring must move to the newly-selected slot"
    );
}

/// A view of `n` players called `P0..P{n-1}`, all survival, all full bars.
fn tab_view(n: usize) -> crate::tablist::TabListView {
    crate::tablist::TabListView {
        rows: (0..n)
            .map(|i| crate::tablist::TabListRow {
                name: crate::overlay::plain_spans(format!("P{i}")),
                ping_sprite: "icon/ping_5",
                spectator: false,
            })
            .collect(),
        header: Vec::new(),
        footer: Vec::new(),
        show_head: false,
    }
}

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
    let panel = |slots: usize| TabPanel::new(640.0, slots, false, 40.0, 0, 0.0);
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
    let panel = TabPanel::new(640.0, 21, false, 40.0, 0, 0.0);
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
    let bare = TabPanel::new(640.0, 3, false, 40.0, 0, 0.0);
    assert_eq!(bare.rows_top, 10.0);
    let with_header = TabPanel::new(640.0, 3, false, 40.0, 2, 0.0);
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
    let bare = TabPanel::new(640.0, 3, false, 40.0, 0, 0.0);
    let narrow = TabPanel::new(640.0, 3, false, 40.0, 1, 4.0);
    assert_eq!(narrow.max_line_width, bare.max_line_width);
    let wide = TabPanel::new(640.0, 3, false, 40.0, 1, bare.max_line_width + 60.0);
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

/// Encode a solid-colour RGBA PNG so a `MemorySource` can stand in for a
/// real jar in a hermetic test — the same trick
/// `lodestone_render::gui_atlas`'s own tests use (no GPU, no disk).
fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| rgba).collect();
        writer.write_image_data(&pixels).expect("png data");
    }
    data
}

/// A minimal synthetic pack covering the boss-bar sprite ids exercised
/// below: two colours' background/progress plates plus one notch-overlay
/// pair, at vanilla's real 182×5 native size
/// (`.cache/mc/26.2/client-src/assets/.../gui/sprites/boss_bar/*.png`).
/// Content is an arbitrary flat colour per id — this is a **geometry**
/// gate (does the draw reach the atlas and land the right rect?), not a
/// pixel-colour gate, so what matters is that each id is *present* and
/// distinct, not what it looks like.
fn boss_bar_synthetic_atlas() -> GuiAtlas {
    let mut src = lodestone_assets::MemorySource::new("boss-bar-test");
    for (id, rgba) in [
        ("boss_bar/purple_background", [60, 20, 90, 255]),
        ("boss_bar/purple_progress", [170, 60, 220, 255]),
        ("boss_bar/red_background", [90, 20, 20, 255]),
        ("boss_bar/red_progress", [220, 40, 40, 255]),
        ("boss_bar/notched_6_background", [10, 10, 10, 255]),
        ("boss_bar/notched_6_progress", [250, 250, 250, 255]),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(182, 5, rgba),
        );
    }
    let manager = lodestone_assets::ResourceManager::new(vec![
        Box::new(src) as Box<dyn lodestone_assets::ResourceSource>
    ]);
    GuiAtlas::build(&manager).expect("synthetic boss-bar atlas must build")
}

/// The bounding box (logical pixels) of each 6-vertex (two-triangle)
/// quad in `verts`, in emission order. `push_sprite_quad` always emits
/// exactly one quad per call, so grouping by six vertices recovers the
/// draw's own call order — one entry per `b.sprite`/`b.push_sprite_quad`
/// invocation.
fn quad_boxes(verts: &[f32], cw: f32, ch: f32) -> Vec<(f32, f32, f32, f32)> {
    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    verts
        .chunks(SPRITE_FLOATS_PER_VERTEX * 6)
        .map(|quad| {
            let mut x0 = f32::MAX;
            let mut y0 = f32::MAX;
            let mut x1 = f32::MIN;
            let mut y1 = f32::MIN;
            for v in quad.chunks(SPRITE_FLOATS_PER_VERTEX) {
                let (x, y) = (px(v[0]), py(v[1]));
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            (x0, y0, x1, y1)
        })
        .collect()
}

/// The boss bar's four vanilla clauses (`BossHealthOverlay.extractBar`,
/// `.cache/mc/26.2/client-src`), each pinned to the layer that actually
/// emits geometry (`HudGeometry::sprite_verts`, via a real
/// [`GuiAtlas`]) rather than to [`crate::overlay::BossBarView`]'s model —
/// the gap this whole fix closes was that every existing gate stopped one
/// layer above this and a flat `rect_px` reached the screen instead.
///
/// 1. background plate, full 182px, drawn even at zero progress
/// 2. background notch overlay, full 182px, only when the overlay style
///    is not `Progress`
/// 3. progress fill, **cropped** (not scaled) to
///    [`crate::overlay::lerp_discrete_width`] — checked at a half-full
///    bar (the brief's own discriminating case) and at `0.2`, which the
///    naive `round(progress * 182)` hypothesis gets wrong (36px, not 37)
/// 4. progress notch overlay, cropped the same way, only when the
///    overlay style is not `Progress`
#[test]
fn boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model() {
    use crate::overlay::{BossBarView, lerp_discrete_width};
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

    let atlas = boss_bar_synthetic_atlas();
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
    let yo = BOSS_BAR_TOP;

    let render = |progress: f32, overlay: BossBarOverlay| -> Vec<(f32, f32, f32, f32)> {
        let bars = [BossBarView {
            title: crate::overlay::plain_spans("Ender Dragon"),
            progress,
            color: BossBarColor::Purple,
            overlay,
        }];
        let frame = HudFrame {
            boss_bars: &bars,
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
        quad_boxes(&geo.sprite_verts, cw, ch)
    };

    let mut wrong = Vec::new();
    let mut check = |name: String, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            wrong.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };

    // -- clause 1: background, full width, drawn even at zero progress,
    // and clauses 3/4 correctly absent (no fill, overlay is Progress).
    let empty = render(0.0, BossBarOverlay::Progress);
    assert_eq!(
        empty.len(),
        1,
        "progress 0.0 with the Progress overlay must draw only the \
         background plate: got {empty:?}"
    );
    check("empty bg x0".into(), empty[0].0, bar_x);
    check("empty bg x1".into(), empty[0].2, bar_x + BOSS_BAR_WIDTH);
    check("empty bg y0".into(), empty[0].1, yo);
    check("empty bg y1".into(), empty[0].3, yo + BOSS_BAR_HEIGHT);

    // -- clause 3 at full progress: the fill spans the whole 182px too.
    let full = render(1.0, BossBarOverlay::Progress);
    assert_eq!(full.len(), 2, "full progress must draw background + fill: got {full:?}");
    check("full bg x0".into(), full[0].0, bar_x);
    check("full bg x1".into(), full[0].2, bar_x + BOSS_BAR_WIDTH);
    check("full fill x0".into(), full[1].0, bar_x);
    check("full fill x1".into(), full[1].2, bar_x + BOSS_BAR_WIDTH);

    // -- clause 3, the discriminating cases: a half-full bar's fill must
    // cover the *predicted partial* width — not zero, not full, and (at
    // 0.2) not the naive `progress * 182` scale either.
    for (progress, want_px) in [(0.5_f32, 91_i32), (0.2_f32, 37_i32)] {
        let quads = render(progress, BossBarOverlay::Progress);
        assert_eq!(
            quads.len(),
            2,
            "progress {progress} must draw background + fill: got {quads:?}"
        );
        let predicted = lerp_discrete_width(progress, BOSS_BAR_WIDTH as i32);
        assert_eq!(predicted, want_px, "lerp_discrete_width regressed for {progress}");
        let fill = quads[1];
        check(format!("progress {progress} fill x0"), fill.0, bar_x);
        check(format!("progress {progress} fill x1"), fill.2, bar_x + want_px as f32);
        check(format!("progress {progress} fill y0"), fill.1, yo);
        check(format!("progress {progress} fill y1"), fill.3, yo + BOSS_BAR_HEIGHT);
    }

    // -- clauses 2 + 4: the notch overlay draws only when the overlay
    // style is not `Progress`, doubling the quad count at both ends.
    let notched_empty = render(0.0, BossBarOverlay::Notched6);
    assert_eq!(
        notched_empty.len(),
        2,
        "zero progress with a notch overlay must draw background + \
         background-notch, no fill: got {notched_empty:?}"
    );
    let notched_full = render(1.0, BossBarOverlay::Notched6);
    assert_eq!(
        notched_full.len(),
        4,
        "full progress with a notch overlay must draw all four clauses: \
         got {notched_full:?}"
    );

    assert!(wrong.is_empty(), "{wrong:?}");
}

/// A minimal synthetic pack covering the locator bar's two sprite ids —
/// same "geometry gate, not a pixel-colour gate" shape as
/// [`boss_bar_synthetic_atlas`], and at the bar's real native sizes:
/// 182x5 for the background, 9x9 for the dot (`LocatorBar::DOT_SIZE`).
fn locator_bar_synthetic_atlas() -> GuiAtlas {
    let mut src = lodestone_assets::MemorySource::new("locator-bar-test");
    for (id, size, rgba) in [
        ("hud/locator_bar_background", (182, 5), [40, 40, 40, 255]),
        (locator::DEFAULT_DOT_SPRITE, (9, 9), [255, 255, 255, 255]),
        // The XP bar's own sprites, needed only for this test's
        // mutual-exclusion control (`xp_only` below) — `b.sprite`
        // silently draws nothing for a missing id, so without these the
        // control would "pass" by drawing nothing for the wrong reason.
        ("hud/experience_bar_background", (182, 5), [20, 90, 20, 255]),
        ("hud/experience_bar_progress", (182, 5), [40, 200, 40, 255]),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(size.0, size.1, rgba),
        );
    }
    let manager = lodestone_assets::ResourceManager::new(vec![
        Box::new(src) as Box<dyn lodestone_assets::ResourceSource>
    ]);
    GuiAtlas::build(&manager).expect("synthetic locator-bar atlas must build")
}

/// The locator bar's dots reach the sprite-geometry layer at their
/// predicted screen positions — the same class of gap
/// [`boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`]
/// closed for the boss bar, and the one `CLAUDE.md`'s `draw_tab`
/// incident names generally: a corpus that only ever asserts on the
/// *model* (here, [`locator::LocatorDot`]) cannot see a draw site that
/// never calls `b.sprite` at all. This one calls `HudGeometry::build_with_gui`
/// and reads the actual emitted quads.
///
/// Also the mutual-exclusion contract `sprite_vitals`'s own doc names:
/// a non-empty `locator` must draw *instead of* the XP bar, not
/// alongside it, even when `frame.xp` is `Some` — `ContextualBar` is one
/// slot in vanilla, never two bars stacked.
#[test]
fn locator_bar_reaches_the_sprite_geometry_layer_and_outranks_the_xp_bar() {
    let atlas = locator_bar_synthetic_atlas();
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    // `ContextualBar::left`/`top`, the same expressions `sprite_vitals`
    // derives `hx`/`bar_top` from — not restated as an independent
    // constant, so a change to the hotbar's own layout moves this
    // prediction with it instead of silently drifting from it.
    let bar_x = (cw - 182.0) / 2.0;
    let bar_y = ch - 22.0 - 5.0 - 2.0;
    let dot = locator::dot_size() as f32;
    let screen_middle = ((cw - dot) / 2.0).ceil();

    let render = |dots: &[locator::LocatorDot], xp: Option<(i32, f32)>| -> Vec<(f32, f32, f32, f32)> {
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            can_hurt_player: true,
            xp,
            locator: dots,
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
        quad_boxes(&geo.sprite_verts, cw, ch)
    };

    // -- No waypoints, XP present: only the XP bar draws (the pre-existing
    //    behaviour every XP gate elsewhere in this file already covers;
    //    this call is the mutual-exclusion control, not new coverage).
    let xp_only = render(&[], Some((3, 0.4)));
    assert!(
        !xp_only.is_empty(),
        "control: the XP bar must still draw when there are no waypoints"
    );

    // -- Two waypoints, no XP: exactly background + two dots, at the
    //    predicted screen rects.
    let dots = [
        locator::LocatorDot { offset: -40, color: [1.0, 0.0, 0.0, 1.0] },
        locator::LocatorDot { offset: 25, color: [0.0, 1.0, 0.0, 1.0] },
    ];
    let quads = render(&dots, None);
    assert_eq!(
        quads.len(),
        3,
        "two waypoints must draw background + two dots, no more, no fewer: got {quads:?}"
    );
    let check = |name: &str, got: f32, want: f32| {
        assert!(
            (got - want).abs() < 0.5,
            "{name}: got {got:.2}, want {want:.2} (all quads: {quads:?})"
        );
    };
    check("background x0", quads[0].0, bar_x);
    check("background x1", quads[0].2, bar_x + 182.0);
    check("background y0", quads[0].1, bar_y);
    check("background y1", quads[0].3, bar_y + 5.0);
    check("dot[0] (-40) x0", quads[1].0, screen_middle - 40.0);
    check("dot[0] (-40) x1", quads[1].2, screen_middle - 40.0 + dot);
    check("dot[1] (+25) x0", quads[2].0, screen_middle + 25.0);
    check("dot[1] (+25) x1", quads[2].2, screen_middle + 25.0 + dot);

    // -- Both present: locator wins outright — this is the assertion
    //    that fails if the mutual-exclusion `if`/`else if` in
    //    `sprite_vitals` is ever loosened back into two independent
    //    conditions, which would draw both bars stacked on one slot.
    let both = render(&dots, Some((3, 0.4)));
    assert_eq!(
        both.len(),
        3,
        "with waypoints present the XP bar must not draw at all, even though frame.xp is \
         Some: got {both:?}"
    );
}

#[test]
fn one_line_is_stable() {
    let stats = DebugStats {
        position: [0.5, 40.0, -3.5],
        fps: 60.0,
        frame_ms: 16.6,
        ..Default::default()
    };
    let line = stats.one_line();
    assert!(line.contains("fps=60"));
    assert!(line.contains("frame=16.60ms"));
}

/// Clear `view` to an opaque `rgb` background (Rgba8Unorm is linear, so the
/// byte value lands verbatim). Used to give the HUD's `Load` pass a known
/// backdrop for pixel readback.
#[cfg(test)]
fn clear_view(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    view: &wgpu::TextureView,
    rgb: [u8; 3],
) {
    let color = wgpu::Color {
        r: f64::from(rgb[0]) / 255.0,
        g: f64::from(rgb[1]) / 255.0,
        b: f64::from(rgb[2]) / 255.0,
        a: 1.0,
    };
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit(std::iter::once(encoder.finish()));
}

/// Pixel-readback proof that server chat *reaches pixels*, not merely that a
/// frame counter ticks. Renders the HUD (chat only — no crosshair, overlay,
/// hotbar or vitals) over a known grey backdrop and inspects the bottom-left
/// chat region. The discriminator is luminance: the translucent backing
/// panel is *darker* than the grey background, the near-white glyphs are
/// *brighter* — so text and panel are counted separately and a blank-but-
/// panelled line cannot masquerade as rendered text.
///
/// Three frames make the assertion two-sided:
/// * no message → the region is untouched background (zero of both);
/// * a whitespace-only line → the panel draws (dark pixels) but no glyphs;
/// * a real line → glyphs add bright pixels the panel-only frame lacks.
#[test]
#[ignore = "requires a GPU adapter"]
fn shared_world_hud_encoder_preserves_pixels_and_submission_counts() {
    use crate::gpu::gpu_timing::{
        PrimaryCommandCounts, primary_encoder, take_primary_command_counts,
    };
    use crate::gpu::{RenderState, ScreenEffects};
    use crate::mesher::{SectionGeometry, SectionKey, mesh_snapshot, snapshot_section};
    use lodestone_render::{Camera, HeadlessTarget, RenderTarget};

    let context = lodestone_render::GpuContext::new_headless_blocking().expect("GPU adapter");
    let (device, queue) = (context.device(), context.queue());
    let (width, height) = (480, 320);
    let world = crate::worldgen::generate(1);
    let feet = crate::worldgen::spawn_feet();
    let camera = Camera {
        position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
        yaw: 0.0,
        pitch: 15.0,
        fov_y_degrees: 70.0,
        aspect: width as f32 / height as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };
    let stats = DebugStats::default();
    for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
        let mut target = HeadlessTarget::new(device, width, height, format);
        let mut render = RenderState::new(device, queue, format, width, height, None);
        for cz in -1..=1 {
            for cx in -1..=1 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey { cx, cz, si, min_y: crate::worldgen::MIN_Y };
                    let Some(snapshot) = snapshot_section(&world, key) else { continue };
                    let mesh = mesh_snapshot(&snapshot, &crate::blocks::DemoClassifier);
                    if !mesh.indices.is_empty() {
                        render.upload_section(device, queue, key, &SectionGeometry::Packed(mesh));
                    }
                }
            }
        }
        let mut draw = |merged: bool, hidden: bool| {
            let frame = target.acquire().expect("headless frame");
            let mut hud = HudRenderer::new(device, target.raw_view_format());
            let raw_view = hud.flat_colour_view(&frame);
            let chat = [("shared frame control", 0.0)];
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: !hidden,
                chat: if hidden { &[] } else { &chat },
                ..HudFrame::new(&stats)
            };
            let _ = take_primary_command_counts();
            let world_stats = if merged {
                let mut encoder = primary_encoder(device, "world-hud-control");
                let result = render.encode_with_crack_and_effects(
                    device, queue, frame.view(), &camera, None, &[], &[],
                    ScreenEffects::default(), &mut encoder,
                );
                hud.encode_with_item_models(
                    device, queue, frame.view(), &raw_view, Some(render.depth_view()),
                    &hud_frame, None, 1, width, height, &mut encoder,
                );
                render.submit_encoded_frame(queue, encoder);
                result
            } else {
                let result = render.render(device, queue, frame.view(), &camera, None, &[]);
                hud.render_with_item_models(
                    device, queue, frame.view(), &raw_view, Some(render.depth_view()),
                    &hud_frame, None, 1, width, height,
                );
                result
            };
            assert!(world_stats.sections_drawn > 0, "fixture must draw terrain");
            let counts = take_primary_command_counts();
            (target.read_texels(device, queue), counts)
        };
        let (separate, separate_counts) = draw(false, false);
        let (shared, shared_counts) = draw(true, false);
        let (empty_separate, empty_separate_counts) = draw(false, true);
        let (empty_shared, empty_shared_counts) = draw(true, true);
        assert_eq!(separate_counts, PrimaryCommandCounts { created: 2, finished: 2, submitted: 2 });
        let one = PrimaryCommandCounts { created: 1, finished: 1, submitted: 1 };
        assert_eq!(shared_counts, one);
        assert_eq!(empty_separate_counts, one);
        assert_eq!(empty_shared_counts, one);
        for (expected, actual) in [(&separate, &shared), (&empty_separate, &empty_shared)] {
            let mut bounds = None::<[usize; 4]>;
            for (index, (left, right)) in expected.chunks_exact(4).zip(actual.chunks_exact(4)).enumerate() {
                if left != right {
                    let (x, y) = (index % width as usize, index / width as usize);
                    bounds = Some(bounds.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]));
                }
            }
            assert!(bounds.is_none(), "{format:?}: changed pixels at {bounds:?}");
        }
        assert!(shared != empty_shared, "control must detect omitted HUD pixels");
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn chat_text_reaches_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // Count bright (glyph) and dark (panel) pixels in the bottom-left chat
    // region, well clear of the bottom-centre hotbar/vitals (which are off
    // anyway) and the top-left debug overlay.
    let x_max = (w as f32 * 0.55) as u32;
    let y_min = (h as f32 * 0.60) as u32;

    let mut render = |chat: &[(&str, f32)]| -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            chat,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let (mut bright, mut dark) = (0usize, 0usize);
        for y in y_min..h {
            for x in 0..x_max {
                let i = ((y * w + x) * 4) as usize;
                let avg = (u32::from(pixels[i])
                    + u32::from(pixels[i + 1])
                    + u32::from(pixels[i + 2]))
                    / 3;
                if avg > u32::from(BG) + 30 {
                    bright += 1;
                } else if avg + 30 < u32::from(BG) {
                    dark += 1;
                }
            }
        }
        (bright, dark)
    };

    let (blank_bright, blank_dark) = render(&[]);
    let (panel_bright, panel_dark) = render(&[(" ", 0.0)]);
    let (text_bright, text_dark) = render(&[("chat works", 0.0)]);

    eprintln!("=== chat readback (headless) ===");
    eprintln!("blank  bright={blank_bright} dark={blank_dark}");
    eprintln!("panel  bright={panel_bright} dark={panel_dark}");
    eprintln!("text   bright={text_bright} dark={text_dark}");

    // No message: pure background — neither panel nor glyphs.
    assert_eq!(
        (blank_bright, blank_dark),
        (0, 0),
        "with no chat, the chat region must be untouched background"
    );
    // A line draws its translucent backing panel (dark) but a space has no
    // glyphs, so almost no bright pixels.
    assert!(panel_dark > 0, "a chat line must draw its backing panel");
    assert!(
        panel_bright < 50,
        "a whitespace-only line must not paint glyph pixels, got {panel_bright}"
    );
    // The glyphs of a real line add bright pixels the panel-only frame lacks
    // — this is the assertion that fails if the text were blank.
    assert!(
        text_dark > 0,
        "the text line must also draw its backing panel"
    );
    assert!(
        text_bright > panel_bright + 150,
        "chat glyphs must reach pixels over the bare panel: text_bright={text_bright}, \
         panel_bright={panel_bright}"
    );
}

/// The **wiring** half of the tab-list gamma fix, at production's own
/// surface format.
///
/// `gpu::pixel_gates`' `hud_flat_colour_blend_matches_vanilla_gamma_on_a_raw_target`
/// already measured that `hud.wgsl` reproduces vanilla's raw-gamma blend
/// when it is given a non-sRGB attachment — but it builds the pipeline by
/// hand, so it proves the *shader*, not that `HudRenderer` and its callers
/// pair a pipeline with a matching view. That pairing is what was actually
/// broken (and what a previous attempt got wrong in the other direction),
/// and it lives in two files, so it needs its own subject.
///
/// # The fixture, and why it is predictable to the byte
///
/// A **black** backdrop and rows whose names are empty spans. The overlay
/// then paints exactly two things: `TAB_PLATE` (black at alpha 128), which
/// over black is a fixed point of both blend models and contributes
/// nothing, and `TAB_ROW_FILL` (white at alpha 32). With no GUI atlas
/// attached the ping sprites draw nothing and with empty names no glyph
/// does either, so **every non-zero byte in the frame is the row fill** and
/// the frame maximum is that one composite — no rect arithmetic, and a zero
/// maximum is a failed premise rather than a silent pass.
///
/// Raw-byte alpha compositing is plain interpolation, so the subject is
/// predicted exactly: `0 * (1 - 32/255) + 255 * (32/255)` = **32**. The
/// control is only *bracketed* — this codebase has measured real sRGB
/// `ALPHA_BLENDING` as a non-trivial function of the fragment alpha that
/// resists a closed form on Metal — but it must land far away, and the
/// recorded sweep puts it near 99.
///
/// # The format
///
/// `Bgra8UnormSrgb`, which is what native `wgpu-core`'s
/// `Surface::get_default_config` actually picks — so the two formats this
/// gate pairs are the pair production pairs, not a headless-only
/// `Rgba8Unorm` where `format()` and `raw_view_format()` coincide and the
/// whole question is vacuous. That non-coincidence is asserted rather than
/// assumed.
///
/// **What this does not prove.** The target is a `HeadlessTarget`, not a
/// swapchain. That `SurfaceTarget` reports the same format pair, and
/// declares both in `view_formats`, is `lodestone_render::target`'s claim,
/// not this gate's.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_flat_colour_pass_blends_on_gamma_bytes_at_the_surface_format() {
    use crate::tablist::{TabListRow, TabListView};
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Native's own swapchain format, so `format()` and `raw_view_format()`
    // genuinely differ and the comparison below has something to compare.
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    assert_ne!(
        target.format(),
        target.raw_view_format(),
        "this gate is vacuous unless the corrected and raw formats differ — pick a \
         format whose sRGB and non-sRGB siblings are distinct"
    );

    let stats = DebugStats::default();
    // Two rows, no names, no banner: the row fill and nothing else. `spectator`
    // only chooses an ink colour, and there is no ink to colour.
    let view = TabListView {
        rows: vec![
            TabListRow { name: Vec::new(), ping_sprite: "", spectator: false },
            TabListRow { name: Vec::new(), ping_sprite: "", spectator: false },
        ],
        header: Vec::new(),
        footer: Vec::new(),
        show_head: false,
    };

    // `wiring` picks which `(pipeline format, attachment view)` pair the
    // flat-colour pass gets. `Correct` is production's; `Corrected` is the
    // pairing production had before this fix, kept as the control that must
    // land on the other hypothesis rather than merely "somewhere else".
    let mut shoot = |raw: bool| -> (u8, usize) {
        let flat_format = if raw {
            target.raw_view_format()
        } else {
            target.format()
        };
        let mut hud = HudRenderer::new(device, flat_format);
        assert_eq!(hud.flat_colour_format(), flat_format);
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        let attachment = if raw {
            hud.flat_colour_view(&frame)
        } else {
            frame.create_view(target.format())
        };
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            players: Some(&view),
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), &attachment, &hud_frame, w, h);
        drop(frame);
        let pixels = target.read_texels(device, queue);
        let mut max = 0u8;
        let mut lit = 0usize;
        for px in pixels.chunks_exact(4) {
            // Channel order is BGRA here; the fill is white over black, so
            // every colour channel carries the same value and the max is
            // order-independent.
            let v = px[0].max(px[1]).max(px[2]);
            max = max.max(v);
            if v > 0 {
                lit += 1;
            }
        }
        (max, lit)
    };

    let (raw_max, raw_lit) = shoot(true);
    let (srgb_max, srgb_lit) = shoot(false);

    // `0x20FFFFFF` over black, composited on raw gamma bytes — the whole
    // claim, derived from the constants rather than restated.
    let alpha = f32::from(0x20u8) / 255.0;
    let predicted = (255.0 * alpha).round() as i32;

    eprintln!("=== hud flat-colour wiring at Bgra8UnormSrgb ===");
    eprintln!("predicted vanilla gamma byte = {predicted}");
    eprintln!("raw-view  max = {raw_max}  lit = {raw_lit}");
    eprintln!("srgb-view max = {srgb_max}  lit = {srgb_lit}");

    // Premise: the overlay drew at all. A zero here means the fixture
    // produced no row fill and both arms below would agree vacuously.
    assert!(
        raw_lit > 0 && srgb_lit > 0,
        "the tab overlay must paint its row fill in both arms, or neither arm is \
         measuring a blend: raw_lit={raw_lit}, srgb_lit={srgb_lit}"
    );

    // 1) Production's pairing reproduces vanilla's own blend to the byte.
    assert!(
        (i32::from(raw_max) - predicted).abs() <= 2,
        "the raw-view pairing must reproduce vanilla's raw-gamma blend of TAB_ROW_FILL \
         over black: predicted {predicted}, got {raw_max}"
    );
    // 2) And the pairing this replaced must be far away — otherwise arm 1
    // would pass for a pipeline indifferent to its attachment's format, and
    // the owner-reported "too light" would have had no cause.
    assert!(
        i32::from(srgb_max) - predicted > 40,
        "the corrected-view pairing must still come out markedly lighter than vanilla \
         (this is the bug being fixed, reproduced live): predicted {predicted}, got \
         {srgb_max}"
    );
}

/// Pixel-readback proof that the **XP bar** reaches pixels once the server
/// has sent experience — the same "prove it's on screen, with a control"
/// discipline as the chat gate. The discriminator is *green dominance*: the
/// vanilla XP fill (and the level digits) are green (`G` well above `R`/`B`),
/// which the grey background, grey hotbar wells, red health and gold food
/// pips all fail, so a green-dominant pixel can only be the XP bar.
///
/// Two frames make it two-sided:
/// * `xp = None` (no server experience) → zero green pixels, no bar;
/// * `xp = Some((level, progress))` → a run of green fill + digit pixels.
///
/// The control is the load-bearing half: it fails if the bar ever draws
/// without server-sent experience (the §12.24 "plausible gauge" trap).
#[test]
#[ignore = "requires a GPU adapter"]
fn xp_bar_reaches_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // The XP bar and level digits live at the bottom-centre; scan a generous
    // bottom band there. Hotbar/vitals/crosshair are all off so nothing else
    // paints here.
    let x0 = (w as f32 * 0.20) as u32;
    let x1 = (w as f32 * 0.80) as u32;
    let y0 = (h as f32 * 0.78) as u32;

    let mut render = |xp: Option<(i32, f32)>| -> usize {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            xp,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let mut green = 0usize;
        for y in y0..h {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (
                    u32::from(pixels[i]),
                    u32::from(pixels[i + 1]),
                    u32::from(pixels[i + 2]),
                );
                // Green-dominant: clearly more green than red or blue, and
                // brighter than the grey background so unblended greys and
                // the gold food pips (high red) are excluded.
                if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                    green += 1;
                }
            }
        }
        green
    };

    let no_xp = render(None);
    let with_xp = render(Some((5, 0.5)));

    eprintln!("=== xp bar readback (headless) ===");
    eprintln!("no_xp green={no_xp}");
    eprintln!("with_xp green={with_xp}");

    // Control: off a live server (no experience) the bar must not draw.
    assert_eq!(
        no_xp, 0,
        "without server experience the XP bar must not draw a single green pixel"
    );
    // A half-full level-5 bar paints a wide green fill plus the green level
    // digit — hundreds of pixels. This fails if the bar were blank.
    assert!(
        with_xp > 150,
        "the XP bar's green fill must reach pixels once experience arrives, got {with_xp}"
    );
}

/// GPU gate for the player report this fix addresses: "the boss bar ...
/// is just a solid rectangle and doesn't use the texture pack for it at
/// all." Runs through the **real vanilla `client.jar`** atlas
/// (`GuiAtlas::build`), not a synthetic one, because the bug's own
/// symptom — a flat fill instead of `BossHealthOverlay`'s real
/// per-colour sprite art — can only be told apart from a correct draw by
/// looking at the *actual shipped pixels*, which
/// [`boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`]
/// (synthetic solid-colour sprites, no GPU) structurally cannot see.
///
/// Deliberately colour-agnostic per CLAUDE.md's "you cannot predict an
/// exact composited byte through `ALPHA_BLENDING` on this backend":
/// every threshold below is **measured from this gate's own renders**
/// (a background-only frame vs a full-fill frame), not a hand-picked RGB
/// value, and every assertion is a magnitude/direction claim with
/// tolerance, never an exact byte.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn boss_bar_paints_real_sprite_art_not_a_flat_rectangle() {
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};
    use lodestone_render::{HeadlessTarget, RenderTarget};

    use crate::overlay::{BossBarView, lerp_discrete_width, plain_spans};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // Same (480, 320) the XP gates above use, for the same reason: it is
    // where `calculate_gui_scale(AUTO, w, h) == 1`, so the logical canvas
    // this module lays `BOSS_BAR_WIDTH`/`BOSS_BAR_TOP` into is the
    // physical target 1:1 and the pixel math below needs no scale term.
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    let mut hud = HudRenderer::new(device, format);
    hud.attach_gui(device, queue, format, atlas);

    const BG: u8 = 128;
    let mut render = |progress: Option<f32>| -> Vec<u8> {
        let bars = [BossBarView {
            title: plain_spans(""),
            progress: progress.unwrap_or(0.0),
            color: BossBarColor::Purple,
            overlay: BossBarOverlay::Progress,
        }];
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            hotbar: None,
            health: None,
            food: None,
            xp: None,
            boss_bars: if progress.is_some() { &bars } else { &[] },
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        target.read_texels(device, queue)
    };

    let bar_x = (w as f32 * 0.5 - BOSS_BAR_WIDTH * 0.5).round() as u32;
    let yo = BOSS_BAR_TOP as u32;
    // Row 2 of the 5-row bar: constant along X in the raw sprite (a
    // horizontal bevel varies by row, not by column — measured directly
    // off `.cache/mc/26.2/client-src`'s `purple_progress.png`), so it is
    // the row to use for the fill/background boundary scan below.
    let mid_row = yo + 2;
    let x_probe = bar_x + 90; // interior column, well clear of the sprite's rounded corners

    let sample = |pixels: &[u8], x: u32, y: u32| -> (i32, i32, i32) {
        let i = ((y * w + x) * 4) as usize;
        (i32::from(pixels[i]), i32::from(pixels[i + 1]), i32::from(pixels[i + 2]))
    };
    let painted = |pixels: &[u8], x: u32, y: u32| -> bool {
        let (r, g, b) = sample(pixels, x, y);
        (r - i32::from(BG)).abs() + (g - i32::from(BG)).abs() + (b - i32::from(BG)).abs() > 30
    };

    let none = render(None);
    let bg_only = render(Some(0.0));
    let full = render(Some(1.0));
    let half = render(Some(0.5));

    // -- negative control: no active boss bar paints nothing at the rect.
    let mut wrong = Vec::new();
    for dy in 0..5 {
        if painted(&none, x_probe, yo + dy) {
            wrong.push(format!(
                "row {dy}: with no boss bar the rect must stay background, \
                 got {:?}",
                sample(&none, x_probe, yo + dy)
            ));
        }
    }

    // -- the bug this fixes: a flat rect is one solid colour top to
    // bottom; vanilla's real sprite has a highlight/shadow bevel across
    // its 5 rows. This is the assertion that falls straight out under
    // the pre-fix `rect_px` draw and is the direct pixel-level check of
    // the player's own report.
    let rows: Vec<(i32, i32, i32)> = (0..5).map(|dy| sample(&bg_only, x_probe, yo + dy)).collect();
    let all_identical = rows.windows(2).all(|w| w[0] == w[1]);
    if all_identical {
        wrong.push(format!(
            "the boss bar's 5 rows are all one solid colour — this is the \
             reported bug (a flat rectangle, no sprite art): rows={rows:?}"
        ));
    }

    // -- clause 3 reaches real pixels, and its width is *measured*, not
    // guessed: the fill must be visibly brighter (blue channel) than the
    // bare background at the same column, or nothing below is
    // meaningful.
    let bg_b = sample(&bg_only, x_probe, mid_row).2;
    let full_b = sample(&full, x_probe, mid_row).2;
    if full_b <= bg_b + 20 {
        wrong.push(format!(
            "the progress fill must be visibly brighter than the bare background \
             at a filled column (blue channel): background={bg_b}, full={full_b}"
        ));
    }

    // -- the half-full bar's fill edge lands at the *predicted* partial
    // column, not at the background's own full-182px edge and not at
    // zero — the threshold is this gate's own measured midpoint between
    // background and full fill, not a hand-picked byte value.
    let threshold = (bg_b + full_b) / 2;
    let mut half_edge = None;
    for dx in 0..BOSS_BAR_WIDTH as u32 {
        if sample(&half, bar_x + dx, mid_row).2 > threshold {
            half_edge = Some(dx);
        }
    }
    let predicted_edge = lerp_discrete_width(0.5, BOSS_BAR_WIDTH as i32) as u32;
    match half_edge {
        Some(edge) => {
            let diff = (edge as i32 - predicted_edge as i32).abs();
            if diff > 4 {
                wrong.push(format!(
                    "half-full bar's fill edge should land near the predicted \
                     {predicted_edge}px column, got {edge}px (diff {diff})"
                ));
            }
        }
        None => wrong.push("a half-full bar must still show some fill".to_string()),
    }

    eprintln!("=== boss bar sprite-art gate (headless) ===");
    eprintln!("bg_only rows @ x={x_probe}: {rows:?}");
    eprintln!("bg_b={bg_b} full_b={full_b} threshold={threshold}");
    eprintln!("half_edge={half_edge:?} predicted_edge={predicted_edge}");

    assert!(wrong.is_empty(), "{wrong:?}");
}

/// GPU gate for a live player report: "the xp bar number is too big and too
/// high." Both halves of that sentence are magnitude claims, not sign
/// claims, so this predicts vanilla's real numbers and requires the
/// measurement to land on them — the CLAUDE.md "magnitude species" repair,
/// not a "some digit painted somewhere" check.
///
/// Runs through the **real vanilla atlas + font** (`HudRenderer::attach_gui`,
/// `VanillaFont::shared` via `HudRenderer::new`), because
/// [`xp_bar_reaches_pixels`] above only exercises the jar-less procedural
/// fallback and would not have caught this: the player was looking at
/// `sprite_vitals`, a different code path with its own (until now,
/// independently wrong) scale and offset.
///
/// Two independent renders isolate each claim instead of restating the
/// source's own constants as the expected value:
///
/// * **"too high"**: render the fill alone (`level: 0, progress: 1.0` — no
///   digit, since the digit only draws `if level > 0`) to find the bar's own
///   top row from its pixels, then render the digit alone (`level: 5,
///   progress: 0.0` — no fill, since the fill only draws `if p > 0.0`) to
///   find the digit's top row. The **gap** between them is what
///   `ContextualBar.extractExperienceLevel` vs `ContextualBar.top`
///   fixes at vanilla's `6` logical px —
///   independent of wherever the cluster's own bottom margin happens to
///   place the bar, so this cannot pass by coincidentally agreeing with our
///   own `by`.
/// * **"too big"**: the digit-alone render's ink bounding box width, against
///   the *real jar font's* advance for `"5"` at scale 1 (correct hypothesis)
///   and at scale 2 (the old bug's hypothesis, exactly double) — both
///   computed from [`VanillaFont::from_manager`], outside the code under
///   test.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));
    let font = VanillaFont::from_manager(&manager).expect("build the vanilla font");

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // Chosen for `calculate_gui_scale(AUTO, 480, 320) == 1` (see
    // `hud_vitals_draw_the_real_heart_sprite`'s comment), so the logical
    // canvas `sprite_vitals` lays out into is the physical target 1:1 and no
    // scale multiplication enters the pixel math below.
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    let mut hud = HudRenderer::new(device, format);
    hud.attach_gui(device, queue, format, atlas);
    assert!(
        hud.font_attached(),
        "this gate measures vanilla font metrics; the fixed-advance fallback \
         would make every width prediction below meaningless"
    );

    const BG: u8 = 40;
    let x0 = (w as f32 * 0.20) as u32;
    let x1 = (w as f32 * 0.80) as u32;
    let y0 = (h as f32 * 0.50) as u32;

    // Bounding box of green-dominant pixels in the scan band, or `None` if
    // nothing painted there.
    let mut render_bbox = |xp: Option<(i32, f32)>| -> Option<(u32, u32, u32, u32)> {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            xp,
            hotbar: None,
            health: None,
            food: None,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (u32::MAX, 0u32, u32::MAX, 0u32);
        let mut found = false;
        for y in y0..h {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (
                    u32::from(pixels[i]),
                    u32::from(pixels[i + 1]),
                    u32::from(pixels[i + 2]),
                );
                if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                    found = true;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        found.then_some((min_x, max_x, min_y, max_y))
    };

    // **The order of these two renders is load-bearing, and it was wrong.**
    //
    // This gate used to render the bar (`level: 0`) first and the digit
    // (`level: 5`) second, and it was red for a reason nothing in it could
    // reveal: `XpFlash::tick` sees `0 → 5` across those two frames as a
    // **level-up**, and a flash at full strength runs the digit's green
    // through `flash_toward_white(…, 1.0)`, i.e. paints it pure white. The
    // digit reached pixels perfectly — 947 painted texels against the bar's
    // 906, its own bounding box six rows higher — and not one of them was
    // green-dominant, so the `expect` below fired.
    //
    // A *world*-species failure in CLAUDE.md's table: the flash landed after
    // this gate, and the gate's premise had been "no such subsystem exists".
    // Reading the test could not show it, because the flaw was in the input.
    //
    // Rendering the digit **first** fixes it without weakening anything:
    // `XpFlash` only triggers when it is already `primed` by a previous
    // frame, so the first render of a fresh `HudRenderer` never flashes, and
    // the following `5 → 0` is a decrease, which never flashes either.
    // Digit alone: no fill (`progress: 0.0`), a single glyph (`level: 5`).
    let digit =
        render_bbox(Some((5, 0.0))).expect("the level digit must paint green pixels");
    // Fill alone: no digit (`level: 0`), full bar (`progress: 1.0`).
    let bar = render_bbox(Some((0, 1.0))).expect("a full XP bar must paint green pixels");
    // Negative control: neither renders without server experience.
    let none = render_bbox(None);

    let (bar_x0, bar_x1, bar_y0, bar_y1) = bar;
    let (digit_x0, digit_x1, digit_y0, digit_y1) = digit;
    let digit_width = digit_x1 - digit_x0 + 1;
    let gap = bar_y0 as i32 - digit_y0 as i32;

    let w1 = font.width("5", 1.0);
    let w2 = font.width("5", 2.0);

    eprintln!("=== xp level-number magnitude gate ===");
    eprintln!("bar bbox    = x[{bar_x0}..{bar_x1}] y[{bar_y0}..{bar_y1}]");
    eprintln!("digit bbox  = x[{digit_x0}..{digit_x1}] y[{digit_y0}..{digit_y1}]");
    eprintln!("digit_width = {digit_width}, gap(bar_top - digit_top) = {gap}");
    eprintln!("real font width('5'): scale1={w1:.1} scale2={w2:.1}");

    assert!(
        none.is_none(),
        "without server experience neither the bar nor the digit may paint, got {none:?}"
    );

    // "too high": vanilla's real gap is exactly 6 logical px
    // (vanilla's own contextual-bar rendering bar top, `:34-40` text y). The old bug's
    // `line_h` was `(GLYPH_H + 2) * 2 == 18`, three times too far — a wide
    // enough margin that a few px of font-glyph internal padding cannot
    // produce a false pass.
    assert!(
        (4..=10).contains(&gap),
        "the level digit must sit ~6 logical px above the bar's top row \
         (vanilla `ContextualBar`), got a gap of {gap} — bar_top={bar_y0} digit_top={digit_y0}"
    );

    // "too big": the digit's ink must match the real font's scale-1 advance,
    // not scale-2's (which is exactly double).
    assert!(
        (digit_width as f32) < w2 - 1.0,
        "the level digit is as wide as scale 2 predicts ({w2:.1}px) — the old \
         `let scale = 2.0;` bug is back, got digit_width={digit_width}"
    );
    assert!(
        (digit_width as f32) <= w1 + 2.0,
        "the level digit is wider than scale 1's real font advance ({w1:.1}px) \
         allows, got digit_width={digit_width}"
    );
}

/// GPU gate: the **title/subtitle** overlay and the **action bar** must reach
/// pixels once a server sends them, and must paint **nothing** when empty.
///
/// This is the "show me pixels, with a control" shape, applied to the text
/// path (the strongest control per the director's template): an empty overlay
/// and a populated one must give measurably different coverage inside the
/// widget's own rect, or the text path has proven nothing.
///
/// Two independent bands are scanned — the title's mid-screen rect and the
/// action bar's lower-centre rect — and each state paints only its own band.
/// That isolation is a second control: a blanket-fill or wrong-clear bug would
/// light the *other* band and fail. Everything else (hotbar, vitals,
/// crosshair, debug) is off so nothing else paints in either band.
#[test]
#[ignore = "requires a GPU adapter; run with --ignored"]
fn title_and_action_bar_reach_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // Title band: mid-screen, centred (title draws at y≈0.40h, tall). Action
    // band: lower-centre, above the (absent) hotbar/vitals. x kept central so
    // the bottom-left chat feed never intrudes.
    let xa = (w as f32 * 0.15) as u32;
    let xb = (w as f32 * 0.85) as u32;
    let title_y0 = (h as f32 * 0.30) as u32;
    let title_y1 = (h as f32 * 0.64) as u32;
    let act_y0 = (h as f32 * 0.78) as u32;
    let act_y1 = (h as f32 * 0.96) as u32;

    // Count near-white text texels (white glyphs on the grey clear) in a band.
    let bright_in = |pixels: &[u8], y0: u32, y1: u32| -> usize {
        let mut n = 0usize;
        for y in y0..y1 {
            for x in xa..xb {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
                if r > BG + 40 && g > BG + 40 && b > BG + 40 {
                    n += 1;
                }
            }
        }
        n
    };

    let mut render = |title: Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)>,
                      action_bar: Option<(Vec<TextSpan>, f32)>|
     -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            title,
            action_bar,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        (
            bright_in(&pixels, title_y0, title_y1),
            bright_in(&pixels, act_y0, act_y1),
        )
    };

    let (empty_title, empty_act) = render(None, None);
    let (shown_title, title_leak_act) = render(
        Some((
            crate::overlay::plain_spans("TITLE"),
            Some(crate::overlay::plain_spans("subtitle")),
            1.0,
        )),
        None,
    );
    let (act_leak_title, shown_act) =
        render(None, Some((crate::overlay::plain_spans("Action bar!"), 1.0)));

    eprintln!("=== title/action-bar readback (headless) ===");
    eprintln!("empty:  title_band={empty_title} act_band={empty_act}");
    eprintln!("title:  title_band={shown_title} act_band={title_leak_act}");
    eprintln!("action: title_band={act_leak_title} act_band={shown_act}");

    // Controls: with no server title/action-bar, neither band paints a pixel.
    assert_eq!(
        (empty_title, empty_act),
        (0, 0),
        "an empty HUD must not paint the title or action-bar rects"
    );
    // The title's large glyphs + subtitle cover hundreds of texels.
    assert!(
        shown_title > 100,
        "a server-sent title must reach pixels in its rect, got {shown_title}"
    );
    // The action-bar line is smaller but still tens of texels of white text.
    assert!(
        shown_act > 40,
        "a server-sent action bar must reach pixels in its rect, got {shown_act}"
    );
    // Isolation control: each widget paints only its own band. A blanket-fill
    // or wrong-clear bug would light the other band and trip these.
    assert_eq!(
        title_leak_act, 0,
        "the title overlay must not bleed into the action-bar rect"
    );
    assert_eq!(
        act_leak_title, 0,
        "the action bar must not bleed into the title rect"
    );
}

/// **The closing gate for the HUD-textures island**: proves the survival
/// vitals draw from the *actual vanilla heart sprite in `client.jar`*, not
/// the procedural fallback, by comparing rendered pixels texel-for-texel
/// against the jar art — then EXECUTES the negative control (no atlas
/// attached) and confirms the same assertion *fails*. A gate never watched
/// fail proves nothing; "it draws" is not a gate.
///
/// sRGB note: the atlas uploads as `Rgba8UnormSrgb` and we render into an
/// `Rgba8UnormSrgb` target, so the sample→tint→store roundtrip re-encodes
/// back to ~the source bytes. We compare only *opaque* source texels — the
/// heart's transparent corners show the backdrop and carry no identity — and
/// at an integer 2× scale each texel maps to a clean 2×2 Nearest block.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn hud_vitals_draw_the_real_heart_sprite() {
    use lodestone_assets::Image;
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

    // The source art we must reproduce on screen.
    let heart_png = manager
        .read("assets/minecraft/textures/gui/sprites/hud/heart/full.png")
        .expect("client.jar must carry hud/heart/full.png");
    let heart = Image::decode_png(&heart_png).expect("decode hud/heart/full.png");
    assert_eq!(
        (heart.width, heart.height),
        (9, 9),
        "the heart sprite is 9x9 native"
    );

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // sRGB target so the sampler's linear decode is re-encoded on store,
    // letting opaque texels land near the source PNG bytes.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    // A backdrop that is neither red (heart) nor grey, so an opaque heart
    // texel can never be mistaken for the background.
    const BG: [u8; 3] = [24, 96, 176];

    // Only health on: no hotbar, XP or hunger, so the hearts sit at a
    // location we can compute exactly. `(w, h) = (480, 320)` is chosen
    // specifically so `calculate_gui_scale(AUTO, 480, 320) == 1` — below
    // vanilla's 320-logical-pixel-wide floor at any scale above 1 — so the
    // logical canvas `HudGeometry::build_inner` lays `sprite_vitals` into
    // is identical to this physical target and no scale multiplication
    // enters the picture here at all. `sprite_vitals` draws hearts at their
    // native 9×9 size (no more hardcoded ×2 — see its own doc comment), the
    // first at `xLeft == guiWidth/2 - 91` on vanilla's own vitals-cluster baseline.
    //
    // **`y0` was the hardcoded `h - 19`, and that was correct for the wrong
    // reason.** The hearts used to be stacked upward from a `cluster_top`
    // that moved with the hotbar and the XP bar, and this fixture supplies
    // neither, so `h - 6 - 9 - 4` happened to be `h - 19`. Vanilla's
    // `yLineBase` is `guiHeight - 39` and takes no such branch, so correcting
    // the draw moved the row 20 px and this gate failed — correctly. Derived
    // through [`vitals_line_base`] now, the same call the draw makes.
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        health: Some(20.0),
        food: None,
        xp: None,
        hotbar: None,
        ..HudFrame::new(&stats)
    };
    let s = 1u32;
    let cx = w / 2;
    let x0 = cx - 91;
    let y0 = vitals_line_base(h as f32) as u32;

    // Render one frame with `hud`, read it back, and score how many *opaque*
    // heart texels match the jar sprite within tolerance after the 2× Nearest
    // downsample.
    let mut score = |hud: &mut HudRenderer, tag: &str| -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), BG);
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        const TOL: i32 = 24;
        let (mut opaque, mut matched) = (0usize, 0usize);
        for ty in 0..9u32 {
            for tx in 0..9u32 {
                let si = ((ty * 9 + tx) * 4) as usize;
                if heart.rgba[si + 3] < 250 {
                    continue; // transparent corner — no identity
                }
                opaque += 1;
                let px = x0 + tx * s + s / 2;
                let py = y0 + ty * s + s / 2;
                let di = ((py * w + px) * 4) as usize;
                let dr = i32::from(pixels[di]) - i32::from(heart.rgba[si]);
                let dg = i32::from(pixels[di + 1]) - i32::from(heart.rgba[si + 1]);
                let db = i32::from(pixels[di + 2]) - i32::from(heart.rgba[si + 2]);
                if dr.abs() <= TOL && dg.abs() <= TOL && db.abs() <= TOL {
                    matched += 1;
                }
            }
        }
        eprintln!("{tag}: matched {matched}/{opaque} opaque heart texels");
        (matched, opaque)
    };

    // Positive: atlas attached → real heart sprite → high match.
    let mut lit = HudRenderer::new(device, format);
    lit.attach_gui(device, queue, format, Arc::clone(&atlas));
    let (pos_matched, opaque) = score(&mut lit, "vanilla-atlas");
    assert!(
        opaque > 20,
        "the heart sprite must have a solid opaque body, got {opaque}"
    );

    // Negative control, EXECUTED: no atlas → procedural fallback → the same
    // region does NOT reproduce the jar heart, so the match collapses.
    let mut dark = HudRenderer::new(device, format);
    let (neg_matched, _) = score(&mut dark, "procedural-fallback (negative control)");

    let pos_frac = pos_matched as f32 / opaque as f32;
    let neg_frac = neg_matched as f32 / opaque as f32;
    eprintln!("=== heart-sprite gate: vanilla={pos_frac:.2} fallback={neg_frac:.2} ===");

    // Load-bearing: vanilla pixels match the jar; the fallback fails the very
    // same check; and the delta is wide enough that no coincidence passes
    // both.
    assert!(
        pos_frac > 0.80,
        "with the vanilla atlas the rendered hearts must reproduce hud/heart/full.png, \
         got {pos_matched}/{opaque}"
    );
    assert!(
        neg_frac < 0.40,
        "negative control failed to fail: the procedural fallback reproduced the jar \
         heart sprite ({neg_matched}/{opaque}) — the gate would be vacuous"
    );
    assert!(
        pos_frac - neg_frac > 0.40,
        "vanilla vs fallback delta too small to prove the atlas is what reaches pixels: \
         vanilla={pos_frac:.2} fallback={neg_frac:.2}"
    );
}

/// The scoreboard sidebar's two background plates, predicted from
/// `Hud.displayScoreboardSidebar` (`.cache/mc/26.2/client-src`) rather than
/// eyeballed — the *magnitude* species this repo warns against otherwise.
/// Content is chosen so the 1x/2x hypotheses diverge everywhere (title
/// 30px vs 60px; row widths 54/36 vs 108/72, never coinciding after a
/// clamp) and so the two rows' label/score lengths are pairwise-distinct,
/// which a transposed measurement could not survive.
#[test]
fn sidebar_panel_lands_on_vanillas_own_geometry_not_a_2x_pitch() {
    let plain = |s: &str| {
        vec![TextSpan {
            text: s.to_string(),
            style: lodestone_model::text::TextStyle::default(),
        }]
    };
    let line = |label: &str, score: &str| crate::overlay::SidebarLine {
        label: plain(label),
        score: plain(score),
    };
    let sidebar = Sidebar {
        title: plain("Kills"),
        lines: vec![line("Alice", "11"), line("Bob", "7")],
    };
    let stats = DebugStats::default();
    let frame = HudFrame {
        show_debug: false,
        crosshair: false,
        sidebar: Some(&sidebar),
        ..HudFrame::new(&stats)
    };
    let (w, h) = (640u32, 480u32);
    let geo = HudGeometry::build(&frame, w, h);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

    // Independently hand-derived from `Hud.displayScoreboardSidebar` and
    // the shell's fixed-advance jar-less font (`(GLYPH_W + 1) * scale` per
    // visible char, `GLYPH_W == 5`) — not by calling the code under test.
    let str_w = |s: &str| s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0);
    let spacer_w = str_w(": ");
    let title_w = str_w("Kills");
    let row0_w = str_w("Alice") + spacer_w + str_w("11");
    let row1_w = str_w("Bob") + spacer_w + str_w("7");
    let width = title_w.max(row0_w).max(row1_w);
    assert!(
        (width - 54.0).abs() < f32::EPSILON,
        "hand check: expected width 54.0, derived {width} \
         (title {title_w}, row0 {row0_w}, row1 {row1_w})"
    );
    let height = 2.0 * 9.0;
    let bottom = ch / 2.0 + height / 3.0;
    let left = cw - width - 3.0;
    let right = cw - 3.0 + 2.0;
    let header_y = bottom - height;
    let plate_x = left - 2.0;
    let plate_w = right - plate_x;

    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    let mut header_bounds: Option<(f32, f32, f32, f32)> = None;
    let mut body_bounds: Option<(f32, f32, f32, f32)> = None;
    for chunk in geo.verts.chunks(FLOATS_PER_VERTEX) {
        let (x, y) = (px(chunk[0]), py(chunk[1]));
        let (r, g, b, a) = (chunk[2], chunk[3], chunk[4], chunk[5]);
        if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_HEADER_BG_ALPHA).abs() < 1e-4 {
            let e = header_bounds.get_or_insert((x, y, x, y));
            *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
        } else if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_BODY_BG_ALPHA).abs() < 1e-4 {
            let e = body_bounds.get_or_insert((x, y, x, y));
            *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
        }
    }
    let header = header_bounds
        .expect("the header plate must draw at exactly SIDEBAR_HEADER_BG_ALPHA");
    let body = body_bounds.expect("the body plate must draw at exactly SIDEBAR_BODY_BG_ALPHA");

    let mut mismatches = Vec::new();
    let mut check = |name: &str, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };
    check("header x0", header.0, plate_x);
    check("header x1", header.2, plate_x + plate_w);
    check("header y0", header.1, header_y - 10.0);
    check("header y1", header.3, header_y - 1.0);
    check("body x0", body.0, plate_x);
    check("body x1", body.2, plate_x + plate_w);
    check("body y0", body.1, header_y - 1.0);
    check("body y1", body.3, bottom);
    assert!(
        mismatches.is_empty(),
        "sidebar panel diverged from vanilla's own geometry: {mismatches:?}"
    );
}

/// The boss bar's fixed native rect —
/// `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` (182×5,
/// `.cache/mc/26.2/client-src`) and `extractRenderState`'s `yOffset`
/// arithmetic — not a canvas-relative width or this HUD's ambient 2×
/// text pitch.
#[test]
fn boss_bar_lands_on_vanillas_fixed_182x5_rect_not_a_canvas_fraction() {
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

    // Zero progress so the background plate is the *only* sprite quad —
    // this test's job is placement, not the fill's width (covered by
    // `boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`).
    let bars = vec![BossBarView {
        title: vec![TextSpan {
            text: "Boss".to_string(),
            style: lodestone_model::text::TextStyle::default(),
        }],
        progress: 0.0,
        color: BossBarColor::Red,
        overlay: BossBarOverlay::Progress,
    }];
    let stats = DebugStats::default();
    let frame = HudFrame {
        show_debug: false,
        crosshair: false,
        boss_bars: &bars,
        ..HudFrame::new(&stats)
    };
    let (w, h) = (640u32, 480u32);
    let atlas = boss_bar_synthetic_atlas();
    let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

    let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
    let yo = BOSS_BAR_TOP;

    let quads = quad_boxes(&geo.sprite_verts, cw, ch);
    assert_eq!(
        quads.len(),
        1,
        "zero progress with the Progress overlay must draw exactly the \
         background plate: got {quads:?}"
    );
    let bg = quads[0];
    let mut mismatches = Vec::new();
    let mut check = |name: &str, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };
    check("bar x0", bg.0, bar_x);
    check("bar x1", bg.2, bar_x + BOSS_BAR_WIDTH);
    check("bar y0", bg.1, yo);
    check("bar y1", bg.3, yo + BOSS_BAR_HEIGHT);
    assert!(
        mismatches.is_empty(),
        "boss bar diverged from vanilla's fixed 182x5 rect: {mismatches:?}"
    );
}

/// Covered sample cells and their bounding box inside `rect`, an
/// `(x0, y0, x1, y1)` NDC box — a real CPU rasteriser over the actual
/// triangle geometry, not a vertex-sample count. Shared by every rasterised
/// coverage gate in this file (`recipe_toast_gate`, `chat_hover_tooltip_gate`)
/// rather than one copy per gate, so a fix to the rasteriser cannot land in
/// one gate and not the other. Returns the box because a bare fraction
/// cannot distinguish a uniform-but-wrong frame from a localised blob.
#[cfg(test)]
pub(super) fn coverage(
    verts: &[f32],
    rect: (f32, f32, f32, f32),
    res: usize,
) -> (usize, usize, Option<(f32, f32, f32, f32)>) {
    let (rx0, ry0, rx1, ry1) = rect;
    let to_ndc = |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32;
    let (mut covered, mut inside) = (0usize, 0usize);
    let mut bbox: Option<(f32, f32, f32, f32)> = None;
    for gy in 0..res {
        for gx in 0..res {
            let (px, py) = (to_ndc(gx), to_ndc(gy));
            if px < rx0 || px > rx1 || py < ry0 || py > ry1 {
                continue;
            }
            inside += 1;
            let mut hit = false;
            for tri in verts.chunks_exact(FLOATS_PER_VERTEX * 3) {
                let (ax, ay) = (tri[0], tri[1]);
                let (bx, by) = (tri[FLOATS_PER_VERTEX], tri[FLOATS_PER_VERTEX + 1]);
                let (cx, cy) = (tri[FLOATS_PER_VERTEX * 2], tri[FLOATS_PER_VERTEX * 2 + 1]);
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
    (covered, inside, bbox)
}
