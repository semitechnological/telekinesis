use std::sync::Arc;

use rx4::agent::{Agent, Event as Rx4Event, ToolCall};
use rx4::mode::Scope;
use rx4::permissions::{ChannelApprover, Decision};
use rx4::provider::OpenAIProvider;
use rx4::{register_builtin_tools, ToolRegistry};
use tokio::sync::Mutex;

use crate::codex_provider;
use crate::view::session::CompanionEvent;

/// Clicky-style system prompt for the telekinesis companion.
/// Instructs the agent to capture the screen, use [POINT:] tags for cursor
/// pointing, and be conversational.
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

fn oauth_provider(name: &str) -> Option<rs_ai_oauth::OAuthProvider> {
    match name {
        "openai" | "chatgpt" => Some(rs_ai_oauth::OAuthProvider::ChatGpt),
        "grok" | "xai" => Some(rs_ai_oauth::OAuthProvider::Xai),
        "gemini" | "google" => Some(rs_ai_oauth::OAuthProvider::Gemini),
        _ => None,
    }
}

fn legacy_telekinesis_token(provider: &str) -> Option<rs_ai_oauth::OAuthTokens> {
    let home = std::env::var_os("HOME")?;
    let path = std::path::PathBuf::from(home)
        .join(".telekinesis")
        .join(format!("{provider}_token.json"));
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn saved_token(provider: &str, rt: &tokio::runtime::Runtime) -> Option<String> {
    let oauth = oauth_provider(provider)?;
    let mut tokens = rs_ai_oauth::credentials::load(&oauth)
        .or_else(|| legacy_telekinesis_token(provider))?;
    if rs_ai_oauth::credentials::is_expired(&tokens) {
        tokens = rt
            .block_on(rs_ai_oauth::refresh_oauth_token(oauth, &tokens))
            .ok()?;
        rs_ai_oauth::credentials::save(&oauth, &tokens).ok()?;
    }
    (!tokens.access_token.is_empty()).then_some(tokens.access_token)
}

fn env_key(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|key| !key.is_empty())
}

fn setup_provider(rt: &tokio::runtime::Runtime) -> Option<(Arc<dyn rx4::Provider>, String)> {
    // 1. ChatGPT OAuth (Codex) — preferred.
    if let Some(token) = saved_token("openai", rt) {
        return Some((codex_provider::provider_arc(token), "gpt-5.5".into()));
    }

    // 2. OpenAI API key.
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

    // 3. xAI Grok (OAuth or API key).
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

    // 4. Google Gemini (OAuth or API key).
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

    None
}

pub struct AgentSetup {
    pub computer_use: Arc<Mutex<Agent>>,
    pub coding: Arc<Mutex<Agent>>,
    pub model: String,
    pub event_rx: tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>,
    pub rt_handle: tokio::runtime::Handle,
    pub event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
    /// Shared pending approvals (both agents use same ChannelApprover pair via clone of sender side).
    pub approval_rx: std::sync::mpsc::Receiver<(ToolCall, std::sync::mpsc::Sender<Decision>)>,
}

fn create_agent(
    scope: Scope,
    model: &str,
    provider: Arc<dyn rx4::Provider>,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
    session_idx: usize,
    approver: Arc<dyn rx4::permissions::Approver>,
) -> Arc<Mutex<Agent>> {
    let mut agent = Agent::new();
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
    agent.set_graph_memory(rx4::GraphMemory::new());
    agent.enable_auto_dream(true);
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

    Arc::new(Mutex::new(agent))
}

pub fn setup_agents() -> Option<AgentSetup> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok()?;

    let (provider, model) = setup_provider(&rt)?;
    let handle = rt.handle().clone();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<CompanionEvent>();
    let (approver, approval_rx) = ChannelApprover::pair();
    let approver: Arc<dyn rx4::permissions::Approver> = Arc::new(approver);

    let computer_use = create_agent(
        Scope::ComputerUse,
        &model,
        provider.clone(),
        event_tx.clone(),
        0,
        Arc::clone(&approver),
    );
    let coding = create_agent(
        Scope::Coding,
        &model,
        provider,
        event_tx.clone(),
        1,
        Arc::clone(&approver),
    );

    std::mem::forget(rt);

    Some(AgentSetup {
        computer_use,
        coding,
        model,
        event_rx,
        rt_handle: handle,
        event_tx,
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
