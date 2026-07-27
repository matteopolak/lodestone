//! Live sound gate: a real, **server-decided** sound from the running
//! `lodestone-mc262` server (`:25565`) must cross `ClientHandle`'s public event
//! API and reach the mixer as **non-silent audio**.
//!
//! This is the test the whole crate existed to earn. Every other test proves a
//! *part* — decode against a real ogg, weighted selection against a JVM golden,
//! the resolve→play glue against a synthetic pack. None of them prove the seam:
//! that a sound the *server* decided to play actually arrives and plays. A
//! correct decoder nothing ever calls is the project's recurring failure shape
//! (§12.24, §12.51); this gate is what stops audio being another instance.
//!
//! # Why ambient/world sounds, not `/playsound`
//!
//! The plan wanted RCON `/playsound` so every field (source, volume, pitch,
//! position) could be pinned. That is **currently blocked by a client-side
//! gap**, verified here: this bot joins, reaches Play, and streams chunks and
//! sounds, but the vanilla server never spawns a *targetable* player entity for
//! it — neither `@a` nor `@e[type=player]` resolves it, and `list` shows zero
//! players. The client's driver auto-sends only `KeepAliveResponse` and
//! `Respawn` (play serverbound is ~9/69); it never confirms the initial
//! teleport / signals "player loaded", so the server keeps the connection in a
//! pre-spawn state that receives world data but is not in the world for command
//! targeting. That is a `lodestone-client`/v770 seam gap, out of this crate's
//! scope — so the gate triggers on **server-pushed sounds** instead, which the
//! reference raw-socket test
//! (`lodestone-v770::sound_from_real_server_decodes_with_zero_trailing_bytes`)
//! proves arrive within a fraction of a second of a join.
//!
//! # Anti-vacuity
//!
//! "A sound played without erroring" is satisfied by a path that resolves
//! nothing and mixes zeros — exactly the silence trap a genuinely-silent vanilla
//! ogg set earlier (§12.47). So the gate asserts on things that can only be true
//! if **real samples flowed end to end**:
//!   * a `ClientEvent::Sound` crossed the *public* `ClientHandle` event stream
//!     (not a raw socket) — proving the seam, not just the decoder;
//!   * the driver decoded a real ogg (`decoded_file_count() >= 1`) — the
//!     mechanism fired, not a coincidental non-zero buffer;
//!   * the co-located mixed peak exceeds `0.3` — the same teeth-bearing floor the
//!     decode tests use; a silent or degenerate path fails here. A resolved but
//!     *silent* sound does not satisfy the gate: we keep waiting for a real one.
//!
//! A fresh driver per candidate keeps the peak attributable to that one sound —
//! no residual energy from an earlier voice can fake a pass.
//!
//! # Preconditions fail loudly, never skip (Rule 1)
//!
//! An `#[ignore]`d test asked to run is an explicit opt-in: a missing server,
//! a missing asset, or an offline CDN is a **failure with a fix**, not a silent
//! pass. A joined bot that hears no sound within 120s is likewise a failure,
//! never an `ok`.
//!
//! Run:
//! ```text
//! cargo test -p lodestone-sound --features live-v770 --test live_sound_gate \
//!   -- --ignored --nocapture
//! ```
#![cfg(feature = "live-v770")]

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use glam::Vec3;
use lodestone_assets::{ResourceSource, sound::SoundRegistry};
use lodestone_client::{ClientBuilder, ClientEvent, LoginProfile, ServerAddress};
use lodestone_sound::SoundDriver;
use lodestone_testsupport::unique_username;
use uuid::Uuid;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const PROTOCOL_776: i32 = 776;

const CACHE_ROOT: &str = ".cache/mc/26.2";
const ASSET_INDEX: &str = ".cache/mc/26.2/asset-index-32.json";

/// Cargo runs integration tests with the crate directory as the working
/// directory, but the shared asset cache lives at the workspace root. Anchor
/// every cache path to the workspace root (two levels up from this crate) so
/// the gate does not depend on where it is launched from.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve workspace root from CARGO_MANIFEST_DIR")
}

/// A `ResourceSource` backed by the vanilla asset-object store: it maps in-pack
/// paths (`assets/minecraft/sounds/<p>.ogg`, `minecraft/sounds.json`) through
/// `asset-index-32.json` to `objects/<sha1[0..2]>/<sha1>`. If an object is
/// absent it fetches it from Mojang's CDN — exactly what the launcher does —
/// and verifies the sha1, so the gate can decode a *real* vanilla ogg. This
/// download logic lives here in the test, never in the crate: the driver only
/// ever sees `read(path) -> Option<Vec<u8>>`.
struct ObjectStoreSource {
    root: PathBuf,
    /// in-pack path (index key) -> sha1 hex.
    index: HashMap<String, String>,
}

impl fmt::Debug for ObjectStoreSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStoreSource")
            .field("objects", &self.index.len())
            .finish()
    }
}

impl ObjectStoreSource {
    fn load() -> Self {
        let root = workspace_root();
        let index_file = root.join(ASSET_INDEX);
        let index_bytes = std::fs::read(&index_file).unwrap_or_else(|e| {
            panic!(
                "missing {} ({e}) — run the asset fetch for 26.2 \
                 (the launcher/xtask that populates .cache/mc/26.2)",
                index_file.display()
            )
        });
        let json: serde_json::Value =
            serde_json::from_slice(&index_bytes).expect("asset-index-32.json parses");
        let mut index = HashMap::new();
        for (k, v) in json["objects"].as_object().expect("objects map") {
            if let Some(hash) = v["hash"].as_str() {
                index.insert(k.clone(), hash.to_string());
            }
        }
        Self {
            root: root.join(CACHE_ROOT),
            index,
        }
    }

    /// Reads `minecraft/sounds.json` bytes through the object store.
    fn sounds_json(&self) -> Vec<u8> {
        self.object_bytes("minecraft/sounds.json")
            .expect("sounds.json is present in the asset index and object store")
    }

    fn object_path(&self, sha1: &str) -> PathBuf {
        self.root.join("objects").join(&sha1[0..2]).join(sha1)
    }

    /// Bytes for an index key, downloading + verifying on a miss.
    fn object_bytes(&self, index_key: &str) -> Option<Vec<u8>> {
        let sha1 = self.index.get(index_key)?;
        let path = self.object_path(sha1);
        if let Ok(bytes) = std::fs::read(&path) {
            return Some(bytes);
        }
        // Launcher-style fetch into the object store, then sha1-verify.
        let url = format!(
            "https://resources.download.minecraft.net/{}/{}",
            &sha1[0..2],
            sha1
        );
        std::fs::create_dir_all(path.parent().unwrap()).expect("create object dir");
        let status = Command::new("curl")
            .args(["-sS", "-f", "-m", "30", "-o"])
            .arg(&path)
            .arg(&url)
            .status()
            .expect("spawn curl");
        assert!(
            status.success(),
            "failed to fetch asset object {index_key} ({sha1}) from {url} — \
             the CDN is unreachable and the object is not cached. Connect to a \
             network or pre-populate {}",
            path.display()
        );
        let got = sha1_hex(&path);
        assert_eq!(
            &got, sha1,
            "downloaded object {index_key} sha1 mismatch (corrupt CDN fetch)"
        );
        Some(std::fs::read(&path).expect("read freshly downloaded object"))
    }
}

impl ResourceSource for ObjectStoreSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        // The driver reads `assets/<ns>/sounds/<p>.ogg`; the index key drops the
        // leading `assets/`.
        let key = path.strip_prefix("assets/").unwrap_or(path);
        self.object_bytes(key)
    }

    fn list(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
}

fn sha1_hex(path: &Path) -> String {
    let out = Command::new("shasum")
        .arg("-a")
        .arg("1")
        .arg(path)
        .output()
        .expect("run shasum");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

// A multi-thread runtime is required: the client's driver task (which answers
// keep-alives) shares this runtime, and the RCON client is a *synchronous*
// blocking socket. On a current-thread runtime a blocking RCON call would
// starve the driver, keep-alives would go unanswered, and the server would
// evict the bot. Two worker threads keep the driver progressing regardless.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the live lodestone-mc262 server on 127.0.0.1:25565 and asset CDN access"]
async fn live_sound_packet_crosses_the_public_api_and_reaches_the_mixer() {
    // A fresh driver per candidate: parses the server's own sounds.json and
    // backs oggs by the on-demand object store. Rebuilding per candidate keeps
    // the mixed peak attributable to *that* sound alone — no residual energy
    // from a previously-played voice can produce a false "non-silent" reading.
    let build_driver = || {
        let source = ObjectStoreSource::load();
        let registry =
            SoundRegistry::parse(&source.sounds_json()).expect("real sounds.json parses");
        SoundDriver::new(48_000, registry, Box::new(source))
    };

    // --- Join the real server through the public client API. ---
    let server = ServerAddress {
        host: HOST.into(),
        port: PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL_776)
        .expect("v770 family compiled via the live-v770 feature");
    let (handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .expect("connect to live lodestone-mc262 on 127.0.0.1:25565");

    // The event channel is bounded: if it fills, the client's driver task blocks
    // on send and stops answering keep-alives, and the server drops the bot. So
    // drain the stream continuously in the background, forwarding every `Sound`
    // event onto an unbounded channel. This keeps the driver — and the
    // keep-alive it drives — unblocked for the whole test.
    let (sound_tx, mut sound_rx) = tokio::sync::mpsc::unbounded_channel();
    let drain = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Some(ClientEvent::Sound {
                    sound,
                    category,
                    pos,
                    volume,
                    pitch,
                    seed,
                    ..
                }) => {
                    let _ = sound_tx.send((
                        sound.path().to_string(),
                        category,
                        pos,
                        volume,
                        pitch,
                        seed,
                    ));
                }
                Some(ClientEvent::Disconnect { reason }) => {
                    eprintln!("[live-gate] DISCONNECTED: {}", reason.to_plain_string());
                }
                Some(_) => {}
                None => {
                    eprintln!("[live-gate] event stream ENDED (driver stopped)");
                    return;
                }
            }
        }
    });

    // Drive a *full* join: reach Play and load at least one chunk, so the server
    // is actively simulating the world around us and pushing ambient/world
    // sounds to this connection.
    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("bot never reached Play state on the live server");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("bot never received terrain (health 0.0 => inherited corpse?)");

    // --- The gate: a *server-decided* sound, arriving through the public
    // `ClientHandle` event stream, must drive the full resolve → decode → mix
    // path to non-silent audio. We take the first sound that both resolves in
    // the real registry and renders above the silence floor; a connected-but-
    // silent path (the §12.47 trap in live form) cannot satisfy it, and a bot
    // that receives no sound at all times out (a failure, never a skip). ---
    let mut attempts = 0usize;
    let mut resolved_names: Vec<String> = Vec::new();
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some((name, category, spos, volume, pitch, seed)) = sound_rx.recv().await {
            attempts += 1;
            // Fresh driver so the render reflects only this sound.
            let mut driver = build_driver();
            // Co-locate the voice and listener at the origin so distance
            // attenuation cannot mask a genuinely non-silent decode.
            let played = match driver.play_sound(&name, category, Vec3::ZERO, volume, pitch, seed) {
                Ok(Some(h)) => h,
                // Event not in the vanilla registry, or resolved to nothing —
                // not our failure; wait for the next server sound.
                Ok(None) => continue,
                Err(e) => {
                    eprintln!("[live-gate] {name} failed to decode: {e}");
                    continue;
                }
            };
            resolved_names.push(name.clone());
            driver.mixer_mut().set_listener(lodestone_audio::Listener {
                position: Vec3::ZERO,
                ..lodestone_audio::Listener::default()
            });
            driver.mixer_mut().set_voice_position(played, Vec3::ZERO);
            let mut out = vec![0.0f32; 8192];
            driver.mixer_mut().render(&mut out);
            let peak = out.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
            let decoded = driver.decoded_file_count();
            eprintln!("[live-gate] {name}: decoded={decoded} peak={peak:.3}");
            if peak > 0.3 {
                return Some((name, category, spos, decoded, peak));
            }
            // Resolved but silent — keep the anti-vacuity teeth: only a real,
            // audible sample stream may satisfy the gate.
        }
        None
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "no server sound reached the mixer as non-silent audio within 120s \
             ({attempts} sound events seen, resolved: {resolved_names:?}). The bot \
             was joined and receiving world data; if it saw zero sounds the server \
             may be idle — stand near mobs/water, or verify ambient events fire."
        )
    });

    drain.abort();
    let mut handle = handle;
    handle.shutdown();

    let (name, category, spos, decoded, peak) =
        outcome.expect("a server sound resolved and decoded, but none cleared the silence floor");

    // Anti-vacuity, made explicit: this can only be true if a real server sound
    // crossed the public API and produced real samples end to end.
    assert!(
        decoded >= 1,
        "the decode mechanism must have fired (decoded_file_count={decoded})"
    );
    assert!(
        peak > 0.3,
        "live sound reached the mixer but mixed silent (peak {peak}) — \
         connected-but-silent is the exact trap this gate exists to catch"
    );
    // `spos`/`category` come straight off the wire through `ClientHandle`,
    // proving the seam carried a positioned, categorised sound — not a value we
    // manufactured.
    eprintln!(
        "[live-gate] PASSED: server sound '{name}' on {category:?} at {spos:?} \
         decoded {decoded} file(s), mixed peak {peak:.3}"
    );
}
