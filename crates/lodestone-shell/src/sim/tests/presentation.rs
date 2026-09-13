use super::*;
/// that built this session already reflects that generation, so redoing the
/// reload on the very first frame would be pure waste). So the very first call,
/// with nothing having changed the selection since, must be a no-op.
#[test]
fn a_fresh_session_does_not_reload_on_its_first_poll() {
    let mut sim = Sim::with_demo_world(test_config());
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "a fresh session's first poll must see no generation change and do nothing"
    );
}

/// The demo world has no server world to re-texture and never depends on a
/// resource pack (`BlockResources::load(false)` always yields the demo
/// palette), so even a *real* selection change must still be a no-op there —
/// this is the "no `net`" arm of the method's own doc. Bumping
/// `crate::resources::pack_generation` here is what makes this call reach
/// past the equality guard at all; without it this test would only be
/// re-checking the guard above under a different name.
#[test]
fn the_demo_world_never_reloads_even_after_a_real_selection_change() {
    let mut sim = Sim::with_demo_world(test_config());
    crate::resources::set_selected_packs(vec!["some-pack".to_string()]);
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "the demo world has no vanilla atlas to reload and must stay a no-op \
         even once the pack selection has genuinely changed"
    );
    // And the guard is consumed even though nothing else happened: a second
    // call with no further selection change must also see no change, rather
    // than re-attempting the (still pointless, on the demo world) reload
    // every frame.
    assert!(
        sim.reload_resource_pack_atlas().is_none(),
        "the generation was already observed by the call above"
    );
}

/// A total-drop condition must not be silent: `TerrainMesh::mesh_column_inner`
/// drops **every** column once `MeshPolicy::id_spaces_agree` is false, and the
/// state that produces it — a live session with no vanilla atlas — is exactly
/// what `resources::asset_root` returning `None` (an unresolved pack root, the
/// owner's reported "launched from outside the repo" case) or any other
/// vanilla-load failure converges on. `with_demo_world` never even attempts a
/// vanilla load (`BlockResources::load(false)`), so attaching a live net to it
/// reproduces "a live session with no vanilla atlas" deterministically — no
/// filesystem or `LODESTONE_ASSETS` manipulation needed to hit the same `Sim`
/// state a real unresolved asset root produces.
#[test]
fn a_live_session_with_no_vanilla_atlas_fires_the_id_space_diagnostic_once() {
    let mut sim = Sim::with_demo_world(test_config());
    assert!(
        sim.vanilla_atlas().is_none(),
        "precondition: the demo-world build never attempts a vanilla load"
    );
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    sim.step(1.0 / 20.0);

    assert!(
        sim.warned_id_space_mismatch,
        "a live session with no vanilla atlas must fire the one-time id-space \
         diagnostic — every column is about to be dropped unmeshed"
    );
    assert!(
        sim.stats.status.contains("TERRAIN NOT LOADING"),
        "the debug overlay's own status line must name the total-drop \
         condition instead of staying on whatever it last said, got {:?}",
        sim.stats.status
    );

    // One-time, not per-frame: further polls must not re-trip anything (there
    // is nothing to re-observe — `warned_id_space_mismatch` stays latched for
    // the rest of this session, which is what keeps the log from repeating
    // every frame the way the per-column warning already does).
    for _ in 0..5 {
        sim.step(1.0 / 20.0);
    }
    assert!(
        sim.warned_id_space_mismatch,
        "the latch must not un-set itself mid-session"
    );
}

/// Control for the diagnostic above: an always-on warning would be exactly as
/// useless as a silent one, so a session that actually resolves the vanilla
/// pack must stay quiet. `client_config`'s `Mode::Window` is what reaches the
/// real `BlockResources::load(true)` path (`with_demo_world`/`Mode::Headless`
/// never attempts it), so this is the same live-load path
/// `a_client_session_holds_only_the_live_world_never_offline_terrain` and its
/// neighbours already depend on succeeding in this checkout.
#[test]
// The control's premise is "this checkout resolves a real vanilla pack", and
// it asserts that precondition loudly rather than skipping — correct, and it
// makes the gate unrunnable on a runner with no `.cache/mc/<version>/`. The
// measurement it controls (the warning *does* fire when the pack is missing)
// needs no jar and keeps running everywhere; only this half is gated.
#[ignore = "requires a fetched vanilla client.jar + blocks.json under .cache/mc/<version>/"]
fn a_healthy_live_session_never_fires_the_id_space_diagnostic() {
    use crate::net::NetUpdate;

    let mut sim = Sim::new(client_config());
    assert!(
        sim.vanilla_atlas().is_some(),
        "precondition: this checkout must resolve a real vanilla pack under \
         .cache/mc/<ver> for the control to mean anything — banner: {:?}",
        sim.asset_banner()
    );
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    sim.step(1.0 / 20.0);

    assert!(
        !sim.warned_id_space_mismatch,
        "a session with a real vanilla atlas must never trip the id-space \
         mismatch diagnostic"
    );
    assert!(
        !sim.stats.status.contains("TERRAIN NOT LOADING"),
        "the healthy control must not show the mismatch banner, got {:?}",
        sim.stats.status
    );
}

/// The discriminating gate for the vanilla `yBodyRot`/`yHeadRot` split
/// (`LivingEntity.tickHeadTurn`): looking around while standing still must
/// **not** turn the third-person body until the head exceeds `50°` relative
/// to it (`Player.getMaxHeadRotationRelativeToBody`'s default), and once it
/// does, the body must snap so the head sits at *exactly* that clamp — not
/// at the raw look yaw, which is what this engine did before (the body
/// always equalled the head, unconditionally). Three arms in one input space
/// — inside, exactly at, and beyond the clamp — collected and asserted
/// together, per this repo's own rule against an `assert!` inside a `for`
/// loop hiding every failure but the first.
///
/// A head yaw of `0` relative to the body is the coincident input where the
/// clamped and unclamped readings agree; every arm here uses a nonzero
/// offset so the two hypotheses can actually disagree.
#[test]
fn body_yaw_holds_still_within_the_clamp_and_snaps_once_it_is_exceeded() {
    struct Case {
        name: &'static str,
        offset: f32,
        // Expected body yaw delta from the base yaw after one tick.
        expected_body_delta: f32,
        // Expected |head yaw relative to body| after one tick.
        expected_head_relative: f32,
    }
    let cases = [
        Case {
            name: "inside the clamp (30 of 50)",
            offset: 30.0,
            expected_body_delta: 0.0,
            expected_head_relative: 30.0,
        },
        Case {
            name: "exactly at the clamp (50 of 50)",
            offset: 50.0,
            expected_body_delta: 0.0,
            expected_head_relative: 50.0,
        },
        Case {
            name: "beyond the clamp (90 of 50)",
            offset: 90.0,
            // tick_head_turn: no movement candidate, so the 0.3 catch-up
            // term is a no-op (target == body_yaw); the clamp then bumps
            // body_yaw by (90 - 50) = 40 so the head sits exactly at the
            // 50° boundary. Derived from `LivingEntity.tickHeadTurn`, not
            // guessed.
            expected_body_delta: 40.0,
            expected_head_relative: 50.0,
        },
    ];

    let mut mismatches = Vec::new();
    for case in cases {
        let mut sim = Sim::new(test_config());
        sim.cycle_camera_type();
        let base_yaw = sim.player().yaw;
        sim.player_mut(|p| p.yaw = base_yaw + case.offset);
        sim.step(lodestone_ecs::TICK_PERIOD);
        // Force full-tick interpolation so this reads the tick that just
        // ran rather than easing from the previous one.
        sim.clock_mut(|c| c.interp_alpha = 1.0);
        let state = sim
            .third_person_body_state()
            .expect("third person is on");
        let body_delta = wrap_degrees(state.body_yaw_deg - base_yaw);
        let head_relative = wrap_degrees(state.anim.head_yaw_deg).abs();
        if (body_delta - case.expected_body_delta).abs() > 1.0 {
            mismatches.push(format!(
                "{}: body yaw moved {body_delta:.2}°, want {:.2}°",
                case.name, case.expected_body_delta
            ));
        }
        if (head_relative - case.expected_head_relative).abs() > 1.0 {
            mismatches.push(format!(
                "{}: head relative to body {head_relative:.2}°, want {:.2}°",
                case.name, case.expected_head_relative
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "body/head clamp mismatches: {mismatches:#?}"
    );
}

/// The movement-direction clause `LivingEntity.tick` feeds `tickHeadTurn`'s
/// candidate: while the feet are moving, the body eases toward the
/// *walking* direction rather than the look direction. Strafing
/// perpendicular to a fixed look yaw is the case that discriminates this
/// from the head-clamp gate above, which never moves the feet: sustained
/// strafing must turn the body away from the look yaw at all (rejecting
/// "the body never turns from movement alone"), and — because the movement
/// direction here is durably ~90° from the look yaw — the head-relative-to-
/// body offset converges to *exactly* the clamp regardless of walk speed,
/// the same fixed point the clamp gate's "beyond" arm measures directly.
/// That convergence value is the clamp constant itself, not a guessed round
/// number: it falls out of `tickHeadTurn`'s recursion once the pull toward
/// the movement direction outpaces what the clamp allows each tick.
#[test]
fn body_yaw_eases_toward_the_movement_direction_while_strafing() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let base_yaw = sim.player().yaw;
    sim.input_mut(|i| i.set(lodestone_controller::Action::Left, true));

    let mut moved = false;
    for _ in 0..40 {
        let before = sim.player().position;
        sim.step(lodestone_ecs::TICK_PERIOD);
        let after = sim.player().position;
        if (after.x - before.x).abs() > 1.0e-6 || (after.z - before.z).abs() > 1.0e-6 {
            moved = true;
        }
    }
    assert!(
        moved,
        "precondition: strafing must actually move the player, or this gate \
         measures nothing"
    );
    assert!(
        (sim.player().yaw - base_yaw).abs() < 1.0e-6,
        "precondition: no mouse input was fed, so the look yaw must not have \
         drifted on its own"
    );

    sim.clock_mut(|c| c.interp_alpha = 1.0);
    let state = sim
        .third_person_body_state()
        .expect("third person is on");
    let body_delta = wrap_degrees(state.body_yaw_deg - base_yaw).abs();
    assert!(
        body_delta > 1.0,
        "the body never turned toward the movement direction; it is still \
         at the look yaw (body_delta {body_delta:.2}°) — this is the exact \
         shape of feeding the body the raw look yaw every tick"
    );
    let head_relative = wrap_degrees(state.anim.head_yaw_deg).abs();
    assert!(
        (30.0..=55.0).contains(&head_relative),
        "sustained strafing perpendicular to a fixed look direction must \
         settle the head-relative-to-body offset near the 50° clamp \
         (`Player.getMaxHeadRotationRelativeToBody`), got {head_relative:.2}°"
    );
}

/// Rejects a snap-at-threshold implementation: a mid-frame sample of a tick
/// that moved the body must land strictly *between* the tick's start and end
/// body yaw, not jump straight to the endpoint early. Uses the clamp gate's
/// own "beyond" arm (90° offset -> a real 40° body move) so there is a
/// nonzero span to interpolate across.
#[test]
fn body_yaw_interpolates_between_ticks_rather_than_snapping() {
    let mut sim = Sim::new(test_config());
    sim.cycle_camera_type();
    let base_yaw = sim.player().yaw;
    sim.player_mut(|p| p.yaw = base_yaw + 90.0);
    sim.step(lodestone_ecs::TICK_PERIOD);

    sim.clock_mut(|c| c.interp_alpha = 0.0);
    let start = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;
    sim.clock_mut(|c| c.interp_alpha = 1.0);
    let end = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;
    sim.clock_mut(|c| c.interp_alpha = 0.5);
    let mid = sim
        .third_person_body_state()
        .expect("third person is on")
        .body_yaw_deg;

    assert!(
        (start - base_yaw).abs() < 1.0,
        "start of tick should still read the pre-tick body yaw, got {start} \
         (base {base_yaw})"
    );
    assert!(
        (end - base_yaw - 40.0).abs() < 1.0,
        "end of tick should read the new, clamped body yaw, got {end} (base \
         {base_yaw})"
    );
    let (lo, hi) = (start.min(end), start.max(end));
    assert!(
        mid > lo + 1.0 && mid < hi - 1.0,
        "a mid-frame sample must land strictly between the tick's start \
         ({start}) and end ({end}) body yaw rather than snapping to either \
         endpoint; got {mid}"
    );
}

/// The real gate: a hex chat colour reaching a **drawn vertex**
/// through the actual per-frame wiring, not through hand-authored spans.
///
/// `hud.rs`'s own `chat_spans_carry_hex_named_and_inline_legacy_colour_to_distinct_vertices`
/// proves `HudGeometry::build` draws a hex colour when handed a
/// `Vec<TextSpan>` directly — it never touches `ChatLog`/`Sim` at all, so it
/// could not have caught a routing failure: the bug is entirely upstream, in
/// `Session::recent_chat`/`app/redraw.rs` calling the *legacy*
/// `String`-flattening accessor instead of the span-carrying one. This test
/// drives the real production entry point instead: a `NetUpdate::Chat`
/// folded by the real `Sim::poll_net` (exactly like
/// `end_session_tears_down_and_a_fresh_connect_afterward_starts_clean` above
/// feeds one), read back through `Sim::recent_chat_spans` — the exact
/// accessor `app/redraw.rs` calls every frame — windowed with the same
/// slicing arithmetic `app/redraw.rs` applies, and drawn through the same
/// `HudGeometry::build` the live `HudRenderer` calls.
#[test]
fn hex_chat_colour_reaches_a_vertex_through_the_real_session_and_redraw_wiring() {
    use crate::hud::{DebugStats, HudFrame, HudGeometry};
    use lodestone_model::text::{Text, TextColor, TextContent, TextSpan, TextStyle};

    // The discriminating fixture: a hex `TextColor::Rgb`
    // component style, a named component style, and a literal carrying an
    // *inline* `§c` code — the owner's own report was entirely the third
    // convention. A fixture using only named colours cannot tell "hex is
    // dropped" from "everything works", because a named colour survives
    // legacy flattening intact.
    let hex = Text {
        content: TextContent::Literal("Hex".to_string()),
        style: TextStyle {
            font: None,
            color: Some(TextColor::Rgb(0x1a_2b3c)),
            ..TextStyle::default()
        },
        ..Text::default()
    };
    let inline_legacy = Text::literal("\u{00a7}cRed");
    let named = Text {
        content: TextContent::Literal("Gray".to_string()),
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

    // The real production entry point: a server chat line arriving on the
    // net thread's `NetUpdate` channel, folded by the real `Sim::poll_net`
    // (`NetUpdate::Chat`'s handler in `sim/net_apply.rs` stores the full
    // `Text`, spans and all, in `ChatLog` — nothing flattens it at this
    // point).
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    feed.send(NetUpdate::Chat {
        text: root,
        player: false,
        sender: None,
        // A system message carries no signature to check, so the driver's
        // verdict is `false` — see `NetUpdate::Chat::verified`, which is
        // "unproven", not "forged".
        verified: false,
    })
    .unwrap();
    sim.poll_net();

    // The real accessor `app/redraw.rs` calls every frame.
    let chat_spans_owned = sim.recent_chat_spans(10);
    assert_eq!(
        chat_spans_owned.len(),
        1,
        "setup: exactly one chat line must have arrived"
    );

    // The same windowing/slicing `app/redraw.rs` applies before filling
    // `HudFrame::chat_spans` — closed-chat window is the whole vec, i.e.
    // `(0, chat_spans_owned.len())`, reproduced verbatim here.
    let chat_spans_lines: Vec<(&[TextSpan], f32)> = chat_spans_owned
        .iter()
        .map(|(spans, age)| (spans.as_slice(), *age))
        .collect();

    let stats = DebugStats::default();
    let geo = HudGeometry::build(
        &HudFrame {
            crosshair: false,
            show_debug: false,
            chat_spans: &chat_spans_lines,
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
        "these colours never reached a vertex through the real Sim/redraw \
         wiring: {missing:?} (full expected set: {expected:?})"
    );

    // Control: the exact same stored line, flattened to a plain string and
    // fed into `HudFrame::chat` instead of `chat_spans` — this is the shape
    // the legacy string accessor would hand back, and no production call site
    // builds it (`app/redraw.rs` fills
    // `chat_spans` only). Reproducing the flattening here, rather than
    // reaching for a production accessor that no longer exists, is what
    // proves the detector can actually fail, and localises the loss to the
    // accessor seam rather than to `HudGeometry::build`
    // itself (which is shared by both branches).
    let chat_owned = sim.recent_chat_spans(10);
    assert_eq!(chat_owned.len(), 1, "setup: same one line, legacy accessor");
    let chat_legacy_owned: Vec<(String, f32)> = chat_owned
        .iter()
        .map(|(spans, a)| (crate::overlay::spans_text(spans), *a))
        .collect();
    let chat_legacy: Vec<(&str, f32)> = chat_legacy_owned
        .iter()
        .map(|(l, a)| (l.as_str(), *a))
        .collect();
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
    let legacy_has_colour = |rgb: (u8, u8, u8)| {
        legacy_geo
            .verts
            .chunks_exact(6)
            .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
    };
    assert!(
        !legacy_has_colour((0x1a, 0x2b, 0x3c)),
        "control failed: the legacy accessor was expected to lose the hex \
         colour (that is the bug issue #649 names) but drew it anyway — this \
         test's premise is wrong"
    );
}

/// The anti-island companion to the gate above: a grep control on
/// `app/redraw.rs`'s own source, in the same spirit as
/// `menu::nav::tests::app_rs_still_threads_every_chat_option_into_the_hud_frame`.
/// The gate above proves the wiring works when it is exercised *this* way,
/// but a unit test cannot run `App::redraw` itself (it is the frame loop, and
/// needs a live GPU context) — so if the real call site quietly reverted to
/// filling the legacy `HudFrame::chat` field instead, this test would keep
/// passing while the screen went back to hex-blind. This is one grep wide,
/// which is why it is checked by reading the source rather than by driving
/// the widget.
#[test]
fn app_rs_fills_hud_frame_chat_spans_not_the_legacy_chat_field() {
    let src = include_str!("../../app/redraw.rs");
    assert!(
        src.contains("hud_frame.chat_spans = &chat_spans_lines"),
        "app/redraw.rs must fill `HudFrame::chat_spans` from the real \
         `Sim::recent_chat_spans` wiring — see issue #649"
    );
    // The control: the detector must be able to report an absence. The
    // hex-blind legacy line is intentionally reconstructed below.
    assert!(
        !src.contains("hud_frame.chat = &chat_lines"),
        "app/redraw.rs must not go back to filling the legacy, hex-blind \
         `HudFrame::chat` field from the per-frame chat wiring — that is \
         exactly the regression this test exists to catch"
    );
}

/// **The join-ordering gate.** With the terrain half of the readiness condition
/// already satisfied — the exact moment the world used to become visible — an
/// outstanding server resource pack must still hold it back.
///
/// The three steps are one sequence on one `Sim`, deliberately, because the
/// claim is about an *ordering* and not about a state: the world is presentable,
/// then a pack becomes outstanding and it stops being presentable, then the pack
/// resolves and it is presentable again. A test that only checked the middle
/// step would pass against a `world_wait` that was simply always `Some`.
///
/// The first assertion is the load-bearing control. It establishes that this
/// fixture really is in the "presentable before the pack arrives" state, so the
/// hold in the second step is attributable to the pack and not to the join being
/// unready for some other reason. A terrain-only readiness predicate answers
/// "presentable" at all three steps.
///
/// The guard is `crate::net::hold_pack_apply_for_test`, which hands out the
/// **production** `PackApplyInFlight` that `begin_accept` takes and whose `Drop`
/// is the production decrement — not a stub. A stubbed count would be asserting
/// against itself.
#[test]
fn an_outstanding_resource_pack_holds_the_world_back_at_the_moment_it_would_have_appeared() {
    use crate::net::NetUpdate;

    let mut sim = Sim::new(client_config());
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    sim.poll_net();
    assert_eq!(sim.session_phase(), SessionPhase::Connected);

    // `pack_generation` is process-wide and other tests in this binary bump it
    // (the Resource Packs screen's own gates do), so settle the atlas latch
    // before reading it rather than assuming a quiet process. Bounded, and the
    // assertion below is what reports a failure to settle.
    for _ in 0..8 {
        if !sim.asset_wait().atlas_stale {
            break;
        }
        let _ = sim.reload_resource_pack_atlas();
    }

    assert_eq!(
        sim.world_wait(),
        None,
        "control: with no pack outstanding this join is presentable — this is \
         the state the world used to appear in, and it is what makes the hold \
         below attributable to the pack"
    );

    {
        let _pack = crate::net::hold_pack_apply_for_test();
        assert_eq!(
            sim.world_wait(),
            Some(crate::menu::loading::WorldWait::ApplyingPack),
            "an accepted, unresolved server pack must hold the loading screen: \
             presenting here is the reported defect — the world appears wearing \
             the previous pack's textures and pops a second later"
        );
    }

    assert_eq!(
        sim.world_wait(),
        None,
        "and it must let go once the pack resolves, or the screen never clears"
    );
}

/// The labelled terrain screen is opt-in state, not a side effect of receiving
/// a login or a server view radius. This isolates the scope boundary used by
/// the launcher: a new survival creation arms it, while every fresh `Sim`
/// (including remote joins and existing saves) starts unarmed and a reset
/// clears the latch again.
#[test]
fn initial_terrain_screen_is_scoped_to_the_new_world_latch() {
    let mut sim = Sim::new(client_config());
    assert!(!sim.shows_new_world_loading());
    assert!(sim.terrain_progress().is_none());

    sim.arm_new_world_loading(32);
    assert!(sim.shows_new_world_loading());
    assert_eq!(
        sim.terrain_progress.snapshot().map(|progress| progress.expected),
        Some(65 * 65),
        "arming a new world must establish the selected 32-radius square"
    );

    sim.reset_loading_state();
    assert!(!sim.shows_new_world_loading());
    assert!(sim.terrain_progress.snapshot().is_none());
}

/// The **ordering inside `app/redraw.rs`**, which no type check and no unit test
/// can see, checked by reading that file's source from this one.
///
/// `WindowApp::redraw` is a single ~2,700-line function and the fix depends
/// entirely on where two of its blocks sit relative to each other:
/// `Sim::reload_resource_pack_atlas` rebuilds the block atlas and re-meshes
/// every loaded column (the visible hitch), and the loading overlay's own block
/// asks `Sim::world_wait` whether to keep covering the screen. The reload must
/// come **first**. If it did not, the frame on which a pack lands would present
/// one frame of the old atlas before the overlay went up — reinstating exactly
/// the flash this is meant to remove — and every other gate here would stay
/// green, because the two blocks are individually correct either way.
///
/// This lives in `sim/tests.rs` rather than in `redraw.rs` on purpose: a
/// source-grep gate placed inside the file it greps matches its own assertion
/// string and passes with the real line deleted.
#[test]
fn redraw_applies_a_pending_pack_before_it_asks_whether_to_stop_covering_the_world() {
    let src = include_str!("../../app/redraw.rs");

    let reload = src
        .find("self.sim.reload_resource_pack_atlas()")
        .expect("app/redraw.rs must still apply a pending resource pack itself");
    let gate = src
        .find("self.sim.world_wait()")
        .expect("the loading overlay must be gated on `Sim::world_wait`, which \
                 is the two-rule predicate — terrain readiness alone dismisses \
                 while a pack is still being applied");

    assert!(
        reload < gate,
        "the atlas reload must run before the loading overlay's own gate in \
         `WindowApp::redraw`, so the first world frame presented after the \
         hitch already wears the new pack"
    );
}

/// `Sim::detach_presentation`/`attach_presentation` actually add
/// and remove real systems, and a round trip is exact — not "no error was
/// returned", a measured count.
///
/// The control this test needs (per this repo's own rule that an absence
/// assertion needs a detector proven to work, not merely described): a
/// **second** consecutive detach, with nothing re-attached in between, must
/// report `0` removed. If it instead reported the same nonzero number every
/// time regardless of state, that would mean `detach_presentation`'s count
/// is not measuring real removal at all — this is the run that would catch
/// that bug and does not merely assert the direction of a change.
///
/// The second control is the round trip itself: `add_systems` does not
/// deduplicate (`crate::sim::presentation`'s own module doc), so if
/// `attach_presentation` ever registered a presentation system on top of a
/// leftover copy, the **second** detach would report *more* systems removed
/// than the first — this is the discriminating input a naive
/// "attach re-adds without checking" bug would fail, while a correct
/// implementation passes.
#[cfg(feature = "runtime-presentation")]
#[test]
fn detach_presentation_removes_systems_and_reattach_is_exact() {
    let mut sim = Sim::new(test_config());
    assert!(
        sim.presentation_attached(),
        "a freshly constructed Sim composes the four presentation plugins \
         unconditionally (Sim::client_app) — presentation starts attached"
    );

    let first_detach = sim.detach_presentation();
    assert!(
        first_detach > 0,
        "detach must actually remove systems from the schedules — the terrain \
         mesher, render-side interpolation, the Display extract and the \
         pick/interaction/particle systems all tag into PresentationSet, so \
         zero removed here means the tagging or the removal call is broken, \
         not that there was nothing to remove"
    );
    assert!(!sim.presentation_attached());

    // Control 1: detaching an already-detached session removes nothing.
    assert_eq!(
        sim.detach_presentation(),
        0,
        "a second consecutive detach must be a real no-op, proving the first \
         count above reflected actual work rather than a constant"
    );

    sim.attach_presentation();
    assert!(sim.presentation_attached());

    // Control 2: the round trip is exact.
    let second_detach = sim.detach_presentation();
    assert_eq!(
        second_detach, first_detach,
        "re-attach then detach must remove exactly what the first detach did — \
         a larger count here means attach_presentation duplicated a system \
         add_systems never deduplicates on its own"
    );
}
