//! Live acceptance: scoreboard, teams and boss bar end-to-end through the *real*
//! client.
//!
//! This is the gate the brief demands, phrased so it cannot pass on decode
//! alone: it drives `/scoreboard`, `/team` and `/bossbar` over RCON against a
//! live 26.2 server, then asserts the values are readable through the client's
//! **public API** — [`ClientHandle::scoreboard()`] and
//! [`ClientHandle::boss_bars()`] — not through the v770 decoder. The whole path
//! is exercised: v770 decodes `set_objective`/`set_display_objective`/
//! `set_score`/`set_player_team`/`boss_event`, the adapter lifts each into a
//! canonical `ClientEvent`, the client folds it into its read-model, and the
//! public accessors return it. A misparse anywhere shows up as a missing or
//! wrong value.
//!
//! Anti-vacuity, three layers:
//!   1. It asserts *specific* server-set values — score `42`, boss progress
//!      `0.7`, team colour red, the display-name strings — so a transposed
//!      layout that round-trips happily against a hand-built fixture cannot
//!      survive here, and the poll times out with a loud panic (never a silent
//!      skip) if the server is unreachable or the state never arrives.
//!   2. **Negative controls that prove the detector fires.** A positive-only
//!      gate can pass against a read-model that returns `Some(_)` for
//!      everything. So this also asserts: a holder we never set has *no* score
//!      while the set holder does; a team we never created is absent; and —
//!      crucially — after `scoreboard players reset` the score transitions
//!      present→absent, and after `scoreboard objectives remove` the objective
//!      transitions present→absent. Those two transitions are only meaningful
//!      because we first proved the value *was* present, and they are the only
//!      thing in this file that exercises the `RESET_SCORE` and
//!      `ObjectiveMode::Remove` packet paths at all.
//!   3. **An anti-vacuity floor.** `checked` is incremented only after a real
//!      comparison actually ran; the test asserts `checked >= EXPECTED_CHECKS`
//!      at the end, so a future refactor that accidentally skips assertions
//!      (the §12.56 shape: `?`/`continue` swallowing a branch) fails loudly
//!      instead of passing vacuously. I proved the floor bites by temporarily
//!      raising `EXPECTED_CHECKS` and watching it fail with the true count.
//!
//! Gated behind the `live-scoreboard` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic and version-free. Run against the creative
//! server on `127.0.0.1:25570` (RCON `:25571`):
//!
//! ```text
//! cargo test -p lodestone-game --features live-scoreboard \
//!     --test live_scoreboard -- --ignored --nocapture
//! ```
#![cfg(feature = "live-scoreboard")]

use std::time::Duration;

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_model::{BossColor, DisplaySlot, ObjectiveRenderType, TeamColor};
use lodestone_testsupport::{AsyncRconClient as Rcon, poll_until, unique_username};
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25570;
const RCON_PORT: u16 = 25571;
const RCON_PASSWORD: &str = "lodestone";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the lodestone-creative server on 127.0.0.1:25570 (RCON :25571)"]
async fn scoreboard_teams_bossbar_reach_client_public_api() {
    println!("=== LIVE SCOREBOARD/TEAM/BOSSBAR ORACLE (protocol 776, creative :25570) ===");

    // Collision-proof by construction (AtomicU64 + pid); reused so the objective,
    // team and boss-bar names all share one unique suffix.
    let holder = unique_username();
    let objective = holder.clone();
    let team = holder.clone();
    // Boss-bar ids are ResourceLocations: path must be lowercase [a-z0-9/._-].
    let bar = format!("lodestone:{}", holder.to_lowercase());
    println!("holder/objective/team = {holder}; bossbar = {bar}");

    let server = ServerAddress {
        host: HOST.into(),
        port: PORT,
    };
    let profile = LoginProfile {
        username: holder.clone(),
        uuid: Uuid::new_v4(),
    };

    // Version selection through the registry; `lodestone-game` names no version.
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via lodestone-client/live-v770");

    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect(
            "connect to lodestone-creative on 127.0.0.1:25570 — start it with: \
             docker run --rm -d -p 25570:25570 -p 25571:25571 --name lodestone-creative <creative-image>",
        );

    // The driver pushes events onto a *bounded* channel, so something must keep
    // draining it or the driver backpressures and stops folding. A background
    // task drains forever; the main flow observes state through the public API.
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let ClientEvent::Disconnect { reason } = event {
                eprintln!("driver saw disconnect: {}", reason.to_plain_string());
                break;
            }
        }
    });

    // Wait until the server knows our player (we've reached Play) before issuing
    // commands that target it by name — otherwise `/scoreboard players set` runs
    // against a player the server hasn't spawned yet.
    let ready = poll_until(
        Duration::from_secs(30),
        Duration::from_millis(100),
        || async {
            handle
                .players()
                .into_iter()
                .find(|p| p.name.as_deref() == Some(holder.as_str()))
        },
    )
    .await;
    assert!(
        ready.is_some(),
        "player {holder} never appeared in the live tab list — is lodestone-creative on :25570 in Play? \
         (alive={})",
        handle.is_alive()
    );
    println!("player is in-game; issuing scoreboard/team/bossbar commands over RCON");

    let mut rcon = Rcon::connect((HOST, RCON_PORT), RCON_PASSWORD)
        .await
        .expect(
            "connect RCON on 127.0.0.1:25571 (password 'lodestone') — is lodestone-creative up?",
        );

    // Order matters: create + display the objective before setting the score so
    // the client is tracking it; set the boss bar's final style/value before
    // adding our player so the ADD packet we receive carries the final state.
    for cmd in [
        format!("scoreboard objectives add {objective} dummy \"LodeBoard\""),
        format!("scoreboard objectives setdisplay sidebar {objective}"),
        format!("scoreboard players set {holder} {objective} 42"),
        format!("team add {team} \"LodeTeam\""),
        format!("team modify {team} color red"),
        format!("team modify {team} prefix \"[L] \""),
        format!("team join {team} {holder}"),
        format!("bossbar add {bar} \"LodeBoss\""),
        format!("bossbar set {bar} max 100"),
        format!("bossbar set {bar} value 70"),
        format!("bossbar set {bar} color red"),
        format!("bossbar set {bar} players {holder}"),
    ] {
        let resp = rcon.cmd(&cmd).await;
        println!("  RCON `{cmd}` -> {resp:?}");
    }

    // Poll, never assert immediately: the effects are tick-published and arrive
    // as separate packets. Wait until every piece is visible through the public
    // API, then snapshot the concrete values for assertion.
    struct Snapshot {
        objective_display: Option<String>,
        render_type: Option<ObjectiveRenderType>,
        score: Option<i32>,
        displayed: Option<String>,
        team_color: Option<TeamColor>,
        team_prefix: String,
        team_has_member: bool,
        boss_title: String,
        boss_progress: f32,
        boss_color: BossColor,
    }

    let snap = poll_until(
        Duration::from_secs(30),
        Duration::from_millis(150),
        || async {
            let sb = handle.scoreboard();
            let obj = sb.objective(&objective)?;
            let score = sb.score(&objective, &holder)?;
            let team = sb.team(&team)?;
            let bars = handle.boss_bars();
            let bar_state = bars
                .iter()
                .find(|b| b.title.to_plain_string() == "LodeBoss")?;
            Some(Snapshot {
                objective_display: obj.display_name.as_ref().map(|t| t.to_plain_string()),
                render_type: obj.render_type,
                score: Some(score.value),
                displayed: sb.displayed(DisplaySlot::Sidebar).map(str::to_owned),
                team_color: team.params.color,
                team_prefix: team.params.prefix.to_plain_string(),
                team_has_member: team.members.iter().any(|m| m == &holder),
                boss_title: bar_state.title.to_plain_string(),
                boss_progress: bar_state.progress,
                boss_color: bar_state.color,
            })
        },
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "scoreboard/team/bossbar state never reached the client public API \
             within 30s (alive={}); objective={objective} team={team} bar={bar}",
            handle.is_alive()
        )
    });

    // Concrete, server-set values — a misparse or transposed field fails these.
    // `checked` is bumped only after a comparison actually runs (anti-vacuity
    // floor asserted at the end).
    let mut checked = 0usize;

    assert_eq!(
        snap.objective_display.as_deref(),
        Some("LodeBoard"),
        "objective display name via ClientHandle::scoreboard()"
    );
    checked += 1;
    assert_eq!(
        snap.render_type,
        Some(ObjectiveRenderType::Integer),
        "dummy criterion renders as an integer"
    );
    checked += 1;
    assert_eq!(snap.score, Some(42), "score value set over RCON");
    checked += 1;
    assert_eq!(
        snap.displayed.as_deref(),
        Some(objective.as_str()),
        "sidebar display slot points at our objective"
    );
    checked += 1;
    assert_eq!(
        snap.team_color,
        Some(TeamColor::Red),
        "team colour set to red over RCON"
    );
    checked += 1;
    assert_eq!(snap.team_prefix, "[L] ", "team prefix component");
    checked += 1;
    assert!(snap.team_has_member, "our player joined the team");
    checked += 1;
    assert_eq!(snap.boss_title, "LodeBoss", "boss bar title component");
    checked += 1;
    assert!(
        (snap.boss_progress - 0.7).abs() < 1e-4,
        "boss progress 70/100 = 0.7, got {}",
        snap.boss_progress
    );
    checked += 1;
    assert_eq!(snap.boss_color, BossColor::Red, "boss bar colour");
    checked += 1;

    // Negative controls, in-phase: prove the read-model *discriminates* rather
    // than returning `Some(_)` for everything. These run while the objective and
    // team are known-present, so a `None` here is a genuine "not found", not a
    // "nothing loaded yet".
    {
        let sb = handle.scoreboard();
        assert_eq!(
            sb.score(&objective, "player.we.never.set"),
            None,
            "a holder we never scored must have no score — proves score() keys on holder"
        );
        checked += 1;
        assert!(
            sb.team("team.we.never.created").is_none(),
            "a team we never created must be absent — proves team() keys on name"
        );
        checked += 1;
    }

    // Transition controls: the strongest negative controls. We already proved
    // the score and objective were *present* above; now mutate the server and
    // require the client to observe present->absent. This is the only coverage
    // in the file for the RESET_SCORE and ObjectiveMode::Remove packet paths, so
    // a decoder/adapter/read-model break on either shows up as a timeout here.
    let reset_resp = rcon
        .cmd(&format!("scoreboard players reset {holder} {objective}"))
        .await;
    println!("  RCON `scoreboard players reset {holder} {objective}` -> {reset_resp:?}");
    let score_cleared = poll_until(Duration::from_secs(15), Duration::from_millis(150), || async {
        // Objective still present, but this holder's score must vanish.
        match handle.scoreboard().score(&objective, &holder) {
            None => Some(()),
            Some(_) => None,
        }
    })
    .await;
    assert!(
        score_cleared.is_some(),
        "score for {holder} was still present 15s after `players reset`; the client never \
         observed RESET_SCORE (alive={})",
        handle.is_alive()
    );
    checked += 1;

    let remove_resp = rcon
        .cmd(&format!("scoreboard objectives remove {objective}"))
        .await;
    println!("  RCON `scoreboard objectives remove {objective}` -> {remove_resp:?}");
    let objective_removed = poll_until(Duration::from_secs(15), Duration::from_millis(150), || async {
        match handle.scoreboard().objective(&objective) {
            None => Some(()),
            Some(_) => None,
        }
    })
    .await;
    assert!(
        objective_removed.is_some(),
        "objective {objective} was still present 15s after `objectives remove`; the client \
         never observed ObjectiveUpdate::Remove (alive={})",
        handle.is_alive()
    );
    checked += 1;

    // Anti-vacuity floor: if a refactor silently drops assertions, this bites.
    const EXPECTED_CHECKS: usize = 14;
    assert!(
        checked >= EXPECTED_CHECKS,
        "anti-vacuity floor: only {checked} comparisons ran, expected >= {EXPECTED_CHECKS} — \
         an assertion was skipped, the gate is no longer proving what it claims"
    );

    // Best-effort cleanup of the remaining shared state (objective already
    // removed by the transition control above; the server is shared and --rm).
    let _ = rcon.cmd(&format!("team remove {team}")).await;
    let _ = rcon.cmd(&format!("bossbar remove {bar}")).await;

    println!(
        "=== SCOREBOARD ORACLE PASSED: {checked} comparisons (positive values + negative \
         controls + present->absent transitions) through ClientHandle::scoreboard() + \
         boss_bars() ==="
    );
    handle.shutdown();
    drain.abort();
}
