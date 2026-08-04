//! Bakes `minecraft:paletted_permutations` atlas sources
//! ([`AtlasSource::PalettedPermutations`]) into decoded sprites.
//!
//! [`crate::atlas_source`]'s own module docs say this kind is "parsed into a
//! typed variant... but the actual palette-swap pixel generation is a bake
//! step and is intentionally left to the atlas-baking layer". This is that
//! layer — armour trims (`assets/minecraft/atlases/armor_trims.json`, see
//! [`crate::trim`]) are the concrete consumer, ported directly from
//! `PalettedPermutations.java`/`NativeImage.mappedCopy` in
//! `.cache/mc/26.2/client-src/net/minecraft/client/renderer/texture/atlas/
//! sources/PalettedPermutations.java`.
//!
//! # The algorithm, and why it is not a stitch
//!
//! A base texture (e.g. `trims/entity/humanoid/sentry.png`) is not itself the
//! finished sprite — it is a **greyscale index image**: every opaque pixel's
//! RGB is one of exactly eight values, `224, 192, 160, 128, 96, 64, 32, 0`
//! (verified against the real `sentry.png`: its four opaque colours are all
//! members of that set, and against `trims/color_palettes/trim_palette.png`,
//! the reference strip those eight values come from). A "permutation" swaps
//! every pixel's colour for the same-indexed entry of a different 8-colour
//! strip (`trims/color_palettes/iron.png`, `.../iron_darker.png`, …),
//! preserving alpha (scaled by the target entry's own alpha, which is always
//! 255 for every real palette measured here, but vanilla's formula is kept
//! exact rather than assumed).
//!
//! This is genuinely different from [`crate::atlas::AtlasBuilder`]'s job:
//! there is no packing, no UV rectangle, no shared sheet — the output is one
//! full-size [`Image`] per `(base texture, permutation)` pair, the same shape
//! [`crate::banner_pattern_atlas::BannerPatternAtlas`] already produces for
//! its own per-pattern masks (see that module's "why not `AtlasBuilder`"
//! note, which applies here for the same reason: a consumer wants one
//! addressable image, not a sub-rect into a big one, at least until a real
//! GPU-side reason to pack them appears).

use std::collections::HashMap;

use crate::atlas_source::AtlasSource;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::texture::Image;

/// A census of what [`bake_paletted_permutations`] produced.
#[derive(Debug, Clone, Default)]
pub struct PaletteBakeReport {
    /// Sprites baked successfully.
    pub loaded: usize,
    /// The source's own reference `palette_key` failed to load or decode —
    /// fatal for *every* sprite this source would have produced (vanilla's
    /// `paletteKeySupplier` failure propagates into every permutation's
    /// mapping), recorded once here rather than once per sprite.
    pub reference_palette_error: Option<String>,
    /// Base textures the source named that were not found in any pack
    /// (`texture id: path`).
    pub missing_base_textures: Vec<String>,
    /// Base textures found but not decodable as a PNG (`texture id: reason`).
    pub decode_errors: Vec<String>,
    /// Permutation palettes that failed to load, decode, or whose entry
    /// count disagreed with the reference palette (`suffix: reason`) —
    /// vanilla's `PalettedSpriteSupplier.get()` catches exactly this and
    /// skips the one sprite, so it is per-sprite here too, not fatal.
    pub palette_errors: Vec<String>,
}

/// Bakes every sprite [`AtlasSource::derived_sprite_ids`] promises for a
/// `paletted_permutations` source, keyed by the derived sprite id. Any other
/// source kind produces nothing (empty map, default report), mirroring
/// [`AtlasSource::resolve`]'s "this kind returns nothing" convention for the
/// sibling kinds — the two functions are meant to be called side by side,
/// one per source, never on a kind the other one owns.
#[must_use]
pub fn bake_paletted_permutations(
    source: &AtlasSource,
    manager: &ResourceManager,
) -> (HashMap<ResourceLocation, Image>, PaletteBakeReport) {
    let AtlasSource::PalettedPermutations {
        textures,
        palette_key,
        permutations,
        separator,
    } = source
    else {
        return (HashMap::new(), PaletteBakeReport::default());
    };

    let mut report = PaletteBakeReport::default();
    let reference = match load_palette(manager, palette_key) {
        Ok(p) => p,
        Err(e) => {
            report.reference_palette_error = Some(format!("{palette_key}: {e}"));
            return (HashMap::new(), report);
        }
    };

    // Target palettes are shared across every base texture, so load each one
    // once rather than once per (texture, permutation) pair.
    let mut target_cache: HashMap<&str, Result<Vec<[u8; 4]>, String>> = HashMap::new();
    for (suffix, loc) in permutations {
        target_cache
            .entry(suffix.as_str())
            .or_insert_with(|| load_palette(manager, loc).map_err(|e| format!("{loc}: {e}")));
    }

    let mut out = HashMap::new();
    for texture in textures {
        let base_path = ResourceManager::asset_path(texture, "textures", "png");
        let Some(bytes) = manager.read(&base_path) else {
            report.missing_base_textures.push(format!("{texture}: {base_path}"));
            continue;
        };
        let base = match Image::decode_png(&bytes) {
            Ok(img) => img,
            Err(e) => {
                report.decode_errors.push(format!("{texture}: {e}"));
                continue;
            }
        };

        for suffix in permutations.keys() {
            let target = match target_cache.get(suffix.as_str()) {
                Some(Ok(p)) => p,
                Some(Err(e)) => {
                    report.palette_errors.push(format!("{suffix}: {e}"));
                    continue;
                }
                None => continue,
            };
            if target.len() != reference.len() {
                report.palette_errors.push(format!(
                    "{suffix}: reference palette has {} entries, this one has {}",
                    reference.len(),
                    target.len()
                ));
                continue;
            }
            let rgba = recolor_by_palette(&base.rgba, &reference, target);
            let Ok(sprite_id) = ResourceLocation::parse(&format!(
                "{}:{}{separator}{suffix}",
                texture.namespace(),
                texture.path()
            )) else {
                continue;
            };
            out.insert(
                sprite_id,
                Image {
                    width: base.width,
                    height: base.height,
                    rgba,
                },
            );
            report.loaded += 1;
        }
    }
    (out, report)
}

fn load_palette(manager: &ResourceManager, loc: &ResourceLocation) -> Result<Vec<[u8; 4]>, String> {
    let path = ResourceManager::asset_path(loc, "textures", "png");
    let bytes = manager
        .read(&path)
        .ok_or_else(|| format!("not found: {path}"))?;
    let image = Image::decode_png(&bytes).map_err(|e| format!("decode: {e}"))?;
    Ok(image.rgba.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect())
}

/// The pixel transform itself: `PalettedPermutations.createPaletteMapping`
/// (the `rgb -> replacement` lookup table) composed with
/// `NativeImage.mappedCopy` (applying it per pixel), reproduced byte for
/// byte.
///
/// `reference` and `target` are read positionally — index `i` of `reference`
/// maps to index `i` of `target` — and must be the same length (a debug
/// assertion catches a caller mismatch; [`bake_paletted_permutations`] never
/// calls this with mismatched lengths, having already checked and reported
/// it).
///
/// Per pixel, straight off `createPaletteMapping`/the lambda it returns:
/// - A reference-palette entry whose own alpha is `0` is skipped when
///   building the lookup table (`ARGB.alpha(key) != 0`) — it can never be
///   matched, even by a pixel with the exact same, fully-transparent colour.
///   A duplicate RGB among the *remaining* entries has the later index win
///   (a plain `HashMap::put` overwrite), which this reproduces with a linear
///   table rather than a `[u8; 3]`-keyed map — real palettes are 8-16 entries,
///   so the O(n) lookup costs nothing measurable and needs no `Hash` impl on
///   a raw byte triple.
/// - A base pixel with alpha `0` passes through completely unchanged
///   (`if (pixelAlpha == 0) return pixel;`), *before* any lookup — so a
///   transparent pixel that happens to share an RGB with a real palette entry
///   is never touched.
/// - A base pixel whose RGB matches a lookup entry gets that entry's RGB,
///   with alpha `pixel.alpha * entry.alpha / 255` (integer division, matching
///   Java's `int` arithmetic).
/// - A base pixel whose RGB matches **no** lookup entry is `ARGB.opaque` of
///   itself as the substitute value, and multiplying that through the same
///   formula (`alpha * 255 / 255`) recovers the original alpha exactly — so
///   an unmapped pixel is, byte for byte, a pass-through. This matters for
///   any anti-aliased or stray colour a pattern texture might carry outside
///   the eight reference greys (none of the ones measured here do, but the
///   fallback is real vanilla behaviour, not a guess).
#[must_use]
pub fn recolor_by_palette(base_rgba: &[u8], reference: &[[u8; 4]], target: &[[u8; 4]]) -> Vec<u8> {
    debug_assert_eq!(
        reference.len(),
        target.len(),
        "recolor_by_palette requires two same-length palettes"
    );

    let mut lut: Vec<([u8; 3], [u8; 4])> = Vec::with_capacity(reference.len());
    for (r, t) in reference.iter().zip(target.iter()) {
        if r[3] == 0 {
            continue;
        }
        let rgb = [r[0], r[1], r[2]];
        match lut.iter_mut().find(|(k, _)| *k == rgb) {
            Some(entry) => entry.1 = *t,
            None => lut.push((rgb, *t)),
        }
    }

    let mut out = vec![0u8; base_rgba.len()];
    for (px_in, px_out) in base_rgba.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
        let alpha = px_in[3];
        if alpha == 0 {
            px_out.copy_from_slice(px_in);
            continue;
        }
        let rgb = [px_in[0], px_in[1], px_in[2]];
        match lut.iter().find(|(k, _)| *k == rgb) {
            Some((_, value)) => {
                let out_alpha = (u16::from(alpha) * u16::from(value[3]) / 255) as u8;
                px_out.copy_from_slice(&[value[0], value[1], value[2], out_alpha]);
            }
            None => px_out.copy_from_slice(px_in),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] {
        [r, g, b, a]
    }

    /// A base image that is *only* pixels from the reference palette, plus
    /// one fully-transparent pixel and one colour absent from the palette.
    fn base_pixels() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&px(224, 224, 224, 255)); // index 0
        out.extend_from_slice(&px(0, 0, 0, 255)); // index 7 (real black, opaque)
        out.extend_from_slice(&px(1, 2, 3, 0)); // fully transparent, arbitrary colour
        out.extend_from_slice(&px(50, 60, 70, 200)); // not in the reference palette at all
        out
    }

    fn reference() -> Vec<[u8; 4]> {
        vec![
            px(224, 224, 224, 255),
            px(192, 192, 192, 255),
            px(160, 160, 160, 255),
            px(128, 128, 128, 255),
            px(96, 96, 96, 255),
            px(64, 64, 64, 255),
            px(32, 32, 32, 255),
            px(0, 0, 0, 255),
        ]
    }

    fn iron() -> Vec<[u8; 4]> {
        vec![
            px(197, 210, 212, 255),
            px(191, 201, 200, 255),
            px(157, 170, 170, 255),
            px(123, 137, 137, 255),
            px(113, 125, 125, 255),
            px(101, 112, 112, 255),
            px(87, 99, 99, 255),
            px(70, 81, 81, 255),
        ]
    }

    #[test]
    fn a_referenced_grey_maps_to_the_same_indexed_target_colour() {
        let out = recolor_by_palette(&base_pixels(), &reference(), &iron());
        // Index 0 (224,224,224) -> iron's index 0.
        assert_eq!(&out[0..4], &[197, 210, 212, 255]);
        // Index 7 (0,0,0) -> iron's index 7.
        assert_eq!(&out[4..8], &[70, 81, 81, 255]);
    }

    #[test]
    fn a_fully_transparent_pixel_passes_through_unchanged_even_if_its_rgb_would_match() {
        let out = recolor_by_palette(&base_pixels(), &reference(), &iron());
        assert_eq!(&out[8..12], &[1, 2, 3, 0]);
    }

    #[test]
    fn an_unmapped_colour_passes_through_unchanged() {
        let out = recolor_by_palette(&base_pixels(), &reference(), &iron());
        assert_eq!(&out[12..16], &[50, 60, 70, 200]);
    }

    #[test]
    fn a_reference_entry_with_zero_alpha_can_never_be_matched() {
        // A pixel whose RGB equals a *transparent* reference slot must not be
        // recoloured — `ARGB.alpha(key) != 0` excludes it from the lookup
        // table entirely, so it falls to the "unmapped" pass-through even
        // though its colour is technically "in" the reference image.
        let reference = vec![px(10, 20, 30, 0), px(224, 224, 224, 255)];
        let target = vec![px(255, 0, 0, 255), px(0, 255, 0, 255)];
        let base = px(10, 20, 30, 255); // matches the zero-alpha slot's RGB
        let out = recolor_by_palette(&base, &reference, &target);
        assert_eq!(out, vec![10, 20, 30, 255], "must pass through unchanged");
    }

    #[test]
    fn alpha_scales_by_the_targets_own_alpha() {
        // A half-transparent base pixel against a half-transparent target
        // entry: out_alpha = 128 * 128 / 255 = 64 (integer division).
        let reference = vec![px(1, 1, 1, 255)];
        let target = vec![px(9, 9, 9, 128)];
        let base = px(1, 1, 1, 128);
        let out = recolor_by_palette(&base, &reference, &target);
        assert_eq!(out, vec![9, 9, 9, 64]);
    }

    #[test]
    fn a_later_duplicate_reference_entry_wins_the_lookup() {
        // Two reference slots share an RGB; vanilla's HashMap `put` lets the
        // later index overwrite the earlier one.
        let reference = vec![px(5, 5, 5, 255), px(5, 5, 5, 255)];
        let target = vec![px(1, 0, 0, 255), px(2, 0, 0, 255)];
        let base = px(5, 5, 5, 255);
        let out = recolor_by_palette(&base, &reference, &target);
        assert_eq!(out, vec![2, 0, 0, 255], "the second entry must win");
    }
}
