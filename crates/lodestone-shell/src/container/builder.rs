//! The four vertex streams every container surface fills, and the atlas-less
//! swatch fallback.
//!
//! Split out of `container.rs` verbatim.

use lodestone_assets::ResourceLocation;
use lodestone_render::{GuiSpriteQuad, ModelVertex};

use crate::hud::HotbarSlot;
use crate::hud::VanillaFont;
use crate::hud::item_icon::{self, ColourStream, IconAssets, IconSink, SpecialIconDraw};

use super::CELL;

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
        // The container surfaces share the hotbar's foil predicate — a stacked
        // enchanted item in a chest, furnace or recipe panel must glint exactly
        // like the one in the hotbar (issue #452).
        enchanted: item_icon::stack_has_foil(stack),
    })
}

/// The stack-count ink on the **atlas-less** fallback path (the real path uses
/// [`item_icon::COUNT_INK`]). Named rather than inline so the recipe-panel
/// submission-order gate can find a count-digit vertex by the same constant the
/// draw writes, instead of restating the literal.
pub(super) const FALLBACK_COUNT_INK: [f32; 4] = [0.98, 0.98, 0.92, 1.0];

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

/// The overlay's four vertex streams, filled in one pass over the layout. The
/// colour stream is this module's own; the item-sprite and block-model streams
/// are the shared hotbar ones (see [`crate::hud::item_icon`]); the background
/// stream samples [`ContainerBackground`]'s own atlas.
#[derive(Debug)]
pub(super) struct Builder<'a> {
    w: f32,
    h: f32,
    pub(super) verts: Vec<f32>,
    pub(super) item_verts: Vec<f32>,
    pub(super) model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons; see [`ContainerGeometry::special`].
    pub(super) special: Vec<SpecialIconDraw>,
    /// Flat `[x, y, u, v, r, g, b, a]` per vertex, off
    /// [`ContainerBackground`]'s atlas.
    pub(super) bg_verts: Vec<f32>,
    /// The vanilla proportional font, for stack counts. `None` on a jar-less
    /// run, where [`item_icon::draw_item_icon`] falls back to the fixed-advance
    /// 5×7 debug font — the same degradation the HUD's own text uses.
    pub(super) font: Option<&'a VanillaFont>,
}

impl<'a> Builder<'a> {
    pub(super) fn new(w: f32, h: f32, font: Option<&'a VanillaFont>) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            item_verts: Vec::new(),
            model_verts: Vec::new(),
            special: Vec::new(),
            bg_verts: Vec::new(),
            font,
        }
    }

    pub(super) fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    /// A pixel-space rectangle with a vertical gradient from `top` (its own top
    /// edge) to `bottom` (its bottom edge) — see [`ColourStream::gradient_rect`].
    pub(super) fn gradient_rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, top: [f32; 4], bottom: [f32; 4]) {
        self.colour().gradient_rect(x, y, w, h, top, bottom);
    }

    /// One [`GuiSpriteQuad`] onto the background stream, untinted.
    pub(super) fn bg_sprite(&mut self, q: GuiSpriteQuad) {
        let (w, h) = (self.w, self.h);
        item_icon::push_sprite_quad(&mut self.bg_verts, w, h, q, [1.0, 1.0, 1.0, 1.0]);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().text(s, x, y, scale, c);
    }

    /// One of vanilla's two container labels: the **proportional** font when one
    /// is attached, and **no drop shadow** either way — the trailing `false` in
    /// `AbstractContainerScreen.java:190-191`'s `graphics.text` calls. Every
    /// other text surface in this crate is shadowed, which is why this needs its
    /// own entry point rather than reusing `VanillaFont::draw`.
    ///
    /// Degrades to the fixed-advance 5×7 debug font on a jar-less run, the same
    /// way stack counts do — advances will be wrong, but the words are readable
    /// and the anchor is identical, so the geometry gate still measures the same
    /// thing.
    pub(super) fn label(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let mut cs = self.colour();
                f.draw_plain(&mut cs, s, x, y, scale, c);
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// The anvil/enchanting-table cost numbers' own text call — vanilla's
    /// **default** `graphics.text(font, str, x, y, colour)` overload
    /// (`GuiGraphicsExtractor.java:239-241`), which defaults `dropShadow` to
    /// `true`, unlike [`label`](Self::label)'s explicit `false` for the two
    /// container labels. Degrades to the same fixed-advance debug font
    /// [`label`](Self::label) does on a jar-less run.
    pub(super) fn shadowed_label(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let mut cs = self.colour();
                f.draw(&mut cs, s, x, y, scale, c);
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// A handle onto the colour stream, for the shared pixel-space primitives.
    fn colour(&mut self) -> ColourStream<'_> {
        ColourStream {
            w: self.w,
            h: self.h,
            verts: &mut self.verts,
        }
    }

    /// One slot's real icon, through the shared pass, with an explicit
    /// stack-count ink — [`item_icon::COUNT_INK`] for everything except the drag
    /// preview's clamped counts, which are yellow.
    fn item_icon_counted(
        &mut self,
        assets: &IconAssets<'_>,
        record: &HotbarSlot,
        x: f32,
        y: f32,
        size: f32,
        count_ink: [f32; 4],
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
            special: &mut self.special,
        };
        item_icon::draw_item_icon_counted(
            &mut sink,
            assets,
            (w, h),
            record,
            x,
            y,
            size,
            self.font,
            count_ink,
        );
    }

    /// Draw one occupied cell's contents at `(x, y)`: the real icon when the
    /// item resolves against an attached atlas, else the hash-derived
    /// swatch-and-letter fallback. Shared by the per-slot loop and the carried
    /// stack, so an atlas-less run shows the cursor's stack exactly as it
    /// shows an occupied well.
    pub(super) fn draw_stack(&mut self, assets: &IconAssets<'_>, stack: &lodestone_game::item::ItemStack, x: f32, y: f32) {
        self.draw_stack_counted(assets, stack, x, y, item_icon::COUNT_INK);
    }

    /// As [`draw_stack`](Self::draw_stack), with an explicit stack-count ink. The
    /// drag preview uses [`item_icon::COUNT_INK_CLAMPED`] (vanilla's yellow) when
    /// the provisional count hit the destination cell's cap.
    pub(super) fn draw_stack_counted(
        &mut self,
        assets: &IconAssets<'_>,
        stack: &lodestone_game::item::ItemStack,
        x: f32,
        y: f32,
        count_ink: [f32; 4],
    ) {
        match (assets.items, icon_record(stack)) {
            // The real thing: the shared hotbar icon pass, which also draws
            // the stack count and the durability bar.
            (Some(_), Some(record)) => {
                self.item_icon_counted(assets, &record, x, y, CELL, count_ink)
            }
            // No atlas (or an item id the atlas could never key): the old
            // hash-derived swatch plus a letter, so an occupied cell still
            // reads as occupied on a jar-less run.
            _ => {
                let color = item_color(stack.item().path());
                self.rect_px(x + 3.0, y + 3.0, 10.0, 10.0, color);
                let label = item_label(stack.item().path());
                self.text(&label, x + 5.0, y + 5.0, 1.0, [0.97, 0.95, 0.86, 1.0]);
                if stack.count() > 1 {
                    self.text(&stack.count().to_string(), x + 8.0, y + 10.0, 1.0, FALLBACK_COUNT_INK);
                }
            }
        }
    }
}
