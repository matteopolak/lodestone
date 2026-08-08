//! Browser feasibility spike — W6: join the halves.
//!
//! Earlier spike turns proved two things *separately*: the real
//! `lodestone-render` greedy-mesh path draws in a browser (WebGPU), and the
//! `lodestone-relay` bridges a real vanilla join over WebSocket. This turn wires
//! the surrounding browser reality that a real multiplayer client needs, using
//! only the **public** APIs of crates this spike owns or may depend on:
//!
//!  1. **Assets over `fetch`.** `lodestone-assets`' `ResourceSource` is a *sync,
//!     byte-based* trait; `ZipSource::from_bytes` builds a fully in-memory pack
//!     with no `std::fs`. So the "browser has no filesystem" wall is crossed by
//!     acquiring the zip bytes asynchronously *once* (here, `fetch`) and then
//!     using the existing sync parsers unchanged. We prove that end to end: a
//!     real vanilla block texture is deflate-inflated (`zlib-rs`) and PNG-decoded
//!     (`png`) **in the browser at runtime**, then uploaded and drawn.
//!  2. **A `performance.now()` clock.** `Instant::now()` traps on wasm;
//!     `lodestone-render` already exposes an injectable `TimeSource`. We back it
//!     with `performance.now()` and drive a real `FramePacer`, reporting live
//!     frame time — proof the wasm-safe clock seam works.
//!  3. **Live networking through the relay.** A real Server-List-Ping is driven
//!     over `WsWebTransport` (browser `WebSocket`) through `lodestone-relay` to
//!     the live vanilla server, and the server's real status JSON is shown. This
//!     proves the *browser* transport (not just the native one) reaches a real
//!     server — isolating the one remaining wall (see the report) to the client
//!     driver's `tokio::spawn`, not to byte flow.
//!
//! ## Scope — what this build does NOT yet prove (read before citing it)
//!
//! This is a **feasibility spike, not the real client**, and the distinction is
//! the §12.24 failure mode (recording a gate as proving *delivery* when it only
//! proved *decode*). Stated plainly so nobody over-reads the demo:
//!
//!  * **No `lodestone-client` dependency.** Terrain here is decoded from a
//!    committed fixture of real server bytes straight through the `v770` parser
//!    (see `terrain.rs`). It renders real chunk *bytes*, but it is not the
//!    library-first client the project is organised around — there is no client
//!    driver, no world store, no event loop in this build.
//!  * **The relay path is proven natively, not in the browser.** A real live
//!    26.2 join over the WS relay is covered by a native `#[ignore]`d test
//!    (`lodestone-relay`); in *this* browser build the relay is exercised only by
//!    a Server-List-Ping, not a full play join. Byte flow is proven; a browser
//!    play session is not.
//!
//! ## The browser is *not* the limiting factor
//!
//! Read this before optimising the wrong layer. Everything the browser needs
//! already works in the browser: transport (`WsWebTransport` over `web_sys`),
//! framing, the online-mode encryption/crypto path, asset acquisition over
//! `fetch`, and wgpu rendering. What is thin is *upstream and version-specific*:
//! `v770`'s play dispatch is now **46 of 141 clientbound** ids (up from 8) and
//! **15 of 69 serverbound** — growing fast, but still missing enough that a
//! browser wired to a live server receives chunks, keep-alives and a partial
//! world rather than a full one. (Earlier notes said "8 of 265"; 265 counts
//! every id in both directions across all five states — the real play
//! denominators are 141 clientbound and 69 serverbound.) That gap is **adapter
//! dispatch breadth in the version crates, entirely outside
//! `web/`/`lodestone-relay`/`lodestone-net`'s wasm layer** (assigned elsewhere).
//! So "the browser isn't a real client yet" is true but misleading: the browser
//! is ready; the packet coverage feeding it is what's catching up.
//!
//! Singleplayer sidesteps that entirely: `web/src/singleplayer.rs` runs the real
//! `lodestone-client` ↔ `lodestone-server` ↔ worldgen stack in-browser over an
//! in-memory duplex (no relay, no CORS, no server, no dispatch dependency) and is
//! proven to reach `Play` and receive a real worldgen chunk. The one thing left
//! before it *renders* is the same client seam below.
//!
//! When `impl-client`'s seam lands (`chunk(pos) -> Option<Arc<LoadedChunk>>`, and
//! stripping `column` from `ClientEvent::ChunkLoaded`), W5 becomes the real
//! milestone: browser -> WS relay -> live 26.2 server -> `World` query -> meshed
//! frame, end to end. Until then, treat this as "decode + render proven in a
//! browser", nothing more.

mod input;
mod multiplayer;
mod singleplayer;
mod terrain;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use glam::Vec3;
use input::{Controls, FlyCamera};
use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_client::ChunkPos;
use lodestone_net::WsWebTransport;
// `shared_camera_buffer` + `section_origin_buffer`, not the old
// `camera_buffer(device, CameraUniform)`. Issue #76 split group 0 into a shared
// per-frame uniform (view-projection + fog, binding 0) and a dynamic-offset
// section-origin slot (binding 1), precisely to stop the whole-struct write per
// section per frame this file used to do.
use lodestone_render::block::{shared_camera_buffer, sprite_uv_buffer};
use lodestone_render::{
    BlockPipeline, DepthBuffer, FramePacer, GpuAtlas, GpuContext, GpuMesh, RenderTarget,
    SurfaceTarget, TimeSource, section_origin_buffer,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{HtmlCanvasElement, Response, window};

const WIDTH: u32 = 900;
const HEIGHT: u32 = 640;

/// URL of the trimmed real resource pack, copied into `dist/` by trunk. It is a
/// real zip of real vanilla 26.2 blockstates/models/textures for exactly the
/// blocks the fixture contains (see `web/assets/blocks_pack.zip`, ~6 KB). A real
/// client would `fetch` `client.jar` (the full 4.9 MiB renderable corpus,
/// measured earlier) here instead, and **nothing else in this path changes** —
/// the seam is the byte source.
const PACK_URL: &str = "blocks_pack.zip";

/// The committed fixture of real `level_chunk_with_light` payloads captured from
/// the live vanilla 26.2 server. This is W7's real-server-bytes input.
const FIXTURE_URL: &str = "fixtures/chunks.bin";

/// The density/noise JSON the browser worldgen resolver reads (the native test's
/// `FsResolver` reads these from disk; a browser has no filesystem, so they are
/// fetched once — the 97 files concatenated into one map, ~145 KB).
const WORLDGEN_URL: &str = "worldgen.json";

/// The relay's WebSocket address. Run:
/// `cargo run -p lodestone-relay -- --listen 127.0.0.1:25580 --target 127.0.0.1:25565`
const RELAY_URL: &str = "ws://127.0.0.1:25580";

/// A `performance.now()`-backed [`TimeSource`]. `Instant::now()` traps on wasm;
/// this is the browser clock the render crate's seam was built for.
struct PerfClock {
    perf: web_sys::Performance,
}

impl PerfClock {
    fn new() -> Option<Self> {
        window()?.performance().map(|perf| Self { perf })
    }
}

impl TimeSource for PerfClock {
    fn now(&self) -> Duration {
        // `performance.now()` is a monotonic millisecond timestamp (sub-ms
        // precision), which is exactly what a monotonic `TimeSource` wants.
        Duration::from_secs_f64(self.perf.now().max(0.0) / 1000.0)
    }
}

/// Everything needed to draw one frame, owned by the animation closure.
struct State {
    ctx: GpuContext,
    target: SurfaceTarget<'static>,
    pipeline: BlockPipeline,
    atlas: GpuAtlas,
    uv: wgpu::Buffer,
    /// One greedy-meshed section per entry, with its world-space origin passed
    /// to `CameraUniform` so section-local vertices land at the right place.
    meshes: Vec<(GpuMesh, [f32; 3])>,
    /// Interactive free-fly camera pose, advanced from `controls` each frame.
    fly: FlyCamera,
    /// Browser input platform layer (pointer lock + keyboard + mouse-look).
    controls: Controls,
    /// Number of fixture chunks that contributed geometry (for the HUD).
    chunk_count: usize,
    depth: DepthBuffer,
    frame: u32,
    clock: Option<PerfClock>,
    pacer: FramePacer,
    ema_ms: f64,
    /// Last frame delta (seconds) for frame-rate-independent camera movement.
    last_dt: f32,
}

// The live `State`, reachable from outside the `requestAnimationFrame` closure.
// (A `//` comment, not a doc comment: rustdoc does not document items produced by
// a macro invocation, and `#[warn(unused_doc_comments)]` fires on `///` here.)
//
// It used to be *owned* by that closure, which made the render path
// **unobservable from anywhere else** — a verification hole, not a style point. A
// hidden or backgrounded tab does not run `requestAnimationFrame` at all
// (measured in a headless Chrome pane: `document.visibilityState == "hidden"` =>
// `requestAnimationFrame` fired **0** times in 600 ms), so a browser harness sees
// a transparent canvas and *no* error while every HUD line still reports success
// — the island failure exactly, tree green and screen wrong. Parking the state
// here lets `lodestone_render_frames` drive frames directly, so "does it draw?"
// becomes a measurement instead of an inference from a HUD string.
thread_local! {
    static RENDER_STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Renders `frames` frames synchronously, bypassing `requestAnimationFrame`, and
/// returns the number of frames this `State` has drawn in total.
///
/// This exists so a browser gate can assert **pixels**, the only evidence that
/// separates "compiled and initialised" from "drew something".
/// `draw_geometry = false` is the **negative control**: it runs the identical
/// pass — same surface, same clear colour, same depth attachment — and submits no
/// draws, so the canvas must come back as exactly the clear colour. A gate whose
/// control does not go one way while its subject goes the other is measuring
/// nothing. See `web/README.md` -> "Verifying that it actually draws" for the
/// measured control/subject numbers this hook produced.
///
/// Returns `u32::MAX` when the state is not initialised (async setup still in
/// flight, or it failed) — deliberately not `0`, which a caller would read as
/// "rendered 0 frames, fine".
#[wasm_bindgen]
pub fn lodestone_render_frames(frames: u32, draw_geometry: bool) -> u32 {
    RENDER_STATE.with_borrow_mut(|slot| match slot.as_mut() {
        Some(state) => {
            for _ in 0..frames {
                state.render(draw_geometry);
            }
            state.frame
        }
        None => u32::MAX,
    })
}

impl State {
    fn render(&mut self, draw_geometry: bool) {
        let device = self.ctx.device();
        let queue = self.ctx.queue();

        // Advance the injectable clock. This is the whole point of the seam: on
        // native this is `SystemClock`; here it is `performance.now()`, and no
        // `Instant::now()` is ever reached.
        if let Some(clock) = &self.clock {
            let timing = self.pacer.tick(clock);
            let ms = timing.delta.as_secs_f64() * 1000.0;
            self.last_dt = timing.delta.as_secs_f32().clamp(0.0, 0.1);
            if timing.frame_index > 0 {
                self.ema_ms = if self.ema_ms == 0.0 {
                    ms
                } else {
                    self.ema_ms * 0.9 + ms * 0.1
                };
            }
            if timing.frame_index.is_multiple_of(30) {
                let fps = if self.ema_ms > 0.0 {
                    1000.0 / self.ema_ms
                } else {
                    0.0
                };
                set_line(
                    "frame",
                    &format!(
                        "clock: performance.now() via FramePacer — frame {} | {:.2} ms/frame (~{fps:.0} fps)",
                        timing.frame_index, self.ema_ms,
                    ),
                );
            }
        }

        // Advance the interactive free-fly camera from browser input. This
        // replaced the old auto-orbit: the scene is now one you move through
        // (WASD + mouse-look under pointer lock). Look + input semantics come
        // from the shared `lodestone-controller` (vanilla cubic sensitivity,
        // forward-gated sprint); only the gravity-free fly integration is
        // browser-local, mirroring native `fly_tick` — see `input.rs`.
        self.frame = self.frame.wrapping_add(1);
        let aspect = WIDTH as f32 / HEIGHT as f32;
        let camera = self.fly.advance(&self.controls, self.last_dt, aspect);

        // Each section's vertices are section-local (0..16), so group 0 needs the
        // section's world origin. Since issue #76 that origin is a *separate*
        // binding from the view-projection: one shared buffer for the frame,
        // built once here, and a 16-byte origin slot per section. Fog is
        // `disabled()` — this spike has no sky, and a fog uniform whose ranges
        // are degenerate reports `enabled = 0.0` rather than fogging to black.
        //
        // A bind group per section is still one draw's worth of setup here
        // because the spike has a handful of sections; the arena + dynamic-offset
        // route `camera_bind_group`'s doc describes is what the native client
        // uses and what this would grow into.
        let atlas_bg = self.pipeline.atlas_bind_group(device, &self.atlas, &self.uv);
        let shared_cam = shared_camera_buffer(
            device,
            camera.view_projection().to_cols_array_2d(),
            lodestone_render::fog::FogUniform::disabled(),
        );
        let cam_bgs: Vec<wgpu::BindGroup> = self
            .meshes
            .iter()
            .map(|(_, origin)| {
                let origin_buf = section_origin_buffer(device, *origin);
                self.pipeline
                    .camera_bind_group(device, &shared_cam, &origin_buf)
            })
            .collect();

        let frame = match self.target.acquire() {
            Ok(f) => f,
            Err(e) => {
                if e.needs_reconfigure() {
                    self.target.reconfigure(device);
                }
                return;
            }
        };

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("block pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: frame.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.55,
                            g: 0.68,
                            b: 0.85,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // `draw_geometry == false` is the negative control's only difference
            // from a real frame: the pass, the clear and the depth attachment are
            // identical, and nothing is drawn into it.
            if draw_geometry {
                pass.set_pipeline(&self.pipeline.pipeline);
                pass.set_bind_group(1, &atlas_bg, &[]);
                for ((mesh, _origin), cam_bg) in self.meshes.iter().zip(&cam_bgs) {
                    // `&[0]`, not `&[]`. `lodestone-camera-bgl` binding 1 (the
                    // section origin) is declared `has_dynamic_offset: true` by
                    // issue #76's group-0 split, so this call must supply exactly
                    // ONE dynamic offset. Passing an empty slice made every
                    // command buffer invalid, and the browser said so:
                    //
                    //   The number of dynamic offsets (0) does not match the
                    //   number of dynamic buffers (1) in [BindGroupLayoutInternal
                    //   "lodestone-camera-bgl"]
                    //   [Invalid CommandBuffer] is invalid due to a previous error
                    //
                    // The failure mode is why it survived: the clear still lands,
                    // so the canvas shows a clean sky and every HUD line reports
                    // success ("250 greedy quads"), while zero geometry pixels
                    // reach the screen. wgpu reports it as a **warning**, not a
                    // panic, so nothing fails loudly and no `cargo` command can
                    // see it. Each section owns its own 16-byte
                    // `section_origin_buffer`, so the offset here is 0 — the arena
                    // route the native client uses is what passes a non-zero one.
                    pass.set_bind_group(0, cam_bg, &[0]);
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..mesh.index_count, 0, 0..1);
                }
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
        frame.present(queue);
    }
}

/// Sets a single HUD line by element id (the ids exist in `index.html`).
fn set_line(id: &str, text: &str) {
    if let Some(doc) = window().and_then(|w| w.document())
        && let Some(el) = doc.get_element_by_id(id)
    {
        el.set_text_content(Some(text));
    }
    log::info!("[{id}] {text}");
}

/// Fetches a URL into bytes. This is the *only* async step the asset path needs;
/// everything downstream is the existing synchronous parser stack.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    let win = window().ok_or("no window")?;
    let resp_val = JsFuture::from(win.fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch failed: {e:?}"))?;
    let resp: Response = resp_val.dyn_into().map_err(|_| "not a Response")?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let buf = JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("array_buffer failed: {e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Everything the terrain path needs, loaded over `fetch`.
struct LoadedTerrain {
    assets: terrain::TerrainAssets,
    chunks: Vec<terrain::DecodedChunk>,
    pack_len: usize,
    fixture_len: usize,
    /// The raw pack bytes, kept so a live join can rebuild the atlas and
    /// classifier for whatever blocks the *server* turns out to send. `ZipSource`
    /// consumes its bytes, so re-parsing needs a second copy; the trimmed pack is
    /// ~6 KB, so keeping one is cheaper than a second `fetch`.
    pack_bytes: Rc<Vec<u8>>,
}

/// Loads the real terrain path end to end: fetch the trimmed pack and the
/// captured-chunk fixture, decode the chunks with the real v770 parser, and
/// build the atlas + classifier with the real assets resolver. All sync parsing
/// after the two `fetch`es — the browser filesystem wall is crossed once, at the
/// byte source, exactly as native's only differing step (`std::fs`) would be.
async fn load_terrain() -> Result<LoadedTerrain, String> {
    let pack = fetch_bytes(PACK_URL).await?;
    let pack_len = pack.len();
    let pack_bytes = Rc::new(pack.clone());
    let source = ZipSource::from_bytes(pack).map_err(|e| format!("zip parse: {e}"))?;
    let manager = ResourceManager::new(vec![Box::new(source)]);

    let fixture = fetch_bytes(FIXTURE_URL).await?;
    let fixture_len = fixture.len();
    let chunks = terrain::parse_fixture(&fixture)?;
    let ids = terrain::distinct_block_ids(&chunks);
    let assets = terrain::build_terrain_assets(&manager, &ids)?;

    Ok(LoadedTerrain {
        assets,
        chunks,
        pack_len,
        fixture_len,
        pack_bytes,
    })
}

/// Minecraft VarInt (7 bits/byte, little-endian groups, MSB = continuation).
fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            break;
        }
        buf.push(byte | 0x80);
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

async fn read_varint<R: AsyncReadExt + Unpin>(r: &mut R) -> Result<i32, String> {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    loop {
        let byte = r.read_u8().await.map_err(|e| e.to_string())?;
        result |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(result as i32);
        }
        shift += 7;
        if shift >= 32 {
            return Err("varint too long".to_string());
        }
    }
}

/// Drives a real Server-List-Ping through the relay over the browser WebSocket
/// transport, returning the server's status JSON. This is a protocol handshake
/// the relay forwards blindly — proof the *browser* transport reaches the live
/// server, without needing the (blocked) client driver.
async fn ping_via_relay(url: &str) -> Result<String, String> {
    let mut t = WsWebTransport::connect(url)
        .await
        .map_err(|e| format!("ws connect: {e}"))?;

    // Handshake (state → status) then a status request, in one write.
    let mut handshake = Vec::new();
    write_varint(&mut handshake, 0x00); // packet id: handshake
    write_varint(&mut handshake, 770); // protocol version (status ignores exact value)
    write_string(&mut handshake, "127.0.0.1");
    handshake.extend_from_slice(&25565u16.to_be_bytes());
    write_varint(&mut handshake, 1); // next state: status

    let mut out = Vec::new();
    write_varint(&mut out, handshake.len() as i32);
    out.extend_from_slice(&handshake);
    write_varint(&mut out, 1); // status request length
    write_varint(&mut out, 0x00); // status request packet id

    t.write_all(&out).await.map_err(|e| e.to_string())?;
    t.flush().await.map_err(|e| e.to_string())?;

    // Response: [len][packet id = 0x00][json string].
    let _len = read_varint(&mut t).await?;
    let pid = read_varint(&mut t).await?;
    if pid != 0 {
        return Err(format!("unexpected status packet id {pid}"));
    }
    let json_len = read_varint(&mut t).await?;
    if !(0..=512 * 1024).contains(&json_len) {
        return Err(format!("implausible status length {json_len}"));
    }
    let mut buf = vec![0u8; json_len as usize];
    t.read_exact(&mut buf).await.map_err(|e| e.to_string())?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

/// Extracts `version.name` from status JSON without pulling a JSON dependency
/// into the spike, plus a short raw prefix for the HUD.
fn summarise_status(json: &str) -> String {
    let name = extract_after(json, "\"name\":\"");
    let short = if json.len() > 160 { &json[..160] } else { json };
    match name {
        Some(v) => format!("version.name = \"{v}\" | raw: {short}…"),
        None => format!("raw status: {short}…"),
    }
}

fn extract_after(haystack: &str, needle: &str) -> Option<String> {
    let start = haystack.find(needle)? + needle.len();
    let rest = &haystack[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// Live multiplayer: a real browser join, rendered.
// ---------------------------------------------------------------------------

/// Columns either side of the player that the live scene meshes. A flat creative
/// world is ~2 non-empty sections per column, so 9×9 columns is a cheap scene;
/// normal terrain is much heavier, which is why this is small rather than the
/// server's whole view distance.
const LIVE_VIEW_RADIUS: i32 = 4;

/// How often the live scene is rebuilt from the client's chunk store.
const LIVE_POLL_MS: u32 = 500;

// One session at a time. A second click while a session is live would spawn a
// second driver against the same relay, and the two would fight over the scene.
thread_local! {
    static SESSION_LIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Uploads a scene and swaps it into the live [`RENDER_STATE`], returning
/// `(sections, quads)`.
///
/// `assets` re-uploads the atlas and its UV table (a live world's block set is
/// whatever the *server* sends, so the atlas is not fixed at startup like the
/// fixture path's). `view` reseats the camera to `(eye, target)`; pass it only
/// once per session, or the camera snaps back on every chunk batch.
fn install_scene(
    meshes: &[terrain::SectionMesh],
    assets: Option<&terrain::TerrainAssets>,
    view: Option<(Vec3, Vec3)>,
) -> Result<(usize, usize), String> {
    // `wgpu::Device`/`Queue` are `Arc`-backed and `Clone`, so cloning them out
    // releases the `RENDER_STATE` borrow before the uploads — which matters,
    // because the install below needs it mutably.
    let (device, queue) = RENDER_STATE
        .with_borrow(|slot| {
            slot.as_ref()
                .map(|state| (state.ctx.device().clone(), state.ctx.queue().clone()))
        })
        .ok_or("render state not initialised yet")?;

    let mut uploaded = Vec::with_capacity(meshes.len());
    let mut quads = 0usize;
    for section in meshes {
        quads += section.mesh.quad_count();
        if let Some(gpu) = GpuMesh::upload(&device, &section.mesh) {
            uploaded.push((gpu, section.origin));
        }
    }

    let atlas_pair = assets.map(|assets| {
        let atlas = GpuAtlas::from_rgba(
            &device,
            &queue,
            assets.atlas.width,
            assets.atlas.height,
            &assets.atlas.rgba,
            &[],
        );
        let uv = sprite_uv_buffer(&device, &assets.uv_rects);
        (atlas, uv)
    });

    RENDER_STATE.with_borrow_mut(|slot| {
        let state = slot.as_mut().ok_or("render state vanished")?;
        let sections = uploaded.len();
        state.meshes = uploaded;
        if let Some((atlas, uv)) = atlas_pair {
            state.atlas = atlas;
            state.uv = uv;
        }
        if let Some((eye, target)) = view {
            state.fly = FlyCamera::looking_at(eye, target);
        }
        Ok((sections, quads))
    })
}

/// Rebuilds the atlas and classifier for the block ids a *live* world actually
/// contains, tolerating blocks the trimmed pack has no assets for.
fn build_live_assets(pack_bytes: &[u8], ids: &[u32]) -> Result<terrain::TerrainAssets, String> {
    let source =
        ZipSource::from_bytes(pack_bytes.to_vec()).map_err(|error| format!("zip parse: {error}"))?;
    let manager = ResourceManager::new(vec![Box::new(source)]);
    terrain::build_terrain_assets_with(&manager, ids, true)
}

/// Joins a server through the relay and keeps the drawn scene in sync with the
/// world the server streams.
///
/// The scene is rebuilt by *querying* the client's chunk store rather than by
/// folding `ChunkLoaded` events, so it converges no matter when this loop starts
/// relative to the stream (see `multiplayer`'s module docs).
async fn run_multiplayer(target: multiplayer::JoinTarget, pack_bytes: Rc<Vec<u8>>) {
    if SESSION_LIVE.get() {
        set_line("world", "live world: a session is already running (reload to rejoin)");
        return;
    }
    SESSION_LIVE.set(true);

    set_line(
        "mp",
        &format!(
            "join: opening {} (advertising {}:{}) as {} …",
            target.relay_url, target.host, target.port, target.username
        ),
    );

    let (handle, mut events) = match multiplayer::join(&target).await {
        Ok(pair) => pair,
        Err(error) => {
            set_line("mp", &format!("join FAILED — {error}"));
            SESSION_LIVE.set(false);
            return;
        }
    };
    set_line(
        "mp",
        &format!("join: relay socket open — logging in as {} …", target.username),
    );

    // Draining the event stream is load-bearing, not bookkeeping: the channel is
    // bounded, and a full channel stalls the driver that fills it.
    spawn_local(async move {
        let mut keep_alives = 0usize;
        while let Some(event) = events.recv().await {
            match event {
                lodestone_client::ClientEvent::Login { entity_id, .. } => set_line(
                    "mp",
                    &format!("join: Play reached (entity id {entity_id}) — streaming world…"),
                ),
                lodestone_client::ClientEvent::KeepAlive { .. } => {
                    keep_alives += 1;
                    if keep_alives == 1 {
                        log::info!("[mp] first keep-alive — session is live");
                    }
                }
                lodestone_client::ClientEvent::Disconnect { reason } => set_line(
                    "mp",
                    &format!(
                        "join: server disconnected us — {}",
                        reason.to_plain_string()
                    ),
                ),
                _ => {}
            }
        }
        log::info!("[mp] event stream ended after {keep_alives} keep-alive(s)");
    });

    let mut assets: Option<terrain::TerrainAssets> = None;
    let mut asset_ids: Vec<u32> = Vec::new();
    let mut last_signature = (usize::MAX, usize::MAX);
    let mut seated = false;
    let start = now_ms();

    loop {
        gloo_timers::future::TimeoutFuture::new(LIVE_POLL_MS).await;

        if handle.is_finished() {
            set_line("world", "live world: session ended (see the join line)");
            SESSION_LIVE.set(false);
            return;
        }

        let centre = handle
            .position()
            .map(|pos| ChunkPos::new((pos.x.floor() as i32) >> 4, (pos.z.floor() as i32) >> 4))
            .unwrap_or_else(|| ChunkPos::new(0, 0));

        let Some(live) = multiplayer::collect_sections(&handle, centre, LIVE_VIEW_RADIUS) else {
            set_line(
                "world",
                &format!(
                    "live world: waiting for the first column ({:.0}s elapsed)…",
                    (now_ms() - start) / 1000.0
                ),
            );
            continue;
        };

        let signature = (live.columns, live.sections.len());
        if signature == last_signature {
            continue;
        }
        last_signature = signature;
        if live.sections.is_empty() {
            continue;
        }

        let ids = live.distinct_block_ids();
        if assets.is_none() || ids != asset_ids {
            match build_live_assets(&pack_bytes, &ids) {
                Ok(built) => {
                    asset_ids = ids;
                    assets = Some(built);
                }
                Err(error) => {
                    set_line("world", &format!("live world: asset build failed — {error}"));
                    continue;
                }
            }
        }
        let assets = assets.as_ref().expect("assets built above");

        let (meshes, min, max) = terrain::mesh_live_sections(
            &live.sections,
            live.min_y,
            live.section_count,
            &assets.classifier,
        );

        // Seat the camera once, near the *player*, not at the centre of the whole
        // streamed region: a 9×9-column AABB is ~400 blocks tall, so an AABB-derived
        // orbit puts the eye far outside the world and frames its dark underside.
        // A short third-person offset is what makes the first frame look like a
        // game rather than a distant cube.
        let view = if seated {
            None
        } else {
            match handle.position() {
                Some(pos) => {
                    let target = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                    Some((target + Vec3::new(14.0, 12.0, 14.0), target))
                }
                None => {
                    // No position yet: fall back to the meshed region's centre.
                    let centre = Vec3::new(
                        (min[0] + max[0]) * 0.5,
                        (min[1] + max[1]) * 0.5,
                        (min[2] + max[2]) * 0.5,
                    );
                    Some((centre + Vec3::new(40.0, 40.0, 40.0), centre))
                }
            }
        };
        match install_scene(&meshes, Some(assets), view) {
            Ok((sections, quads)) => {
                seated = true;
                set_line(
                    "world",
                    &format!(
                        "LIVE world from {}:{} — {} of {} columns, {sections} sections, {quads} greedy quads | player chunk ({}, {}) | {}",
                        target.host,
                        target.port,
                        live.columns,
                        live.loaded_columns,
                        centre.x,
                        centre.z,
                        assets.summary,
                    ),
                );
            }
            Err(error) => set_line("world", &format!("live world: install failed — {error}")),
        }
    }
}

/// Reads a join target from the page's query string
/// (`?relay=…&host=…&port=…&name=…&join=1`), falling back to the defaults.
///
/// Returns the target plus whether `join` was present, i.e. whether to join
/// without waiting for a click — which is what lets a harness drive a join with
/// no UI interaction.
fn target_from_query() -> (multiplayer::JoinTarget, bool) {
    let mut target = multiplayer::JoinTarget::default_target();
    let mut auto_join = false;
    if let Some(win) = window()
        && let Ok(search) = win.location().search()
        && let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search)
    {
        if let Some(value) = params.get("relay").filter(|v| !v.is_empty()) {
            target.relay_url = value;
        }
        if let Some(value) = params.get("host").filter(|v| !v.is_empty()) {
            target.host = value;
        }
        if let Some(value) = params.get("port").and_then(|v| v.parse::<u16>().ok()) {
            target.port = value;
        }
        if let Some(value) = params.get("name").filter(|v| !v.is_empty()) {
            target.username = value;
        }
        auto_join = params.get("join").is_some();
    }
    (target, auto_join)
}

/// Reads one text input by id, or `None` when it is absent or blank.
fn input_value(id: &str) -> Option<String> {
    window()?
        .document()?
        .get_element_by_id(id)?
        .dyn_into::<web_sys::HtmlInputElement>()
        .ok()
        .map(|input| input.value().trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Writes one text input by id, ignoring a missing element.
fn set_input_value(id: &str, value: &str) {
    if let Some(input) = window()
        .and_then(|w| w.document())
        .and_then(|doc| doc.get_element_by_id(id))
        .and_then(|el| el.dyn_into::<web_sys::HtmlInputElement>().ok())
    {
        input.set_value(value);
    }
}

/// Prefills the join form, wires its button, and honours `?join=1`.
fn install_join_ui(pack_bytes: Rc<Vec<u8>>) {
    let (target, auto_join) = target_from_query();

    set_input_value("relay", &target.relay_url);
    set_input_value("host", &target.host);
    set_input_value("port", &target.port.to_string());
    set_input_value("name", &target.username);
    set_line(
        "mp",
        "join: enter a relay URL and press Join (or load with ?join=1 to join on start)",
    );

    if let Some(button) = window()
        .and_then(|w| w.document())
        .and_then(|doc| doc.get_element_by_id("join"))
        .and_then(|el| el.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let pack = pack_bytes.clone();
        let closure = Closure::<dyn FnMut()>::new(move || {
            // Re-read the form on every click: the point of the inputs is that the
            // target is not baked in at startup.
            let mut target = multiplayer::JoinTarget::default_target();
            if let Some(value) = input_value("relay") {
                target.relay_url = value;
            }
            if let Some(value) = input_value("host") {
                target.host = value;
            }
            if let Some(value) = input_value("port").and_then(|v| v.parse::<u16>().ok()) {
                target.port = value;
            }
            if let Some(value) = input_value("name") {
                target.username = value;
            }
            spawn_local(run_multiplayer(target, pack.clone()));
        });
        button.set_onclick(Some(closure.as_ref().unchecked_ref()));
        // The button outlives this function, so the closure must too.
        closure.forget();
    }

    if auto_join {
        spawn_local(run_multiplayer(target, pack_bytes));
    }
}

/// `performance.now()` in milliseconds, or 0 where it is unavailable.
fn now_ms() -> f64 {
    window()
        .and_then(|w| w.performance())
        .map(|perf| perf.now())
        .unwrap_or(0.0)
}

async fn run() {
    let win = window().expect("no window");
    let doc = win.document().expect("no document");
    let canvas: HtmlCanvasElement = doc
        .get_element_by_id("scene")
        .expect("missing #scene canvas")
        .dyn_into()
        .expect("#scene is not a canvas");
    canvas.set_width(WIDTH);
    canvas.set_height(HEIGHT);
    // Keep a handle for the input layer before the canvas is moved into wgpu's
    // surface (cheap: `HtmlCanvasElement` is a reference-counted JS handle).
    let input_canvas = canvas.clone();

    // Browser singleplayer probe (W-next item 1), spawned concurrently with the
    // render path. Fetches the worldgen data, then runs the REAL integrated
    // server ↔ REAL client in-process over an in-memory duplex — first time the
    // client/server/worldgen stack runs in a browser. Reports to its own HUD
    // line. NOTE: worldgen column generation is synchronous and single-threaded,
    // so it briefly blocks the event loop while it runs (a real UX finding,
    // surfaced as the timing number).
    //
    // Skipped when the page was loaded with `?join=1`. In-browser worldgen is
    // synchronous and single-threaded, so it blocks the event loop for seconds at
    // a time — which starves the live session's WebSocket and can get us timed
    // out by the server. The two probes compete for the one thread; a live join
    // wins.
    let skip_singleplayer = target_from_query().1;
    if skip_singleplayer {
        set_line(
            "singleplayer",
            "singleplayer: skipped (?join=1) — synchronous worldgen would stall the live session",
        );
    }
    spawn_local(async move {
        if skip_singleplayer {
            return;
        }
        set_line("singleplayer", "singleplayer: fetching worldgen data…");
        let bytes = match fetch_bytes(WORLDGEN_URL).await {
            Ok(b) => b,
            Err(e) => {
                set_line("singleplayer", &format!("singleplayer: worldgen fetch FAILED: {e}"));
                return;
            }
        };
        set_line(
            "singleplayer",
            &format!("singleplayer: worldgen {} B fetched — running server↔client…", bytes.len()),
        );
        match singleplayer::run_singleplayer(&bytes).await {
            Ok(r) => {
                let sample = match r.sample {
                    Some((x, y, z, id)) => format!("block({x},{y},{z})=id{id}"),
                    None => "no solid block sampled".into(),
                };
                set_line(
                    "singleplayer",
                    &format!(
                        "singleplayer OK: Play reached, {} chunk(s) via in-memory transport | \
                         worldgen {:.0} ms/chunk (1 thread) | {} | {}/{} sampled solid",
                        r.chunks,
                        r.worldgen_ms,
                        sample,
                        r.solid_sampled,
                        r.checked_sampled,
                    ),
                );
                let _ = r.play_reached;
            }
            Err(e) => set_line("singleplayer", &format!("singleplayer FAILED: {e}")),
        }
    });

    // WebGPU only. The WebGL2 fallback was dropped (W-next): it added 537 KB
    // brotli and panicked before frame 0 because our atlas bind group layout
    // needs a vertex-stage storage buffer WebGL2 lacks (`VERTEX_STORAGE`). A
    // browser without WebGPU therefore gets a clear message below, not a blank
    // canvas.
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::BROWSER_WEBGPU;
    let instance = wgpu::util::new_instance_with_webgpu_detection(desc).await;

    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .expect("create surface from canvas");

    let ctx = match GpuContext::new_for_surface(instance, &surface).await {
        Ok(c) => c,
        Err(e) => {
            set_line(
                "status",
                &format!(
                    "GPU init failed — this build requires WebGPU (no WebGL2 fallback; \
                     enable WebGPU or use Chrome/Edge/Safari/Firefox with it on): {e}"
                ),
            );
            return;
        }
    };

    let info = ctx.adapter().get_info();
    let backend = format!("{:?}", info.backend);
    let adapter_label = if info.name.trim().is_empty() {
        String::new()
    } else {
        format!(" | adapter: {}", info.name)
    };
    // The *probe's* opinion from `select_strategy`, not what this app draws with
    // (it uses a plain `draw_indexed` path). It will read `PerDraw` once the
    // capability-probe bug is fixed in `lodestone-render`.
    let selected = lodestone_render::select_strategy(ctx.capabilities());
    set_line(
        "status",
        &format!("rendering — backend: {backend}{adapter_label} | select_strategy(): {selected:?}"),
    );

    let target = SurfaceTarget::new(surface, ctx.adapter(), ctx.device(), WIDTH, HEIGHT)
        .expect("surface incompatible with adapter");

    let device = ctx.device();
    let queue = ctx.queue();

    // Load the real terrain path end to end. If any step fails, report the
    // honest wall and stop — we deliberately draw **no** fake geometry, so a
    // failure can never be mistaken for the milestone.
    let loaded = match load_terrain().await {
        Ok(l) => l,
        Err(e) => {
            set_line("assets", &format!("terrain load FAILED: {e}"));
            set_line(
                "status",
                "terrain load failed — nothing drawn (no synthetic fallback; see report)",
            );
            return;
        }
    };

    set_line(
        "assets",
        &format!(
            "{} | pack {} B, fixture {} B, {} chunks fetched",
            loaded.assets.summary,
            loaded.pack_len,
            loaded.fixture_len,
            loaded.chunks.len(),
        ),
    );

    // Mesh every non-empty section of every fixture chunk.
    let (section_meshes, min, max) = terrain::mesh_chunks(&loaded.chunks, &loaded.assets.classifier);
    let total_quads: usize = section_meshes.iter().map(|m| m.mesh.quad_count()).sum();
    let mut meshes = Vec::with_capacity(section_meshes.len());
    for sm in &section_meshes {
        if let Some(gpu) = GpuMesh::upload(device, &sm.mesh) {
            meshes.push((gpu, sm.origin));
        }
    }
    let chunk_count = loaded.chunks.len();
    let section_count = meshes.len();

    let centre = Vec3::new(
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    );
    let extent = Vec3::new(max[0] - min[0], max[1] - min[1], max[2] - min[2]);
    let orbit_radius = (extent.length() * 0.9).max(24.0);
    // Start where the old auto-orbit sat — above and to the side, looking at the
    // slab — but now you can fly away from it.
    let start_pos = centre + Vec3::new(orbit_radius * 0.7, orbit_radius * 0.6, orbit_radius * 0.7);
    let fly = FlyCamera::looking_at(start_pos, centre);
    let controls = input::install(&input_canvas, &doc, |s| set_line("controls", s));
    set_line(
        "controls",
        "click the scene to look around · WASD move · Shift/Space down/up · Ctrl boost",
    );

    set_line(
        "status",
        &format!(
            "REAL terrain from real server bytes — {chunk_count} chunks, {section_count} sections, {total_quads} greedy quads | backend: {backend} | select_strategy(): {}",
            format_args!("{:?}", lodestone_render::select_strategy(ctx.capabilities())),
        ),
    );

    let atlas = GpuAtlas::from_rgba(
        device,
        queue,
        loaded.assets.atlas.width,
        loaded.assets.atlas.height,
        &loaded.assets.atlas.rgba,
        &[],
    );
    let uv = sprite_uv_buffer(device, &loaded.assets.uv_rects);

    if meshes.is_empty() {
        set_line(
            "status",
            "decoded real chunks but produced 0 geometry — see report (not drawing a fake)",
        );
        return;
    }

    let pipeline = BlockPipeline::new(device, target.format());
    let depth = DepthBuffer::new(device, WIDTH, HEIGHT);

    let clock = PerfClock::new();
    if clock.is_none() {
        set_line("frame", "clock: performance.now() unavailable");
    }

    // Not `mut`: the state is mutated through `RENDER_STATE` now, not here.
    let state = State {
        ctx,
        target,
        pipeline,
        atlas,
        uv,
        meshes,
        fly,
        controls,
        chunk_count,
        depth,
        frame: 0,
        clock,
        pacer: FramePacer::new(60),
        ema_ms: 0.0,
        last_dt: 1.0 / 60.0,
    };
    set_line(
        "frame",
        &format!(
            "clock: performance.now() via FramePacer | {} chunks / {} sections meshed",
            state.chunk_count, section_count,
        ),
    );

    // Kick off the live networking probe concurrently; it must not block render.
    set_line("net", &format!("relay probe: connecting to {RELAY_URL} …"));
    spawn_local(async move {
        match ping_via_relay(RELAY_URL).await {
            Ok(json) => set_line(
                "net",
                &format!(
                    "relay probe OK — browser WebSocket → relay → live server. {}",
                    summarise_status(&json)
                ),
            ),
            Err(e) => set_line(
                "net",
                &format!(
                    "⚠ relay UNREACHABLE — {e}. No relay is listening on {RELAY_URL}; \
                     start `lodestone-relay` to exercise browser → relay → live server. \
                     (Render and singleplayer are unaffected — this path is isolated.)"
                ),
            ),
        }
    });

    // Park the state where `lodestone_render_frames` can reach it, then drive it
    // from `requestAnimationFrame` as before. The closure no longer *owns* the
    // state — that is what makes the render path measurable from a harness (see
    // `RENDER_STATE`).
    RENDER_STATE.set(Some(state));

    // The live multiplayer join. It is wired *after* the state is parked because
    // it swaps the drawn scene for the served world, which needs a scene to swap.
    install_join_ui(loaded.pack_bytes.clone());

    // Standard wasm-bindgen requestAnimationFrame loop.
    let cb = Rc::new(RefCell::new(None));
    let cb2 = cb.clone();
    *cb2.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        RENDER_STATE.with_borrow_mut(|slot| {
            if let Some(state) = slot.as_mut() {
                state.render(true);
            }
        });
        request_animation_frame(cb.borrow().as_ref().unwrap());
    }) as Box<dyn FnMut()>));
    request_animation_frame(cb2.borrow().as_ref().unwrap());
}

fn request_animation_frame(f: &Closure<dyn FnMut()>) {
    window()
        .expect("no window")
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("requestAnimationFrame failed");
}

fn main() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    spawn_local(run());
}
