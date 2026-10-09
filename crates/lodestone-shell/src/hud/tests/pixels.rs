//! GPU gates: what actually reaches pixels.

use super::*;

#[test]
fn one_line_is_stable() {
    let stats = DebugStats {
        position: [0.5, 40.0, -3.5],
        fps: 60.0,
        frame_ms: 16.6,
        ..Default::default()
    };
    let line = stats.one_line();
    assert!(line.contains("fps=60"));
    assert!(line.contains("frame=16.60ms"));
}

/// Pixel-readback proof that server chat *reaches pixels*, not merely that a
/// frame counter ticks. Renders the HUD (chat only — no crosshair, overlay,
/// hotbar or vitals) over a known grey backdrop and inspects the bottom-left
/// chat region. The discriminator is luminance: the translucent backing
/// panel is *darker* than the grey background, the near-white glyphs are
/// *brighter* — so text and panel are counted separately and a blank-but-
/// panelled line cannot masquerade as rendered text.
///
/// Three frames make the assertion two-sided:
/// * no message → the region is untouched background (zero of both);
/// * a whitespace-only line → the panel draws (dark pixels) but no glyphs;
/// * a real line → glyphs add bright pixels the panel-only frame lacks.
#[test]
#[ignore = "requires a GPU adapter"]
fn shared_world_hud_encoder_preserves_pixels_and_submission_counts() {
    use crate::gpu::gpu_timing::{
        PrimaryCommandCounts, primary_encoder, take_primary_command_counts,
    };
    use crate::gpu::{RenderState, ScreenEffects};
    use crate::mesher::{SectionGeometry, SectionKey, mesh_snapshot, snapshot_section};
    use lodestone_render::{Camera, HeadlessTarget, RenderTarget};

    let context = lodestone_render::GpuContext::new_headless_blocking().expect("GPU adapter");
    let (device, queue) = (context.device(), context.queue());
    let (width, height) = (480, 320);
    let world = crate::worldgen::generate(1);
    let feet = crate::worldgen::spawn_feet();
    let camera = Camera {
        position: glam::Vec3::new(feet[0] as f32, feet[1] as f32 + 6.0, feet[2] as f32 - 18.0),
        yaw: 0.0,
        pitch: 15.0,
        fov_y_degrees: 70.0,
        aspect: width as f32 / height as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };
    let stats = DebugStats::default();
    for format in [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Rgba8UnormSrgb] {
        let mut target = HeadlessTarget::new(device, width, height, format);
        let mut render = RenderState::new(device, queue, format, width, height, None);
        for cz in -1..=1 {
            for cx in -1..=1 {
                for si in 0..crate::worldgen::SECTION_COUNT {
                    let key = SectionKey { cx, cz, si, min_y: crate::worldgen::MIN_Y };
                    let Some(snapshot) = snapshot_section(&world, key, Default::default()) else { continue };
                    let mesh = mesh_snapshot(&snapshot, &crate::blocks::DemoClassifier);
                    if !mesh.indices.is_empty() {
                        render.upload_section(device, queue, key, &SectionGeometry::Packed(mesh));
                    }
                }
            }
        }
        let mut draw = |merged: bool, hidden: bool| {
            let frame = target.acquire().expect("headless frame");
            let mut hud = HudRenderer::new(device, target.raw_view_format());
            let raw_view = hud.flat_colour_view(&frame);
            let chat = [("shared frame control", 0.0)];
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: !hidden,
                chat: if hidden { &[] } else { &chat },
                ..HudFrame::new(&stats)
            };
            let _ = take_primary_command_counts();
            let world_stats = if merged {
                let mut encoder = primary_encoder(device, "world-hud-control");
                let result = render.encode_with_crack_and_effects(
                    device, queue, frame.view(), &camera, None, &[], &[],
                    ScreenEffects::default(), &mut encoder,
                );
                hud.encode_with_item_models(
                    device, queue, frame.view(), &raw_view, Some(render.depth_view()),
                    &hud_frame, None, 1, width, height, &mut encoder,
                );
                render.submit_encoded_frame(queue, encoder);
                result
            } else {
                let result = render.render(device, queue, frame.view(), &camera, None, &[]);
                hud.render_with_item_models(
                    device, queue, frame.view(), &raw_view, Some(render.depth_view()),
                    &hud_frame, None, 1, width, height,
                );
                result
            };
            assert!(world_stats.sections_drawn > 0, "fixture must draw terrain");
            let counts = take_primary_command_counts();
            (target.read_texels(device, queue), counts)
        };
        let (separate, separate_counts) = draw(false, false);
        let (shared, shared_counts) = draw(true, false);
        let (empty_separate, empty_separate_counts) = draw(false, true);
        let (empty_shared, empty_shared_counts) = draw(true, true);
        assert_eq!(separate_counts, PrimaryCommandCounts { created: 2, finished: 2, submitted: 2 });
        let one = PrimaryCommandCounts { created: 1, finished: 1, submitted: 1 };
        assert_eq!(shared_counts, one);
        assert_eq!(empty_separate_counts, one);
        assert_eq!(empty_shared_counts, one);
        for (expected, actual) in [(&separate, &shared), (&empty_separate, &empty_shared)] {
            let mut bounds = None::<[usize; 4]>;
            for (index, (left, right)) in expected.chunks_exact(4).zip(actual.chunks_exact(4)).enumerate() {
                if left != right {
                    let (x, y) = (index % width as usize, index / width as usize);
                    bounds = Some(bounds.map_or([x, y, x, y], |b| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)]));
                }
            }
            assert!(bounds.is_none(), "{format:?}: changed pixels at {bounds:?}");
        }
        assert!(shared != empty_shared, "control must detect omitted HUD pixels");
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn chat_text_reaches_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // Count bright (glyph) and dark (panel) pixels in the bottom-left chat
    // region, well clear of the bottom-centre hotbar/vitals (which are off
    // anyway) and the top-left debug overlay.
    let x_max = (w as f32 * 0.55) as u32;
    let y_min = (h as f32 * 0.60) as u32;

    let mut render = |chat: &[(&str, f32)]| -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            chat,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let (mut bright, mut dark) = (0usize, 0usize);
        for y in y_min..h {
            for x in 0..x_max {
                let i = ((y * w + x) * 4) as usize;
                let avg = (u32::from(pixels[i])
                    + u32::from(pixels[i + 1])
                    + u32::from(pixels[i + 2]))
                    / 3;
                if avg > u32::from(BG) + 30 {
                    bright += 1;
                } else if avg + 30 < u32::from(BG) {
                    dark += 1;
                }
            }
        }
        (bright, dark)
    };

    let (blank_bright, blank_dark) = render(&[]);
    let (panel_bright, panel_dark) = render(&[(" ", 0.0)]);
    let (text_bright, text_dark) = render(&[("chat works", 0.0)]);

    eprintln!("=== chat readback (headless) ===");
    eprintln!("blank  bright={blank_bright} dark={blank_dark}");
    eprintln!("panel  bright={panel_bright} dark={panel_dark}");
    eprintln!("text   bright={text_bright} dark={text_dark}");

    // No message: pure background — neither panel nor glyphs.
    assert_eq!(
        (blank_bright, blank_dark),
        (0, 0),
        "with no chat, the chat region must be untouched background"
    );
    // A line draws its translucent backing panel (dark) but a space has no
    // glyphs, so almost no bright pixels.
    assert!(panel_dark > 0, "a chat line must draw its backing panel");
    assert!(
        panel_bright < 50,
        "a whitespace-only line must not paint glyph pixels, got {panel_bright}"
    );
    // The glyphs of a real line add bright pixels the panel-only frame lacks
    // — this is the assertion that fails if the text were blank.
    assert!(
        text_dark > 0,
        "the text line must also draw its backing panel"
    );
    assert!(
        text_bright > panel_bright + 150,
        "chat glyphs must reach pixels over the bare panel: text_bright={text_bright}, \
         panel_bright={panel_bright}"
    );
}

/// The **wiring** half of the tab-list gamma fix, at production's own
/// surface format.
///
/// `gpu::pixel_gates`' `hud_flat_colour_blend_matches_vanilla_gamma_on_a_raw_target`
/// already measured that `hud.wgsl` reproduces vanilla's raw-gamma blend
/// when it is given a non-sRGB attachment — but it builds the pipeline by
/// hand, so it proves the *shader*, not that `HudRenderer` and its callers
/// pair a pipeline with a matching view. That pairing is what was actually
/// broken (and what a previous attempt got wrong in the other direction),
/// and it lives in two files, so it needs its own subject.
///
/// # The fixture, and why it is predictable to the byte
///
/// A **black** backdrop and rows whose names are empty spans. The overlay
/// then paints exactly two things: `TAB_PLATE` (black at alpha 128), which
/// over black is a fixed point of both blend models and contributes
/// nothing, and `TAB_ROW_FILL` (white at alpha 32). With no GUI atlas
/// attached the ping sprites draw nothing and with empty names no glyph
/// does either, so **every non-zero byte in the frame is the row fill** and
/// the frame maximum is that one composite — no rect arithmetic, and a zero
/// maximum is a failed premise rather than a silent pass.
///
/// Raw-byte alpha compositing is plain interpolation, so the subject is
/// predicted exactly: `0 * (1 - 32/255) + 255 * (32/255)` = **32**. The
/// control is only *bracketed* — this codebase has measured real sRGB
/// `ALPHA_BLENDING` as a non-trivial function of the fragment alpha that
/// resists a closed form on Metal — but it must land far away, and the
/// recorded sweep puts it near 99.
///
/// # The format
///
/// `Bgra8UnormSrgb`, which is what native `wgpu-core`'s
/// `Surface::get_default_config` actually picks — so the two formats this
/// gate pairs are the pair production pairs, not a headless-only
/// `Rgba8Unorm` where `format()` and `raw_view_format()` coincide and the
/// whole question is vacuous. That non-coincidence is asserted rather than
/// assumed.
///
/// **What this does not prove.** The target is a `HeadlessTarget`, not a
/// swapchain. That `SurfaceTarget` reports the same format pair, and
/// declares both in `view_formats`, is `lodestone_render::target`'s claim,
/// not this gate's.
#[test]
#[ignore = "requires a GPU adapter"]
fn the_flat_colour_pass_blends_on_gamma_bytes_at_the_surface_format() {
    use crate::tablist::{TabListRow, TabListView};
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Native's own swapchain format, so `format()` and `raw_view_format()`
    // genuinely differ and the comparison below has something to compare.
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    assert_ne!(
        target.format(),
        target.raw_view_format(),
        "this gate is vacuous unless the corrected and raw formats differ — pick a \
         format whose sRGB and non-sRGB siblings are distinct"
    );

    let stats = DebugStats::default();
    // Two rows, no names, no banner: the row fill and nothing else. `spectator`
    // only chooses an ink colour, and there is no ink to colour.
    let view = TabListView {
        rows: vec![
            TabListRow { name: Vec::new(), ping_sprite: "", spectator: false },
            TabListRow { name: Vec::new(), ping_sprite: "", spectator: false },
        ],
        header: Vec::new(),
        footer: Vec::new(),
    };

    // `wiring` picks which `(pipeline format, attachment view)` pair the
    // flat-colour pass gets. `Correct` is production's; `Corrected` is the
    // pairing production had before this fix, kept as the control that must
    // land on the other hypothesis rather than merely "somewhere else".
    let mut shoot = |raw: bool| -> (u8, usize) {
        let flat_format = if raw {
            target.raw_view_format()
        } else {
            target.format()
        };
        let mut hud = HudRenderer::new(device, flat_format);
        assert_eq!(hud.flat_colour_format(), flat_format);
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [0, 0, 0]);
        let attachment = if raw {
            hud.flat_colour_view(&frame)
        } else {
            frame.create_view(target.format())
        };
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            players: Some(&view),
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), &attachment, &hud_frame, w, h);
        drop(frame);
        let pixels = target.read_texels(device, queue);
        let mut max = 0u8;
        let mut lit = 0usize;
        for px in pixels.chunks_exact(4) {
            // Channel order is BGRA here; the fill is white over black, so
            // every colour channel carries the same value and the max is
            // order-independent.
            let v = px[0].max(px[1]).max(px[2]);
            max = max.max(v);
            if v > 0 {
                lit += 1;
            }
        }
        (max, lit)
    };

    let (raw_max, raw_lit) = shoot(true);
    let (srgb_max, srgb_lit) = shoot(false);

    // `0x20FFFFFF` over black, composited on raw gamma bytes — the whole
    // claim, derived from the constants rather than restated.
    let alpha = f32::from(0x20u8) / 255.0;
    let predicted = (255.0 * alpha).round() as i32;

    eprintln!("=== hud flat-colour wiring at Bgra8UnormSrgb ===");
    eprintln!("predicted vanilla gamma byte = {predicted}");
    eprintln!("raw-view  max = {raw_max}  lit = {raw_lit}");
    eprintln!("srgb-view max = {srgb_max}  lit = {srgb_lit}");

    // Premise: the overlay drew at all. A zero here means the fixture
    // produced no row fill and both arms below would agree vacuously.
    assert!(
        raw_lit > 0 && srgb_lit > 0,
        "the tab overlay must paint its row fill in both arms, or neither arm is \
         measuring a blend: raw_lit={raw_lit}, srgb_lit={srgb_lit}"
    );

    // 1) Production's pairing reproduces vanilla's own blend to the byte.
    assert!(
        (i32::from(raw_max) - predicted).abs() <= 2,
        "the raw-view pairing must reproduce vanilla's raw-gamma blend of TAB_ROW_FILL \
         over black: predicted {predicted}, got {raw_max}"
    );
    // 2) And the pairing this replaced must be far away — otherwise arm 1
    // would pass for a pipeline indifferent to its attachment's format, and
    // the owner-reported "too light" would have had no cause.
    assert!(
        i32::from(srgb_max) - predicted > 40,
        "the corrected-view pairing must still come out markedly lighter than vanilla \
         (this is the bug being fixed, reproduced live): predicted {predicted}, got \
         {srgb_max}"
    );
}

/// Pixel-readback proof that the **XP bar** reaches pixels once the server
/// has sent experience — the same "prove it's on screen, with a control"
/// discipline as the chat gate. The discriminator is *green dominance*: the
/// vanilla XP fill (and the level digits) are green (`G` well above `R`/`B`),
/// which the grey background, grey hotbar wells, red health and gold food
/// pips all fail, so a green-dominant pixel can only be the XP bar.
///
/// Two frames make it two-sided:
/// * `xp = None` (no server experience) → zero green pixels, no bar;
/// * `xp = Some((level, progress))` → a run of green fill + digit pixels.
///
/// The control is the load-bearing half: it fails if the bar ever draws
/// without server-sent experience (the §12.24 "plausible gauge" trap).
#[test]
#[ignore = "requires a GPU adapter"]
fn xp_bar_reaches_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // The XP bar and level digits live at the bottom-centre; scan a generous
    // bottom band there. Hotbar/vitals/crosshair are all off so nothing else
    // paints here.
    let x0 = (w as f32 * 0.20) as u32;
    let x1 = (w as f32 * 0.80) as u32;
    let y0 = (h as f32 * 0.78) as u32;

    let mut render = |xp: Option<(i32, f32)>| -> usize {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            xp,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let mut green = 0usize;
        for y in y0..h {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (
                    u32::from(pixels[i]),
                    u32::from(pixels[i + 1]),
                    u32::from(pixels[i + 2]),
                );
                // Green-dominant: clearly more green than red or blue, and
                // brighter than the grey background so unblended greys and
                // the gold food pips (high red) are excluded.
                if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                    green += 1;
                }
            }
        }
        green
    };

    let no_xp = render(None);
    let with_xp = render(Some((5, 0.5)));

    eprintln!("=== xp bar readback (headless) ===");
    eprintln!("no_xp green={no_xp}");
    eprintln!("with_xp green={with_xp}");

    // Control: off a live server (no experience) the bar must not draw.
    assert_eq!(
        no_xp, 0,
        "without server experience the XP bar must not draw a single green pixel"
    );
    // A half-full level-5 bar paints a wide green fill plus the green level
    // digit — hundreds of pixels. This fails if the bar were blank.
    assert!(
        with_xp > 150,
        "the XP bar's green fill must reach pixels once experience arrives, got {with_xp}"
    );
}

/// GPU gate for the player report this fix addresses: "the boss bar ...
/// is just a solid rectangle and doesn't use the texture pack for it at
/// all." Runs through the **real vanilla `client.jar`** atlas
/// (`GuiAtlas::build`), not a synthetic one, because the bug's own
/// symptom — a flat fill instead of `BossHealthOverlay`'s real
/// per-colour sprite art — can only be told apart from a correct draw by
/// looking at the *actual shipped pixels*, which
/// [`boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`]
/// (synthetic solid-colour sprites, no GPU) structurally cannot see.
///
/// Deliberately colour-agnostic per CLAUDE.md's "you cannot predict an
/// exact composited byte through `ALPHA_BLENDING` on this backend":
/// every threshold below is **measured from this gate's own renders**
/// (a background-only frame vs a full-fill frame), not a hand-picked RGB
/// value, and every assertion is a magnitude/direction claim with
/// tolerance, never an exact byte.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn boss_bar_paints_real_sprite_art_not_a_flat_rectangle() {
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};
    use lodestone_render::{HeadlessTarget, RenderTarget};

    use crate::overlay::{BossBarView, lerp_discrete_width, plain_spans};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // Same (480, 320) the XP gates above use, for the same reason: it is
    // where `calculate_gui_scale(AUTO, w, h) == 1`, so the logical canvas
    // this module lays `BOSS_BAR_WIDTH`/`BOSS_BAR_TOP` into is the
    // physical target 1:1 and the pixel math below needs no scale term.
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    let mut hud = HudRenderer::new(device, format);
    hud.attach_gui(device, queue, format, atlas);

    const BG: u8 = 128;
    let mut render = |progress: Option<f32>| -> Vec<u8> {
        let bars = [BossBarView {
            title: plain_spans(""),
            progress: progress.unwrap_or(0.0),
            color: BossBarColor::Purple,
            overlay: BossBarOverlay::Progress,
        }];
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            hotbar: None,
            health: None,
            food: None,
            xp: None,
            boss_bars: if progress.is_some() { &bars } else { &[] },
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        target.read_texels(device, queue)
    };

    let bar_x = (w as f32 * 0.5 - BOSS_BAR_WIDTH * 0.5).round() as u32;
    let yo = BOSS_BAR_TOP as u32;
    // Row 2 of the 5-row bar: constant along X in the raw sprite (a
    // horizontal bevel varies by row, not by column — measured directly
    // off `.cache/mc/26.2/client-src`'s `purple_progress.png`), so it is
    // the row to use for the fill/background boundary scan below.
    let mid_row = yo + 2;
    let x_probe = bar_x + 90; // interior column, well clear of the sprite's rounded corners

    let sample = |pixels: &[u8], x: u32, y: u32| -> (i32, i32, i32) {
        let i = ((y * w + x) * 4) as usize;
        (i32::from(pixels[i]), i32::from(pixels[i + 1]), i32::from(pixels[i + 2]))
    };
    let painted = |pixels: &[u8], x: u32, y: u32| -> bool {
        let (r, g, b) = sample(pixels, x, y);
        (r - i32::from(BG)).abs() + (g - i32::from(BG)).abs() + (b - i32::from(BG)).abs() > 30
    };

    let none = render(None);
    let bg_only = render(Some(0.0));
    let full = render(Some(1.0));
    let half = render(Some(0.5));

    // -- negative control: no active boss bar paints nothing at the rect.
    let mut wrong = Vec::new();
    for dy in 0..5 {
        if painted(&none, x_probe, yo + dy) {
            wrong.push(format!(
                "row {dy}: with no boss bar the rect must stay background, \
                 got {:?}",
                sample(&none, x_probe, yo + dy)
            ));
        }
    }

    // -- the bug this fixes: a flat rect is one solid colour top to
    // bottom; vanilla's real sprite has a highlight/shadow bevel across
    // its 5 rows. This is the assertion that falls straight out under
    // the pre-fix `rect_px` draw and is the direct pixel-level check of
    // the player's own report.
    let rows: Vec<(i32, i32, i32)> = (0..5).map(|dy| sample(&bg_only, x_probe, yo + dy)).collect();
    let all_identical = rows.windows(2).all(|w| w[0] == w[1]);
    if all_identical {
        wrong.push(format!(
            "the boss bar's 5 rows are all one solid colour — this is the \
             reported bug (a flat rectangle, no sprite art): rows={rows:?}"
        ));
    }

    // -- clause 3 reaches real pixels, and its width is *measured*, not
    // guessed: the fill must be visibly brighter (blue channel) than the
    // bare background at the same column, or nothing below is
    // meaningful.
    let bg_b = sample(&bg_only, x_probe, mid_row).2;
    let full_b = sample(&full, x_probe, mid_row).2;
    if full_b <= bg_b + 20 {
        wrong.push(format!(
            "the progress fill must be visibly brighter than the bare background \
             at a filled column (blue channel): background={bg_b}, full={full_b}"
        ));
    }

    // -- the half-full bar's fill edge lands at the *predicted* partial
    // column, not at the background's own full-182px edge and not at
    // zero — the threshold is this gate's own measured midpoint between
    // background and full fill, not a hand-picked byte value.
    let threshold = (bg_b + full_b) / 2;
    let mut half_edge = None;
    for dx in 0..BOSS_BAR_WIDTH as u32 {
        if sample(&half, bar_x + dx, mid_row).2 > threshold {
            half_edge = Some(dx);
        }
    }
    let predicted_edge = lerp_discrete_width(0.5, BOSS_BAR_WIDTH as i32) as u32;
    match half_edge {
        Some(edge) => {
            let diff = (edge as i32 - predicted_edge as i32).abs();
            if diff > 4 {
                wrong.push(format!(
                    "half-full bar's fill edge should land near the predicted \
                     {predicted_edge}px column, got {edge}px (diff {diff})"
                ));
            }
        }
        None => wrong.push("a half-full bar must still show some fill".to_string()),
    }

    eprintln!("=== boss bar sprite-art gate (headless) ===");
    eprintln!("bg_only rows @ x={x_probe}: {rows:?}");
    eprintln!("bg_b={bg_b} full_b={full_b} threshold={threshold}");
    eprintln!("half_edge={half_edge:?} predicted_edge={predicted_edge}");

    assert!(wrong.is_empty(), "{wrong:?}");
}

/// GPU gate for a live player report: "the xp bar number is too big and too
/// high." Both halves of that sentence are magnitude claims, not sign
/// claims, so this predicts vanilla's real numbers and requires the
/// measurement to land on them — the CLAUDE.md "magnitude species" repair,
/// not a "some digit painted somewhere" check.
///
/// Runs through the **real vanilla atlas + font** (`HudRenderer::attach_gui`,
/// `VanillaFont::shared` via `HudRenderer::new`), because
/// [`xp_bar_reaches_pixels`] above only exercises the jar-less procedural
/// fallback and would not have caught this: the player was looking at
/// `sprite_vitals`, a different code path with its own (until now,
/// independently wrong) scale and offset.
///
/// Two independent renders isolate each claim instead of restating the
/// source's own constants as the expected value:
///
/// * **"too high"**: render the fill alone (`level: 0, progress: 1.0` — no
///   digit, since the digit only draws `if level > 0`) to find the bar's own
///   top row from its pixels, then render the digit alone (`level: 5,
///   progress: 0.0` — no fill, since the fill only draws `if p > 0.0`) to
///   find the digit's top row. The **gap** between them is what
///   `ContextualBar.extractExperienceLevel` vs `ContextualBar.top`
///   fixes at vanilla's `6` logical px —
///   independent of wherever the cluster's own bottom margin happens to
///   place the bar, so this cannot pass by coincidentally agreeing with our
///   own `by`.
/// * **"too big"**: the digit-alone render's ink bounding box width, against
///   the *real jar font's* advance for `"5"` at scale 1 (correct hypothesis)
///   and at scale 2 (the old bug's hypothesis, exactly double) — both
///   computed from [`VanillaFont::from_manager`], outside the code under
///   test.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn xp_level_number_is_the_right_size_and_the_right_distance_above_the_bar() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));
    let font = VanillaFont::from_manager(&manager).expect("build the vanilla font");

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // Chosen for `calculate_gui_scale(AUTO, 480, 320) == 1` (see
    // `hud_vitals_draw_the_real_heart_sprite`'s comment), so the logical
    // canvas `sprite_vitals` lays out into is the physical target 1:1 and no
    // scale multiplication enters the pixel math below.
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    let mut hud = HudRenderer::new(device, format);
    hud.attach_gui(device, queue, format, atlas);
    assert!(
        hud.font_attached(),
        "this gate measures vanilla font metrics; the fixed-advance fallback \
         would make every width prediction below meaningless"
    );

    const BG: u8 = 40;
    let x0 = (w as f32 * 0.20) as u32;
    let x1 = (w as f32 * 0.80) as u32;
    let y0 = (h as f32 * 0.50) as u32;

    // Bounding box of green-dominant pixels in the scan band, or `None` if
    // nothing painted there.
    let mut render_bbox = |xp: Option<(i32, f32)>| -> Option<(u32, u32, u32, u32)> {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            xp,
            hotbar: None,
            health: None,
            food: None,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (u32::MAX, 0u32, u32::MAX, 0u32);
        let mut found = false;
        for y in y0..h {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (
                    u32::from(pixels[i]),
                    u32::from(pixels[i + 1]),
                    u32::from(pixels[i + 2]),
                );
                if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                    found = true;
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        found.then_some((min_x, max_x, min_y, max_y))
    };

    // **The order of these two renders is load-bearing, and it was wrong.**
    //
    // This gate used to render the bar (`level: 0`) first and the digit
    // (`level: 5`) second, and it was red for a reason nothing in it could
    // reveal: `XpFlash::tick` sees `0 → 5` across those two frames as a
    // **level-up**, and a flash at full strength runs the digit's green
    // through `flash_toward_white(…, 1.0)`, i.e. paints it pure white. The
    // digit reached pixels perfectly — 947 painted texels against the bar's
    // 906, its own bounding box six rows higher — and not one of them was
    // green-dominant, so the `expect` below fired.
    //
    // A *world*-species failure in CLAUDE.md's table: the flash landed after
    // this gate, and the gate's premise had been "no such subsystem exists".
    // Reading the test could not show it, because the flaw was in the input.
    //
    // Rendering the digit **first** fixes it without weakening anything:
    // `XpFlash` only triggers when it is already `primed` by a previous
    // frame, so the first render of a fresh `HudRenderer` never flashes, and
    // the following `5 → 0` is a decrease, which never flashes either.
    // Digit alone: no fill (`progress: 0.0`), a single glyph (`level: 5`).
    let digit =
        render_bbox(Some((5, 0.0))).expect("the level digit must paint green pixels");
    // Fill alone: no digit (`level: 0`), full bar (`progress: 1.0`).
    let bar = render_bbox(Some((0, 1.0))).expect("a full XP bar must paint green pixels");
    // Negative control: neither renders without server experience.
    let none = render_bbox(None);

    let (bar_x0, bar_x1, bar_y0, bar_y1) = bar;
    let (digit_x0, digit_x1, digit_y0, digit_y1) = digit;
    let digit_width = digit_x1 - digit_x0 + 1;
    let gap = bar_y0 as i32 - digit_y0 as i32;

    let w1 = font.width("5", 1.0);
    let w2 = font.width("5", 2.0);

    eprintln!("=== xp level-number magnitude gate ===");
    eprintln!("bar bbox    = x[{bar_x0}..{bar_x1}] y[{bar_y0}..{bar_y1}]");
    eprintln!("digit bbox  = x[{digit_x0}..{digit_x1}] y[{digit_y0}..{digit_y1}]");
    eprintln!("digit_width = {digit_width}, gap(bar_top - digit_top) = {gap}");
    eprintln!("real font width('5'): scale1={w1:.1} scale2={w2:.1}");

    assert!(
        none.is_none(),
        "without server experience neither the bar nor the digit may paint, got {none:?}"
    );

    // "too high": vanilla's real gap is exactly 6 logical px
    // (vanilla's own contextual-bar rendering bar top, `:34-40` text y). The old bug's
    // `line_h` was `(GLYPH_H + 2) * 2 == 18`, three times too far — a wide
    // enough margin that a few px of font-glyph internal padding cannot
    // produce a false pass.
    assert!(
        (4..=10).contains(&gap),
        "the level digit must sit ~6 logical px above the bar's top row \
         (vanilla `ContextualBar`), got a gap of {gap} — bar_top={bar_y0} digit_top={digit_y0}"
    );

    // "too big": the digit's ink must match the real font's scale-1 advance,
    // not scale-2's (which is exactly double).
    assert!(
        (digit_width as f32) < w2 - 1.0,
        "the level digit is as wide as scale 2 predicts ({w2:.1}px) — the old \
         `let scale = 2.0;` bug is back, got digit_width={digit_width}"
    );
    assert!(
        (digit_width as f32) <= w1 + 2.0,
        "the level digit is wider than scale 1's real font advance ({w1:.1}px) \
         allows, got digit_width={digit_width}"
    );
}

/// GPU gate: the **title/subtitle** overlay and the **action bar** must reach
/// pixels once a server sends them, and must paint **nothing** when empty.
///
/// This is the "show me pixels, with a control" shape, applied to the text
/// path (the strongest control per the director's template): an empty overlay
/// and a populated one must give measurably different coverage inside the
/// widget's own rect, or the text path has proven nothing.
///
/// Two independent bands are scanned — the title's mid-screen rect and the
/// action bar's lower-centre rect — and each state paints only its own band.
/// That isolation is a second control: a blanket-fill or wrong-clear bug would
/// light the *other* band and fail. Everything else (hotbar, vitals,
/// crosshair, debug) is off so nothing else paints in either band.
#[test]
#[ignore = "requires a GPU adapter; run with --ignored"]
fn title_and_action_bar_reach_pixels() {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU (or a software adapter such as \
         LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
         would assert nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    const BG: u8 = 128;
    // Title band: mid-screen, centred (title draws at y≈0.40h, tall). Action
    // band: lower-centre, above the (absent) hotbar/vitals. x kept central so
    // the bottom-left chat feed never intrudes.
    let xa = (w as f32 * 0.15) as u32;
    let xb = (w as f32 * 0.85) as u32;
    let title_y0 = (h as f32 * 0.30) as u32;
    let title_y1 = (h as f32 * 0.64) as u32;
    let act_y0 = (h as f32 * 0.78) as u32;
    let act_y1 = (h as f32 * 0.96) as u32;

    // Count near-white text texels (white glyphs on the grey clear) in a band.
    let bright_in = |pixels: &[u8], y0: u32, y1: u32| -> usize {
        let mut n = 0usize;
        for y in y0..y1 {
            for x in xa..xb {
                let i = ((y * w + x) * 4) as usize;
                let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
                if r > BG + 40 && g > BG + 40 && b > BG + 40 {
                    n += 1;
                }
            }
        }
        n
    };

    let mut render = |title: Option<(Vec<TextSpan>, Option<Vec<TextSpan>>, f32)>,
                      action_bar: Option<(Vec<TextSpan>, f32)>|
     -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), [BG, BG, BG]);
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            title,
            action_bar,
            ..HudFrame::new(&stats)
        };
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        (
            bright_in(&pixels, title_y0, title_y1),
            bright_in(&pixels, act_y0, act_y1),
        )
    };

    let (empty_title, empty_act) = render(None, None);
    let (shown_title, title_leak_act) = render(
        Some((
            crate::overlay::plain_spans("TITLE"),
            Some(crate::overlay::plain_spans("subtitle")),
            1.0,
        )),
        None,
    );
    let (act_leak_title, shown_act) =
        render(None, Some((crate::overlay::plain_spans("Action bar!"), 1.0)));

    eprintln!("=== title/action-bar readback (headless) ===");
    eprintln!("empty:  title_band={empty_title} act_band={empty_act}");
    eprintln!("title:  title_band={shown_title} act_band={title_leak_act}");
    eprintln!("action: title_band={act_leak_title} act_band={shown_act}");

    // Controls: with no server title/action-bar, neither band paints a pixel.
    assert_eq!(
        (empty_title, empty_act),
        (0, 0),
        "an empty HUD must not paint the title or action-bar rects"
    );
    // The title's large glyphs + subtitle cover hundreds of texels.
    assert!(
        shown_title > 100,
        "a server-sent title must reach pixels in its rect, got {shown_title}"
    );
    // The action-bar line is smaller but still tens of texels of white text.
    assert!(
        shown_act > 40,
        "a server-sent action bar must reach pixels in its rect, got {shown_act}"
    );
    // Isolation control: each widget paints only its own band. A blanket-fill
    // or wrong-clear bug would light the other band and trip these.
    assert_eq!(
        title_leak_act, 0,
        "the title overlay must not bleed into the action-bar rect"
    );
    assert_eq!(
        act_leak_title, 0,
        "the action bar must not bleed into the title rect"
    );
}

/// **The closing gate for the HUD-textures island**: proves the survival
/// vitals draw from the *actual vanilla heart sprite in `client.jar`*, not
/// the procedural fallback, by comparing rendered pixels texel-for-texel
/// against the jar art — then EXECUTES the negative control (no atlas
/// attached) and confirms the same assertion *fails*. A gate never watched
/// fail proves nothing; "it draws" is not a gate.
///
/// sRGB note: the atlas uploads as `Rgba8UnormSrgb` and we render into an
/// `Rgba8UnormSrgb` target, so the sample→tint→store roundtrip re-encodes
/// back to ~the source bytes. We compare only *opaque* source texels — the
/// heart's transparent corners show the backdrop and carry no identity — and
/// at an integer 2× scale each texel maps to a clean 2×2 Nearest block.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn hud_vitals_draw_the_real_heart_sprite() {
    use lodestone_assets::Image;
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let manager = crate::resources::vanilla_manager().expect(
        "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
         LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    );
    let atlas =
        Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

    // The source art we must reproduce on screen.
    let heart_png = manager
        .read("assets/minecraft/textures/gui/sprites/hud/heart/full.png")
        .expect("client.jar must carry hud/heart/full.png");
    let heart = Image::decode_png(&heart_png).expect("decode hud/heart/full.png");
    assert_eq!(
        (heart.width, heart.height),
        (9, 9),
        "the heart sprite is 9x9 native"
    );

    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // sRGB target so the sampler's linear decode is re-encoded on store,
    // letting opaque texels land near the source PNG bytes.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (w, h) = (480u32, 320u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let stats = DebugStats::default();

    // A backdrop that is neither red (heart) nor grey, so an opaque heart
    // texel can never be mistaken for the background.
    const BG: [u8; 3] = [24, 96, 176];

    // Only health on: no hotbar, XP or hunger, so the hearts sit at a
    // location we can compute exactly. `(w, h) = (480, 320)` is chosen
    // specifically so `calculate_gui_scale(AUTO, 480, 320) == 1` — below
    // vanilla's 320-logical-pixel-wide floor at any scale above 1 — so the
    // logical canvas `HudGeometry::build_inner` lays `sprite_vitals` into
    // is identical to this physical target and no scale multiplication
    // enters the picture here at all. `sprite_vitals` draws hearts at their
    // native 9×9 size (no more hardcoded ×2 — see its own doc comment), the
    // first at `xLeft == guiWidth/2 - 91` on vanilla's own vitals-cluster baseline.
    //
    // **`y0` was the hardcoded `h - 19`, and that was correct for the wrong
    // reason.** The hearts used to be stacked upward from a `cluster_top`
    // that moved with the hotbar and the XP bar, and this fixture supplies
    // neither, so `h - 6 - 9 - 4` happened to be `h - 19`. Vanilla's
    // `yLineBase` is `guiHeight - 39` and takes no such branch, so correcting
    // the draw moved the row 20 px and this gate failed — correctly. Derived
    // through [`vitals_line_base`] now, the same call the draw makes.
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        health: Some(20.0),
        food: None,
        xp: None,
        hotbar: None,
        ..HudFrame::new(&stats)
    };
    let s = 1u32;
    let cx = w / 2;
    let x0 = cx - 91;
    let y0 = vitals_line_base(h as f32) as u32;

    // Render one frame with `hud`, read it back, and score how many *opaque*
    // heart texels match the jar sprite within tolerance after the 2× Nearest
    // downsample.
    let mut score = |hud: &mut HudRenderer, tag: &str| -> (usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        clear_view(device, queue, frame.view(), BG);
        hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
        let pixels = target.read_texels(device, queue);
        const TOL: i32 = 24;
        let (mut opaque, mut matched) = (0usize, 0usize);
        for ty in 0..9u32 {
            for tx in 0..9u32 {
                let si = ((ty * 9 + tx) * 4) as usize;
                if heart.rgba[si + 3] < 250 {
                    continue; // transparent corner — no identity
                }
                opaque += 1;
                let px = x0 + tx * s + s / 2;
                let py = y0 + ty * s + s / 2;
                let di = ((py * w + px) * 4) as usize;
                let dr = i32::from(pixels[di]) - i32::from(heart.rgba[si]);
                let dg = i32::from(pixels[di + 1]) - i32::from(heart.rgba[si + 1]);
                let db = i32::from(pixels[di + 2]) - i32::from(heart.rgba[si + 2]);
                if dr.abs() <= TOL && dg.abs() <= TOL && db.abs() <= TOL {
                    matched += 1;
                }
            }
        }
        eprintln!("{tag}: matched {matched}/{opaque} opaque heart texels");
        (matched, opaque)
    };

    // Positive: atlas attached → real heart sprite → high match.
    let mut lit = HudRenderer::new(device, format);
    lit.attach_gui(device, queue, format, Arc::clone(&atlas));
    let (pos_matched, opaque) = score(&mut lit, "vanilla-atlas");
    assert!(
        opaque > 20,
        "the heart sprite must have a solid opaque body, got {opaque}"
    );

    // Negative control, EXECUTED: no atlas → procedural fallback → the same
    // region does NOT reproduce the jar heart, so the match collapses.
    let mut dark = HudRenderer::new(device, format);
    let (neg_matched, _) = score(&mut dark, "procedural-fallback (negative control)");

    let pos_frac = pos_matched as f32 / opaque as f32;
    let neg_frac = neg_matched as f32 / opaque as f32;
    eprintln!("=== heart-sprite gate: vanilla={pos_frac:.2} fallback={neg_frac:.2} ===");

    // Load-bearing: vanilla pixels match the jar; the fallback fails the very
    // same check; and the delta is wide enough that no coincidence passes
    // both.
    assert!(
        pos_frac > 0.80,
        "with the vanilla atlas the rendered hearts must reproduce hud/heart/full.png, \
         got {pos_matched}/{opaque}"
    );
    assert!(
        neg_frac < 0.40,
        "negative control failed to fail: the procedural fallback reproduced the jar \
         heart sprite ({neg_matched}/{opaque}) — the gate would be vacuous"
    );
    assert!(
        pos_frac - neg_frac > 0.40,
        "vanilla vs fallback delta too small to prove the atlas is what reaches pixels: \
         vanilla={pos_frac:.2} fallback={neg_frac:.2}"
    );
}

/// The scoreboard sidebar's two background plates, predicted from
/// `Hud.displayScoreboardSidebar` (`.cache/mc/26.2/client-src`) rather than
/// eyeballed — the *magnitude* species this repo warns against otherwise.
/// Content is chosen so the 1x/2x hypotheses diverge everywhere (title
/// 30px vs 60px; row widths 54/36 vs 108/72, never coinciding after a
/// clamp) and so the two rows' label/score lengths are pairwise-distinct,
/// which a transposed measurement could not survive.
#[test]
fn sidebar_panel_lands_on_vanillas_own_geometry_not_a_2x_pitch() {
    let plain = |s: &str| {
        vec![TextSpan {
            text: s.to_string(),
            style: lodestone_model::text::TextStyle::default(),
        }]
    };
    let line = |label: &str, score: &str| crate::overlay::SidebarLine {
        label: plain(label),
        score: plain(score),
    };
    let sidebar = Sidebar {
        title: plain("Kills"),
        lines: vec![line("Alice", "11"), line("Bob", "7")],
    };
    let stats = DebugStats::default();
    let frame = HudFrame {
        show_debug: false,
        crosshair: false,
        sidebar: Some(&sidebar),
        ..HudFrame::new(&stats)
    };
    let (w, h) = (640u32, 480u32);
    let geo = HudGeometry::build(&frame, w, h);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

    // Independently hand-derived from `Hud.displayScoreboardSidebar` and
    // the shell's fixed-advance jar-less font (`(GLYPH_W + 1) * scale` per
    // visible char, `GLYPH_W == 5`) — not by calling the code under test.
    let str_w = |s: &str| s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0);
    let spacer_w = str_w(": ");
    let title_w = str_w("Kills");
    let row0_w = str_w("Alice") + spacer_w + str_w("11");
    let row1_w = str_w("Bob") + spacer_w + str_w("7");
    let width = title_w.max(row0_w).max(row1_w);
    assert!(
        (width - 54.0).abs() < f32::EPSILON,
        "hand check: expected width 54.0, derived {width} \
         (title {title_w}, row0 {row0_w}, row1 {row1_w})"
    );
    let height = 2.0 * 9.0;
    let bottom = ch / 2.0 + height / 3.0;
    let left = cw - width - 3.0;
    let right = cw - 3.0 + 2.0;
    let header_y = bottom - height;
    let plate_x = left - 2.0;
    let plate_w = right - plate_x;

    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    let mut header_bounds: Option<(f32, f32, f32, f32)> = None;
    let mut body_bounds: Option<(f32, f32, f32, f32)> = None;
    for chunk in geo.verts.chunks(FLOATS_PER_VERTEX) {
        let (x, y) = (px(chunk[0]), py(chunk[1]));
        let (r, g, b, a) = (chunk[2], chunk[3], chunk[4], chunk[5]);
        if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_HEADER_BG_ALPHA).abs() < 1e-4 {
            let e = header_bounds.get_or_insert((x, y, x, y));
            *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
        } else if r == 0.0 && g == 0.0 && b == 0.0 && (a - SIDEBAR_BODY_BG_ALPHA).abs() < 1e-4 {
            let e = body_bounds.get_or_insert((x, y, x, y));
            *e = (e.0.min(x), e.1.min(y), e.2.max(x), e.3.max(y));
        }
    }
    let header = header_bounds
        .expect("the header plate must draw at exactly SIDEBAR_HEADER_BG_ALPHA");
    let body = body_bounds.expect("the body plate must draw at exactly SIDEBAR_BODY_BG_ALPHA");

    let mut mismatches = Vec::new();
    let mut check = |name: &str, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };
    check("header x0", header.0, plate_x);
    check("header x1", header.2, plate_x + plate_w);
    check("header y0", header.1, header_y - 10.0);
    check("header y1", header.3, header_y - 1.0);
    check("body x0", body.0, plate_x);
    check("body x1", body.2, plate_x + plate_w);
    check("body y0", body.1, header_y - 1.0);
    check("body y1", body.3, bottom);
    assert!(
        mismatches.is_empty(),
        "sidebar panel diverged from vanilla's own geometry: {mismatches:?}"
    );
}

/// The boss bar's fixed native rect —
/// `BossHealthOverlay.BAR_WIDTH`/`BAR_HEIGHT` (182×5,
/// `.cache/mc/26.2/client-src`) and `extractRenderState`'s `yOffset`
/// arithmetic — not a canvas-relative width or this HUD's ambient 2×
/// text pitch.
#[test]
fn boss_bar_lands_on_vanillas_fixed_182x5_rect_not_a_canvas_fraction() {
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

    // Zero progress so the background plate is the *only* sprite quad —
    // this test's job is placement, not the fill's width (covered by
    // `boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`).
    let bars = vec![BossBarView {
        title: vec![TextSpan {
            text: "Boss".to_string(),
            style: lodestone_model::text::TextStyle::default(),
        }],
        progress: 0.0,
        color: BossBarColor::Red,
        overlay: BossBarOverlay::Progress,
    }];
    let stats = DebugStats::default();
    let frame = HudFrame {
        show_debug: false,
        crosshair: false,
        boss_bars: &bars,
        ..HudFrame::new(&stats)
    };
    let (w, h) = (640u32, 480u32);
    let atlas = boss_bar_synthetic_atlas();
    let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);

    let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
    let yo = BOSS_BAR_TOP;

    let quads = quad_boxes(&geo.sprite_verts, cw, ch);
    assert_eq!(
        quads.len(),
        1,
        "zero progress with the Progress overlay must draw exactly the \
         background plate: got {quads:?}"
    );
    let bg = quads[0];
    let mut mismatches = Vec::new();
    let mut check = |name: &str, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            mismatches.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };
    check("bar x0", bg.0, bar_x);
    check("bar x1", bg.2, bar_x + BOSS_BAR_WIDTH);
    check("bar y0", bg.1, yo);
    check("bar y1", bg.3, yo + BOSS_BAR_HEIGHT);
    assert!(
        mismatches.is_empty(),
        "boss bar diverged from vanilla's fixed 182x5 rect: {mismatches:?}"
    );
}
