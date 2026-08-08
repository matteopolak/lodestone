//! Browser **multiplayer join** — the real `lodestone-client` driving a real
//! 26.2 (`v770`) session over the browser WebSocket transport and the
//! protocol-blind relay.
//!
//! ```text
//! browser  --WebSocket-->  lodestone-relay  --TCP-->  vanilla 26.2 server
//!    |
//!    +-- WsWebTransport -> ClientBuilder::connect_with -> Driver (spawn_local)
//!                                     |
//!                                     +-- client-owned chunk store
//!                                            -> sections_at() -> greedy mesh -> wgpu
//! ```
//!
//! ## Why this module exists
//!
//! `wasm32` cannot open a raw TCP socket, so a browser reaches a server only
//! through the relay. Both legs of that were already proven — `WsWebTransport`
//! carried a real Server-List-Ping to a live server — but nothing in `web/`
//! ever drove a *client* over it, so a browser play join was an island with no
//! producer. This is the producer.
//!
//! ## The two things that are not obvious
//!
//! * **The username must be unique per run, and the UUID cannot be.** An
//!   offline-mode server derives the account UUID from the *username* and
//!   discards the one we send, so every session sharing a name shares one
//!   persisted player file — and a dead player is held on the death screen,
//!   which sends **no chunks** while login, keep-alives and entity movement all
//!   continue looking perfect. [`browser_username`] is the browser's stand-in
//!   for `lodestone-testsupport`'s `unique_username` (that crate is native).
//! * **Meshing reads the client's store, not the event stream.**
//!   `ClientEvent::ChunkLoaded` is a bare `{ pos }` dirty signal by design, so
//!   the live scene is rebuilt by *querying* [`ClientHandle::sections_at`]. That
//!   is idempotent, so it also survives a tab that attaches mid-stream — which a
//!   lossy event fold would not.
//!
//! Read timeouts are ignored on wasm (no runtime timer), so a stalled join
//! surfaces as this module's own deadline rather than a `ClientError::Timeout`.

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_client::{
    ChunkPos, ChunkSection, ClientBuilder, ClientHandle, EventStream, LoginProfile, ServerAddress,
};
use lodestone_net::WsWebTransport;
use uuid::Uuid;

/// Everything a browser join needs, all of it user-supplied.
#[derive(Clone, Debug)]
pub struct JoinTarget {
    /// The relay's WebSocket URL, e.g. `ws://127.0.0.1:25580`.
    pub relay_url: String,
    /// Host to advertise in the handshake. The relay decides where the bytes
    /// actually go (its `--target`), so this only has to be what the server
    /// expects to see — vanilla does not validate it in offline mode.
    pub host: String,
    /// Port to advertise in the handshake, same caveat as `host`.
    pub port: u16,
    /// The account name. Must be unique per session — see the module docs.
    pub username: String,
}

impl JoinTarget {
    /// The default target: a relay on the loopback port `web/README.md`
    /// documents, bridging to a server on the vanilla default port.
    #[must_use]
    pub fn default_target() -> Self {
        Self {
            relay_url: "ws://127.0.0.1:25580".to_string(),
            host: "127.0.0.1".to_string(),
            port: 25565,
            username: browser_username(),
        }
    }
}

/// A per-session username, unique by construction.
///
/// `performance.now()` is sub-millisecond and monotonic within a page, and
/// `Date.now()` separates reloads, so their sum keyed to a short hex suffix
/// gives a fresh name per join without a random source. Vanilla caps names at
/// 16 characters; this is 11.
#[must_use]
pub fn browser_username() -> String {
    let millis = js_sys::Date::now();
    let fine = web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0);
    let seed = (millis as u64).wrapping_mul(1_000).wrapping_add(fine as u64);
    format!("web{:08x}", (seed & 0xffff_ffff) as u32)
}

/// Opens the WebSocket to the relay and starts a real client session over it.
///
/// The returned pair is the ordinary [`ClientHandle`] / [`EventStream`] every
/// native caller gets: the only thing that differs from a native join is the
/// transport handed to `connect_with`.
///
/// # Errors
///
/// Returns a human-readable message if the WebSocket cannot be opened (no relay
/// listening, wrong URL scheme, or a mixed-content block on an `https` page).
/// Failures *after* the socket opens — a kick, a protocol error — arrive on the
/// event stream instead, not here.
pub async fn join(target: &JoinTarget) -> Result<(ClientHandle, EventStream), String> {
    let transport = WsWebTransport::connect(&target.relay_url)
        .await
        .map_err(|error| {
            format!(
                "cannot open {}: {error} — is `lodestone-relay` listening there?",
                target.relay_url
            )
        })?;

    // Breadcrumbs, kept deliberately: a panic anywhere below aborts the wasm
    // instance without unwinding (`panic = "abort"` in the release profile), so
    // the *last line logged* is the only evidence of where it stopped. This is
    // how `V770Adapter::new`'s `std::time::Instant::now()` was found.
    log::info!("[mp] relay socket open");

    let server = ServerAddress {
        host: target.host.clone(),
        port: target.port,
    };
    let profile = LoginProfile {
        username: target.username.clone(),
        // Discarded by an offline-mode server (it derives the UUID from the
        // username), and `getrandom`'s `js` backend is enabled for this crate,
        // so a v4 is both honest and cheap here.
        uuid: Uuid::new_v4(),
    };

    // The one real 26.2 adapter. `singleplayer.rs` uses a `StandInProtocol`
    // instead, which is why it is not evidence that this path works.
    log::info!("[mp] constructing the v770 adapter");
    let adapter = Box::new(lodestone_v770::adapter());
    log::info!("[mp] adapter ready — starting the driver");

    Ok(ClientBuilder::new(server, profile, adapter).connect_with(transport))
}

/// An owned, lock-free snapshot of the loaded world around a point, ready to
/// mesh.
pub struct LiveSections {
    /// `(chunk_x, chunk_z, section_index)` → block-state section. Sections that
    /// are unloaded or all-air are simply absent.
    pub sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
    /// The dimension's bottom block-`y`, needed to place section `i` at
    /// `min_y + i * 16`.
    pub min_y: i32,
    /// Sections per column (`height / 16`).
    pub section_count: usize,
    /// How many columns contributed (for the HUD).
    pub columns: usize,
    /// Total columns the client currently holds, loaded or not in range.
    pub loaded_columns: usize,
}

/// Pulls every non-empty section of every loaded column within `radius` columns
/// of `centre`.
///
/// Returns `None` before the dimension's vertical extent is known — i.e. before
/// the first column arrives — because a section index means nothing without the
/// `min_y` anchor. Every section comes back as an `Arc` snapshot carrying no
/// borrow and pinning no lock, so meshing can take as long as it likes while
/// streaming continues.
#[must_use]
pub fn collect_sections(
    handle: &ClientHandle,
    centre: ChunkPos,
    radius: i32,
) -> Option<LiveSections> {
    let dimensions = handle.world_dimensions()?;
    let section_count = dimensions.section_count();
    if section_count == 0 {
        return None;
    }

    let loaded = handle.loaded_chunks();
    let loaded_columns = loaded.len();
    let in_range: Vec<ChunkPos> = loaded
        .into_iter()
        .filter(|pos| (pos.x - centre.x).abs() <= radius && (pos.z - centre.z).abs() <= radius)
        .collect();

    // One lock acquisition for the whole neighbourhood, per `sections_at`'s doc.
    let requests: Vec<(ChunkPos, usize)> = in_range
        .iter()
        .flat_map(|pos| (0..section_count).map(move |index| (*pos, index)))
        .collect();
    let answers = handle.sections_at(&requests);

    let mut sections = HashMap::new();
    for ((pos, index), section) in requests.iter().zip(answers) {
        if let Some(section) = section {
            sections.insert((pos.x, pos.z, *index), section);
        }
    }

    Some(LiveSections {
        sections,
        min_y: dimensions.min_y,
        section_count,
        columns: in_range.len(),
        loaded_columns,
    })
}

impl LiveSections {
    /// The distinct non-air block-state ids present, which is what the atlas and
    /// classifier have to be built for.
    #[must_use]
    pub fn distinct_block_ids(&self) -> Vec<u32> {
        let mut seen = std::collections::BTreeSet::new();
        for section in self.sections.values() {
            for y in 0..16 {
                for z in 0..16 {
                    for x in 0..16 {
                        let id = section.get_block(x, y, z);
                        if id != 0 {
                            seen.insert(id);
                        }
                    }
                }
            }
        }
        seen.into_iter().collect()
    }
}
