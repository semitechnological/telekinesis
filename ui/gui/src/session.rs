use std::sync::Arc;

use rx4::agent::{Agent, CancellationHandle, Event as Rx4Event, ToolSource};
use rx4::provider::Role;
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageItem {
    pub role: String,
    pub content: String,
    pub is_tool: bool,
    pub is_user: bool,
    pub is_error: bool,
}

impl MessageItem {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        let is_tool = role.starts_with("tool:");
        let is_user = role == "user";
        let is_error = role == "error";
        Self {
            role: role.to_string(),
            content: content.into(),
            is_tool,
            is_user,
            is_error,
        }
    }
}

#[derive(Clone, Debug)]
pub enum CompanionEvent {
    Session(usize, Rx4Event),
    SessionError(usize, String),
    PromptFinished(usize),
    LoginSucceeded,
    LoginFailed(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionKind {
    ComputerUse,
    Coding,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PointTarget {
    pub x: f32,
    pub y: f32,
    pub label: String,
}

pub fn split_point_tag(text: &str) -> (String, Option<PointTarget>) {
    const PREFIX: &str = "[POINT:";
    let Some(start) = text.rfind(PREFIX) else {
        return (text.to_string(), None);
    };
    let after = &text[start + PREFIX.len()..];
    let Some(end) = after.find(']') else {
        return (text.to_string(), None);
    };
    if !after[end + 1..].trim().is_empty() {
        return (text.to_string(), None);
    }
    let inner = &after[..end];
    let visible = text[..start].trim_end().to_string();
    if inner.eq_ignore_ascii_case("none") {
        return (visible, None);
    }
    let (coords, label) = match inner.split_once(':') {
        Some((coords, label)) => (coords, label.trim().to_string()),
        None => (inner, String::new()),
    };
    let Some((x, y)) = coords.split_once(',') else {
        return (text.to_string(), None);
    };
    let (Ok(x), Ok(y)) = (x.trim().parse::<f32>(), y.trim().parse::<f32>()) else {
        return (text.to_string(), None);
    };
    (visible, Some(PointTarget { x, y, label }))
}

pub struct AgentSession {
    pub name: String,
    pub kind: SessionKind,
    pub agent: Option<Arc<Mutex<Agent>>>,
    pub cancellation: Option<CancellationHandle>,
    pub messages: Vec<MessageItem>,
    pub streaming_role: Option<String>,
    pub streaming_content: String,
    pub busy: bool,
    pub model: String,
    pub context_pct: usize,
}

impl AgentSession {
    pub fn new(
        name: &str,
        kind: SessionKind,
        agent: Option<Arc<Mutex<Agent>>>,
        model: &str,
    ) -> Self {
        let cancellation = agent
            .as_ref()
            .and_then(|agent| agent.try_lock().ok())
            .map(|agent| agent.cancellation_handle());
        Self {
            name: name.to_string(),
            kind,
            agent,
            cancellation,
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            busy: false,
            model: model.to_string(),
            context_pct: 0,
        }
    }

    pub fn render_messages(&self) -> Vec<MessageItem> {
        let mut messages = self.messages.clone();
        if let Some(role) = &self.streaming_role {
            messages.push(MessageItem::new(role, self.streaming_content.clone()));
        }
        messages
    }

    pub fn handle_rx4_event(&mut self, event: Rx4Event) -> Option<PointTarget> {
        match event {
            Rx4Event::AgentStart => None,
            Rx4Event::ContextUsage {
                used_tokens,
                context_window,
                ..
            } => {
                self.context_pct = used_tokens
                    .saturating_mul(100)
                    .checked_div(context_window)
                    .unwrap_or(0);
                None
            }
            Rx4Event::Usage { .. } => None,
            Rx4Event::CompactionStart { .. } => {
                self.messages
                    .push(MessageItem::new("tool:context", "compacting"));
                None
            }
            Rx4Event::CompactionEnd { result, .. } => {
                self.messages.push(MessageItem::new(
                    "tool:context",
                    format!("{} tokens remain", result.remaining_tokens),
                ));
                None
            }
            Rx4Event::SkillActivated { name, .. } => {
                self.messages.push(MessageItem::new("tool:skill", name));
                None
            }
            Rx4Event::ToolSource { tool, source } => {
                let activity = match source {
                    ToolSource::Builtin => None,
                    ToolSource::Mcp { server } => Some(format!("{server} (MCP)")),
                    ToolSource::ComputerUse => Some(tool),
                };
                if let Some(activity) = activity {
                    self.messages.push(MessageItem::new("tool:used", activity));
                }
                None
            }
            Rx4Event::TurnStart { .. } => {
                self.streaming_role = Some("assistant".to_string());
                self.streaming_content.clear();
                self.busy = true;
                None
            }
            Rx4Event::MessageStart { role } => {
                if role == Role::Assistant
                    && self
                        .messages
                        .last()
                        .is_none_or(|m| m.role != "assistant" || !m.content.is_empty())
                {
                    self.streaming_role = Some("assistant".to_string());
                    self.streaming_content.clear();
                }
                None
            }
            Rx4Event::MessageDelta { delta } => {
                self.streaming_content.push_str(&delta);
                None
            }
            Rx4Event::MessageEnd { content, .. } => {
                let role = self
                    .streaming_role
                    .take()
                    .unwrap_or_else(|| "assistant".to_string());
                let raw = if content.is_empty() {
                    std::mem::take(&mut self.streaming_content)
                } else {
                    content
                };
                let (visible, point) = split_point_tag(&raw);
                self.messages.push(MessageItem::new(&role, visible));
                self.streaming_content.clear();
                point
            }
            Rx4Event::ToolCall(call) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(
                        &role,
                        std::mem::take(&mut self.streaming_content),
                    ));
                }
                let tool_role = format!("tool:{}", call.name);
                self.streaming_role = Some(tool_role);
                self.streaming_content.clear();
                self.busy = true;
                None
            }
            Rx4Event::ApprovalRequired(req) => {
                self.messages.push(MessageItem::new(
                    "system",
                    format!("Approval required: {} ({})", req.tool_name, req.reason),
                ));
                None
            }
            Rx4Event::ToolExecutionStart(_) => None,
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, result.content));
                }
                self.streaming_content.clear();
                None
            }
            Rx4Event::TurnEnd { .. } => None,
            Rx4Event::AgentEnd => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(
                        &role,
                        std::mem::take(&mut self.streaming_content),
                    ));
                }
                self.busy = false;
                None
            }
            Rx4Event::Error(msg) => {
                self.messages
                    .push(MessageItem::new("error", format!("Error: {msg}")));
                self.busy = false;
                None
            }
            Rx4Event::BudgetExceeded { reason } => {
                self.messages.push(MessageItem::new(
                    "error",
                    format!("Budget exceeded: {reason}"),
                ));
                self.busy = false;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_point_tag_from_assistant_text() {
        let (text, point) = split_point_tag("click the inspector\n[POINT:1100,42:color inspector]");
        assert_eq!(text, "click the inspector");
        let point = point.expect("point");
        assert_eq!(point.x, 1100.0);
        assert_eq!(point.y, 42.0);
        assert_eq!(point.label, "color inspector");
    }

    #[test]
    fn strips_point_none() {
        let (text, point) = split_point_tag("html is the skeleton. [POINT:none]");
        assert_eq!(text, "html is the skeleton.");
        assert!(point.is_none());
    }

    #[test]
    fn leaves_plain_text_alone() {
        let (text, point) = split_point_tag("no pointing here");
        assert_eq!(text, "no pointing here");
        assert!(point.is_none());
    }

    #[test]
    fn streams_deltas_until_agent_end() {
        let mut session = AgentSession::new("coding", SessionKind::Coding, None, "gpt-5.5");
        assert!(session
            .handle_rx4_event(Rx4Event::TurnStart { turn: 1 })
            .is_none());
        session.handle_rx4_event(Rx4Event::MessageDelta {
            delta: "hel".into(),
        });
        session.handle_rx4_event(Rx4Event::MessageDelta { delta: "lo".into() });
        assert_eq!(session.streaming_content, "hello");
        assert!(session.busy);
        session.handle_rx4_event(Rx4Event::AgentEnd);
        assert!(!session.busy);
        assert_eq!(
            session.messages.last().map(|m| m.content.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn message_end_records_visible_text_and_point() {
        let mut session = AgentSession::new("cu", SessionKind::ComputerUse, None, "gpt-5.5");
        session.streaming_role = Some("assistant".into());
        let point = session.handle_rx4_event(Rx4Event::MessageEnd {
            content: "see the menu [POINT:10,20:menu]".into(),
            role: Role::Assistant,
        });
        let point = point.expect("point");
        assert_eq!(point.label, "menu");
        assert_eq!(
            session.messages.last().map(|m| m.content.as_str()),
            Some("see the menu")
        );
    }
}
