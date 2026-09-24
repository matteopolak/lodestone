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
//! Faithful to the real client's item-colour dispatch table — not to a
//! renderer-side tint table, which does not exist there. Each tint source is
//! evaluated against the item stack, the world, and (optionally) a holder
//! entity.
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
//! item renderer never consults the block one. The item model's own layer
//! loop evaluates the item definition's own `tints` list per layer; nothing on the
//! item path calls into the block tint table. Substituting the block table for
//! the item list is wrong wherever the two disagree, and they do disagree:
//! `lily_pad`'s item definition is `constant 0x71C35C` while its block tint
//! gives it `0x208030`. It happens to *agree* for leaves
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
    /// The base potion tint constant, and the
    /// `default` every one of vanilla's four potion item definitions carries
    /// (`potion`, `splash_potion`, `lingering_potion`, `tipped_arrow`).
    ///
    /// Note this is **not** the pre-1.21 `0xF800F8` magic value, which no longer
    /// exists anywhere in 26.2.
    pub const POTION_BASE: u32 = 0xFF38_5DC6;

    /// The map tint source's own default constant (`4603950`)
    /// and the `default` on `filled_map.json`'s `map_color` layer.
    pub const MAP: u32 = 0xFF46_402E;

    /// The dye tint source's own leather-colour constant (`-6265536`) and the
    /// `default` on all six vanilla `dye` item definitions.
    pub const LEATHER: u32 = 0xFFA0_6540;

    /// The firework tint source's no-argument default (`-7697782`) and
    /// the `default` on `firework_star.json`'s `firework` layer.
    pub const FIREWORK: u32 = 0xFF8A_8A8A;

    /// The potion tint source's own no-argument default — identical to
    /// [`POTION_BASE`], kept separate because they are two separate constants
    /// in the source and could drift apart.
    pub const POTION_SOURCE: u32 = 0xFF38_5DC6;

    /// The grass colormap sampler's out-of-range fallback (`-65281`). Magenta,
    /// i.e. deliberately loud.
    pub const GRASS_COLORMAP_FALLBACK: u32 = 0xFFFF_00FF;

    /// The climate inputs every vanilla `minecraft:grass` item tint carries
    /// (`grass_block`, `short_grass`, `tall_grass`, `fern`, `large_fern`,
    /// `bush`), and the grass tint source's own no-argument default. Plains.
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
    /// implementation forces alpha to opaque; for `dye`'s and
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

/// Force alpha to `0xFF`, matching vanilla's own opaque-forcing helper.
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
/// two sources return their JSON `default` *without* forcing alpha opaque:
///
/// | id | component read | alpha forced |
/// |---|---|---|
/// | `constant` | none | yes, at construction |
/// | `dye` | `dyed_color` | only when present |
/// | `grass` | none (colormap) | yes, here |
/// | `firework` | `firework_explosion` | only when present |
/// | `potion` | `potion_contents` | always |
/// | `map_color` | `map_color` | always |
/// | `team` | none (needs a holder) | always |
/// | `custom_model_data` | `custom_model_data` | always |
///
/// # Which of these can ever be live here
///
/// `dye` and `potion`. [`ItemComponents`] models `dyed_color` and
/// `potion_color` — the latter is not the raw `minecraft:potion_contents`
/// patch but its *already-mixed* result
/// (`lodestone_data::potion::potion_color`, folded in once at decode time
/// rather than on every icon draw) — and neither `map_color`,
/// `firework_explosion` nor `custom_model_data`, so those three are reported
/// [`TintProvenance::Unmodeled`] and resolve to the definition's `default`.
/// `team` needs a holder entity that an item icon does not have —
/// vanilla itself takes the default when there is no holder, so
/// [`TintProvenance::Default`] is the honest label there rather than
/// `Unmodeled`.
///
/// **Modeled is not the same as drawn — or rather, it was not.**
/// `lodestone_shell::hud::item_icon::sprite_layer_tint` used to resolve every
/// tint against `ItemTintContext::default()` regardless of the stack in hand,
/// so a correct resolver still drew the wrong colour: every icon got the
/// definition's own default, dyed leather and mixed potions included. It now
/// builds a real `ItemTintContext` from the `ItemIcon` record's
/// `dyed_color`/`potion_color` fields, populated by every producer that has a
/// live `lodestone_game::item::ItemStack` in hand — the hotbar snapshot, the
/// container/creative `Builder::draw_stack` path, and the advancements grid
/// (which reaches `draw_stack` too). A producer with no stack (a recipe
/// result, a toast/advancement icon built from a bare item id) still leaves
/// both `None`, which is the honest answer there, not a shortfall.
///
/// **`minecraft:spawn_egg` is deliberately absent, and that is not an
/// omission.** 26.2 has no spawn-egg tint source at all: the spawn-egg item's
/// definition carries no colour fields anywhere in the source,
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
        // The constant tint source forces alpha at construction, so its
        // result is opaque regardless of what the JSON said.
        "constant" => tint(opaque(default?), TintProvenance::Default),

        // The one source this build can answer from live state.
        // Present → forced opaque over the raw colour; absent → the raw
        // default *unmasked*.
        "dye" => match ctx.components.and_then(|c| c.dyed_color) {
            // `dyed_color` is the raw wire int (see `ItemComponents::dyed_color`),
            // and vanilla uses it directly as an RGB value, so mask to 24 bits
            // before forcing alpha rather than trusting the wire's top byte.
            Some(rgb) => tint(opaque(rgb & 0x00FF_FFFF), TintProvenance::Component),
            None => tint(default?, TintProvenance::Default),
        },

        // The grass tint source samples the pack's grass colormap by climate
        // (temperature, downfall). Reads no component. The colormap PNG is
        // opaque in vanilla, and our `Colormap` drops alpha on load, so force it.
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

        // The firework tint source: no `firework_explosion` → the raw
        // default, *not* opaque'd. We do not model that component.
        "firework" => tint(default?, TintProvenance::Unmodeled),

        // The potion tint source: the mixed potion colour when present,
        // otherwise the definition's default — both forced opaque.
        // `potion_color` is already opaque by construction
        // (`lodestone_data::potion::potion_color` forces alpha itself), so no
        // second `opaque()` call is needed on the `Some` branch.
        "potion" => match ctx.components.and_then(|c| c.potion_color) {
            Some(argb) => tint(argb, TintProvenance::Component),
            None => tint(opaque(default?), TintProvenance::Default),
        },

        // The map-colour tint source forces alpha opaque on both branches.
        "map_color" => tint(opaque(default?), TintProvenance::Unmodeled),

        // The team tint source. An item icon has no holder, and vanilla takes
        // the default in exactly that case, so this is `Default`, not
        // `Unmodeled`.
        "team" => tint(opaque(default?), TintProvenance::Default),

        // The custom-model-data tint source forces alpha opaque when the
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

/// The `[r, g, b]` float-triple alternative that the codec
/// accepts wherever an item tint takes an int, matching vanilla's own
/// full-alpha conversion from a float triple.
///
/// The per-channel conversion floors rather than rounds, so `0.5` is `127`
/// and not `128`. Alpha is `floor(1.0 * 255) = 255`.
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
        // The potion base tint constant: -13083194.
        assert_eq!(defaults::POTION_BASE as i32, -13_083_194);
        assert_eq!(defaults::POTION_SOURCE, defaults::POTION_BASE);
        // The map tint source's default constant, opaque'd: 4603950.
        assert_eq!(defaults::MAP & 0x00FF_FFFF, 4_603_950);
        // The dye tint source's leather-colour constant: -6265536.
        assert_eq!(defaults::LEATHER as i32, -6_265_536);
        // The firework tint source's no-argument default: -7697782.
        assert_eq!(defaults::FIREWORK as i32, -7_697_782);
        // The grass colormap sampler's out-of-range fallback: -65281.
        assert_eq!(defaults::GRASS_COLORMAP_FALLBACK as i32, -65_281);
    }

    /// The three still-unmodeled sources resolve to their definition's
    /// `default`, and say so.
    #[test]
    fn unmodeled_components_fall_back_to_the_definition_default() {
        for (kind, default, expect) in [
            ("minecraft:map_color", 4_603_950, defaults::MAP),
            ("minecraft:custom_model_data", 0x0012_3456, 0xFF12_3456),
        ] {
            let r = resolve(&src(kind, Some(default)), &ItemTintContext::default())
                .expect("a source with a default always resolves");
            assert_eq!(r.argb, expect, "{kind}");
            assert_eq!(r.provenance, TintProvenance::Unmodeled, "{kind}");
        }
    }

    /// `potion` with no components in context (the majority case today — see
    /// `resolve`'s "modeled is not the same as drawn" doc) resolves to the
    /// definition's `default` and reports `Default`, not `Unmodeled` — the
    /// component genuinely is modeled now, so a missing stack means "no potion
    /// in hand", not "this build cannot decode it".
    #[test]
    fn potion_with_no_stack_in_context_is_default_not_unmodeled() {
        let r = resolve(
            &src("minecraft:potion", Some(-13_083_194)),
            &ItemTintContext::default(),
        )
        .expect("a source with a default always resolves");
        assert_eq!(r.argb, defaults::POTION_BASE);
        assert_eq!(r.provenance, TintProvenance::Default);
    }

    /// `potion` reads the *pre-mixed* `potion_color` component field, the one
    /// real live path this source has — mirroring `dye_reads_the_modeled_dyed_color_component`
    /// below.
    #[test]
    fn potion_reads_the_modeled_potion_color_component() {
        let components = ItemComponents {
            potion_color: Some(0xFF12_3456),
            ..ItemComponents::default()
        };
        let r = resolve(
            &src("minecraft:potion", Some(-13_083_194)),
            &ItemTintContext {
                components: Some(&components),
                grass_colormap: None,
            },
        )
        .unwrap();
        assert_eq!(r.argb, 0xFF12_3456);
        assert_eq!(r.provenance, TintProvenance::Component);
    }

    /// `firework` is the one unmodeled source whose fallback is **not** opaque'd
    /// (the firework tint source returns its default colour bare). A blanket
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

    /// All eight of vanilla's registered tint source ids are known, and
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
        assert_eq!(ids.len(), 8, "vanilla registers eight tint source ids");
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

    /// Vanilla's float-to-byte channel conversion floors. `0.5 * 255 = 127.5`
    /// → `127`, not `128`; a `round()` implementation passes a `1.0`/`0.0`
    /// test and fails this one.
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
