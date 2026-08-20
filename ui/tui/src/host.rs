use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex as ParkingMutex;
use rx4::agent::Agent;
use rx4::hooks::{HookDecision, HookEvent, HookRegistry};
use rx4::mode::{self, Scope};
use rx4::provider::Provider;
use rx4::subagent::SubagentManager;
use rx4::ModelRegistry;

use crate::models::host_model_info;
use crate::product_policy;
use crate::tools::{self, McpToolSpec};

const MAX_HISTORY: usize = 100;

pub fn config_home() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".telekinesis"))
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub scope: Option<String>,
}

pub fn history_path() -> Option<PathBuf> {
    config_home().map(|home| home.join("input_history.json"))
}

pub fn prefs_path() -> Option<PathBuf> {
    config_home().map(|home| home.join("prefs.json"))
}

pub fn load_history() -> Vec<String> {
    let Some(path) = history_path() else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_history(history: &[String]) {
    let Some(path) = history_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let trimmed: Vec<&String> = history.iter().take(MAX_HISTORY).collect();
    let _ = std::fs::write(path, serde_json::to_string(&trimmed).unwrap_or_default());
}

pub fn load_prefs() -> Prefs {
    let Some(path) = prefs_path() else {
        return Prefs::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

pub fn save_prefs(prefs: &Prefs) {
    let Some(path) = prefs_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(prefs).unwrap_or_default(),
    );
}

pub fn parse_host_scope(name: &str) -> Result<Scope, String> {
    let Some(scope) = Scope::parse_scope(name) else {
        return Err(scope_usage());
    };
    if scope == Scope::ComputerUse && !cfg!(feature = "computer-use") {
        return Err(
            "computer_use scope requires a rebuild with --features full (or computer-use)"
                .to_string(),
        );
    }
    Ok(scope)
}

pub fn scope_usage() -> String {
    if cfg!(feature = "computer-use") {
        "Usage: /scope <coding|research|plan|ask|computer_use>".to_string()
    } else {
        "Usage: /scope <coding|research|plan|ask>".to_string()
    }
}

pub fn cycle_scopes() -> &'static [&'static str] {
    if cfg!(feature = "computer-use") {
        &["coding", "research", "plan", "ask", "computer_use"]
    } else {
        &["coding", "research", "plan", "ask"]
    }
}

pub fn apply_scope(agent: &mut Agent, scope: Scope) {
    agent.scope = scope;
    let profile = mode::profile(scope);
    agent.policy.apply_scope(&profile.policy);
    agent.set_policy(agent.policy.clone());
    let base = include_str!("../SYSTEM_PROMPT.md");
    agent.set_system_prompt(mode::compose_prompt(Some(base), &profile));
    install_scope_hooks(agent, scope);
}

fn install_scope_hooks(agent: &mut Agent, scope: Scope) {
    let hooks = HookRegistry::new();
    hooks.add(move |event: &HookEvent| match event {
        HookEvent::BeforeTool { tool } => {
            if host_tool_allowed(scope, &tool.name) {
                HookDecision::Allow
            } else {
                HookDecision::Deny {
                    reason: format!("tool not in scope {}: {}", scope.name(), tool.name),
                }
            }
        }
        _ => HookDecision::Allow,
    });
    agent.set_hooks(hooks);
}

pub fn host_tool_allowed(scope: Scope, tool_name: &str) -> bool {
    let profile = mode::profile(scope);
    if mode::tool_allowed(&profile, tool_name) {
        return true;
    }
    tool_name.starts_with("mcp__") && matches!(scope, Scope::Coding | Scope::Research)
}

pub(crate) fn build_agent(
    provider: Option<Arc<dyn Provider>>,
    model: &str,
    effort: &str,
    workspace: PathBuf,
    model_registry: ModelRegistry,
    mcp: &[McpToolSpec],
) -> (Agent, Arc<ParkingMutex<SubagentManager>>) {
    let mut agent = Agent::new();
    let mut model_registry = model_registry;
    if let Some(provider) = &provider {
        model_registry.register(host_model_info(provider.id(), model));
    }
    agent.set_model_registry(model_registry);
    agent.set_system_prompt(include_str!("../SYSTEM_PROMPT.md"));
    let mut subagent = SubagentManager::new().with_model(model.to_string());
    if let Some(provider) = &provider {
        subagent = subagent.with_provider(provider.clone());
    }
    let subagent_manager = Arc::new(ParkingMutex::new(subagent));
    let tools = tools::build_tool_registry(&subagent_manager, mcp);
    agent.set_tools(tools);
    subagent_manager.lock().set_tools(agent.tools.clone());
    agent.set_workspace_root(workspace);
    agent.load_project_context();
    agent.set_model(model);
    agent.set_reasoning_effort(Some(effort.to_string()));
    if let Some(provider) = provider {
        agent.set_provider(provider);
    }
    agent.set_policy(product_policy::tele_coding_policy());
    let _ = agent.enable_os_sandbox();
    apply_scope(&mut agent, Scope::Coding);
    attach_optional_engine(&mut agent);
    (agent, subagent_manager)
}

fn attach_optional_engine(agent: &mut Agent) {
    let _ = &agent;
    #[cfg(feature = "skills")]
    if let Some(home) = dirs::home_dir() {
        let mut engine = rx4::SkillEngine::new(home.join(".agents").join("skills"));
        engine.add_extra_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills"));
        engine.add_extra_dir(agent.workspace_root.join(".telekinesis").join("skills"));
        if engine.load().is_ok() {
            let mut reg = rx4::SkillRegistry::new();
            for skill in engine.list() {
                reg.register(skill.clone());
            }
            agent.set_skill_registry(reg);
            agent.set_skill_engine(engine);
        }
    }
    #[cfg(feature = "graph-memory")]
    {
        agent.set_graph_memory(rx4::GraphMemory::new());
        agent.enable_auto_dream(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_and_mcp_require_a_home_dir() {
        assert_eq!(
            config_home().is_some(),
            dirs::home_dir().is_some(),
            "config paths must not fall back to ./.telekinesis"
        );
        if dirs::home_dir().is_none() {
            assert!(prefs_path().is_none());
            assert!(history_path().is_none());
        }
    }

    #[test]
    fn computer_use_scope_is_feature_gated() {
        assert!(parse_host_scope("coding").is_ok());
        let parsed = parse_host_scope("computer_use");
        if cfg!(feature = "computer-use") {
            assert_eq!(parsed.unwrap(), Scope::ComputerUse);
        } else {
            assert!(parsed.unwrap_err().contains("--features full"));
        }
    }

    #[test]
    fn coding_allows_mcp_prefix() {
        assert!(host_tool_allowed(Scope::Coding, "bash"));
        assert!(host_tool_allowed(Scope::Coding, "mcp__fs__read_file"));
        assert!(!host_tool_allowed(Scope::Plan, "mcp__fs__read_file"));
        assert!(!host_tool_allowed(Scope::Ask, "bash"));
    }
}
