use std::collections::BTreeMap;

use lodestone_data::block_states::StateId;
use serde_json::Value;

use super::{FrontendError, resource_id};

/// Resolves a compact block name or `{id, properties}` into the canonical census.
/// Omitted properties retain the block's defaults; specified properties must
/// exist and have a valid value. Unknown blocks never become air.
pub fn parse_state(value: &Value) -> Result<StateId, FrontendError> {
    parse_state_at(value, "state")
}

/// Resolves a data-written key (`name` or `name[k=v,...]`, as the 26.3 engine
/// interns material-rule states) into the canonical census. Properties the key
/// omits take the block's defaults, exactly as in [`parse_state`].
pub fn parse_state_key(key: &str) -> Result<StateId, FrontendError> {
    let (name, props) = match key.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').ok_or_else(|| FrontendError::new(key, "unterminated property list"))?),
        None => (key, ""),
    };
    let mut properties = serde_json::Map::new();
    for pair in props.split(',').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').ok_or_else(|| FrontendError::new(key, format!("malformed property {pair:?}")))?;
        properties.insert(k.to_owned(), Value::String(v.to_owned()));
    }
    parse_state_at(&serde_json::json!({ "id": name, "properties": properties }), key)
}

pub(super) fn parse_state_at(value: &Value, path: &str) -> Result<StateId, FrontendError> {
    let (name, supplied) = match value {
        Value::String(name) => (name.as_str(), None),
        Value::Object(object) => {
            if let Some(key) = object.keys().find(|key| !matches!(key.as_str(), "id" | "properties")) {
                return Err(FrontendError::new(path, format!("unexpected state field {key:?}")));
            }
            let name = object.get("id").and_then(Value::as_str)
                .ok_or_else(|| FrontendError::new(path, "state requires a string id"))?;
            let properties = object.get("properties").map(|v| {
                v.as_object().ok_or_else(|| FrontendError::new(path, "properties must be an object"))
            }).transpose()?;
            (name, properties)
        }
        _ => return Err(FrontendError::new(path, "expected block name or {id, properties}")),
    };
    let name = resource_id(name, path)?;
    let default = StateId::from_state_str(&name)
        .filter(|state| state.name() == name)
        .ok_or_else(|| FrontendError::new(path, format!("unknown block {name}")))?;
    let mut properties: BTreeMap<String, String> = default.properties().iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect();
    if let Some(supplied) = supplied {
        for (key, value) in supplied {
            let value = value.as_str()
                .ok_or_else(|| FrontendError::new(path, format!("property {key} must be a string")))?;
            let target = properties.get_mut(key)
                .ok_or_else(|| FrontendError::new(path, format!("unknown property {name}.{key}")))?;
            *target = value.to_owned();
        }
    }
    StateId::from_exact_parts(&name, &properties)
        .ok_or_else(|| FrontendError::new(path, format!("invalid property value for {name}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn partial_properties_preserve_defaults_and_validate_every_value() {
        let state = parse_state(&json!({"id":"oak_log", "properties":{"axis":"x"}})).unwrap();
        assert_eq!(state.name(), "minecraft:oak_log");
        assert_eq!(state.properties(), &[("axis", "x")]);
        assert!(parse_state(&json!({"id":"oak_log", "properties":{"axis":"diagonal"}})).is_err());
        assert!(parse_state(&json!({"id":"oak_log", "properties":{"missing":"x"}})).is_err());
    }

    #[test]
    fn compact_and_empty_properties_select_the_registered_default() {
        let compact = parse_state(&json!("minecraft:oak_leaves")).unwrap();
        assert!(compact.is_default());
        assert_eq!(compact, parse_state(&json!({"id":"oak_leaves","properties":{}})).unwrap());
        assert!(parse_state(&json!("minecraft:absent_block")).is_err());
        assert!(parse_state(&json!({"Name":"minecraft:stone"})).is_err());
    }
}
