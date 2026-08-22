use std::sync::Arc;
use std::sync::OnceLock;

use rx4::agent::{Agent, CancellationHandle, Event as Rx4Event, ToolCall};
use rx4::mode::Scope;
use rx4::permissions::{ChannelApprover, Decision};
use rx4::provider::OpenAIProvider;
use rx4::{register_builtin_tools, ModelInfo, ModelRegistry, ToolRegistry};
use tokio::sync::Mutex;

use crate::codex_provider;
use crate::session::CompanionEvent;

const SYSTEM_PROMPT: &str = r#"you're telekinesis, a friendly companion that lives in the user's menu bar. you can see their screen via the cu_see tool and interact with their computer via cu_click, cu_type, cu_hotkey tools. your reply will be displayed in a chat panel.

rules:
- be direct and helpful. default to 1-3 sentences unless the user asks for more detail.
- casual, warm tone. no emojis.
- you can help with anything — coding, writing, general knowledge, computer tasks.
- when the user asks about something on their screen, use cu_see to capture the screen first, then answer based on what you see.
- you can click, type, and press keys on the user's computer using cu_click, cu_type, and cu_hotkey. ask before doing anything destructive.
- never say "simply" or "just".

element pointing:
you have a blue cursor overlay that can fly to and point at things on screen. use it whenever pointing would genuinely help the user — if they're asking how to do something, looking for a menu, trying to find a button, or need help navigating an app.

when you point, append a coordinate tag at the very end of your response, AFTER your text: [POINT:x,y:label] where x,y are integer pixel coordinates in the screenshot's coordinate space (the image from cu_see), and label is a short 1-3 word description of the element.

if pointing wouldn't help, append [POINT:none].

examples:
- "the color inspector is in the top right of the toolbar. click that to get the color wheels. [POINT:1100,42:color inspector]"
- "html is the skeleton of every web page. [POINT:none]"
- "see the source control menu up top? click that and hit commit. [POINT:285,11:source control]"
"#;

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

pub fn oauth_provider(name: &str) -> Option<rs_ai_oauth::OAuthProvider> {
    rs_ai_oauth::OAuthProvider::parse(name)
}

fn legacy_telekinesis_token(provider: &str) -> Option<rs_ai_oauth::OAuthTokens> {
    let path = dirs::home_dir()?
        .join(".telekinesis")
        .join(format!("{provider}_token.json"));
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn saved_token(provider: &str, rt: &tokio::runtime::Runtime) -> Option<String> {
    let oauth = oauth_provider(provider)?;
    let mut tokens =
        rs_ai_oauth::credentials::load(&oauth).or_else(|| legacy_telekinesis_token(provider))?;
    if rs_ai_oauth::credentials::is_expired(&tokens) {
        tokens = rt
            .block_on(rs_ai_oauth::refresh_oauth_token(oauth, &tokens))
            .ok()?;
        rs_ai_oauth::credentials::save(&oauth, &tokens).ok()?;
    }
    (!tokens.access_token.is_empty()).then_some(tokens.access_token)
}

fn env_key(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|key| !key.is_empty())
}

fn host_model_registry(provider: &dyn rx4::Provider, model: &str) -> ModelRegistry {
    let mut info = ModelInfo::new(provider.id(), model, 128_000, 8_192);
    info.supports_tools = true;
    info.supports_vision = true;
    info.supports_reasoning = model.contains("reason")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("gpt-5");
    info.supports_reasoning_effort = info.supports_reasoning;
    ModelRegistry::from_models([info])
}

fn setup_provider(rt: &tokio::runtime::Runtime) -> Option<(Arc<dyn rx4::Provider>, String)> {
    if let Some(token) = saved_token("openai", rt) {
        return Some((codex_provider::provider_arc(token), "gpt-5.5".into()));
    }

    if let Some(key) = env_key("OPENAI_API_KEY") {
        return Some((
            Arc::new(OpenAIProvider::with_base_url(
                "https://api.openai.com/v1",
                key,
                "openai",
                "OpenAI",
            )),
            "gpt-5.4".into(),
        ));
    }

    if let Some(token) = saved_token("claude", rt) {
        return Some((
            Arc::new(OpenAIProvider::anthropic(token)),
            "claude-sonnet-4-5".into(),
        ));
    }

    if let Some(token) = saved_token("grok", rt).or_else(|| env_key("XAI_API_KEY")) {
        return Some((
            Arc::new(OpenAIProvider::with_base_url(
                "https://api.x.ai/v1",
                token,
                "xai",
                "xAI",
            )),
            "grok-4.5".into(),
        ));
    }

    if let Some(token) = saved_token("gemini", rt).or_else(|| env_key("GOOGLE_API_KEY")) {
        return Some((
            Arc::new(OpenAIProvider::with_base_url(
                "https://generativelanguage.googleapis.com/v1beta",
                token,
                "google",
                "Google Gemini",
            )),
            "gemini-2.0-flash".into(),
        ));
    }

    if let Some(token) = saved_token("kimi", rt) {
        return Some((
            Arc::new(OpenAIProvider::with_base_url(
                "https://api.moonshot.ai/v1",
                token,
                "moonshot",
                "Kimi",
            )),
            "kimi-k2.5".into(),
        ));
    }

    None
}

pub struct AgentSetup {
    pub computer_use: Arc<Mutex<Agent>>,
    pub computer_use_cancel: CancellationHandle,
    pub coding: Arc<Mutex<Agent>>,
    pub coding_cancel: CancellationHandle,
    pub model: String,
    pub approval_rx: std::sync::mpsc::Receiver<(ToolCall, std::sync::mpsc::Sender<Decision>)>,
}

fn create_agent(
    scope: Scope,
    model: &str,
    provider: Arc<dyn rx4::Provider>,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
    session_idx: usize,
    approver: Arc<dyn rx4::permissions::Approver>,
) -> (Arc<Mutex<Agent>>, CancellationHandle) {
    let mut agent = Agent::new();
    agent.set_model_registry(host_model_registry(provider.as_ref(), model));
    agent.set_scope(scope);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    if scope == Scope::ComputerUse {
        rx4::computer_use::register_tools(&mut tools);
    }
    agent.set_tools(tools);
    agent.set_workspace_root(
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    );
    agent.load_project_context();
    agent.set_system_prompt(SYSTEM_PROMPT);
    agent.set_model(model);
    agent.set_provider(provider);
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    agent.set_policy(crate::product_policy::tele_coding_policy());
    let _ = agent.enable_os_sandbox();
    agent.set_approver(approver);

    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx.send(CompanionEvent::Session(session_idx, event.clone()));
    });
    let cancellation = agent.cancellation_handle();

    (Arc::new(Mutex::new(agent)), cancellation)
}

pub fn setup_agents(
    rt: &tokio::runtime::Runtime,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
) -> Option<AgentSetup> {
    let (provider, model) = setup_provider(rt)?;
    let (approver, approval_rx) = ChannelApprover::pair();
    let approver: Arc<dyn rx4::permissions::Approver> = Arc::new(approver);

    let (computer_use, computer_use_cancel) = create_agent(
        Scope::ComputerUse,
        &model,
        provider.clone(),
        event_tx.clone(),
        0,
        Arc::clone(&approver),
    );
    let (coding, coding_cancel) = create_agent(
        Scope::Coding,
        &model,
        provider,
        event_tx,
        1,
        Arc::clone(&approver),
    );

    Some(AgentSetup {
        computer_use,
        computer_use_cancel,
        coding,
        coding_cancel,
        model,
        approval_rx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_provider_maps_names() {
        assert!(oauth_provider("openai").is_some());
        assert!(oauth_provider("chatgpt").is_some());
        assert!(oauth_provider("grok").is_some());
        assert!(oauth_provider("gemini").is_some());
        assert!(oauth_provider("unknown").is_none());
    }
}
