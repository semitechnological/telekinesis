use rx4::agent::ToolCall;
use rx4::permissions::Decision;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::agent::{oauth_provider, runtime, setup_agents, AgentSetup};
use crate::session::{AgentSession, CompanionEvent, MessageItem, PointTarget, SessionKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostCommand {
    Login(Option<String>),
    Clear,
    ComputerUse,
    Coding,
}

pub fn parse_host_command(text: &str) -> Option<HostCommand> {
    let text = text.trim();
    if !text.starts_with('/') {
        return None;
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let cmd = parts.next()?.to_ascii_lowercase();
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match cmd.as_str() {
        "/login" => Some(HostCommand::Login(arg.map(str::to_string))),
        "/clear" => Some(HostCommand::Clear),
        "/computer" | "/computer_use" => Some(HostCommand::ComputerUse),
        "/coding" => Some(HostCommand::Coding),
        "/scope" => match arg {
            Some("computer_use") | Some("computer") => Some(HostCommand::ComputerUse),
            Some("coding") => Some(HostCommand::Coding),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct HostSnapshot {
    pub input: String,
    pub model: String,
    pub busy: bool,
    pub connected: bool,
    pub context_pct: usize,
    pub status: String,
    pub provider: String,
    pub messages: Vec<MessageItem>,
    pub login_busy: bool,
    pub permission_pending: bool,
    pub session_name: String,
    pub session_kind: SessionKind,
    pub computer_active: bool,
    pub coding_active: bool,
}

#[derive(Default)]
pub struct HostTick {
    pub dirty: bool,
    pub point: Option<PointTarget>,
}

pub struct CompanionHost {
    pub input: String,
    sessions: Vec<AgentSession>,
    active_session: usize,
    event_rx: UnboundedReceiver<CompanionEvent>,
    event_tx: UnboundedSender<CompanionEvent>,
    approval_rx: Option<std::sync::mpsc::Receiver<(ToolCall, std::sync::mpsc::Sender<Decision>)>>,
    permission_respond: Option<std::sync::mpsc::Sender<Decision>>,
    permission_pending: bool,
    login_busy: bool,
}

impl CompanionHost {
    pub fn boot() -> Self {
        let (event_tx, event_rx) = unbounded_channel();
        let mut host = Self {
            input: String::new(),
            sessions: Vec::new(),
            active_session: 0,
            event_rx,
            event_tx: event_tx.clone(),
            approval_rx: None,
            permission_respond: None,
            permission_pending: false,
            login_busy: false,
        };
        if let Some(setup) = setup_agents(runtime(), event_tx) {
            host.apply_setup(setup);
        } else {
            host.sessions.push(Self::login_session());
        }
        host
    }

    fn login_session() -> AgentSession {
        let mut session =
            AgentSession::new("login required", SessionKind::Coding, None, "not connected");
        session.messages.push(MessageItem::new(
            "system",
            "Log in to start. Click login or run `tk login openai`, then send a prompt.",
        ));
        session
    }

    fn apply_setup(&mut self, setup: AgentSetup) {
        let AgentSetup {
            computer_use,
            computer_use_cancel,
            coding,
            coding_cancel,
            model,
            approval_rx,
        } = setup;
        let mut computer = AgentSession::new(
            "computer use",
            SessionKind::ComputerUse,
            Some(computer_use),
            &model,
        );
        computer.cancellation = Some(computer_use_cancel);
        let mut code = AgentSession::new("coding", SessionKind::Coding, Some(coding), &model);
        code.cancellation = Some(coding_cancel);
        self.sessions = vec![computer, code];
        self.active_session = 0;
        self.approval_rx = Some(approval_rx);
        self.permission_pending = false;
        self.permission_respond = None;
    }

    fn active_session(&self) -> Option<&AgentSession> {
        self.sessions.get(self.active_session)
    }

    fn active_session_mut(&mut self) -> Option<&mut AgentSession> {
        self.sessions.get_mut(self.active_session)
    }

    fn push_system(&mut self, content: impl Into<String>) {
        if let Some(session) = self.active_session_mut() {
            session.messages.push(MessageItem::new("system", content));
        }
    }

    pub fn snapshot(&self) -> HostSnapshot {
        let session = self.active_session();
        let model = session
            .map(|s| s.model.clone())
            .unwrap_or_else(|| "not connected".into());
        let busy = session.map(|s| s.busy).unwrap_or(false);
        let context_pct = session.map(|s| s.context_pct).unwrap_or(0);
        let connected = session.and_then(|s| s.agent.as_ref()).is_some();
        let status = if self.login_busy {
            "logging in".to_string()
        } else if connected {
            if busy {
                "working".into()
            } else {
                "ready".into()
            }
        } else {
            "login required".into()
        };
        let provider = if connected {
            match model.as_str() {
                "gpt-5.5" => "ChatGPT Codex",
                "gpt-5.4" => "OpenAI",
                "claude-sonnet-4-5" => "Claude",
                "grok-4.5" => "xAI",
                "gemini-2.0-flash" => "Google Gemini",
                "kimi-k2.5" => "Kimi",
                _ => "AI",
            }
        } else {
            "none"
        };
        HostSnapshot {
            input: self.input.clone(),
            model,
            busy,
            connected,
            context_pct,
            status,
            provider: provider.into(),
            messages: session
                .map(AgentSession::render_messages)
                .unwrap_or_default(),
            login_busy: self.login_busy,
            permission_pending: self.permission_pending,
            session_name: session
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "none".into()),
            session_kind: session.map(|s| s.kind).unwrap_or(SessionKind::Coding),
            computer_active: session
                .map(|s| s.kind == SessionKind::ComputerUse)
                .unwrap_or(false),
            coding_active: session
                .map(|s| s.kind == SessionKind::Coding)
                .unwrap_or(false),
        }
    }

    pub fn poll(&mut self) -> HostTick {
        let mut tick = HostTick::default();
        while let Ok(event) = self.event_rx.try_recv() {
            tick.dirty = true;
            match event {
                CompanionEvent::Session(idx, e) => {
                    if let Some(session) = self.sessions.get_mut(idx) {
                        if let Some(point) = session.handle_rx4_event(e) {
                            tick.point = Some(point);
                        }
                    }
                }
                CompanionEvent::SessionError(idx, msg) => {
                    if let Some(session) = self.sessions.get_mut(idx) {
                        session
                            .messages
                            .push(MessageItem::new("error", format!("Error: {msg}")));
                        session.busy = false;
                    }
                }
                CompanionEvent::PromptFinished(idx) => {
                    if let Some(session) = self.sessions.get_mut(idx) {
                        session.busy = false;
                    }
                }
                CompanionEvent::LoginSucceeded => {
                    self.login_busy = false;
                    if let Some(setup) = setup_agents(runtime(), self.event_tx.clone()) {
                        self.apply_setup(setup);
                        self.push_system("Logged in. Ready.");
                    } else {
                        self.push_system(
                            "Login saved, but no provider is ready. Check `tk login` or API keys.",
                        );
                    }
                }
                CompanionEvent::LoginFailed(msg) => {
                    self.login_busy = false;
                    self.push_system(format!("Login failed: {msg}"));
                }
            }
        }
        if self.poll_approvals() {
            tick.dirty = true;
        }
        tick
    }

    pub fn poll_approvals(&mut self) -> bool {
        let mut pending = Vec::new();
        if let Some(rx) = self.approval_rx.as_ref() {
            while let Ok(item) = rx.try_recv() {
                pending.push(item);
            }
        }
        let dirty = !pending.is_empty();
        for (call, respond) in pending {
            self.permission_pending = true;
            self.permission_respond = Some(respond);
            self.push_system(format!(
                "Approval required: {}\nargs: {}\n[y] allow  [n] deny",
                call.name,
                call.arguments.chars().take(200).collect::<String>()
            ));
        }
        dirty
    }

    pub fn resolve_permission(&mut self, allow: bool) {
        if let Some(tx) = self.permission_respond.take() {
            let _ = tx.send(if allow {
                Decision::Allow
            } else {
                Decision::Deny
            });
        }
        self.permission_pending = false;
    }

    pub fn set_active_session(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.active_session = idx;
        }
    }

    pub fn use_computer(&mut self) {
        if let Some((idx, _)) = self
            .sessions
            .iter()
            .enumerate()
            .find(|(_, session)| session.kind == SessionKind::ComputerUse)
        {
            self.active_session = idx;
        }
    }

    pub fn use_coding(&mut self) {
        if let Some((idx, _)) = self
            .sessions
            .iter()
            .enumerate()
            .find(|(_, session)| session.kind == SessionKind::Coding && session.agent.is_some())
        {
            self.active_session = idx;
        }
    }

    pub fn start_login(&mut self, provider: Option<&str>) {
        if self.login_busy {
            return;
        }
        let name = provider.unwrap_or("openai");
        let Some(oauth) = oauth_provider(name) else {
            self.push_system(format!(
                "Unknown provider: {name}. Try openai, grok, claude, or gemini."
            ));
            return;
        };
        self.login_busy = true;
        self.push_system(format!("Starting OAuth login for {name}..."));
        let tx = self.event_tx.clone();
        std::thread::spawn(move || match rs_ai_oauth::start_oauth_flow(oauth) {
            Ok(tokens) => {
                if let Err(error) = rs_ai_oauth::credentials::save(&oauth, &tokens) {
                    let _ = tx.send(CompanionEvent::LoginFailed(error.to_string()));
                    return;
                }
                let _ = tx.send(CompanionEvent::LoginSucceeded);
            }
            Err(error) => {
                let _ = tx.send(CompanionEvent::LoginFailed(error.to_string()));
            }
        });
    }

    pub fn send_prompt(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(command) = parse_host_command(&text) {
            self.input.clear();
            self.dispatch_command(command);
            return;
        }
        if text.starts_with('/') {
            self.input.clear();
            self.push_system(format!(
                "unknown command: {text}. Try /login, /clear, /coding, /computer."
            ));
            return;
        }

        let agent = {
            let Some(session) = self.active_session_mut() else {
                return;
            };
            if session.busy {
                return;
            }
            let Some(agent) = session.agent.clone() else {
                session.messages.push(MessageItem::new(
                    "system",
                    "Log in first. Click login or send `/login openai`.",
                ));
                return;
            };
            session
                .messages
                .push(MessageItem::new("user", text.clone()));
            session.busy = true;
            agent
        };
        self.input.clear();
        let session_idx = self.active_session;
        let tx = self.event_tx.clone();
        runtime().handle().spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(error) = agent.prompt(&text).await {
                let _ = tx.send(CompanionEvent::SessionError(session_idx, error.to_string()));
            }
            let _ = tx.send(CompanionEvent::PromptFinished(session_idx));
        });
    }

    fn dispatch_command(&mut self, command: HostCommand) {
        match command {
            HostCommand::Login(provider) => {
                self.start_login(provider.as_deref());
            }
            HostCommand::Clear => {
                if let Some(session) = self.active_session_mut() {
                    session.messages.clear();
                    session.streaming_role = None;
                    session.streaming_content.clear();
                    session.busy = false;
                    session.context_pct = 0;
                }
            }
            HostCommand::ComputerUse => self.use_computer(),
            HostCommand::Coding => self.use_coding(),
        }
    }

    pub fn capture_screen(&mut self) {
        let session_idx = self.active_session;
        let agent = {
            let Some(session) = self.active_session_mut() else {
                return;
            };
            if session.busy {
                return;
            }
            let Some(agent) = session.agent.clone() else {
                session.messages.push(MessageItem::new(
                    "system",
                    "Log in first. Click login or send `/login openai`.",
                ));
                return;
            };
            session
                .messages
                .push(MessageItem::new("user", "see screen"));
            session.busy = true;
            agent
        };
        let tx = self.event_tx.clone();
        runtime().handle().spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(error) = agent
                .prompt("Use cu_see to capture my screen, then tell me what you see. Wait for my next instruction.")
                .await
            {
                let _ = tx.send(CompanionEvent::SessionError(session_idx, error.to_string()));
            }
            let _ = tx.send(CompanionEvent::PromptFinished(session_idx));
        });
    }

    pub fn interrupt(&mut self) {
        if let Some(session) = self.active_session_mut() {
            if let Some(cancellation) = &session.cancellation {
                cancellation.cancel();
            }
            session.busy = false;
            session.streaming_role = None;
            session.streaming_content.clear();
        }
    }

    pub fn push_char(&mut self, ch: &str) {
        self.input.push_str(ch);
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_commands() {
        assert_eq!(
            parse_host_command("/login grok"),
            Some(HostCommand::Login(Some("grok".into())))
        );
        assert_eq!(parse_host_command("/clear"), Some(HostCommand::Clear));
        assert_eq!(
            parse_host_command("/scope computer_use"),
            Some(HostCommand::ComputerUse)
        );
        assert_eq!(parse_host_command("hello"), None);
    }

    #[test]
    fn empty_send_is_noop() {
        let mut host = CompanionHost::boot();
        let before = host.snapshot().messages.len();
        host.send_prompt();
        assert_eq!(host.snapshot().messages.len(), before);
    }

    #[test]
    fn unknown_login_provider_is_reported() {
        let mut host = CompanionHost::boot();
        host.input = "/login not-a-provider".into();
        host.send_prompt();
        let snap = host.snapshot();
        assert!(snap
            .messages
            .iter()
            .any(|message| message.content.contains("Unknown provider")));
        assert!(host.input.is_empty());
    }

    #[test]
    fn unknown_slash_is_not_sent_as_prompt() {
        let mut host = CompanionHost::boot();
        host.input = "/nope".into();
        host.send_prompt();
        assert!(host
            .snapshot()
            .messages
            .iter()
            .any(|message| message.content.contains("unknown command")));
    }

    #[test]
    fn poll_applies_stream_deltas() {
        let mut host = CompanionHost::boot();
        if host.sessions.is_empty() {
            host.sessions
                .push(AgentSession::new("coding", SessionKind::Coding, None, "m"));
        }
        let idx = host.active_session;
        let _ = host.event_tx.send(CompanionEvent::Session(
            idx,
            rx4::agent::Event::MessageDelta {
                delta: "streamed".into(),
            },
        ));
        if let Some(session) = host.sessions.get_mut(idx) {
            session.streaming_role = Some("assistant".into());
        }
        let tick = host.poll();
        assert!(tick.dirty);
        assert!(host
            .snapshot()
            .messages
            .iter()
            .any(|message| message.content.contains("streamed")));
    }
}
