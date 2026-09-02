//! Issue #646's remaining half: the inventory paperdoll and the world's
//! per-player default skin must derive from **one resolver, keyed on the
//! same uuid**, so they cannot disagree — the owner's report was exactly
//! that they *did* disagree ("shows Alex in the inventory... in-game my
//! player renders as Steve").
//!
//! This asserts state, not pixels: [`ContainerRenderer::player_preview_model`]
//! after a real render call, against
//! [`lodestone_assets::skin::default_skin_for_uuid`] — the exact function
//! `entities.rs::default_remote_skin` calls for the world-side default of
//! every other player with no declared skin. Two call sites reading one pure
//! function of the same uuid cannot disagree by construction; this proves
//! the paperdoll is actually one of those two call sites now; before this
//! fix it hardcoded `PlayerModelType::Wide` and ignored the uuid entirely.
//!
//! **Discriminating uuids, not one.** A single uuid could pass by accident if
//! the paperdoll happened to hardcode exactly that uuid's answer — this repo
//! has a rule about exactly that shape of coincidence. Two uuids are used,
//! hand-verified against `lodestone-assets/src/skin.rs`'s own
//! `default_skin_for_uuid_matches_hand_derived_cases`: the nil uuid
//! (`most_sig=0, least_sig=0`) resolves to **Slim** (index 0), and `(9, 0)`
//! resolves to **Wide** (index 9) — the two are on opposite sides of the
//! rig, so a hardcoded constant of *either* rig fails one of the two
//! assertions below, and a resolver that ignores the uuid fails both.
//!
//! **A real local `skin.png`/`skin.model` override on the host machine is a
//! legitimate state, not noise to ignore.** `PlayerPreview` deliberately lets
//! a user's own override outrank a uuid guess (see
//! `container/player_preview.rs`'s `uuid_default_model`), and the override
//! lives in the real, process-global data directory — not something a test
//! can safely fake from inside the process (`std::env::set_var` is `unsafe`
//! under this workspace's edition and races other tests in the same binary;
//! `singleplayer_persistence.rs` documents the same constraint). So this
//! gate reads `player_preview_used_local_override` and asserts the
//! **override-wins** claim instead of the uuid-resolution claim when one is
//! present, printing which branch ran — never a silent skip either way. The
//! pure, environment-free proof of the uuid resolution itself is
//! `container::player_preview::tests::uuid_default_model_picks_the_uuids_own_answer`
//! and `..._matches_the_shared_resolver_across_a_spread`.
//!
//! Fail-closed otherwise: no GPU adapter, or no vanilla `client.jar` (so
//! `attach_player_preview` returns `false`), is a failure, never a silent
//! skip — this gate exists specifically to prove the resolver runs, so a
//! quiet skip would be exactly the false confidence `CLAUDE.md` warns about.
//!
//! ```text
//! cargo test -p lodestone-shell --test paperdoll_skin_resolver -- --ignored --nocapture
//! ```

use lodestone::container::{ContainerFrame, ContainerRenderer};
use lodestone_assets::PlayerModelType;
use lodestone_game::menu::Menu;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// Render one frame with `uuid` attached, through a **fresh**
/// `ContainerRenderer` — fresh because `PlayerPreview::maybe_default_from_uuid`
/// is deliberately idempotent (applies once per instance, never re-derives),
/// so reusing a renderer across the two discriminating uuids below would
/// measure the first uuid's answer twice rather than two independent
/// resolutions. Returns the bound model plus whether a real local override
/// pre-empted the uuid resolution on this machine.
fn resolved_model_for(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    uuid: uuid::Uuid,
) -> (PlayerModelType, bool) {
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut renderer = ContainerRenderer::new(device, format);
    assert!(
        renderer.attach_player_preview(device, queue, format),
        "attach_player_preview returned false: no vanilla client.jar / player skin sheet \
         found — this gate needs the real pack, not a degraded run"
    );

    let menu = Menu::player();
    let frame = ContainerFrame::new(Some(&menu), "Inventory").with_avatar_uuid(Some(uuid));

    let mut target = HeadlessTarget::new(device, W, H, format);
    let acquired = target.acquire().expect("headless acquire");
    // The render call itself is what drives
    // `ContainerRenderer::render_geometry_scaled_between_strata`'s per-frame
    // drain, which is where `maybe_default_from_uuid` actually runs — this is
    // not a struct-field check, it goes through the real draw entry point.
    renderer.render(device, queue, acquired.view(), &frame, W, H);
    acquired.present(queue);

    let model = renderer
        .player_preview_model()
        .expect("player_preview_attached() was true, so a model must be bound");
    let used_override = renderer
        .player_preview_used_local_override()
        .expect("attached above");
    (model, used_override)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_paperdoll_and_the_world_default_resolve_the_same_uuid_to_the_same_model() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();

    // Hand-verified against `default_skin_for_uuid_matches_hand_derived_cases`
    // in `lodestone-assets/src/skin.rs`: nil -> index 0 (Slim/alex).
    let slim_uuid = uuid::Uuid::from_u64_pair(0, 0);
    // `(9, 0)` -> index 9 (Wide/alex) — the other side of the rig.
    let wide_uuid = uuid::Uuid::from_u64_pair(9, 0);

    let (slim_hi, slim_lo) = slim_uuid.as_u64_pair();
    let (wide_hi, wide_lo) = wide_uuid.as_u64_pair();
    let expected_slim =
        lodestone_assets::skin::default_skin_for_uuid(slim_hi as i64, slim_lo as i64).model;
    let expected_wide =
        lodestone_assets::skin::default_skin_for_uuid(wide_hi as i64, wide_lo as i64).model;
    assert_eq!(expected_slim, PlayerModelType::Slim, "test premise: the nil uuid must resolve slim");
    assert_eq!(expected_wide, PlayerModelType::Wide, "test premise: (9, 0) must resolve wide");

    let (got_slim, override_slim) = resolved_model_for(device, queue, slim_uuid);
    let (got_wide, override_wide) = resolved_model_for(device, queue, wide_uuid);

    eprintln!("=== paperdoll skin resolver gate (#646) ===");
    eprintln!(
        "slim_uuid -> resolver says {expected_slim:?}, paperdoll bound {got_slim:?}, \
         local_override={override_slim}"
    );
    eprintln!(
        "wide_uuid -> resolver says {expected_wide:?}, paperdoll bound {got_wide:?}, \
         local_override={override_wide}"
    );

    // `attach_player_preview` reads the *same* real data directory on every
    // call in this process, so whether an override exists cannot differ
    // between the two — if it somehow did, neither branch below would be
    // trustworthy.
    assert_eq!(
        override_slim, override_wide,
        "local-override presence must be the same for both calls in one process — a \
         difference here means something is reading a different data directory per call"
    );

    if override_slim {
        // A real skin.png/skin.model exists on this host (this machine's own
        // data directory, e.g. from actually running the game) — the uuid
        // default correctly never got a chance to apply. The claim this
        // branch can still make: the override wins identically regardless of
        // which uuid was attached, i.e. the uuid genuinely has zero effect
        // once an override exists, not just "happened to match" for one of
        // the two.
        eprintln!(
            "local override present on this host — asserting override-wins instead of \
             uuid-resolution; see this file's module doc for why an env-var workaround is \
             not used here"
        );
        assert_eq!(
            got_slim, got_wide,
            "with a local override bound, the two different uuids must resolve to the \
             identical (override's) model — a difference here means the uuid is leaking \
             through despite the override"
        );
    } else {
        assert_eq!(
            got_slim, expected_slim,
            "the paperdoll must resolve the nil uuid to Slim, matching \
             lodestone_assets::skin::default_skin_for_uuid — a hardcoded \
             PlayerModelType::Wide (the pre-fix constant) fails exactly this \
             assertion"
        );
        assert_eq!(
            got_wide, expected_wide,
            "the paperdoll must resolve (9, 0) to Wide, matching the same \
             resolver — proving the uuid genuinely selects the model rather than \
             both arms coincidentally landing on Slim"
        );
        assert_ne!(
            got_slim, got_wide,
            "the two discriminating uuids must bind two different rigs, or this \
             gate cannot tell a working resolver from a hardcoded constant"
        );
    }
}
