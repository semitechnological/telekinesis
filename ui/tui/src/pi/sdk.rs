//! Pi SDK surface — createAgentSession, AgentSessionHandle.
//!
//! Compatible with pi_agent_rust SDK:
//! ```ignore
//! use tk::pi::sdk::{create_agent_session, AgentSessionOptions};
//!
//! # #[tokio::main]
//! # async fn main() {
//! let handle = create_agent_session(AgentSessionOptions::default());
//! handle.prompt("hello", |event| { /* ... */ }).await;
//! # }
//! ```

use parking_lot::Mutex as SyncMutex;
use rx4::agent::{Agent, Event};
use rx4::provider::Message;
use rx4::ModelRegistry;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::host::{apply_scope, build_agent, parse_host_scope};

/// Transport for the session (pi pattern).
#[derive(Debug, Clone, Default)]
pub enum SessionTransport {
    /// Direct in-process embedding.
    #[default]
    InProcess,
    /// RPC subprocess (spawns a child process running `rx4 --mode rpc`).
    RpcSubprocess { command: String },
}

/// Options for creating an agent session (pi SDK).
#[derive(Debug, Clone)]
pub struct AgentSessionOptions {
    pub model: String,
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub scope: String,
    pub workspace_root: Option<std::path::PathBuf>,
    pub max_tool_iterations: usize,
    pub auto_compact_after: usize,
    pub transport: SessionTransport,
}

impl Default for AgentSessionOptions {
    fn default() -> Self {
        Self {
            model: "gpt-5.5".into(),
            provider: None,
            api_key: None,
            scope: "coding".into(),
            workspace_root: None,
            max_tool_iterations: 50,
            auto_compact_after: 0,
            transport: SessionTransport::default(),
        }
    }
}

/// Event listener callback type.
pub type EventListener = Arc<dyn Fn(&Event) + Send + Sync>;

/// Handle to an agent session — pi SDK surface.
/// Uses tokio::sync::Mutex for the agent (async-safe across .await points)
/// and parking_lot::Mutex for the listeners (sync, never held across await).
pub struct AgentSessionHandle {
    agent: Arc<Mutex<Agent>>,
    /// Shared history, so reads do not queue behind an in-flight turn.
    messages: Arc<parking_lot::RwLock<Vec<Message>>>,
    listeners: SyncMutex<Vec<EventListener>>,
    transport: SessionTransport,
}

impl AgentSessionHandle {
    pub fn new(agent: Agent, transport: SessionTransport) -> Self {
        let messages = agent.messages_handle();
        Self {
            agent: Arc::new(Mutex::new(agent)),
            messages,
            listeners: SyncMutex::new(Vec::new()),
            transport,
        }
    }

    /// Subscribe to events from the session.
    pub fn subscribe(&self, listener: impl Fn(&Event) + Send + Sync + 'static) {
        self.listeners.lock().push(Arc::new(listener));
    }

    /// Send a prompt to the agent.
    pub async fn prompt(&self, text: &str, _on_event: impl Fn(&Event)) -> Result<(), SdkError> {
        let listeners: Vec<EventListener> = self.listeners.lock().clone();

        {
            let mut a = self.agent.lock().await;
            for listener in listeners {
                let l = listener;
                a.subscribe(move |e| l(e));
            }
        }

        let result = self.agent.lock().await.prompt(text).await;
        result.map_err(|e| SdkError::Agent(e.to_string()))
    }

    /// Set the model for the session.
    pub async fn set_model(&self, provider: &str, model: &str) {
        let _ = provider;
        let mut a = self.agent.lock().await;
        a.set_model(model);
    }

    /// Trigger context compaction.
    pub async fn compact(&self) {
        let a = self.agent.lock().await;
        a.compact("sdk compact");
    }

    /// Get the current model.
    pub async fn model(&self) -> String {
        self.agent.lock().await.model.clone()
    }

    /// Get the current message count.
    pub async fn message_count(&self) -> usize {
        self.messages.read().len()
    }

    /// Get all messages.
    pub async fn messages(&self) -> Vec<Message> {
        self.messages.read().clone()
    }

    /// Clear all messages.
    pub async fn clear(&self) {
        self.agent.lock().await.clear_messages();
    }

    /// Abort the current operation (best-effort).
    pub fn abort(&self) {
        info!("abort requested via SDK");
    }

    /// Get the transport type.
    pub fn transport(&self) -> &SessionTransport {
        &self.transport
    }
}

impl Clone for AgentSessionHandle {
    fn clone(&self) -> Self {
        Self {
            agent: self.agent.clone(),
            messages: self.messages.clone(),
            listeners: SyncMutex::new(self.listeners.lock().clone()),
            transport: self.transport.clone(),
        }
    }
}

/// Create an agent session (pi SDK entry point).
pub fn create_agent_session(options: AgentSessionOptions) -> AgentSessionHandle {
    let provider = options.api_key.as_ref().map(|api_key| {
        let provider: Arc<dyn rx4::provider::Provider> = match options.provider.as_deref() {
            Some("openai-codex") | Some("chatgpt") => crate::codex_provider::provider_arc(api_key),
            Some("ollama") | Some("local") => Arc::new(rx4::provider::OpenAIProvider::ollama()),
            _ => Arc::new(rx4::provider::OpenAIProvider::new(api_key)),
        };
        provider
    });
    let workspace = options.workspace_root.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    let (mut agent, _subagent) = build_agent(
        provider,
        &options.model,
        "high",
        workspace,
        ModelRegistry::new(),
        &[],
    );
    agent.max_tool_iterations = options.max_tool_iterations;
    agent.auto_compact_after = options.auto_compact_after;
    if let Ok(scope) = parse_host_scope(&options.scope) {
        apply_scope(&mut agent, scope);
    }

    info!(
        "created agent session: model={}, scope={}",
        options.model, options.scope
    );
    AgentSessionHandle::new(agent, options.transport)
}

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("agent error: {0}")]
    Agent(String),
    #[error("transport error: {0}")]
    Transport(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_session_defaults() {
        let handle = create_agent_session(AgentSessionOptions::default());
        assert_eq!(handle.model().await, "gpt-5.5");
        assert_eq!(handle.message_count().await, 0);
        assert!(matches!(handle.transport(), SessionTransport::InProcess));
    }

    #[tokio::test]
    async fn create_session_custom() {
        let handle = create_agent_session(AgentSessionOptions {
            model: "gpt-5.4-mini".into(),
            scope: "research".into(),
            ..Default::default()
        });
        assert_eq!(handle.model().await, "gpt-5.4-mini");
    }

    #[tokio::test]
    async fn session_handle_clone() {
        let handle = create_agent_session(AgentSessionOptions::default());
        let cloned = handle.clone();
        assert_eq!(handle.model().await, cloned.model().await);
    }
}
