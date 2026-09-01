//! Remote players' skins: the `textures` profile property off the tab list,
//! through vanilla's host allow list, to a per-player bind group in the world
//! entity pass.
//!
//! ## What it is
//!
//! The sibling of [`crate::skin_fetch`], which does the same job for **our own**
//! account. That one starts from the services `/minecraft/profile` response and
//! lands on the inventory avatar; this one starts from the `ADD_PLAYER` profile
//! properties the tab list already carries and lands on other players' bodies in
//! the world.
//!
//! ```text
//! player_info ADD_PLAYER -> GameProfile::properties  (already decoded)
//!   entities::resolve_entity_facts
//!     -> skin_for_profile(&profile)          -- memoised base64+JSON decode
//!     -> EntityDraw::player_skin
//!   app::redraw
//!     -> request_all(this frame's urls)      -- one fetch per URL, ever
//!     -> RenderState::install_pending_player_skins
//!   RenderState::prepare_entities
//!     -> group by (hurt, skin url); EntityDrawBatch::skin
//!   the draw
//!     -> player_skins[url], falling back to the model's default sheet
//! ```
//!
//! ## How it works
//!
//! Two caches, both keyed by the texture **URL** and not by player UUID: two
//! accounts wearing the same skin (every default-skin account, and any two
//! players who picked the same one) then share one decode, one GET and one bind
//! group. The URL is also stable across a reconnect, where an entity id is not.
//!
//! * [`skin_for_profile`] memoises the *decode* against the raw property value.
//!   It runs once per player per session rather than once per player per frame,
//!   which matters because the caller is `resolve_entity_facts` — inside the
//!   per-frame entity fold.
//! * [`request`] memoises the *fetch* against the URL, with a three-state entry
//!   so a failure is remembered. Without that, a dead CDN or a refused host
//!   would be retried every frame, for every player, forever.
//!
//! ## How to change it, and the gotchas
//!
//! **The wide model is spelled `default`, not `wide`.** That decision is not
//! made here — it lives in [`lodestone_assets::PlayerModelType::by_legacy_services_name`],
//! and this module reaches it through `lodestone_assets::decode_textures_property`
//! so there is exactly one parse. Matching `metadata.model` against `"wide"`
//! anywhere resolves **every** skin wide, slim ones included, with no error and
//! no blank texture — only slightly-too-thick arms.
//!
//! **The host allow list is not optional, and a remote URL is the least
//! trustworthy input in this codebase.** It arrives from whatever server we
//! joined. [`lodestone_auth::texture::fetch_texture`] applies authlib's
//! `TextureUrlChecker` before opening a socket, including the two clauses that
//! only a bytecode read reveals: the host must **already** be lower-case
//! (`HTTPS://TEXTURES.MINECRAFT.NET/…` is refused, not folded) and the
//! allowed-domain test is exact-match on the whole host. **Do not add a laxer
//! path here.** Note the `url` crate lower-cases both scheme and host while
//! parsing, which is precisely the question that rule asks case-sensitively, so
//! a check built on `Url::host_str` alone looks rigorous and accepts all four
//! upper-case spellings.
//!
//! **The fetch forks on `#[cfg(test)]`, not on `cfg!(test)`.** A unit test that
//! reached [`request`] would otherwise perform a real HTTP GET as a side effect
//! of `cargo test`, which no health check in this repo can see. The test build
//! records the URL in [`requested_urls`] instead, so the routing is *assertable*
//! rather than silently skipped.
//!
//! **An unknown or still-fetching skin is not an error and must not be one.**
//! The draw falls back to the model's own default sheet, so a remote player is
//! Steve until their skin lands and then becomes themselves. Offline-mode
//! servers send no `textures` property at all (the UUID is derived from the
//! username), so that fallback is the *normal* path against every one of our own
//! oracles — a gate here must never assert that a skin arrives.
//!
//! ## Dependencies
//!
//! * `lodestone-assets` — `decode_textures_property`, `PlayerModelType`,
//!   `Image::decode_png`.
//! * `lodestone-auth` — `texture::fetch_texture` (the allow list and the GET).
//! * `lodestone-game` — `tablist::GameProfile::skin_texture`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestone_assets::{Image, PlayerModelType};

/// One remote player's declared skin: where to fetch it and which rig it wants.
///
/// `model` is the whole reason this is not just a `String`: the rig and the
/// sheet have to change **together**. A slim-authored sheet on the wide rig puts
/// the arm UVs a texel out, which reads as a texture bug rather than a model
/// bug, and the wide sheet on the slim rig leaves a gap at the shoulder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSkin {
    /// The texture URL, verbatim off the wire. Not yet host-checked — that
    /// happens in [`request`], before any socket is opened.
    pub url: String,
    /// The declared rig, from `metadata.model`.
    pub model: PlayerModelType,
    /// The `CAPE` property's texture URL, when the profile declares one.
    /// Fetched through the exact same URL-keyed pipeline as [`Self::url`] —
    /// [`request`]/[`drain_ready`] know nothing about skins vs. capes, so a
    /// cape URL just becomes one more entry in [`FETCHED`]/[`READY`], and the
    /// GPU-side bind-group cache (`RenderState::player_skins`,
    /// `install_pending_player_skins`) already caches by URL, not by "is this
    /// a skin". Not yet host-checked, same as [`Self::url`].
    pub cape: Option<String>,
    /// The built-in identity sheet to draw while [`Self::url`] has no bind
    /// group — a corpus reference like `entity/player/slim/ari`, resolved by
    /// `DefaultPlayerSkin.get(uuid)`'s hash pick.
    ///
    /// **Not a fallback for failure only.** Vanilla's `SkinManager` resolves a
    /// `PlayerSkin` whose texture is the default one until the fetched sheet
    /// lands, so this is what a player looks like for the first few frames after
    /// they come into view, as well as forever on an offline-mode server (which
    /// sends no `textures` property at all) and for any account that has never
    /// set a skin.
    ///
    /// It is populated **outside** [`skin_for_textures_property`]'s memoised
    /// decode, and that is load-bearing: the decode is keyed by the raw property
    /// value so two accounts wearing one skin share an entry, while this field
    /// is a function of the *uuid* and must differ between them. Setting it
    /// inside the memoisation would hand the second account the first's
    /// identity.
    pub default_sheet: &'static str,
}

/// What we know about one texture URL.
///
/// `Done`/`Failed` are only ever *constructed* by the real fetch, which the test
/// build replaces wholesale (see [`spawn_fetch`]) — so they are legitimately dead
/// under `cfg(test)` and nowhere else.
#[cfg_attr(test, allow(dead_code, reason = "only the real fetch reaches these"))]
enum FetchState {
    /// A worker is in flight.
    Pending,
    /// Decoded and handed to [`READY`]; kept so a re-request is a no-op.
    Done,
    /// Refused host, dead CDN, oversized body or undecodable PNG. **Remembered
    /// on purpose** — the alternative is retrying every frame forever.
    Failed,
}

/// Memoised decode of a raw `textures` property value.
///
/// Keyed by the property value rather than the player UUID: the base64 blob is
/// self-contained, so two players wearing the same skin hash to one entry.
static DECODED: Mutex<Option<HashMap<String, Option<RemoteSkin>>>> = Mutex::new(None);

/// The most recent **real** (non-default) skin resolved for each player UUID.
///
/// This is the one cache in this module keyed by player UUID rather than
/// texture URL, and it exists for a different reason: `entities.rs`'s
/// `resolve_entity_facts` re-derives a player's skin from the tab list *every
/// frame*, and the tab list is not append-only — a `player_info_remove`
/// clears a UUID's entry outright. A player-type NPC whose plugin adds a
/// tab-list entry (carrying `textures`) and then removes it shortly after —
/// a common technique for keeping a fake player out of the visible player
/// list while its entity stays spawned — makes the entry vanish exactly the
/// way a real disconnect would, and `resolve_entity_facts` cannot tell the
/// two apart from the tab list alone. Without this cache, that lookup miss
/// silently falls back to the UUID-hash default skin every frame afterwards,
/// even though the real texture is still sitting in `player_skins`' GPU
/// cache and `DECODED`/`FETCHED` above — nothing was lost, the *association*
/// to this UUID just had nowhere to live once the tab-list entry was gone.
/// See `entities::resolve_entity_facts`'s use of [`remember`]/[`last_known`].
static LAST_KNOWN: Mutex<Option<HashMap<uuid::Uuid, RemoteSkin>>> = Mutex::new(None);

/// The most recent tab-list display name resolved for each player UUID.
///
/// Same shape and same reason as [`LAST_KNOWN`] one field over: a player-type
/// NPC's name comes from the same tab-list profile as its skin, through the
/// same `entities::resolve_entity_facts` per-frame re-derivation, and is
/// vulnerable to the identical `player_info_remove`-shaped miss when a
/// plugin adds a tab-list entry and then removes it while the entity stays
/// spawned. Without this cache the nametag simply stops drawing the instant
/// the entry disappears, even though nothing about the entity or its name
/// actually changed — see `entities::resolve_entity_facts`'s use of
/// [`remember_name`]/[`last_known_name`].
static NAME_LAST_KNOWN: Mutex<Option<HashMap<uuid::Uuid, String>>> = Mutex::new(None);

/// **Our own** skin, paired with the profile UUID
/// `Sim::local_player_skin` resolved it for.
///
/// The slot retains one current local player but carries its UUID, so a
/// renderer resolving a newly selected account cannot consume the prior
/// account's value. It exists for a consumer that cannot reach
/// the value any other way: the **first-person arm**
/// (`RenderState::prepare_first_person_hand`).
///
/// The arm and the third-person body are mutually exclusive by construction —
/// the arm draws precisely on the frames `Sim::third_person_body_state` returns
/// `None` — so the arm structurally cannot read the body's `ThirdPersonBodyState`,
/// which is where every other piece of local-player draw state travels. A slot
/// here is how the resolution crosses that gate.
///
/// A `Mutex<Option<..>>` and not a channel for [`LAST_KNOWN`]'s reasons: the
/// producer overwrites, the consumer never drains, and poisoning is ignored
/// because a panicking producer must not stop the renderer from drawing.
static LOCAL: Mutex<Option<(uuid::Uuid, RemoteSkin)>> = Mutex::new(None);

/// Publish the local player's resolved skin for the first-person arm to read.
/// Called every frame by `Sim::local_player_skin`; the newest wins.
pub fn set_local(id: uuid::Uuid, skin: &RemoteSkin) {
    let mut guard = match LOCAL.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some((id, skin.clone()));
}

/// The local player's skin, or `None` before the first resolution (pre-login,
/// or a server that never told us our own uuid). See [`LOCAL`].
#[must_use]
pub fn local() -> Option<RemoteSkin> {
    match LOCAL.lock() {
        Ok(guard) => guard.as_ref().map(|(_, skin)| skin.clone()),
        Err(poisoned) => poisoned
            .into_inner()
            .as_ref()
            .map(|(_, skin)| skin.clone()),
    }
}

/// The local skin only when it belongs to `id`.
///
/// The inventory preview knows the UUID of the live session. Requiring that
/// UUID here prevents the process-global hand-off from showing the previously
/// selected account during the first frames of a new join.
#[must_use]
pub fn local_for(id: uuid::Uuid) -> Option<RemoteSkin> {
    match LOCAL.lock() {
        Ok(guard) => local_for_slot(&guard, id),
        Err(poisoned) => local_for_slot(&poisoned.into_inner(), id),
    }
}

fn local_for_slot(
    slot: &Option<(uuid::Uuid, RemoteSkin)>,
    id: uuid::Uuid,
) -> Option<RemoteSkin> {
    slot.as_ref()
        .filter(|(owner, _)| *owner == id)
        .map(|(_, skin)| skin.clone())
}

/// Every URL we have started, finished or given up on.
static FETCHED: Mutex<Option<HashMap<String, FetchState>>> = Mutex::new(None);

/// Sheets waiting for a frame to upload them, as `(url, decoded PNG)`.
static READY: Mutex<Vec<(String, Image)>> = Mutex::new(Vec::new());

/// Every sheet a fetch has decoded this session, **retained** and keyed by the
/// same texture URL [`READY`] uses.
///
/// [`READY`] is a *drain*, and a drain has exactly one consumer: whoever calls
/// [`drain_ready`] first takes the sheet and every other consumer never sees it.
/// The world entity pass (`RenderState::install_pending_player_skins`) is that
/// consumer. A placed head and a head in an inventory slot are drawn by
/// two *different* passes owning two different bind-group caches — see
/// `hud::item_icon`'s `SpecialIcons`, which owns everything it draws with — so a
/// second drain would not have given the GUI pass a sheet, it would have stolen
/// one from the world.
///
/// So the GUI side **pulls** instead: it asks [`sheet`] for a URL it has no bind
/// group for, on every frame that draws one, and uploads on the first frame the
/// answer is `Some`. That also removes the ordering hazard outright — a record
/// built before the fetch lands simply resolves on a later frame, where a
/// one-shot drain would have had to arrive on exactly the right one.
///
/// `Arc<Image>` so two readers share one 16 KB decode rather than copying it.
static SHEETS: Mutex<Option<HashMap<String, Arc<Image>>>> = Mutex::new(None);

/// Monotonic generation for [`SHEETS`]. Consumers use this cheap atomic check
/// to avoid cloning the retained map on every frame; a newly-created renderer
/// or one that missed a publication enumerates the cache once for its epoch.
static SHEETS_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Test-only record of every URL [`request`] routed to a fetch, in place of the
/// fetch itself.
#[cfg(test)]
static REQUESTED: Mutex<Vec<String>> = Mutex::new(Vec::new());


/// Runs `f` over a lazily-created map behind `slot`, ignoring poisoning.
///
/// Poisoning is ignored for [`skin_fetch`](crate::skin_fetch)'s reason: a
/// panicking worker must not stop the renderer from drawing. The `Option`
/// wrapper is only there because `HashMap::new` is not `const`.
fn with_map<K, V, R>(
    slot: &Mutex<Option<HashMap<K, V>>>,
    f: impl FnOnce(&mut HashMap<K, V>) -> R,
) -> R {
    let mut guard = match slot.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(guard.get_or_insert_with(HashMap::new))
}

/// The skin a tab-list profile declares, or `None` when it declares none.
///
/// `None` covers three cases that are all normal and none of which is an error:
/// the profile carries no `textures` property at all (**every** offline-mode
/// server, since the UUID is derived from the username), the property is not
/// decodable, or it decodes to a payload with no `SKIN` entry (an account that
/// has never set one — vanilla picks one of eight built-ins by UUID hash there,
/// which we do not model).
#[must_use]
pub fn skin_for_profile(profile: &lodestone_game::tablist::GameProfile) -> Option<RemoteSkin> {
    let value = profile.skin_texture()?;
    let mut skin = skin_for_textures_property(value)?;
    // The uuid-hash identity, stamped here rather than inside the decode: see
    // [`RemoteSkin::default_sheet`] for why the memoised decode structurally
    // cannot know it.
    skin.default_sheet = default_sheet_for_uuid(profile.id);
    Some(skin)
}

/// `DefaultPlayerSkin.get(uuid)`'s sheet reference — the identity this account
/// draws until (or unless) a real texture is bound for it.
///
/// The uuid→`i64` pair is `UUID.getMostSignificantBits`/`getLeastSignificantBits`,
/// which is what vanilla's hash is defined over.
#[must_use]
pub fn default_sheet_for_uuid(id: uuid::Uuid) -> &'static str {
    let (hi, lo) = id.as_u64_pair();
    lodestone_assets::skin::default_skin_for_uuid(hi as i64, lo as i64).texture
}

/// The usable skin declared by one raw `textures` profile-property value.
///
/// This is shared by tab-list profiles and placed player-head block entities:
/// both wire formats ultimately carry the same Base64-encoded Mojang texture
/// payload, so keeping the memoisation here prevents the two producers from
/// growing subtly different URL or model parsing rules.
#[must_use]
pub fn skin_for_textures_property(value: &str) -> Option<RemoteSkin> {
    with_map(&DECODED, |map| {
        if let Some(cached) = map.get(value) {
            return cached.clone();
        }
        let decoded = lodestone_assets::decode_textures_property(value)
            .ok()
            .and_then(|textures| {
                textures.skin.map(|skin| RemoteSkin {
                    url: skin.url,
                    model: skin.model,
                    cape: textures.cape,
                    // `DefaultPlayerSkin.getDefaultSkin()` — vanilla's own
                    // answer when no uuid is in hand, which is exactly this
                    // function's situation: it is keyed by the property value
                    // and shared with placed player heads, which carry a
                    // texture blob and no account. A caller that *does* know
                    // the uuid (`skin_for_profile`) overwrites this with the
                    // hash pick.
                    default_sheet: lodestone_assets::skin::default_skin().texture,
                })
            });
        if decoded.is_none() {
            // Logged once per distinct property value, not once per frame —
            // which is the point of caching the `None` as well as the `Some`.
            tracing::debug!(
                target: "assets",
                "a profile textures property carries no usable skin texture"
            );
        }
        map.insert(value.to_owned(), decoded.clone());
        decoded
    })
}

/// Records `skin` as the most recently resolved real skin for `id`, so a
/// later frame that cannot find `id` in the tab list can still recover it.
/// See [`LAST_KNOWN`]'s doc.
pub fn remember(id: uuid::Uuid, skin: &RemoteSkin) {
    with_map(&LAST_KNOWN, |map| {
        map.insert(id, skin.clone());
    });
}

/// The last real skin [`remember`] recorded for `id`, if any.
#[must_use]
pub fn last_known(id: &uuid::Uuid) -> Option<RemoteSkin> {
    with_map(&LAST_KNOWN, |map| map.get(id).cloned())
}

/// Records `name` as the most recently resolved tab-list display name for
/// `id`, so a later frame that cannot find `id` in the tab list can still
/// recover it. See [`NAME_LAST_KNOWN`]'s doc.
pub fn remember_name(id: uuid::Uuid, name: &str) {
    with_map(&NAME_LAST_KNOWN, |map| {
        map.insert(id, name.to_owned());
    });
}

/// The last display name [`remember_name`] recorded for `id`, if any.
#[must_use]
pub fn last_known_name(id: &uuid::Uuid) -> Option<String> {
    with_map(&NAME_LAST_KNOWN, |map| map.get(id).cloned())
}

/// The cache key **our own** profile skin is stored under — the sheet
/// `skin_fetch` downloads after sign-in and caches at `<data_dir>/skin.png`.
///
/// A key, **not a URL**, and deliberately not one: it names no host and must
/// never reach [`request`]. The real texture URL would work as a key too, but
/// only for a session that actually performed the sign-in — a *later* launch
/// has the cached PNG on disk and no memory of where it came from, so keying on
/// the URL would make the world body's skin depend on having signed in this run
/// while the inventory avatar's did not.
///
/// It exists because the two URL-keyed caches in this module (`SHEETS` and the
/// renderer's `player_skins` bind groups) are how a sheet reaches a draw, and
/// our own cached skin has to enter them somehow. Everything downstream treats
/// it as an ordinary entry.
pub const LOCAL_PROFILE_SKIN_KEY_PREFIX: &str = "lodestone-local-profile-skin:";

/// Start fetching `url` unless it has already been started, finished or failed.
///
/// Idempotent, so the per-frame call site can hand it the same URLs forever.
///
/// `""` is refused outright rather than treated as an ordinary (doomed) URL.
/// `entities::resolve_entity_facts` publishes a `RemoteSkin` with an empty
/// `url` as the sentinel for "no declared skin, drawing the uuid-derived
/// default sheet instead" (see its own `default_remote_skin` doc), and
/// `app/redraw.rs`'s per-frame `request_all` call collects every player's
/// `player_skin.url` unconditionally — real or synthetic. Without this guard
/// that sentinel would reach `spawn_fetch`, open (and fail) a real GET against
/// an empty URL once per session, and log a spurious warning for every player
/// with no declared skin, which after this change is the common case rather
/// than the rare one.
pub fn request(url: &str) {
    if url.is_empty() || url.starts_with(LOCAL_PROFILE_SKIN_KEY_PREFIX) {
        // The local-profile key names no host and its sheet is published
        // directly, so there is nothing to fetch. Refused here rather than
        // relying on every caller to remember, the same way the empty
        // sentinel is.
        return;
    }
    let start = with_map(&FETCHED, |map| {
        if map.contains_key(url) {
            return false;
        }
        map.insert(url.to_owned(), FetchState::Pending);
        true
    });
    if !start {
        return;
    }
    // The **same** allow list `lodestone_auth::texture::fetch_texture` applies
    // before opening a socket, called one layer earlier — not a second
    // implementation of it, a second application of it.
    //
    // Two things this earlier check buys, neither of which is security (the
    // inner check is what makes the fetch safe and stays where it is). A refused
    // URL now costs no thread, no runtime and no `reqwest::Client`, which is the
    // dominant case against any server that decorates with heads from a mirror.
    // And the refusal is recorded synchronously, so a URL that can never be
    // fetched is memoised `Failed` on the frame it first appears rather than
    // whenever a worker gets round to it.
    #[cfg(not(target_arch = "wasm32"))]
    if !lodestone_auth::texture::is_allowed_texture_domain(url) {
        tracing::warn!(
            target: "assets",
            "refusing a skin texture url outside the allowed domain; that head or \
             player draws the default sheet"
        );
        with_map(&FETCHED, |map| {
            map.insert(url.to_owned(), FetchState::Failed);
        });
        return;
    }
    spawn_fetch(url.to_owned());
}

/// Start fetching every URL in `urls`, skipping the ones already known.
pub fn request_all<'a>(urls: impl IntoIterator<Item = &'a str>) {
    for url in urls {
        request(url);
    }
}

/// Record the state of a finished fetch. Only the real fetch calls this.
#[cfg(not(test))]
fn finish(url: &str, state: FetchState) {
    with_map(&FETCHED, |map| {
        map.insert(url.to_owned(), state);
    });
}

/// Hand a decoded sheet to the renderers: onto the world pass's one-shot queue
/// ([`READY`]) *and* into the retained by-URL store ([`SHEETS`]) the GUI icon
/// pass pulls from. Both, because they are two consumers and a drain only ever
/// serves one — see [`SHEETS`]'s doc.
///
/// `pub` for a second reason beyond the fetch: it is the seam a headless gate
/// installs a sheet through. A pixel gate for a custom head must not perform an
/// HTTP GET as a side effect of `cargo test`, and it cannot reach the private
/// fetch fork an integration test compiles the library without.
pub fn publish(url: String, image: Image) {
    let shared = Arc::new(image.clone());
    with_map(&SHEETS, |map| {
        map.insert(url.clone(), shared);
    });
    SHEETS_EPOCH.fetch_add(1, Ordering::Release);
    match READY.lock() {
        Ok(mut ready) => ready.push((url, image)),
        Err(poisoned) => poisoned.into_inner().push((url, image)),
    }
}

/// The decoded sheet for `url`, if a fetch has finished it this session.
///
/// Unlike [`drain_ready`] this does **not** consume: it is the pull half of
/// [`SHEETS`]'s doc, for a consumer that builds its own bind groups and needs to
/// keep asking until the fetch lands.
#[must_use]
pub fn sheet(url: &str) -> Option<Arc<Image>> {
    with_map(&SHEETS, |map| map.get(url).cloned())
}

/// Every decoded sheet retained for the lifetime of this process.
///
/// The GUI pass pulls individual URLs with [`sheet`], while a world renderer
/// normally receives the one-shot [`drain_ready`] hand-off. A renderer can be
/// recreated after that hand-off (for example while changing display state),
/// so it also needs a way to rehydrate its bind-group cache without fetching or
/// decoding the skin again. The returned `Arc`s keep this recovery path free of
/// image copies; callers still decide which URLs they have already uploaded.
#[must_use]
pub fn cached_sheets() -> Vec<(String, Arc<Image>)> {
    with_map(&SHEETS, |map| {
        map.iter()
            .map(|(url, image)| (url.clone(), Arc::clone(image)))
            .collect()
    })
}

/// The retained-sheet generation, for consumers that cache their own uploads.
#[must_use]
pub fn sheets_epoch() -> u64 {
    SHEETS_EPOCH.load(Ordering::Acquire)
}

/// Take every sheet that has landed since the last call, for the renderer to
/// turn into bind groups. Empty on all but the few frames after a fetch lands.
#[must_use]
pub fn drain_ready() -> Vec<(String, Image)> {
    match READY.lock() {
        Ok(mut ready) => std::mem::take(&mut *ready),
        Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
    }
}

/// The real fetch: one short-lived thread with its own current-thread runtime,
/// the same shape `menu::status`'s one-shot ping uses.
///
/// A thread per *distinct skin*, not per player and not per frame, so the cost
/// is bounded by how many different skins are in view over a session. There is
/// no runtime to plumb through the render thread this way, and nothing here has
/// to be cancellable: the worst case is a wasted GET after a disconnect.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
fn spawn_fetch(url: String) {
    let for_failure = url.clone();
    let spawned = std::thread::Builder::new()
        .name("lodestone-remote-skin".to_owned())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(target: "assets", "no runtime for a skin fetch: {e}");
                    finish(&url, FetchState::Failed);
                    return;
                }
            };
            rt.block_on(async move {
                // Without an installed rustls crypto provider `Client::new()`
                // **panics**, and under this workspace's `panic = "abort"`
                // release profile that takes the process with it — so a placed
                // head or a remote player's skin would have killed the game on
                // any launch where nothing else had installed one first (a
                // signed-out session joining a server, for instance). Every
                // `Client::new()` in this tree needs this line; see
                // `lodestone_auth::tls`'s module doc, which states the rule.
                lodestone_auth::install_crypto_provider();
                let client = reqwest::Client::new();
                // The allow list lives in here, and is applied before a socket
                // is opened. See this module's gotchas.
                let png = match lodestone_auth::texture::fetch_texture(&client, &url).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::warn!(target: "assets", "could not fetch a remote skin: {e}");
                        finish(&url, FetchState::Failed);
                        return;
                    }
                };
                match Image::decode_png(&png) {
                    Ok(image) => {
                        tracing::debug!(
                            target: "assets",
                            bytes = png.len(),
                            "fetched a remote player's skin"
                        );
                        finish(&url, FetchState::Done);
                        publish(url, image);
                    }
                    Err(e) => {
                        tracing::warn!(target: "assets", "a remote skin did not decode: {e}");
                        finish(&url, FetchState::Failed);
                    }
                }
            });
        });
    if let Err(e) = spawned {
        // The entry is `Pending` at this point and nothing will ever finish it,
        // so mark it `Failed` rather than leaving a URL that can never be
        // retried *and* never draws.
        tracing::warn!(target: "assets", "could not spawn a skin fetch: {e}");
        finish(&for_failure, FetchState::Failed);
    }
}

/// Browser build: mark the fetch failed, with a reason, and draw the default skin.
///
/// A **third arm on the same fork**, for the same reason the test arm is a
/// `#[cfg]` fork rather than a `cfg!()` early return: routing that is forked is
/// assertable, routing that early-returns is a silent skip.
///
/// Three things are missing here and none is a shim away. The thread and the
/// blocking `current_thread` runtime are both fatal on wasm32 —
/// `std::thread::spawn` traps (measured: `RuntimeError: unreachable`) — so the
/// browser shape is `spawn_local` plus `fetch`, not this function with a different
/// executor. And `lodestone_auth::texture::fetch_texture` carries authlib's
/// `TextureUrlChecker` host allow list, which is applied *before* a socket opens;
/// it is `cfg(not(wasm32))` because it is built on `reqwest`. Reimplementing the
/// GET over `web_sys::fetch` without porting that allow list would drop the one
/// security check in this path, so the allow list has to move first.
///
/// `FetchState::Failed` rather than leaving the entry `Pending`: a pending URL that
/// nothing will ever finish can neither draw nor be retried, which is the same
/// argument the native arm's spawn-failure branch makes.
#[cfg(all(not(test), target_arch = "wasm32"))]
fn spawn_fetch(url: String) {
    tracing::warn!(
        target: "assets",
        "not fetching a remote player's skin in a browser: this path needs \
         lodestone_auth's TextureUrlChecker allow list, which is native-only \
         (reqwest-based). Players draw with the default skin."
    );
    finish(&url, FetchState::Failed);
}

/// Test build: record the URL instead of performing an HTTP GET.
///
/// A `cfg!(test)` early return inside the real function would make this a
/// *silent skip*; a `#[cfg(test)]` fork makes the routing assertable. See this
/// module's gotchas, and `DESIGN.md` on the unit test that was opening a browser
/// on every `cargo test`.
#[cfg(test)]
fn spawn_fetch(url: String) {
    match REQUESTED.lock() {
        Ok(mut seen) => seen.push(url),
        Err(poisoned) => poisoned.into_inner().push(url),
    }
}

/// Test-only: every URL [`request`] routed to a fetch so far.
#[cfg(test)]
#[must_use]
pub(crate) fn requested_urls() -> Vec<String> {
    match REQUESTED.lock() {
        Ok(seen) => seen.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::tablist::{GameProfile, ProfileProperty};

    /// A blank decoded sheet of the given size, for the queue/store tests that
    /// care about routing rather than about pixels.
    fn sheet_image(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            rgba: vec![0u8; (width * height * 4) as usize],
        }
    }

    fn profile_with(value: &str) -> GameProfile {
        let mut profile = GameProfile::new(uuid::Uuid::nil(), "Tester");
        profile.properties.push(ProfileProperty {
            name: "textures".to_owned(),
            value: value.to_owned(),
            signature: None,
        });
        profile
    }

    #[test]
    fn local_skin_handoff_is_scoped_to_the_active_account_uuid() {
        let alice = uuid::Uuid::from_u128(0xA11CE);
        let bob = uuid::Uuid::from_u128(0xB0B);
        let skin = RemoteSkin {
            url: "https://textures.minecraft.net/texture/alice".to_owned(),
            model: PlayerModelType::Slim,
            cape: None,
            default_sheet: "entity/player/slim/ari",
        };
        let slot = Some((alice, skin.clone()));
        assert_eq!(local_for_slot(&slot, alice), Some(skin));
        assert_eq!(
            local_for_slot(&slot, bob),
            None,
            "a preview for Bob must not consume Alice's process-local handoff"
        );
    }

    /// Base64 of a payload, built the way a real `textures` property is.
    fn payload(model: Option<&str>, url: &str) -> String {
        let metadata = model.map_or(String::new(), |m| {
            format!(r#","metadata":{{"model":"{m}"}}"#)
        });
        let json = format!(r#"{{"textures":{{"SKIN":{{"url":"{url}"{metadata}}}}}}}"#);
        base64_encode(json.as_bytes())
    }

    /// A local base64 encoder, so the fixture does not depend on which base64
    /// crate the workspace happens to expose.
    fn base64_encode(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                chunk.get(1).copied().unwrap_or(0),
                chunk.get(2).copied().unwrap_or(0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    let idx = ((n >> (18 - 6 * i)) & 0x3f) as usize;
                    out.push(char::from(TABLE[idx]));
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// **The `default`-not-`wide` trap**, asserted as a pair so a swapped or
    /// naive implementation cannot pass.
    ///
    /// `metadata.model` holds authlib's `legacyServicesId`, so wide is spelled
    /// `default`. Reading it as `"wide"` matches nothing, and
    /// `by_legacy_services_name`'s fallback *is* wide — so every slim skin
    /// silently resolves wide, with no error and no blank texture. The third
    /// case is the control that makes the pair meaningful: the literal string
    /// `"wide"` must **not** be the thing that selects the wide rig by matching,
    /// and here it lands on wide only via the fallback, exactly as a missing
    /// `metadata` does.
    #[test]
    fn the_declared_model_reaches_the_rig_through_the_legacy_spelling() {
        let url = "https://textures.minecraft.net/texture/abc";
        for (declared, expected) in [
            (Some("default"), PlayerModelType::Wide),
            (Some("slim"), PlayerModelType::Slim),
            (None, PlayerModelType::Wide),
        ] {
            let profile = profile_with(&payload(declared, url));
            let skin = skin_for_profile(&profile).expect("a SKIN entry must decode");
            assert_eq!(skin.url, url);
            assert_eq!(skin.model, expected, "metadata.model = {declared:?}");
        }
        // And `slim` really is distinguishable, which is the whole point: if the
        // parse read `"wide"` instead of `"default"` this pair would collapse.
        assert_ne!(
            skin_for_profile(&profile_with(&payload(Some("slim"), url)))
                .unwrap()
                .model,
            skin_for_profile(&profile_with(&payload(Some("default"), url)))
                .unwrap()
                .model
        );
    }

    /// A profile with no `textures` property — every offline-mode server — is
    /// `None` rather than an error or a default URL, and so is a property that
    /// is not decodable.
    #[test]
    fn a_profile_without_a_usable_texture_property_declares_no_skin() {
        let bare = GameProfile::new(uuid::Uuid::nil(), "Offline");
        assert!(skin_for_profile(&bare).is_none());
        assert!(skin_for_profile(&profile_with("not base64 at all !!!")).is_none());
        // Well-formed, but no `SKIN` entry: the skinless account.
        assert!(skin_for_profile(&profile_with(&base64_encode(b"{\"textures\":{}}"))).is_none());
    }

    /// `request` is idempotent per URL, which is what lets the per-frame call
    /// site hand it the same list forever.
    ///
    /// Also the routing assertion the `#[cfg(test)]` fork exists for: without
    /// it, this test would perform two real HTTP GETs.
    #[test]
    fn a_url_is_only_ever_requested_once() {
        let url = "https://textures.minecraft.net/texture/only-once-please";
        let before = requested_urls().iter().filter(|u| *u == url).count();
        request(url);
        request(url);
        request(url);
        let after = requested_urls().iter().filter(|u| *u == url).count();
        assert_eq!(after - before, 1, "one fetch per URL, ever");
    }

    /// `entities::resolve_entity_facts` now publishes a `RemoteSkin` with an
    /// empty `url` for a player with no declared `textures` property (the
    /// uuid-hash default sentinel), and `app/redraw.rs` collects every player's
    /// `player_skin.url` into `request_all` unconditionally — so a default
    /// player must never reach a fetch. The control: a non-empty url in the
    /// same call *does* get routed, so this is not passing because `request`
    /// broke entirely.
    #[test]
    fn an_empty_url_is_never_routed_to_a_fetch() {
        let before = requested_urls().len();
        request("");
        request("");
        assert_eq!(
            requested_urls().len(),
            before,
            "an empty url (the default-skin sentinel) must never reach spawn_fetch"
        );
        let real = "https://textures.minecraft.net/texture/empty-url-guard-control";
        request(real);
        assert!(
            requested_urls().contains(&real.to_owned()),
            "the guard must not have disabled routing for a real url"
        );
    }

    /// The ready queue hands each sheet over exactly once — the renderer builds
    /// a bind group from it and keeps that, so a second yield would rebuild the
    /// same texture on every frame.
    ///
    /// **Asserted per URL, not on the queue's length.** [`READY`] is
    /// process-wide and libtest runs a binary's tests on several threads, so any
    /// other publisher — this file's own sibling gate, `skin_fetch::publish`
    /// mirroring our profile sheet — legitimately has entries in the same drain.
    /// `drained.len() == 1` was asserting exclusivity, which is not a property
    /// this queue has and not the property under test.
    #[test]
    fn the_ready_queue_yields_each_sheet_exactly_once() {
        let url = "https://textures.minecraft.net/texture/drained";
        publish(
            url.to_owned(),
            Image {
                width: 64,
                height: 64,
                rgba: vec![0u8; 64 * 64 * 4],
            },
        );
        let mine: Vec<_> = drain_ready().into_iter().filter(|(u, _)| u == url).collect();
        assert_eq!(mine.len(), 1, "exactly one entry for this url");
        assert_eq!((mine[0].1.width, mine[0].1.height), (64, 64));
        assert!(
            !drain_ready().iter().any(|(u, _)| u == url),
            "a second drain yielded this url again -- the renderer would rebuild \
             the same texture every frame"
        );
    }

    /// The **pull** half, and the discriminator against the drain: the world
    /// pass taking a sheet off [`READY`] must not blind the GUI icon pass, which
    /// asks [`sheet`] for the same URL on every frame it draws that head.
    ///
    /// A gate that only checked `sheet(url).is_some()` would pass with `SHEETS`
    /// filled and `drain_ready` never called, so the drain here is the point of
    /// the test rather than setup — before this store existed, a second consumer
    /// would have seen exactly `None` at this line.
    #[test]
    fn a_retained_sheet_survives_the_world_passs_drain() {
        let url = "https://textures.minecraft.net/texture/retained-not-drained";
        assert!(
            sheet(url).is_none(),
            "sanity: this url must be unknown before the publish, or the \
             assertion below cannot attribute the hit to it"
        );
        publish(url.to_owned(), sheet_image(64, 64));

        // The world entity pass's once-per-frame drain.
        let drained = drain_ready();
        assert!(
            drained.iter().any(|(u, _)| u == url),
            "sanity: the publish must have reached the drain queue too"
        );
        assert!(
            !drain_ready().iter().any(|(u, _)| u == url),
            "the drain stays one-shot for this url"
        );

        let retained = sheet(url).expect(
            "the GUI icon pass must still be able to pull a sheet the world \
             pass already drained — two consumers, one fetch",
        );
        assert_eq!((retained.width, retained.height), (64, 64));
        assert!(
            sheet(url).is_some(),
            "the retained store must not consume either: the pull is retried \
             every frame until a bind group exists"
        );
        assert!(
            sheet("https://textures.minecraft.net/texture/never-published").is_none(),
            "control: an unpublished url must miss, or the hit above says \
             nothing about the key"
        );
    }

    /// A renderer rebuilt after the world pass drained [`READY`] must still be
    /// able to discover the decoded image. This is the lifecycle regression
    /// for player heads: losing the bind group must not turn a retained skin
    /// into a blank/default head or start another network request.
    #[test]
    fn cached_sheets_rehydrates_a_renderer_after_ready_is_drained() {
        let url = "https://textures.minecraft.net/texture/rehydrate-after-drain";
        assert!(
            cached_sheets().iter().all(|(known, _)| known != url),
            "sanity: this test URL must not already be retained"
        );
        let before = sheets_epoch();
        publish(url.to_owned(), sheet_image(64, 64));
        assert!(sheets_epoch() > before, "publishing a sheet advances its generation");
        let ready = drain_ready();
        assert!(ready.iter().any(|(known, _)| known == url));

        let retained = cached_sheets()
            .into_iter()
            .find(|(known, _)| known == url)
            .map(|(_, image)| image)
            .expect("a rebuilt renderer must see the retained decoded sheet");
        assert_eq!((retained.width, retained.height), (64, 64));
    }
}
