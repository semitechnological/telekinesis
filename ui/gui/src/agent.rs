use std::sync::Arc;

use rx4::agent::{Agent, Event as Rx4Event};
use rx4::mode::Scope;
use rx4::provider::OpenAIProvider;
use rx4::{register_builtin_tools, ToolRegistry};
use tokio::sync::Mutex;

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

fn setup_provider() -> Option<(Arc<dyn rx4::Provider>, String)> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some((
                Arc::new(OpenAIProvider::anthropic(key)),
                "claude-3-5-sonnet-20241022".into(),
            ));
        }
    }
    for (env_var, base_url, id, name, model) in [
        ("XAI_API_KEY", "https://api.x.ai/v1", "xai", "xAI", "grok-4.5"),
        ("OPENAI_API_KEY", "https://api.openai.com/v1", "openai", "OpenAI", "gpt-4o"),
        (
            "GOOGLE_API_KEY",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google",
            "gemini-2.0-flash",
        ),
    ] {
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                return Some((
                    Arc::new(OpenAIProvider::with_base_url(base_url, key, id, name)),
                    model.into(),
                ));
            }
        }
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
}

fn create_agent(
    scope: Scope,
    model: &str,
    provider: Arc<dyn rx4::Provider>,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
    session_idx: usize,
) -> Arc<Mutex<Agent>> {
    let mut agent = Agent::new();
    agent.set_scope(scope);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    if scope == Scope::ComputerUse {
        rx4::computer_use::register_tools(&mut tools);
    }
    agent.set_tools(tools);
    agent.set_workspace_root(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    agent.set_system_prompt(SYSTEM_PROMPT);
    agent.set_model(model);
    agent.set_provider(provider);
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    agent.set_policy(rx4::Policy::workspace_write());

    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx.send(CompanionEvent::Session(session_idx, event.clone()));
    });

    Arc::new(Mutex::new(agent))
}

pub fn setup_agents() -> Option<AgentSetup> {
    let (provider, model) = setup_provider()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok()?;

    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<CompanionEvent>();

    let computer_use = create_agent(
        Scope::ComputerUse,
        &model,
        provider.clone(),
        event_tx.clone(),
        0,
    );
    let coding = create_agent(
        Scope::Coding,
        &model,
        provider,
        event_tx.clone(),
        1,
    );

    std::mem::forget(rt);

    Some(AgentSetup {
        computer_use,
        coding,
        model,
        event_rx,
        rt_handle: handle,
        event_tx,
    })
}
