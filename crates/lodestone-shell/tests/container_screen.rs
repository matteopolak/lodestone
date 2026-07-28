use lodestone::container::{
    ContainerFrame, ContainerGeometry, ContainerRenderer, Rect, slot_layout,
};
use lodestone_game::{item::ItemStack, menu::Menu};
use lodestone_model::Identifier;

fn id(path: &str) -> Identifier {
    Identifier::new("minecraft", path).unwrap()
}

fn stack(path: &str, count: i32) -> ItemStack {
    ItemStack::new(id(path), count)
}

#[test]
fn generic_container_hotbar_is_relative_to_container_size() {
    let layout = slot_layout(&Menu::generic(27));
    assert_eq!(layout.slots.len(), 63);

    let generic_slot_36 = layout.slots.iter().find(|r| r.menu_index == 36).unwrap();
    let first_hotbar = layout.slots.iter().find(|r| r.menu_index == 54).unwrap();
    assert!(
        first_hotbar.y > generic_slot_36.y,
        "generic hotbar must be below the player main rows"
    );
    assert_eq!(
        first_hotbar.x, layout.slots[27].x,
        "first generic hotbar slot aligns with first player main slot"
    );
    assert_ne!(
        generic_slot_36.y, first_hotbar.y,
        "generic slot 36 is not the hotbar; the hotbar starts at n + 27"
    );
}

#[test]
fn player_inventory_layout_keeps_armour_hotbar_and_offhand_slots_distinct() {
    let layout = slot_layout(&Menu::player());
    assert_eq!(layout.slots.len(), 46);

    let result = layout.slots.iter().find(|r| r.menu_index == 0).unwrap();
    let armour_head = layout.slots.iter().find(|r| r.menu_index == 5).unwrap();
    let first_hotbar = layout.slots.iter().find(|r| r.menu_index == 36).unwrap();
    let offhand = layout.slots.iter().find(|r| r.menu_index == 45).unwrap();

    assert!(
        result.x > armour_head.x,
        "crafting result is not an armour slot"
    );
    assert!(
        first_hotbar.y > armour_head.y,
        "hotbar sits below armour slots"
    );
    assert_ne!(offhand.x, first_hotbar.x, "offhand has its own player slot");
}

/// A crafting table is a `Generic { container_size: 10 }`, so without the
/// `craft_layout` branch its first ten slots would be laid out as a 9-wide run —
/// putting the take-only result slot inside the input grid, and the ninth input
/// cell on a row of its own. The branch is additive on purpose: `MenuKind` is
/// matched exhaustively across the crate and must not grow a variant for this.
#[test]
fn crafting_table_lays_out_a_grid_and_a_result_not_a_nine_wide_run() {
    let menu = Menu::crafting(3, 3);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    let layout = slot_layout(&menu);
    assert_eq!(layout.slots.len(), 46);

    let at = |i: usize| {
        *layout
            .slots
            .iter()
            .find(|r| r.menu_index == i)
            .unwrap_or_else(|| panic!("menu index {i} must be laid out"))
    };

    // The 3x3 grid is three rows of three, in row-major order.
    let first = at(craft.first_input);
    for row in 0..3 {
        for col in 0..3 {
            let cell = at(craft.first_input + row * 3 + col);
            assert_eq!(cell.x, first.x + col as f32 * 18.0);
            assert_eq!(cell.y, first.y + row as f32 * 18.0);
        }
    }

    // The result sits to the right of the whole grid, vertically centred on it —
    // not in it.
    let result = at(craft.result_slot);
    let last_input = at(craft.first_input + 8);
    assert!(
        result.x > last_input.x,
        "the result slot must be right of the grid, not inside it"
    );
    assert_eq!(result.y, first.y + 18.0, "result centres on the middle row");

    // ...and the player inventory is below the grid, with its hotbar last. The
    // container hotbar starts at `n + 27` = 37, never at 36.
    let main_start = at(10);
    let hotbar_start = at(37);
    assert!(main_start.y > last_input.y, "player storage sits below the grid");
    assert!(hotbar_start.y > main_start.y, "hotbar sits below player storage");
    assert_eq!(hotbar_start.x, main_start.x);
    assert_ne!(
        at(36).y,
        hotbar_start.y,
        "slot 36 is the last main-storage slot, not the first hotbar slot"
    );
}

/// The result slot is drawn but never filled locally: a vanilla server computes
/// the result and pushes it as a `container_set_slot` for slot 0. Reading it
/// back is reading server truth.
#[test]
fn crafting_result_slot_renders_whatever_the_server_put_there() {
    let mut menu = Menu::crafting(3, 3);
    let craft = menu.craft_layout().expect("a crafting table has a grid");
    // Inputs alone produce nothing client-side — no local matcher runs.
    menu.set_slot_item(craft.first_input, Some(stack("oak_planks", 4)));
    assert!(
        menu.slot_item(craft.result_slot).is_none(),
        "the shell must not synthesise a result; that is the server's job"
    );

    // The server's reply lands in the result slot and the screen draws it. The
    // slot *well* already covers the whole cell, so coverage cannot see this;
    // the observable is the extra geometry the contents emit.
    let empty_verts = ContainerGeometry::build(&ContainerFrame::new(Some(&menu), "Crafting"), 640, 480)
        .vertex_count();
    menu.set_slot_item(craft.result_slot, Some(stack("crafting_table", 1)));
    let filled_verts = ContainerGeometry::build(&ContainerFrame::new(Some(&menu), "Crafting"), 640, 480)
        .vertex_count();
    assert!(
        filled_verts > empty_verts,
        "a result slot the server filled must draw its contents: {empty_verts} -> \
         {filled_verts} vertices"
    );
    assert_eq!(
        menu.slot_item(craft.result_slot).map(|s| s.item().path()),
        Some("crafting_table"),
        "the result slot must hold exactly what the server sent"
    );
}

#[test]
fn non_empty_container_produces_pixel_coverage_inside_widget_rect() {
    let mut menu = Menu::player();
    menu.set_slot_item(36, Some(stack("diamond", 5)));

    let empty = ContainerGeometry::build(&ContainerFrame::empty(), 640, 480);
    let full = ContainerGeometry::build(&ContainerFrame::new(Some(&menu), "Inventory"), 640, 480);
    let rect = full
        .widget_rect
        .expect("non-empty frame should have a widget rect");

    assert_eq!(covered_pixels(&empty, rect, 640, 480), 0);
    assert!(
        covered_pixels(&full, rect, 640, 480) > 1_000,
        "container geometry must visibly cover pixels inside its own widget rect"
    );
}

#[test]
#[ignore = "requires a GPU adapter"]
fn container_renderer_reaches_pixels_inside_widget_rect() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
        "headless GPU test opted in but no adapter is available; do not treat this as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (width, height) = (640u32, 480u32);
    let bg = [16i32, 18, 23];

    let mut menu = Menu::player();
    menu.set_slot_item(36, Some(stack("diamond_sword", 1)));
    menu.set_slot_item(37, Some(stack("torch", 64)));
    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let rect = ContainerGeometry::build(&frame, width, height)
        .widget_rect
        .expect("populated frame has a widget rect");

    let mut renderer = ContainerRenderer::new(device, format);
    let empty = render_container_frame(
        device,
        queue,
        format,
        width,
        height,
        &mut renderer,
        &ContainerFrame::empty(),
        bg,
    );
    let populated = render_container_frame(
        device,
        queue,
        format,
        width,
        height,
        &mut renderer,
        &frame,
        bg,
    );

    let empty_widget_px = changed_pixels_in_rect(&empty, width, rect, bg);
    let populated_widget_px = changed_pixels_in_rect(&populated, width, rect, bg);
    let corner_px = changed_pixels_in_corners(&populated, width, height, bg);
    let coverage = populated_widget_px as f64 / f64::from(width * height);

    eprintln!("=== shell container render ===");
    eprintln!("container coverage = {:.2}%", coverage * 100.0);
    eprintln!("container rect px  = {populated_widget_px}");
    eprintln!("empty control px   = {empty_widget_px}");
    eprintln!("corner px          = {corner_px}");

    assert_eq!(
        empty_widget_px, 0,
        "empty container state must not light the widget rect"
    );
    assert!(
        populated_widget_px > 1_000,
        "populated container must reach pixels inside its widget rect, only {populated_widget_px}"
    );
    assert_eq!(
        corner_px, 0,
        "container is centred, so frame corners should stay background; got {corner_px}"
    );
}

fn covered_pixels(geo: &ContainerGeometry, rect: Rect, width: u32, height: u32) -> usize {
    let mut covered = 0;
    let min_x = rect.x.max(0.0).floor() as u32;
    let max_x = (rect.x + rect.w).min(width as f32).ceil() as u32;
    let min_y = rect.y.max(0.0).floor() as u32;
    let max_y = (rect.y + rect.h).min(height as f32).ceil() as u32;
    for y in min_y..max_y {
        for x in min_x..max_x {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if geo.verts.chunks_exact(18).any(|tri| {
                let a = ndc_to_px(tri[0], tri[1], width, height);
                let b = ndc_to_px(tri[6], tri[7], width, height);
                let c = ndc_to_px(tri[12], tri[13], width, height);
                point_in_tri((px, py), a, b, c)
            }) {
                covered += 1;
            }
        }
    }
    covered
}

fn render_container_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    renderer: &mut ContainerRenderer,
    frame: &ContainerFrame<'_>,
    bg: [i32; 3],
) -> Vec<u8> {
    use lodestone_render::{HeadlessTarget, RenderTarget};

    let mut target = HeadlessTarget::new(device, width, height, format);
    let acquired = target.acquire().expect("headless acquire");
    clear(device, queue, acquired.view(), bg);
    renderer.render(device, queue, acquired.view(), frame, width, height);
    acquired.present(queue);
    target.read_texels(device, queue)
}

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, bg: [i32; 3]) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("container-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("container-clear-pass"),
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

fn ndc_to_px(x: f32, y: f32, width: u32, height: u32) -> (f32, f32) {
    (
        (x + 1.0) * 0.5 * width as f32,
        (1.0 - y) * 0.5 * height as f32,
    )
}

fn point_in_tri(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign(p, a, b);
    let d2 = sign(p, b, c);
    let d3 = sign(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}
