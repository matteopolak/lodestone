use super::{
    GraphicsPreset, InactivityFpsLimit, Options, ParticleLevel, MAX_BIOME_BLEND_RADIUS,
    MAX_FOV, MAX_RENDER_DISTANCE, MIN_BIOME_BLEND_RADIUS, MIN_FOV, MIN_FRAMERATE_LIMIT,
    MIN_RENDER_DISTANCE, UNLIMITED_FRAMERATE_CUTOFF, graphics_preset_from_name,
    inactivity_fps_limit_from_name, particle_level_from_name,
};
use lodestone_render::CloudStatus;
use serde_json::{Map, Value};

/// Complete graphics declaration shared by benchmark launch boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchmarkGraphicsSettings {
    framerate_limit: u32,
    enable_vsync: bool,
    inactivity_fps_limit: InactivityFpsLimit,
    graphics_preset: GraphicsPreset,
    cloud_status: CloudStatus,
    cutout_leaves: bool,
    entity_shadows: bool,
    particles: ParticleLevel,
    fov: u32,
    render_distance: u32,
    biome_blend_radius: i32,
}

impl BenchmarkGraphicsSettings {
    /// Requires every comparison field and rejects unknown fields or coercions.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        const FIELDS: &[&str] = &[
            "framerate_limit", "enable_vsync", "inactivity_fps_limit", "graphics_preset",
            "cloud_status", "cutout_leaves", "entity_shadows", "particles", "fov",
            "render_distance", "biome_blend_radius",
        ];
        let object = value.as_object().ok_or("benchmark.settings must be an object")?;
        for key in object.keys() {
            if !FIELDS.contains(&key.as_str()) {
                return Err(format!("unknown benchmark.settings field: {key}"));
            }
        }
        for key in FIELDS {
            if !object.contains_key(*key) {
                return Err(format!("missing benchmark.settings field: {key}"));
            }
        }
        Ok(Self {
            framerate_limit: integer(object, "framerate_limit", MIN_FRAMERATE_LIMIT, UNLIMITED_FRAMERATE_CUTOFF)?,
            enable_vsync: boolean(object, "enable_vsync")?,
            inactivity_fps_limit: inactivity_fps_limit_from_name(name(object, "inactivity_fps_limit")?)
                .ok_or("benchmark.settings.inactivity_fps_limit must be minimized or afk")?,
            graphics_preset: graphics_preset_from_name(name(object, "graphics_preset")?)
                .ok_or("benchmark.settings.graphics_preset must be fast, fancy, fabulous, or custom")?,
            cloud_status: match name(object, "cloud_status")? {
                "off" => CloudStatus::Off,
                "fast" => CloudStatus::Fast,
                "fancy" => CloudStatus::Fancy,
                _ => return Err("benchmark.settings.cloud_status must be off, fast, or fancy".into()),
            },
            cutout_leaves: boolean(object, "cutout_leaves")?,
            entity_shadows: boolean(object, "entity_shadows")?,
            particles: particle_level_from_name(name(object, "particles")?)
                .ok_or("benchmark.settings.particles must be all, decreased, or minimal")?,
            fov: integer(object, "fov", MIN_FOV, MAX_FOV)?,
            render_distance: integer(object, "render_distance", MIN_RENDER_DISTANCE, MAX_RENDER_DISTANCE)?,
            biome_blend_radius: integer(object, "biome_blend_radius", MIN_BIOME_BLEND_RADIUS as u32, MAX_BIOME_BLEND_RADIUS as u32)? as i32,
        })
    }

    /// Assigns declared values without expanding the preset or normalizing them.
    pub fn apply(self, options: &mut Options) {
        options.framerate_limit = self.framerate_limit;
        options.enable_vsync = self.enable_vsync;
        options.inactivity_fps_limit = self.inactivity_fps_limit;
        options.graphics_preset = self.graphics_preset;
        options.cloud_status = self.cloud_status;
        options.cutout_leaves = self.cutout_leaves;
        options.entity_shadows = self.entity_shadows;
        options.particles = self.particles;
        options.fov = self.fov;
        options.render_distance = self.render_distance;
        options.biome_blend_radius = self.biome_blend_radius;
    }
}

fn integer(object: &Map<String, Value>, key: &str, min: u32, max: u32) -> Result<u32, String> {
    let value = object[key].as_u64().filter(|value| (u64::from(min)..=u64::from(max)).contains(value));
    value.map(|value| value as u32)
        .ok_or_else(|| format!("benchmark.settings.{key} must be an integer in {min}..={max}"))
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<bool, String> {
    object[key].as_bool().ok_or_else(|| format!("benchmark.settings.{key} must be a boolean"))
}

fn name<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    object[key].as_str().ok_or_else(|| format!("benchmark.settings.{key} must be a string"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration() -> Value {
        json!({
            "framerate_limit": 144, "enable_vsync": false, "inactivity_fps_limit": "afk",
            "graphics_preset": "fast", "cloud_status": "fancy", "cutout_leaves": true,
            "entity_shadows": false, "particles": "decreased", "fov": 83,
            "render_distance": 9, "biome_blend_radius": 3,
        })
    }

    #[test]
    fn strict_complete_graphics_declaration_preserves_explicit_values() {
        let settings = BenchmarkGraphicsSettings::from_json(&declaration()).unwrap();
        let mut options = Options::default();
        settings.apply(&mut options);
        assert_eq!(options.framerate_limit, 144);
        assert_eq!(options.graphics_preset, GraphicsPreset::Fast);
        assert_eq!(options.cloud_status, CloudStatus::Fancy);
        assert!(options.cutout_leaves);
        assert_eq!((options.fov, options.render_distance, options.biome_blend_radius), (83, 9, 3));
        for key in declaration().as_object().unwrap().keys() {
            let mut missing = declaration();
            missing.as_object_mut().unwrap().remove(key);
            assert!(BenchmarkGraphicsSettings::from_json(&missing).is_err(), "{key}");
        }
        for (key, value) in [
            ("unknown", json!(true)), ("enable_vsync", json!(1)),
            ("framerate_limit", json!(9)), ("framerate_limit", json!(261)),
            ("framerate_limit", json!(144.5)), ("fov", json!(111)),
            ("render_distance", json!(1)), ("biome_blend_radius", json!(8)),
            ("cloud_status", json!("true")), ("graphics_preset", json!("unknown")),
            ("particles", json!(null)), ("inactivity_fps_limit", json!("always")),
        ] {
            let mut invalid = declaration();
            invalid[key] = value;
            assert!(BenchmarkGraphicsSettings::from_json(&invalid).is_err(), "{key}");
        }
    }
}
