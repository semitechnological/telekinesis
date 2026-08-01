//! Pi RPC protocol — JSON Lines over stdin/stdout.
//!
//! Compatible with pi_agent_rust RPC mode (`pi --mode rpc`).
//! Line-delimited JSON, each line is a request or event.

use rx4::agent::Agent;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, info, warn};

/// RPC commands that a client can send (pi protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method")]
pub enum PiRpcCommand {
    #[serde(rename = "prompt")]
    Prompt { text: String },
    #[serde(rename = "steer")]
    Steer { text: String },
    #[serde(rename = "follow-up")]
    FollowUp { text: String },
    #[serde(rename = "abort")]
    Abort,
    #[serde(rename = "get-state")]
    GetState,
    #[serde(rename = "compact")]
    Compact,
    #[serde(rename = "set-model")]
    SetModel { provider: String, model: String },
    #[serde(rename = "subscribe")]
    Subscribe,
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
}

/// RPC events that the server emits (pi protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PiRpcEvent {
    #[serde(rename = "agent_start")]
    AgentStart,
    #[serde(rename = "turn_start")]
    TurnStart { turn: usize },
    #[serde(rename = "message_delta")]
    MessageDelta { delta: String },
    #[serde(rename = "message_end")]
    MessageEnd { role: String, content: String },
    #[serde(rename = "tool_call")]
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        content: String,
        is_error: bool,
    },
    #[serde(rename = "turn_end")]
    TurnEnd { turn: usize },
    #[serde(rename = "agent_end")]
    AgentEnd,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "state")]
    State {
        model: String,
        scope: String,
        message_count: usize,
        tool_count: usize,
    },
    #[serde(rename = "compaction")]
    Compaction { summary: String },
}

impl PiRpcEvent {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// Model and scope shadowed outside the agent mutex so `get-state` can answer
/// without waiting for an in-flight turn to release it.
#[derive(Debug, Clone)]
struct ShadowState {
    model: String,
    scope: String,
}

/// RPC server — reads commands from stdin, writes events to stdout.
pub struct PiRpcServer {
    agent: Arc<tokio::sync::Mutex<Agent>>,
    messages: Arc<parking_lot::RwLock<Vec<rx4::provider::Message>>>,
    tools: Arc<rx4::agent::ToolRegistry>,
    shadow: Arc<parking_lot::RwLock<ShadowState>>,
    runtime: std::sync::OnceLock<tokio::runtime::Runtime>,
}

impl PiRpcServer {
    pub fn new(agent: Agent) -> Self {
        let messages = agent.messages_handle();
        let tools = Arc::clone(&agent.tools);
        let shadow = ShadowState {
            model: agent.model.clone(),
            scope: agent.scope.name().to_string(),
        };
        Self {
            agent: Arc::new(tokio::sync::Mutex::new(agent)),
            messages,
            tools,
            shadow: Arc::new(parking_lot::RwLock::new(shadow)),
            runtime: std::sync::OnceLock::new(),
        }
    }

    /// Build a `state` event purely from lock-free handles. Never touches the
    /// agent mutex, so it answers while a turn is running.
    fn state_event(&self) -> PiRpcEvent {
        let shadow = self.shadow.read().clone();
        PiRpcEvent::State {
            model: shadow.model,
            scope: shadow.scope,
            message_count: self.messages.read().len(),
            tool_count: self.tools.count(),
        }
    }

    /// Handle for the runtime that runs prompts: the ambient one when the
    /// server is driven from async code, otherwise a single owned runtime
    /// shared by every command.
    fn handle(&self) -> tokio::runtime::Handle {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            return handle;
        }
        self.runtime
            .get_or_init(|| {
                tokio::runtime::Runtime::new().expect("failed to build pi RPC tokio runtime")
            })
            .handle()
            .clone()
    }

    /// Run the RPC server on stdin/stdout.
    /// Blocks until stdin is closed or an abort command is received.
    pub fn run(&self) -> std::io::Result<()> {
        use std::io::{BufRead, Write};
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut stdout = stdout.lock();

        info!("pi RPC server started on stdin/stdout");

        for line in stdin.lock().lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    warn!("stdin read error: {e}");
                    break;
                }
            };
            if line.is_empty() {
                continue;
            }

            let response = self.handle_command(&line);
            if let Some(event) = response {
                writeln!(stdout, "{event}")?;
                stdout.flush()?;
            }
        }

        info!("pi RPC server shutting down");
        Ok(())
    }

    pub fn handle_command(&self, line: &str) -> Option<String> {
        let cmd: PiRpcCommand = match serde_json::from_str(line) {
            Ok(c) => c,
            Err(e) => {
                let event = PiRpcEvent::Error {
                    message: format!("parse error: {e}"),
                };
                return Some(event.to_json());
            }
        };

        match cmd {
            PiRpcCommand::GetState => Some(self.state_event().to_json()),
            PiRpcCommand::SetModel { provider, model } => {
                let _ = provider;
                // Record the request in the shadow first so `get-state` reflects
                // it immediately, then apply it to the agent off-thread. The
                // applier re-reads the shadow, so if several set-model commands
                // queue behind one turn they all converge on the last request
                // rather than racing on spawn order.
                self.shadow.write().model = model;
                let agent = self.agent.clone();
                let shadow = self.shadow.clone();
                self.handle().spawn(async move {
                    let target = shadow.read().model.clone();
                    let mut agent = agent.lock().await;
                    agent.set_model(&target);
                });
                Some(self.state_event().to_json())
            }
            PiRpcCommand::Compact => {
                // Compaction rewrites the same history vector the tool loop
                // reads at the top of every iteration, so it must not run
                // mid-turn. Queue it behind the agent mutex on the runtime
                // instead of blocking the stdin reader on it.
                let agent = self.agent.clone();
                self.handle().spawn(async move {
                    let agent = agent.lock().await;
                    agent.compact("rpc compact command");
                });
                let event = PiRpcEvent::Compaction {
                    summary: "context compacted".into(),
                };
                Some(event.to_json())
            }
            PiRpcCommand::Abort => {
                info!("abort received — stopping RPC server");
                None
            }
            PiRpcCommand::Prompt { text } => {
                let agent = self.agent.clone();
                self.handle().spawn(async move {
                    let mut agent = agent.lock().await;
                    if let Err(e) = agent.prompt(&text).await {
                        error!("prompt error: {e}");
                    }
                });
                let event = PiRpcEvent::AgentStart;
                Some(event.to_json())
            }
            PiRpcCommand::Steer { text } => {
                // Push straight through the shared history handle — no agent
                // mutex, so this lands while a turn is in flight and the next
                // tool iteration picks it up. The write guard is taken and
                // dropped in one statement because the tool loop takes the same
                // lock every iteration.
                self.messages
                    .write()
                    .push(rx4::provider::Message::user(text));
                Some(self.state_event().to_json())
            }
            PiRpcCommand::FollowUp { text } => {
                let agent = self.agent.clone();
                self.handle().spawn(async move {
                    let mut agent = agent.lock().await;
                    if let Err(e) = agent.prompt(&text).await {
                        error!("follow-up error: {e}");
                    }
                });
                Some(PiRpcEvent::AgentStart.to_json())
            }
            PiRpcCommand::Subscribe | PiRpcCommand::Unsubscribe => Some(
                PiRpcEvent::State {
                    model: "ok".into(),
                    scope: "ok".into(),
                    message_count: 0,
                    tool_count: 0,
                }
                .to_json(),
            ),
        }
    }
}

/// RPC client — connects to a pi RPC server subprocess or in-process.
pub struct PiRpcClient {
    writer: Box<dyn std::io::Write + Send>,
}

impl PiRpcClient {
    pub fn new(writer: Box<dyn std::io::Write + Send>) -> Self {
        Self { writer }
    }

    pub fn send_command(&mut self, cmd: &PiRpcCommand) -> std::io::Result<()> {
        let json = serde_json::to_string(cmd)?;
        writeln!(self.writer, "{json}")?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn prompt(&mut self, text: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Prompt { text: text.into() })
    }

    pub fn steer(&mut self, text: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Steer { text: text.into() })
    }

    pub fn abort(&mut self) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::Abort)
    }

    pub fn get_state(&mut self) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::GetState)
    }

    pub fn set_model(&mut self, provider: &str, model: &str) -> std::io::Result<()> {
        self.send_command(&PiRpcCommand::SetModel {
            provider: provider.into(),
            model: model.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serialization() {
        let cmd = PiRpcCommand::Prompt {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        assert!(json.contains("\"method\":\"prompt\""));
        assert!(json.contains("\"text\":\"hello\""));

        let parsed: PiRpcCommand = serde_json::from_str(&json).unwrap();
        match parsed {
            PiRpcCommand::Prompt { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn event_serialization() {
        let event = PiRpcEvent::MessageDelta { delta: "hi".into() };
        let json = event.to_json();
        assert!(json.contains("\"type\":\"message_delta\""));
    }

    #[test]
    fn abort_command_parses() {
        let json = r#"{"method":"abort"}"#;
        let cmd: PiRpcCommand = serde_json::from_str(json).unwrap();
        assert!(matches!(cmd, PiRpcCommand::Abort));
    }

    fn state_of(json: &str) -> (String, usize, usize) {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["type"], "state");
        (
            value["model"].as_str().unwrap().to_string(),
            value["message_count"].as_u64().unwrap() as usize,
            value["tool_count"].as_u64().unwrap() as usize,
        )
    }

    /// A turn holds the agent mutex for its whole duration. `get-state` must
    /// answer anyway.
    #[tokio::test]
    async fn get_state_answers_while_a_turn_holds_the_agent() {
        let server = Arc::new(PiRpcServer::new(Agent::new()));
        let held = server.agent.clone().lock_owned().await;

        let answered = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::task::spawn_blocking({
                let server = server.clone();
                move || server.handle_command(r#"{"method":"get-state"}"#)
            }),
        )
        .await
        .expect("get-state blocked on the in-flight turn")
        .unwrap()
        .unwrap();

        let (_, message_count, _) = state_of(&answered);
        assert_eq!(message_count, 0);
        drop(held);
    }

    /// Steering must land in the agent's own history — the vector its tool loop
    /// re-reads each iteration — without taking the agent mutex.
    #[tokio::test]
    async fn steer_lands_mid_turn_exactly_once_and_in_order() {
        let agent = Agent::new();
        let observed = agent.messages_handle();
        let server = Arc::new(PiRpcServer::new(agent));
        let held = server.agent.clone().lock_owned().await;

        for text in ["first", "second"] {
            let line = serde_json::to_string(&PiRpcCommand::Steer { text: text.into() }).unwrap();
            let response = tokio::time::timeout(
                std::time::Duration::from_secs(1),
                tokio::task::spawn_blocking({
                    let server = server.clone();
                    move || server.handle_command(&line)
                }),
            )
            .await
            .expect("steer blocked on the in-flight turn")
            .unwrap()
            .unwrap();
            assert_eq!(state_of(&response).0, server.shadow.read().model);
        }

        // Visible to the agent while the turn still holds the mutex.
        let seen: Vec<String> = observed
            .read()
            .iter()
            .map(|message| message.content.clone())
            .collect();
        assert_eq!(seen, vec!["first".to_string(), "second".to_string()]);
        drop(held);
    }

    /// `set-model` must not wedge the reader, and the shadow must answer with
    /// the requested model straight away.
    #[tokio::test]
    async fn set_model_reports_immediately_without_the_agent() {
        let server = Arc::new(PiRpcServer::new(Agent::new()));
        let held = server.agent.clone().lock_owned().await;

        let line = serde_json::to_string(&PiRpcCommand::SetModel {
            provider: "openai-codex".into(),
            model: "some-model".into(),
        })
        .unwrap();
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::task::spawn_blocking({
                let server = server.clone();
                move || server.handle_command(&line)
            }),
        )
        .await
        .expect("set-model blocked on the in-flight turn")
        .unwrap()
        .unwrap();
        assert_eq!(state_of(&response).0, "some-model");

        // The agent itself is still on the old model until the turn ends.
        drop(held);
        tokio::task::yield_now().await;
        for _ in 0..100 {
            if server.agent.lock().await.model == "some-model" {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("set-model never reached the agent");
    }

    /// `compact` is deferred behind the agent mutex rather than blocking the
    /// reader on it.
    #[tokio::test]
    async fn compact_does_not_block_the_reader() {
        let server = Arc::new(PiRpcServer::new(Agent::new()));
        let held = server.agent.clone().lock_owned().await;

        let response = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::task::spawn_blocking({
                let server = server.clone();
                move || server.handle_command(r#"{"method":"compact"}"#)
            }),
        )
        .await
        .expect("compact blocked on the in-flight turn")
        .unwrap()
        .unwrap();

        let value: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["type"], "compaction");
        drop(held);
    }
}
