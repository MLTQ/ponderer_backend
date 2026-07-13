use serde::{Deserialize, Serialize};

pub const PLUGIN_MANIFEST_VERSION_V1: u32 = 1;
pub const PLUGIN_PROTOCOL_VERSION_V1: u32 = 1;
pub const CURRENT_PLUGIN_MANIFEST_VERSION: u32 = PLUGIN_MANIFEST_VERSION_V1;
pub const CURRENT_PLUGIN_PROTOCOL_VERSION: u32 = PLUGIN_PROTOCOL_VERSION_V1;
pub const SUPPORTED_PLUGIN_PROTOCOL_VERSIONS: &[u32] = &[PLUGIN_PROTOCOL_VERSION_V1];

pub fn default_plugin_manifest_version() -> u32 {
    PLUGIN_MANIFEST_VERSION_V1
}

pub fn default_plugin_protocol_version() -> u32 {
    PLUGIN_PROTOCOL_VERSION_V1
}

pub fn default_supported_plugin_protocol_versions() -> Vec<u32> {
    SUPPORTED_PLUGIN_PROTOCOL_VERSIONS.to_vec()
}

pub fn is_supported_plugin_protocol_version(version: u32) -> bool {
    SUPPORTED_PLUGIN_PROTOCOL_VERSIONS.contains(&version)
}

pub fn negotiate_plugin_protocol_version(peer_versions: &[u32]) -> Option<u32> {
    SUPPORTED_PLUGIN_PROTOCOL_VERSIONS
        .iter()
        .rev()
        .copied()
        .find(|candidate| peer_versions.contains(candidate))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginHostDescriptor {
    pub name: String,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiation_selects_highest_shared_version() {
        assert_eq!(negotiate_plugin_protocol_version(&[99, 1]), Some(1));
        assert_eq!(negotiate_plugin_protocol_version(&[99]), None);
    }
}
