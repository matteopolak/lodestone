use std::time::Duration;

use lodestone::{Config, Mode};
use lodestone::config::{
    BenchmarkConfig, BenchmarkDebugOverlay, BenchmarkGraphicsSettings, BenchmarkPacingPolicy,
    BenchmarkWindowMode, BenchmarkWorkload, Options,
};
use serde_json::{Map, Value};

/// Validates a stationary external-fixture trial before any mount side effects.
pub fn config_from_json(value: &Value) -> Result<Config, String> {
    if !cfg!(feature = "multiplayer") {
        return Err("options.benchmark requires a multiplayer-enabled build".into());
    }
    const REQUIRED: &[&str] = &[
        "host", "port", "protocol", "width", "height", "warmupSeconds",
        "stationarySeconds", "settings",
    ];
    let object = value.as_object().ok_or("options.benchmark must be an object")?;
    for key in object.keys() {
        if key != "username" && key != "witness" && !REQUIRED.contains(&key.as_str()) {
            return Err(format!("unknown options.benchmark field: {key}"));
        }
    }
    for key in REQUIRED {
        if !object.contains_key(*key) {
            return Err(format!("missing options.benchmark field: {key}"));
        }
    }
    let host = object["host"].as_str().filter(|host| {
        !host.is_empty() && host.len() <= 253
            && !host.chars().any(|ch| ch.is_whitespace() || ch.is_control() || matches!(ch, '/' | '\\'))
    }).ok_or("options.benchmark.host must be a nonempty hostname or IP address")?;
    let port = integer(object, "port", 1, u16::MAX as u32)? as u16;
    let protocol = integer(object, "protocol", 0, i32::MAX as u32)? as i32;
    if !Config::supports_protocol(protocol) {
        return Err(format!("options.benchmark.protocol {protocol} is unavailable in this build"));
    }
    let width = integer(object, "width", 320, 8192)?;
    let height = integer(object, "height", 240, 8192)?;
    let warmup = Duration::from_secs(integer(object, "warmupSeconds", 0, 3600)? as u64);
    let stationary = Duration::from_secs(integer(object, "stationarySeconds", 1, 10)? as u64);
    let username = object.get("username").map(|value| {
        value.as_str().filter(|name| {
            (1..=16).contains(&name.len())
                && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }).map(str::to_owned)
            .ok_or("options.benchmark.username must contain 1..16 ASCII letters, digits, or underscores")
    }).transpose()?;
    let settings = BenchmarkGraphicsSettings::from_json(&object["settings"])?;
    let mut options = Options::default();
    settings.apply(&mut options);
    let mut config = Config {
        mode: Mode::Window,
        host: host.into(),
        port,
        protocol,
        port_given: true,
        address_given: true,
        connect_in_window: true,
        initial_options: Some(options),
        benchmark_username: username,
        benchmark_witness: object.get("witness")
            .map(lodestone::config::BenchmarkWitnessHeader::from_json).transpose()?,
        benchmark: Some(BenchmarkConfig {
            workload: BenchmarkWorkload::Terrain,
            debug_overlay: BenchmarkDebugOverlay::Closed,
            window_mode: BenchmarkWindowMode::WindowedPhysical,
            pacing_policy: BenchmarkPacingPolicy::Options,
            physical_size: (width, height),
            heavyweight: None,
            warmup,
            stationary,
            mutation: Duration::ZERO,
            moving: Duration::ZERO,
            walk_mine: false,
        }),
        ..Config::default()
    };
    config.resolve_persisted(&options);
    Ok(config)
}

/// Converts JavaScript numeric scalars without losing nonfinite-value errors.
pub fn number_value(value: f64) -> Result<Value, String> {
    if !value.is_finite() {
        return Err("options.benchmark numbers must be finite".into());
    }
    if value.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&value) {
        return Ok((value as u64).into());
    }
    if value.fract() == 0.0 && (f64::from(i32::MIN)..0.0).contains(&value) {
        return Ok((value as i64).into());
    }
    Ok(serde_json::Number::from_f64(value).expect("finite number").into())
}

fn integer(object: &Map<String, Value>, key: &str, min: u32, max: u32) -> Result<u32, String> {
    object[key].as_u64().filter(|value| (u64::from(min)..=u64::from(max)).contains(value))
        .map(|value| value as u32)
        .ok_or_else(|| format!("options.benchmark.{key} must be an integer in {min}..={max}"))
}

#[cfg(all(test, feature = "multiplayer"))]
mod tests {
    use super::*;
    use serde_json::json;

    fn declaration() -> Value {
        json!({
            "host": "127.0.0.1", "port": 25565, "protocol": 776,
            "width": 1280, "height": 720, "warmupSeconds": 0, "stationarySeconds": 5,
            "username": "Trial_01",
            "settings": {
                "framerate_limit": 144, "enable_vsync": false, "inactivity_fps_limit": "minimized",
                "graphics_preset": "custom", "cloud_status": "off", "cutout_leaves": true,
                "entity_shadows": true, "particles": "all", "fov": 83,
                "render_distance": 9, "biome_blend_radius": 3,
            },
        })
    }

    #[test]
    fn declaration_uses_production_options_and_rejects_invalid_launch_fields() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(number_value(value).is_err());
        }
        assert_eq!(number_value(144.0).unwrap().as_u64(), Some(144));
        assert_eq!(number_value(-17.0).unwrap().as_i64(), Some(-17));
        assert!(number_value(-17.5).unwrap().as_i64().is_none());
        assert!(number_value(144.5).unwrap().as_u64().is_none());
        let config = config_from_json(&declaration()).unwrap();
        assert_eq!(config.render_distance, 9);
        assert_eq!(config.initial_options.unwrap().framerate_limit, 144);
        assert_eq!(config.explicit_port(), Some(25565));
        assert!(config.address_given && config.connect_in_window);
        assert_eq!(config.benchmark_username.as_deref(), Some("Trial_01"));
        let benchmark = config.benchmark.unwrap();
        assert_eq!(benchmark.physical_size, (1280, 720));
        assert_eq!(benchmark.window_mode, BenchmarkWindowMode::WindowedPhysical);
        assert_eq!(benchmark.pacing_policy, BenchmarkPacingPolicy::Options);
        assert_eq!((benchmark.moving, benchmark.mutation), (Duration::ZERO, Duration::ZERO));
        let mut no_username = declaration();
        no_username.as_object_mut().unwrap().remove("username");
        assert!(config_from_json(&no_username).unwrap().benchmark_username.is_none());
        for (key, value) in [
            ("extra", json!(true)), ("host", json!("")), ("port", json!(0)),
            ("port", json!(25565.5)), ("protocol", json!(999999)),
            ("width", json!(319)), ("height", json!(8193)), ("width", json!("1280")),
            ("warmupSeconds", json!(-1)), ("stationarySeconds", json!(0)),
            ("stationarySeconds", json!(11)), ("username", json!("bad name")),
            ("username", json!("abcdefghijklmnopq")), ("username", json!(null)),
        ] {
            let mut invalid = declaration();
            invalid[key] = value;
            assert!(config_from_json(&invalid).is_err(), "{key}");
        }
        for key in ["host", "port", "protocol", "width", "height", "warmupSeconds", "stationarySeconds", "settings"] {
            let mut invalid = declaration();
            invalid.as_object_mut().unwrap().remove(key);
            assert!(config_from_json(&invalid).is_err(), "{key}");
        }
    }
}
