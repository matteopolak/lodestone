//! Small helpers for reading the data documents with located errors.

use serde_json::Value;

pub type Res<T> = Result<T, String>;

pub fn obj<'v>(v: &'v Value, ctx: &str) -> Res<&'v serde_json::Map<String, Value>> {
    v.as_object().ok_or_else(|| format!("{ctx}: expected an object"))
}

pub fn get<'v>(v: &'v Value, key: &str, ctx: &str) -> Res<&'v Value> {
    v.get(key).ok_or_else(|| format!("{ctx}: missing `{key}`"))
}

pub fn int(v: &Value, key: &str, ctx: &str) -> Res<i32> {
    get(v, key, ctx)?.as_i64().map(|n| n as i32).ok_or_else(|| format!("{ctx}.{key}: expected an integer"))
}

pub fn int_or(v: &Value, key: &str, default: i32, ctx: &str) -> Res<i32> {
    match v.get(key) {
        None => Ok(default),
        Some(x) => x.as_i64().map(|n| n as i32).ok_or_else(|| format!("{ctx}.{key}: expected an integer")),
    }
}

pub fn float(v: &Value, key: &str, ctx: &str) -> Res<f32> {
    get(v, key, ctx)?.as_f64().map(|n| n as f32).ok_or_else(|| format!("{ctx}.{key}: expected a number"))
}

pub fn double(v: &Value, key: &str, ctx: &str) -> Res<f64> {
    get(v, key, ctx)?.as_f64().ok_or_else(|| format!("{ctx}.{key}: expected a number"))
}

pub fn float_or(v: &Value, key: &str, default: f32, ctx: &str) -> Res<f32> {
    match v.get(key) {
        None => Ok(default),
        Some(x) => x.as_f64().map(|n| n as f32).ok_or_else(|| format!("{ctx}.{key}: expected a number")),
    }
}

pub fn boolean(v: &Value, key: &str, default: bool, ctx: &str) -> Res<bool> {
    match v.get(key) {
        None => Ok(default),
        Some(x) => x.as_bool().ok_or_else(|| format!("{ctx}.{key}: expected a boolean")),
    }
}

pub fn string<'v>(v: &'v Value, key: &str, ctx: &str) -> Res<&'v str> {
    get(v, key, ctx)?.as_str().ok_or_else(|| format!("{ctx}.{key}: expected a string"))
}

pub fn array<'v>(v: &'v Value, key: &str, ctx: &str) -> Res<&'v Vec<Value>> {
    get(v, key, ctx)?.as_array().ok_or_else(|| format!("{ctx}.{key}: expected an array"))
}

/// A `type` discriminator without its namespace.
pub fn type_of<'v>(v: &'v Value, ctx: &str) -> Res<&'v str> {
    let t = string(v, "type", ctx)?;
    Ok(t.strip_prefix("minecraft:").unwrap_or(t))
}

/// A resource name without the `minecraft:` namespace.
pub fn strip(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}
