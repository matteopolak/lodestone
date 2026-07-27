//! Live-server attribute oracle.
//!
//! This is the external grounding the project insists on: instead of trusting
//! our own table or a static data file, it asks a **running vanilla 26.2 server**
//! what a mob's attributes actually are and checks our numbers against it. The
//! `/attribute … base get` command reports the live base value, so attributes a
//! mob does not override reveal the game's `RangedAttribute` default directly.
//!
//! It is `#[ignore]`d because it needs an isolated Docker server with RCON. Bring
//! one up (never touch the shared `lodestone-mc262` / `lodestone-mc189`):
//!
//! ```text
//! mkdir -p .cache/mc/oracle && cp .cache/mc/26.2/server.jar .cache/mc/26.2/eula.txt .cache/mc/oracle/
//! cat > .cache/mc/oracle/server.properties <<'EOF'
//! online-mode=false
//! level-type=minecraft:flat
//! enable-rcon=true
//! rcon.port=25575
//! rcon.password=lodestone
//! server-port=25567
//! EOF
//! docker run --rm -d --name lodestone-entity-oracle \
//!   -v "$PWD/.cache/mc/oracle:/w" -w /w -p 25567:25567 -p 25575:25575 \
//!   eclipse-temurin:25-jdk java -Xmx2G -jar server.jar nogui
//! ```
//!
//! Then: `cargo test -p lodestone-entity --test live_oracle -- --ignored`.
//! Stop it with `docker stop lodestone-entity-oracle` (it is `--rm`).
//!
//! # Why there is no live *pathfinding* oracle here
//!
//! The obvious next step — summon a zombie, watch it path around a wall toward a
//! villager, and compare its route to a `PathFinder` — does not work over RCON
//! alone. Minecraft only ticks entity AI/navigation in chunks within simulation
//! distance of a **connected player**. With no player online, a summoned mob
//! never moves: a pig dropped from y=100 on this oracle stays at y=100
//! indefinitely (verified). `/attribute … get` works precisely because it reads
//! state without requiring ticks. A faithful live path oracle therefore needs a
//! bot client connected so mobs tick; `lodestone-client` is being restructured
//! by another agent, so that test is designed but not wired here. The hermetic
//! `tests/pathfinding.rs` cases carry pathfinding correctness in the meantime.
//!
//! ## Traps for whoever wires the bot-client oracle later
//!
//! When this is finally connected via a real login, two offline-mode landmines
//! (learned the hard way by a sibling session) will otherwise cost hours:
//!
//! 1. **Offline UUID is derived from the username, not the client-sent UUID.**
//!    The server computes `OfflinePlayer:<name>` and discards whatever UUID the
//!    client sends, so `Uuid::new_v4()` does NOT give per-run isolation — every
//!    login with the same username shares one persisted player .dat on disk.
//!    Generate a unique *username* per run instead (see the `unique_username()`
//!    approach in `crates/lodestone-client/tests/live_chunk.rs`: a short prefix
//!    plus a pid⊕nanos suffix, kept under vanilla's 16-char limit).
//! 2. **A dead inherited player receives zero chunks, silently.** If a mob killed
//!    a previous run's player, vanilla persists `Health = 0.0` + a `DeathLocation`
//!    and every later join with that username is pinned to the death screen until
//!    the client sends `client_command(perform_respawn)`. Login succeeds,
//!    keep-alives flow, entity packets arrive, `set_chunk_cache_center` arrives —
//!    but the chunk stream stays empty and the test just times out. If a live
//!    test suddenly gets no chunks, dump `set_health` FIRST: `0.0` means you
//!    inherited a corpse, not that your code regressed. (`impl-world` is adding
//!    proper death/respawn handling — coordinate, don't reimplement it.)
//!
//! Both are avoided for this file's *attribute* tests because RCON runs as the
//! server console (op level 4), never joins as a player, and never dies.

use lodestone_entity::attribute::default_def;
use lodestone_model::Identifier;
use lodestone_testsupport::RconClient;
use std::str::FromStr;
use std::time::Duration;

const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";

struct Rcon {
    inner: RconClient,
}

impl Rcon {
    fn connect() -> Self {
        Self {
            inner: RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
                "oracle RCON reachable at 127.0.0.1:25575 — is lodestone-entity-oracle up?",
            ),
        }
    }

    fn cmd(&mut self, command: &str) -> String {
        self.inner.cmd(command)
    }

    /// Blocks until `selector` matches a live entity, or panics after a timeout.
    ///
    /// A freshly `/summon`ed entity is only added to the world on the *next*
    /// server tick, so an immediate selector query races entity registration and
    /// returns "No entity was found". (An earlier version of this test queried
    /// straight after summon and passed only because network round-trip latency
    /// happened to span a tick; on a fast-booting fresh server it flaked.) Poll
    /// instead of sleeping a fixed amount so we neither flake nor over-wait.
    fn wait_for_entity(&mut self, selector: &str) {
        self.inner
            .wait_for_entity(
                selector,
                Duration::from_secs(10),
                Duration::from_millis(100),
            )
            .unwrap_or_else(|e| panic!("entity {selector} never registered within 10s: {e}"));
    }

    /// Reads a mob's live base value for an attribute via `/attribute … base get`.
    fn base_value(&mut self, selector: &str, attribute: &str) -> f64 {
        let resp = self.cmd(&format!("attribute {selector} {attribute} base get"));
        // "The base value of attribute X for entity Y is 0.6"
        resp.rsplit(" is ")
            .next()
            .and_then(|s| s.trim().trim_end_matches('.').parse::<f64>().ok())
            .unwrap_or_else(|| panic!("could not parse attribute value from: {resp:?}"))
    }
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
}

#[test]
#[ignore = "needs the isolated lodestone-entity-oracle Docker server with RCON"]
fn live_attribute_defaults_match_our_table() {
    let mut rcon = Rcon::connect();
    rcon.cmd("kill @e[type=pig]");
    rcon.cmd("summon minecraft:pig 0 -60 0 {NoAI:1b,NoGravity:1b}");
    let sel = "@e[type=pig,limit=1]";
    rcon.wait_for_entity(sel);

    // Attributes a pig does NOT override expose the game's RangedAttribute
    // default. These validate our `default_def` table against the running game.
    for path in ["step_height", "knockback_resistance", "gravity"] {
        let key = Identifier::from_str(&format!("minecraft:{path}")).unwrap();
        let ours = default_def(&key).unwrap().default;
        let live = rcon.base_value(sel, &format!("minecraft:{path}"));
        approx(live, ours);
    }

    // Attributes a pig DOES override: these are mob-specific base values, not
    // the attribute default. We assert the real vanilla numbers so a future
    // per-mob base table can be checked against them.
    approx(rcon.base_value(sel, "minecraft:movement_speed"), 0.25);
    approx(rcon.base_value(sel, "minecraft:max_health"), 10.0);
    approx(rcon.base_value(sel, "minecraft:follow_range"), 16.0);

    rcon.cmd("kill @e[type=pig]");
}

#[test]
#[ignore = "needs the isolated lodestone-entity-oracle Docker server with RCON"]
fn live_zombie_step_height_and_speed() {
    let mut rcon = Rcon::connect();
    rcon.cmd("difficulty easy"); // monsters cannot spawn on peaceful
    rcon.cmd("kill @e[type=zombie]");
    rcon.cmd("summon minecraft:zombie 0 -60 0 {NoAI:1b,NoGravity:1b}");
    let sel = "@e[type=zombie,limit=1]";
    rcon.wait_for_entity(sel);

    // Step height is the STEP_HEIGHT attribute default (0.6), shared by our
    // MobShape::land default `max_up_step`; a zombie does not override it.
    approx(rcon.base_value(sel, "minecraft:step_height"), 0.6);
    // Zombie movement speed base is 0.23 in vanilla.
    approx(rcon.base_value(sel, "minecraft:movement_speed"), 0.23);

    rcon.cmd("kill @e[type=zombie]");
}
