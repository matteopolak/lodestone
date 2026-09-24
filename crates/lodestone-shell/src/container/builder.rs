//! The four vertex streams every container surface fills, and the atlas-less
//! swatch fallback.
//!
//! Split out of `container.rs` verbatim.

use lodestone_assets::ResourceLocation;
use lodestone_render::{GuiSpriteQuad, ModelVertex};

use lodestone_model::text::TextSpan;

use crate::hud::HotbarSlot;
use crate::hud::VanillaFont;
use crate::hud::item_icon::{self, ColourStream, IconAssets, IconSink, SpecialIconDraw};

use super::CELL;

/// Turn a menu slot's stack into the shared per-slot draw record, mirroring what
/// `app.rs` builds for the hotbar. `None` when the item id does not parse as a
/// [`ResourceLocation`], which no vanilla id does.
fn icon_record(stack: &lodestone_game::item::ItemStack) -> Option<HotbarSlot> {
    // `minecraft:item_model` replaces only the client-side definition lookup.
    // Keep the base item in the game stack for gameplay, but hand every icon
    // surface the pack definition that actually supplies its model and sprites.
    let item = stack.item_model().unwrap_or_else(|| stack.item().clone());
    let item = ResourceLocation::parse(&item.to_string()).ok()?;
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
        // like the one in the hotbar.
        enchanted: item_icon::stack_has_foil(stack),
        custom_model_data: stack.custom_model_data(),
        // The live tint components: without these, `sprite_layer_tint` resolved
        // every icon against `ItemTintContext::default()` regardless of the real
        // stack, so a dyed leather chestplate or a mixed potion drew the
        // definition's plain default in every container, the creative menu and
        // the advancements grid (all of which route through `draw_stack` into
        // this function) — see `lodestone_assets::item_tint`'s module doc.
        dyed_color: stack.dyed_color(),
        potion_color: stack.potion_color(),
        // Same crate-boundary loss as the dye/potion pair above, for a
        // banner's loom patterns rather than its colour — without this a
        // banner in a chest, the creative menu or the advancements grid drew
        // its base colour only, never its pattern.
        banner_patterns: stack.banner_patterns().to_vec(),
        // Same crate-boundary loss as the line above, for a shield's own dye
        // tint rather than its loom patterns.
        base_color: stack.base_color().map(str::to_owned),
        // A custom head's own skin — the last of this family of losses, and the
        // one that stayed longest because its symptom is a *plausible* icon
        // rather than a missing one: without it every decorative head a server
        // places drew the default skull sheet in a slot while the same head was
        // correct once placed in the world. `stack_skin_url` also starts the
        // fetch; see its doc.
        skin: item_icon::stack_skin_url(stack),
    })
}

/// The stack-count ink on the **atlas-less** fallback path (the real path uses
/// [`item_icon::COUNT_INK`]). Named rather than inline so the recipe-panel
/// submission-order gate can find a count-digit vertex by the same constant the
/// draw writes, instead of restating the literal.
pub(crate) const FALLBACK_COUNT_INK: [f32; 4] = [0.98, 0.98, 0.92, 1.0];

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
pub(crate) struct Builder<'a> {
    w: f32,
    h: f32,
    pub(crate) verts: Vec<f32>,
    pub(crate) item_verts: Vec<f32>,
    /// The enchantment-glint copies of `item_verts`; see [`IconSink::glint`].
    pub(crate) glint_verts: Vec<f32>,
    pub(crate) model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons; see [`ContainerGeometry::special`].
    pub(crate) special: Vec<SpecialIconDraw>,
    /// Flat `[x, y, u, v, r, g, b, a]` per vertex, off
    /// [`ContainerBackground`]'s atlas.
    pub(crate) bg_verts: Vec<f32>,
    /// The vanilla proportional font, for stack counts. `None` on a jar-less
    /// run, where [`item_icon::draw_item_icon`] falls back to the fixed-advance
    /// 5×7 debug font — the same degradation the HUD's own text uses.
    pub(crate) font: Option<&'a VanillaFont>,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(w: f32, h: f32, font: Option<&'a VanillaFont>) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            item_verts: Vec::new(),
            glint_verts: Vec::new(),
            model_verts: Vec::new(),
            special: Vec::new(),
            bg_verts: Vec::new(),
            font,
        }
    }

    pub(crate) fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    /// A pixel-space rectangle with a vertical gradient from `top` (its own top
    /// edge) to `bottom` (its bottom edge) — see [`ColourStream::gradient_rect`].
    pub(crate) fn gradient_rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, top: [f32; 4], bottom: [f32; 4]) {
        self.colour().gradient_rect(x, y, w, h, top, bottom);
    }

    /// One [`GuiSpriteQuad`] onto the background stream, untinted.
    pub(crate) fn bg_sprite(&mut self, q: GuiSpriteQuad) {
        let (w, h) = (self.w, self.h);
        item_icon::push_sprite_quad(&mut self.bg_verts, w, h, q, [1.0, 1.0, 1.0, 1.0]);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().text(s, x, y, scale, c);
    }

    /// One of vanilla's two container labels: the **proportional** font when one
    /// is attached, and **no drop shadow** either way — the trailing `false` in
    /// vanilla's own container-screen text-draw calls. Every
    /// other text surface in this crate is shadowed, which is why this needs its
    /// own entry point rather than reusing `VanillaFont::draw`.
    ///
    /// Degrades to the fixed-advance 5×7 debug font on a jar-less run, the same
    /// way stack counts do — advances will be wrong, but the words are readable
    /// and the anchor is identical, so the geometry gate still measures the same
    /// thing.
    pub(crate) fn label(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
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
    ///, which defaults `dropShadow` to
    /// `true`, unlike [`label`](Self::label)'s explicit `false` for the two
    /// container labels. Degrades to the same fixed-advance debug font
    /// [`label`](Self::label) does on a jar-less run.
    pub(crate) fn shadowed_label(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let mut cs = self.colour();
                f.draw(&mut cs, s, x, y, scale, c);
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// The [`Self::shadowed_label`] sibling for a styled [`TextSpan`] list —
    /// the structured draw a caller holding [`Text`](lodestone_model::Text)
    /// output should prefer, for the same reason `hud::Builder::text_spans`
    /// exists over `text_legacy`: flattening to a `§`-coded `String` first
    /// drops any [`TextColor::Rgb`](lodestone_model::text::TextColor::Rgb),
    /// silently, before this module ever sees it. `c`'s RGB is the base
    /// colour an unstyled span (or `§r`) draws in; its alpha scales every run.
    pub(crate) fn shadowed_label_spans(&mut self, spans: &[TextSpan], x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let base = [c[0], c[1], c[2]];
        let alpha = c[3];
        match self.font {
            Some(f) => {
                let mut cs = self.colour();
                f.draw_spans(&mut cs, spans, x, y, scale, base, alpha);
            }
            None => self.colour().spans(spans, x, y, scale, base, alpha),
        }
    }

    /// Pixel width of `s` at `scale` in whichever font
    /// [`label`](Self::label)/[`shadowed_label`](Self::shadowed_label) will
    /// actually draw with — the vanilla proportional one when attached, the
    /// fixed-advance 5x7 debug font otherwise.
    ///
    /// Layout that measures with one font and draws with the other is wrong in
    /// exactly the way that is hardest to see: the words appear, in the wrong
    /// place, by an amount that depends on the string.
    pub(crate) fn text_width(&self, s: &str, scale: f32) -> f32 {
        match self.font {
            Some(f) => f.width(s, scale),
            None => item_icon::text_w(s, scale),
        }
    }

    /// The longest prefix of `s` that fits in `max_px` at `scale`, measured
    /// with [`text_width`](Self::text_width)'s font — vanilla's
    /// `Font.substrByWidth`.
    pub(crate) fn substr_by_width(&self, s: &str, max_px: f32, scale: f32) -> String {
        let mut out = String::new();
        for ch in s.chars() {
            let mut candidate = out.clone();
            candidate.push(ch);
            if self.text_width(&candidate, scale) > max_px {
                break;
            }
            out = candidate;
        }
        out
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
            glint: &mut self.glint_verts,
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
    pub(crate) fn draw_stack(&mut self, assets: &IconAssets<'_>, stack: &lodestone_game::item::ItemStack, x: f32, y: f32) {
        self.draw_stack_counted(assets, stack, x, y, item_icon::COUNT_INK);
    }

    /// As [`draw_stack`](Self::draw_stack), with an explicit stack-count ink. The
    /// drag preview uses [`item_icon::COUNT_INK_CLAMPED`] (vanilla's yellow) when
    /// the provisional count hit the destination cell's cap.
    pub(crate) fn draw_stack_counted(
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
            (Some(_), Some(record)) => self.item_icon_counted(assets, &record, x, y, CELL, count_ink),
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

#[cfg(test)]
mod tests {
    use super::{Builder, icon_record};

    /// The `stack -> HotbarSlot` hop: `icon_record` is what every container
    /// surface (chest, furnace, recipe panel, creative menu) actually reads
    /// `enchanted` off, so a correct `stack_has_foil` that never reached this
    /// field would still leave every one of those surfaces dark. Checks both
    /// the reported case (second half, an unenchanted
    /// `minecraft:enchanted_book`) and a plain item as the negative control.
    #[test]
    fn icon_record_carries_the_baked_glint_override_through_to_enchanted() {
        use lodestone_game::item::ItemStack;

        let book = ItemStack::new(
            "minecraft:enchanted_book".parse().expect("static id parses"),
            1,
        );
        let record = icon_record(&book).expect("enchanted_book is a valid ResourceLocation");
        assert!(
            record.enchanted,
            "an unenchanted enchanted_book's icon record must set enchanted"
        );

        let stick = ItemStack::new("minecraft:stick".parse().expect("static id parses"), 1);
        let record = icon_record(&stick).expect("stick is a valid ResourceLocation");
        assert!(!record.enchanted, "a plain stick must not set enchanted");
    }

    /// A modern server can keep the gameplay item as a diamond sword while
    /// directing its visual lookup to a pack-owned gun definition.
    #[test]
    fn icon_record_uses_the_stack_item_model_for_render_lookup() {
        use lodestone_game::item::ItemStack;

        let mut sword = ItemStack::new(
            "minecraft:diamond_sword".parse().expect("static id parses"),
            1,
        );
        sword.set_item_model(Some("server:gun".parse().expect("static id parses")));

        let record = icon_record(&sword).expect("diamond_sword is a valid ResourceLocation");
        assert_eq!(record.item.to_string(), "server:gun");
    }

    /// The **producer** half of the custom-head fix, which no pixel gate can
    /// reach: a pixel gate installs its own `ItemIcon` and so proves the draw
    /// and nothing about what builds the record in production. `icon_record` is
    /// that builder for every container surface, so this is where a dropped
    /// `minecraft:profile` would be invisible — and was, for as long as the
    /// field did not exist.
    ///
    /// Three claims, each with its own control:
    ///
    /// * a head carrying a `textures` property resolves to **that payload's**
    ///   URL, not merely to `Some`;
    /// * a **plain** head resolves to `None` — the control that makes the first
    ///   claim mean something, since a record that filled `skin` unconditionally
    ///   would pass a `Some`-only assertion;
    /// * the URL reaches `remote_skins::request`. Without the fetch the record
    ///   is correct and the head still draws the default sheet forever, which is
    ///   exactly the island shape that produced this bug one layer down.
    #[test]
    fn icon_record_carries_a_custom_heads_profile_skin_and_starts_its_fetch() {
        use lodestone_game::item::ItemStack;

        const URL: &str = "https://textures.minecraft.net/texture/icon-record-custom-head";

        // A local base64 encoder, so the fixture does not depend on which
        // base64 crate the workspace happens to expose — the same reason
        // `remote_skins`' own tests carry one.
        fn base64(bytes: &[u8]) -> String {
            const TABLE: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut out = String::new();
            for chunk in bytes.chunks(3) {
                let b = [
                    chunk[0],
                    chunk.get(1).copied().unwrap_or(0),
                    chunk.get(2).copied().unwrap_or(0),
                ];
                let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
                for i in 0..4 {
                    if i <= chunk.len() {
                        out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 0x3f) as usize]));
                    } else {
                        out.push('=');
                    }
                }
            }
            out
        }

        let payload = base64(
            format!(r#"{{"textures":{{"SKIN":{{"url":"{URL}"}}}}}}"#).as_bytes(),
        );

        let mut custom = ItemStack::new(
            "minecraft:player_head".parse().expect("static id parses"),
            1,
        );
        custom.set_profile(Some(lodestone_model::ItemProfile {
            name: Some("Notch".to_owned()),
            id: None,
            properties: vec![lodestone_model::ProfileProperty {
                name: "textures".to_owned(),
                value: payload,
                signature: None,
            }],
        }));

        let record = icon_record(&custom).expect("player_head is a valid ResourceLocation");
        assert_eq!(
            record.skin.as_deref(),
            Some(URL),
            "a custom head's icon record must carry its own texture url"
        );

        let plain = ItemStack::new(
            "minecraft:player_head".parse().expect("static id parses"),
            1,
        );
        assert!(
            icon_record(&plain)
                .expect("player_head is a valid ResourceLocation")
                .skin
                .is_none(),
            "control: a plain head declares no profile, so it must resolve to \
             None and draw the default skull sheet"
        );

        assert!(
            crate::remote_skins::requested_urls().iter().any(|u| u == URL),
            "the record carried the url but nothing started its fetch, so the \
             skin could never arrive and the head would draw the default sheet \
             forever"
        );
    }

    /// [`Builder::shadowed_label_spans`] — the tooltip title's real draw
    /// call (`container::tooltip::emit_tooltip_for_stack`) — must put a
    /// hex, a named and an inline-`§`-coded run into pairwise-distinct
    /// vertex RGBs, on the jar-less path (`font: None`) this unit test can
    /// exercise without a real `client.jar`. A control through
    /// [`Builder::shadowed_label`] (the `String`/`Text::to_legacy_string`
    /// path `styled_hover_name` used before its spans sibling existed)
    /// proves the loss really lives in that legacy conversion path, not in
    /// `shadowed_label_spans`'s own vertex output.
    #[test]
    fn shadowed_label_spans_carries_hex_named_and_inline_legacy_colour_to_distinct_vertices() {
        use lodestone_model::text::{Text, TextColor, TextContent, TextStyle};

        let hex = Text {
            content: TextContent::Literal("Hex".to_string()),
            style: TextStyle {
                font: None,
                color: Some(TextColor::Rgb(0x1a_2b3c)),
                ..TextStyle::default()
            },
            ..Text::default()
        };
        let inline_legacy = Text::literal("\u{00a7}cRed");
        let named = Text {
            content: TextContent::Literal("Gray".to_string()),
            style: TextStyle {
                font: None,
                color: Some(TextColor::Gray),
                ..TextStyle::default()
            },
            ..Text::default()
        };
        let root = Text {
            extra: vec![hex, inline_legacy, named],
            ..Text::default()
        };
        let spans = root.resolve(&|_| None).to_spans();
        assert_eq!(spans.len(), 3, "sanity: three runs in, three runs out — {spans:?}");

        let mut b = Builder::new(200.0, 100.0, None);
        b.shadowed_label_spans(&spans, 0.0, 0.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        assert!(!b.verts.is_empty(), "sanity: the label must draw something at all");

        let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
        let has_colour = |verts: &[f32], rgb: (u8, u8, u8)| {
            verts
                .chunks_exact(6)
                .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
        };
        let expected = [
            ("hex", (0x1a_u8, 0x2b_u8, 0x3c_u8)),
            ("inline §c", (0xff_u8, 0x55_u8, 0x55_u8)),
            ("named gray", (0xaa_u8, 0xaa_u8, 0xaa_u8)),
        ];
        let missing: Vec<&str> = expected
            .iter()
            .filter(|(_, rgb)| !has_colour(&b.verts, *rgb))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            missing.is_empty(),
            "these colours never reached a vertex: {missing:?} (full expected set: {expected:?})"
        );

        // Control: the same three-way name, but through
        // `shadowed_label`/`Text::to_legacy_string`, which has no
        // representation for `TextColor::Rgb`. Must show the loss, or the
        // assertion above proves nothing about which call actually carries
        // the colour.
        let flattened = root.resolve(&|_| None).to_legacy_string();
        let mut legacy_b = Builder::new(200.0, 100.0, None);
        legacy_b.shadowed_label(&flattened, 0.0, 0.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        assert!(
            !has_colour(&legacy_b.verts, (0x1a, 0x2b, 0x3c)),
            "control failed: the legacy-string path was expected to lose the hex colour \
             (that is the bug), but it drew it anyway — this test's premise is wrong"
        );
    }
}
