//! Live UI pixel gate for the tab list and scoreboard sidebar.
//!
//! This test connects the shell's live net path to the flat creative 26.2 oracle
//! (`:25570`, RCON `:25571`), creates a known scoreboard sidebar over RCON, then
//! reads the **client's** folded tab list and scoreboard back out through
//! `NetClient`, lowers them through the shell's display projections, renders the
//! HUD into a headless target and reads pixels back.
//!
//! Before Stage 3 of `docs/bevy-migration.md` this gate folded
//! `NetUpdate::{TabListEvent, ScoreboardEvent}` into its *own* `TabList` /
//! `Scoreboard` — i.e. it reimplemented the shell's fold rather than reading it,
//! so it could have passed with the shell's own path broken. It now reads the one
//! fold, which is what makes the pixels evidence about production.
//!
//! The assertions are deliberately pixel-based:
//!
//! * the tab-list control is an **empty tab list** panel; the populated live list
//!   must add bright text pixels inside the tab rect;
//! * the scoreboard control has **no sidebar**; the live sidebar must add pixels
//!   inside the right-edge rect.
//!
//! Run explicitly:
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_tab_scoreboard_pixels -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lodestone::net::NetClient;
use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone::{scoreboard, tablist};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::{RconClient, unique_username};

const GAME_HOST: &str = "127.0.0.1";
/// The flat creative 26.2 oracle: game on `:25570`, RCON on `:25571`.
const GAME_PORT: u16 = 25570;
const RCON_ADDR: &str = "127.0.0.1:25571";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

#[test]
#[ignore = "requires the flat creative 26.2 oracle on :25570 (+ RCON :25571) and a GPU adapter"]
fn live_tab_list_and_scoreboard_reach_pixels() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "no wgpu adapter. This #[ignore]d gate is an explicit live+GPU proof; \
         do not treat missing GPU as a pass.",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (640u32, 480u32);
    let mut target = HeadlessTarget::new(device, w, h, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();

    let token = short_token();
    let objective = format!("ldui{token}");
    let title = format!("Lode{token}");
    let holder = format!("Pixels{token}");

    {
        let mut r = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
            "RCON reachable/authenticated at 127.0.0.1:25571 — is the flat creative 26.2 oracle up?",
        );
        // Idempotent cleanup before setup; the first remove may fail if this
        // token has never been used, which is harmless.
        let _ = r.command(&format!("scoreboard objectives remove {objective}"));
        r.cmd(&format!(
            "scoreboard objectives add {objective} dummy {{\"text\":\"{title}\"}}"
        ));
        r.cmd(&format!("scoreboard players set {holder} {objective} 7"));
        r.cmd(&format!(
            "scoreboard objectives setdisplay sidebar {objective}"
        ));
    }

    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    let net = NetClient::connect_as(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2, None, unique_username());

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut view = tablist::TabListView::default();
    let mut sidebar = None;
    let mut scores = lodestone_game::scoreboard::Scoreboard::new();
    while Instant::now() < deadline {
        // Drain so the shell's bounded update channel cannot back up. Nothing in
        // it is folded here any more — the tab list and scoreboard come out of
        // the client's own `NetIngest` fold below.
        let _ = net.poll();
        scores = net.scoreboard();
        view = tablist::tab_list_view(&net.tab_list(), Some(&scores), &|_: &str| None);
        sidebar = scoreboard::sidebar_from(&scores, &|_: &str| None);
        if !view.is_empty()
            && sidebar
                .as_ref()
                .is_some_and(|side| {
                    lodestone::overlay::spans_text(&side.title).contains(&title)
                        && side.lines.len() == 1
                })
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let sidebar = sidebar.unwrap_or_else(|| {
        cleanup_objective(&objective);
        panic!(
            "live scoreboard/sidebar did not reach the shell fold before timeout: \
             rows={:?}, displayed={:?}",
            view.rows,
            scores.displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar)
        )
    });
    assert!(
        !view.is_empty(),
        "live player-list updates did not produce any tab rows"
    );

    let empty_view = tablist::TabListView::default();
    let tab_empty = render_tab_bright_pixels(
        &mut target,
        &mut hud,
        device,
        queue,
        &stats,
        &empty_view,
        w,
        h,
    );
    let tab_live =
        render_tab_bright_pixels(&mut target, &mut hud, device, queue, &stats, &view, w, h);
    let score_empty =
        render_sidebar_changed_pixels(&mut target, &mut hud, device, queue, &stats, None, w, h);
    let score_live = render_sidebar_changed_pixels(
        &mut target,
        &mut hud,
        device,
        queue,
        &stats,
        Some(&sidebar),
        w,
        h,
    );

    cleanup_objective(&objective);
    drop(net);

    eprintln!("=== shell live UI pixel gate ===");
    eprintln!("tab rows          = {:?}", view.rows);
    eprintln!("sidebar title     = {:?}", sidebar.title);
    eprintln!("sidebar lines     = {:?}", sidebar.lines);
    eprintln!("tab empty bright  = {tab_empty}");
    eprintln!("tab live bright   = {tab_live}");
    eprintln!("score empty px    = {score_empty}");
    eprintln!("score live px     = {score_live}");

    assert!(
        tab_live > tab_empty + 80,
        "live tab-list rows must add text pixels inside the tab rect; empty={tab_empty}, live={tab_live}"
    );
    assert_eq!(
        score_empty, 0,
        "with no sidebar, the right-edge scoreboard rect must stay untouched"
    );
    assert!(
        score_live > 250,
        "live scoreboard sidebar must paint pixels inside the right-edge rect, got {score_live}"
    );
}

fn short_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut s = format!("{:x}", nanos & 0xfffff);
    s.truncate(5);
    s
}

fn cleanup_objective(objective: &str) {
    if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
        let _ = r.command(&format!("scoreboard objectives remove {objective}"));
    }
}

fn render_tab_bright_pixels(
    target: &mut HeadlessTarget,
    hud: &mut HudRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    stats: &DebugStats,
    view: &tablist::TabListView,
    w: u32,
    h: u32,
) -> usize {
    let frame = target.acquire().expect("headless acquire");
    clear(device, queue, frame.view());
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        players: Some(view),
        ..HudFrame::new(stats)
    };
    hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
    let pixels = target.read_texels(device, queue);
    count_bright(&pixels, w, w / 4, h / 4, w / 2, h / 2)
}

fn render_sidebar_changed_pixels(
    target: &mut HeadlessTarget,
    hud: &mut HudRenderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    stats: &DebugStats,
    side: Option<&lodestone::overlay::Sidebar>,
    w: u32,
    h: u32,
) -> usize {
    let frame = target.acquire().expect("headless acquire");
    clear(device, queue, frame.view());
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        sidebar: side,
        ..HudFrame::new(stats)
    };
    hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
    let pixels = target.read_texels(device, queue);
    count_changed(&pixels, w, w * 2 / 3, h / 4, w / 3, h / 2)
}

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("live-ui-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("live-ui-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 128.0 / 255.0,
                        g: 128.0 / 255.0,
                        b: 128.0 / 255.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit(std::iter::once(encoder.finish()));
}

fn count_bright(pixels: &[u8], width: u32, x0: u32, y0: u32, rw: u32, rh: u32) -> usize {
    let mut bright = 0;
    for y in y0..(y0 + rh) {
        for x in x0..(x0 + rw) {
            let i = ((y * width + x) * 4) as usize;
            let avg =
                (u32::from(pixels[i]) + u32::from(pixels[i + 1]) + u32::from(pixels[i + 2])) / 3;
            if avg > 158 {
                bright += 1;
            }
        }
    }
    bright
}

fn count_changed(pixels: &[u8], width: u32, x0: u32, y0: u32, rw: u32, rh: u32) -> usize {
    let mut changed = 0;
    for y in y0..(y0 + rh) {
        for x in x0..(x0 + rw) {
            let i = ((y * width + x) * 4) as usize;
            let d = (i32::from(pixels[i]) - 128).abs()
                + (i32::from(pixels[i + 1]) - 128).abs()
                + (i32::from(pixels[i + 2]) - 128).abs();
            if d > 25 {
                changed += 1;
            }
        }
    }
    changed
}
