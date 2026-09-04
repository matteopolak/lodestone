//! The inventory paperdoll and the world's per-player default skin must derive
//! from **one resolver, keyed on the same uuid**, so they cannot disagree.
//!
//! This asserts state, not pixels: [`ContainerRenderer::player_preview_model`]
//! after a real render call, against
//! [`lodestone_assets::skin::default_skin_for_uuid`] — the exact function
//! `entities.rs::default_remote_skin` calls for the world-side default of
//! every other player with no declared skin. Two call sites reading one pure
//! function of the same uuid must choose the same rig. The real render path
//! proves that the paperdoll is one of those call sites.
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
//! The pure, environment-free proof of the uuid resolution itself is
//! `container::player_preview::tests::uuid_default_model_picks_the_uuids_own_answer`
//! and `..._matches_the_shared_resolver_across_a_spread`.
//!
//! Fail-closed otherwise: no GPU adapter, or no vanilla `client.jar` (so
//! `attach_player_preview` returns `false`), is a failure, never a silent
//! skip — this gate exists specifically to prove the resolver runs, so a
//! quiet skip would be exactly the false confidence `CLAUDE.md` warns about.
//!
//! ```text
//! cargo test -p lodestone-shell --test session paperdoll_skin_resolver -- --ignored --nocapture
//! ```

use lodestone::container::{ContainerFrame, ContainerRenderer};
use lodestone_assets::PlayerModelType;
use lodestone_game::menu::Menu;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// Render one frame with `uuid` attached through a **fresh**
/// `ContainerRenderer`. Separate instances isolate the two discriminating
/// renders; the test compares the model each render's UUID lookup binds.
fn resolved_model_for(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    uuid: uuid::Uuid,
) -> PlayerModelType {
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
    // drain, which is where `maybe_skin_for_uuid` actually runs — this is
    // not a struct-field check, it goes through the real draw entry point.
    renderer.render(device, queue, acquired.view(), &frame, W, H);
    acquired.present(queue);

    let model = renderer
        .player_preview_model()
        .expect("player_preview_attached() was true, so a model must be bound");
    model
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

    let got_slim = resolved_model_for(device, queue, slim_uuid);
    let got_wide = resolved_model_for(device, queue, wide_uuid);

    eprintln!("=== paperdoll skin resolver gate ===");
    eprintln!(
        "slim_uuid -> resolver says {expected_slim:?}, paperdoll bound {got_slim:?}"
    );
    eprintln!(
        "wide_uuid -> resolver says {expected_wide:?}, paperdoll bound {got_wide:?}"
    );

    assert_eq!(
        got_slim, expected_slim,
        "the paperdoll must resolve the nil uuid to Slim, matching \
         lodestone_assets::skin::default_skin_for_uuid — a hardcoded \
         PlayerModelType::Wide fails exactly this assertion"
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
