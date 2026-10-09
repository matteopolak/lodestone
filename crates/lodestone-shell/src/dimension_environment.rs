//! The resolved dimension type's visual environment attributes, turned into the
//! values the renderer consumes: fog and sky colour, cloud colour, the base
//! sky-light factor and the lightmap's ambient floor.
//!
//! Every function takes the decoded [`DimensionTypeInfo`] (or `None` before the
//! registry resolves) and returns what the frame should use. They are pure so
//! the connect paths in `app/session.rs` and `app/lifecycle.rs`, and
//! `Sim::fog_settings`/`Sim::cloud_color`, all share one rule. An attribute the
//! dimension did not declare leaves the existing behaviour untouched; no value
//! is invented for it.

use lodestone_model::event::DimensionTypeInfo;
use lodestone_render::fog::FogSettings;

/// The sea level the weather height falloff measures from. The login packet's
/// per-dimension value is not carried past the version seam, so this is the
/// overworld's.
pub(crate) const SEA_LEVEL: i32 = 63;

/// Packed `0xRRGGBB` (the alpha byte, if any, ignored) as linear RGB.
#[must_use]
pub(crate) fn packed_rgb_to_linear(packed: u32) -> [f32; 3] {
    lodestone_render::fog::srgb_u8_to_linear([
        ((packed >> 16) & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        (packed & 0xFF) as u8,
    ])
}

/// `settings` with the dimension's declared `fog_color` and `sky_color`
/// replacing the fixed ones. Absent attributes keep `settings` as given.
#[must_use]
pub(crate) fn fog_with_dimension(
    mut settings: FogSettings,
    info: Option<&DimensionTypeInfo>,
) -> FogSettings {
    if let Some(info) = info {
        if let Some(packed) = info.fog_color {
            settings.color = packed_rgb_to_linear(packed);
        }
        if let Some(packed) = info.sky_color {
            settings.sky_color = packed_rgb_to_linear(packed);
        }
    }
    settings
}

/// Linear RGBA cloud tint. No resolved dimension keeps the registered
/// fallback; a resolved dimension that omits `cloud_color` has no clouds
/// (alpha 0), and a declared one carries its own ARGB alpha.
#[must_use]
pub(crate) fn cloud_color_for(info: Option<&DimensionTypeInfo>) -> [f32; 4] {
    let Some(info) = info else {
        return [1.0, 1.0, 1.0, lodestone_render::sky::CLOUD_COLOR_ALPHA];
    };
    let Some(packed) = info.cloud_color else {
        return [1.0, 1.0, 1.0, 0.0];
    };
    let [r, g, b] = packed_rgb_to_linear(packed);
    [r, g, b, ((packed >> 24) & 0xFF) as f32 / 255.0]
}

/// The base sky-light factor, before weather: the dimension's declared
/// `sky_light_factor` (the End's is a constant zero), else the time-of-day
/// curve of `world_time_of_day`.
#[must_use]
pub(crate) fn base_sky_light_factor(info: &DimensionTypeInfo, world_time_of_day: i64) -> f32 {
    info.sky_light_factor
        .unwrap_or_else(|| lodestone_render::entity::sky_darken_for_time_of_day(world_time_of_day))
}

/// The lightmap's ambient floor: the declared `ambient_light_color`, else the
/// overworld's own.
#[must_use]
pub(crate) fn ambient_light_for(info: &DimensionTypeInfo) -> [f32; 3] {
    match info.ambient_light_color {
        Some(packed) => lodestone_render::light::rgb24_to_channels(packed),
        None => lodestone_render::light::OVERWORLD_AMBIENT_LIGHT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_render::light::OVERWORLD_AMBIENT_LIGHT;

    fn bare() -> DimensionTypeInfo {
        DimensionTypeInfo {
            name: "mypack:custom".parse().expect("valid key"),
            has_skylight: true,
            has_ceiling: false,
            has_fixed_time: false,
            coordinate_scale: 1.0,
            min_y: -64,
            height: 384,
            logical_height: 384,
            ambient_light: 0.0,
            ambient_light_color: None,
            environment_attributes: Vec::new(),
            fog_color: None,
            sky_color: None,
            cloud_color: None,
            sky_light_factor: None,
        }
    }

    /// A custom dimension declaring every attribute with a value no built-in
    /// dimension uses.
    fn custom() -> DimensionTypeInfo {
        DimensionTypeInfo {
            ambient_light_color: Some(0x20_40_80),
            fog_color: Some(0x00_10_20_30),
            sky_color: Some(0x00_aa_00_55),
            cloud_color: Some(0x80_ff_00_00),
            sky_light_factor: Some(0.375),
            ..bare()
        }
    }

    #[test]
    fn declared_fog_and_sky_colours_replace_the_fixed_ones() {
        let base = crate::sim::fog_for_render_distance(8);
        let fogged = fog_with_dimension(base.clone(), Some(&custom()));
        assert_eq!(
            fogged.color,
            lodestone_render::fog::srgb_u8_to_linear([0x10, 0x20, 0x30])
        );
        assert_eq!(
            fogged.sky_color,
            lodestone_render::fog::srgb_u8_to_linear([0xaa, 0x00, 0x55])
        );
        assert_ne!(fogged.color, base.color);
        assert_ne!(fogged.sky_color, base.sky_color);
        // Distances are untouched: only the colours are attributes.
        assert_eq!((fogged.start, fogged.end), (base.start, base.end));
    }

    /// Negative control: an absent attribute, or no resolved dimension, leaves
    /// the settings exactly as given, so the Overworld default is unchanged.
    #[test]
    fn absent_fog_and_sky_attributes_leave_the_settings_unchanged() {
        let base = crate::sim::fog_for_render_distance(8);
        assert_eq!(fog_with_dimension(base.clone(), Some(&bare())), base);
        assert_eq!(fog_with_dimension(base.clone(), None), base);
        // Fog alone must not drag the sky colour with it, nor the reverse.
        let fog_only = DimensionTypeInfo { sky_color: None, ..custom() };
        assert_eq!(fog_with_dimension(base.clone(), Some(&fog_only)).sky_color, base.sky_color);
        let sky_only = DimensionTypeInfo { fog_color: None, ..custom() };
        assert_eq!(fog_with_dimension(base.clone(), Some(&sky_only)).color, base.color);
    }

    #[test]
    fn declared_cloud_colour_carries_its_own_alpha() {
        let [r, g, b, a] = cloud_color_for(Some(&custom()));
        let expected = lodestone_render::fog::srgb_u8_to_linear([0xff, 0x00, 0x00]);
        assert_eq!([r, g, b], expected);
        assert!((a - 128.0 / 255.0).abs() < 1e-6, "alpha is the ARGB top byte, got {a}");
    }

    #[test]
    fn cloud_colour_fallbacks_are_distinct_for_unresolved_and_omitted() {
        assert_eq!(
            cloud_color_for(None),
            [1.0, 1.0, 1.0, lodestone_render::sky::CLOUD_COLOR_ALPHA]
        );
        assert_eq!(cloud_color_for(Some(&bare())), [1.0, 1.0, 1.0, 0.0]);
        assert_ne!(cloud_color_for(None), cloud_color_for(Some(&bare())));
    }

    #[test]
    fn declared_sky_light_factor_overrides_the_clock_and_absence_follows_it() {
        // Noon and midnight: the clock gives different answers, the attribute
        // gives one constant.
        let (noon, midnight) = (6000, 18000);
        let declared = DimensionTypeInfo { sky_light_factor: Some(0.0), ..bare() };
        assert_eq!(base_sky_light_factor(&declared, noon), 0.0);
        assert_eq!(base_sky_light_factor(&declared, midnight), 0.0);
        let overworld = bare();
        let day = base_sky_light_factor(&overworld, noon);
        let night = base_sky_light_factor(&overworld, midnight);
        assert_eq!(day, lodestone_render::entity::sky_darken_for_time_of_day(noon));
        assert!(day > night, "the Overworld curve still moves: {day} vs {night}");
        assert_ne!(day, 0.0);
    }

    #[test]
    fn declared_ambient_colour_replaces_the_overworld_floor() {
        assert_eq!(
            ambient_light_for(&custom()),
            lodestone_render::light::rgb24_to_channels(0x20_40_80)
        );
        assert_ne!(ambient_light_for(&custom()), OVERWORLD_AMBIENT_LIGHT);
        assert_eq!(ambient_light_for(&bare()), OVERWORLD_AMBIENT_LIGHT);
    }
}
