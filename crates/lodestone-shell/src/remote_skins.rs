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
use std::sync::Mutex;

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

/// Every URL we have started, finished or given up on.
static FETCHED: Mutex<Option<HashMap<String, FetchState>>> = Mutex::new(None);

/// Sheets waiting for a frame to upload them, as `(url, decoded PNG)`.
static READY: Mutex<Vec<(String, Image)>> = Mutex::new(Vec::new());

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
                })
            });
        if decoded.is_none() {
            // Logged once per distinct property value, not once per frame —
            // which is the point of caching the `None` as well as the `Some`.
            tracing::debug!(
                target: "assets",
                player = %profile.name,
                "the profile properties carry no usable skin texture"
            );
        }
        map.insert(value.to_owned(), decoded.clone());
        decoded
    })
}

/// Start fetching `url` unless it has already been started, finished or failed.
///
/// Idempotent, so the per-frame call site can hand it the same URLs forever.
pub fn request(url: &str) {
    let start = with_map(&FETCHED, |map| {
        if map.contains_key(url) {
            return false;
        }
        map.insert(url.to_owned(), FetchState::Pending);
        true
    });
    if start {
        spawn_fetch(url.to_owned());
    }
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

/// Hand a decoded sheet to the renderer.
fn publish(url: String, image: Image) {
    match READY.lock() {
        Ok(mut ready) => ready.push((url, image)),
        Err(poisoned) => poisoned.into_inner().push((url, image)),
    }
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
#[cfg(not(test))]
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
                        tracing::info!(
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

    fn profile_with(value: &str) -> GameProfile {
        let mut profile = GameProfile::new(uuid::Uuid::nil(), "Tester");
        profile.properties.push(ProfileProperty {
            name: "textures".to_owned(),
            value: value.to_owned(),
            signature: None,
        });
        profile
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

    /// The ready queue hands each sheet over exactly once — the renderer builds
    /// a bind group from it and keeps that, so a second yield would rebuild the
    /// same texture on every frame.
    #[test]
    fn the_ready_queue_yields_each_sheet_exactly_once() {
        let _ = drain_ready();
        publish(
            "https://textures.minecraft.net/texture/drained".to_owned(),
            Image {
                width: 64,
                height: 64,
                rgba: vec![0u8; 64 * 64 * 4],
            },
        );
        let drained = drain_ready();
        assert_eq!(drained.len(), 1);
        assert_eq!((drained[0].1.width, drained[0].1.height), (64, 64));
        assert!(drain_ready().is_empty());
    }
}
