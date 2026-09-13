//! Pixel gate: **the drag preview reaches pixels, only in painted cells, and the
//! number it draws is the split the release will produce** (part 2).
//!
//! While a paint-drag is held, vanilla shows the provisional result in each
//! painted cell — a 50%-white wash and the stack that cell would receive. This
//! client computed `quick_craft_slots` and drew nothing with it.
//!
//! # Why "a number drew in each painted cell" is not the assertion
//!
//! That passes on a *wrong* split, which is the failure that matters: a preview
//! disagreeing with the outcome is worse than no preview. The arithmetic half of
//! the proof is hermetic and lives in `lodestone-game`'s
//! `tests/drag_preview_agreement.rs`, which compares the plan against a real
//! `perform_drag`. What cannot be proved there is that the number on *screen* is
//! that number, so this gate carries two assertions a coverage check does not:
//!
//! * **Position.** An unpainted cell must be pixel-identical to the no-drag
//!   frame. Without it, a preview nailed to every cell — or to cell 0 — passes.
//!   This is the same discriminator the hover highlight needs, and the reason
//!   `CLAUDE.md` calls out the *magnitude* species: measuring *whether* something
//!   changed cannot see *what* changed.
//! * **The digit tracks the split, by two independent routes.** A cursor of 9
//!   over three cells (3 each) and a cursor of 7 over three cells (2 each) must
//!   **differ** inside a painted cell — the count changed. And 7 over three cells
//!   and 4 over two cells, which are both 2 each, must be **pixel-identical**
//!   inside the cell they share. That second one is what a wrong formula breaks
//!   while still drawing a number: `ceil` instead of `floor` makes 7/3 into 3
//!   while 4/2 stays 2, so the two frames stop matching.
//!
//! # The four assertions are not redundant — each catches what the others cannot
//!
//! Measured, by neutering one thing at a time and watching which fire:
//!
//! | control | 1 reaches | 2 position | 3 magnitude | 4 split |
//! | --- | --- | --- | --- | --- |
//! | `drag_preview` returns `None` (the island) | **fails** 0 px | passes | **fails** 0 px | passes **vacuously** |
//! | `cell()` ignores its index (every cell previews) | passes | **fails** 256 px | passes | passes |
//! | `EVEN` share is `ceil` not `floor` | passes | passes | **fails** 0 px | **fails** 8 px |
//!
//! Two rows of that table are the reason all four exist. Assertion 4 compares two
//! frames, so with the preview absent it compares two *blanks* and passes — it
//! cannot see an island, and only assertion 1 can. Assertion 1 compares against a
//! no-drag frame, so it is satisfied by a wash with any number in it — it cannot
//! see a wrong split, and only 4 can. And a preview drawn in the wrong *place*
//! satisfies every assertion but 2.
//!
//! # What else paints here
//!
//! Per `CLAUDE.md`: the panel fill, the slot well, the item sprite, its count
//! glyphs and the full-canvas dim gradient all paint in this rect. Every count
//! below is a **difference between two frames rendered through identical
//! wiring**, differing only in the drag, so all of that cancels by construction.
//! No cursor is attached (`ContainerFrame::cursor` stays `None`), so the carried
//! stack — the one thing that would move between frames for an unrelated reason,
//! since a drag also changes the *cursor's* previewed count — is not in any of
//! these frames at all.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_drag_preview_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, slot_layout};
use lodestone::gpu::RenderState;
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_game::{click::drag_type, item::ItemStack, menu::Menu};
use lodestone_model::Identifier;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

/// Same fixture as the sibling container gates: `calculate_gui_scale(AUTO, 480,
/// 320) == 1`, so a logical GUI pixel is a physical one. Asserted below, not
/// assumed.
const W: u32 = 480;
const H: u32 = 320;

/// A flat `item/generated` icon — the count glyph is what this gate reads, and a
/// flat sprite keeps the cell's ink simple.
const ITEM: &str = "minecraft:stick";

/// Cells 0, 1 and 2 of a 27-slot chest are the top row; cell 5 is three to the
/// right of them and is never painted.
const PAINTED_A: [usize; 3] = [0, 1, 2];
const PAINTED_B: [usize; 2] = [0, 1];
/// The cell every case shares, and the one the digit comparisons read.
const SHARED: usize = 0;
/// Never painted by any case — the position control.
const UNPAINTED: usize = 5;

fn id(path: &str) -> Identifier {
    path.parse().expect("valid item id")
}

/// The `(x, y, w, h)` **physical** rect of a menu slot, derived from the same
/// expressions the draw uses — `ContainerGeometry::widget_rect` plus
/// `slot_layout`'s own offset. Nothing here restates a constant: `CLAUDE.md`'s
/// HUD gate that hardcoded a *moving* anchor reported 0 px for a row that was
/// drawing perfectly.
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

/// Whether two readbacks differ perceptibly at one pixel. The same threshold the
/// sibling container gates use.
fn differs(a: &[u8], b: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    let d = (i32::from(a[i]) - i32::from(b[i])).abs()
        + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
        + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
    d > 12
}

/// A counted set of pixels inside a rect, **with its bounding box**. Per
/// `CLAUDE.md`, a gate that reports only a count cannot tell a uniform-but-wrong
/// frame from a localised blob — so failure output says *where*.
#[derive(Debug, Default, Clone, Copy)]
struct Region {
    count: usize,
    bbox: Option<[u32; 4]>,
}

impl Region {
    fn add(&mut self, x: u32, y: u32) {
        self.count += 1;
        self.bbox = Some(match self.bbox {
            None => [x, y, x, y],
            Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
        });
    }

    fn describe(&self) -> String {
        match self.bbox {
            None => format!("{} px (empty)", self.count),
            Some([x0, y0, x1, y1]) => format!(
                "{} px, bbox x{}..{} y{}..{} ({}x{})",
                self.count,
                x0,
                x1,
                y0,
                y1,
                x1 - x0 + 1,
                y1 - y0 + 1
            ),
        }
    }
}

fn scan(a: &[u8], b: &[u8], rect: [u32; 4]) -> Region {
    let [rx, ry, rw, rh] = rect;
    let mut region = Region::default();
    for dy in 0..rh {
        for dx in 0..rw {
            let (x, y) = (rx + dx, ry + dy);
            if differs(a, b, x, y) {
                region.add(x, y);
            }
        }
    }
    region
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_drag_preview_draws_the_split_the_release_will_produce() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the cell math below assumes W x H divides to itself under the GUI scale"
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
        "{ITEM} must resolve to an icon through the production atlas, or every frame \
         below is an empty well and the comparisons are between two blanks"
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

    let probe = Menu::generic(27);
    let probe_frame = ContainerFrame::new(Some(&probe), "Chest");
    let shared_rect = slot_rect(&probe, &probe_frame, SHARED);
    let unpainted_rect = slot_rect(&probe, &probe_frame, UNPAINTED);

    eprintln!("=== drag-preview gate (issue #378 part 2) ===");
    eprintln!("shared cell {SHARED} rect = {shared_rect:?}");
    eprintln!("unpainted control cell {UNPAINTED} rect = {unpainted_rect:?}");

    // Four renders. Every menu is identical — 27 empty cells, `count` sticks on
    // the cursor — and they differ only in the drag handed to the frame. The
    // cursor position is deliberately not set, so no carried stack is drawn and
    // the only thing that can move between frames is the preview.
    let shoot = |renderer: &mut ContainerRenderer,
                 target: &mut HeadlessTarget,
                 count: i32,
                 drag: Option<(i32, &[usize])>| {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(ItemStack::new(id(ITEM), count)));
        let frame = ContainerFrame::new(Some(&menu), "Chest").with_drag(drag);
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

    // No drag at all: the baseline.
    let baseline = shoot(&mut renderer, &mut target, 9, None);
    // 9 over three cells -> 3 each.
    let three_each = shoot(
        &mut renderer,
        &mut target,
        9,
        Some((drag_type::EVEN, &PAINTED_A)),
    );
    // 7 over three cells -> floor(7/3) = 2 each.
    let two_each_via_three = shoot(
        &mut renderer,
        &mut target,
        7,
        Some((drag_type::EVEN, &PAINTED_A)),
    );
    // 4 over two cells -> 2 each. Same number, different route.
    let two_each_via_two = shoot(
        &mut renderer,
        &mut target,
        4,
        Some((drag_type::EVEN, &PAINTED_B)),
    );

    let mut failures: Vec<String> = Vec::new();

    // 1 — the preview reaches pixels at all.
    let reached = scan(&three_each, &baseline, shared_rect);
    eprintln!("  preview vs no-drag, painted cell = {}", reached.describe());
    if reached.count < 40 {
        failures.push(format!(
            "the preview barely reached the painted cell ({}) — `quick_craft_slots` \
             computes a split that never becomes geometry, which is this repo's island \
             defect",
            reached.describe()
        ));
    }

    // 2 — POSITION. An unpainted cell must be untouched. A preview nailed to
    // cell 0, or to every cell, fails here and passes everything else.
    let spill = scan(&three_each, &baseline, unpainted_rect);
    eprintln!("  preview vs no-drag, UNPAINTED cell = {}", spill.describe());
    if spill.count > 0 {
        failures.push(format!(
            "the preview painted a cell that is not in the paint set: {}. The wash and \
             the provisional stack must follow `quickCraftSlots`, not the whole grid.",
            spill.describe()
        ));
    }

    // 3 — MAGNITUDE. A different share must look different. Both frames draw a
    // wash and a stick; only the digit differs, so the difference should be small
    // and localised to where a count glyph sits — read the bounding box.
    let digit = scan(&three_each, &two_each_via_three, shared_rect);
    eprintln!("  '3' vs '2' in the same cell        = {}", digit.describe());
    if digit.count == 0 {
        failures.push(
            "a cursor of 9 and a cursor of 7 over the same three cells previewed \
             identically. The cell is lit either way, so this is exactly the gate that \
             tells a real split from a constant — see `CLAUDE.md`'s magnitude species."
                .to_string(),
        );
    }

    // 4 — THE SPLIT ITSELF. `floor(7/3)` and `floor(4/2)` are both 2, so these
    // two frames must be pixel-identical inside the cell they share. A `ceil`
    // would make the first a 3 and break this while still drawing a number.
    let route = scan(&two_each_via_three, &two_each_via_two, shared_rect);
    eprintln!("  7-over-3 vs 4-over-2 (both 2 each) = {}", route.describe());
    if route.count > 0 {
        failures.push(format!(
            "two drags whose per-cell share is the same number previewed differently: \
             {}. `floor(7/3)` and `floor(4/2)` are both 2 — see \
             `AbstractContainerMenu.getQuickCraftPlaceCount`.",
            route.describe()
        ));
    }

    assert!(
        failures.is_empty(),
        "drag-preview gate failed:\n  {}",
        failures.join("\n  ")
    );
}
