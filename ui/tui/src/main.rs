use parking_lot::Mutex as ParkingMutex;
use std::io::{stdin, stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crepuscularity_tui::ratatui::backend::CrosstermBackend;
use crepuscularity_tui::ratatui::text::Line;
use crepuscularity_tui::{Template, TemplateContext, TemplateValue};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use rx4::agent::{
    Agent, AgentBudget, Event as Rx4Event, ToolDefinition, ToolEffect, ToolResult, ToolSource,
};
use rx4::mode::Scope;
use rx4::permissions::Decision;
use rx4::provider::{OpenAIProvider, Provider, Role};
use rx4::subagent::{SubagentConfig, SubagentManager, SubagentStatus};
use rx4::{register_builtin_tools, register_spawn_agent_tool, ModelRegistry, ToolRegistry};

mod channel_approver;
mod markdown;
mod mcp_config;
mod product_policy;
use channel_approver::{ApprovalMode, ChannelApprover, PendingApproval};
#[cfg(feature = "pi-compat")]
mod pi;
#[cfg(feature = "pi-compat")]
use pi::{PiEntryType, PiSession};

const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

const MAX_HISTORY: usize = 100;

fn context_color(pct: usize) -> &'static str {
    if pct >= 90 {
        "red-400"
    } else if pct >= 70 {
        "amber-400"
    } else {
        "green-400"
    }
}

fn effort_color(effort: &str) -> &'static str {
    match effort {
        "low" => "green-400",
        "medium" => "blue-400",
        "high" => "amber-400",
        _ => "fuchsia-400",
    }
}

fn format_tokens(count: usize) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 10000 {
        format!("{:.1}k", count as f64 / 1000.0)
    } else if count < 1000000 {
        format!("{}k", count / 1000)
    } else {
        format!("{:.1}M", count as f64 / 1000000.0)
    }
}

fn project_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| "-".to_string())
}

fn history_path() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".telekinesis/input_history.json"))
        .unwrap_or_else(|| PathBuf::from(".telekinesis/input_history.json"))
}

fn load_history() -> Vec<String> {
    std::fs::read_to_string(history_path())
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn save_history(history: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let trimmed: Vec<&String> = history.iter().take(MAX_HISTORY).collect();
    let _ = std::fs::write(path, serde_json::to_string(&trimmed).unwrap_or_default());
}

fn spinner_frame(start: Instant) -> String {
    let elapsed = start.elapsed().as_millis();
    let idx = ((elapsed / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx].to_string()
}

fn blink_cursor(start: Instant) -> String {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "▏"
    } else {
        " "
    }
    .to_string()
}

fn load_template(path: Option<&std::ffi::OsStr>) -> anyhow::Result<Template> {
    match path {
        Some(path) => Template::from_path(PathBuf::from(path)).map_err(|e| anyhow::anyhow!("{e}")),
        None => Ok(Template::from_source(include_str!("../shell.crepus"))),
    }
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    content: String,
    is_tool: bool,
    tool_name: String,
    tool_call_id: String,
    is_streaming: bool,
}

#[derive(Clone)]
struct ConfiguredProvider {
    id: String,
    name: String,
    client: Arc<dyn Provider>,
}

#[derive(Clone)]
struct ModelChoice {
    id: String,
    provider: String,
}

struct App {
    input: String,
    messages: Vec<ChatMessage>,
    model: String,
    effort: String,
    model_choices: Vec<ModelChoice>,
    model_choice: Option<usize>,
    selecting_model: bool,
    providers: Vec<ConfiguredProvider>,
    provider_choice: usize,
    busy: bool,
    auto_scroll: bool,
    input_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    input_tokens: usize,
    output_tokens: usize,
    cost: f64,
    spinner_start: Instant,
    cursor_start: Instant,
    show_header: bool,
    permission_prompt: bool,
    permission_tool: String,
    /// Ones-shot reply channel while UI waits for y/n.
    permission_respond: Option<std::sync::mpsc::SyncSender<Decision>>,
    session_name: String,
    context_pct: usize,
    context_window: usize,
    agent: Option<Arc<Mutex<Agent>>>,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AppEvent>>,
    approval_rx: Option<std::sync::mpsc::Receiver<PendingApproval>>,
    approval_mode: Option<ApprovalMode>,
    prompt_char: String,
    /// Fully-qualified MCP tool names registered at startup (`mcp__server__tool`).
    mcp_tools: Vec<String>,
    subagent_manager: Option<Arc<ParkingMutex<SubagentManager>>>,
    #[cfg(feature = "pi-compat")]
    session: Option<(PiSession, PathBuf)>,
}

enum AppEvent {
    Rx4(Rx4Event),
    Error(String),
    Idle,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            messages: Vec::new(),
            model: "no-model".to_string(),
            effort: "high".to_string(),
            model_choices: Vec::new(),
            model_choice: None,
            selecting_model: false,
            providers: Vec::new(),
            provider_choice: 0,
            busy: false,
            auto_scroll: true,
            input_history: load_history(),
            history_index: None,
            history_draft: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            show_header: false,
            permission_prompt: false,
            permission_tool: String::new(),
            permission_respond: None,
            session_name: "default".to_string(),
            context_pct: 0,
            context_window: 128_000,
            agent: None,
            event_rx: None,
            approval_rx: None,
            approval_mode: None,
            prompt_char: ">".to_string(),
            mcp_tools: Vec::new(),
            subagent_manager: None,
            #[cfg(feature = "pi-compat")]
            session: None,
        }
    }

    #[cfg(feature = "pi-compat")]
    fn persist(&self) -> std::io::Result<()> {
        if let Some((session, dir)) = &self.session {
            session.save_jsonl(dir)?;
        }
        Ok(())
    }

    #[cfg(feature = "pi-compat")]
    fn append_session(&mut self, entry: PiEntryType) {
        if let Some((session, _)) = &mut self.session {
            session.append(entry);
            if let Err(error) = self.persist() {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Session save failed: {error}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
    }

    fn poll_pending_approvals(&mut self) {
        let Some(rx) = self.approval_rx.as_ref() else {
            return;
        };
        while let Ok(pending) = rx.try_recv() {
            self.permission_prompt = true;
            let detail = tool_detail(&pending.tool_name, &pending.arguments);
            self.permission_tool = if detail.is_empty() {
                pending.tool_name
            } else {
                format!("{} {detail}", pending.tool_name)
            };
            self.permission_respond = Some(pending.respond);
        }
    }

    fn resolve_permission(&mut self, allow: bool) {
        if let Some(tx) = self.permission_respond.take() {
            let _ = tx.send(if allow {
                Decision::Allow
            } else {
                Decision::Deny
            });
        }
        self.permission_prompt = false;
        self.permission_tool.clear();
    }

    fn toggle_permission_mode(&mut self) {
        let Some(mode) = &self.approval_mode else {
            return;
        };
        if mode.toggle() && self.permission_prompt {
            self.resolve_permission(true);
        }
    }

    fn update_template(&self, tpl: &mut Template) {
        tpl.set("input", self.input.clone());
        tpl.set("input_len", self.input.chars().count() as i64);
        tpl.set("model", self.model.clone());
        tpl.set("effort", self.effort.clone());
        tpl.set("input_color", effort_color(&self.effort));
        tpl.set("selecting_model", self.selecting_model);
        tpl.set(
            "selected_provider",
            self.providers
                .get(self.provider_choice)
                .map(|provider| provider.name.clone())
                .unwrap_or_default(),
        );
        tpl.set(
            "selected_model",
            self.model_choice
                .and_then(|index| {
                    self.filtered_models()
                        .get(index)
                        .map(|model| model.id.clone())
                })
                .unwrap_or_default(),
        );
        let filtered_models = self.filtered_models();
        let model_rows = filtered_models
            .into_iter()
            .enumerate()
            .skip(self.model_choice.unwrap_or_default().saturating_sub(2))
            .take(5)
            .map(|(index, model)| {
                let mut row = TemplateContext::new();
                row.set("model_id", model.id.clone());
                row.set("selected", Some(index) == self.model_choice);
                row
            })
            .collect();
        tpl.set("model_rows", TemplateValue::List(model_rows));
        tpl.set("busy", self.busy);
        tpl.set("auto_scroll", self.auto_scroll);
        tpl.set("version", env!("CARGO_PKG_VERSION"));
        tpl.set("session_name", self.session_name.clone());
        tpl.set("show_header", self.show_header);
        tpl.set("spinner", spinner_frame(self.spinner_start));
        tpl.set("cursor", blink_cursor(self.cursor_start));
        tpl.set("prompt_char", self.prompt_char.clone());
        tpl.set("permission_prompt", self.permission_prompt);
        tpl.set("permission_tool", self.permission_tool.clone());
        tpl.set(
            "permission_mode",
            self.approval_mode.as_ref().map_or("bypass", |mode| {
                if mode.is_bypass() {
                    "bypass"
                } else {
                    "ask"
                }
            }),
        );
        tpl.set("project", project_name());
        tpl.set("branch", git_branch());
        tpl.set("cost", format!("{:.3}", self.cost));
        tpl.set("context_pct", self.context_pct.to_string());
        tpl.set("context_window", format_tokens(self.context_window));
        tpl.set("context_color", context_color(self.context_pct));
        let running_subagents = self
            .subagent_manager
            .as_ref()
            .map(|manager| {
                let manager = manager.lock();
                manager
                    .list()
                    .iter()
                    .filter(|handle| {
                        matches!(
                            handle.status(),
                            SubagentStatus::Pending | SubagentStatus::Running
                        )
                    })
                    .map(|handle| {
                        let mut subagent = TemplateContext::new();
                        subagent.set("name", handle.name().to_string());
                        subagent
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        tpl.set("has_running_subagents", !running_subagents.is_empty());
        tpl.set("running_subagents", TemplateValue::List(running_subagents));

        let msgs: Vec<TemplateContext> = self
            .messages
            .iter()
            .map(|m| {
                let mut mc = TemplateContext::new();
                mc.set("is_user", m.role == "user");
                mc.set("is_tool", m.is_tool);
                mc.set("tool_name", m.tool_name.clone());
                mc.set("is_streaming", m.is_streaming);
                let lines: Vec<TemplateContext> = m
                    .content
                    .lines()
                    .map(|line| {
                        let mut lc = TemplateContext::new();
                        lc.set("text", line.to_string());
                        lc
                    })
                    .collect();
                mc.set("lines", TemplateValue::List(lines));
                mc
            })
            .collect();
        tpl.set("messages", TemplateValue::List(msgs));
    }

    fn submit_prompt(
        &mut self,
        agent: &Arc<Mutex<Agent>>,
        tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    ) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        self.input_history.insert(0, text.clone());
        save_history(&self.input_history);
        self.history_index = None;

        self.messages.push(ChatMessage {
            role: "user".to_string(),
            content: text.clone(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: false,
        });
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::Message {
            role: Role::User,
            content: text.clone(),
            tool_call_id: None,
        });

        self.input.clear();
        self.busy = true;

        let agent = agent.clone();
        tokio::spawn(async move {
            let mut agent = agent.lock().await;
            let result = agent.prompt(&text).await;
            if let Err(e) = result {
                let _ = tx.send(AppEvent::Error(e.to_string()));
            }
            let _ = tx.send(AppEvent::Idle);
        });
    }

    fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Rx4(e) => self.handle_rx4_event(e),
            AppEvent::Error(msg) => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            AppEvent::Idle => {
                self.busy = false;
            }
        }
    }

    fn handle_rx4_event(&mut self, event: Rx4Event) {
        match event {
            Rx4Event::AgentStart => {}
            Rx4Event::ContextUsage {
                used_tokens,
                context_window,
                ..
            } => {
                self.context_window = context_window;
                self.context_pct = used_tokens
                    .saturating_mul(100)
                    .checked_div(context_window)
                    .unwrap_or(0);
            }
            Rx4Event::Usage { usage, .. } => {
                self.input_tokens += usage.input_tokens;
                self.output_tokens += usage.output_tokens;
            }
            Rx4Event::CompactionStart { .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: String::new(),
                    is_tool: true,
                    tool_name: "compacting context".to_string(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::CompactionEnd { result, .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: format!("{} tokens remain", result.remaining_tokens),
                    is_tool: true,
                    tool_name: "compacted context".to_string(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Compaction {
                    summary: result.summary,
                    cut_at: result.removed_count,
                });
            }
            Rx4Event::SkillActivated { name, .. } => {
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: String::new(),
                    is_tool: true,
                    tool_name: format!("skill {name}"),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::ToolSource { tool, source } => {
                let activity = match source {
                    ToolSource::Builtin => None,
                    ToolSource::Mcp { server } => Some(format!("used {server} (MCP)")),
                    ToolSource::ComputerUse => Some(format!("used {tool}")),
                };
                if let Some(tool_name) = activity {
                    self.messages.push(ChatMessage {
                        role: "tool".to_string(),
                        content: String::new(),
                        is_tool: true,
                        tool_name,
                        tool_call_id: String::new(),
                        is_streaming: false,
                    });
                }
            }
            Rx4Event::TurnStart { .. } => {
                self.messages.push(ChatMessage {
                    role: "assistant".to_string(),
                    content: String::new(),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: true,
                });
            }
            Rx4Event::MessageStart { role } => {
                if role == Role::Assistant
                    && self
                        .messages
                        .last()
                        .is_none_or(|m| m.role != "assistant" || !m.content.is_empty())
                {
                    self.messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: String::new(),
                        is_tool: false,
                        tool_name: String::new(),
                        tool_call_id: String::new(),
                        is_streaming: true,
                    });
                }
            }
            Rx4Event::MessageDelta { delta } => {
                if let Some(msg) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == "assistant" && message.is_streaming)
                {
                    msg.content.push_str(&delta);
                }
            }
            Rx4Event::MessageEnd { content, .. } => {
                if let Some(msg) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find(|message| message.role == "assistant" && message.is_streaming)
                {
                    if !content.is_empty() {
                        msg.content = content.clone();
                    }
                    msg.is_streaming = false;
                }
                if !content.is_empty() {
                    #[cfg(feature = "pi-compat")]
                    self.append_session(PiEntryType::Message {
                        role: Role::Assistant,
                        content,
                        tool_call_id: None,
                    });
                }
            }
            Rx4Event::ToolCall(call) => {
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Custom {
                    extension: "telekinesis.tool_call".to_string(),
                    payload: serde_json::json!({
                        "id": &call.id,
                        "name": &call.name,
                        "arguments": &call.arguments,
                    }),
                });
                self.messages.push(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_detail(&call.name, &call.arguments),
                    is_tool: true,
                    tool_name: call.name,
                    tool_call_id: call.id,
                    is_streaming: true,
                });
            }
            Rx4Event::ApprovalRequired(_) => {}
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(msg) = self.messages.iter_mut().rev().find(|message| {
                    message.is_tool && message.is_streaming && message.tool_call_id == result.id
                }) {
                    let detail = std::mem::take(&mut msg.content);
                    let summary =
                        tool_result_summary(&msg.tool_name, &result.content, result.is_error);
                    msg.content = if detail.is_empty() {
                        summary
                    } else {
                        format!("{detail} → {summary}")
                    };
                    msg.role = if result.is_error { "error" } else { "tool" }.to_string();
                    msg.is_streaming = false;
                }
                #[cfg(feature = "pi-compat")]
                self.append_session(PiEntryType::Custom {
                    extension: "telekinesis.tool_result".to_string(),
                    payload: serde_json::json!({
                        "id": &result.id,
                        "content": &result.content,
                        "is_error": result.is_error,
                    }),
                });
            }
            Rx4Event::TurnEnd { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(msg) = self.messages.last_mut() {
                    msg.is_streaming = false;
                }
            }
            Rx4Event::Error(msg) => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Error: {msg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::BudgetExceeded { reason } => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Budget exceeded: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
    }

    fn history_get(&self) -> String {
        if let Some(idx) = self.history_index {
            self.input_history.get(idx).cloned().unwrap_or_default()
        } else {
            String::new()
        }
    }

    fn open_model_selector(&mut self) {
        let mut choices: Vec<ModelChoice> = ModelRegistry::load()
            .models()
            .filter(|model| {
                self.providers
                    .iter()
                    .any(|provider| provider.id == model.provider)
            })
            .map(|model| ModelChoice {
                id: model.id.clone(),
                provider: model.provider.clone(),
            })
            .collect();
        choices.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        if let Some(choice) = choices.iter().find(|choice| choice.id == self.model) {
            self.provider_choice = self
                .providers
                .iter()
                .position(|provider| provider.id == choice.provider)
                .unwrap_or(0);
        }
        self.model_choices = choices;
        self.input.clear();
        self.selecting_model = true;
        self.reset_model_choice();
    }

    fn filtered_models(&self) -> Vec<&ModelChoice> {
        let Some(provider) = self.providers.get(self.provider_choice) else {
            return Vec::new();
        };
        let query = self.input.to_ascii_lowercase();
        self.model_choices
            .iter()
            .filter(|model| {
                model.provider == provider.id && model.id.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    fn reset_model_choice(&mut self) {
        let choices = self.filtered_models();
        self.model_choice = choices
            .iter()
            .position(|model| model.id == self.model)
            .or((!choices.is_empty()).then_some(0));
    }

    fn move_provider_choice(&mut self, offset: isize) {
        if !self.selecting_model || self.providers.is_empty() {
            return;
        }
        let start = self.provider_choice;
        loop {
            self.provider_choice = (self.provider_choice as isize + offset)
                .rem_euclid(self.providers.len() as isize)
                as usize;
            self.reset_model_choice();
            if self.model_choice.is_some() || self.provider_choice == start {
                break;
            }
        }
    }

    fn move_model_choice(&mut self, offset: isize) {
        let Some(index) = self.model_choice else {
            return;
        };
        let len = self.filtered_models().len();
        if len != 0 {
            self.model_choice = Some((index as isize + offset).rem_euclid(len as isize) as usize);
        }
    }

    fn choose_model(&mut self) {
        let Some(index) = self.model_choice.take() else {
            return;
        };
        let Some(model) = self.filtered_models().get(index).cloned().cloned() else {
            return;
        };
        let Some(provider) = self.providers.get(self.provider_choice).cloned() else {
            return;
        };
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::ModelChange {
            from: self.model.clone(),
            to: model.id.clone(),
        });
        self.model = model.id.clone();
        self.selecting_model = false;
        self.input.clear();
        if let Some(agent) = &self.agent {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_provider(provider.client.clone());
                agent.set_model(model.id.clone());
            }
        }
        if let Some(manager) = &self.subagent_manager {
            let mut manager = manager.lock();
            manager.set_provider(provider.client);
            manager.set_model(model.id);
        }
    }

    fn cycle_effort(&mut self) {
        self.effort = match self.effort.as_str() {
            "low" => "medium",
            "medium" => "high",
            "high" => "xhigh",
            _ => "low",
        }
        .to_string();
        #[cfg(feature = "pi-compat")]
        self.append_session(PiEntryType::ThinkingLevelChange {
            level: self.effort.clone(),
        });
        if let Some(agent) = &self.agent {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_reasoning_effort(Some(self.effort.clone()));
            }
        }
    }

    fn take_scrollback(&mut self, width: usize) -> Vec<Line<'static>> {
        use crepuscularity_tui::ratatui::style::Color;

        let count = self
            .messages
            .iter()
            .take_while(|message| !message.is_streaming)
            .count();
        self.messages
            .drain(..count)
            .flat_map(|message| {
                if message.is_tool {
                    let color = if message.role == "error" {
                        Color::Red
                    } else {
                        tool_color(&message.tool_name)
                    };
                    let text = if message.content.is_empty() {
                        message.tool_name
                    } else {
                        format!("{} {}", message.tool_name, message.content)
                    };
                    wrap_scrollback_line("| ", &text, width, color)
                } else if message.role == "user" {
                    message
                        .content
                        .lines()
                        .enumerate()
                        .flat_map(|(index, line)| {
                            wrap_scrollback_line(
                                if index == 0 { "> " } else { "  " },
                                line,
                                width,
                                Color::Cyan,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    if message.role == "error" {
                        message
                            .content
                            .lines()
                            .flat_map(|line| wrap_scrollback_line("", line, width, Color::Red))
                            .collect()
                    } else {
                        markdown::render(&message.content, width)
                    }
                }
            })
            .collect()
    }
}

fn run_login(provider: Option<&str>) -> anyhow::Result<()> {
    let provider = provider.unwrap_or("grok");
    let oauth_provider = match provider {
        "grok" | "xai" => rs_ai_oauth::OAuthProvider::Xai,
        "openai" | "chatgpt" => rs_ai_oauth::OAuthProvider::ChatGpt,
        "claude" | "anthropic" => rs_ai_oauth::OAuthProvider::Claude,
        "gemini" | "google" => rs_ai_oauth::OAuthProvider::Gemini,
        "copilot" => rs_ai_oauth::OAuthProvider::Copilot,
        "kimi" => rs_ai_oauth::OAuthProvider::Kimi,
        "antigravity" => rs_ai_oauth::OAuthProvider::Antigravity,
        _ => {
            eprintln!("Unknown provider: {provider}");
            eprintln!("Available: grok, openai, claude, gemini, copilot, kimi, antigravity");
            std::process::exit(1);
        }
    };
    println!("Starting OAuth flow for {provider}...");
    let tokens = rs_ai_oauth::start_oauth_flow(oauth_provider)?;
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join(".telekinesis");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{provider}_token.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&tokens)?)?;
    println!("Token saved to {}", path.display());
    Ok(())
}

fn choose_provider() -> anyhow::Result<&'static str> {
    const PROVIDERS: [(&str, &str); 4] = [
        ("1", "grok"),
        ("2", "openai"),
        ("3", "claude"),
        ("4", "gemini"),
    ];
    println!("No saved login found. Which provider do you want?");
    for (number, provider) in PROVIDERS {
        println!("  {number}) {provider}");
    }
    loop {
        print!("Provider [1-4]: ");
        stdout().flush()?;
        let mut choice = String::new();
        if stdin().read_line(&mut choice)? == 0 {
            anyhow::bail!("Provider selection cancelled");
        }
        let choice = choice.trim().to_ascii_lowercase();
        if let Some((_, provider)) = PROVIDERS
            .iter()
            .find(|(number, provider)| choice == *number || choice == *provider)
        {
            return Ok(provider);
        }
        println!("Choose 1-4 or enter a provider name.");
    }
}

fn saved_token(provider: &str, rt: &tokio::runtime::Runtime) -> Option<String> {
    let path = dirs::home_dir()?
        .join(".telekinesis")
        .join(format!("{provider}_token.json"));
    let mut tokens =
        serde_json::from_str::<rs_ai_oauth::OAuthTokens>(&std::fs::read_to_string(&path).ok()?)
            .ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if tokens.expires_at <= now {
        let oauth_provider = match provider {
            "grok" => rs_ai_oauth::OAuthProvider::Xai,
            "openai" => rs_ai_oauth::OAuthProvider::ChatGpt,
            "claude" => rs_ai_oauth::OAuthProvider::Claude,
            "gemini" => rs_ai_oauth::OAuthProvider::Gemini,
            _ => return None,
        };
        tokens = rt
            .block_on(rs_ai_oauth::refresh_oauth_token(oauth_provider, &tokens))
            .ok()?;
        std::fs::write(&path, serde_json::to_string_pretty(&tokens).ok()?).ok()?;
    }
    (!tokens.access_token.is_empty()).then_some(tokens.access_token)
}

fn setup_providers(rt: &tokio::runtime::Runtime) -> Vec<(ConfiguredProvider, String)> {
    let providers = [
        (
            "XAI_API_KEY",
            "grok",
            "https://api.x.ai/v1",
            "xai",
            "xAI",
            "grok-4.5",
        ),
        (
            "OPENAI_API_KEY",
            "openai",
            "https://api.openai.com/v1",
            "openai",
            "OpenAI",
            "gpt-4o",
        ),
        (
            "ANTHROPIC_API_KEY",
            "claude",
            "https://api.anthropic.com/v1",
            "anthropic",
            "Anthropic",
            "claude-3-5-sonnet-20241022",
        ),
        (
            "GOOGLE_API_KEY",
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google Gemini",
            "gemini-2.0-flash",
        ),
    ];
    providers
        .iter()
        .filter_map(|(env, login, base_url, id, name, model)| {
            std::env::var(env)
                .ok()
                .filter(|key| !key.is_empty())
                .or_else(|| saved_token(login, rt))
                .map(|key| {
                    (
                        ConfiguredProvider {
                            id: (*id).to_string(),
                            name: (*name).to_string(),
                            client: Arc::new(OpenAIProvider::with_base_url(
                                *base_url, key, *id, *name,
                            )),
                        },
                        (*model).to_string(),
                    )
                })
        })
        .collect()
}

#[cfg(feature = "pi-compat")]
fn newest_session(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
        .max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .map(|entry| entry.path())
}

#[cfg(feature = "pi-compat")]
fn restored_chat(session: &PiSession) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = session
        .entries
        .iter()
        .filter_map(|entry| match &entry.entry_type {
            PiEntryType::Message { role, content, .. } => Some(ChatMessage {
                role: match role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                    Role::System => "system",
                }
                .to_string(),
                content: content.clone(),
                is_tool: *role == Role::Tool,
                tool_name: if *role == Role::Tool {
                    "tool".to_string()
                } else {
                    String::new()
                },
                tool_call_id: String::new(),
                is_streaming: false,
            }),
            PiEntryType::Custom { extension, payload } if extension == "telekinesis.tool_call" => {
                Some(ChatMessage {
                    role: "tool".to_string(),
                    content: tool_detail(
                        payload["name"].as_str().unwrap_or("tool"),
                        payload["arguments"].as_str().unwrap_or_default(),
                    ),
                    is_tool: true,
                    tool_name: payload["name"].as_str().unwrap_or("tool").to_string(),
                    tool_call_id: payload["id"].as_str().unwrap_or_default().to_string(),
                    is_streaming: false,
                })
            }
            PiEntryType::Compaction { summary, .. } => Some(ChatMessage {
                role: "tool".to_string(),
                content: summary.clone(),
                is_tool: true,
                tool_name: "compacted context".to_string(),
                tool_call_id: String::new(),
                is_streaming: false,
            }),
            _ => None,
        })
        .collect();
    for entry in &session.entries {
        let PiEntryType::Custom { extension, payload } = &entry.entry_type else {
            continue;
        };
        if extension != "telekinesis.tool_result" {
            continue;
        }
        let id = payload["id"].as_str().unwrap_or_default();
        if let Some(message) = messages
            .iter_mut()
            .rev()
            .find(|message| message.tool_call_id == id)
        {
            let detail = std::mem::take(&mut message.content);
            let summary = tool_result_summary(
                &message.tool_name,
                payload["content"].as_str().unwrap_or_default(),
                payload["is_error"].as_bool().unwrap_or(false),
            );
            message.content = if detail.is_empty() {
                summary
            } else {
                format!("{detail} → {summary}")
            };
        }
    }
    messages
}

fn run_tui(continue_session: bool) -> anyhow::Result<()> {
    let mut tpl = load_template(std::env::var_os("TELEKINESIS_TEMPLATE").as_deref())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let mut providers = setup_providers(&rt);
    if providers.is_empty() {
        run_login(Some(choose_provider()?))?;
        providers = setup_providers(&rt);
    }
    #[cfg(feature = "pi-compat")]
    let session_dir = pi::pi_sessions_dir(&std::env::current_dir()?);
    #[cfg(feature = "pi-compat")]
    let loaded_session = continue_session
        .then(|| newest_session(&session_dir))
        .flatten()
        .map(|path| PiSession::load_jsonl(&path))
        .transpose()?;
    #[cfg(feature = "pi-compat")]
    let resumed_model = loaded_session.as_ref().map(|session| {
        session
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.entry_type {
                PiEntryType::ModelChange { to, .. } => Some(to.clone()),
                _ => None,
            })
            .unwrap_or_else(|| session.header.model.clone())
    });
    #[cfg(feature = "pi-compat")]
    let resumed_effort = loaded_session
        .as_ref()
        .and_then(|session| {
            session
                .entries
                .iter()
                .rev()
                .find_map(|entry| match &entry.entry_type {
                    PiEntryType::ThinkingLevelChange { level } => Some(level.clone()),
                    _ => None,
                })
        })
        .unwrap_or_else(|| "high".to_string());
    #[cfg(not(feature = "pi-compat"))]
    let resumed_model: Option<String> = None;
    #[cfg(not(feature = "pi-compat"))]
    let resumed_effort = "high".to_string();
    let preferred_provider = resumed_model
        .as_deref()
        .and_then(|model| {
            ModelRegistry::load()
                .get(model)
                .map(|entry| entry.provider.clone())
        })
        .and_then(|provider| providers.iter().position(|entry| entry.0.id == provider))
        .unwrap_or(0);
    let (provider, model) = if let Some(selected) = providers.get(preferred_provider).cloned() {
        (selected.0.client, resumed_model.unwrap_or(selected.1))
    } else {
        anyhow::bail!("Login completed without a usable token");
    };

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();

    let mut agent = Agent::new();
    agent.set_scope(Scope::Coding);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    rx4::computer_use::register_tools(&mut tools);
    let mcp_tools = rt.block_on(connect_mcp_tools(&mut tools));
    let subagent_manager = Arc::new(ParkingMutex::new(
        SubagentManager::new()
            .with_provider(provider.clone())
            .with_model(model.clone()),
    ));
    register_spawn_agent_tool(&mut tools, Arc::clone(&subagent_manager));
    agent.set_tools(tools);
    subagent_manager.lock().set_tools(agent.tools.clone());
    agent.set_workspace_root(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    agent.load_project_context();
    agent.set_model(&model);
    agent.set_reasoning_effort(Some(resumed_effort.clone()));
    agent.set_provider(provider.clone());
    #[cfg(feature = "pi-compat")]
    if let Some(session) = &loaded_session {
        *agent.messages.write() = session.messages();
    }
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    // Policy.workspace_write enables OS sandbox flag; enable_os_sandbox installs runner.
    agent.set_policy(product_policy::tele_coding_policy());
    let (approver, approval_rx) = ChannelApprover::pair();
    let approval_mode = approver.mode();
    agent.set_approver(Arc::new(approver));
    let _ = agent.enable_os_sandbox();
    if let Some(home) = dirs::home_dir() {
        let mut engine = rx4::SkillEngine::new(home.join(".agents").join("skills"));
        if engine.load().is_ok() {
            let mut reg = rx4::SkillRegistry::new();
            for skill in engine.list() {
                reg.register(skill.clone());
            }
            agent.set_skill_registry(reg);
            agent.set_skill_engine(engine);
        }
    }
    agent.set_graph_memory(rx4::GraphMemory::new());
    agent.enable_auto_dream(true);

    let event_tx_clone = event_tx.clone();
    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx_clone.send(AppEvent::Rx4(event.clone()));
    });

    let agent = Arc::new(Mutex::new(agent));

    let mut app = App::new();
    app.model = model;
    app.effort = resumed_effort;
    #[cfg(feature = "pi-compat")]
    {
        app.messages = loaded_session
            .as_ref()
            .map(restored_chat)
            .unwrap_or_default();
        app.session = Some((
            loaded_session.unwrap_or_else(|| {
                PiSession::new(
                    std::env::current_dir()
                        .unwrap_or_else(|_| PathBuf::from("."))
                        .to_string_lossy(),
                    app.model.clone(),
                )
            }),
            session_dir,
        ));
        app.persist()?;
    }
    app.providers = providers
        .into_iter()
        .map(|(provider, _)| provider)
        .collect();
    app.agent = Some(agent.clone());
    app.event_rx = Some(event_rx);
    app.approval_rx = Some(approval_rx);
    app.approval_mode = Some(approval_mode);
    app.mcp_tools = mcp_tools;
    app.subagent_manager = Some(subagent_manager);

    let _rt_guard = rt.enter();

    enable_raw_mode()?;
    let mut stdout = stdout();
    stdout.flush()?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = crepuscularity_tui::ratatui::Terminal::with_options(
        backend,
        crepuscularity_tui::ratatui::TerminalOptions {
            viewport: crepuscularity_tui::ratatui::Viewport::Inline(9),
        },
    )?;

    loop {
        let mut pending = Vec::new();
        if let Some(rx) = app.event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                pending.push(event);
            }
        }
        for event in pending {
            app.handle_event(event);
        }
        app.poll_pending_approvals();

        let width = terminal.size()?.width;
        let scrollback = app.take_scrollback(width as usize);
        if !scrollback.is_empty() {
            terminal.insert_before(scrollback.len() as u16, |buffer| {
                for (index, line) in scrollback.iter().enumerate() {
                    buffer.set_line(0, index as u16, line, width);
                }
            })?;
        }

        app.update_template(&mut tpl);
        if !tpl.changed_keys().is_empty() {
            terminal.draw(|f| {
                if let Err(e) = tpl.draw(f, f.area()) {
                    use crepuscularity_tui::ratatui::style::Style;
                    use crepuscularity_tui::ratatui::widgets::Paragraph;
                    let p = Paragraph::new(format!("Template error: {e}"))
                        .style(Style::default().fg(crepuscularity_tui::ratatui::style::Color::Red));
                    f.render_widget(p, f.area());
                }
            })?;
            tpl.mark_rendered();
        }

        if crossterm::event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if is_permission_toggle(key.code, key.modifiers) {
                    app.toggle_permission_mode();
                    continue;
                }
                if app.permission_prompt {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.resolve_permission(true);
                            continue;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.resolve_permission(false);
                            continue;
                        }
                        _ => continue,
                    }
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Enter, _) => {
                        if app.selecting_model {
                            app.choose_model();
                            continue;
                        }
                        if app.busy {
                            continue;
                        }
                        let text = app.input.trim().to_string();
                        if text == "/quit" || text == "/exit" {
                            break;
                        }
                        if text.starts_with('/') {
                            handle_slash_command(&mut app, &text, &agent, &event_tx);
                        } else if !text.is_empty() {
                            app.submit_prompt(&agent, event_tx.clone());
                        }
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if app.busy {
                            app.busy = false;
                        } else {
                            break;
                        }
                    }
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        if app.input.is_empty() {
                            break;
                        }
                    }
                    (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                        let _ = terminal.clear();
                    }
                    (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                        app.show_header = !app.show_header;
                    }
                    (KeyCode::F(1), _) => {
                        handle_slash_command(&mut app, "/help", &agent, &event_tx);
                    }
                    (KeyCode::BackTab, _) => {
                        app.cycle_effort();
                    }
                    (KeyCode::Esc, _) if app.selecting_model => {
                        app.model_choice = None;
                        app.selecting_model = false;
                        app.input.clear();
                    }
                    (KeyCode::Left, _) if app.selecting_model => {
                        app.move_provider_choice(-1);
                    }
                    (KeyCode::Right, _) if app.selecting_model => {
                        app.move_provider_choice(1);
                    }
                    (KeyCode::Up, _) => {
                        if app.selecting_model {
                            app.move_model_choice(-1);
                            continue;
                        }
                        if app.history_index.is_none() && !app.input_history.is_empty() {
                            app.history_draft = app.input.clone();
                            app.history_index = Some(0);
                            app.input = app.history_get();
                        } else if let Some(idx) = app.history_index {
                            if idx + 1 < app.input_history.len() {
                                app.history_index = Some(idx + 1);
                                app.input = app.history_get();
                            }
                        }
                    }
                    (KeyCode::Down, _) => {
                        if app.selecting_model {
                            app.move_model_choice(1);
                            continue;
                        }
                        if let Some(idx) = app.history_index {
                            if idx == 0 {
                                app.history_index = None;
                                app.input = app.history_draft.clone();
                            } else {
                                app.history_index = Some(idx - 1);
                                app.input = app.history_get();
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        app.input.pop();
                        if app.selecting_model {
                            app.reset_model_choice();
                        }
                    }
                    (KeyCode::PageUp, _) => {
                        app.auto_scroll = false;
                    }
                    (KeyCode::PageDown, _) => {
                        app.auto_scroll = true;
                    }
                    (KeyCode::Home, _) => {
                        app.auto_scroll = false;
                    }
                    (KeyCode::End, _) => {
                        app.auto_scroll = true;
                    }
                    (KeyCode::Char(c), _) => {
                        app.input.push(c);
                        if app.selecting_model {
                            app.reset_model_choice();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    terminal.backend_mut().flush()?;
    Ok(())
}

fn truncate_args(args: &str, max: usize) -> String {
    let flat = args.replace('\n', " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn tool_detail(name: &str, arguments: &str) -> String {
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return truncate_args(arguments, 120);
    };
    let key = match name {
        "bash" => "command",
        "grep" | "find" => "pattern",
        _ => "path",
    };
    arguments
        .get(key)
        .or_else(|| arguments.get("name"))
        .and_then(|value| value.as_str())
        .map(|value| truncate_args(value, 120))
        .unwrap_or_default()
}

fn tool_result_summary(name: &str, content: &str, is_error: bool) -> String {
    if is_error {
        return content
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| truncate_args(line, 120))
            .unwrap_or_else(|| "error".to_string());
    }
    let count = content.lines().filter(|line| !line.is_empty()).count();
    match name {
        "read" => format!("{count} lines"),
        "grep" => format!("{count} matches"),
        "find" => format!("{count} files"),
        "ls" => format!("{count} entries"),
        "write" => "written".to_string(),
        "edit" => "applied".to_string(),
        "bash" => content
            .lines()
            .rev()
            .find_map(|line| {
                line.trim()
                    .strip_prefix("(exit code: ")
                    .and_then(|code| code.strip_suffix(')'))
                    .map(|code| format!("failed · exit {code}"))
            })
            .or_else(|| {
                content
                    .lines()
                    .find(|line| !line.trim().is_empty() && *line != "(no output)")
                    .map(|line| truncate_args(line.trim(), 120))
            })
            .unwrap_or_else(|| "done".to_string()),
        _ if count == 0 => "done".to_string(),
        _ => format!("{count} results"),
    }
}

fn is_permission_toggle(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::Char('~')
        || code == KeyCode::Char('`') && modifiers.contains(KeyModifiers::SHIFT)
}

fn tool_color(name: &str) -> crepuscularity_tui::ratatui::style::Color {
    use crepuscularity_tui::ratatui::style::Color;
    match name {
        "read" | "grep" | "find" | "ls" => Color::Cyan,
        "write" | "edit" => Color::Yellow,
        "bash" => Color::Magenta,
        _ => Color::Blue,
    }
}

fn wrap_scrollback_line(
    prefix: &str,
    text: &str,
    width: usize,
    color: crepuscularity_tui::ratatui::style::Color,
) -> Vec<Line<'static>> {
    use crepuscularity_tui::ratatui::style::Style;

    let prefix_width = prefix.chars().count();
    let content_width = width.saturating_sub(prefix_width).max(1);
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![Line::styled(prefix.to_string(), Style::default().fg(color))];
    }
    let mut lines = Vec::new();
    let mut remaining = chars.as_slice();
    while !remaining.is_empty() {
        let split = if remaining.len() <= content_width {
            remaining.len()
        } else {
            remaining[..content_width]
                .iter()
                .rposition(|ch| ch.is_whitespace())
                .filter(|index| *index > 0)
                .unwrap_or(content_width)
        };
        let chunk = remaining[..split].iter().collect::<String>();
        remaining = &remaining[split..];
        remaining = &remaining[remaining.iter().take_while(|ch| ch.is_whitespace()).count()..];
        let indent = if lines.is_empty() {
            prefix.to_string()
        } else {
            " ".repeat(prefix_width)
        };
        lines.push(Line::styled(
            format!("{indent}{chunk}"),
            Style::default().fg(color),
        ));
    }
    lines
}

/// Best-effort MCP connect from ~/.telekinesis/mcp.json. Never fails TUI startup.
async fn connect_mcp_tools(tools: &mut ToolRegistry) -> Vec<String> {
    let configs = mcp_config::load();
    if configs.is_empty() {
        return Vec::new();
    }

    let mut names = Vec::new();
    for cfg in configs {
        let transport = match cfg.transport.to_ascii_lowercase().as_str() {
            "http" => rx4::McpTransportKind::Http,
            "sse" => rx4::McpTransportKind::Sse,
            _ => rx4::McpTransportKind::Stdio,
        };
        let engine_cfg = rx4::McpServerConfig {
            name: cfg.name.clone(),
            command: cfg.command.clone().unwrap_or_default(),
            args: cfg.args.clone(),
            env: Default::default(),
            transport,
            url: cfg.url.clone(),
            headers: cfg.headers.clone(),
        };
        match rx4::McpClient::connect_config(&engine_cfg).await {
            Ok(client) => match client.list_tools().await {
                Ok(listed) => {
                    let client = Arc::new(client);
                    for tool in listed {
                        let full = format!("mcp__{}__{}", cfg.name, tool.name);
                        let desc = if tool.description.is_empty() {
                            format!("MCP tool {} from {}", tool.name, cfg.name)
                        } else {
                            tool.description.clone()
                        };
                        let params = tool.input_schema.to_string();
                        let client_c = client.clone();
                        let remote_name = tool.name.clone();
                        tools.register(
                            ToolDefinition::new_boxed(
                                full.clone(),
                                desc,
                                params,
                                Box::new(move |_ctx, args| {
                                    let client = client_c.clone();
                                    let remote_name = remote_name.clone();
                                    Box::pin(async move {
                                        let value: serde_json::Value = serde_json::from_str(&args)
                                            .unwrap_or_else(|_| serde_json::json!({ "raw": args }));
                                        match client.call_tool(&remote_name, &value).await {
                                            Ok(v) => {
                                                ToolResult::ok(remote_name.clone(), v.to_string())
                                            }
                                            Err(e) => {
                                                ToolResult::err(remote_name.clone(), e.to_string())
                                            }
                                        }
                                    })
                                }),
                            )
                            .with_effect(ToolEffect::Network),
                        );
                        names.push(full);
                    }
                }
                Err(e) => {
                    eprintln!("telekinesis: MCP list_tools failed for `{}`: {e}", cfg.name);
                }
            },
            Err(e) => {
                eprintln!("telekinesis: MCP connect failed for `{}`: {e}", cfg.name);
            }
        }
    }
    names
}

fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    _agent: &Arc<Mutex<Agent>>,
    _tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");
    app.input.clear();

    match command {
        "/quit" | "/exit" => {}
        "/clear" => {
            app.messages.clear();
            app.input_tokens = 0;
            app.output_tokens = 0;
            app.cost = 0.0;
        }
        "/help" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: "Commands: /model [name], /scope <coding|research|plan|ask|computer_use>, /subagent spawn|list|cancel, /budget <max-cost>, /mcp, /todo, /clear, /cost, /help, /quit\nKeys: model selector: type search, Left/Right provider, Up/Down model, Enter apply, Esc cancel · Shift+Tab effort · Ctrl+B header · Ctrl+L clear · Ctrl+C interrupt".to_string(),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/model" => {
            if arg.is_empty() {
                app.open_model_selector();
            } else {
                #[cfg(feature = "pi-compat")]
                app.append_session(PiEntryType::ModelChange {
                    from: app.model.clone(),
                    to: arg.to_string(),
                });
                app.model = arg.to_string();
                if let Some(a) = &app.agent {
                    if let Ok(mut agent) = a.try_lock() {
                        agent.set_model(arg);
                    }
                }
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Model set to: {arg}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
        "/cost" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!(
                    "Input: {} tokens, Output: {} tokens, Cost: ${:.4}",
                    app.input_tokens, app.output_tokens, app.cost
                ),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/scope" => {
            if let Some(a) = &app.agent {
                if let Ok(mut agent) = a.try_lock() {
                    let scope = match arg {
                        "coding" => Scope::Coding,
                        "research" => Scope::Research,
                        "plan" => Scope::Plan,
                        "ask" => Scope::Ask,
                        "computer_use" | "computer-use" | "cu" => Scope::ComputerUse,
                        _ => Scope::Coding,
                    };
                    agent.set_scope(scope);
                }
            }
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Scope set to: {arg}"),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/mcp" => {
            let path = mcp_config::config_path();
            let body = if app.mcp_tools.is_empty() {
                format!(
                    "No MCP tools connected.\nConfig: {}\nFormat: {{\"servers\":[{{\"name\":\"fs\",\"transport\":\"stdio\",\"command\":\"npx\",\"args\":[\"-y\",\"@modelcontextprotocol/server-filesystem\",\".\"]}}]}}\nRemote HTTP/SSE: put url+transport=http|sse in config (host loader documents it; engine stdio works today).",
                    path.display()
                )
            } else {
                format!(
                    "MCP tools ({}):\n{}\nConfig: {}",
                    app.mcp_tools.len(),
                    app.mcp_tools.join("\n"),
                    path.display()
                )
            };
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: body,
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/todo" => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: "/todo: host surface only. Engine may expose todo tool later — track work in chat or project TODO for now.".to_string(),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/budget" => {
            if arg.is_empty() {
                let msg = if let Some(a) = &app.agent {
                    if let Ok(agent) = a.try_lock() {
                        match &agent.budget {
                            Some(b) => format!(
                                "Budget: max_cost=${:?}, max_duration={:?}s",
                                b.max_cost, b.max_duration_seconds
                            ),
                            None => "No budget set.".to_string(),
                        }
                    } else {
                        "Agent busy.".to_string()
                    }
                } else {
                    "No agent.".to_string()
                };
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: msg,
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            } else if let Ok(cost) = arg.parse::<f64>() {
                if let Some(a) = &app.agent {
                    if let Ok(mut agent) = a.try_lock() {
                        agent.budget = Some(AgentBudget {
                            max_cost: Some(cost),
                            ..AgentBudget::default()
                        });
                    }
                }
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Budget max_cost set to ${cost:.4}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            } else {
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("Invalid budget: {arg}. Use /budget <max-cost>."),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
        }
        "/subagent" => {
            let sub_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let sub = sub_parts.first().copied().unwrap_or("");
            let rest = sub_parts.get(1).copied().unwrap_or("");
            match sub {
                "spawn" => {
                    if let Some(mgr) = app.subagent_manager.clone() {
                        let prompt = rest.to_string();
                        let name = prompt
                            .split_whitespace()
                            .next()
                            .unwrap_or("subagent")
                            .to_string();
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: format!("Spawning subagent '{name}'..."),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                        let workspace =
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let result = mgr.lock().spawn_background(
                            SubagentConfig {
                                name: name.clone(),
                                workspace_isolation: true,
                                ..SubagentConfig::default()
                            },
                            &prompt,
                            &workspace,
                        );
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: match result {
                                Ok(handle) => {
                                    format!("Subagent {name} running — id: {}", handle.id())
                                }
                                Err(error) => format!("Subagent error: {error}"),
                            },
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    } else {
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: "Subagent manager not initialized.".to_string(),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                "list" => {
                    if let Some(mgr) = app.subagent_manager.as_ref() {
                        let mgr = mgr.lock();
                        let handles = mgr.list();
                        let body = if handles.is_empty() {
                            "No subagents.".to_string()
                        } else {
                            handles
                                .iter()
                                .map(|h| {
                                    format!(
                                        "{}: {} [{:?}] depth={} children={} descendants={}",
                                        h.id(),
                                        h.name(),
                                        h.status(),
                                        h.depth(),
                                        h.children().len(),
                                        h.descendant_count()
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        };
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: body,
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                "cancel" => {
                    if rest.is_empty() {
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: "Usage: /subagent cancel <id>".to_string(),
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    } else if let Some(mgr) = app.subagent_manager.as_ref() {
                        let body = match mgr.lock().cancel(rest) {
                            Ok(()) => format!("Cancelled subagent {rest}."),
                            Err(e) => format!("Cancel failed: {e}"),
                        };
                        app.messages.push(ChatMessage {
                            role: "system".to_string(),
                            content: body,
                            is_tool: false,
                            tool_name: String::new(),
                            tool_call_id: String::new(),
                            is_streaming: false,
                        });
                    }
                }
                _ => {
                    app.messages.push(ChatMessage {
                        role: "system".to_string(),
                        content: "Usage: /subagent spawn <prompt> | list | cancel <id>".to_string(),
                        is_tool: false,
                        tool_name: String::new(),
                        tool_call_id: String::new(),
                        is_streaming: false,
                    });
                }
            }
        }
        _ => {
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Unknown command: {command}. Type /help for available commands."),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "login" {
        return run_login(args.get(2).map(|s| s.as_str()));
    }
    if args.len() >= 2 && (args[1] == "--help" || args[1] == "-h") {
        println!("telekinesis (tk) — AI coding agent TUI");
        println!();
        println!("USAGE:");
        println!("  tk              Start interactive TUI");
        println!("  tk -c           Continue newest session for this project");
        println!(
            "  tk login <provider>  OAuth login (grok, openai, claude, gemini, copilot, kimi)"
        );
        println!("  tk --help       Show this help");
        println!();
        println!("ENVIRONMENT:");
        println!("  XAI_API_KEY         xAI Grok API key");
        println!("  OPENAI_API_KEY      OpenAI API key");
        println!("  ANTHROPIC_API_KEY   Anthropic Claude API key");
        println!("  GOOGLE_API_KEY      Google Gemini API key");
        println!();
        println!("KEYS:");
        println!("  Enter        Submit prompt");
        println!("  Ctrl+C       Interrupt / exit");
        println!("  Ctrl+L       Clear screen");
        println!("  Ctrl+B       Toggle header");
        println!("  F1           Show help");
        println!("  Shift+Tab    Cycle reasoning effort");
        println!("  Up/Down      Input history");
        println!("  PgUp/PgDn    Scroll chat view");
        println!("  Home/End     Jump to top/bottom of chat");
        return Ok(());
    }

    let continue_session = args.iter().skip(1).any(|arg| is_continue_arg(arg));
    run_tui(continue_session)
}

fn is_continue_arg(arg: &str) -> bool {
    arg == "-c" || arg == "--continue"
}

#[cfg(test)]
mod tests {
    use super::{
        is_continue_arg, is_permission_toggle, load_template, tool_result_summary, App,
        ChatMessage, ConfiguredProvider,
    };
    #[cfg(feature = "pi-compat")]
    use super::{restored_chat, PiEntryType, PiSession};
    use crossterm::event::{KeyCode, KeyModifiers};
    use rx4::provider::OpenAIProvider;
    use std::sync::Arc;

    fn provider(id: &str) -> ConfiguredProvider {
        ConfiguredProvider {
            id: id.to_string(),
            name: id.to_string(),
            client: Arc::new(OpenAIProvider::with_base_url(
                "http://localhost",
                "test",
                id,
                id,
            )),
        }
    }

    #[test]
    fn continue_accepts_short_and_long_flags() {
        assert!(is_continue_arg("-c"));
        assert!(is_continue_arg("--continue"));
        assert!(!is_continue_arg("-C"));
    }

    #[cfg(feature = "pi-compat")]
    #[test]
    fn continued_session_restores_transcript_and_tool_summary() {
        let mut session = PiSession::new("/project", "grok-4.5");
        session.append_message(rx4::provider::Role::User, "inspect");
        session.append(PiEntryType::Custom {
            extension: "telekinesis.tool_call".to_string(),
            payload: serde_json::json!({
                "id": "call-1",
                "name": "bash",
                "arguments": "{\"command\":\"pwd\"}",
            }),
        });
        session.append(PiEntryType::Custom {
            extension: "telekinesis.tool_result".to_string(),
            payload: serde_json::json!({
                "id": "call-1",
                "content": "/project",
                "is_error": false,
            }),
        });
        session.append_message(rx4::provider::Role::Assistant, "done");

        let messages = restored_chat(&session);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].content, "inspect");
        assert_eq!(messages[1].content, "pwd → /project");
        assert_eq!(messages[2].content, "done");
    }

    #[test]
    fn embedded_template_ignores_stale_home_template() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".telekinesis")).unwrap();
        std::fs::write(
            home.path().join(".telekinesis/shell.crepus"),
            "stale template",
        )
        .unwrap();
        let template = load_template(None).unwrap();
        assert!(template.source().contains("Telekinesis v{version}"));
        assert!(!template.source().contains("stale template"));
    }

    #[test]
    fn explicit_template_override_is_available_for_development() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell.crepus");
        std::fs::write(&path, "div\n  \"override\"").unwrap();
        assert!(load_template(Some(path.as_os_str())).is_ok());
    }

    #[test]
    fn completed_activity_moves_to_terminal_scrollback() {
        let mut app = App::new();
        app.handle_rx4_event(rx4::agent::Event::ToolCall(rx4::agent::ToolCall {
            id: "read-1".to_string(),
            name: "read".to_string(),
            arguments: r#"{"path":"AGENTS.md"}"#.to_string(),
        }));
        assert!(app.take_scrollback(80).is_empty());
        app.handle_rx4_event(rx4::agent::Event::ToolExecutionEnd(
            rx4::agent::ToolResult {
                id: "read-1".to_string(),
                content: "one\ntwo".to_string(),
                is_error: false,
            },
        ));
        assert_eq!(
            app.take_scrollback(80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["| read AGENTS.md → 2 lines"]
        );
        assert!(app.messages.is_empty());

        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "review this repo".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: false,
        });
        assert_eq!(
            app.take_scrollback(80)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["> review this repo"]
        );
    }

    #[test]
    fn parallel_tool_results_update_the_matching_activity() {
        let mut app = App::new();
        for (id, name, arguments) in [
            ("read-1", "read", r#"{"path":"AGENTS.md"}"#),
            ("bash-1", "bash", r#"{"command":"pwd"}"#),
        ] {
            app.handle_rx4_event(rx4::agent::Event::ToolCall(rx4::agent::ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            }));
        }
        app.handle_rx4_event(rx4::agent::Event::ToolExecutionEnd(
            rx4::agent::ToolResult {
                id: "read-1".to_string(),
                content: "one\ntwo".to_string(),
                is_error: false,
            },
        ));

        assert!(!app.messages[0].is_streaming);
        assert!(app.messages[0].content.ends_with("2 lines"));
        assert!(app.messages[1].is_streaming);
        assert_eq!(app.messages[1].tool_call_id, "bash-1");
    }

    #[test]
    fn scrollback_wraps_unicode_without_splitting_characters() {
        let mut app = App::new();
        app.messages.push(ChatMessage {
            role: "user".to_string(),
            content: "review — then fix".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: false,
        });
        assert_eq!(
            app.take_scrollback(10)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>(),
            ["> review", "  — then", "  fix"]
        );
    }

    #[test]
    fn permission_shortcut_and_bash_failures_are_tidy() {
        assert!(is_permission_toggle(
            KeyCode::Char('~'),
            KeyModifiers::SHIFT
        ));
        assert!(is_permission_toggle(
            KeyCode::Char('`'),
            KeyModifiers::SHIFT
        ));
        assert!(!is_permission_toggle(
            KeyCode::Char('`'),
            KeyModifiers::NONE
        ));
        assert_eq!(
            tool_result_summary("bash", "permission denied\n(exit code: -1)", false),
            "failed · exit -1"
        );
        assert_eq!(tool_result_summary("bash", "hello\n", false), "hello");
    }

    #[test]
    fn model_selector_and_effort_cycle_update_state() {
        let mut app = App::new();
        app.providers = vec![provider("openai"), provider("anthropic")];
        app.open_model_selector();
        assert!(app.model_choice.is_some());
        assert!(!app.model_choices.is_empty());
        assert!(app
            .filtered_models()
            .iter()
            .all(|model| model.provider == "openai"));
        app.input = "claude".to_string();
        app.reset_model_choice();
        assert!(app.model_choice.is_none());
        app.move_provider_choice(1);
        assert!(app
            .filtered_models()
            .iter()
            .all(|model| model.provider == "anthropic" && model.id.contains("claude")));
        app.move_model_choice(1);
        app.choose_model();
        assert!(app.model_choice.is_none());
        assert!(app.model.contains("claude"));

        app.cycle_effort();
        assert_eq!(app.effort, "xhigh");
        app.cycle_effort();
        assert_eq!(app.effort, "low");
    }

    #[test]
    fn embedded_template_renders_compact_header_and_prompt() {
        use crepuscularity_tui::ratatui::backend::TestBackend;
        use crepuscularity_tui::ratatui::Terminal;

        let mut template = load_template(None).unwrap();
        App::new().update_template(&mut template);
        let mut terminal = Terminal::new(TestBackend::new(200, 9)).unwrap();
        terminal
            .draw(|frame| template.draw(frame, frame.area()).unwrap())
            .unwrap();
        let output =
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .fold(String::new(), |mut output, cell| {
                    output.push_str(cell.symbol());
                    output
                });
        assert!(output.contains("$0.000"));
        assert!(output.contains("no-model · high"));
        assert!(output.contains("> "));
    }
}
