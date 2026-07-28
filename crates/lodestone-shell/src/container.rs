//! Container and inventory screen rendering.
//!
//! Slot state is folded by `lodestone-client`/`lodestone-game`; this module only
//! projects a [`Menu`](lodestone_game::menu::Menu) into rectangles, coloured
//! quads and **item icons**. The generic-container hotbar starts at `n + 27`,
//! not absolute slot 36.
//!
//! # Layout
//!
//! [`slot_layout`] dispatches on [`MenuKind`] and then, additively, on
//! [`Menu::craft_layout`]: a menu that reports a crafting grid gets the vanilla
//! crafting-table arrangement (grid + result to its right, player inventory
//! below) rather than the flat 9-wide run a plain container gets. That branch is
//! deliberately *not* a new `MenuKind` — a crafting table's quick-move regions
//! and content size are a generic container's, only its slot kinds and its
//! screen differ, and `MenuKind` is matched exhaustively across this crate.
//!
//! Every `SlotRect` carries the real `menu_index`, so there is no constant
//! offset anywhere: window 0 is `0` result / `1..=4` craft / `5..=8` armour /
//! `9..=35` main / `36..=44` hotbar / `45` offhand, while a `Generic { n }` has
//! neither armour nor offhand and its hotbar is at `n + 27`.
//!
//! # Icons
//!
//! Slot contents draw through [`crate::hud::item_icon`] — the same flat-sprite
//! and 3-D block-item pass the hotbar uses, with the same atlases, tint palette
//! and animation slots. Without [`ContainerRenderer::attach_items`] the screen
//! falls back to the hash-derived colour swatch and letter it drew before there
//! was an atlas to draw from, so a jar-less run still shows *something* in an
//! occupied slot.

use lodestone_game::menu::{CraftLayout, Menu, MenuKind};
use lodestone_render::{BlockModels, ModelVertex};

use lodestone_assets::{ItemAtlas, ResourceLocation};

use std::sync::Arc;

use crate::hud::HotbarSlot;
use crate::hud::item_icon::{self, ColourStream, IconAssets, IconRenderer, IconSink};

const FLOATS_PER_VERTEX: usize = 6;
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

/// Geometry for the container overlay: coloured chrome plus, when an item atlas
/// is attached, real slot icons on the two icon streams.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex, with positions in NDC. Panel,
    /// slot wells, title, stack counts and durability bars.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured **item**-sprite vertex,
    /// sampling the [`ItemAtlas`]. Empty unless one was supplied.
    pub item_verts: Vec<f32>,
    /// The 3-D **block-item** icons, already posed into GUI pixel space on the
    /// CPU. Empty unless a [`BlockModels`] was supplied.
    pub model_verts: Vec<ModelVertex>,
    /// How many leading vertices of [`verts`](Self::verts) are *chrome* — the
    /// panel, the title and the slot wells. The remainder (stack counts,
    /// durability bars, the atlas-less swatch fallback) belongs **on top of**
    /// the icons, so the renderer draws this stream in two ranges with the icon
    /// passes in between.
    pub chrome_vertex_count: usize,
    /// Pixel rect covered by the widget, if anything was drawn.
    pub widget_rect: Option<Rect>,
}

impl ContainerGeometry {
    /// Number of coloured vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Builds container overlay geometry for a viewport, with no item atlas: the
    /// slot contents fall back to a colour swatch and a letter. This is the
    /// jar-less / headless path, and the negative control the pixel gate
    /// exercises.
    #[must_use]
    pub fn build(frame: &ContainerFrame<'_>, width: u32, height: u32) -> Self {
        Self::build_inner(frame, width, height, &IconAssets {
            items: None,
            models: None,
        })
    }

    /// Builds container overlay geometry drawing **real item icons** from the
    /// atlases. `models` may be `None`, in which case flat sprite items draw and
    /// block items do not.
    #[must_use]
    pub fn build_with_icons(
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
        items: &ItemAtlas,
        models: Option<&BlockModels>,
    ) -> Self {
        Self::build_inner(frame, width, height, &IconAssets {
            items: Some(items),
            models,
        })
    }

    fn build_inner(
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
        assets: &IconAssets<'_>,
    ) -> Self {
        let Some(menu) = frame.menu else {
            return Self {
                verts: Vec::new(),
                item_verts: Vec::new(),
                model_verts: Vec::new(),
                chrome_vertex_count: 0,
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

        // Every well first, so the colour stream splits cleanly into "chrome"
        // and "what goes on top of an icon". The icons are drawn between the two
        // halves (they are a separate pass, and the 3-D ones need a depth
        // buffer), so a stack count emitted in the same loop as its well would
        // end up *underneath* the sprite it is counting.
        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
            b.rect_px(sx - 1.0, sy - 1.0, SLOT, SLOT, [0.04, 0.035, 0.032, 0.92]);
            b.rect_px(sx, sy, CELL, CELL, [0.32, 0.30, 0.27, 0.86]);
        }
        let chrome_floats = b.verts.len();

        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
            let Some(stack) = menu.slot_item(slot.menu_index) else {
                continue;
            };
            match (assets.items, icon_record(stack)) {
                // The real thing: the shared hotbar icon pass, which also draws
                // the stack count and the durability bar.
                (Some(_), Some(record)) => b.item_icon(assets, &record, sx, sy, CELL),
                // No atlas (or an item id the atlas could never key): the old
                // hash-derived swatch plus a letter, so an occupied slot still
                // reads as occupied on a jar-less run.
                _ => {
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
        }

        Self {
            chrome_vertex_count: chrome_floats / FLOATS_PER_VERTEX,
            verts: b.verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
            widget_rect: Some(Rect {
                x,
                y,
                w: layout.width,
                h: layout.height,
            }),
        }
    }
}

/// Turn a menu slot's stack into the shared per-slot draw record, mirroring what
/// `app.rs` builds for the hotbar. `None` when the item id does not parse as a
/// [`ResourceLocation`], which no vanilla id does.
fn icon_record(stack: &lodestone_game::item::ItemStack) -> Option<HotbarSlot> {
    let item = ResourceLocation::parse(&stack.item().to_string()).ok()?;
    let damage = stack
        .components()
        .get_int(lodestone_game::item::DAMAGE_COMPONENT)
        .and_then(|v| u32::try_from(v).ok());
    let max_damage = stack
        .components()
        .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
        .and_then(|v| u32::try_from(v).ok());
    Some(HotbarSlot {
        item,
        count: stack.count().max(0) as u32,
        damage,
        max_damage,
        enchanted: false,
    })
}

/// Computes the slot layout in local widget coordinates.
///
/// The [`MenuKind`] match stays exhaustive over two variants; the crafting
/// screen is reached *additively* through [`Menu::craft_layout`], which is
/// exactly why that descriptor was put on [`Menu`] instead of in `MenuKind`. A
/// crafting table is a `Generic { container_size: 10 }` whose result and 3×3
/// grid happen to be its first ten slots, and laying those out as a 9-wide run
/// (which is what a plain container would do) puts the result slot in the middle
/// of the grid.
#[must_use]
pub fn slot_layout(menu: &Menu) -> SlotLayout {
    match menu.kind() {
        MenuKind::Player => player_layout(),
        MenuKind::Generic { container_size } => match menu.craft_layout() {
            Some(craft) => crafting_layout(craft, container_size),
            None => generic_layout(container_size),
        },
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

/// The crafting-table arrangement: the input grid top-left, the take-only
/// result slot to its right, then the player's main storage and hotbar below.
///
/// The constants are vanilla's `crafting_table.png` slot origins for the 3×3
/// case — grid at `(30, 17)`, result at `(124, 35)`, main at `(8, 84)`, hotbar at
/// `(8, 142)`, panel `176x166` — expressed in terms of the grid's real
/// dimensions so a differently sized grid (none ships in vanilla) still lands
/// somewhere sane rather than on top of the inventory.
///
/// The result slot is drawn but never *filled* here: a vanilla server computes
/// the crafting result itself and pushes it as a `container_set_slot` for slot
/// 0, which `Menus::apply` reconciles into the menu. Reading `slot_item` is
/// therefore reading server truth; matching a recipe locally to fill this slot
/// would overwrite it with a guess.
fn crafting_layout(craft: CraftLayout, container_size: usize) -> SlotLayout {
    let grid_x = 30.0;
    let grid_y = 17.0;
    let cols = craft.width.max(1);
    let rows = craft.height.max(1);
    let mut slots = Vec::with_capacity(container_size + 36);

    slots.push(slot(
        craft.result_slot,
        grid_x + cols as f32 * SLOT + 40.0,
        grid_y + (rows as f32 - 1.0) * SLOT * 0.5,
    ));
    for i in 0..craft.cell_count() {
        slots.push(slot(
            craft.first_input + i,
            grid_x + (i % cols) as f32 * SLOT,
            grid_y + (i / cols) as f32 * SLOT,
        ));
    }

    let main_y = (grid_y + rows as f32 * SLOT + 13.0).max(84.0);
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

/// The overlay's three vertex streams, filled in one pass over the layout. The
/// colour stream is this module's own; the two icon streams are the shared
/// hotbar ones (see [`crate::hud::item_icon`]).
#[derive(Debug)]
struct Builder {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    item_verts: Vec<f32>,
    model_verts: Vec<ModelVertex>,
}

impl Builder {
    fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            item_verts: Vec::new(),
            model_verts: Vec::new(),
        }
    }

    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().text(s, x, y, scale, c);
    }

    /// A handle onto the colour stream, for the shared pixel-space primitives.
    fn colour(&mut self) -> ColourStream<'_> {
        ColourStream {
            w: self.w,
            h: self.h,
            verts: &mut self.verts,
        }
    }

    /// One slot's real icon, through the shared pass.
    fn item_icon(
        &mut self,
        assets: &IconAssets<'_>,
        record: &HotbarSlot,
        x: f32,
        y: f32,
        size: f32,
    ) {
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
        };
        item_icon::draw_item_icon(&mut sink, assets, (w, h), record, x, y, size);
    }
}

/// GPU renderer for the container overlay.
#[derive(Debug)]
pub struct ContainerRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The flat item atlas and the 3-D block-item pass, shared verbatim with the
    /// hotbar. Both halves start detached, so [`render`](Self::render) alone
    /// keeps the pre-icon behaviour.
    icons: IconRenderer,
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
            icons: IconRenderer::new(),
        }
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so container slots draw real
    /// item icons instead of the colour-swatch fallback. Mirrors
    /// [`HudRenderer::attach_items`](crate::hud::HudRenderer::attach_items) and
    /// costs a second upload of the (small) item atlas; the *block* atlas, the
    /// expensive one, is borrowed rather than uploaded by
    /// [`attach_item_models`](Self::attach_item_models).
    pub fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
    ) {
        self.icons
            .attach_items(device, queue, color_format, atlas, "container-item");
    }

    /// Attach the **3-D block-item** pass, so container slots holding a block
    /// draw vanilla's isometric mini-block. Every resource is borrowed from the
    /// world renderer — the same block atlas, tint palette and animation slots
    /// the terrain and the hotbar use.
    pub fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
    ) {
        self.icons.attach_item_models(
            device,
            color_format,
            atlas_view,
            atlas_sampler,
            palette,
            anim,
            "container-item-model",
        );
    }

    /// Draws the container overlay over the current frame, with **no** item
    /// icons: slot contents fall back to the colour swatch. The plain entry
    /// point, kept so existing callers and the headless gates are unchanged.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons(device, queue, view, None, frame, None, width, height);
    }

    /// Draws the container overlay including **real item icons**.
    ///
    /// `models` supplies baked block-item geometry (`None` falls back to flat
    /// sprites only) and `depth` is a depth attachment matching the target size,
    /// normally [`RenderState::depth_view`](crate::gpu::RenderState::depth_view).
    /// Both are needed for a mini-block to draw; either being `None` degrades to
    /// flat sprites rather than erroring. The flat icons themselves need
    /// [`attach_items`](Self::attach_items) and nothing else.
    ///
    /// # Pass structure
    ///
    /// Three passes, in this order, all loading the existing colour — the same
    /// shape, and for the same reasons, as the HUD's:
    ///
    /// 1. **chrome** (no depth) — panel, slot wells, title;
    /// 2. **item models** (depth, **cleared**) — the isometric mini-blocks;
    /// 3. **flat icons + text** (no depth) — sprite icons, stack counts,
    ///    durability bars.
    ///
    /// The chrome must precede the icons (it is the well they sit in), and the
    /// counts must follow them (they sit on top). The model pass clears depth
    /// because the world's is still resident and would swallow a GUI item at
    /// clip depth ~0.5.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        width: u32,
        height: u32,
    ) {
        // Only ask for model geometry when there is somewhere to draw it.
        let want_models = self.icons.models_attached() && depth.is_some();
        let item_atlas = self.icons.item_atlas();
        let geo = ContainerGeometry::build_inner(frame, width, height, &IconAssets {
            items: item_atlas.as_deref(),
            models: models.filter(|_| want_models),
        });
        if geo.verts.is_empty() && geo.item_verts.is_empty() && geo.model_verts.is_empty() {
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
        if !geo.verts.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }
        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            width,
            height,
            "container-item-verts",
        );

        let vertex_count = geo.vertex_count() as u32;
        let chrome_count = (geo.chrome_vertex_count as u32).min(vertex_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container"),
        });
        if chrome_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..chrome_count, 0..1);
        }

        self.icons.draw_models(
            &mut encoder,
            view,
            depth,
            model_count,
            "container-item-model-pass",
        );

        if item_count > 0 || vertex_count > chrome_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-item-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites(&mut pass, item_count);
            // Stack counts, durability bars and the atlas-less swatch fallback,
            // over whichever kind of icon drew beneath them.
            if vertex_count > chrome_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(chrome_count..vertex_count, 0..1);
            }
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
