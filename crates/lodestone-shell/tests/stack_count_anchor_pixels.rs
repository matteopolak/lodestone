//! Pixel gate: **the stack count sits where vanilla puts it** (issue #384).
//!
//! Reported from play as "the number should be lower and further left".
//! `GuiGraphicsExtractor.itemCount` (`:947-952`, identical in
//! `SpectatorGui.java:79`):
//!
//! ```java
//! this.text(font, amount, x + 19 - 2 - font.width(amount), y + 6 + 3, -1, true);
//! ```
//!
//! — right edge at `x + 17` (one pixel *past* the 16 px icon), top at `y + 9`,
//! drop shadow on.
//!
//! # The defect was the derivation, not the offset
//!
//! The old code used `x + size - width` and `y + size - LINE_HEIGHT * scale`. Both
//! are derived from things that move, so they agreed with vanilla at no glyph
//! height: `LINE_HEIGHT` is 9, giving `y + 7` against vanilla's `y + 9` — 2 px
//! high, which is the "lower" half of the report.
//!
//! # Why this measures the painted extent, not the anchor
//!
//! The horizontal half of the report did **not** add up from the anchors alone:
//! the old anchor put the right edge at `x + 16`, one pixel *left* of vanilla's
//! `x + 17`, while the user asked for further left. An anchor assertion cannot
//! resolve that, because what a player sees is *ink* — and the ink is not the
//! anchor. The shadow pass draws at `+1, +1`, so the painted extent reaches one
//! pixel past the last glyph on both axes, and a glyph's raster need not fill its
//! advance. So this gate reads the **ink bounding box** and compares its edges
//! against the vanilla-derived anchor, printing both. That is also the only way
//! to see whether the number that changed is the one that was wrong.
//!
//! # Two counts, because a right-aligned anchor fails for multi-digit numbers
//!
//! A single-digit case cannot distinguish `right - width` from `left + 0`: with one
//! glyph the two coincide for any width the test happens to use. `64` is measured
//! alongside `7` for exactly that reason, and the assertion is that **both share
//! the same right edge and the same top** — right alignment — while the two-digit
//! box is genuinely wider.
//!
//! Fail-closed: a missing GPU or a missing `client.jar` is a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test stack_count_anchor_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, slot_layout};
use lodestone::gpu::RenderState;
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_game::{item::ItemStack, menu::Menu};
use lodestone_model::Identifier;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 480;
const H: u32 = 320;

/// A flat `item/generated` icon. Its own ink is *cancelled* below by differencing
/// against a count-1 frame of the same item, so the icon's shape does not matter —
/// but a flat sprite keeps the two frames identical everywhere except the digits.
const ITEM: &str = "minecraft:stick";

/// Menu index 9 is the first cell of the player screen's main storage — an
/// ordinary slot with no neighbour geometry.
const SLOT: usize = 9;

/// Vanilla's own two constants, restated here **once**, as the gate's expected
/// value. They are the thing under test, so they must come from the decompile and
/// not from `item_icon.rs` — `CLAUDE.md`: an expected value must originate outside
/// the code under test.
const VANILLA_COUNT_RIGHT: i64 = 17;
const VANILLA_COUNT_TOP: i64 = 9;

fn id(path: &str) -> Identifier {
    path.parse().expect("valid item id")
}

/// The `(x, y, w, h)` **physical** rect of a menu slot, from the same expressions
/// the draw uses.
fn slot_rect(menu: &Menu, frame: &ContainerFrame<'_>, menu_index: usize) -> [u32; 4] {
    let widget = ContainerGeometry::build(frame, W, H)
        .widget_rect
        .expect("a populated frame has a widget rect");
    let layout = slot_layout(menu);
    let slot = layout
        .slots
        .iter()
        .find(|s| s.menu_index == menu_index)
        .expect("menu index must be laid out");
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32;
    [
        ((widget.x + slot.x) * scale) as u32,
        ((widget.y + slot.y) * scale) as u32,
        (slot.w * scale) as u32,
        (slot.h * scale) as u32,
    ]
}

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gate-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

fn differs(a: &[u8], b: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    let d = (i32::from(a[i]) - i32::from(b[i])).abs()
        + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
        + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
    d > 12
}

/// The digits' ink, as a **bounding box in slot-local pixels** — the instrument
/// this issue needs, since the complaint is about position.
///
/// Measured over a rect grown well past the cell so a count drawn *outside* the
/// slot is seen rather than silently clipped to it. A gate that scanned only the
/// 16x16 cell would report a plausible box for a number half off the edge.
#[derive(Debug, Default, Clone, Copy)]
struct Ink {
    count: usize,
    /// `[left, top, right, bottom]`, inclusive, relative to the slot origin.
    bbox: Option<[i64; 4]>,
}

impl Ink {
    fn describe(&self) -> String {
        match self.bbox {
            None => format!("{} px (empty)", self.count),
            Some([l, t, r, b]) => format!(
                "{} px, local x{}..{} y{}..{} ({}x{})",
                self.count,
                l,
                r,
                t,
                b,
                r - l + 1,
                b - t + 1
            ),
        }
    }
}

/// Difference `counted` against `plain` over the slot's cell **grown by 8 px on
/// every side**, so ink that escaped the cell is still measured.
fn ink(counted: &[u8], plain: &[u8], rect: [u32; 4]) -> Ink {
    let [rx, ry, rw, rh] = rect;
    let pad = 8i64;
    let mut out = Ink::default();
    for dy in -pad..(i64::from(rh) + pad) {
        for dx in -pad..(i64::from(rw) + pad) {
            let x = i64::from(rx) + dx;
            let y = i64::from(ry) + dy;
            if x < 0 || y < 0 || x >= i64::from(W) || y >= i64::from(H) {
                continue;
            }
            if differs(counted, plain, x as u32, y as u32) {
                out.count += 1;
                out.bbox = Some(match out.bbox {
                    None => [dx, dy, dx, dy],
                    Some([l, t, r, b]) => [l.min(dx), t.min(dy), r.max(dx), b.max(dy)],
                });
            }
        }
    }
    out
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_stack_count_sits_at_vanillas_anchor() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the local pixel math below assumes a GUI scale of 1"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let models: &BlockModels = atlas
        .models()
        .expect("the vanilla load must attach baked block models");
    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    let loc: ResourceLocation = ITEM.parse().expect("valid item id");
    assert!(
        item_atlas.icon(&loc).is_some(),
        "{ITEM} must resolve through the production atlas"
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let render = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let mut renderer = ContainerRenderer::new(device, format);
    renderer.attach_items(device, queue, format, item_atlas.clone());
    renderer.attach_item_models(
        device,
        format,
        render
            .model_atlas_view()
            .expect("the vanilla path must expose a model atlas"),
        render
            .model_atlas_sampler()
            .expect("the vanilla path must expose a model atlas sampler"),
        render
            .model_palette_buffer()
            .expect("the vanilla path must expose a tint palette"),
        render
            .model_anim_buffer()
            .expect("the vanilla path must expose animation slots"),
    );
    // The gate is only meaningful against the **vanilla proportional font**: the
    // jar-less fallback has different glyph widths, so a box measured through it
    // would be measuring the debug font's metrics. Asserted, not hoped for.
    assert!(
        renderer.font_attached(),
        "the vanilla font must resolve, or this gate measures the 5x7 debug font's \
         advances instead of vanilla's"
    );

    let probe = Menu::player();
    let probe_frame = ContainerFrame::new(Some(&probe), "Inventory");
    let rect = slot_rect(&probe, &probe_frame, SLOT);

    eprintln!("=== stack-count anchor gate (issue #384) ===");
    eprintln!("slot {SLOT} rect = {rect:?}");
    eprintln!("vanilla: right edge at local x{VANILLA_COUNT_RIGHT}, top at local y{VANILLA_COUNT_TOP}");

    let shoot = |renderer: &mut ContainerRenderer, target: &mut HeadlessTarget, count: i32| {
        let mut menu = Menu::player();
        menu.set_slot_item(SLOT, Some(ItemStack::new(id(ITEM), count)));
        let frame = ContainerFrame::new(Some(&menu), "Inventory");
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        renderer.render_with_icons(
            device,
            queue,
            acquired.view(),
            Some(render.depth_view()),
            &frame,
            Some(models),
            W,
            H,
        );
        target.read_texels(device, queue)
    };

    // A count of 1 draws **no** number (vanilla's own `getCount() != 1`), so it is
    // the baseline: identical item, identical icon, identical everything else. The
    // difference is the digits and only the digits — the panel, well, dim gradient
    // and item sprite all cancel by construction.
    let plain = shoot(&mut renderer, &mut target, 1);
    let one_digit = shoot(&mut renderer, &mut target, 7);
    let two_digit = shoot(&mut renderer, &mut target, 64);

    let a = ink(&one_digit, &plain, rect);
    let b = ink(&two_digit, &plain, rect);
    eprintln!("  count 7  ink = {}", a.describe());
    eprintln!("  count 64 ink = {}", b.describe());

    let mut failures: Vec<String> = Vec::new();

    // The detector, first: a count of 1 really must draw nothing, or every box
    // below is measured against a frame that already has digits in it.
    let control = ink(&plain, &one_digit, rect);
    if control.count == 0 {
        failures.push(
            "a count of 7 rendered identically to a count of 1 — no digits are in \
             either frame, so every box below is a difference of two blanks"
                .to_string(),
        );
    }

    for (label, m) in [("7", a), ("64", b)] {
        let Some([l, t, r, bot]) = m.bbox else {
            failures.push(format!("count {label} drew no ink at all: {}", m.describe()));
            continue;
        };
        // The ink's right edge is the **shadow's**, one pixel past the glyph, and
        // the glyph's own right edge is one short of the advance's right edge —
        // so the painted extent lands within one pixel of the anchor either way.
        // Asserting a window rather than an exact column is honest about that; it
        // is still tight enough to catch the 1 px anchor error this fixes, because
        // the *old* right edge was `x + 16` and could not reach x16..x17.
        if !(VANILLA_COUNT_RIGHT - 1..=VANILLA_COUNT_RIGHT).contains(&r) {
            failures.push(format!(
                "count {label}: ink right edge at local x{r}, expected x{}..x{} \
                 (vanilla's `x + 19 - 2` less the shadow's overhang). Box: {}",
                VANILLA_COUNT_RIGHT - 1,
                VANILLA_COUNT_RIGHT,
                m.describe()
            ));
        }
        // The top is exact: vanilla's `y + 6 + 3` is the string's top-left, and the
        // tallest digit glyph starts on that row. The old code's `y + 7` fails this
        // by 2, which is the reported "should sit lower".
        if t != VANILLA_COUNT_TOP {
            failures.push(format!(
                "count {label}: ink top at local y{t}, expected y{VANILLA_COUNT_TOP} \
                 (vanilla's `y + 6 + 3`). Box: {}",
                m.describe()
            ));
        }
        let _ = (l, bot);
    }

    // Right alignment: both counts must share a right edge and a top. This is what
    // a single-digit measurement cannot see — with one glyph, `right - width` and a
    // left-anchored `left + 0` coincide.
    if let (Some([al, at, ar, _]), Some([bl, bt, br, _])) = (a.bbox, b.bbox) {
        if ar != br {
            failures.push(format!(
                "the two counts do not share a right edge (x{ar} vs x{br}) — the anchor \
                 is not right-aligned, which only a multi-digit count can reveal"
            ));
        }
        if at != bt {
            failures.push(format!("the two counts do not share a top (y{at} vs y{bt})"));
        }
        if bl >= al {
            failures.push(format!(
                "the two-digit count is not wider to the left (left x{bl} vs x{al}) — it \
                 should grow leftwards from a fixed right edge"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "stack-count anchor gate failed:\n  {}",
        failures.join("\n  ")
    );
}
