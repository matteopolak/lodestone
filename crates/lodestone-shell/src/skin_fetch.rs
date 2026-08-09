//! Fetching **our own** skin after sign-in, and getting it onto the screen in
//! the same session (issue #62).
//!
//! ## What it is
//!
//! The producer half of `docs/player-skins.md`. `lodestone-auth`'s
//! `fetch_profile` now keeps the services profile's `skins` array, so a
//! completed sign-in carries a texture URL and a rig declaration; this module
//! turns those into a decoded 64×64 sheet in two places at once:
//!
//! 1. **the cache** — `<data_dir>/skin.png` plus `<data_dir>/skin.model`, the
//!    exact pair `container::player_preview::local_skin_override` already reads
//!    at startup, so every *later* launch draws it with no code involved;
//! 2. **the pending slot** — [`publish`]/[`take_pending`], drained by
//!    `ContainerRenderer::render_geometry_scaled`, so **this** session's
//!    inventory avatar changes without a restart.
//!
//! Writing only the cache would have been the smaller change and it would have
//! been an island in the shape this repo keeps hitting: sign-in lives in the
//! main menu, and the inventory is opened later in the same run, so the whole
//! visible effect of the fetch would have been deferred to the next launch.
//! `PlayerPreview` is built once, during `app::lifecycle`'s resume, and never
//! re-reads the cache.
//!
//! ## How it works
//!
//! ```text
//! menu::accounts worker  (device-code and loopback, both)
//!   -> lodestone_auth::login::finish_interactive          -- Session { profile.skin }
//!   -> fetch_own_skin(&client, &profile)
//!        -> lodestone_auth::texture::fetch_texture        -- TextureUrlChecker, then GET
//!        -> Image::decode_png
//!        -> write <data_dir>/skin.png + skin.model        -- the startup path
//!        -> publish(model, image)                         -- this session's path
//! ContainerRenderer::render_geometry_scaled
//!   -> take_pending() -> set_player_skin(..)
//! ```
//!
//! Every failure is a `warn!` and a `false`, never an error the sign-in inherits:
//! a refused host, a dead CDN or a corrupt PNG must not fail an otherwise
//! successful login over a cosmetic asset.
//!
//! ## How to change it, and the gotchas
//!
//! **The host allow list is not optional and does not belong here.** The URL
//! arrives over the network, so it goes through
//! [`lodestone_auth::texture::fetch_texture`], which applies authlib's own
//! `TextureUrlChecker` *before* opening a socket. Do not add a "just fetch it"
//! path here for a URL from a `PlayerInfo` entry either — that one is strictly
//! less trustworthy than our own profile's.
//!
//! **The rig comes from the same parse the other two paths use.** The services
//! profile says `CLASSIC`/`SLIM`; a `textures` property says `default`/`slim`.
//! `SkinVariant::legacy_services_id` bridges the two so
//! `PlayerModelType::by_legacy_services_name` stays the single decision, and
//! `skin.model` on disk is written in the *property* vocabulary — the one
//! `local_skin_override` reads. Writing `CLASSIC` there would silently resolve
//! wide-by-fallback and look right for every Steve and wrong for every Alex.
//!
//! **The pending slot is a slot, not a queue.** A second fetch replaces an
//! undrained first one, which is what you want: only the newest skin matters.
//! It is drained on the next container frame, so a fetch that completes while no
//! container is open is applied the moment one opens.
//!
//! ## Dependencies
//!
//! * `lodestone-auth` — `texture::fetch_texture` (the allow list and the GET),
//!   `Profile`/`ProfileSkin`/`SkinVariant`, `paths::data_dir`.
//! * `lodestone-assets` — `Image::decode_png`, `PlayerModelType`.
//! * `crate::container::ContainerRenderer::set_player_skin` — the draw seam.

use std::sync::Mutex;

use lodestone_assets::{Image, PlayerModelType};

/// The most recently fetched skin, waiting for a container frame to bind it.
///
/// A `Mutex<Option<..>>` rather than a channel because the consumer
/// (`render_geometry_scaled`) is on the render thread and the producer is a
/// short-lived worker: there is no back-pressure to model, and a stale entry is
/// simply overwritten. Poisoning is ignored — a panicking producer must not stop
/// the renderer from drawing.
static PENDING: Mutex<Option<(PlayerModelType, Image)>> = Mutex::new(None);

/// Hand a decoded sheet to the renderer. Replaces any undrained earlier one.
pub(crate) fn publish(model: PlayerModelType, image: Image) {
    match PENDING.lock() {
        Ok(mut slot) => *slot = Some((model, image)),
        Err(poisoned) => *poisoned.into_inner() = Some((model, image)),
    }
}

/// Take the pending skin, if any. Called once per container frame from
/// `ContainerRenderer::render_geometry_scaled`; `None` on all but the one frame
/// after a fetch lands.
pub(crate) fn take_pending() -> Option<(PlayerModelType, Image)> {
    match PENDING.lock() {
        Ok(mut slot) => slot.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// Write the fetched sheet into the same cache `local_skin_override` reads, so
/// the next launch draws it without any of this running again.
///
/// Best-effort: a read-only data directory costs the *cache*, not this session's
/// avatar, because [`publish`] has already happened by then.
/// Native-only: reached only from [`fetch_own_skin`], which is itself native-only.
/// A browser cache would be `localStorage`/IndexedDB, not `std::fs` (whose writes
/// return `Err(Unsupported)` there), and it has nothing to cache until the fetch
/// path above it exists.
#[cfg(not(target_arch = "wasm32"))]
fn write_cache(model: PlayerModelType, png: &[u8]) {
    let dir = lodestone_auth::paths::data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(target: "assets", "could not create {}: {e}", dir.display());
        return;
    }
    if let Err(e) = std::fs::write(dir.join("skin.png"), png) {
        tracing::warn!(target: "assets", "could not cache skin.png: {e}");
        return;
    }
    // The **legacy services** spelling (`default`/`slim`), because that is what
    // `local_skin_override` parses. See this module's gotchas.
    let marker = model.legacy_services_id();
    if let Err(e) = std::fs::write(dir.join("skin.model"), marker) {
        tracing::warn!(target: "assets", "could not cache skin.model: {e}");
    }
}

/// Fetch, decode, cache and publish the skin a signed-in profile declares.
///
/// Returns whether a sheet reached [`publish`]. `false` for "the profile
/// declares no skin" (the common case for an account that has never set one)
/// just as much as for a failure, and both are logged; no caller has anything
/// different to do about them.
/// Native-only. Both parameter types are `cfg(not(wasm32))` at their own crates:
/// `reqwest` is not a browser dependency of this crate, and
/// `lodestone_auth::Profile` lives in the `flow` module gated with the rest of the
/// Microsoft sign-in. Its only callers are in `menu::accounts`' sign-in workers,
/// which are gated for the same reason — so there is no browser call site to
/// satisfy, and no stub is needed.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn fetch_own_skin(
    client: &reqwest::Client,
    profile: &lodestone_auth::Profile,
) -> bool {
    let Some(skin) = profile.skin.as_ref() else {
        tracing::info!(
            target: "assets",
            player = %profile.name,
            "the signed-in profile declares no skin; the default rig stays"
        );
        return false;
    };
    let png = match lodestone_auth::texture::fetch_texture(client, &skin.url).await {
        Ok(bytes) => bytes,
        Err(e) => {
            // Deliberately includes the refused-host case: a URL outside
            // vanilla's allow list is a `warn!` here and nothing more.
            tracing::warn!(target: "assets", "could not fetch the profile skin: {e}");
            return false;
        }
    };
    let image = match Image::decode_png(&png) {
        Ok(img) => img,
        Err(e) => {
            tracing::warn!(target: "assets", "the profile skin did not decode as a PNG: {e}");
            return false;
        }
    };
    // One parse for all three sources of a rig declaration — see the gotchas.
    let model = PlayerModelType::by_legacy_services_name(Some(skin.variant.legacy_services_id()));
    tracing::info!(
        target: "assets",
        player = %profile.name,
        model = model.serialized_name(),
        bytes = png.len(),
        "fetched the profile skin"
    );
    write_cache(model, &png);
    publish(model, image);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(w: u32, h: u32) -> Image {
        Image {
            width: w,
            height: h,
            rgba: vec![0u8; (w * h * 4) as usize],
        }
    }

    /// The slot's whole contract: what goes in comes out once, and the *newest*
    /// wins. `take_pending` returning `None` afterwards is what keeps the
    /// renderer from re-binding the same texture on every frame.
    #[test]
    fn the_pending_slot_yields_the_newest_skin_exactly_once() {
        // Drain anything a concurrently-running test in this binary left behind.
        let _ = take_pending();

        publish(PlayerModelType::Wide, sheet(64, 64));
        publish(PlayerModelType::Slim, sheet(64, 64));
        let (model, img) = take_pending().expect("a published skin must be takeable");
        assert_eq!(model, PlayerModelType::Slim, "the newest publish must win");
        assert_eq!((img.width, img.height), (64, 64));
        assert!(take_pending().is_none(), "the slot must not yield twice");
    }

    /// The vocabulary bridge, asserted as a pair so a swapped mapping cannot
    /// pass: `CLASSIC` must reach the **wide** rig through the spelling
    /// `default`, and `SLIM` the slim one. Matching `variant` on `"wide"`
    /// anywhere in this chain resolves *every* skin wide, slim included, and the
    /// only symptom is Alex's arms a texel too thick.
    #[test]
    fn the_services_variant_reaches_the_rig_through_the_legacy_spelling() {
        for (variant, expected) in [
            (lodestone_auth::SkinVariant::Classic, PlayerModelType::Wide),
            (lodestone_auth::SkinVariant::Slim, PlayerModelType::Slim),
        ] {
            let id = variant.legacy_services_id();
            assert_eq!(
                PlayerModelType::by_legacy_services_name(Some(id)),
                expected,
                "{variant:?} spelled {id:?} must reach {expected:?}"
            );
        }
        // And the spelling really is the non-obvious one.
        assert_eq!(
            lodestone_auth::SkinVariant::Classic.legacy_services_id(),
            "default"
        );
    }

    /// `write_cache` must produce exactly the pair `local_skin_override` reads,
    /// with the marker in the property vocabulary. Uses `LODESTONE_DATA_DIR`?
    /// No — `set_var` is `unsafe` under this workspace's lint, so this asserts
    /// the *filenames and marker content* through the same accessors instead,
    /// which is the part that can silently disagree.
    #[test]
    fn the_cache_marker_is_the_spelling_the_startup_path_parses() {
        for model in [PlayerModelType::Wide, PlayerModelType::Slim] {
            let marker = model.legacy_services_id();
            assert_eq!(
                PlayerModelType::by_legacy_services_name(Some(marker)),
                model,
                "the marker `write_cache` writes must round-trip through the \
                 parse `local_skin_override` uses"
            );
        }
    }
}
