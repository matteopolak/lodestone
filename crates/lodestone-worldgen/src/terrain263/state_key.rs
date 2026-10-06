//! Block-state keys: the `name` or `name[k=v,...]` strings the 26.3 engine interns
//! material-rule states as, resolved into the canonical block-state census.

use std::collections::BTreeMap;

use lodestone_data::block_states::StateId;
use serde_json::Value;

/// A rejected state key and the contract it broke.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateKeyError {
    pub path: String,
    pub reason: String,
}

impl StateKeyError {
    pub(crate) fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self { path: path.into(), reason: reason.into() }
    }
}

impl std::fmt::Display for StateKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

impl std::error::Error for StateKeyError {}

fn resource_id(value: &str, path: &str) -> Result<String, StateKeyError> {
    let (namespace, name) = value.split_once(':').unwrap_or(("minecraft", value));
    let valid_namespace = !namespace.is_empty()
        && namespace.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c));
    let valid_name = !name.is_empty()
        && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_./-".contains(&c));
    if !valid_namespace || !valid_name {
        return Err(StateKeyError::new(path, format!("invalid resource identifier {value:?}")));
    }
    Ok(format!("{namespace}:{name}"))
}

/// Resolves a data-written key (`name` or `name[k=v,...]`, as the 26.3 engine
/// interns material-rule states) into the canonical census. Properties the key
/// omits take the block's defaults, exactly as in [`parse_state_at`].
pub fn parse_state_key(key: &str) -> Result<StateId, StateKeyError> {
    let (name, props) = match key.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').ok_or_else(|| StateKeyError::new(key, "unterminated property list"))?),
        None => (key, ""),
    };
    let mut properties = serde_json::Map::new();
    for pair in props.split(',').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').ok_or_else(|| StateKeyError::new(key, format!("malformed property {pair:?}")))?;
        properties.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    parse_state_at(&serde_json::json!({ "id": name, "properties": properties }), key)
}

fn parse_state_at(value: &Value, path: &str) -> Result<StateId, StateKeyError> {
    let (name, supplied) = match value {
        Value::String(name) => (name.as_str(), None),
        Value::Object(object) => {
            if let Some(key) = object.keys().find(|key| !matches!(key.as_str(), "id" | "properties")) {
                return Err(StateKeyError::new(path, format!("unexpected state field {key:?}")));
            }
            let name = object.get("id").and_then(Value::as_str)
                .ok_or_else(|| StateKeyError::new(path, "state requires a string id"))?;
            let properties = object.get("properties").map(|v| {
                v.as_object().ok_or_else(|| StateKeyError::new(path, "properties must be an object"))
            }).transpose()?;
            (name, properties)
        }
        _ => return Err(StateKeyError::new(path, "expected block name or {id, properties}")),
    };
    let name = resource_id(name, path)?;
    let default = StateId::from_state_str(&name)
        .filter(|state| state.name() == name)
        .ok_or_else(|| StateKeyError::new(path, format!("unknown block {name}")))?;
    let mut properties: BTreeMap<String, String> = default.properties().iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
    if let Some(supplied) = supplied {
        for (key, value) in supplied {
            let value = value.as_str()
                .ok_or_else(|| StateKeyError::new(path, format!("property {key} must be a string")))?;
            let target = properties.get_mut(key)
                .ok_or_else(|| StateKeyError::new(path, format!("unknown property {name}.{key}")))?;
            *target = value.to_owned();
        }
    }
    StateId::from_exact_parts(&name, &properties)
        .ok_or_else(|| StateKeyError::new(path, format!("invalid property value for {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_properties_preserve_defaults_and_validate_every_value() {
        let state = parse_state_at(&json!({"id":"oak_log", "properties":{"axis":"x"}}), "state").unwrap();
        assert_eq!(state.name(), "minecraft:oak_log");
        assert_eq!(state.properties(), &[("axis", "x")]);
        assert!(parse_state_at(&json!({"id":"oak_log", "properties":{"axis":"diagonal"}}), "state").is_err());
        assert!(parse_state_at(&json!({"id":"oak_log", "properties":{"missing":"x"}}), "state").is_err());
    }

    #[test]
    fn compact_and_empty_properties_select_the_registered_default() {
        let compact = parse_state_at(&json!("minecraft:oak_leaves"), "state").unwrap();
        assert!(compact.is_default());
        assert_eq!(compact, parse_state_at(&json!({"id":"oak_leaves","properties":{}}), "state").unwrap());
        assert!(parse_state_at(&json!("minecraft:absent_block"), "state").is_err());
        assert!(parse_state_at(&json!({"Name":"minecraft:stone"}), "state").is_err());
    }
}
