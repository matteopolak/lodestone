//! Missing optional pack art must not disable unrelated production draws.

use std::path::PathBuf;

use lodestone::gpu::{RenderState, SkyClock, RenderStats, ScreenEffects};
use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};
use lodestone_render::fog::FogSettings;
use lodestone_render::{
    Camera, CloudStatus, GpuContext, HeadlessTarget, RenderTarget,
    ScreenEffectRenderer, SkyRenderer,
};

const W: u32 = 256;
const H: u32 = 256;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const CLOUDS: &str = "assets/minecraft/textures/environment/clouds.png";
const NAUSEA: &str = "assets/minecraft/textures/misc/nausea.png";
const FIRE: &str = "assets/minecraft/textures/block/fire_1.png";
const SUN: &str = "assets/minecraft/textures/environment/celestial/sun.png";
const WATER: &str = "assets/minecraft/textures/misc/underwater.png";
type Rect = (u32, u32, u32, u32);

#[derive(Debug)]
struct WithheldSource {
    source: ZipSource,
    paths: Vec<&'static str>,
}

impl ResourceSource for WithheldSource {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if self.paths.contains(&path) { None } else { self.source.read(path) }
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        self.source.list(prefix).into_iter()
            .filter(|path| !self.paths.contains(&path.as_str()))
            .collect()
    }
}

fn manager(source: &ZipSource, extra: Option<&'static str>) -> ResourceManager {
    let mut paths = vec![CLOUDS, NAUSEA];
    if let Some(path) = extra {
        paths.push(path);
    }
    ResourceManager::new(vec![Box::new(WithheldSource { source: source.clone(), paths })])
}

fn state(ctx: &GpuContext, clear: [f32; 3]) -> RenderState {
    let mut state = RenderState::new(ctx.device(), ctx.queue(), FORMAT, W, H, None);
    state.set_fog(FogSettings::disabled(), 16);
    state.set_clear_color(clear);
    state.set_entity_light_source(|_| Some(0xFF));
    state.set_time_of_day_source(|| Some(SkyClock::at_time_of_day(6_000)));
    state.set_cloud_status(CloudStatus::Fancy);
    state
}

fn shoot(
    ctx: &GpuContext,
    target: &mut HeadlessTarget,
    state: &RenderState,
    camera: &Camera,
    effects: ScreenEffects,
) -> (Vec<u8>, RenderStats) {
    let frame = target.acquire().expect("headless target");
    let stats = state.render_with_effects(
        ctx.device(), ctx.queue(), frame.view(), camera, None, &[], effects,
    );
    (target.read_texels(ctx.device(), ctx.queue()), stats)
}

fn survey(pixels: &[u8], rect: Rect, matches: impl Fn([u8; 3], usize) -> bool) -> (u32, Option<Rect>) {
    let (mut count, mut bounds) = (0u32, None::<Rect>);
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let i = ((y * W + x) * 4) as usize;
            if matches([pixels[i], pixels[i + 1], pixels[i + 2]], i) {
                count += 1;
                bounds = Some(match bounds {
                    None => (x, y, x + 1, y + 1),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                });
            }
        }
    }
    (count, bounds)
}

fn difference(pixels: &[u8], baseline: &[u8], rect: Rect) -> (u32, Option<Rect>) {
    survey(pixels, rect, |pixel, i| {
        (0..3).map(|c| pixel[c].abs_diff(baseline[i + c]) as u32).sum::<u32>() > 8
    })
}

// At noon the sun's corners are (±30, 100, ±30). Project them independently
// onto an 80-degree upward camera with a 60-degree vertical field of view.
fn sun_rect() -> Rect {
    let (s, c) = 80f32.to_radians().sin_cos();
    let half_fov = 30f32.to_radians().tan();
    let (mut x0, mut y0, mut x1, mut y1) = (W as f32, H as f32, 0f32, 0f32);
    for x in [-30f32, 30.0] {
        for z in [-30f32, 30.0] {
            let depth = 100.0 * s + z * c;
            let px = W as f32 * 0.5 * (1.0 + x / (depth * half_fov));
            let py = H as f32 * 0.5 * (1.0 - (100.0 * c - z * s) / (depth * half_fov));
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
    }
    (x0.floor() as u32, y0.floor() as u32, x1.ceil() as u32, y1.ceil() as u32)
}

fn linear(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[test]
#[ignore = "requires a GPU adapter and the staged built-in resource archive"]
fn absent_clouds_and_nausea_preserve_sun_water_and_fire_pixels() {
    let root = std::env::var_os("LODESTONE_ASSETS").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/mc/26.2")
    });
    let source = ZipSource::open(&root.join("lodestone-resources.zip")).expect("staged resource archive");
    assert!(source.read(SUN).is_some() && source.read(FIRE).is_some());
    let resources = manager(&source, None);
    assert!(resources.read(CLOUDS).is_none() && resources.read(NAUSEA).is_none());
    let ctx = GpuContext::new_headless_blocking().expect("GPU gate requires an adapter");
    let mut target = HeadlessTarget::new(ctx.device(), W, H, FORMAT);
    let mut camera = Camera {
        position: glam::Vec3::new(0.0, 70.0, 0.0),
        yaw: 0.0, pitch: -80.0, fov_y_degrees: 60.0, aspect: 1.0,
        near: 0.05, far: 1024.0,
    };

    let mut sky_state = state(&ctx, [0.0; 3]);
    let (no_sky, _) = shoot(&ctx, &mut target, &sky_state, &camera, ScreenEffects::default());
    let sky = SkyRenderer::new(ctx.device(), ctx.queue(), FORMAT, &resources).expect("sky without clouds");
    assert!(!sky.has_clouds());
    sky_state.install_sky(sky);
    let (sun, stats) = shoot(&ctx, &mut target, &sky_state, &camera, ScreenEffects::default());
    let rect = sun_rect();
    let sun_pixels = difference(&sun, &no_sky, rect);
    eprintln!("sun ROI {rect:?}: {sun_pixels:?}");
    assert!(stats.sky_drawn && sun_pixels.0 > 256, "sun not visible in projected ROI: {sun_pixels:?}");
    assert_eq!(difference(&sun, &no_sky, (0, 0, 48, 16)).0, 0, "sun paint outside projected footprint");

    camera.pitch = 0.0;
    let mut effects_state = state(&ctx, [1.0; 3]);
    let fx = ScreenEffectRenderer::new(ctx.device(), ctx.queue(), FORMAT, &resources).expect("overlays without nausea");
    assert!(!fx.has_confusion_overlay());
    effects_state.install_screen_effects(fx);
    let (dry, _) = shoot(&ctx, &mut target, &effects_state, &camera, ScreenEffects::default());
    let water = Image::decode_png(&source.read(WATER).expect("underwater art")).expect("decode art");
    let texel = &water.rgba[..4];
    assert!(water.rgba.chunks_exact(4).all(|pixel| pixel == texel), "water colour oracle requires the built-in uniform sheet");
    // Fullbright tint is 1.0; source alpha times 0.1 blends decoded linear
    // RGB over a white linear target. No geometry or blend helper is reused.
    let alpha = f32::from(texel[3]) / 255.0 * 0.1;
    let expected: [u8; 3] = std::array::from_fn(|c| {
        ((linear(texel[c]) * alpha + 1.0 - alpha) * 255.0).round() as u8
    });
    let water_rect = (16, 16, 112, 112);
    assert_eq!(survey(&dry, water_rect, |pixel, _| pixel == [255; 3]).0, 96 * 96);
    let (wet, wet_stats) = shoot(&ctx, &mut target, &effects_state, &camera, ScreenEffects {
        eye_in_water: true, ..ScreenEffects::default()
    });
    let water_pixels = survey(&wet, water_rect, |pixel, _| {
        (0..3).all(|c| pixel[c].abs_diff(expected[c]) <= 2)
    });
    eprintln!("water ROI {water_rect:?}, expected {expected:?}: {water_pixels:?}");
    assert!(wet_stats.underwater_overlay_drawn && water_pixels.0 >= 9_120, "wrong water blend: {water_pixels:?}");

    // Unit-square quads: ±0.24 horizontal offset, -0.3 vertical offset,
    // 10-degree tilt. The scale fits their combined width into [-1, 1].
    let (s, c) = 10f32.to_radians().sin_cos();
    let scale = 1.0 / (0.24 + 0.5 * s + 0.5 * c);
    let top = ((1.0 - 0.2 * scale) * H as f32 * 0.5).floor() as u32;
    let bottom = ((1.0 + 0.8 * scale) * H as f32 * 0.5).ceil().min(H as f32) as u32;
    let fire_rect = (16, top + 8, 112, bottom - 8);
    let burning = ScreenEffects { on_fire: true, ..ScreenEffects::default() };
    let (fire, fire_stats) = shoot(&ctx, &mut target, &effects_state, &camera, burning);
    let fire_pixels = difference(&fire, &dry, fire_rect);
    eprintln!("fire ROI {fire_rect:?}, predicted rows {top}..{bottom}: {fire_pixels:?}");
    assert!(fire_stats.fire_overlay_drawn && fire_pixels.0 > 256, "fire not visible: {fire_pixels:?}");
    assert_eq!(difference(&fire, &dry, (16, 0, 112, top.saturating_sub(2))).0, 0);

    let (confused, confused_stats) = shoot(&ctx, &mut target, &effects_state, &camera, ScreenEffects {
        nausea_intensity: 1.0, ..ScreenEffects::default()
    });
    assert!(!confused_stats.confusion_overlay_drawn);
    assert_eq!(confused, dry, "unavailable confusion pass must leave the target unchanged");

    let no_fire = manager(&source, Some(FIRE));
    assert!(no_fire.read(FIRE).is_none() && no_fire.read(SUN).is_some());
    assert!(ScreenEffectRenderer::new(ctx.device(), ctx.queue(), FORMAT, &no_fire).is_err());
    let control_state = state(&ctx, [1.0; 3]);
    let (control, control_stats) = shoot(&ctx, &mut target, &control_state, &camera, burning);
    let control_pixels = difference(&control, &dry, fire_rect);
    eprintln!("withheld mandatory fire control: {control_pixels:?}");
    assert!(!control_stats.fire_overlay_drawn && control_pixels.0 == 0,
        "withheld-resource control did not fail the same fire detector: {control_pixels:?}");
}
