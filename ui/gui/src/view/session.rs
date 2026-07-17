use std::sync::Arc;

use crepuscularity_gpui::prelude::*;
use rx4::agent::{Agent, Event as Rx4Event};
use rx4::provider::Role;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct MessageItem {
    pub role: SharedString,
    pub content: SharedString,
    pub is_tool: bool,
    pub is_user: bool,
    pub is_error: bool,
}

impl MessageItem {
    pub fn new(role: &str, content: impl Into<SharedString>) -> Self {
        let is_tool = role.starts_with("tool:");
        let is_user = role == "user";
        let is_error = role == "error";
        Self {
            role: role.to_string().into(),
            content: content.into(),
            is_tool,
            is_user,
            is_error,
        }
    }
}

pub enum CompanionEvent {
    /// Session index this event belongs to
    Session(usize, Rx4Event),
    SessionError(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SessionKind {
    ComputerUse,
    Coding,
}

/// A single agent session — one agent with its own message history.
pub struct AgentSession {
    #[allow(dead_code)]
    name: SharedString,
    #[allow(dead_code)]
    kind: SessionKind,
    pub agent: Option<Arc<Mutex<Agent>>>,
    pub messages: Vec<MessageItem>,
    pub streaming_role: Option<String>,
    pub streaming_content: String,
    pub busy: bool,
    pub model: SharedString,
}

impl AgentSession {
    pub fn new(name: &str, kind: SessionKind, agent: Option<Arc<Mutex<Agent>>>, model: &str) -> Self {
        Self {
            name: name.to_string().into(),
            kind,
            agent,
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            busy: false,
            model: model.to_string().into(),
        }
    }

    #[allow(dead_code)]
    fn role_color(role: &str) -> u32 {
        match role {
            "user" => 0x818cf8,
            "assistant" => 0x34d399,
            "system" => 0xfbbf24,
            "error" => 0xf87171,
            _ if role.starts_with("tool:") => 0x38bdf8,
            _ => 0xa1a1aa,
        }
    }

    pub fn handle_rx4_event(&mut self, event: Rx4Event) {
        match event {
            Rx4Event::AgentStart => {}
            Rx4Event::TurnStart { .. } => {
                self.streaming_role = Some("assistant".to_string());
                self.streaming_content.clear();
                self.busy = true;
            }
            Rx4Event::MessageStart { role } => {
                if role == Role::Assistant
                    && self
                        .messages
                        .last()
                        .is_none_or(|m| m.role.as_ref() != "assistant" || !m.content.is_empty())
                {
                    self.streaming_role = Some("assistant".to_string());
                    self.streaming_content.clear();
                }
            }
            Rx4Event::MessageDelta { delta } => {
                self.streaming_content.push_str(&delta);
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
                self.messages.push(MessageItem::new(&role, raw));
                self.streaming_content.clear();
            }
            Rx4Event::ToolCall(call) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, std::mem::take(&mut self.streaming_content)));
                }
                let tool_role = format!("tool:{}", call.name);
                self.streaming_role = Some(tool_role.clone());
                self.streaming_content.clear();
                self.busy = true;
            }
            Rx4Event::ApprovalRequired(req) => {
                self.messages.push(MessageItem::new("system", format!("Approval required: {} ({})", req.tool_name, req.reason)));
            }
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, result.content));
                }
                self.streaming_content.clear();
            }
            Rx4Event::TurnEnd { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, std::mem::take(&mut self.streaming_content)));
                }
                self.busy = false;
            }
            Rx4Event::Error(msg) => {
                self.messages.push(MessageItem::new("error", format!("Error: {msg}")));
            }
        }
    }
}
