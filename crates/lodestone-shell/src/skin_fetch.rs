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
//! 1. **the disk cache** — `<data_dir>/skin.png`, `skin.model`, and
//!    `skin.uuid`; the UUID marker is mandatory because the first two files are
//!    process-global and otherwise identify whichever account signed in last;
//! 2. **the retained decoded cache** — [`publish`] stores the sheet under an
//!    account-scoped synthetic key in `remote_skins`, which the world, arm and
//!    inventory preview all pull from. A renderer rebuilt after a resource-pack
//!    reload therefore rehydrates instead of consuming a one-shot event.
//!
//! ## How it works
//!
//! ```text
//! menu::accounts worker  (device-code and loopback, both)
//!   -> lodestone_auth::login::finish_interactive          -- Session { profile.skin }
//!   -> fetch_own_skin(&client, &profile)
//!        -> lodestone_auth::texture::fetch_texture        -- TextureUrlChecker, then GET
//!        -> Image::decode_png
//!        -> write skin.png + skin.model + skin.uuid       -- later launches
//!        -> publish(profile.id, model, image)             -- this session
//! Sim::local_player_skin(profile id)
//!   -> account-scoped local key -> remote_skins::set_local(profile id, skin)
//! ContainerRenderer -> PlayerPreview::maybe_skin_for_uuid(profile id)
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
//! `skin.model` on disk is written in the *property* vocabulary. Writing
//! `CLASSIC` there would silently resolve
//! wide-by-fallback and look right for every Steve and wrong for every Alex.
//!
//! **A skin without a matching UUID marker is not a local profile skin.** Old
//! unmarked cache files are deliberately ignored. Guessing their owner is what
//! showed a different account from the switcher in the inventory preview.
//!
//! ## Dependencies
//!
//! * `lodestone-auth` — `texture::fetch_texture` (the allow list and the GET),
//!   `Profile`/`ProfileSkin`/`SkinVariant`, `paths::data_dir`.
//! * `lodestone-assets` — `Image::decode_png`, `PlayerModelType`.
//! * `crate::remote_skins` — retained decoded sheets and the active local handoff.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestone_assets::{Image, PlayerModelType};

/// The last model [`publish`] handed out, keyed by the profile UUID that owns
/// it. The decoded sheet itself is retained by `remote_skins` under the same
/// account-scoped synthetic key.
static CURRENT: Mutex<Option<HashMap<uuid::Uuid, PlayerModelType>>> = Mutex::new(None);

/// Whether the on-disk `<data_dir>/skin.png` has been offered to
/// [`crate::remote_skins`] yet, so the read is attempted **once** rather than
/// on every frame that asks.
///
/// Keyed by UUID rather than a single once flag: changing accounts must perform
/// a distinct ownership check, while a miss for one account should still be
/// remembered rather than rereading the same files every frame.
static CACHED_SHEET_OFFERED: Mutex<Option<HashMap<uuid::Uuid, bool>>> = Mutex::new(None);

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

/// Non-URL key used by the shared decoded-sheet cache for one local profile.
#[must_use]
fn local_profile_key(id: uuid::Uuid) -> String {
    format!("{}{}", crate::remote_skins::LOCAL_PROFILE_SKIN_KEY_PREFIX, id.simple())
}

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
pub(crate) fn local_profile_sheet_key(id: uuid::Uuid) -> Option<String> {
    let key = local_profile_key(id);
    if crate::remote_skins::sheet(&key).is_some() {
        return Some(key);
    }
    let offered = with_map(&CACHED_SHEET_OFFERED, |map| {
        *map.entry(id).or_insert_with(|| read_cached_sheet(id, &key))
    });
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
fn read_cached_sheet(id: uuid::Uuid, key: &str) -> bool {
    {
        let dir = lodestone_auth::paths::data_dir();
        let Ok(owner) = std::fs::read_to_string(dir.join("skin.uuid")) else {
            return false;
        };
        if owner.trim() != id.to_string() {
            return false;
        }
        // `Err(Unsupported)` in a browser rather than a trap; the cache simply
        // degrades to the UUID-derived identity there.
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
                let declared = std::fs::read_to_string(dir.join("skin.model")).ok();
                let model = PlayerModelType::by_legacy_services_name(
                    declared.as_deref().map(str::trim),
                );
                with_map(&CURRENT, |map| {
                    map.insert(id, model);
                });
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
fn read_cached_sheet(_id: uuid::Uuid, _key: &str) -> bool {
    false
}

/// Hand a decoded sheet to the renderer. Replaces any undrained earlier one.
pub(crate) fn publish(id: uuid::Uuid, model: PlayerModelType, image: Image) {
    with_map(&CURRENT, |map| {
        map.insert(id, model);
    });
    // The world body and the first-person arm bind from `remote_skins`'
    // URL-keyed cache — see `local_profile_sheet_key`. This is the fetch half;
    // the disk half is the read it guards. The inventory preview and world
    // renderer both pull from this retained store, so a resource-pack rebuild
    // cannot consume or lose the only copy.
    crate::remote_skins::publish(
        local_profile_key(id),
        image,
    );
}

/// The local player's currently-known skin rig, for a consumer that is not
/// the inventory avatar — namely `sim/camera.rs::third_person_body_state`,
/// which needs the same account-scoped rig every frame rather than a one-shot
/// value. `None` means that account has not published or loaded a real skin;
/// callers then use the UUID-derived default.
#[must_use]
pub(crate) fn current_model(id: uuid::Uuid) -> Option<PlayerModelType> {
    with_map(&CURRENT, |map| map.get(&id).copied())
}

/// Write the fetched sheet and its owner UUID into the launch cache.
///
/// Best-effort: a read-only data directory costs the *cache*, not this session's
/// avatar, because [`publish`] has already happened by then.
/// Native-only: reached only from [`fetch_own_skin`], which is itself native-only.
/// A browser cache would be `localStorage`/IndexedDB, not `std::fs` (whose writes
/// return `Err(Unsupported)` there), and it has nothing to cache until the fetch
/// path above it exists.
#[cfg(not(target_arch = "wasm32"))]
fn write_cache(id: uuid::Uuid, model: PlayerModelType, png: &[u8]) {
    let dir = lodestone_auth::paths::data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(target: "assets", "could not create {}: {e}", dir.display());
        return;
    }
    if let Err(e) = std::fs::write(dir.join("skin.png"), png) {
        tracing::warn!(target: "assets", "could not cache skin.png: {e}");
        return;
    }
    if let Err(e) = std::fs::write(dir.join("skin.uuid"), id.to_string()) {
        tracing::warn!(target: "assets", "could not cache skin.uuid: {e}");
        return;
    }
    // The **legacy services** spelling (`default`/`slim`), because that is what
    // the cache reader parses. See this module's gotchas.
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
/// Native-only — but **not** because either parameter type is: `reqwest::Client`
/// and `lodestone_auth::Profile` both compile and work on wasm32 now (`flow`,
/// the module `Profile` lives in, is no longer native-only — see that
/// module's doc). What stays native-only is the allow-list check this
/// function goes through, [`lodestone_auth::texture::fetch_texture`], which
/// has not been ported. `menu::accounts::finish_ms_token` — this function's
/// one caller, shared by both sign-in flows on both targets — skips this
/// call on wasm32 rather than gating itself, so a browser account still
/// signs in and joins; it just keeps the default skin rig until this is
/// ported.
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
    write_cache(profile.id, model, &png);
    publish(profile.id, model, image);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialises every test that touches this module's statics.
    ///
    /// [`CURRENT`] is process-wide, libtest runs a binary's
    /// tests on several threads, and each of the tests below asserts on a value
    /// it just published — so without this they race, and the failure looks
    /// like a bug in `publish` rather than in the harness.
    ///
    /// It guards [`CURRENT`] only. [`publish`] also mirrors into
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
    /// covers `publish`'s own contract that [`CURRENT`] is retained.
    #[test]
    fn current_model_distinguishes_wide_from_slim_and_remains_retained() {
        let _serial = serial();
        let id = uuid::Uuid::from_u128(2);
        publish(id, PlayerModelType::Wide, sheet(64, 64));
        assert_eq!(current_model(id), Some(PlayerModelType::Wide));
        publish(id, PlayerModelType::Slim, sheet(64, 64));
        assert_eq!(
            current_model(id),
            Some(PlayerModelType::Slim),
            "a slim publish must read back as slim, not the wide default"
        );
        assert_eq!(
            current_model(id),
            Some(PlayerModelType::Slim),
            "current_model must retain the account-scoped answer"
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
        let id = uuid::Uuid::from_u128(3);
        let key = local_profile_key(id);
        // Through `publish`, the production path -- not by calling
        // `remote_skins::publish` directly, which would prove only that the
        // cache works and nothing about this module mirroring into it.
        publish(id, PlayerModelType::Slim, sheet(64, 64));

        assert_eq!(
            local_profile_sheet_key(id),
            Some(key.clone()),
            "a published profile skin must be reachable under the shared key, or \
             our own body has nothing to bind and falls to the identity sheet"
        );
        assert!(
            crate::remote_skins::sheet(&key).is_some(),
            "the key must resolve in the same cache `player_skins` is filled from"
        );

    }

    /// Two accounts in one process must occupy distinct cache identities. This
    /// is the regression for an inventory avatar that showed another account
    /// from the account switcher after a resource-pack rebuild.
    #[test]
    fn local_profile_sheets_are_keyed_by_account_uuid() {
        let _serial = serial();
        let alice = uuid::Uuid::from_u128(0xA11CE);
        let bob = uuid::Uuid::from_u128(0xB0B);
        publish(alice, PlayerModelType::Slim, sheet(64, 64));
        publish(bob, PlayerModelType::Wide, sheet(64, 64));

        let alice_key = local_profile_sheet_key(alice).expect("Alice's sheet");
        let bob_key = local_profile_sheet_key(bob).expect("Bob's sheet");
        assert_ne!(alice_key, bob_key);
        assert_eq!(current_model(alice), Some(PlayerModelType::Slim));
        assert_eq!(current_model(bob), Some(PlayerModelType::Wide));
        assert!(crate::remote_skins::sheet(&alice_key).is_some());
        assert!(crate::remote_skins::sheet(&bob_key).is_some());
    }

    /// `write_cache` must produce a model marker the cache reader understands,
    /// in the property vocabulary. Uses `LODESTONE_DATA_DIR`?
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
                 cache-reader parse"
            );
        }
    }
}
