//! Evaluating an item model's `tints` list to a concrete ARGB colour.
//!
//! # What it is
//!
//! [`item_model`](crate::item_model) *parses* an item definition's `tints` array
//! into [`TintSource`]s but deliberately never evaluates one, because evaluating
//! needs runtime state (the stack's data components, the pack's grass colormap)
//! that the parser does not own. This module is the missing half: it maps one
//! [`TintSource`] plus an [`ItemTintContext`] to the packed ARGB that the item
//! model's tinted layer multiplies.
//!
//! Faithful to `net.minecraft.client.color.item` in the de-obfuscated 26.2
//! client — **not** `net.minecraft.client.renderer.item.tint`, which does not
//! exist. The dispatch table is `ItemTintSources.bootstrap` and the interface
//! is `ItemTintSource.calculate(ItemStack, ClientLevel, LivingEntity)`.
//!
//! # How it works
//!
//! [`resolve`] switches on the source's namespaced id and returns a
//! [`ResolvedTint`]: the ARGB, plus a [`TintProvenance`] recording *where the
//! number came from*. The provenance is the point. Six of vanilla's eight tint
//! sources read a data component, and this client models only one of them
//! ([`ItemComponents::dyed_color`]), so most item tints necessarily resolve to
//! the item definition's own JSON `default` rather than to live stack state. That
//! is not a bug and it is not a guess — it is vanilla's own fallback for an
//! absent component, and it is the *correct* colour for an ordinary uncustomised
//! stack, which is the overwhelming majority. But "correct because the component
//! is absent" and "correct because this build cannot see the component" are
//! different states, and a caller that cannot tell them apart cannot report
//! honestly on what it is drawing. Hence
//! [`TintProvenance::Unmodeled`](TintProvenance::Unmodeled).
//!
//! # The trap this module exists to avoid, and it is not the obvious one
//!
//! The obvious trap is gamma (see below). The *expensive* trap is that vanilla's
//! item tints and vanilla's **block** tints are two unrelated mechanisms and the
//! item renderer never consults the block one. `CuboidItemModelWrapper`'s layer
//! loop evaluates the item definition's own `tints` list per layer; nothing on the
//! item path calls `BlockColors`. Substituting the block table for the item list
//! is wrong wherever the two disagree, and they do disagree: `lily_pad`'s item
//! definition is `constant 0x71C35C` while `BlockColors` gives it
//! `LILY_PAD_IN_WORLD` = `0x208030`. It happens to *agree* for leaves
//! (`0x48B518` both ways) and for `grass_block`, which is exactly why the
//! substitution survived review.
//!
//! # Gamma
//!
//! Every colour here is an sRGB multiplier in gamma space, to be applied as
//! `srgb_to_linear(linear_to_srgb(texel) * tint)`. Multiplying in linear light
//! pulls every factor toward `1.0` and washes the item out — the same rule as
//! every other tint in this workspace. This module returns the raw bytes and
//! does no colour maths at all, precisely so there is one place downstream
//! (`lodestone_render::fog::multiply_gamma`) that owns the round-trip.

use crate::item_model::TintSource;
use crate::tint::Colormap;
use lodestone_model::item::ItemComponents;

/// Jar-derived default colours, each the value vanilla's own no-argument
/// constructor or `DEFAULT` constant supplies. Every one is a full ARGB with
/// alpha `0xFF`; see each constant for the vanilla symbol it came from.
pub mod defaults {
    /// `PotionContents.BASE_POTION_COLOR` and the
    /// `default` every one of vanilla's four potion item definitions carries
    /// (`potion`, `splash_potion`, `lingering_potion`, `tipped_arrow`).
    ///
    /// Note this is **not** the pre-1.21 `0xF800F8` magic value, which no longer
    /// exists anywhere in 26.2.
    pub const POTION_BASE: u32 = 0xFF38_5DC6;

    /// `MapItemColor.DEFAULT` (`new MapItemColor(4603950)`)
    /// and the `default` on `filled_map.json`'s `map_color` layer.
    pub const MAP: u32 = 0xFF46_402E;

    /// `DyedItemColor.LEATHER_COLOR` (`-6265536`) and the
    /// `default` on all six vanilla `dye` item definitions.
    pub const LEATHER: u32 = 0xFFA0_6540;

    /// `Firework`'s no-argument default (`-7697782`) and
    /// the `default` on `firework_star.json`'s `firework` layer.
    pub const FIREWORK: u32 = 0xFF8A_8A8A;

    /// `Potion`'s no-argument default — identical to
    /// [`POTION_BASE`], kept separate because they are separate declarations in
    /// the jar and could drift.
    pub const POTION_SOURCE: u32 = 0xFF38_5DC6;

    /// `ColorMapColorUtil.get`'s out-of-range fallback as reached through
    /// `GrassColor.get` (`-65281`). Magenta, i.e.
    /// deliberately loud.
    pub const GRASS_COLORMAP_FALLBACK: u32 = 0xFFFF_00FF;

    /// The climate inputs every vanilla `minecraft:grass` item tint carries
    /// (`grass_block`, `short_grass`, `tall_grass`, `fern`, `large_fern`,
    /// `bush`), and `GrassColorSource`'s own no-argument default. Plains.
    pub const GRASS_CLIMATE: [f32; 2] = [0.5, 1.0];
}

/// Where a [`ResolvedTint`]'s colour came from — the axis a caller needs to
/// report honestly on what it drew.
///
/// A zero count of [`Component`](Self::Component) distinguishes "no stack in
/// this render carried a customising component" from "this build cannot read the
/// component at all"; only the latter shows up as
/// [`Unmodeled`](Self::Unmodeled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TintProvenance {
    /// Read from a data component the wire actually carried and this build
    /// models — today that means `minecraft:dyed_color` and nothing else.
    Component,
    /// The tint source's own JSON `default` (or `minecraft:constant`'s `value`),
    /// used because the component it reads is genuinely absent from the stack.
    /// This is vanilla's own behaviour and the right colour for a plain stack.
    Default,
    /// The tint source's JSON `default`, used because the component it reads is
    /// **not modeled by this build** (see [`ItemComponents`]'s doc: components a
    /// build does not understand are dropped at decode). Vanilla might have
    /// shown a different colour here for a customised stack; for an
    /// uncustomised one this is identical to [`Default`](Self::Default).
    Unmodeled,
    /// Sampled from the pack's grass colormap. Reads no component at all.
    Colormap,
}

/// One evaluated tint: the packed ARGB and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTint {
    /// Packed `0xAARRGGBB`. Alpha is `0xFF` for every source whose vanilla
    /// implementation wraps its result in `ARGB.opaque`; for `dye`'s and
    /// `firework`'s fallback paths it is whatever the JSON `default` carried,
    /// because vanilla does **not** force alpha on those two (see
    /// [`resolve`]'s doc table).
    pub argb: u32,
    /// Where [`Self::argb`] came from.
    pub provenance: TintProvenance,
}

impl ResolvedTint {
    /// The `0x00RRGGBB` half, discarding alpha — the form a gamma-space
    /// multiplier takes. Item tints are opaque multipliers in every vanilla
    /// case; a downstream palette that cannot carry alpha should use this and
    /// say so rather than silently truncating.
    #[must_use]
    pub const fn rgb(self) -> u32 {
        self.argb & 0x00FF_FFFF
    }
}

/// The runtime state [`resolve`] evaluates a [`TintSource`] against.
///
/// Both fields are optional and a `None` is a *fact*, not a placeholder: no
/// components means a bake-time resolution with no stack in hand (which is
/// exactly the item-atlas/geometry-bake case, and yields vanilla's default for
/// every source), and no colormap means the pack shipped no
/// `textures/colormap/grass.png`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ItemTintContext<'a> {
    /// The stack's modeled components, or `None` when resolving with no stack.
    pub components: Option<&'a ItemComponents>,
    /// The pack's `textures/colormap/grass.png`, for `minecraft:grass`.
    pub grass_colormap: Option<&'a Colormap>,
}

/// `ARGB.opaque`: force alpha to `0xFF`.
const fn opaque(rgb: u32) -> u32 {
    rgb | 0xFF00_0000
}

/// Strips the `minecraft:` namespace so `minecraft:dye` and a
/// (technically invalid, but harmless to accept) bare `dye` behave alike.
fn strip_ns(kind: &str) -> &str {
    kind.strip_prefix("minecraft:").unwrap_or(kind)
}

/// Evaluate one item-model tint source against `ctx`, or `None` when nothing
/// should be applied (an unknown tint type, or a source whose inputs are all
/// missing).
///
/// # Per-source behaviour, and which ones force alpha
///
/// The alpha column is not decoration — vanilla is inconsistent about it, and
/// two sources return their JSON `default` *without* `ARGB.opaque`:
///
/// | id | component read | alpha forced | jar symbol |
/// |---|---|---|---|
/// | `constant` | none | yes, at construction | `Constant`'s canonical constructor |
/// | `dye` | `dyed_color` | only when present | `Dye.calculate`, `DyedItemColor.getOrDefault` |
/// | `grass` | none (colormap) | yes, here | `GrassColorSource.calculate` |
/// | `firework` | `firework_explosion` | only when present | `Firework.calculate` |
/// | `potion` | `potion_contents` | always | `Potion.calculate` |
/// | `map_color` | `map_color` | always | `MapColor.calculate` |
/// | `team` | none (needs a holder) | always | `TeamColor.calculate` |
/// | `custom_model_data` | `custom_model_data` | always | `CustomModelDataSource.calculate` |
///
/// # Which of these can ever be live here
///
/// Only `dye`. [`ItemComponents`] models `dyed_color` and none of
/// `potion_contents`, `map_color`, `firework_explosion` or `custom_model_data`,
/// so those four are reported [`TintProvenance::Unmodeled`] and resolve to the
/// definition's `default`. `team` needs a `LivingEntity` holder that an item
/// icon does not have — vanilla itself takes the default when `owner == null`
/// (`TeamColor.calculate`), so [`TintProvenance::Default`] is the honest label
/// there rather than `Unmodeled`.
///
/// **`minecraft:spawn_egg` is deliberately absent, and that is not an
/// omission.** 26.2 has no spawn-egg tint source: `SpawnEggItem`'s whole class
/// body has no colour fields,
/// `assets/minecraft/items/creeper_spawn_egg.json` carries no `tints` array, and
/// the two historical background/highlight colours are now baked as pixels into
/// per-mob textures. There is no integer to resolve.
#[must_use]
pub fn resolve(source: &TintSource, ctx: &ItemTintContext<'_>) -> Option<ResolvedTint> {
    // The JSON int is signed (vanilla writes `-13083194` for `0xFF385DC6`), so
    // reinterpret the bits rather than converting the value.
    let default = source.default.map(|v| v as u32);
    let tint = |argb: u32, provenance: TintProvenance| {
        Some(ResolvedTint {
            argb,
            provenance,
        })
    };

    match strip_ns(&source.kind) {
        // `Constant`'s canonical constructor forces alpha, so
        // `calculate` is opaque regardless of what the JSON said.
        "constant" => tint(opaque(default?), TintProvenance::Default),

        // The one source this build can answer from live state.
        // `DyedItemColor.getOrDefault`: present →
        // `ARGB.opaque(rgb())`, absent → the raw default *unmasked*.
        "dye" => match ctx.components.and_then(|c| c.dyed_color) {
            // `dyed_color` is the raw wire int (see `ItemComponents::dyed_color`),
            // and `DyedItemColor.rgb()` is used directly, so mask to 24 bits
            // before forcing alpha rather than trusting the wire's top byte.
            Some(rgb) => tint(opaque(rgb & 0x00FF_FFFF), TintProvenance::Component),
            None => tint(default?, TintProvenance::Default),
        },

        // `GrassColorSource.calculate` → `GrassColor.get(temperature, downfall)`.
        // Reads no component. The colormap PNG is opaque in vanilla, and our
        // `Colormap` drops alpha on load, so force it.
        "grass" => {
            let [temperature, downfall] = source.grass.unwrap_or(defaults::GRASS_CLIMATE);
            match ctx.grass_colormap {
                Some(map) => tint(
                    opaque(map.sample(temperature, downfall)),
                    TintProvenance::Colormap,
                ),
                // No colormap in the pack. `grass` has no JSON `default` field
                // at all, so there is nothing to fall back to but vanilla's own
                // out-of-range fallback — and reporting that loud magenta as if
                // it were a real colour would be worse than not tinting. Zero
                // with no vanilla pack is the honest degradation.
                None => None,
            }
        }

        // `Firework.calculate`: no `firework_explosion` → the raw default, *not*
        // opaque'd. We do not model that component.
        "firework" => tint(default?, TintProvenance::Unmodeled),

        // `Potion.calculate`: `ARGB.opaque(...)` on both branches.
        "potion" => tint(opaque(default?), TintProvenance::Unmodeled),

        // `MapColor.calculate`: `ARGB.opaque(...)` on both branches.
        "map_color" => tint(opaque(default?), TintProvenance::Unmodeled),

        // `TeamColor.calculate`. An item icon has no holder, and vanilla takes the
        // default in exactly that case, so this is `Default`, not `Unmodeled`.
        "team" => tint(opaque(default?), TintProvenance::Default),

        // `CustomModelDataSource.calculate`: `ARGB.opaque(defaultColor)` when the
        // component is absent or the index is out of range.
        "custom_model_data" => tint(opaque(default?), TintProvenance::Unmodeled),

        // A tint type this build does not know. Applying white would be the
        // multiplicative identity and therefore *indistinguishable from having
        // handled it*, which is how an unimplemented source hides; returning
        // `None` lets a caller count it (see [`is_known`]) and say so.
        _ => None,
    }
}

/// Whether [`resolve`] knows this tint source's `type` id at all.
///
/// Distinct from `resolve(...).is_none()`, which is also `None` for a *known*
/// source whose inputs are missing (a `constant` with no `value`, a `grass` with
/// no colormap). A caller building a misses/coverage report wants this one: it
/// separates "a pack used a tint type we have never heard of" from "we know this
/// type and there was nothing to apply".
#[must_use]
pub fn is_known(kind: &str) -> bool {
    matches!(
        strip_ns(kind),
        "constant"
            | "dye"
            | "grass"
            | "firework"
            | "potion"
            | "map_color"
            | "team"
            | "custom_model_data"
    )
}

/// `ARGB.colorFromFloat(1.0, r, g, b)`, the `[r, g, b]`
/// float-triple alternative that `ExtraCodecs.RGB_COLOR_CODEC`
/// accepts wherever an item tint takes an int.
///
/// The per-channel conversion is `ARGB.as8BitChannel` = `Mth.floor(v * 255.0)`
/// — **floor, not round**, so `0.5` is `127` and not
/// `128`. Alpha is `floor(1.0 * 255) = 255`.
///
/// No vanilla item definition uses this form; it exists for resource packs.
#[must_use]
pub fn color_from_float(r: f32, g: f32, b: f32) -> i32 {
    let c = |v: f32| ((v * 255.0).floor() as i64 & 0xFF) as u32;
    (opaque((c(r) << 16) | (c(g) << 8) | c(b))) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(kind: &str, default: Option<i32>) -> TintSource {
        TintSource {
            kind: kind.to_string(),
            default,
            grass: None,
            index: 0,
        }
    }

    /// Every jar-derived default constant, asserted against the number read out
    /// of the 26.2 sources rather than against anything this crate computes.
    #[test]
    fn jar_default_constants_match_the_decompiled_values() {
        // `PotionContents.BASE_POTION_COLOR = -13083194`.
        assert_eq!(defaults::POTION_BASE as i32, -13_083_194);
        assert_eq!(defaults::POTION_SOURCE, defaults::POTION_BASE);
        // `MapItemColor.DEFAULT`: `new MapItemColor(4603950)`, opaque'd.
        assert_eq!(defaults::MAP & 0x00FF_FFFF, 4_603_950);
        // `DyedItemColor.LEATHER_COLOR = -6265536`.
        assert_eq!(defaults::LEATHER as i32, -6_265_536);
        // `Firework`'s no-argument default: `-7697782`.
        assert_eq!(defaults::FIREWORK as i32, -7_697_782);
        // `GrassColor.get` / `ColorMapColorUtil.get`'s out-of-range fallback: `-65281`.
        assert_eq!(defaults::GRASS_COLORMAP_FALLBACK as i32, -65_281);
    }

    /// The four unmodeled-component sources resolve to their definition's
    /// `default`, and say so.
    #[test]
    fn unmodeled_components_fall_back_to_the_definition_default() {
        for (kind, default, expect) in [
            ("minecraft:potion", -13_083_194, defaults::POTION_BASE),
            ("minecraft:map_color", 4_603_950, defaults::MAP),
            ("minecraft:custom_model_data", 0x0012_3456, 0xFF12_3456),
        ] {
            let r = resolve(&src(kind, Some(default)), &ItemTintContext::default())
                .expect("a source with a default always resolves");
            assert_eq!(r.argb, expect, "{kind}");
            assert_eq!(r.provenance, TintProvenance::Unmodeled, "{kind}");
        }
    }

    /// `firework` is the one unmodeled source whose fallback is **not** opaque'd
    /// (`Firework.calculate` returns `this.defaultColor` bare). A blanket
    /// `opaque()` here would be indistinguishable for vanilla's own default,
    /// which already has alpha `0xFF` — so the discriminating input is a
    /// default with a zero top byte.
    #[test]
    fn firework_fallback_preserves_the_defaults_own_alpha() {
        let vanilla = resolve(
            &src("minecraft:firework", Some(-7_697_782)),
            &ItemTintContext::default(),
        )
        .unwrap();
        assert_eq!(vanilla.argb, defaults::FIREWORK);

        let alpha_zero = resolve(
            &src("minecraft:firework", Some(0x0012_3456)),
            &ItemTintContext::default(),
        )
        .unwrap();
        assert_eq!(
            alpha_zero.argb, 0x0012_3456,
            "vanilla does not opaque() firework's zero-colour fallback"
        );
    }

    /// `dye` is the only source live stack state can reach, and the provenance
    /// must distinguish the two paths.
    #[test]
    fn dye_reads_the_modeled_dyed_color_component() {
        let plain = resolve(
            &src("minecraft:dye", Some(-6_265_536)),
            &ItemTintContext::default(),
        )
        .unwrap();
        assert_eq!(plain.argb, defaults::LEATHER);
        assert_eq!(plain.provenance, TintProvenance::Default);

        let components = ItemComponents {
            dyed_color: Some(0x00FF_0000),
            ..ItemComponents::default()
        };
        let dyed = resolve(
            &src("minecraft:dye", Some(-6_265_536)),
            &ItemTintContext {
                components: Some(&components),
                grass_colormap: None,
            },
        )
        .unwrap();
        assert_eq!(dyed.argb, 0xFFFF_0000);
        assert_eq!(dyed.provenance, TintProvenance::Component);
    }

    /// `team` takes the default because an icon has no holder — vanilla's own
    /// `owner == null` path — so it is `Default`, not `Unmodeled`.
    #[test]
    fn team_with_no_holder_is_default_not_unmodeled() {
        let r = resolve(
            &src("minecraft:team", Some(0x0000_FF00)),
            &ItemTintContext::default(),
        )
        .unwrap();
        assert_eq!(r.argb, 0xFF00_FF00);
        assert_eq!(r.provenance, TintProvenance::Default);
    }

    /// A source this build does not know applies nothing, rather than guessing
    /// white (which is the identity, and so indistinguishable from "handled").
    #[test]
    fn an_unknown_tint_type_applies_nothing() {
        assert!(
            resolve(
                &src("minecraft:some_future_tint", Some(0x0012_3456)),
                &ItemTintContext::default()
            )
            .is_none()
        );
        assert!(!is_known("minecraft:some_future_tint"));
    }

    /// All eight of `ItemTintSources.bootstrap`'s registrations are known, and
    /// the count is asserted so adding a ninth to [`is_known`] without adding a
    /// `resolve` arm (or vice versa) shows up here.
    #[test]
    fn every_vanilla_tint_source_id_is_known() {
        let ids = [
            "minecraft:constant",
            "minecraft:dye",
            "minecraft:grass",
            "minecraft:firework",
            "minecraft:potion",
            "minecraft:map_color",
            "minecraft:team",
            "minecraft:custom_model_data",
        ];
        assert_eq!(ids.len(), 8, "ItemTintSources.bootstrap registers eight");
        for id in ids {
            assert!(is_known(id), "{id}");
            // Every one of them resolves to *something* given a default and,
            // for grass, a climate — i.e. `is_known` is not claiming coverage
            // `resolve` does not have.
            let mut s = src(id, Some(0x0012_3456));
            s.grass = Some(defaults::GRASS_CLIMATE);
            let ctx = ItemTintContext::default();
            if id == "minecraft:grass" {
                // grass needs a colormap, which this unit test has none of;
                // covered by `grass_with_no_colormap_applies_nothing`.
                continue;
            }
            assert!(resolve(&s, &ctx).is_some(), "{id} is known but unresolvable");
        }
        // `spawn_egg` is not a tint source in 26.2 at all — see `resolve`'s doc.
        assert!(!is_known("minecraft:spawn_egg"));
    }

    /// A source whose `default` is missing applies nothing. This is the shape
    /// that catches the `constant`-uses-`value` bug from the other direction: if
    /// `parse_tint` fails to pick a colour up, `resolve` must degrade to
    /// untinted rather than to some invented colour.
    #[test]
    fn a_source_with_no_default_applies_nothing() {
        for kind in [
            "minecraft:constant",
            "minecraft:potion",
            "minecraft:map_color",
            "minecraft:firework",
            "minecraft:team",
            "minecraft:custom_model_data",
        ] {
            assert!(resolve(&src(kind, None), &ItemTintContext::default()).is_none(), "{kind}");
        }
        // `dye` with no default and no component likewise.
        assert!(resolve(&src("minecraft:dye", None), &ItemTintContext::default()).is_none());
    }

    /// `grass` with no colormap must not invent vanilla's loud magenta fallback.
    #[test]
    fn grass_with_no_colormap_applies_nothing() {
        let mut s = src("minecraft:grass", None);
        s.grass = Some([0.5, 1.0]);
        assert!(resolve(&s, &ItemTintContext::default()).is_none());
    }

    /// `ARGB.as8BitChannel` floors. `0.5 * 255 = 127.5` → `127`, not `128`; a
    /// `round()` implementation passes a `1.0`/`0.0` test and fails this one.
    #[test]
    fn color_from_float_floors_like_as_8_bit_channel() {
        assert_eq!(color_from_float(1.0, 1.0, 1.0), -1); // 0xFFFFFFFF
        assert_eq!(color_from_float(0.5, 0.5, 0.5) as u32, 0xFF7F_7F7F);
        assert_eq!(color_from_float(0.0, 0.0, 0.0) as u32, 0xFF00_0000);
    }

    /// `rgb()` drops alpha and nothing else.
    #[test]
    fn rgb_discards_only_alpha() {
        let r = ResolvedTint {
            argb: 0xFF38_5DC6,
            provenance: TintProvenance::Default,
        };
        assert_eq!(r.rgb(), 0x0038_5DC6);
    }
}
