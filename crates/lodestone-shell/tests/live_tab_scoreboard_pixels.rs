//! Live UI pixel gate for the tab list and scoreboard sidebar.
//!
//! This test connects the shell's live net path to the flat creative 26.2 oracle
//! (`:25570`, RCON `:25571`), creates a known scoreboard sidebar over RCON, folds
//! live `ClientEvent` deltas through `lodestone-game`, lowers them through the
//! shell's tab-list / scoreboard display projections, then renders the HUD into a
//! headless target and reads pixels back.
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

use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone::net::{NetClient, NetUpdate};
use lodestone::{scoreboard, tablist};
use lodestone_game::{scoreboard::Scoreboard, tablist::TabList};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::RconClient;

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

    let net = NetClient::connect(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2);
    let mut tabs = TabList::new();
    let mut scores = Scoreboard::new();

    let deadline = Instant::now() + Duration::from_secs(25);
    let mut rows = Vec::new();
    let mut sidebar = None;
    while Instant::now() < deadline {
        for update in net.poll() {
            match update {
                NetUpdate::TabListEvent(event) => {
                    let _ = tabs.apply(&event);
                }
                NetUpdate::ScoreboardEvent(event) => {
                    let _ = scores.apply(&event);
                }
                _ => {}
            }
        }
        rows = tablist::player_rows(&tabs, &|_: &str| None);
        sidebar = scoreboard::sidebar_from(&scores, &|_: &str| None);
        if !rows.is_empty()
            && sidebar
                .as_ref()
                .is_some_and(|side| side.title.contains(&title) && side.lines.len() == 1)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let sidebar = sidebar.unwrap_or_else(|| {
        cleanup_objective(&objective);
        panic!(
            "live scoreboard/sidebar did not reach the shell fold before timeout: \
             rows={rows:?}, displayed={:?}",
            scores.displayed(lodestone_game::scoreboard::DisplaySlot::Sidebar)
        )
    });
    assert!(
        !rows.is_empty(),
        "live player-list updates did not produce any tab rows"
    );

    let empty_rows: Vec<String> = Vec::new();
    let tab_empty = render_tab_bright_pixels(
        &mut target,
        &mut hud,
        device,
        queue,
        &stats,
        &empty_rows,
        w,
        h,
    );
    let tab_live =
        render_tab_bright_pixels(&mut target, &mut hud, device, queue, &stats, &rows, w, h);
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
    eprintln!("tab rows          = {rows:?}");
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
    rows: &[String],
    w: u32,
    h: u32,
) -> usize {
    let frame = target.acquire().expect("headless acquire");
    clear(device, queue, frame.view());
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        players: Some(rows),
        ..HudFrame::new(stats)
    };
    hud.render(device, queue, frame.view(), &hud_frame, w, h);
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
    hud.render(device, queue, frame.view(), &hud_frame, w, h);
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
