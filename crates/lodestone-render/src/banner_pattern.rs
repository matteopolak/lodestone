//! Banner and shield pattern-layer compositing.
//!
//! # What this module is
//!
//! This is the **shared compositing/colour math** consumed by both the
//! banner block-entity mesh and item-icon/held-item rendering, so that
//! logic is written exactly once rather than duplicated per consumer. It
//! does **not** draw a banner or a shield itself: given a base colour and
//! an ordered list of pattern layers, it produces the ordered, coloured
//! draw list a consumer (a block-entity mesh, an inventory icon, a
//! held-item pose) turns into draw calls over its own mesh.
//!
//! # Vanilla's mechanism (not a texture composite — a draw list)
//!
//! Vanilla's own pattern-submit function
//! (banner-renderer's own decompiled source, 26.2)
//! does **not** pre-composite a texture on the CPU. It draws the **same**
//! flag/shield mesh once per layer, each time sampling a different mask
//! sprite (white = covered, transparent = not) and tinted by that layer's
//! own flat colour:
//!
//! 1. Layer 0 is always the *base* mask (vanilla's own sheet-mapper constants
//!    for the banner and shield base mask, i.e. `entity/banner/base` or
//!    `entity/shield/base` — a solid-covered mask), tinted by the banner's
//!    own base colour.
//! 2. Then, for up to [`MAX_PATTERN_LAYERS`] (16) pattern layers in the
//!    item's own stored order, each layer's mask sprite
//!    (`entity/banner/<pattern-asset-id>` / `entity/shield/<pattern-asset-id>`,
//!    resolved by vanilla's own banner/shield sprite lookup functions,
//!    banner-renderer's own decompiled source) is drawn, tinted by *that layer's* dye colour.
//!
//! So the "compositing" is draw order plus a per-layer tint, not pixel
//! blending on the CPU — [`banner_pattern_layers`]/[`shield_pattern_layers`]
//! return exactly that ordered list, ready for a caller to turn into that
//! many draw calls (or, if a future caller genuinely wants one baked
//! texture instead — e.g. to avoid 17 draw calls for one item icon — the
//! same ordered list is what a CPU compositor would walk too; this module
//! does not force either choice).
//!
//! # Colour is gamma space, tint only (no mask blending here)
//!
//! Every colour this module returns is vanilla's own raw per-dye diffuse
//! colour field (its dye-colour registration's own decompiled source), i.e. **gamma-space** sRGB bytes normalised to
//! `0.0..=1.0` — the same convention every other tint in this codebase
//! uses (`CLAUDE.md`: "vanilla is not colour-managed... tint and shade
//! multiply in gamma space"). A caller's shader must multiply this by the
//! sampled mask texel in gamma space, exactly like `model_pipeline.wgsl`'s
//! `srgb_to_linear(linear_to_srgb(rgb) * tint)` round-trip
//! (`crates/lodestone-render/src/screen_effects.rs`'s `overlay.wgsl` is the
//! same pattern for a full-screen quad instead of a masked one). Doing this
//! multiply in linear light would wash out every banner and shield in the
//! game, the same failure mode already documented for every other tint
//! here.
//!
//! # Consumers
//!
//! `minecraft:banner_patterns` and `minecraft:base_color` decode as typed
//! [`lodestone_game::item::ComponentValue`] variants, so a caller resolves a
//! stack's stored pattern list and dye colour directly rather than through
//! the generic opaque-component fallback. The banner block-entity mesh and
//! item-icon/held-item rendering both call [`banner_pattern_layers`] (or
//! [`shield_pattern_layers`] for a shield's decorative pattern) with the
//! decoded base colour and pattern list, get back the ordered
//! `(sprite, gamma_rgb)` draw list, and issue one draw per entry over their
//! own mesh — no layer math duplicated between the two.

use lodestone_assets::ResourceLocation;

/// Vanilla's 16 dye-colour variants, in enum-declaration order
/// (vanilla's own dye-colour registration's decompiled source — id `0..=15`, white first, black last). Used
/// as the type for both a banner's base colour and each pattern layer's
/// colour; vanilla's own pattern-layer record type pairs a pattern with
/// exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DyeColor {
    /// `id = 0`.
    White,
    /// `id = 1`.
    Orange,
    /// `id = 2`.
    Magenta,
    /// `id = 3`.
    LightBlue,
    /// `id = 4`.
    Yellow,
    /// `id = 5`.
    Lime,
    /// `id = 6`.
    Pink,
    /// `id = 7`.
    Gray,
    /// `id = 8`.
    LightGray,
    /// `id = 9`.
    Cyan,
    /// `id = 10`.
    Purple,
    /// `id = 11`.
    Blue,
    /// `id = 12`.
    Brown,
    /// `id = 13`.
    Green,
    /// `id = 14`.
    Red,
    /// `id = 15`.
    Black,
}

/// Vanilla's own per-dye diffuse-colour field, per [`DyeColor`] — the constructor
/// argument immediately after each variant's display name in
/// vanilla's own dye-colour registration's decompiled source, e.g. white's own entry pairs
/// id `0`, name `"white"` and diffuse colour `16383998`. Packed
/// `0x00RRGGBB`, **gamma-space** sRGB bytes (see the module doc's gamma
/// note) — this is the colour vanilla's own per-layer submit step passes as
/// the diffuse colour to tint a mask sprite, not a linear-light value.
const DYE_TEXTURE_DIFFUSE_COLOR: [u32; 16] = [
    0x00F9_FFFE, // white      16383998
    0x00F9_801D, // orange     16351261
    0x00C7_4EBD, // magenta    13061821
    0x003A_B3DA, // light_blue  3847130
    0x00FE_D83D, // yellow     16701501
    0x0080_C71F, // lime        8439583
    0x00F3_8BAA, // pink       15961002
    0x0047_4F52, // gray        4673362
    0x009D_9D97, // light_gray 10329495
    0x0016_9C9C, // cyan        1481884
    0x008932_B8, // purple      8991416
    0x003C_44AA, // blue        3949738
    0x0083_5432, // brown       8606770
    0x005E_7C16, // green       6192150
    0x00B0_2E26, // red        11546150
    0x001D_1D21, // black       1908001
];

impl DyeColor {
    /// All 16 colours in vanilla enum order.
    pub const ALL: [DyeColor; 16] = [
        DyeColor::White,
        DyeColor::Orange,
        DyeColor::Magenta,
        DyeColor::LightBlue,
        DyeColor::Yellow,
        DyeColor::Lime,
        DyeColor::Pink,
        DyeColor::Gray,
        DyeColor::LightGray,
        DyeColor::Cyan,
        DyeColor::Purple,
        DyeColor::Blue,
        DyeColor::Brown,
        DyeColor::Green,
        DyeColor::Red,
        DyeColor::Black,
    ];

    /// Vanilla's own dye-colour id accessor — `0..=15`, `White` first.
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// Vanilla's own snake_case name accessor,
    /// e.g. `"light_blue"` — matches `minecraft:light_blue_dye`'s own path and
    /// vanilla's own pattern-layers wire encoding, which stores the colour by this
    /// name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            DyeColor::White => "white",
            DyeColor::Orange => "orange",
            DyeColor::Magenta => "magenta",
            DyeColor::LightBlue => "light_blue",
            DyeColor::Yellow => "yellow",
            DyeColor::Lime => "lime",
            DyeColor::Pink => "pink",
            DyeColor::Gray => "gray",
            DyeColor::LightGray => "light_gray",
            DyeColor::Cyan => "cyan",
            DyeColor::Purple => "purple",
            DyeColor::Blue => "blue",
            DyeColor::Brown => "brown",
            DyeColor::Green => "green",
            DyeColor::Red => "red",
            DyeColor::Black => "black",
        }
    }

    /// Parses vanilla's snake_case name back into a [`DyeColor`] — the
    /// inverse of [`Self::name`], for a caller reading `minecraft:base_color`
    /// or a layer's stored colour once it is decoded.
    #[must_use]
    pub fn from_name(name: &str) -> Option<DyeColor> {
        DyeColor::ALL.into_iter().find(|c| c.name() == name)
    }

    /// Vanilla's own per-dye diffuse-colour field, packed `0x00RRGGBB`, gamma-space sRGB
    /// bytes — see [`DYE_TEXTURE_DIFFUSE_COLOR`]'s doc.
    #[must_use]
    pub const fn packed_rgb(self) -> u32 {
        DYE_TEXTURE_DIFFUSE_COLOR[self.id() as usize]
    }

    /// [`Self::packed_rgb`] unpacked to gamma-space `[r, g, b]` in
    /// `0.0..=1.0` — the form a caller multiplies a sampled mask texel by
    /// (see the module doc's gamma note; **do not** convert this to linear
    /// before the multiply).
    #[must_use]
    pub fn gamma_rgb(self) -> [f32; 3] {
        let packed = self.packed_rgb();
        [
            ((packed >> 16) & 0xFF) as f32 / 255.0,
            ((packed >> 8) & 0xFF) as f32 / 255.0,
            (packed & 0xFF) as f32 / 255.0,
        ]
    }
}

/// Quantises a gamma-space `[r, g, b]` in `0.0..=1.0` (a [`PatternLayer::color`],
/// or [`DyeColor::gamma_rgb`] directly) to the `[u8; 3]` bytes
/// [`lodestone_render::InstanceTint::rgb`](crate::InstanceTint::rgb) and
/// `upload_instances_tinted` want. Shared by every consumer that turns a
/// resolved layer into an instance tint — the world block-entity pass, the
/// first-person hand and the GUI/inventory icon — so the clamp-then-round
/// only has one implementation to get right.
#[must_use]
pub fn gamma_rgb_to_bytes(rgb: [f32; 3]) -> [u8; 3] {
    rgb.map(|c| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=255 first"
        )]
        {
            (c.clamp(0.0, 1.0) * 255.0).round() as u8
        }
    })
}

/// The maximum number of pattern layers vanilla ever draws
/// (vanilla's own named pattern-renderer constant for this, banner-renderer's own decompiled source — `= 16`) —
/// vanilla's own pattern-submit loop bound (banner-renderer's own decompiled source:
/// the mask index must stay below both 16 and the stack's own stored layer count). A stack
/// carrying more layers than this (vanilla itself refuses to add a 17th in
/// survival, but a command or a foreign save could still produce one) has
/// every layer past the 16th silently dropped, matching vanilla exactly
/// rather than drawing an unbounded stack.
pub const MAX_PATTERN_LAYERS: usize = 16;

/// One resolved layer to draw, back-to-front: which masked sprite to sample
/// and the gamma-space colour to tint it by. `sprite` is a full
/// [`ResourceLocation`] under `textures/…` (e.g.
/// `minecraft:entity/banner/base` or `minecraft:entity/banner/creeper`), not
/// a bare pattern id, so a caller never has to re-derive the
/// `entity/banner/<id>` vs `entity/shield/<id>` convention itself.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternLayer {
    /// The masked sprite to sample, resolved against `textures/…`.
    pub sprite: ResourceLocation,
    /// Gamma-space `[r, g, b]` in `0.0..=1.0` to tint the sampled texel by
    /// (see the module doc's gamma note).
    pub color: [f32; 3],
}

/// One item-stored pattern layer: a pattern's asset id (e.g. `"creeper"`,
/// matching `assets/minecraft/data/minecraft/banner_pattern/creeper.json`'s
/// own `asset_id` field) plus the dye colour it is drawn in. A caller
/// decoding `minecraft:banner_patterns` builds a `Vec` of these, in the
/// stack's own stored order — vanilla draws them in exactly that order and
/// no other.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPatternLayer {
    /// The pattern's asset id, e.g. `"creeper"` — not a full
    /// [`ResourceLocation`], since vanilla's own pattern asset-id accessor is
    /// the un-namespaced path component the sheet mapper appends
    /// (vanilla's own sprite-sheet mapper's decompiled source, whose sprite mapper adds the `"entity/banner"` /
    /// `"entity/shield"` prefix).
    pub pattern_asset_id: String,
    /// The dye colour this layer is drawn in.
    pub color: DyeColor,
}

fn banner_or_shield_layers(
    base_color: DyeColor,
    patterns: &[StoredPatternLayer],
    mask_namespace: &str,
) -> Vec<PatternLayer> {
    let mut out = Vec::with_capacity(1 + patterns.len().min(MAX_PATTERN_LAYERS));
    // Layer 0: the base mask, tinted by the banner/shield's own base colour
    // — vanilla's own named base-mask sprite constants for banner and shield are always
    // `<mapper-prefix>/base`, regardless of pattern list (sprite-sheet mapper's own decompiled source).
    out.push(PatternLayer {
        sprite: ResourceLocation::new("minecraft", format!("{mask_namespace}/base"))
            .expect("static path is always valid"),
        color: base_color.gamma_rgb(),
    });
    // Then up to MAX_PATTERN_LAYERS pattern layers, in stored order —
    // vanilla's own pattern-renderer's decompiled source's exact loop bound.
    for layer in patterns.iter().take(MAX_PATTERN_LAYERS) {
        out.push(PatternLayer {
            sprite: ResourceLocation::new(
                "minecraft",
                format!("{mask_namespace}/{}", layer.pattern_asset_id),
            )
            .expect("pattern asset ids are valid resource-location paths"),
            color: layer.color.gamma_rgb(),
        });
    }
    out
}

/// Resolves a banner's full ordered draw list — base mask first (tinted by
/// `base_color`), then up to [`MAX_PATTERN_LAYERS`] pattern layers in
/// `patterns`' own order — mirroring
/// vanilla's own pattern-submit function's banner branch
/// (banner-renderer's own decompiled source) exactly. Sprites resolve under
/// `entity/banner/…` (vanilla's own named banner sprite mapper, sprite-sheet mapper's own decompiled source).
#[must_use]
pub fn banner_pattern_layers(base_color: DyeColor, patterns: &[StoredPatternLayer]) -> Vec<PatternLayer> {
    banner_or_shield_layers(base_color, patterns, "entity/banner")
}

/// Resolves a shield's full ordered draw list — same algorithm as
/// [`banner_pattern_layers`], but sprites resolve under `entity/shield/…`
/// (vanilla's own named shield sprite mapper, sprite-sheet mapper's own decompiled source) instead, per
/// vanilla's own pattern-submit function's shield branch
/// (banner-renderer's own decompiled source).
#[must_use]
pub fn shield_pattern_layers(base_color: DyeColor, patterns: &[StoredPatternLayer]) -> Vec<PatternLayer> {
    banner_or_shield_layers(base_color, patterns, "entity/shield")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(id: &str, color: DyeColor) -> StoredPatternLayer {
        StoredPatternLayer {
            pattern_asset_id: id.to_string(),
            color,
        }
    }

    /// Every one of the 16 diffuse-colour decimal constants,
    /// transcribed straight from vanilla's own dye-colour registration's decompiled source's constructor calls
    /// (the *decimal* literal, not the hex table above) and checked against
    /// every entry in one pass — the hex table was hand re-typed from these
    /// same decimals once already and had one digit wrong (lime's
    /// `0x80C71C` vs the jar's `0x80C71F`) until this test was added; a
    /// spot-check of 3 entries had not caught it.
    #[test]
    fn every_dye_diffuse_color_matches_its_jar_decimal_constant() {
        let expected: [(DyeColor, u32); 16] = [
            (DyeColor::White, 16_383_998),
            (DyeColor::Orange, 16_351_261),
            (DyeColor::Magenta, 13_061_821),
            (DyeColor::LightBlue, 3_847_130),
            (DyeColor::Yellow, 16_701_501),
            (DyeColor::Lime, 8_439_583),
            (DyeColor::Pink, 15_961_002),
            (DyeColor::Gray, 4_673_362),
            (DyeColor::LightGray, 10_329_495),
            (DyeColor::Cyan, 1_481_884),
            (DyeColor::Purple, 8_991_416),
            (DyeColor::Blue, 3_949_738),
            (DyeColor::Brown, 8_606_770),
            (DyeColor::Green, 6_192_150),
            (DyeColor::Red, 11_546_150),
            (DyeColor::Black, 1_908_001),
        ];
        for (color, decimal) in expected {
            assert_eq!(
                color.packed_rgb(),
                decimal,
                "{color:?}: jar decimal {decimal} disagrees with packed_rgb 0x{:06X}",
                color.packed_rgb()
            );
        }
    }

    #[test]
    fn gamma_rgb_is_normalised_and_matches_packed_bytes() {
        let [r, g, b] = DyeColor::Red.gamma_rgb();
        let packed = DyeColor::Red.packed_rgb();
        assert!((r - ((packed >> 16) & 0xFF) as f32 / 255.0).abs() < 1e-6);
        assert!((g - ((packed >> 8) & 0xFF) as f32 / 255.0).abs() < 1e-6);
        assert!((b - (packed & 0xFF) as f32 / 255.0).abs() < 1e-6);
        assert!((0.0..=1.0).contains(&r) && (0.0..=1.0).contains(&g) && (0.0..=1.0).contains(&b));
    }

    #[test]
    fn name_round_trips_for_every_colour() {
        for c in DyeColor::ALL {
            assert_eq!(DyeColor::from_name(c.name()), Some(c), "{c:?} must round-trip through its name");
        }
    }

    #[test]
    fn id_matches_declaration_order() {
        for (i, c) in DyeColor::ALL.into_iter().enumerate() {
            assert_eq!(c.id() as usize, i);
        }
    }

    #[test]
    fn no_patterns_still_draws_the_base_layer() {
        let layers = banner_pattern_layers(DyeColor::Blue, &[]);
        assert_eq!(layers.len(), 1, "the base mask is always drawn, even with zero patterns");
        assert_eq!(layers[0].sprite, ResourceLocation::parse("minecraft:entity/banner/base").unwrap());
        assert_eq!(layers[0].color, DyeColor::Blue.gamma_rgb());
    }

    #[test]
    fn base_layer_is_always_first_and_tinted_by_base_color_not_the_first_pattern() {
        let patterns = vec![layer("creeper", DyeColor::Lime)];
        let layers = banner_pattern_layers(DyeColor::White, &patterns);
        assert_eq!(layers[0].color, DyeColor::White.gamma_rgb(), "layer 0 is the base colour");
        assert_eq!(layers[1].color, DyeColor::Lime.gamma_rgb(), "layer 1 is the pattern's own colour");
    }

    #[test]
    fn pattern_order_is_preserved_exactly() {
        let patterns = vec![layer("stripe_top", DyeColor::Red), layer("circle", DyeColor::Black)];
        let layers = banner_pattern_layers(DyeColor::White, &patterns);
        assert_eq!(layers.len(), 3);
        assert_eq!(
            layers[1].sprite,
            ResourceLocation::parse("minecraft:entity/banner/stripe_top").unwrap()
        );
        assert_eq!(layers[2].sprite, ResourceLocation::parse("minecraft:entity/banner/circle").unwrap());
    }

    #[test]
    fn caps_at_sixteen_pattern_layers_plus_the_base() {
        let patterns: Vec<StoredPatternLayer> = (0..20).map(|i| layer(&format!("p{i}"), DyeColor::Cyan)).collect();
        let layers = banner_pattern_layers(DyeColor::White, &patterns);
        assert_eq!(
            layers.len(),
            1 + MAX_PATTERN_LAYERS,
            "vanilla's own MAX_PATTERNS=16 loop bound must be respected, not the stack's actual count"
        );
        // The *first* 16 are kept, not the last 16 or an arbitrary subset.
        assert_eq!(layers[1].sprite, ResourceLocation::parse("minecraft:entity/banner/p0").unwrap());
        assert_eq!(layers[16].sprite, ResourceLocation::parse("minecraft:entity/banner/p15").unwrap());
    }

    #[test]
    fn banner_and_shield_use_different_sprite_namespaces_for_identical_input() {
        let patterns = vec![layer("creeper", DyeColor::Lime)];
        let banner = banner_pattern_layers(DyeColor::White, &patterns);
        let shield = shield_pattern_layers(DyeColor::White, &patterns);
        assert_eq!(banner[0].sprite, ResourceLocation::parse("minecraft:entity/banner/base").unwrap());
        assert_eq!(shield[0].sprite, ResourceLocation::parse("minecraft:entity/shield/base").unwrap());
        assert_eq!(banner[1].sprite, ResourceLocation::parse("minecraft:entity/banner/creeper").unwrap());
        assert_eq!(shield[1].sprite, ResourceLocation::parse("minecraft:entity/shield/creeper").unwrap());
        // Colours are identical -- only the sprite namespace differs.
        assert_eq!(banner[0].color, shield[0].color);
        assert_eq!(banner[1].color, shield[1].color);
    }
}
