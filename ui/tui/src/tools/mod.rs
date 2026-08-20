use std::sync::Arc;

use parking_lot::Mutex as ParkingMutex;
use rx4::subagent::SubagentManager;
use rx4::{register_builtin_tools, register_spawn_agent_tool, ToolRegistry};

#[cfg(feature = "search")]
mod darash;
#[cfg(feature = "mcp")]
mod mcp;

#[cfg(feature = "search")]
pub(crate) use darash::register_darash_tool;
#[cfg(feature = "mcp")]
pub(crate) use mcp::{discover_mcp_tools, register_mcp_tools, McpToolSpec};

#[cfg(not(feature = "mcp"))]
pub(crate) struct McpToolSpec;

#[cfg(not(feature = "mcp"))]
pub(crate) async fn discover_mcp_tools() -> (Vec<McpToolSpec>, Vec<String>) {
    (Vec::new(), Vec::new())
}

#[cfg(not(feature = "mcp"))]
pub(crate) fn register_mcp_tools(_tools: &mut ToolRegistry, _specs: &[McpToolSpec]) {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolProfile {
    Minimal,
    Coding,
    Full,
}

impl ToolProfile {
    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("minimal") => Self::Minimal,
            Some("coding") => Self::Coding,
            _ => Self::Full,
        }
    }

    pub fn from_env() -> Self {
        Self::from_name(std::env::var("TK_TOOL_PROFILE").ok().as_deref())
    }

    fn computer_use(self) -> bool {
        matches!(self, Self::Full)
    }

    fn darash(self) -> bool {
        matches!(self, Self::Full)
    }

    fn spawn(self) -> bool {
        matches!(self, Self::Full | Self::Coding)
    }
}

pub(crate) fn build_tool_registry(
    subagent_manager: &Arc<ParkingMutex<SubagentManager>>,
    mcp: &[McpToolSpec],
) -> ToolRegistry {
    build_tool_registry_with_profile(subagent_manager, mcp, ToolProfile::from_env())
}

pub(crate) fn build_tool_registry_with_profile(
    subagent_manager: &Arc<ParkingMutex<SubagentManager>>,
    mcp: &[McpToolSpec],
    profile: ToolProfile,
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    if profile.computer_use() {
        #[cfg(feature = "computer-use")]
        rx4::computer_use::register_tools(&mut tools);
    }
    if profile.darash() {
        #[cfg(feature = "search")]
        register_darash_tool(&mut tools);
    }
    register_mcp_tools(&mut tools, mcp);
    if profile.spawn() {
        register_spawn_agent_tool(&mut tools, Arc::clone(subagent_manager));
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_tool_names(tools: &rx4::ToolRegistry) -> Vec<String> {
        tools
            .definitions()
            .iter()
            .filter_map(|definition| definition["name"].as_str().map(str::to_string))
            .collect()
    }

    #[cfg(not(feature = "computer-use"))]
    #[test]
    fn default_and_full_profiles_omit_computer_use_without_feature() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        for profile in [
            ToolProfile::Full,
            ToolProfile::from_name(None),
            ToolProfile::Coding,
            ToolProfile::Minimal,
        ] {
            let tools = build_tool_registry_with_profile(&manager, &[], profile);
            assert!(
                registered_tool_names(&tools)
                    .iter()
                    .all(|name| !name.starts_with("cu_")),
                "profile {profile:?} registered computer-use tools without the feature"
            );
        }
    }

    #[cfg(feature = "computer-use")]
    #[test]
    fn computer_use_feature_registers_cu_tools_on_default_and_full() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        let default = build_tool_registry_with_profile(&manager, &[], ToolProfile::from_name(None));
        let full = build_tool_registry_with_profile(&manager, &[], ToolProfile::Full);
        let coding = build_tool_registry_with_profile(&manager, &[], ToolProfile::Coding);
        assert!(registered_tool_names(&default)
            .iter()
            .any(|name| name.starts_with("cu_")));
        assert!(registered_tool_names(&full)
            .iter()
            .any(|name| name.starts_with("cu_")));
        assert!(registered_tool_names(&coding)
            .iter()
            .all(|name| !name.starts_with("cu_")));
    }

    #[test]
    fn default_and_full_share_one_profile_row() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        let default = registered_tool_names(&build_tool_registry_with_profile(
            &manager,
            &[],
            ToolProfile::from_name(None),
        ));
        let full = registered_tool_names(&build_tool_registry_with_profile(
            &manager,
            &[],
            ToolProfile::Full,
        ));
        assert_eq!(default, full);
        assert_eq!(ToolProfile::from_name(None), ToolProfile::Full);
        assert_eq!(ToolProfile::from_name(Some("full")), ToolProfile::Full);
    }

    #[cfg(not(feature = "mcp"))]
    #[tokio::test]
    async fn discover_is_empty_without_mcp_feature() {
        let (specs, errors) = discover_mcp_tools().await;
        assert!(specs.is_empty());
        assert!(errors.is_empty());
    }

    #[cfg(not(feature = "search"))]
    #[test]
    fn profiles_omit_darash_without_search_feature() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        for profile in [
            ToolProfile::Full,
            ToolProfile::from_name(None),
            ToolProfile::Coding,
            ToolProfile::Minimal,
        ] {
            let tools = build_tool_registry_with_profile(&manager, &[], profile);
            assert!(
                registered_tool_names(&tools)
                    .iter()
                    .all(|name| name != "web_search"),
                "profile {profile:?} registered darash without the search feature"
            );
        }
    }

    #[cfg(feature = "search")]
    #[test]
    fn darash_tool_is_registered_as_network_effect() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        let tools = build_tool_registry_with_profile(&manager, &[], ToolProfile::Full);
        assert!(tools
            .definitions()
            .iter()
            .any(|definition| definition["name"] == "web_search"));
        assert_eq!(tools.effect_of("web_search"), rx4::ToolEffect::Network);
    }

    #[cfg(feature = "search")]
    #[test]
    fn tool_profiles_are_environment_independent() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        let full = build_tool_registry_with_profile(&manager, &[], ToolProfile::Full);
        let minimal = build_tool_registry_with_profile(&manager, &[], ToolProfile::Minimal);
        let coding = build_tool_registry_with_profile(&manager, &[], ToolProfile::Coding);

        assert!(full
            .definitions()
            .iter()
            .any(|definition| definition["name"] == "web_search"));
        assert!(!minimal
            .definitions()
            .iter()
            .any(|definition| definition["name"] == "web_search"));
        assert!(!coding
            .definitions()
            .iter()
            .any(|definition| definition["name"] == "web_search"));
    }
}
