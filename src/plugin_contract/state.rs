use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginStateValue {
    #[serde(default = "default_state_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginStateMutation {
    pub key: String,
    #[serde(default = "default_state_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub delete: bool,
}

fn default_state_schema_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_state_values_default_to_schema_v1() {
        let value: PluginStateValue =
            serde_json::from_value(serde_json::json!({"value": {"cursor": 4}})).unwrap();
        assert_eq!(value.schema_version, 1);
    }
}
