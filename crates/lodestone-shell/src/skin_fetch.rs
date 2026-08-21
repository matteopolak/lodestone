//! Fetching **our own** skin after sign-in, and getting it onto the screen in
//! the same session.
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

/// The last model [`publish`] handed out, kept **in addition to** [`PENDING`]
/// and never drained — [`PENDING`] is a one-shot slot the inventory avatar
/// consumes, so a rig this session already fetched would otherwise be
/// unreadable to any other consumer the moment a container first opens. See
/// [`current_model`], the reader this exists for.
static CURRENT: Mutex<Option<PlayerModelType>> = Mutex::new(None);

/// Whether the on-disk `<data_dir>/skin.png` has been offered to
/// [`crate::remote_skins`] yet, so the read is attempted **once** rather than
/// on every frame that asks.
///
/// A `OnceLock<bool>` rather than a bare flag because the answer is also the
/// result: `false` means "there is no usable cached sheet", and re-reading a
/// missing or corrupt file every frame is exactly the retry loop
/// `remote_skins::request`'s three-state memo exists to avoid.
static CACHED_SHEET_OFFERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The [`crate::remote_skins`] cache key our own profile skin is available
/// under, or `None` if we have none.
///
/// # Why the world body needs this and the inventory avatar does not
///
/// Our own body and arm resolve their skin from the **tab list**, the same
/// ladder every other player goes through — which is the right source, because
/// it is the profile the *server* saw. But an offline-mode server (and every
/// singleplayer world) sends no `textures` property at all, so that ladder
/// correctly reports "no declared skin" and falls to the uuid-hash identity.
///
/// The inventory avatar has never had that problem: it reads
/// `<data_dir>/skin.png` directly. So without this, the owner's own skin showed
/// in the inventory and an Ari or a Zuri showed in the world, in exactly the
/// case where both are drawing the same person. This closes that asymmetry by
/// putting the cached sheet into the same URL-keyed cache the world draw
/// already binds from.
///
/// # The read happens at most once
///
/// [`publish`] mirrors this session's fetch into the cache as it lands, so the
/// disk read only matters for a launch that has not signed in — the common
/// case, since a token refresh is not a sign-in. Guarded by
/// [`CACHED_SHEET_OFFERED`] so a per-frame caller does not re-read a missing
/// file forever.
#[must_use]
pub(crate) fn local_profile_sheet_key() -> Option<&'static str> {
    let key = crate::remote_skins::LOCAL_PROFILE_SKIN_KEY;
    if crate::remote_skins::sheet(key).is_some() {
        return Some(key);
    }
    let offered = *CACHED_SHEET_OFFERED.get_or_init(read_cached_sheet);
    offered.then_some(key)
}

/// The disk half of [`local_profile_sheet_key`]: read `<data_dir>/skin.png` and
/// hand it to [`crate::remote_skins`], returning whether that worked.
///
/// **Forks on `#[cfg(test)]`, not on `cfg!(test)`**, for `crate::remote_skins`'
/// own reason one layer over: this reads the *real* user data directory, so
/// under `cargo test` it would pick up whatever skin the person running the
/// suite happens to have signed in with. That is a filesystem side effect no
/// health check here can see, and it is not hypothetical — it made
/// `the_cached_profile_sheet_is_reachable_by_key_and_never_fetched` pass under
/// a neuter that had removed the mirroring entirely, because the assertion was
/// being satisfied by the owner's own cached PNG instead of by the code under
/// test. A `cfg!(test)` early return would have skipped it silently; the fork
/// makes the test build reach *only* the published-sheet path, so the gate can
/// fail.
#[cfg(not(test))]
fn read_cached_sheet() -> bool {
    {
        let key = crate::remote_skins::LOCAL_PROFILE_SKIN_KEY;
        let dir = lodestone_auth::paths::data_dir();
        // `Err(Unsupported)` in a browser rather than a trap — the same
        // degradation `container::player_preview::local_skin_override` already
        // takes for the identical read.
        let Ok(png) = std::fs::read(dir.join("skin.png")) else {
            tracing::debug!(
                target: "assets",
                "no cached skin.png, so our own body falls back to the uuid-hash identity"
            );
            return false;
        };
        match Image::decode_png(&png) {
            Ok(img) => {
                tracing::info!(
                    target: "assets",
                    "publishing the cached profile skin for our own body and arm"
                );
                crate::remote_skins::publish(key.to_owned(), img);
                true
            }
            Err(e) => {
                tracing::warn!(target: "assets", "cached skin.png did not decode: {e}");
                false
            }
        }
    }
}

/// The test build's half: there is no cached sheet unless a test published one,
/// so the only way to [`local_profile_sheet_key`] is through [`publish`] — the
/// production path a gate actually wants to exercise. See the native version's
/// doc for what this prevents.
#[cfg(test)]
fn read_cached_sheet() -> bool {
    false
}

/// Hand a decoded sheet to the renderer. Replaces any undrained earlier one.
pub(crate) fn publish(model: PlayerModelType, image: Image) {
    match CURRENT.lock() {
        Ok(mut slot) => *slot = Some(model),
        Err(poisoned) => *poisoned.into_inner() = Some(model),
    }
    // The world body and the first-person arm bind from `remote_skins`'
    // URL-keyed cache, not from `PENDING` — see `local_profile_sheet_key`. This
    // is the *fetch* half of that; the disk half is the read it guards. Mirrored
    // rather than moved: `PENDING` is a one-shot slot with exactly one consumer
    // (the inventory avatar), and a second drain would steal from it.
    crate::remote_skins::publish(
        crate::remote_skins::LOCAL_PROFILE_SKIN_KEY.to_owned(),
        image.clone(),
    );
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

/// The local player's currently-known skin rig, for a consumer that is not
/// the inventory avatar — namely `sim/camera.rs::third_person_body_state`,
/// which needs the same rig every frame rather than a one-shot value that
/// [`take_pending`] has usually already consumed.
///
/// Prefers this session's freshest fetch ([`CURRENT`]), then falls back to
/// the on-disk marker `write_cache`/`container::player_preview::local_skin_override`
/// already share — so a rig fetched in an *earlier* session is honoured
/// before this session's own sign-in (or a signed-out launch) has published
/// anything. [`PlayerModelType::Wide`] is the last resort, matching
/// `by_legacy_services_name`'s own default for an absent marker.
#[must_use]
pub(crate) fn current_model() -> PlayerModelType {
    let cached = match CURRENT.lock() {
        Ok(slot) => *slot,
        Err(poisoned) => *poisoned.into_inner(),
    };
    if let Some(model) = cached {
        return model;
    }
    let dir = lodestone_auth::paths::data_dir();
    let declared = std::fs::read_to_string(dir.join("skin.model")).ok();
    PlayerModelType::by_legacy_services_name(declared.as_deref().map(str::trim))
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

    /// Serialises every test that touches this module's statics.
    ///
    /// [`PENDING`] and [`CURRENT`] are process-wide, libtest runs a binary's
    /// tests on several threads, and each of the tests below asserts on a value
    /// it just published — so without this they race, and the failure looks
    /// like a bug in `publish` rather than in the harness.
    ///
    /// It guards [`PENDING`] and [`CURRENT`] only. [`publish`] also mirrors into
    /// `remote_skins`' ready queue, and that queue is deliberately **not**
    /// covered here: its own gates assert per URL rather than on queue length,
    /// so they tolerate any concurrent publisher instead of requiring every one
    /// of them to have remembered a lock.
    static STATICS: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        match STATICS.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

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
        let _serial = serial();
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

    /// The discriminator for the always-Steve bug (`sim/camera.rs`'s
    /// `third_person_body_state` used to hardcode `slim: false`): a wide
    /// publish and a slim publish must read back as two *different* models
    /// through [`current_model`], not merely "a model was returned". Also
    /// covers `publish`'s own contract that [`CURRENT`] — unlike [`PENDING`]
    /// — is never drained, so a container opening and taking the pending
    /// sheet must not blind a later `current_model()` read.
    #[test]
    fn current_model_distinguishes_wide_from_slim_and_survives_a_pending_drain() {
        let _serial = serial();
        publish(PlayerModelType::Wide, sheet(64, 64));
        assert_eq!(current_model(), PlayerModelType::Wide);
        publish(PlayerModelType::Slim, sheet(64, 64));
        assert_eq!(
            current_model(),
            PlayerModelType::Slim,
            "a slim publish must read back as slim, not the wide default"
        );
        // Draining `PENDING` (what the inventory-avatar draw does every
        // container frame) must not reset `current_model`'s answer.
        let _ = take_pending();
        assert_eq!(
            current_model(),
            PlayerModelType::Slim,
            "current_model must survive a PENDING drain"
        );
    }

    /// The offline/singleplayer rung: our own cached profile sheet has to be
    /// reachable through the **same URL-keyed cache the world draw binds
    /// from**, or the inventory avatar shows the owner's real skin while the
    /// body standing beside it shows a uuid-hash identity — same person, same
    /// frame.
    ///
    /// # What this gate does and does not cover
    ///
    /// It covers the mirroring: `publish` puts the sheet where the world draw
    /// binds from, and `local_profile_sheet_key` finds it there. Both halves
    /// fail under a neuter of either.
    ///
    /// It does **not** cover `request`'s explicit refusal of the key, and an
    /// earlier version of this test claimed to. That assertion was
    /// unfalsifiable: `is_allowed_texture_domain` already refuses a key that
    /// names no host, so `requested_urls` does not move whether the guard is
    /// there or not. The guard's real value is that it short-circuits *before*
    /// the domain check, which would otherwise log "refusing a skin texture url
    /// outside the allowed domain" once per session — a warning describing a
    /// problem that does not exist. That is worth having and is not gated here;
    /// saying so beats an assertion that cannot fail.
    #[test]
    fn the_cached_profile_sheet_is_reachable_by_key_and_never_fetched() {
        let _serial = serial();
        let key = crate::remote_skins::LOCAL_PROFILE_SKIN_KEY;
        // Through `publish`, the production path -- not by calling
        // `remote_skins::publish` directly, which would prove only that the
        // cache works and nothing about this module mirroring into it.
        publish(PlayerModelType::Slim, sheet(64, 64));

        assert_eq!(
            local_profile_sheet_key(),
            Some(key),
            "a published profile skin must be reachable under the shared key, or \
             our own body has nothing to bind and falls to the identity sheet"
        );
        assert!(
            crate::remote_skins::sheet(key).is_some(),
            "the key must resolve in the same cache `player_skins` is filled from"
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
