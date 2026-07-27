//! Container and inventory screen rendering.
//!
//! Slot state is folded by `lodestone-client`/`lodestone-game`; this module only
//! projects a [`Menu`](lodestone_game::menu::Menu) into rectangles and coloured
//! quads. The generic-container hotbar starts at `n + 27`, not absolute slot 36.

use lodestone_game::menu::{Menu, MenuKind};

use crate::hud::glyph_rows;

const FLOATS_PER_VERTEX: usize = 6;
const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;
const SLOT: f32 = 18.0;
const CELL: f32 = 16.0;

/// A pixel-space rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: f32,
    /// Top edge in pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// One laid-out menu slot, in local widget coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotRect {
    /// Menu-slot index.
    pub menu_index: usize,
    /// Left edge in local widget pixels.
    pub x: f32,
    /// Top edge in local widget pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// Complete local layout for a menu.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotLayout {
    /// Widget width in pixels.
    pub width: f32,
    /// Widget height in pixels.
    pub height: f32,
    /// Slot rectangles in menu-slot order.
    pub slots: Vec<SlotRect>,
}

/// The container screen to draw for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ContainerFrame<'a> {
    /// Menu contents to draw. `None` draws nothing.
    pub menu: Option<&'a Menu>,
    /// Title to draw at the top-left of the panel.
    pub title: &'a str,
}

impl<'a> ContainerFrame<'a> {
    /// A frame for an optional menu.
    #[must_use]
    pub fn new(menu: Option<&'a Menu>, title: &'a str) -> Self {
        Self { menu, title }
    }

    /// A frame that deliberately draws nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            menu: None,
            title: "",
        }
    }
}

/// Coloured quad geometry for the container overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex, with positions in NDC.
    pub verts: Vec<f32>,
    /// Pixel rect covered by the widget, if anything was drawn.
    pub widget_rect: Option<Rect>,
}

impl ContainerGeometry {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Builds container overlay geometry for a viewport.
    #[must_use]
    pub fn build(frame: &ContainerFrame<'_>, width: u32, height: u32) -> Self {
        let Some(menu) = frame.menu else {
            return Self {
                verts: Vec::new(),
                widget_rect: None,
            };
        };
        let layout = slot_layout(menu);
        let w = width.max(1) as f32;
        let h = height.max(1) as f32;
        let x = ((w - layout.width) * 0.5).max(8.0);
        let y = ((h - layout.height) * 0.5).max(8.0);
        let mut b = Builder::new(w, h);

        b.rect_px(
            x,
            y,
            layout.width,
            layout.height,
            [0.08, 0.075, 0.065, 0.88],
        );
        b.rect_px(
            x + 3.0,
            y + 3.0,
            layout.width - 6.0,
            layout.height - 6.0,
            [0.22, 0.20, 0.17, 0.70],
        );
        b.text(
            &frame.title.to_ascii_uppercase(),
            x + 8.0,
            y + 7.0,
            1.0,
            [0.88, 0.84, 0.73, 1.0],
        );

        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
            b.rect_px(sx - 1.0, sy - 1.0, SLOT, SLOT, [0.04, 0.035, 0.032, 0.92]);
            b.rect_px(sx, sy, CELL, CELL, [0.32, 0.30, 0.27, 0.86]);
            if let Some(stack) = menu.slot_item(slot.menu_index) {
                let color = item_color(stack.item().path());
                b.rect_px(sx + 3.0, sy + 3.0, 10.0, 10.0, color);
                let label = item_label(stack.item().path());
                b.text(&label, sx + 5.0, sy + 5.0, 1.0, [0.97, 0.95, 0.86, 1.0]);
                if stack.count() > 1 {
                    b.text(
                        &stack.count().to_string(),
                        sx + 8.0,
                        sy + 10.0,
                        1.0,
                        [0.98, 0.98, 0.92, 1.0],
                    );
                }
            }
        }

        Self {
            verts: b.verts,
            widget_rect: Some(Rect {
                x,
                y,
                w: layout.width,
                h: layout.height,
            }),
        }
    }
}

/// Computes the slot layout in local widget coordinates.
#[must_use]
pub fn slot_layout(menu: &Menu) -> SlotLayout {
    match menu.kind() {
        MenuKind::Player => player_layout(),
        MenuKind::Generic { container_size } => generic_layout(container_size),
    }
}

fn player_layout() -> SlotLayout {
    let mut slots = Vec::with_capacity(46);
    slots.push(slot(0, 154.0, 28.0));
    for i in 0..4 {
        slots.push(slot(
            1 + i,
            98.0 + (i % 2) as f32 * SLOT,
            18.0 + (i / 2) as f32 * SLOT,
        ));
    }
    for i in 0..4 {
        slots.push(slot(5 + i, 8.0, 8.0 + i as f32 * SLOT));
    }
    for i in 0..27 {
        slots.push(slot(
            9 + i,
            8.0 + (i % 9) as f32 * SLOT,
            84.0 + (i / 9) as f32 * SLOT,
        ));
    }
    for i in 0..9 {
        slots.push(slot(36 + i, 8.0 + i as f32 * SLOT, 142.0));
    }
    slots.push(slot(45, 77.0, 62.0));
    SlotLayout {
        width: 176.0,
        height: 166.0,
        slots,
    }
}

fn generic_layout(container_size: usize) -> SlotLayout {
    let cols = 9usize;
    let rows = container_size.div_ceil(cols).max(1);
    let mut slots = Vec::with_capacity(container_size + 36);
    for i in 0..container_size {
        slots.push(slot(
            i,
            8.0 + (i % cols) as f32 * SLOT,
            18.0 + (i / cols) as f32 * SLOT,
        ));
    }
    let main_y = 18.0 + rows as f32 * SLOT + 14.0;
    for i in 0..27 {
        slots.push(slot(
            container_size + i,
            8.0 + (i % 9) as f32 * SLOT,
            main_y + (i / 9) as f32 * SLOT,
        ));
    }
    let hotbar_y = main_y + 58.0;
    for i in 0..9 {
        slots.push(slot(
            container_size + 27 + i,
            8.0 + i as f32 * SLOT,
            hotbar_y,
        ));
    }
    SlotLayout {
        width: 176.0,
        height: hotbar_y + 24.0,
        slots,
    }
}

fn slot(menu_index: usize, x: f32, y: f32) -> SlotRect {
    SlotRect {
        menu_index,
        x,
        y,
        w: CELL,
        h: CELL,
    }
}

fn item_label(path: &str) -> String {
    path.rsplit(['/', '_'])
        .find(|part| !part.is_empty())
        .and_then(|part| part.chars().next())
        .unwrap_or('?')
        .to_ascii_uppercase()
        .to_string()
}

fn item_color(path: &str) -> [f32; 4] {
    let mut hash = 0u32;
    for b in path.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u32::from(b));
    }
    let hue = hash as f32 / u32::MAX as f32;
    let r = 0.35 + 0.35 * (hue * std::f32::consts::TAU).sin().abs();
    let g = 0.35 + 0.35 * ((hue + 0.33) * std::f32::consts::TAU).sin().abs();
    let b = 0.35 + 0.35 * ((hue + 0.66) * std::f32::consts::TAU).sin().abs();
    [r, g, b, 0.95]
}

#[derive(Debug)]
struct Builder {
    w: f32,
    h: f32,
    verts: Vec<f32>,
}

impl Builder {
    fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
        }
    }

    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let mut v = |vx: f32, vy: f32| {
            self.verts
                .extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0);
        v(x1, y0);
        v(x1, y1);
        v(x0, y0);
        v(x1, y1);
        v(x0, y1);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let advance = (GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for ch in s.chars() {
            self.glyph(ch, cursor, y, scale, c);
            cursor += advance;
        }
    }

    fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if ch == ' ' {
            return;
        }
        let rows = glyph_rows(ch);
        for (ry, row) in rows.iter().enumerate().take(GLYPH_H) {
            for rx in 0..GLYPH_W {
                let bit = (row >> (GLYPH_W - 1 - rx)) & 1;
                if bit == 1 {
                    self.rect_px(
                        x + rx as f32 * scale,
                        y + ry as f32 * scale,
                        scale,
                        scale,
                        c,
                    );
                }
            }
        }
    }
}

/// GPU renderer for the container overlay.
#[derive(Debug)]
pub struct ContainerRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl ContainerRenderer {
    /// Builds the overlay pipeline.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("container-shader"),
            source: wgpu::ShaderSource::Wgsl(CONTAINER_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("container-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("container-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("container-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            buffer,
            capacity_floats,
        }
    }

    /// Draws the container overlay over the current frame.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
    ) {
        let geo = ContainerGeometry::build(frame, width, height);
        if geo.verts.is_empty() {
            return;
        }
        if geo.verts.len() > self.capacity_floats {
            self.capacity_floats = geo.verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("container-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        let vertex_count = geo.vertex_count() as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..vertex_count, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const CONTAINER_WGSL: &str = r"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
";
