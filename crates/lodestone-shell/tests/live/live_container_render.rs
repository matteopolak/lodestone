//! Live pixel gate for the container/inventory screen.
//!
//! The state fold lives in `lodestone-game::menus`; this test proves the shell
//! actually reads that folded live state and draws it. It connects to the flat
//! creative 26.2 oracle (`:25570`, RCON `:25571`), places a populated chest next
//! to the bot, opens it through the real serverbound `use_item_on` path, then
//! renders the resulting `OpenMenuSnapshot` through `ContainerRenderer`.
//!
//! Run explicitly:
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_container_render -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, Rect};
use lodestone_client::{ClientAction, ClientBuilder, LoginProfile, Rotation, ServerAddress, Vec3};
use lodestone_model::{BlockFace, BlockPos, Hand, Vec3f};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::{RconClient, unique_username};

const GAME_HOST: &str = "127.0.0.1";
/// The flat creative 26.2 oracle: game on `:25570`, RCON on `:25571`.
const GAME_PORT: u16 = 25570;
const RCON_ADDR: &str = "127.0.0.1:25571";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

const CHEST_POS: BlockPos = BlockPos::new(97, 80, 96);
const PLAYER_POS: Vec3 = Vec3::new(96.5, 80.0, 96.5);
const PLAYER_ROTATION: Rotation = Rotation::new(-90.0, 0.0);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the flat creative 26.2 oracle on :25570 (+ RCON :25571) and a GPU adapter"]
async fn live_open_container_reaches_pixels() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "no wgpu adapter. This #[ignore]d gate is an explicit live+GPU proof; \
         do not treat missing GPU as a pass.",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (640u32, 480u32);
    let bg = [16i32, 18, 23];

    let username = unique_username();
    let server = ServerAddress {
        host: GAME_HOST.to_owned(),
        port: GAME_PORT,
    };
    let profile = LoginProfile {
        username: username.clone(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(PROTOCOL_26_2)
        .expect("v770 adapter compiled by lodestone-shell/live feature");
    let (mut handle, mut events) = ClientBuilder::new(server, profile, adapter)
        .event_buffer(4096)
        .connect_timeout(Some(Duration::from_secs(10)))
        .connect()
        .await
        .expect("connect to flat creative 26.2 oracle");
    let mut recent_events = Vec::new();

    wait_for_state(
        &handle,
        &mut events,
        &mut recent_events,
        Duration::from_secs(10),
        |h| h.game_mode().is_some(),
    )
    .await
    .unwrap_or_else(|| {
        panic!(
            "client did not reach Play before container setup; recent events: {recent_events:#?}"
        )
    });
    handle
        .send_action(ClientAction::PlayerLoaded)
        .expect("send player-loaded readiness before server-side interaction");

    setup_populated_chest(&username);
    if handle.is_finished() {
        panic!("client closed before chest interaction; recent events: {recent_events:#?}");
    }
    wait_for_state(
        &handle,
        &mut events,
        &mut recent_events,
        Duration::from_secs(5),
        |h| h.position().is_some_and(near_player_pos),
    )
    .await
    .unwrap_or_else(|| {
        cleanup_chest();
        panic!(
            "client did not acknowledge teleport near the test chest; pos={:?}; recent events: {recent_events:#?}",
            handle.position()
        )
    });
    if let Err(error) = drive_open_chest(&handle, &mut events, &mut recent_events).await {
        cleanup_chest();
        let outcome = handle.join().await;
        panic!("{error}; session outcome: {outcome:?}");
    }

    fn near_player_pos(pos: Vec3) -> bool {
        (pos.x - PLAYER_POS.x).abs() < 0.01
            && (pos.y - PLAYER_POS.y).abs() < 0.01
            && (pos.z - PLAYER_POS.z).abs() < 0.01
    }

    let open = wait_for_open_menu(&handle, &mut events, &mut recent_events)
        .await
        .unwrap_or_else(|| {
            cleanup_chest();
            panic!(
                "live chest did not open into an OpenMenuSnapshot before timeout; recent events: {recent_events:#?}"
            )
        });
    let first_stack = open
        .menu
        .slot_item(0)
        .unwrap_or_else(|| {
            cleanup_chest();
            panic!("live chest opened but slot 0 was empty; container content did not fold")
        })
        .clone();
    assert_eq!(first_stack.item().path(), "diamond");

    let title = open.title.to_plain_string();
    let frame = ContainerFrame::new(Some(&open.menu), &title);
    let rect = ContainerGeometry::build(&frame, w, h)
        .widget_rect
        .expect("open live menu has a widget rect");

    let mut renderer = ContainerRenderer::new(device, format);
    let empty_pixels = render_container(
        device,
        queue,
        format,
        w,
        h,
        &mut renderer,
        &ContainerFrame::empty(),
        bg,
    );
    let live_pixels = render_container(device, queue, format, w, h, &mut renderer, &frame, bg);

    cleanup_chest();
    let _ = handle.send_action(ClientAction::ContainerClose {
        window_id: open.window_id,
    });
    handle.shutdown();

    let empty_rect_px = changed_pixels_in_rect(&empty_pixels, w, rect, bg);
    let live_rect_px = changed_pixels_in_rect(&live_pixels, w, rect, bg);
    let corner_px = changed_pixels_in_corners(&live_pixels, w, h, bg);
    let coverage = live_rect_px as f64 / f64::from(w * h);

    eprintln!("=== shell live container render ===");
    eprintln!("username             = {username}");
    eprintln!("window id            = {}", open.window_id);
    eprintln!("title                = {title:?}");
    eprintln!("slot 0 item          = {:?}", first_stack.item());
    eprintln!("container coverage   = {:.2}%", coverage * 100.0);
    eprintln!("container rect px    = {live_rect_px}");
    eprintln!("empty control px     = {empty_rect_px}");
    eprintln!("corner px            = {corner_px}");

    assert_eq!(
        empty_rect_px, 0,
        "closed/empty container state must not light the live widget rect"
    );
    assert!(
        live_rect_px > 1_000,
        "live opened container must paint pixels inside its widget rect, only {live_rect_px}"
    );
    assert_eq!(
        corner_px, 0,
        "the centred container must leave frame corners at background, got {corner_px}"
    );
}

async fn wait_for_open_menu(
    handle: &lodestone_client::ClientHandle,
    events: &mut lodestone_client::EventStream,
    recent_events: &mut Vec<String>,
) -> Option<lodestone_client::OpenMenuSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        drain_events(events, recent_events);
        if let Some(open) = handle.open_menu()
            && open.menu.slot_item(0).is_some()
        {
            return Some(open);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_state(
    handle: &lodestone_client::ClientHandle,
    events: &mut lodestone_client::EventStream,
    recent_events: &mut Vec<String>,
    timeout: Duration,
    mut predicate: impl FnMut(&lodestone_client::ClientHandle) -> bool,
) -> Option<()> {
    let deadline = Instant::now() + timeout;
    loop {
        drain_events(events, recent_events);
        if predicate(handle) {
            return Some(());
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn drain_events(events: &mut lodestone_client::EventStream, recent_events: &mut Vec<String>) {
    while let Ok(event) = events.try_recv() {
        recent_events.push(format!("{event:?}"));
        if recent_events.len() > 20 {
            recent_events.remove(0);
        }
    }
}

fn setup_populated_chest(username: &str) {
    let mut r = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
        "RCON reachable/authenticated at 127.0.0.1:25571 — is the flat creative 26.2 oracle up?",
    );
    let _ = r.cmd(&format!("gamemode creative {username}"));
    let _ = r.cmd(&format!(
        "setblock {} {} {} minecraft:stone replace",
        PLAYER_POS.x.floor() as i32,
        CHEST_POS.y - 1,
        PLAYER_POS.z.floor() as i32
    ));
    let _ = r.cmd(&format!(
        "setblock {} {} {} minecraft:stone replace",
        CHEST_POS.x,
        CHEST_POS.y - 1,
        CHEST_POS.z
    ));
    let _ = r.cmd(&format!(
        "setblock {} {} {} air replace",
        CHEST_POS.x, CHEST_POS.y, CHEST_POS.z
    ));
    let response = r.cmd(&format!(
        "setblock {} {} {} minecraft:chest[facing=west]{{Items:[{{Slot:0b,id:\"minecraft:diamond\",count:5}},{{Slot:1b,id:\"minecraft:torch\",count:64}}]}} replace",
        CHEST_POS.x, CHEST_POS.y, CHEST_POS.z
    ));
    assert!(
        response.contains("Changed") || response.contains("already"),
        "chest setup failed: {response}"
    );
    let contents = r.cmd(&format!(
        "data get block {} {} {} Items",
        CHEST_POS.x, CHEST_POS.y, CHEST_POS.z
    ));
    assert!(
        contents.contains("diamond") && contents.contains("torch"),
        "chest contents were not installed: {contents}"
    );
    let tp = r.cmd(&format!(
        "tp {username} {:.3} {:.3} {:.3} -90 0",
        PLAYER_POS.x, PLAYER_POS.y, PLAYER_POS.z
    ));
    assert!(
        !tp.contains("No entity") && !tp.contains("No player"),
        "teleport target {username} was not present: {tp}"
    );
    let _ = r.cmd("tick sprint 2");
}

async fn drive_open_chest(
    handle: &lodestone_client::ClientHandle,
    events: &mut lodestone_client::EventStream,
    recent_events: &mut Vec<String>,
) -> Result<(), String> {
    for sequence in 1..=5 {
        handle
            .move_to(PLAYER_POS, PLAYER_ROTATION, true, false)
            .map_err(|err| {
                format!("send movement near chest failed: {err}; recent events: {recent_events:#?}")
            })?;
        handle
            .send_action(ClientAction::UseItemOn {
                hand: Hand::Main,
                pos: CHEST_POS,
                face: BlockFace::West,
                cursor: Vec3f::new(0.0, 0.5, 0.5),
                inside_block: false,
                sequence,
            })
            .map_err(|err| format!("send use_item_on chest failed: {err}"))?;
        handle
            .send_action(ClientAction::SwingArm { hand: Hand::Main })
            .map_err(|err| format!("send swing failed: {err}"))?;
        handle
            .send_action(ClientAction::EndClientTick)
            .map_err(|err| format!("send client tick end failed: {err}"))?;
        drain_events(events, recent_events);
        tokio::time::sleep(Duration::from_millis(150)).await;
        if handle.open_menu().is_some() {
            return Ok(());
        }
    }
    Ok(())
}

fn cleanup_chest() {
    if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
        let _ = r.command(&format!(
            "setblock {} {} {} air replace",
            CHEST_POS.x, CHEST_POS.y, CHEST_POS.z
        ));
    }
}

fn render_container(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    renderer: &mut ContainerRenderer,
    frame: &ContainerFrame<'_>,
    bg: [i32; 3],
) -> Vec<u8> {
    let mut target = HeadlessTarget::new(device, width, height, format);
    let acquired = target.acquire().expect("headless acquire");
    clear(device, queue, acquired.view(), bg);
    renderer.render(device, queue, acquired.view(), frame, width, height);
    acquired.present(queue);
    target.read_texels(device, queue)
}

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, bg: [i32; 3]) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("live-container-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("live-container-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: bg[0] as f64 / 255.0,
                        g: bg[1] as f64 / 255.0,
                        b: bg[2] as f64 / 255.0,
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

fn changed_pixels_in_rect(pixels: &[u8], width: u32, rect: Rect, bg: [i32; 3]) -> usize {
    let mut changed = 0;
    let min_x = rect.x.max(0.0).floor() as u32;
    let max_x = (rect.x + rect.w).min(width as f32).ceil() as u32;
    let min_y = rect.y.max(0.0).floor() as u32;
    let max_y = (rect.y + rect.h).ceil() as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let i = ((y * width + x) * 4) as usize;
            if changed_from_bg(&pixels[i..i + 4], bg) {
                changed += 1;
            }
        }
    }
    changed
}

fn changed_pixels_in_corners(pixels: &[u8], width: u32, height: u32, bg: [i32; 3]) -> usize {
    let mut changed = 0;
    for y in 0..height {
        for x in 0..width {
            let corner =
                (x < width / 8 || x >= 7 * width / 8) && (y < height / 8 || y >= 7 * height / 8);
            if corner {
                let i = ((y * width + x) * 4) as usize;
                if changed_from_bg(&pixels[i..i + 4], bg) {
                    changed += 1;
                }
            }
        }
    }
    changed
}

fn changed_from_bg(px: &[u8], bg: [i32; 3]) -> bool {
    let d = (i32::from(px[0]) - bg[0]).abs()
        + (i32::from(px[1]) - bg[1]).abs()
        + (i32::from(px[2]) - bg[2]).abs();
    d > 25
}
