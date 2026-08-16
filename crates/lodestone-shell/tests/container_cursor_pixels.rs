//! Pixel gate: **the carried stack must draw on top of every slot item**.
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
//!
//! # The second gate here: the **recipe book** is also under the cursor stack
//!
//! Same subject — "the thing following the mouse is on top" — one more thing that
//! was covering it. Reported from play: *"the recipe book seems to be drawn on top
//! of everything - including items in my cursor when i move them and item
//! tooltips."*
//!
//! `AbstractRecipeBookScreen.extractRenderState` is the record and it is explicit:
//! container contents, `nextStratum()`, the recipe-book component, `nextStratum()`,
//! *then* the carried stack and the hovered-slot tooltip. Ours drew the panel as a
//! trailing pass after the whole container call, so it landed over both.
//!
//! [`the_recipe_book_draws_under_the_carried_stack`] needs **no `client.jar`** —
//! jar-less, both the carried stack and the panel body fall back to flat fills, and
//! flat fills are all a layering question needs. That is deliberate: the arm that
//! can run anywhere is the arm that will be run.

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{
    ContainerFrame, ContainerGeometry, ContainerRenderer, recipe_book_panel_geometry,
    recipe_book_panel_layout_with_scale, slot_layout,
};
use lodestone::hud::HudRenderer;
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

/// Mean per-pixel channel-sum difference inside `rect` — a **magnitude**, where
/// [`differs`] is a boolean.
///
/// The recipe-book arm below reports a magnitude because the carried stack's own
/// jar-less swatch is `a = 0.95`, so it composites differently over the panel than
/// over the container's fill even when the ordering is *right* — measured
/// `d_hook = 117.25` against `d_stack = 165.03`, i.e. 71%, not 100%. Per
/// `CLAUDE.md` an exact composited byte through `ALPHA_BLENDING` on this backend is
/// not predictable, so the gate brackets: a **ratio** against the stack's own
/// contrast, measured in the same rect with no panel in the frame at all.
///
/// The wrong ordering measured **`d_trailing = 0.00`, 0 px** — the panel's slot
/// wells are opaque, so a trailing pass erases the carried stack outright rather
/// than merely dimming it. The two hypotheses are therefore 0.71 and 0.00 of
/// `d_stack`, and the thresholds below (0.5 and 0.25) sit between them with room.
fn mean_delta(a: &[u8], b: &[u8], rect: [u32; 4]) -> f64 {
    let [rx, ry, rw, rh] = rect;
    let mut total = 0u64;
    for dy in 0..rh {
        for dx in 0..rw {
            let i = (((ry + dy) * W + rx + dx) * 4) as usize;
            for c in 0..3 {
                total += (i32::from(a[i + c]) - i32::from(b[i + c])).unsigned_abs() as u64;
            }
        }
    }
    total as f64 / f64::from(rw * rh)
}

/// Which ordering to composite the recipe-book panel with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BookOrder {
    /// No panel in the frame — the baselines.
    Absent,
    /// Production: submitted through
    /// `ContainerRenderer::render_with_icons_scaled_between_strata`, at vanilla's
    /// first `nextStratum()`, so the carried stratum still follows it.
    BetweenStrata,
    /// The **wrong hypothesis**, kept in the gate rather than described: a
    /// trailing pass after the whole container call. This is what shipped, and it
    /// is what the report was about.
    Trailing,
}

/// Pixel gate: **the recipe-book panel draws under the carried stack**, not over
/// it.
///
/// See this file's module doc for the vanilla record. Needs a GPU and nothing
/// else — jar-less, the panel body and the carried stack are both flat fills,
/// which is all a layering question needs.
///
/// # The two hypotheses, both measured
///
/// Four renders at the same cursor position, which is the **centre of the open
/// panel** — the discriminating input. A cursor outside the panel's rect passes
/// under either ordering, so it would be a test that the code runs.
///
/// ```text
/// plain     no carried stack, no panel        the background baseline
/// book      no carried stack, panel drawn     the "panel is here" baseline
/// hook      carried stack, panel between strata
/// trailing  carried stack, panel drawn after
/// ```
///
/// `d_stack = |cursor over plain|` is the stack's own contrast with no panel in
/// the frame — an independent measurement, not a constant. Measured on first run:
/// `d_stack 165.03`, `d_hook 117.25` (71%), `d_trailing 0.00` (0 px of 100). Both
/// arms are asserted, so a frame where the panel failed to draw fails the
/// `trailing` arm instead of passing silently — the premise-false trap
/// `CLAUDE.md` names.
///
/// ```text
/// cargo test -p lodestone-shell --test container_cursor_pixels -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires a GPU adapter"]
fn the_recipe_book_draws_under_the_carried_stack() {
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

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut renderer = ContainerRenderer::new(device, format);
    let mut hud = HudRenderer::new(device, format);

    // The panel rect with the book **open**, from the same layout call the
    // production draw and the click hit-test both go through — nothing here
    // restates a constant.
    let probe = Menu::player();
    let layout = recipe_book_panel_layout_with_scale(
        &probe,
        AUTO_GUI_SCALE,
        W,
        H,
        0,
        false,
        false,
        true,
    );
    let panel = layout.panel;
    // `page_results` empty: the panel body, its wells and its buttons are what
    // covers the cursor, and a jar-less run has no icons to put in them anyway.
    let book_geo = recipe_book_panel_geometry(&layout, true, None, &[], AUTO_GUI_SCALE, W, H);
    assert!(
        !book_geo.verts.is_empty(),
        "the open panel must produce colour geometry, or there is nothing to be \
         layered against"
    );

    // Cursor at the panel's centre. Scale is 1 (asserted above), so physical and
    // logical coincide; the cell is `build_inner`'s own
    // `(cx / scale - CELL * 0.5)` expression solved for the top-left.
    let cursor = [panel.x + panel.w * 0.5, panel.y + panel.h * 0.5];
    let cell = [
        (cursor[0] - 8.0) as u32,
        (cursor[1] - 8.0) as u32,
        16u32,
        16u32,
    ];
    assert!(
        cursor[0] > panel.x + 8.0
            && cursor[0] < panel.x + panel.w - 8.0
            && cursor[1] > panel.y + 8.0
            && cursor[1] < panel.y + panel.h - 8.0,
        "the carried stack's whole cell must sit inside the panel rect, or this \
         gate is measuring an overlap that does not exist: cell {cell:?} vs panel \
         {panel:?}"
    );

    let mut shoot = |carried: bool, order: BookOrder| -> Vec<u8> {
        let mut menu = Menu::player();
        if carried {
            menu.set_carried(Some(ItemStack::new(id(SPRITE_ITEM), 1)));
        }
        let frame = ContainerFrame::new(Some(&menu), "Inventory")
            .with_cursor(Some(cursor))
            .with_book_open(true);
        let acquired = target.acquire().expect("headless acquire");
        let raw_view = acquired.create_view(target.raw_view_format());
        clear_view(device, queue, acquired.view());
        renderer.render_with_icons_scaled_between_strata(
            device,
            queue,
            acquired.view(),
            None,
            &frame,
            None,
            AUTO_GUI_SCALE,
            W,
            H,
            || {
                if order == BookOrder::BetweenStrata {
                    hud.render_recipe_book_panel(
                        device,
                        queue,
                        acquired.view(),
                        &raw_view,
                        None,
                        &book_geo,
                        AUTO_GUI_SCALE,
                        W,
                        H,
                    );
                }
            },
        );
        if order == BookOrder::Trailing {
            hud.render_recipe_book_panel(
                device,
                queue,
                acquired.view(),
                &raw_view,
                None,
                &book_geo,
                AUTO_GUI_SCALE,
                W,
                H,
            );
        }
        target.read_texels(device, queue)
    };

    let plain = shoot(false, BookOrder::Absent);
    let stack_no_book = shoot(true, BookOrder::Absent);
    let book = shoot(false, BookOrder::BetweenStrata);
    let hook = shoot(true, BookOrder::BetweenStrata);
    let trailing = shoot(true, BookOrder::Trailing);

    let d_stack = mean_delta(&stack_no_book, &plain, cell);
    let d_book = mean_delta(&book, &plain, cell);
    let d_hook = mean_delta(&hook, &book, cell);
    let d_trailing = mean_delta(&trailing, &book, cell);
    let (_, stack_ink) = ink_mask(&stack_no_book, &plain, cell);
    let (_, hook_ink) = ink_mask(&hook, &book, cell);
    let (_, trailing_ink) = ink_mask(&trailing, &book, cell);

    eprintln!("=== recipe-book vs carried-stack layering gate ===");
    eprintln!("panel = {panel:?}   cursor = {cursor:?}   cell = {cell:?}");
    eprintln!("  d_stack   (stack over no panel)     = {d_stack:.2}   ink {}", stack_ink.describe());
    eprintln!("  d_book    (panel over no stack)     = {d_book:.2}");
    eprintln!("  d_hook    (between strata, subject) = {d_hook:.2}   ink {}", hook_ink.describe());
    eprintln!("  d_trailing(after the container)     = {d_trailing:.2}   ink {}", trailing_ink.describe());
    // Measured on first run: 165.03 / 117.25 / 0.00. The wrong ordering erases the
    // stack outright (opaque slot wells), so the two hypotheses are ~0.71 and 0.00.
    eprintln!(
        "  correct hypothesis ~= 0.71 * {d_stack:.2} = {:.2}; wrong one ~= 0.00",
        d_stack * 0.71
    );

    let mut failures: Vec<String> = Vec::new();
    if d_stack < 5.0 {
        failures.push(format!(
            "the carried stack barely drew over the plain background (d_stack \
             {d_stack:.2}, {}) — every ratio below is against nothing",
            stack_ink.describe()
        ));
    }
    if d_book < 5.0 {
        failures.push(format!(
            "the panel does not paint inside the carried stack's own cell (d_book \
             {d_book:.2}), so there is no overlap here and both arms below would \
             pass whatever the ordering is — this gate's premise is false"
        ));
    }
    if d_hook < 0.5 * d_stack {
        failures.push(format!(
            "the recipe book is painting over the carried stack: with the panel \
             submitted at the nextStratum() hook the stack retains only \
             d_hook {d_hook:.2} of its own d_stack {d_stack:.2} ({}). Vanilla's \
             AbstractRecipeBookScreen.extractRenderState draws the component \
             between two nextStratum() calls, with extractCarriedItem after both.",
            hook_ink.describe()
        ));
    }
    if d_trailing > 0.25 * d_stack {
        failures.push(format!(
            "the control is dark: drawing the panel as a TRAILING pass — the \
             ordering that shipped and was reported — left d_trailing \
             {d_trailing:.2} against d_stack {d_stack:.2}, so this gate cannot \
             tell the two orderings apart and its positive arm proves nothing"
        ));
    }

    assert!(
        failures.is_empty(),
        "recipe-book layering gate failed:\n  {}",
        failures.join("\n  ")
    );
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
