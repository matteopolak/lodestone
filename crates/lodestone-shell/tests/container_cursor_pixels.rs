//! Pixel gate: **the carried stack must draw on top of every slot item**
//! (issue #377).
//!
//! Reported from play: the stack held on the cursor rendered *under* the items in
//! the inventory slots. It is the thing following the mouse; it has to be on top.
//!
//! # Why append order was not the whole answer
//!
//! The carried stack was already appended last on all three icon streams, and two
//! of the four combinations were therefore already right. The container's GUI item
//! passes run **model first, then flat sprites** — the model pass is the only one
//! that needs a depth attachment, and a pass's attachments are fixed for its
//! lifetime — so within one stratum:
//!
//! | cursor holds | slot holds | before the fix |
//! |---|---|---|
//! | flat sprite | flat sprite | correct (later in the same stream) |
//! | flat sprite | 3-D block | correct (sprite pass runs after the model pass) |
//! | **3-D block** | flat sprite | **wrong** — the model pass runs *before* the sprite pass |
//! | **3-D block** | 3-D block | **wrong** — same GUI depth, resolved against the depth buffer, not append order |
//!
//! and, in every combination, the slot layer's **stack-count glyphs** are on the
//! colour stream's second run, which also drew after the carried icon.
//!
//! So this gate runs the three cases that actually differ, two of which a
//! flat-sprite-only test structurally cannot exercise — `CLAUDE.md`'s *world*
//! species, where the flaw is in the input data and invisible in the test source.
//!
//! # The discriminator: "nothing paints inside the cursor's own ink"
//!
//! A percentage cannot see this and neither can plain coverage — the cell is lit
//! either way. Four renders per case, all at the same slot:
//!
//! ```text
//! chrome      slot empty, cursor empty     -> the panel and well baseline
//! cursor_only slot empty, cursor holds C   -> C's footprint, its "ink"
//! slot_only   slot holds S, cursor empty   -> S's footprint
//! both        slot holds S, cursor holds C -> the subject
//! ```
//!
//! `ink(C)` is where `cursor_only` differs from `chrome`. The assertion is that
//! **inside `ink(C)`, `both` is pixel-identical to `cursor_only`** — the slot item
//! changed nothing where the cursor draws. Violations are reported as a bounding
//! box, never a fraction, so a failure says *where*.
//!
//! # What else paints here, and the control for it
//!
//! Per `CLAUDE.md`: before believing this, ask what else paints in that rect. The
//! answer is a lot — panel art, the slot well, the slot item, its count glyphs,
//! and (over the whole canvas) the dim gradient. Two of those are handled by
//! construction: every count is a *difference against a chrome baseline rendered
//! through the identical wiring*, so the panel, well and dim cancel. The slot item
//! is the thing under test.
//!
//! The control that proves the detector is live is the complement: **inside the
//! cell but outside `ink(C)`, `both` must differ from `cursor_only`** — i.e. the
//! slot item really is drawing in this frame. Without it, a frame where `S` failed
//! to draw at all would satisfy the positive assertion perfectly and this gate
//! would be measuring nothing. The slot stack is given a count of 64 so its digit
//! glyphs are in the frame too; those are the colour stream's own layering, which
//! is a separate mechanism from the two item streams.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_cursor_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{ContainerFrame, ContainerGeometry, ContainerRenderer, slot_layout};
use lodestone::gpu::RenderState;
use lodestone::resources::{BlockResources, load_item_atlas};
use lodestone_assets::ResourceLocation;
use lodestone_game::{item::ItemStack, menu::Menu};
use lodestone_model::Identifier;
use lodestone_render::{BlockModels, GpuContext, HeadlessTarget, RenderTarget};

/// Same fixture as `container_item_pixels.rs`, and for the same reason:
/// `calculate_gui_scale(AUTO, 480, 320) == 1`, so a logical GUI pixel is a
/// physical one and the cell rect needs no scaled reasoning. The precondition is
/// asserted below rather than assumed.
const W: u32 = 480;
const H: u32 = 320;

/// Two visually distinct **full opaque cubes**, so a block-over-block case has
/// something to tell apart and the silhouette has no cutout texels to argue over.
const BLOCK_A: &str = "minecraft:redstone_block";
const BLOCK_B: &str = "minecraft:diamond_block";
/// A flat `item/generated` icon, for the other stream.
const SPRITE_ITEM: &str = "minecraft:diamond";

/// The slot under test: menu index 9 is the first cell of the player screen's
/// main storage — an ordinary `Normal` slot with no neighbour geometry.
const SLOT: usize = 9;

fn id(path: &str) -> Identifier {
    path.parse().expect("valid item id")
}

/// The `(x, y, w, h)` **physical** rect of a menu slot, and the physical cursor
/// position that centres a carried stack exactly on it.
///
/// Both are derived from the same expressions the draw uses —
/// `ContainerGeometry::widget_rect` plus `slot_layout`'s own offset for the rect,
/// and `build_inner`'s `(cx / scale - CELL * 0.5, cy / scale - CELL * 0.5)` for
/// the cursor, solved for `cx`/`cy`. Nothing here restates a constant:
/// `CLAUDE.md`'s HUD gate that hardcoded a *moving* anchor reported 0 px for a row
/// that was drawing perfectly.
fn slot_geometry(menu: &Menu, frame: &ContainerFrame<'_>, menu_index: usize) -> ([u32; 4], [f32; 2]) {
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
    let rect = [
        ((widget.x + slot.x) * scale) as u32,
        ((widget.y + slot.y) * scale) as u32,
        (slot.w * scale) as u32,
        (slot.h * scale) as u32,
    ];
    // `build_inner` draws the carried stack at `(cx / scale - 8, cy / scale - 8)`
    // in the logical canvas, so the cursor that lands its top-left on this slot's
    // top-left is the slot's logical centre, scaled back up.
    let cursor = [
        (widget.x + slot.x + 8.0) * scale,
        (widget.y + slot.y + 8.0) * scale,
    ];
    (rect, cursor)
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

/// Whether two readbacks differ perceptibly at one pixel. The same threshold
/// `container_item_pixels.rs` uses.
fn differs(a: &[u8], b: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * W + x) * 4) as usize;
    let d = (i32::from(a[i]) - i32::from(b[i])).abs()
        + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
        + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
    d > 12
}

/// A counted set of pixels inside a rect, with its bounding box. **Prints a box,
/// not a fraction** — per `CLAUDE.md`, a gate that reports only a count cannot
/// tell a uniform-but-wrong frame from a localised blob.
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

/// Pixels in `rect` where `a` differs from `b`, optionally restricted to
/// (`inside = true`) or excluded from (`inside = false`) the cursor's ink mask.
fn scan(a: &[u8], b: &[u8], rect: [u32; 4], mask: &[bool], inside: bool) -> Region {
    let [rx, ry, rw, rh] = rect;
    let mut region = Region::default();
    for dy in 0..rh {
        for dx in 0..rw {
            let (x, y) = (rx + dx, ry + dy);
            if mask[(dy * rw + dx) as usize] != inside {
                continue;
            }
            if differs(a, b, x, y) {
                region.add(x, y);
            }
        }
    }
    region
}

/// The cursor's own footprint inside `rect`, as a per-pixel mask.
fn ink_mask(cursor_only: &[u8], chrome: &[u8], rect: [u32; 4]) -> (Vec<bool>, Region) {
    let [rx, ry, rw, rh] = rect;
    let mut mask = vec![false; (rw * rh) as usize];
    let mut region = Region::default();
    for dy in 0..rh {
        for dx in 0..rw {
            let (x, y) = (rx + dx, ry + dy);
            if differs(cursor_only, chrome, x, y) {
                mask[(dy * rw + dx) as usize] = true;
                region.add(x, y);
            }
        }
    }
    (mask, region)
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_carried_stack_draws_above_every_slot_item() {
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
    // Both blocks must have baked 3-D inventory geometry, or the two "3-D cursor"
    // cases below would be measuring the absence of an item rather than the
    // absence of a draw — and the flat-sprite case would silently become the only
    // one running, which is the exact vacuity this gate exists to avoid.
    for want in [BLOCK_A, BLOCK_B] {
        let loc: ResourceLocation = want.parse().expect("valid item id");
        assert!(
            models.item(&loc).is_some(),
            "{want} must have baked 3-D inventory geometry"
        );
    }
    let item_atlas = load_item_atlas().expect("the item atlas must build from client.jar");
    for want in [BLOCK_A, BLOCK_B, SPRITE_ITEM] {
        let loc: ResourceLocation = want.parse().expect("valid item id");
        assert!(
            item_atlas.icon(&loc).is_some(),
            "{want} must resolve to an icon through the production atlas"
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    // Here for its *resources*, not its terrain: the icon passes borrow its block
    // atlas, tint palette, animation slots and depth buffer.
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

    // The slot rect and the cursor position that centres a carried icon on it,
    // both from a frame with the same layout every case below uses.
    let probe = Menu::player();
    let probe_frame = ContainerFrame::new(Some(&probe), "Inventory");
    let (rect, cursor) = slot_geometry(&probe, &probe_frame, SLOT);

    eprintln!("=== carried-stack layering gate (issue #377) ===");
    eprintln!("slot {SLOT} rect = {rect:?}   cursor = {cursor:?}");

    let mut failures: Vec<String> = Vec::new();

    // Three cases. The first two are the ones append order could not fix; the
    // third is the flat/flat case, kept so a regression that "fixes" the model
    // strata by breaking the sprite order is caught too.
    for (label, slot_item, carried_item) in [
        ("3-D block cursor over a flat-sprite slot", SPRITE_ITEM, BLOCK_B),
        ("3-D block cursor over a 3-D block slot", BLOCK_A, BLOCK_B),
        ("flat-sprite cursor over a 3-D block slot", BLOCK_A, SPRITE_ITEM),
    ] {
        // Four menus, identical but for what is in the slot and on the cursor.
        // `count: 64` on the slot stack so its digit glyphs — the colour stream's
        // own second run — are in the frame as well.
        let empty = Menu::player();
        let mut slot_only_menu = Menu::player();
        slot_only_menu.set_slot_item(SLOT, Some(ItemStack::new(id(slot_item), 64)));
        let mut cursor_only_menu = Menu::player();
        cursor_only_menu.set_carried(Some(ItemStack::new(id(carried_item), 1)));
        let mut both_menu = Menu::player();
        both_menu.set_slot_item(SLOT, Some(ItemStack::new(id(slot_item), 64)));
        both_menu.set_carried(Some(ItemStack::new(id(carried_item), 1)));

        let shots: Vec<Vec<u8>> = [&empty, &slot_only_menu, &cursor_only_menu, &both_menu]
            .into_iter()
            .map(|menu| {
                let frame = ContainerFrame::new(Some(menu), "Inventory").with_cursor(Some(cursor));
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
            })
            .collect();
        let (chrome, slot_only, cursor_only, both) =
            (&shots[0], &shots[1], &shots[2], &shots[3]);

        let (mask, ink) = ink_mask(cursor_only, chrome, rect);
        let slot_ink = {
            let all_false = vec![false; mask.len()];
            scan(slot_only, chrome, rect, &all_false, false)
        };
        // THE ASSERTION: inside the cursor's own ink, the subject must be
        // pixel-identical to the cursor drawn alone.
        let bleed = scan(both, cursor_only, rect, &mask, true);
        // THE CONTROL: outside that ink but inside the cell, the slot item must
        // be visibly present, or the assertion above is vacuous.
        let outside = scan(both, cursor_only, rect, &mask, false);

        eprintln!("--- {label}");
        eprintln!("  slot={slot_item} carried={carried_item}");
        eprintln!("  cursor ink                 = {}", ink.describe());
        eprintln!("  slot ink (chrome baseline)  = {}", slot_ink.describe());
        eprintln!("  BLEED inside cursor ink     = {}", bleed.describe());
        eprintln!("  control, outside cursor ink = {}", outside.describe());

        if ink.count < 60 {
            failures.push(format!(
                "[{label}] the carried stack barely drew ({}) — this case cannot \
                 measure layering at all",
                ink.describe()
            ));
        }
        if slot_ink.count < 60 {
            failures.push(format!(
                "[{label}] the slot item barely drew ({}) — nothing was on top of \
                 anything, so the assertion below is vacuous",
                slot_ink.describe()
            ));
        }
        if bleed.count > 0 {
            failures.push(format!(
                "[{label}] the slot item paints inside the carried stack's own ink: \
                 {}. The carried stack must be a later stratum than every slot — \
                 see `IconStratum`.",
                bleed.describe()
            ));
        }
        if outside.count == 0 {
            failures.push(format!(
                "[{label}] the control is dark: outside the carried stack's ink the \
                 subject is identical to the cursor drawn over an EMPTY slot, so the \
                 slot item is not in this frame and the positive assertion above is \
                 measuring nothing"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "carried-stack layering gate failed:\n  {}",
        failures.join("\n  ")
    );
}
