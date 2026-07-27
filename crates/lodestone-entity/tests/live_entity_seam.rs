//! E4 — the acceptance gate entities have never had.
//!
//! # What this proves, and why it exists
//!
//! Every module in this crate — the [`EntityTracker`], metadata application,
//! the attribute pipeline validated in §12.20, [`EntityPose`] interpolation — is
//! well unit-tested. But unit tests here mock the thing they integrate with, and
//! the project has been burned four times by a green suite that never once let
//! real data cross the public seam. `impl-shell` found exactly this for chunks:
//! the decoder was correct and proven against 225 live chunks, yet `handle_play`
//! never called it and `ClientEvent::ChunkLoaded` could not structurally carry a
//! block. Thousands of green tests coexisted with world data that had never
//! reached the public API.
//!
//! This test asks the same question of entities, through the client's **public**
//! API only ([`lodestone_client::ClientHandle::entities`] and the
//! [`ClientEvent`] stream): summon a real pig on the live oracle and check that
//! it appears, with the right type and position, the way a renderer would need.
//!
//! # The finding (why this test FAILS today)
//!
//! It fails, and the failure *is* the deliverable. Tracing the 26.2 path:
//!
//! * `lodestone-model`'s [`ClientEvent`] has `EntitySpawned` / `EntityMoved` /
//!   `EntityVelocity` / `EntityRemoved` — but **no metadata or attribute
//!   variant at all**, so those two can never cross regardless of decoding.
//! * **No version adapter emits any entity event.** The only constructors of
//!   `ClientEvent::Entity*` in the whole workspace live in
//!   `lodestone-client/tests/read_model.rs`'s `FakeAdapter`. `v47` and `v340`
//!   ship real `entity.rs` + `metadata.rs` decoders that nothing invokes
//!   (correct-but-never-called, exactly like the old chunk decoder); `v770`
//!   — the version this oracle speaks — has neither decoder.
//! * `v770`'s `handle_play` is an if-chain over eight packet ids (login,
//!   keep-alive, disconnect, system-chat, set-health, combat-kill, chunk,
//!   forget-chunk). `ADD_ENTITY`, `SET_ENTITY_DATA` (metadata),
//!   `UPDATE_ATTRIBUTES`, `MOVE_ENTITY_*`, `TELEPORT_ENTITY`,
//!   `SET_ENTITY_MOTION` and `REMOVE_ENTITIES` are all defined in the generated
//!   id table but **never referenced**; each arrives, matches nothing, and hits
//!   the trailing `Ok(Vec::new())` — received and dropped, undecoded.
//! * `lodestone-client` does not depend on `lodestone-entity`. Its read-model
//!   `EntityView` has no metadata/attribute field either, so even if the event
//!   were emitted there is nowhere for that data to land.
//!
//! So on a live 26.2 server `entities()` is *always empty*; a spawned mob is
//! invisible to every consumer. This test encodes the behaviour the seam must
//! have. It fails now (the gap) and turns green the day `v770` decodes the
//! entity packets and an adapter emits the events — at which point it is the
//! standing regression gate.
//!
//! The three positive controls (login, chunks, the pig existing server-side)
//! assert *first* and independently, so a failure here is unambiguously the
//! entity seam and not a dead connection or an empty world.
//!
//! Run with the oracle up:
//! `cargo test -p lodestone-entity --test live_entity_seam -- --ignored --nocapture`.

use lodestone_testsupport::{RconClient, unique_username};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_entity::attribute::default_def;
use lodestone_entity::{AttributeInstance, Modifier, Operation};
use uuid::Uuid;

const GAME_ADDR_HOST: &str = "127.0.0.1";
const GAME_ADDR_PORT: u16 = 25567;
const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;
/// Serializes the live tests so only one bot is ever joined at a time.
///
/// `cargo test` runs a binary's tests in parallel by default, but these each do
/// a full join to the *same* single-spawn oracle. Two players landing on the
/// world spawn simultaneously shove each other apart, which desyncs a
/// stationary bot's join-teleport position from its true server position (the
/// server only re-corrects a client that actually moves) — a false failure of
/// the `PLAYER_POSITION` check that has nothing to do with the seam. Unique
/// usernames stop the *eviction*, but not the *shove*; serializing the joins is
/// what removes the contention. This is a shared async lock rather than
/// `--test-threads=1` so the isolation travels with the tests instead of
/// depending on how they're invoked.
static SEAM_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

struct Rcon {
    inner: RconClient,
}

impl Rcon {
    fn connect() -> Self {
        Self {
            inner: RconClient::connect(RCON_ADDR, RCON_PASSWORD)
                .expect("oracle RCON reachable/authenticated at 127.0.0.1:25575 — is lodestone-entity-oracle up?"),
        }
    }

    fn cmd(&mut self, command: &str) -> String {
        self.inner.cmd(command)
    }

    /// Reads a player's `Pos` `[x,y,z]` via `data get`. `None` if the player is
    /// not present or the response has no list.
    fn player_pos(&mut self, name: &str) -> Option<(f64, f64, f64)> {
        let resp = self.cmd(&format!("data get entity {name} Pos"));
        parse_list3(&resp)
    }

    /// Reads the server's *computed* effective value of an attribute via
    /// `/attribute … get`, i.e. base folded with every modifier by vanilla's own
    /// pipeline. This is the oracle the client's fold must reproduce. `None` if
    /// the trailing number cannot be parsed (e.g. the entity is not present yet).
    fn attribute_value(&mut self, selector: &str, attribute: &str) -> Option<f64> {
        let resp = self.cmd(&format!("attribute {selector} {attribute} get"));
        parse_trailing_f64(&resp)
    }
}

/// Parses the final whitespace-delimited token of a string as an `f64`. The
/// `/attribute … get` response ends in `… is 0.35`.
fn parse_trailing_f64(resp: &str) -> Option<f64> {
    resp.split_whitespace().last()?.parse::<f64>().ok()
}

/// Parses the first `[a, b, c]` list of doubles out of a `data get` response.
fn parse_list3(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    if nums.len() == 3 {
        Some((nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

/// Histogram of event variant names seen on the public stream, plus a capture of
/// any `EntitySpawned` payloads — the exact record of what crossed the API.
#[derive(Default)]
struct Seen {
    counts: BTreeMap<&'static str, usize>,
    spawned: Vec<String>,
}

fn variant_name(event: &ClientEvent) -> &'static str {
    match event {
        ClientEvent::Login { .. } => "Login",
        ClientEvent::Chat { .. } => "Chat",
        ClientEvent::Disconnect { .. } => "Disconnect",
        ClientEvent::KeepAlive { .. } => "KeepAlive",
        ClientEvent::TeleportPlayer { .. } => "TeleportPlayer",
        ClientEvent::EntitySpawned { .. } => "EntitySpawned",
        ClientEvent::EntityMoved { .. } => "EntityMoved",
        ClientEvent::EntityVelocity { .. } => "EntityVelocity",
        ClientEvent::EntityRemoved { .. } => "EntityRemoved",
        ClientEvent::EntityMetadataUpdated { .. } => "EntityMetadataUpdated",
        ClientEvent::EntityAttributesUpdated { .. } => "EntityAttributesUpdated",
        _ => "other",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the live lodestone-entity-oracle server on :25567 (+ RCON :25575)"]
async fn spawned_mob_crosses_the_client_public_api() {
    let _seam = SEAM_LOCK.lock().await;
    let server = ServerAddress {
        host: GAME_ADDR_HOST.into(),
        port: GAME_ADDR_PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let username = profile.username.clone();
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL_26_2)
        .expect("v770 family compiled in via the dev-dependency feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to the live oracle on :25567");

    // Drain the bounded event channel on a background task (an undrained channel
    // backpressures the driver and stalls packet handling), recording exactly
    // which variants — and any EntitySpawned payloads — cross the public API.
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen_bg = Arc::clone(&seen);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            let mut s = seen_bg.lock().unwrap();
            *s.counts.entry(variant_name(&event)).or_default() += 1;
            if let ClientEvent::EntitySpawned {
                entity_id,
                entity_type,
                pos,
                ..
            } = &event
            {
                s.spawned
                    .push(format!("id={entity_id} type={entity_type} pos={pos:?}"));
            }
            if matches!(event, ClientEvent::Disconnect { .. }) {
                break;
            }
        }
    });

    // ---- Positive control 1: we actually entered the world. ----
    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("should reach Play (Login) — otherwise this is a connection fault, not the seam");

    // ---- Positive control 2: real world data crosses (rules out a corpse
    // blackout, where every other signal looks healthy but no chunks arrive). ----
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("chunks should load — a blackout here means a stale username corpse, not the seam");

    let player_name = username.clone();

    // ---- Positive control 3: summon a pig at the player's feet and confirm the
    // server really holds it. Same chunk as the player => inside entity-tracking
    // range => the server WILL send an ADD_ENTITY for it.
    //
    // Player position comes from RCON, not `handle.position()`: v770 does not
    // emit `TeleportPlayer` either, so the read-model's position never populates
    // on 26.2 — itself part of the same `handle_play` gap.
    let (server_pig_count, px, py, pz) = tokio::task::spawn_blocking(move || {
        let mut r = Rcon::connect();
        let (px, py, pz) = r
            .player_pos(&player_name)
            .expect("player Pos readable via RCON after join — otherwise the bot never spawned");
        // Force-load the player's column so the pig can never fall out of a
        // ticking chunk mid-observation.
        r.cmd(&format!(
            "forceload add {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
        r.cmd("kill @e[type=pig,tag=e4probe]");
        r.cmd(&format!(
            "summon pig {px:.3} {py:.3} {pz:.3} {{Tags:[\"e4probe\"],NoAI:1b}}"
        ));
        // Register + let the server emit the tracking packet. `tick sprint`
        // advances entity systems ( `tick step` does not, on these servers ).
        r.cmd("tick sprint 5");
        // Poll: a freshly summoned entity is not selector-visible until the next
        // server tick.
        let deadline = Instant::now() + Duration::from_secs(5);
        let count = loop {
            let resp = r.cmd("execute if entity @e[type=pig,tag=e4probe] run data get entity @e[type=pig,tag=e4probe,limit=1] UUID");
            if resp.contains('[') {
                break 1usize;
            }
            if Instant::now() >= deadline {
                break 0usize;
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        (count, px, py, pz)
    })
    .await
    .expect("rcon task");

    assert!(
        server_pig_count >= 1,
        "precondition failed: the oracle never registered the summoned pig — this is a harness/server fault, not the entity seam"
    );

    // ---- The gate: does that pig cross the public client API? ----
    // Give the driver generous real time to receive and apply the ADD_ENTITY.
    //
    // The oracle's overworld also holds ambient pigs, so we must not grab "any
    // pig" — we select the pig *nearest the summon point* (the probe is spawned
    // at the player's feet, distance ~0, while ambient pigs are chunks away) and
    // then assert it really is at that point. Picking the first pig by type would
    // be a flaky false-negative the moment an ambient pig is loaded.
    let mut pig: Option<(i32, String, f64, f64, f64)> = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let nearest = handle
            .entities()
            .into_iter()
            .filter(|e| e.entity_type.to_string().contains("pig"))
            .min_by(|a, b| {
                let da = (a.position.x - px).powi(2) + (a.position.z - pz).powi(2);
                let db = (b.position.x - px).powi(2) + (b.position.z - pz).powi(2);
                da.total_cmp(&db)
            });
        if let Some(view) = nearest {
            pig = Some((
                view.entity_id,
                view.entity_type.to_string(),
                view.position.x,
                view.position.y,
                view.position.z,
            ));
            // Once we have a pig within summon tolerance, stop early; otherwise
            // keep polling in case the probe's ADD_ENTITY has not yet arrived.
            if (view.position.x - px).abs() < 1.5 && (view.position.z - pz).abs() < 1.5 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The pig must be seen through the *typed* public API at the position it was
    // summoned at — not merely "some entity spawned". This asserts the decode is
    // right end to end: the type id resolved to `minecraft:pig` and the three
    // `f64` coordinates landed where the server put them (an x/z transposition or
    // a byte-misaligned decode would miss the tolerance and fail here).
    let pig_at_summon_pos = pig.as_ref().is_some_and(|&(_, _, x, y, z)| {
        (x - px).abs() < 1.5 && (y - py).abs() < 2.0 && (z - pz).abs() < 1.5
    });

    // Priority 1 of the seam: `PLAYER_POSITION` now decodes and the client
    // confirms the teleport, so the local player's position finally crosses the
    // public API (it was `None` on 26.2 before this — the same `handle_play`
    // gap). The server's join-time synchronize-position places the player at the
    // RCON-reported coordinates, so this is another known-value check.
    let player_position = handle.position();
    let player_position_ok = player_position.is_some_and(|p| {
        (p.x - px).abs() < 2.0 && (p.y - py).abs() < 2.0 && (p.z - pz).abs() < 2.0
    });

    let (pig_seen, entities_len, entities_dbg, spawned_len, histogram_dbg) = {
        let snapshot = seen.lock().unwrap();
        let entities_now = handle.entities();
        (
            pig.is_some(),
            entities_now.len(),
            format!("{entities_now:?}"),
            snapshot.spawned.len(),
            format!("{:?}", snapshot.counts),
        )
    };

    // Surface exactly what crossed the public event stream, so the run is
    // self-documenting rather than a bare green tick.
    eprintln!(
        "=== ENTITY SEAM (live 26.2) ===\n\
         handle.position(): {player_position:?}\n\
         observed pig:      {pig:?}\n\
         entities():        {entities_len} entry(ies)\n\
         event variants:    {histogram_dbg}\n\
         ==============================="
    );

    // Cleanup runs BEFORE the assert so a failing gate never leaves a probe pig
    // or a force-loaded chunk behind on the shared oracle.
    let _ = tokio::task::spawn_blocking(move || {
        let mut r = Rcon::connect();
        r.cmd("kill @e[type=pig,tag=e4probe]");
        r.cmd(&format!(
            "forceload remove {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
    })
    .await;
    drop(handle);
    let _ = drain.await;

    assert!(
        pig_seen,
        "\n\n=== E4 SEAM GAP: a live-summoned mob never crossed the public client API ===\n\
         server-side pigs (RCON selector): {server_pig_count} (confirmed present)\n\
         handle.entities() after 8s:      {entities_len} entries {entities_dbg}\n\
         EntitySpawned events observed:    {spawned_len}\n\
         event variants seen on stream:    {histogram_dbg}\n\
         --> v770 handle_play decodes login/keepalive/chat/health/combat/chunk only;\n\
             ADD_ENTITY/SET_ENTITY_DATA/UPDATE_ATTRIBUTES/MOVE_ENTITY_*/TELEPORT_ENTITY/\n\
             SET_ENTITY_MOTION/REMOVE_ENTITIES fall through to Ok(Vec::new()) undecoded.\n\
         This assertion is the acceptance gate; it goes green once the seam is wired.\n",
    );

    // Wiring the seam is not enough: the pig must arrive with the right type and
    // position. This is the known-value-at-known-position check that a green
    // "an entity exists" gate could otherwise pass vacuously.
    assert!(
        pig_at_summon_pos,
        "\n\n=== pig crossed the API but with the wrong type or position ===\n\
         summoned at:  ({px:.3}, {py:.3}, {pz:.3})\n\
         observed pig: {pig:?}\n\
         a mismatch here means the ADD_ENTITY decode is subtly wrong (type-id table,\n\
         coordinate order, or a byte-misaligned low-precision velocity field).\n",
    );

    // Priority 1: the local player's position now crosses the public API.
    assert!(
        player_position_ok,
        "\n\n=== PLAYER_POSITION did not populate handle.position() ===\n\
         summoned/player at: ({px:.3}, {py:.3}, {pz:.3})\n\
         handle.position():  {player_position:?}\n\
         `player_position` is either undecoded or the teleport-accept confirmation\n\
         is missing (the server would then keep re-correcting us).\n",
    );
}

// ---------------------------------------------------------------------------
// E7 — the metadata + attribute seam.
//
// E4 proved a spawned mob crosses the API with the right *type and position*.
// It did NOT prove that `set_entity_data` (packed metadata) or
// `update_attributes` cross — `EntityView` had no field to hold them and no
// `ClientEvent` variant existed. This test summons a pig with *known* metadata
// (a custom name, name-visible, baby, a specific health) and a *known* extra
// attribute modifier, then asserts those exact values arrive through the public
// `handle.entities()` view.
//
// Two anti-vacuity guards, in the house style:
//   * The probe is deterministic (a uniquely-named baby pig), so we assert on
//     concrete values, not "some metadata arrived".
//   * The attribute value is cross-checked against the server's OWN computed
//     effective value (`/attribute … get`): the client re-folds base+modifiers
//     with the §12.20 pipeline and must land on the same number vanilla did.
//     A wrong fold order (the classic attribute mistake) misses it.
// ---------------------------------------------------------------------------

/// The custom name we stamp on the probe pig; unique enough to pick it out of
/// any ambient pigs by value alone.
const PROBE_NAME: &str = "LodestarPig";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the live lodestone-entity-oracle server on :25567 (+ RCON :25575)"]
async fn entity_metadata_and_attributes_cross_the_client_public_api() {
    let _seam = SEAM_LOCK.lock().await;
    let server = ServerAddress {
        host: GAME_ADDR_HOST.into(),
        port: GAME_ADDR_PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let username = profile.username.clone();
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL_26_2)
        .expect("v770 family compiled in via the dev-dependency feature");

    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to the live oracle on :25567");

    // Drain the event channel so the driver never backpressures, recording the
    // variant histogram — including the two E7 variants, so the run is
    // self-documenting about whether they ever fired.
    let seen = Arc::new(Mutex::new(Seen::default()));
    let seen_bg = Arc::clone(&seen);
    let drain = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            {
                let mut s = seen_bg.lock().unwrap();
                *s.counts.entry(variant_name(&event)).or_default() += 1;
            }
            if matches!(event, ClientEvent::Disconnect { .. }) {
                break;
            }
        }
    });

    // ---- Positive controls: in the world, real chunks arriving. ----
    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("should reach Play (Login) — otherwise this is a connection fault, not the seam");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("chunks should load — a blackout here means a stale username corpse, not the seam");

    // ---- Summon the known-metadata probe and add a known attribute modifier. ----
    let player_name = username.clone();
    let (server_speed, px, _py, pz) = tokio::task::spawn_blocking(move || {
        let mut r = Rcon::connect();
        let (px, py, pz) = r
            .player_pos(&player_name)
            .expect("player Pos readable via RCON after join — otherwise the bot never spawned");
        r.cmd(&format!(
            "forceload add {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
        r.cmd("kill @e[type=pig,tag=e7probe]");
        // Non-default on every field we assert, so the server includes each in
        // the spawn `set_entity_data`: a text-component custom name, name
        // visible, a baby (Age < 0 => `isBaby()` => DATA_BABY_ID true), and a
        // health distinct from the pig's max (10) so DATA_HEALTH_ID is sent.
        r.cmd(&format!(
            "summon pig {px:.3} {py:.3} {pz:.3} \
             {{Tags:[\"e7probe\"],NoAI:1b,Age:-1200,\
             CustomName:{{\"text\":\"{PROBE_NAME}\"}},CustomNameVisible:1b,Health:6f}}"
        ));
        r.cmd("tick sprint 5");
        // Poll until the entity is selector-visible (summon is not visible until
        // the next server tick).
        let selector = "@e[type=pig,tag=e7probe,limit=1]";
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let resp = r.cmd(&format!("execute if entity {selector} run data get entity {selector} UUID"));
            if resp.contains('[') {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // Add a movement_speed modifier. This both marks the attribute dirty (so
        // the server emits `update_attributes`) and gives us a non-trivial fold
        // to reproduce: base 0.25 + ADD_VALUE 0.1 = 0.35.
        r.cmd(&format!(
            "attribute {selector} minecraft:movement_speed modifier add lodestone:probe 0.1 add_value"
        ));
        r.cmd("tick sprint 2");
        // The server's own computed effective value — the oracle for our fold.
        let server_speed = r
            .attribute_value(selector, "minecraft:movement_speed")
            .expect("server should report a movement_speed value for the probe pig");
        (server_speed, px, py, pz)
    })
    .await
    .expect("rcon task");

    // ---- The gate: find the probe pig by its unique custom name through the
    // public API, and read the metadata + attributes off its view. ----
    let mut probe: Option<lodestone_client::EntityView> = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        probe = handle
            .entities()
            .into_iter()
            .find(|e| e.custom_name == Some(Some(PROBE_NAME.to_string())));
        // Wait until the attribute packet has also landed, so a single poll sees
        // a fully-populated view rather than metadata-then-attributes racing.
        if probe
            .as_ref()
            .is_some_and(|v| !v.attributes.is_empty() && v.baby.is_some() && v.health.is_some())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Re-fold the pig's movement_speed with the §12.20 pipeline and compare to
    // the server's own number. Done before the asserts so it appears in output.
    let folded_speed = probe.as_ref().and_then(|view| {
        let snap = view
            .attributes
            .iter()
            .find(|a| a.attribute.to_string() == "minecraft:movement_speed")?;
        let def = default_def(&snap.attribute)?;
        let mut inst = AttributeInstance::new(def);
        inst.set_base_value(snap.base);
        for m in &snap.modifiers {
            inst.add_or_update(Modifier::new(
                m.id.clone(),
                m.amount,
                Operation::from_id(m.operation)?,
            ));
        }
        Some(inst.value())
    });

    let histogram_dbg = format!("{:?}", seen.lock().unwrap().counts);
    eprintln!(
        "=== ENTITY METADATA/ATTRIBUTE SEAM (live 26.2) ===\n\
         probe view:      {probe:?}\n\
         server speed:    {server_speed}\n\
         client fold:     {folded_speed:?}\n\
         event variants:  {histogram_dbg}\n\
         =================================================="
    );

    // Cleanup before asserting so a failure never leaves state on the oracle.
    let _ = tokio::task::spawn_blocking(move || {
        let mut r = Rcon::connect();
        r.cmd("kill @e[type=pig,tag=e7probe]");
        r.cmd(&format!(
            "forceload remove {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
    })
    .await;
    drop(handle);
    let _ = drain.await;

    let view = probe.expect(
        "\n\n=== E7 SEAM GAP: no entity carried the probe's custom name ===\n\
         Either `set_entity_data` never crossed the public API (metadata seam missing),\n\
         or the metadata decode dropped the custom-name field. Expected a pig whose\n\
         `custom_name == Some(Some(\"LodestarPig\"))` in handle.entities().\n",
    );

    // Known-value metadata checks. Each of these is a distinct serializer on the
    // wire (optional-component, boolean, float, boolean at different indices), so
    // together they prove the packed-metadata decode stays byte-aligned across
    // serializer kinds — a single mis-sized value would desync the rest and miss
    // at least one of these.
    assert_eq!(
        view.custom_name,
        Some(Some(PROBE_NAME.to_string())),
        "custom name mismatch (optional-component / network-NBT decode)"
    );
    assert_eq!(
        view.custom_name_visible,
        Some(true),
        "custom-name-visible flag did not arrive (boolean serializer)"
    );
    assert_eq!(
        view.baby,
        Some(true),
        "baby flag did not arrive (boolean serializer at the ageable index)"
    );
    let health = view
        .health
        .expect("health did not arrive (float serializer)");
    assert!(
        (health - 6.0).abs() < 0.01,
        "health mismatch: expected 6.0, got {health}"
    );

    // The attribute cross-check: the client's re-fold must equal the server's own
    // computed value. This is where the §12.20 modifier-order work meets real
    // data. A wrong operation order silently produces a slightly different number
    // and fails here against a real vanilla fold.
    let folded = folded_speed.expect(
        "\n\n=== attribute seam gap: no movement_speed snapshot on the probe ===\n\
         `update_attributes` either never crossed the API or omitted movement_speed.\n",
    );
    assert!(
        (folded - server_speed).abs() < 1e-6,
        "\n\n=== attribute fold disagrees with the server ===\n\
         server /attribute get: {server_speed}\n\
         client re-fold:        {folded}\n\
         base+modifiers folded with the entity pipeline must match vanilla's own\n\
         computed value; a mismatch means a wrong operation order or a mis-decoded\n\
         base/amount.\n",
    );
    assert!(
        (server_speed - 0.35).abs() < 1e-6,
        "sanity: the known modifier (base 0.25 + ADD_VALUE 0.1) should give 0.35, \
         server said {server_speed}"
    );
}
