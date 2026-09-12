//! Tests for session startup, UI synchronization, recipe state, and respawn lifecycle.

use super::*;

/// **Pressing Play Selected World reaches a running integrated server**.
///
/// This is the anti-island gate for singleplayer, and it is the only test
/// anywhere that crosses *every* seam of it in one go: the registry's
/// serverbound lookup, the boxed `ServerProtocol`, the net thread, the
/// in-memory duplex, `IntegratedServer`'s serving loop, the real v770 wire
/// format, and the client's decode — ending at a `NetUpdate` the shell's own
/// frame loop consumes.
///
/// The button half is `menu::nav`'s
/// `play_selected_world_asks_the_app_to_start_singleplayer`, which asserts the
/// click produces `MenuAction::Singleplayer(None)`; `apply_menu_action`'s arm
/// between the two is a single call this file can be read for. The seam
/// *without* the shell is `crates/protocol/v770/tests/singleplayer_seam.rs`.
///
/// **Chunks, not just login, is the load-bearing assertion.** Login is five
/// `ServerProtocol` methods with no trait defaults, so it cannot silently fall
/// through the box; terrain is where a half-wired server shows up, and it is
/// also the only thing here that proves the *world* exists rather than just a
/// handshake. A world that logs in and streams nothing is precisely the shape
/// of the chunk-blackout failures `CLAUDE.md` records.
///
/// `view_radius = 0` is one column: the bundled generator costs ~12 ms per
/// column, and one is enough to prove terrain crosses the wire (its *content*
/// is verified block-for-block in `lodestone-server`'s own tests, against a
/// JVM oracle rather than against our encoder).

#[test]
fn pressing_play_reaches_a_running_integrated_server() {
    let protocol = Config::default().protocol;
    let seed = crate::menu::world_select::BUNDLED_WORLD.seed;
    // `None` world dir: this gate is about the seam reaching a running server,
    // not about persistence (that is covered in
    // `tests/singleplayer_persistence.rs`), and an in-memory world leaves
    // nothing in the developer's real data directory.
    let net = match launch_singleplayer(
        protocol,
        0,
        None,
        seed,
        crate::menu::create_world::WorldTypePreset::Normal,
        None,
    ) {
        Ok(net) => net,
        Err(e) => {
            // A build with no hostable family must *report*, which is the
            // `--no-default-features` contract. In the default build (`live`)
            // this is a failure, not a skip.
            assert!(
                !cfg!(feature = "live"),
                "the default build must be able to host singleplayer: {e}"
            );
            assert!(matches!(e, LaunchError::NoVersionFamily { .. }));
            return;
        }
    };

    // The loop below is bounded by *state reached* — `logged_in && chunks >
    // 0`, or a definitive `fatal` answer from the session itself — never by
    // racing the clock in the ordinary case. `deadline` only exists as a
    // backstop against a genuine hang (no update of any kind, ever), so it
    // must be generous rather than tight: this test spins up a real
    // integrated server and waits for real worldgen on a background thread,
    // and that thread measurably starves for CPU when several agents are
    // running concurrent `cargo` builds in this repo. The previous 30 s
    // budget was measured taking 19.11 s to pass *alone* on an otherwise
    // quiet machine (64% of the budget with zero contention), which is
    // exactly `CLAUDE.md`'s "a timing gathered under load is attributed to
    // the wrong cause" shape — it was failing from contention, not from a
    // regression. 240 s (matching `tests/singleplayer_persistence.rs`'s
    // `SESSION_DEADLINE` and `tests/singleplayer_terrain_arrives.rs`'s own
    // `DEADLINE` — the same class of test, already using this budget) costs
    // a healthy run nothing, because the loop still exits the instant
    // success is observed; it only changes the outcome for a run that was
    // previously timing out on a busy machine despite the session being
    // perfectly healthy.
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut logged_in = false;
    let mut chunks = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut fatal = false;
    while Instant::now() < deadline && !(logged_in && chunks > 0) && !fatal {
        for update in net.poll() {
            match update {
                crate::net::NetUpdate::LoggedIn { .. } => logged_in = true,
                // The production consumer adopts the authoritative pose before
                // releasing the driver's deferred correction response. This
                // direct `NetClient` harness has no `Sim` to do that work, so
                // mirror the consumer contract here; otherwise the driver
                // intentionally pauses inbound reads at the placement teleport
                // and this test would misdiagnose the missing chunk as a
                // transport or decode failure.
                crate::net::NetUpdate::Teleport { pos, rotation, .. } => {
                    net.acknowledge_teleport_correction(pos, rotation);
                }
                crate::net::NetUpdate::Chunk { .. } => chunks += 1,
                // Collected rather than ignored: an `Error`/`Disconnected`
                // here is the actual diagnosis, and without it the failure
                // message would only say "timed out". Also a *definitive*
                // answer, unlike silence — once the session itself has
                // reported failure, waiting out the rest of a now-generous
                // backstop cannot produce more evidence, so `fatal` ends the
                // loop after this batch finishes draining rather than at
                // `deadline`.
                crate::net::NetUpdate::Error(e) => {
                    errors.push(e);
                    fatal = true;
                }
                crate::net::NetUpdate::Disconnected(reason) => {
                    errors.push(format!("disconnected: {reason:?}"));
                    fatal = true;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    assert!(
        logged_in,
        "the client never logged in to the integrated server; errors: {errors:?}"
    );
    assert!(
        chunks > 0,
        "logged in but no terrain arrived — the server is serving nothing; \
         errors: {errors:?}"
    );
    assert!(
        errors.is_empty(),
        "the session reported errors while starting: {errors:?}"
    );
}
/// **The islands sweep's finding, made permanent**: `sim::build` used to
/// insert `Profile(PhysicsProfile::mc_1_21())` unconditionally, so a 1.8.9
/// (`v47`) session ran modern movement physics with no protocol-family
/// selection at all — a correct function (`PhysicsProfile::mc_1_8`) fed a
/// constant by its producer, `CLAUDE.md`'s most common defect shape. The fix
/// threads `config.protocol` through `lodestone_registry::
/// physics_profile_for_protocol`, the one crate allowed to know which
/// protocol numbers belong to which version family.
///
/// This is the discriminating half of that regression test, gated on `v47`
/// specifically because it is not the default-compiled family: with only
/// `live` (`v770`) enabled, every protocol resolves to `mc_1_21()`, which is
/// *also* what the old hardcoded constant produced — an input on which the
/// old and new code agree is not a test (`CLAUDE.md`'s "world" vacuous-test
/// species). `v47`'s correct answer differs from that constant, so this
/// assertion actually fails on the pre-fix code.
#[cfg(feature = "v1-8")]
#[test]
fn physics_profile_threads_the_configured_protocol_through_the_session() {
    // `render_distance: 2`, matching `pacing_sim`'s own reasoning: the
    // smallest span that still builds a real demo world, so this stays a
    // cheap unit test rather than paying for a full render distance's worth
    // of worldgen just to read back one resource.
    let sim = Sim::with_demo_world(Config {
        mode: Mode::Headless,
        render_distance: 2,
        protocol: 47,
        ..Config::default()
    });
    assert_eq!(
        sim.profile(),
        lodestone_physics::PhysicsProfile::mc_1_8(),
        "a v47 (1.8.9) session must run the 1.8 physics profile, not the \
         modern default every construction site used to hardcode"
    );
}

/// The companion baseline: the default-compiled family (`v770`, 26.2) must
/// still resolve to the modern profile — unchanged behaviour for the only
/// family that was ever actually joined before this seam existed. Weak on
/// its own (the pre-fix hardcoded constant satisfies it too, exactly the
/// coincidence [`physics_profile_threads_the_configured_protocol_through_the_session`]'s
/// own doc names), but it is cheap, always runs, and catches the mapping
/// table returning the wrong default or panicking on the ordinary path.
#[test]
fn physics_profile_defaults_to_the_modern_profile_for_the_default_protocol() {
    let sim = pacing_sim();
    assert_eq!(
        sim.profile(),
        lodestone_physics::PhysicsProfile::mc_1_21()
    );
}

/// **Social-roster synchronization, exercised through production code.**
///
/// `crate::menu::social::entries_from_tablist` was pure and unit-tested
/// with **no caller anywhere in the shell** — `docs/social-interactions.md`'s
/// own "Decorative" section. This does not call it a second time by hand
/// (that would just be the existing unit test again, which proves
/// nothing about production); it drives the actual chain: a real
/// `WindowApp`, a `SessionTabList` folded through the same `NetIngest`
/// schedule the net thread runs, and `drive_ui_from_session` itself —
/// the method `redraw()` calls every frame.
#[test]
fn drive_ui_from_session_refreshes_the_social_roster_from_the_real_tab_list() {
    use crate::net::NetUpdate;
    use lodestone_client::{ClientEvent, GameMode, PlayerListEntry};
    use uuid::Uuid;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // `drive_ui_from_session`'s refresh is guarded on `SessionPhase::Connected`
    // — reach it the same way `sim/tests.rs`'s own tab-list test does,
    // through a real `NetUpdate`, not by poking a private field.
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert_eq!(
        app.sim.session_phase(),
        crate::sim::SessionPhase::Connected,
        "precondition: the refresh guard reads this, so it must actually be live"
    );

    let alice = Uuid::from_u128(1);
    let bob = Uuid::from_u128(2);
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::PlayerListUpdate {
            entries: vec![
                PlayerListEntry {
                    uuid: Some(bob),
                    name: Some("Bob".into()),
                    game_mode: Some(GameMode::Creative),
                    latency: Some(20),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
                PlayerListEntry {
                    uuid: Some(alice),
                    name: Some("Alice".into()),
                    game_mode: Some(GameMode::Survival),
                    latency: Some(10),
                    display_name: None,
                    listed: Some(true),
                    properties: None,
                    chat_session: None,
                    list_order: None,
                    hat_visible: None,
                },
            ],
        });

    // Precondition: nothing has refreshed the screen model yet — proves the
    // assertion below actually exercises `drive_ui_from_session`, not some
    // earlier call this test forgot about.
    assert!(
        app.nav.social().entries().is_empty(),
        "precondition: the roster must still be empty before the real call runs"
    );

    app.drive_ui_from_session();

    let names: Vec<&str> = app
        .nav
        .social()
        .entries()
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["Alice", "Bob"],
        "the roster must reflect the real folded tab list, in vanilla's display order"
    );
}

/// The credits-screen handoff, exercised through production code exactly like
/// the social-roster test above: `menu::UiState::show_credits` and
/// `net::NetUpdate::WinGame` both already existed, individually tested,
/// with **nothing calling either from the other** — the credits screen was
/// reachable only from a test, and `WinGame` only reached a channel no
/// one drained into UI state. This drives the real chain end to end: a
/// real `WindowApp`, a real `NetUpdate::WinGame` through the loopback
/// feed (the same seam `NetClient::run`'s background thread publishes
/// into in production, once `net::forward` — separately proven by
/// `forward_translates_win_game_into_the_credits_signal` — turns the real
/// decoded `ClientEvent::WinGame` into it), `Sim::poll_net`'s real
/// `WinGame` arm, and `drive_ui_from_session` itself.
#[test]
fn drive_ui_from_session_opens_credits_on_the_real_win_game_event() {
    use crate::net::NetUpdate;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    // Reach a live-gameplay screen the same way `on_credits` (`menu/
    // nav.rs`'s own test helper) does — `show_credits` only leaves from
    // `Playing | Chat | Container | Paused`, matching `die`'s guard.
    app.ui.enter_dev_world();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: must be on a live-gameplay screen before WinGame arrives"
    );
    assert!(
        !app.sim.has_won(),
        "precondition: nothing has signalled a win yet"
    );

    feed.send(NetUpdate::WinGame).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.has_won(),
        "Sim::poll_net's real WinGame arm must latch the win"
    );
    // Precondition restated after the poll but before the real call this
    // test exercises, so the assertion below cannot be explained by
    // something upstream having already moved the screen.
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: drive_ui_from_session has not run yet"
    );

    app.drive_ui_from_session();

    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Credits,
        "the real WIN_GAME event (GAME_EVENT code 4, vanilla's own game-event packet handling) \
         must open the credits screen"
    );
}

/// **The owner's live report, reproduced deterministically and fixed.**
/// "accepting the custom resource pack didn't do anything, and it kept the
/// choice menu open. when i pressed accept again, it closed it and no
/// texture pack was applied at all."
///
/// Traced: `NetClient::respond_to_resource_pack` only *queues* the answer
/// for the net thread's own loop to drain (up to 15 ms later on native) —
/// [`crate::net::PackPromptCell`]'s doc used to claim the shared cell was
/// cleared "the instant" an answer was queued, which is false; only the
/// drain clears it. `app/session.rs`'s `drive_ui_from_session` reconciles
/// `Screen::ResourcePackPrompt` from that same cell every frame, and used to
/// reopen on any frame where the UI was closed but the (still-stale) cell
/// still reported the answered prompt pending — which a real session hits
/// on the very frame of the click, since the click handler and `redraw`
/// share one winit dispatch. That is symptom 1 and 2 exactly: the first
/// Accept appears to do nothing because the reopen is instant, and the
/// dialog "stays open". A real second click then answers the *same* id
/// again; net's `apply_pack_response` finds it already cleared and does
/// nothing further, so **only the first click's answer ever drives the
/// download** — but the player never got to see that, because the dialog
/// reopening ate their confirmation that anything had happened.
///
/// This drives the real chain end to end — a real `WindowApp`, the real
/// `MenuNav::click`/`apply_menu_action`/`respond_to_resource_pack` path, and
/// the real `drive_ui_from_session` — with a **loopback** `NetClient`
/// standing in for the net thread. That is not a weaker double here: a
/// loopback's `pack_response_tx` has no receiver at all (see
/// [`NetClient::loopback`]'s own doc), so the shared cell *never* clears —
/// the permanent, worst-case version of the up-to-15ms lag a real session
/// only suffers briefly. If the reconcile can stay closed against a ground
/// truth that never catches up, it certainly survives one that catches up
/// within a frame or two.
///
/// **Negative control, executed:** reverting `app/session.rs`'s reconcile to
/// its old unconditional `if !self.ui.is_resource_pack_prompt() {
/// show_resource_pack_prompt(...) }` (dropping the
/// `resource_pack_already_answered` check) makes this fail at the final
/// assertion — the screen is `ResourcePackPrompt` again, reproducing the
/// report.
#[test]
fn accepting_a_resource_pack_prompt_does_not_reopen_it_before_the_net_thread_catches_up() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions) = NetClient::loopback();
    app.sim.attach_net(net);
    app.ui.enter_dev_world();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::Playing,
        "precondition: a live-gameplay screen, one of the five the prompt can open over"
    );

    let id = uuid::Uuid::from_u128(0xF00D);
    app.sim
        .net()
        .expect("attached above")
        .set_pending_resource_pack_prompt_for_test(crate::net::PendingResourcePackPrompt::for_test(
            id, false,
        ));

    // The opening edge: a fresh reconcile with nothing shown yet must arm it.
    app.drive_ui_from_session();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ResourcePackPrompt,
        "precondition: the pushed prompt must open the confirm screen"
    );

    // The player's one and only click on Accept.
    let action = app.nav.click(&mut app.ui, crate::menu::confirm::ACCEPT_ROW);
    app.apply_menu_action(action);
    assert!(
        !app.ui.is_resource_pack_prompt(),
        "the click's own `apply_resource_pack_prompt` must close the screen immediately"
    );

    // The reconcile that used to reopen it: the loopback's shared cell still
    // reports `id` pending (nothing ever drains `respond_to_resource_pack`'s
    // queued answer on a loopback), which is exactly the stale read a real
    // net thread produces for its first ~15 ms.
    app.drive_ui_from_session();
    assert!(
        !app.ui.is_resource_pack_prompt(),
        "a single Accept must not reopen the very prompt it just answered, \
         even while the net thread has not yet cleared the shared cell — \
         this is the owner's \"accepting did nothing, it kept the choice \
         menu open\" report"
    );

    // And the suppression is scoped to *that* id, not sticky forever: a
    // genuinely different prompt (a second push superseding the first, the
    // same case `PackPromptCell::clear_if`'s own doc names) must still open
    // even though `resource_pack_answered_id` is still set from the first.
    app.sim
        .net()
        .expect("attached above")
        .set_pending_resource_pack_prompt_for_test(crate::net::PendingResourcePackPrompt::for_test(
            uuid::Uuid::from_u128(0xBEEF),
            false,
        ));
    app.drive_ui_from_session();
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ResourcePackPrompt,
        "a later, genuinely different prompt must still open — the \
         suppression must be scoped to the answered id, not sticky forever"
    );
}

/// Live gate: `ShellWeatherProbe::precipitation` must reach
/// a real per-column snow/rain decision now that the biome-climate lane
/// is wired, not the `Rain` it answered unconditionally before this
/// session (the shell's session path must not substitute an unconditional value).
///
/// Connects directly through `ClientBuilder`, bypassing `NetClient`'s
/// background thread so the raw event stream can be read here: the real
/// `ClientEvent::BiomeClimates` is captured off it and folded into a
/// `BiomeClimateCell` **by hand, with the same call** `net::forward`'s
/// arm makes — proving the fold, not merely trusting it — while every
/// other event is drained so the driver's bounded channel never blocks.
/// Mirrors `net::tests::live_entity_light_at_distinguishes_loaded_from_unloaded`'s
/// shape.
///
/// The expected precipitation per sampled column is computed **here**,
/// independently of both `ShellWeatherProbe` and `lodestone_render::
/// weather` — the raw climate is pulled straight off the `BiomeClimateCell`
/// and vanilla's own threshold is applied by hand, taken from the
/// decompiled source's behaviour rather than from this crate's constant:
/// vanilla's own warm-enough-to-rain check returns true when the
/// height-adjusted temperature is at least 0.15, and that check is what
/// vanilla's own precipitation-at-position resolve calls. A
/// wrong threshold in either implementation would show up as a mismatch
/// against this independently-computed expectation rather than agreeing
/// with itself — the `decode(encode(x)) == x` trap `CLAUDE.md` warns
/// about, avoided by never calling `precipitation_for_temperature`/
/// `height_adjusted_temperature` from this test.
///
/// ```text
/// cargo test -p lodestone-shell --features live --lib \
///     app::tests::live_precipitation_matches_vanillas_own_threshold_for_real_biomes \
///     -- --ignored --nocapture
/// ```
#[cfg(feature = "live")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-survival server on 127.0.0.1:25565"]
async fn live_precipitation_matches_vanillas_own_threshold_for_real_biomes() {
    use crate::net::BiomeClimateCell;
    use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
    use lodestone_render::WeatherProbe as _;
    use lodestone_testsupport::{poll_until, unique_username};

    let user = unique_username();
    let protocol = 776; // vanilla 26.2 — the `live` feature's compiled-in family
    let adapter = lodestone_registry::adapter_for_protocol(protocol)
        .expect("the `live` feature compiles a family in for protocol 776");
    let (handle, mut events) = ClientBuilder::new(
        ServerAddress {
            host: "127.0.0.1".into(),
            port: 25565,
        },
        LoginProfile {
            username: user.clone(),
            uuid: uuid::Uuid::new_v4(),
        },
        adapter,
    )
    .connect()
    .await
    .expect("connect to lodestone-survival on 127.0.0.1:25565");

    let climates = Arc::new(BiomeClimateCell::default());
    let climates_thread = Arc::clone(&climates);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let lodestone_model::ClientEvent::BiomeClimates {
                temperatures,
                downfall,
                has_precipitation,
            } = event
            {
                // The exact fold `net::forward`'s `BiomeClimates` arm
                // makes — called here by hand since this test bypasses
                // `forward` entirely to read the raw stream.
                climates_thread.apply(&temperatures, &downfall, &has_precipitation);
            }
        }
    });

    assert!(
        poll_until(
            Duration::from_secs(30),
            Duration::from_millis(100),
            || async {
                handle
                    .players()
                    .into_iter()
                    .find(|p| p.name.as_deref() == Some(user.as_str()))
            }
        )
        .await
        .is_some(),
        "player {user} never reached Play on the oracle"
    );

    let dims = poll_until(
        Duration::from_secs(10),
        Duration::from_millis(100),
        || async { handle.world_dimensions() },
    )
    .await
    .expect("world dimensions never arrived");

    let loaded = poll_until(
        Duration::from_secs(15),
        Duration::from_millis(200),
        || async {
            let chunks = handle.loaded_chunks();
            if chunks.is_empty() { None } else { Some(chunks) }
        },
    )
    .await
    .expect("no chunks streamed in within 15s of login");

    // The registry (and with it `BiomeClimates`) lands at `Login`, ahead
    // of chunk data, but poll rather than assume the ordering: this test
    // cares about the fold having happened, not about racing it.
    assert!(
        poll_until(Duration::from_secs(10), Duration::from_millis(100), || {
            let climates = Arc::clone(&climates);
            async move { climates.get(0).map(|_| ()) }
        })
        .await
        .is_some(),
        "ClientEvent::BiomeClimates never arrived — the climate table is still empty"
    );

    let handle = Arc::new(handle);
    let probe = ShellWeatherProbe {
        light: 1.0,
        handle: Some(Arc::clone(&handle)),
        biome_climates: Some(Arc::clone(&climates)),
        // One `section_at` per distinct chunk column rather than per call — this
        // gate samples 16 different columns, so it fetches 16 sections and reuses
        // none, which is exactly the memo's contract.
        memo: Default::default(),
    };

    // Sample a real column in the middle of a loaded chunk, at mid-build-
    // height. `checked` and `snow_seen`/`rain_seen` are reported in the
    // panic message so a failure names the real biome and climate
    // involved, not just "mismatch".
    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();
    for chunk in loaded.iter().take(16) {
        let y = dims.min_y + (dims.height as i32 / 2);
        let block_x = chunk.x * 16 + 8;
        let block_z = chunk.z * 16 + 8;
        let base_si = dims.min_y.div_euclid(16);
        let si = y.div_euclid(16) - base_si;
        if si < 0 || (si as usize) >= dims.section_count() {
            continue;
        }
        let Some(section) = handle.section_at(*chunk, si as usize) else {
            continue;
        };
        let biome = section.biome_at_block(8, y.rem_euclid(16) as usize, 8);
        let Some(climate) = climates.get(usize::try_from(biome).unwrap_or(usize::MAX)) else {
            continue;
        };
        let (Some(temperature), Some(has_precipitation)) =
            (climate.temperature, climate.has_precipitation)
        else {
            continue;
        };
        checked += 1;

        // Independent re-derivation, not a call to `lodestone_render::
        // weather`: vanilla's own height falloff
        // (its own height-adjusted-temperature computation)
        // and its own rain/snow threshold (`0.15F`).
        let above = (y - crate::worldgen::SEA_LEVEL) as f32;
        let adjusted = if above > 0.0 {
            temperature - above * 0.05 / 40.0
        } else {
            temperature
        };
        let expected = if !has_precipitation {
            lodestone_render::Precipitation::None
        } else if adjusted >= 0.15 {
            lodestone_render::Precipitation::Rain
        } else {
            lodestone_render::Precipitation::Snow
        };

        let actual = probe.precipitation(block_x, y, block_z);
        println!(
            "chunk {chunk:?} biome {biome} temperature={temperature} \
             has_precipitation={has_precipitation} adjusted={adjusted} -> {expected:?}"
        );
        if actual != expected {
            mismatches.push(format!(
                "chunk {chunk:?} biome {biome} temperature={temperature} \
                 has_precipitation={has_precipitation} adjusted={adjusted}: \
                 expected {expected:?}, probe returned {actual:?}"
            ));
        }
    }

    assert!(
        checked > 0,
        "no loaded column resolved a section + biome + climate — the wiring \
         chain (section_at → biome_at_block → BiomeClimateCell) never \
         produced real data to check against"
    );
    assert!(
        mismatches.is_empty(),
        "{}/{checked} sampled columns disagreed with vanilla's own threshold: \
         {mismatches:#?}",
        mismatches.len()
    );

    drain.abort();
}

/// **Recipe-book settings synchronization, exercised through production code.**
///
/// `RECIPE_BOOK_SETTINGS` (76) decoded and folded as of `fd53995` and
/// **nothing read it**: the recipe-book panel started closed and unfiltered
/// on every join no matter what the server said. This does not call
/// `RecipeBookSettings::for_type` a second time by hand — that would be the
/// existing unit test again, which proves nothing about production. It drives
/// the real chain: a real `WindowApp`, a real `ClientEvent` folded through the
/// same `NetIngest` schedule the net thread runs, and
/// `drive_ui_from_session` itself — the method `redraw()` calls every frame.
///
/// The `open` bit is the pixel-visible one: `RecipePanelState::open` is what
/// `recipe_panel_geometry` turns into the panel body's vertices, gated by
/// `an_open_panel_covers_its_own_screen_rect` / the closed-panel control in
/// `recipe_book_wiring.rs`.
#[test]
fn drive_ui_from_session_restores_the_recipe_book_panel_the_server_reported() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    // The restore only runs with a recipe-book-bearing menu on screen: the
    // player inventory's own 2x2 grid makes `recipe_book_type_for` answer
    // `Crafting`. Reach it the way a player does.
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Precondition, and it is load-bearing: an unreported record is all-false,
    // which is indistinguishable from "the server wants it closed". If the
    // panel were somehow already open, the assertion below would pass without
    // the restore ever running.
    assert!(
        !app.recipe_panel.open,
        "precondition: the panel must start closed — that is the defect being fixed"
    );
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "control: with nothing reported, the restore must NOT fire — otherwise \
         this gate cannot tell a real restore from the default it replaces"
    );

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings { open: true, filtering: true },
            furnace: RecipeBookTypeSettings::default(),
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        app.recipe_panel.open,
        "the crafting book's reported `open` must reach the panel the draw reads"
    );
    assert!(
        app.recipe_panel.filtering,
        "and so must `filtering` — the All/Craftable state"
    );

    // The latch: a user who closes the panel must not have it reopened on the
    // very next frame by the same reported settings.
    app.recipe_panel.open = false;
    app.drive_ui_from_session();
    assert!(
        !app.recipe_panel.open,
        "the restore is once per book type, not every frame — otherwise it \
         would fight the user's own clicks"
    );
}

/// The **negative control** for the gate above, run and observed: the furnace
/// book's settings must not restore into a crafting panel.
///
/// # What this control can and cannot see — measured, not assumed
///
/// Neutering `for_type(book_type)` to `settings.furnace` fails **both** this
/// test and the positive one above (observed). So the pair really does pin the
/// per-type read in that direction.
///
/// It does **not** catch a restore hardcoded to `settings.crafting`: that was
/// tried, and both tests stayed green. The reason is a property of the
/// harness, not of the assertions — `active_container_menu` here resolves to
/// the *player inventory*, whose 2×2 grid makes `recipe_book_type_for` answer
/// `Crafting`, so `crafting` **is** the correct field for every scenario this
/// harness can construct. Putting a furnace on screen needs a server-opened
/// menu (`Sim::open_menu`), which this loopback feed has no route to.
///
/// Recorded rather than quietly left as a gap: a control whose premise is
/// false fails in the safe-looking direction, and the way to find that out is
/// to run the neuter and watch it *not* fire. Whoever gains a furnace-menu
/// harness should extend this test rather than write a third one.
#[test]
fn a_crafting_panel_does_not_restore_the_furnace_books_settings() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookTypeSettings;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();

    // Only the *furnace* book is open, and it is a different book than the
    // player-inventory crafting grid on screen.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookSettingsChanged {
            crafting: RecipeBookTypeSettings::default(),
            furnace: RecipeBookTypeSettings { open: true, filtering: true },
            blast_furnace: RecipeBookTypeSettings::default(),
            smoker: RecipeBookTypeSettings::default(),
        });

    app.drive_ui_from_session();

    assert!(
        !app.recipe_panel.open,
        "the furnace book's `open` must NOT open the crafting panel — the \
         restore has to read `for_type(book_type)`, not the first field"
    );
    assert!(
        !app.recipe_panel.filtering,
        "same for `filtering`"
    );
}

/// The recipe-unlock toast's missing hop: `SessionRecipeBook` was folded and
/// read by nothing, so `RecipeToastQueue::push` had zero production callers
/// and the toast could never appear. Drives the real chain: a real
/// `WindowApp`, a real `ClientEvent::RecipeBookAdded` folded through the same
/// `NetIngest` schedule the net thread runs, and `drive_ui_from_session`
/// itself — the method `redraw()` calls every frame, which is where
/// `WindowApp::sync_recipe_toasts` now lives.
///
/// Two properties, both load-bearing:
///
/// * the first sync (`replace: true`, vanilla's join-time seed) must seed the
///   "already toasted" set and toast **nothing** — vanilla does not toast a
///   fresh join's entire unlock history;
/// * a genuinely new unlock **after** that must reach
///   [`crate::hud::HudFrame::recipe_toast`]'s own producer,
///   [`recipe_toast_view`], with the *station* and *unlocked* item ids the
///   decode carried — not transposed, and not the discarded `_category` this
///   feature used to drop instead.
#[test]
fn drive_ui_from_session_toasts_a_newly_unlocked_recipe_but_not_the_join_time_seed() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookEntry;

    let torch = lodestone_model::ItemId::canonical(u32::from(Item::Torch.registry_id()));
    let crafting_table =
        lodestone_model::ItemId::canonical(u32::from(Item::CraftingTable.registry_id()));

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 1,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: true,
                highlight: false,
            }],
            replace: true,
        });
    app.drive_ui_from_session();
    assert!(
        recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms()).is_none(),
        "the first join-time sync must seed the seen set, not replay the \
         whole unlock history as toasts"
    );

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 2,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: true,
                highlight: false,
            }],
            replace: false,
        });
    app.drive_ui_from_session();

    let view = recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms())
        .expect("a newly-unlocked, notifying recipe must reach the toast queue");
    assert_eq!(
        view.station.item.to_string(),
        "minecraft:crafting_table",
        "the station icon must be the crafting station, not the unlocked item"
    );
    assert_eq!(
        view.unlocked.item.to_string(),
        "minecraft:torch",
        "the unlocked icon must be the result item, not the station"
    );
}

/// The control for the gate above: a recipe with `notification: false` must
/// never toast, even on a later (non-seeding) sync — vanilla's tab-highlight-
/// only unlocks are silent.
#[test]
fn a_non_notifying_unlock_never_toasts() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::event::RecipeBookEntry;

    let torch = lodestone_model::ItemId::canonical(u32::from(Item::Torch.registry_id()));
    let crafting_table =
        lodestone_model::ItemId::canonical(u32::from(Item::CraftingTable.registry_id()));

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);

    // Seed with an empty first sync.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: Vec::new(),
            replace: true,
        });
    app.drive_ui_from_session();

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 5,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: false,
        });
    app.drive_ui_from_session();

    assert!(
        recipe_toast_view(&app.recipe_toasts, recipe_toast_now_ms()).is_none(),
        "notification: false must never raise a toast"
    );
}

/// The recipe-seen packet: its encoder existed in every protocol
/// family and nothing anywhere called it. Drives the real chain: a real
/// `WindowApp`, a real `ClientEvent::RecipeBookAdded` folded through the same
/// `NetIngest` schedule the net thread runs, a recipe corpus loaded so the
/// panel's page actually contains the unlocked result, and
/// `drive_ui_from_session` itself, which is where `WindowApp::sync_recipe_book_seen`
/// now lives.
///
/// Two properties: the recipe must actually be **on the open page** (vanilla
/// only fires this for a populated recipe button, not for the whole
/// corpus), and reporting it once must not report it again next frame — the
/// dedup [`WindowApp::recipe_book_seen`] exists for.
#[test]
fn drive_ui_from_session_reports_a_visible_highlighted_recipe_as_seen_exactly_once() {
    use crate::net::NetUpdate;
    use lodestone_client::{ClientAction, ClientEvent};
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
    use lodestone_model::event::RecipeBookEntry;

    let torch_id: lodestone_model::Identifier = "minecraft:torch".parse().unwrap();
    let mut book = RecipeBook::new();
    book.insert(
        torch_id.clone(),
        Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item("minecraft:coal".parse().unwrap())),
                Some(Ingredient::Item("minecraft:stick".parse().unwrap())),
            ],
            ItemStack::new(torch_id, 4),
        )),
    );

    let torch = lodestone_model::ItemId::canonical(u32::from(Item::Torch.registry_id()));
    let crafting_table =
        lodestone_model::ItemId::canonical(u32::from(Item::CraftingTable.registry_id()));

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.recipe_book = Some(book);
    let (net, actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();
    app.recipe_panel.open = true;
    // Drain the login-time traffic this harness generates so the assertion
    // below is exactly the seen-recipe send, not an artifact of setup.
    while actions.try_recv().is_ok() {}

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 3,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: true,
        });
    app.drive_ui_from_session();

    assert_eq!(
        actions.try_recv(),
        Ok(ClientAction::RecipeBookSeenRecipe { recipe: 3 }),
        "a highlighted recipe visible on the open page must be reported seen"
    );
    assert!(
        actions.try_recv().is_err(),
        "exactly one report for one newly-shown recipe"
    );

    // The dedup control: the same recipe stays on the same page next frame,
    // and must not be re-reported.
    app.drive_ui_from_session();
    assert!(
        actions.try_recv().is_err(),
        "a recipe already reported seen must not be reported again"
    );
}

/// The control for the gate above: a highlighted recipe that is **not** on
/// the currently open page (the panel is closed) must never be reported —
/// the client only fires this for a populated recipe button.
#[test]
fn a_highlighted_recipe_is_not_reported_while_the_panel_is_closed() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_game::item::ItemStack;
    use lodestone_game::recipe::{Ingredient, Recipe, RecipeBook, ShapedRecipe};
    use lodestone_model::event::RecipeBookEntry;

    let torch_id: lodestone_model::Identifier = "minecraft:torch".parse().unwrap();
    let mut book = RecipeBook::new();
    book.insert(
        torch_id.clone(),
        Recipe::Shaped(ShapedRecipe::new(
            1,
            2,
            vec![
                Some(Ingredient::Item("minecraft:coal".parse().unwrap())),
                Some(Ingredient::Item("minecraft:stick".parse().unwrap())),
            ],
            ItemStack::new(torch_id, 4),
        )),
    );

    let torch = lodestone_model::ItemId::canonical(u32::from(Item::Torch.registry_id()));
    let crafting_table =
        lodestone_model::ItemId::canonical(u32::from(Item::CraftingTable.registry_id()));

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    app.recipe_book = Some(book);
    let (net, actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();
    app.ui.open_container();
    // Deliberately left closed, unlike the positive gate above.
    assert!(!app.recipe_panel.open, "precondition: the panel starts closed");
    while actions.try_recv().is_ok() {}

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::RecipeBookAdded {
            entries: vec![RecipeBookEntry {
                display_id: 3,
                result_items: vec![torch],
                station_items: vec![crafting_table],
                group: None,
                category: 0,
                crafting_requirements: None,
                notification: false,
                highlight: true,
            }],
            replace: true,
        });
    app.drive_ui_from_session();

    assert!(
        actions.try_recv().is_err(),
        "a closed panel must never report a recipe seen, however unlocked it is"
    );
}

/// The owner's report: right-clicking a server-side "server selector" opens a
/// container, selecting a row makes the **server** close it, and our client
/// showed the player's own inventory instead of returning to gameplay —
/// vanilla shows no screen at all.
///
/// Drives the real chain a server-initiated close takes: a real `WindowApp`,
/// a real server-opened (non-zero window id) menu folded through the same
/// `NetIngest` schedule the net thread runs (`ScreenOpened` + a matching
/// `ContainerContent`, exactly as `lodestone-ecs`'s own
/// `menu_family_events_reach_session_menus_through_the_real_schedule` proves
/// reaches `SessionMenus`), then a server `ScreenClosed` for the same window
/// — and `drive_ui_from_session`, the method `redraw()` calls every frame,
/// which is where `UiState::reconcile_server_menu_window` now lives.
///
/// The `open_container()` call below stands in for `redraw()`'s own
/// `Sim::open_menu().is_some() && is_playing()` branch (untouched by this
/// fix, and not itself under test here — the recipe-book tests above already
/// drive it the same indirect way for the same reason: no GPU in this
/// harness).
#[test]
fn server_initiated_container_close_returns_to_gameplay_not_the_player_inventory() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;
    use lodestone_model::Text;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    let ingest = |app: &WindowApp, event: ClientEvent| {
        app.sim.net().expect("net attached above").ingest_session_event(event);
    };

    // A real 9x3 chest: 27 container slots + 36 player-inventory slots, the
    // same shape `lodestone-ecs`'s own menu-family test uses.
    ingest(
        &app,
        ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: "minecraft:generic_9x3".parse().expect("valid resource key"),
            title: Text::literal("Chest"),
        },
    );
    ingest(
        &app,
        ClientEvent::ContainerContent {
            window_id: 5,
            state_id: lodestone_model::ContainerStateId::new(1),
            items: vec![None; 63],
            carried_item: None,
        },
    );
    assert_eq!(
        app.sim.open_menu().map(|open| open.window_id),
        Some(5),
        "precondition: a real server menu is open before the close"
    );

    // Stand-in for `redraw()`'s open branch (see this test's own doc).
    app.ui.open_container();
    app.drive_ui_from_session();
    assert!(
        app.active_container_menu().is_some(),
        "precondition: the container screen is actually showing the server's \
         menu before it closes"
    );

    // The server closes window 5 — the container-close packet is decoded
    // into `ClientEvent::ScreenClosed`.
    ingest(&app, ClientEvent::ScreenClosed { window_id: 5 });
    assert_eq!(
        app.sim.open_menu(),
        None,
        "control: the menu-state half of the close (vanilla's `containerMenu \
         = inventoryMenu`) must actually have landed, or this test cannot \
         tell that half apart from the screen half under test"
    );

    app.drive_ui_from_session();

    // The exact expected UI state, not merely "the container screen is
    // gone": `active_container_menu` is `None` (**no** screen), matching
    // vanilla's unconditional close-to-no-screen — not the
    // player's own inventory, which is what the bug showed instead
    // (`active_container_menu`'s window-0 fallback firing off a stale
    // `Screen::Container` the close never reset).
    assert_eq!(
        app.active_container_menu(),
        None,
        "a server-initiated close must show no screen at all, not the \
         player's own inventory"
    );
    assert!(
        app.ui.is_playing(),
        "and the screen itself must have returned to Playing"
    );
}

/// Negative control for the gate above: the player's own `E`-opened
/// inventory — which never has a server window id at all — must **not** be
/// closed by [`UiState::reconcile_server_menu_window`]. A level check on "no
/// window id right now" would close this the very next frame; only an edge
/// on a real `Some -> None` transition may.
#[test]
fn opening_the_local_inventory_with_no_server_window_is_not_closed_by_the_server_reconciler() {
    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(crate::net::NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    // `E` with nothing else open: no server window, ever.
    app.ui.open_container();
    assert_eq!(app.sim.open_menu(), None, "precondition: no server menu exists");

    // Several frames, matching the steady state a player leaving their own
    // inventory open would sit in.
    for _ in 0..3 {
        app.drive_ui_from_session();
    }

    assert!(
        app.ui.is_container_open(),
        "the local inventory must stay open with no server window ever having existed"
    );
    assert!(
        app.active_container_menu().is_some(),
        "and it must still be showing the player's own menu"
    );
}

/// **Game-rule synchronization, exercised through production code.**
///
/// The immediate-respawn rule is the most user-visible game rule there is: the
/// reference client never puts the death screen up at all when it is on.
/// `SessionGameRules`
/// was folded, reset on quit-to-title and gated through the real
/// `SharedState::apply` path with **no reader anywhere in the shell**, so the
/// rule did nothing.
///
/// Drives the real chain: a real `WindowApp`, a real `NetUpdate::Death`
/// through the loopback feed (`Sim::poll_net`'s own arm, which sets the `Dead`
/// marker), a real `ClientEvent::GameRulesChanged` through the same
/// `NetIngest` schedule the net thread runs, and `drive_ui_from_session`
/// itself — the method `redraw()` calls every frame.
#[test]
fn immediate_respawn_skips_the_death_screen_entirely() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "true".into(),
            )],
        });
    assert_eq!(
        app.sim.game_rules().immediate_respawn(),
        Some(true),
        "precondition: the rule must actually have folded, or this gate is \
         measuring the default and not the rule"
    );

    feed.send(NetUpdate::Death { message: lodestone_model::Text::literal("you died") }).unwrap();
    app.sim.step(1.0 / 20.0);
    assert!(
        app.sim.is_dead(),
        "precondition: the death must have landed, or 'no death screen' is vacuous"
    );

    app.drive_ui_from_session();

    assert!(
        !app.ui.is_death(),
        "with doImmediateRespawn on, the death screen must never appear — not \
         'appear and close next frame', which would flash it for a frame"
    );
    assert_ne!(
        app.ui.screen(),
        crate::menu::Screen::Death,
        "and the screen state must not be Death by any other route"
    );
}

/// **The negative control, run and observed**: the *same* death with the rule
/// off must still raise the death screen.
///
/// Without this, `immediate_respawn_skips_the_death_screen_entirely` is
/// satisfied by a client that never shows a death screen at all — which is
/// exactly the state a broken `is_dead` or a broken loopback feed would
/// produce, and it would read as a pass.
#[test]
fn without_the_rule_the_same_death_still_raises_the_death_screen() {
    use crate::net::NetUpdate;
    use lodestone_client::ClientEvent;

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    app.sim.attach_net(net);
    feed.send(NetUpdate::LoggedIn { entity_id: 1 }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.ui.enter_dev_world();

    // Explicitly `false`, not merely absent: `Some(false)` and `None` take
    // different branches in `immediate_respawn()`, and the shipped behaviour
    // must be identical for both.
    app.sim
        .net()
        .expect("net attached above")
        .ingest_session_event(ClientEvent::GameRulesChanged {
            values: vec![(
                "immediate_respawn".parse().expect("valid identifier"),
                "false".into(),
            )],
        });
    assert_eq!(app.sim.game_rules().immediate_respawn(), Some(false));

    feed.send(NetUpdate::Death { message: lodestone_model::Text::literal("you died") }).unwrap();
    app.sim.step(1.0 / 20.0);
    app.drive_ui_from_session();

    assert!(
        app.ui.is_death(),
        "with the rule off, the death screen must still appear — this is what \
         proves the gate above is measuring the rule and not a client that \
         never shows the screen"
    );
}
