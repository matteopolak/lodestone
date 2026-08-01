//! Live gate for **"the world is fullbright, and the mobs look like they're in
//! the daytime."**
//!
//! # The defect this exists to catch
//!
//! Terrain and mobs are both darkened by one number — the factor the *sky* half
//! of the lightmap is scaled by, `sky_darken_for_time_of_day(time_of_day)`, which
//! `RenderState` folds into the group-0 fog lane both the model and entity passes
//! sample. Two gates already covered the two ends of that chain and **both were
//! green while the bug was on screen**:
//!
//! * `lodestone-render/tests/entity_night_pixels.rs` renders a mob at a
//!   *hand-supplied* `sky_darken` and proves the shader responds.
//! * `lodestone-render/tests/grass_light_response_gate.rs` renders terrain at a
//!   *hand-supplied* light byte and proves the shader responds.
//!
//! Neither touches where the number comes from. The number came from
//! `ClientEvent::TimeChanged::time_of_day`, and that field was carrying the
//! monotonic **world age** rather than the day clock: 26.2's `set_time` mostly
//! ships an empty clock map (`MinecraftServer::forceGameTimeSynchronization`,
//! about once a second), and the v770 adapter used to fall back to `game_time`
//! for those. So `sky_darken` was a **session constant** — permanently whatever
//! hour `age % 24000` happened to name. On a long-lived world that lands in
//! daylight it is permanently `1.0`: full-bright terrain and daytime mobs at
//! midnight, which is the report.
//!
//! # What this measures, and why it needs no GPU
//!
//! The missing link is the *feed*, not the shader, so this gates the feed: drive
//! the server's clock over RCON and require the client's `time_of_day` — and the
//! `sky_darken` derived from it — to follow. That is the one assertion neither
//! pixel gate can make, and it is cheap enough to run every time.
//!
//! Measured **by value at named hours**, never as an average: noon and midnight
//! are the two plateaus of vanilla's curve (`1.0` and `0.24`), so a wiring that
//! is merely *plausible* cannot sit between them.
//!
//! # The control
//!
//! This is not an assertion of an absence, but it has the adjacent hazard: on a
//! *fresh* world the world age and the day clock are equal, so a broken feed and
//! a working one agree and the gate passes vacuously. The control is therefore
//! the divergence itself — the test asserts that `world_age % 24000` is **not**
//! the value being reported, i.e. that the two really have separated on this
//! world. A world young enough for them to coincide fails with a fix hint rather
//! than passing.
//!
//! ```text
//! ./scripts/live-oracles/survival.sh
//! cargo test -p lodestone-shell --features live \
//!     --test live_time_of_day -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::net::{NetClient, NetUpdate};
use lodestone_render::entity::sky_darken_for_time_of_day;
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
/// The survival oracle: normal terrain, RCON on `:25566`. Used rather than the
/// flat creative one only because this world has been running long enough for the
/// world age and the day clock to diverge, which is the condition the defect
/// needs to be visible at all.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
/// Vanilla 26.2, resolved through the registry by the `live` feature.
const PROTOCOL: i32 = 776;

/// Vanilla's day-clock tick for each `/time set` marker, from
/// `Timelines::OVERWORLD_DAY`'s `addTimeMarker` calls in the real 26.2 jar.
const MARKERS: [(&str, i64); 4] = [
    ("noon", 6_000),
    ("midnight", 18_000),
    ("day", 1_000),
    ("night", 13_000),
];

/// `sky_darken` at each marker. `noon`/`day` sit on the bright plateau, `midnight`
/// on the dark one; `night` (13_000) is just inside the ramp, so it is bounded
/// rather than pinned.
const BRIGHT: f32 = 1.0;
const DARK: f32 = 0.24;

fn join(net: &NetClient) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logged_in = false;
    let mut last: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last = Some(e),
                NetUpdate::Disconnected(r) => {
                    last = Some(format!("disconnected: {}", r.to_plain_string()))
                }
                _ => {}
            }
        }
        if logged_in && net.shared_handle().get().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "never logged in to {HOST}:{PORT} within 60s (last event: {last:?}). \
         Fix: ./scripts/live-oracles/survival.sh, and run with `--features live`."
    );
}

/// Drain the client for `secs`, so the server's next `set_time` lands.
fn settle(net: &NetClient, secs: u64) {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        for _ in net.poll() {}
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn world_time(net: &NetClient) -> (i64, i64) {
    net.shared_handle()
        .get()
        .expect("a resolved client handle")
        .world_time()
}

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566) and `--features live`"]
fn the_clients_day_clock_follows_the_servers() {
    let net = NetClient::connect(HOST.into(), PORT, PROTOCOL, None);
    join(&net);

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. \
             Fix: ./scripts/live-oracles/survival.sh (game :25565, RCON :25566)."
        )
    });
    // Freeze the clock **first**, so each reading is the value that was set
    // rather than the value it drifted to while the assertions ran — the client
    // extrapolates the held anchor, so a running clock adds one tick per tick of
    // `settle`. 26.2 renamed the gamerule to snake_case *and* renamed the concept:
    // it is `advance_time`, not `doDaylightCycle`, and it reaches the client as a
    // clock `rate` of `0.0` (`ClockInstance::packNetworkState`), never as a flag.
    let froze = rcon.cmd("gamerule advance_time false");
    assert!(
        !froze.contains("Incorrect") && !froze.contains("Unknown"),
        "`gamerule advance_time false` was rejected ({froze:?}); without a frozen clock every \
         reading below drifts and the exact-marker assertions are meaningless. Has the gamerule \
         been renamed again?"
    );

    let mut readings = Vec::new();
    for (marker, want_ticks) in MARKERS {
        let set = rcon.cmd(&format!("time set {marker}"));
        assert!(
            !set.contains("Incorrect") && !set.contains("Unknown"),
            "`/time set {marker}` was rejected ({set:?}); the oracle is not 26.2, or the time \
             markers were renamed. Cannot drive the clock, so the rest of this gate is vacuous."
        );
        settle(&net, 3);
        let (age, tod) = world_time(&net);
        let darken = sky_darken_for_time_of_day(tod);
        readings.push((marker, want_ticks, age, tod, darken));

        println!(
            "/time set {marker:<9} -> client age={age} time_of_day={tod} \
             (reduced {}) sky_darken={darken:.3}",
            tod.rem_euclid(24_000)
        );

        assert_eq!(
            tod.rem_euclid(24_000),
            want_ticks,
            "after `/time set {marker}` the client's day clock must read {want_ticks} \
             (vanilla's `Timelines::OVERWORLD_DAY` marker), got {} (raw {tod}). A value equal to \
             the world age means `set_time`'s empty clock map is still overwriting the held \
             clock — the permanent-fixed-hour bug.",
            tod.rem_euclid(24_000)
        );
    }

    // The vacuity control: the world age must NOT itself name these hours, or a
    // broken feed would have produced the same numbers.
    for &(marker, want, age, _, _) in &readings {
        assert_ne!(
            age.rem_euclid(24_000),
            want,
            "CONTROL DID NOT FIRE: at `{marker}` the world age reduces to {want} as well, so this \
             reading cannot distinguish a held day clock from the world-age fallback. Fix: run \
             this against a world old enough for `age` and the day clock to have separated (the \
             survival oracle qualifies; a freshly generated world does not)."
        );
    }

    // And the number the renderer actually consumes must span the curve.
    let at = |name: &str| {
        readings
            .iter()
            .find(|r| r.0 == name)
            .map(|r| r.4)
            .expect("marker was measured")
    };
    assert!(
        (at("noon") - BRIGHT).abs() < 1e-3,
        "noon must scale sky light by {BRIGHT}, got {}",
        at("noon")
    );
    assert!(
        (at("day") - BRIGHT).abs() < 1e-3,
        "day must scale sky light by {BRIGHT}, got {}",
        at("day")
    );
    assert!(
        (at("midnight") - DARK).abs() < 1e-3,
        "midnight must scale sky light by {DARK} (vanilla's `NIGHT_SKY_LIGHT_FACTOR`), got {}",
        at("midnight")
    );
    assert!(
        at("midnight") < at("noon") * 0.5,
        "midnight ({}) must be less than half of noon ({}); a session-constant factor makes these \
         equal, which is the reported defect",
        at("midnight"),
        at("noon")
    );

    // Restore, so a re-run and interactive play start from a moving sun.
    rcon.cmd("gamerule advance_time true");
}
