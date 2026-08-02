use parking_lot::Mutex as ParkingMutex;
use std::io::{stdin, stdout, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use darash::{SearchClient, SearchQuery, SearchResponse};

use crepuscularity_tui::ratatui::backend::CrosstermBackend;
use crepuscularity_tui::ratatui::text::Line;
use crepuscularity_tui::{Template, TemplateContext, TemplateValue};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use rx4::agent::{
    Agent, AgentBudget, AgentError, CancellationHandle, Event as Rx4Event, ToolContext,
    ToolDefinition, ToolEffect, ToolFuture, ToolResult, ToolSource,
};
use rx4::mode::Scope;
use rx4::permissions::Decision;
use rx4::provider::{OpenAIProvider, Provider, Role};
use rx4::subagent::{SubagentConfig, SubagentManager, SubagentStatus};
use rx4::{register_builtin_tools, register_spawn_agent_tool, ModelRegistry, ToolRegistry};

mod channel_approver;
mod codex_provider;
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
const GPT_5_CONTEXT_WINDOW: usize = 1_050_000;
const GPT_5_6_MODELS: [&str; 4] = ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

fn context_window_for_model(model: &str) -> usize {
    if model.starts_with("gpt-5.5") || model.starts_with("gpt-5.6") {
        GPT_5_CONTEXT_WINDOW
    } else {
        ModelRegistry::load()
            .get(model)
            .map(|model| model.context_window)
            .unwrap_or(128_000)
    }
}
const LARGE_PASTE_LINES: usize = 10;
const LARGE_PASTE_CHARS: usize = 1000;

const SLASH_COMMANDS: [&str; 15] = [
    "/login",
    "/config",
    "/model",
    "/scope",
    "/plan",
    "/review",
    "/subagent",
    "/budget",
    "/mcp",
    "/todo",
    "/clear",
    "/cost",
    "/help",
    "/quit",
    "/exit",
];

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

fn spinner_frame(start: Instant) -> &'static str {
    let elapsed = start.elapsed().as_millis();
    let idx = ((elapsed / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx]
}

fn file_query(input: &str) -> Option<&str> {
    let token = input
        .rsplit_once(char::is_whitespace)
        .map_or(input, |(_, token)| token);
    token.strip_prefix('@')
}

fn matching_slash_commands(input: &str) -> Vec<String> {
    if input.starts_with('/') && !input.contains(char::is_whitespace) {
        SLASH_COMMANDS
            .iter()
            .filter(|command| command.starts_with(input))
            .map(|command| (*command).to_string())
            .collect()
    } else {
        Vec::new()
    }
}

fn search_files(query: &str, limit: usize) -> Vec<String> {
    let Ok(output) = std::process::Command::new("git")
        .args(["ls-files", "-co", "--exclude-standard", "-z"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let query = query.to_ascii_lowercase();
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| path.to_ascii_lowercase().contains(&query))
        .take(limit)
        .map(str::to_string)
        .collect()
}

fn blink_cursor(start: Instant) -> &'static str {
    if (start.elapsed().as_millis() / 500).is_multiple_of(2) {
        "▏"
    } else {
        " "
    }
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
    pastes: Vec<String>,
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
    file_suggestions: Vec<String>,
    file_choice: usize,
    pending_file_query: Option<String>,
    slash_suggestions: Vec<String>,
    slash_choice: usize,
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
    context_tokens: usize,
    context_window: usize,
    agent: Option<Arc<Mutex<Agent>>>,
    cancellation: Option<CancellationHandle>,
    cancellation_requested: bool,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AppEvent>>,
    approval_rx: Option<std::sync::mpsc::Receiver<PendingApproval>>,
    approval_mode: Option<ApprovalMode>,
    prompt_char: String,
    agent_mode: String,
    /// Fully-qualified MCP tool names registered at startup (`mcp__server__tool`).
    mcp_tools: Vec<String>,
    subagent_manager: Option<Arc<ParkingMutex<SubagentManager>>>,
    project: String,
    branch: String,
    branch_checked: Option<Instant>,
    #[cfg(feature = "pi-compat")]
    session: Option<(PiSession, PathBuf)>,
}

enum AppEvent {
    Rx4(Rx4Event),
    Error(String),
    PromptFailed { prompt: String },
    FileSuggestions { query: String, paths: Vec<String> },
    McpTools(Vec<String>),
    Idle,
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            pastes: Vec::new(),
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
            file_suggestions: Vec::new(),
            file_choice: 0,
            pending_file_query: None,
            slash_suggestions: Vec::new(),
            slash_choice: 0,
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
            context_tokens: 0,
            context_window: 128_000,
            agent: None,
            cancellation: None,
            cancellation_requested: false,
            event_rx: None,
            approval_rx: None,
            approval_mode: None,
            prompt_char: ">".to_string(),
            agent_mode: "coding".to_string(),
            mcp_tools: Vec::new(),
            subagent_manager: None,
            project: project_name(),
            branch: "-".to_string(),
            branch_checked: None,
            #[cfg(feature = "pi-compat")]
            session: None,
        }
    }

    fn refresh_branch(&mut self) {
        if self
            .branch_checked
            .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_secs(5))
        {
            self.branch = git_branch();
            self.branch_checked = Some(Instant::now());
        }
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.pastes.clear();
    }

    fn expanded_input(&self) -> String {
        self.pastes
            .iter()
            .enumerate()
            .fold(self.input.clone(), |input, (index, paste)| {
                input.replace(&format!("[paste #{}]", index + 1), paste)
            })
    }

    fn paste(&mut self, pasted: &str) {
        let pasted: String = pasted
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|character| *character == '\n' || !character.is_control())
            .collect();
        if pasted.is_empty() {
            return;
        }
        if pasted.split('\n').count() > LARGE_PASTE_LINES
            || pasted.chars().count() > LARGE_PASTE_CHARS
        {
            self.pastes.push(pasted);
            self.input
                .push_str(&format!("[paste #{}]", self.pastes.len()));
        } else {
            self.input.push_str(&pasted);
        }
        self.file_suggestions.clear();
        self.pending_file_query = None;
    }

    fn insert_newline(&mut self) {
        self.input.push('\n');
        self.file_suggestions.clear();
        self.pending_file_query = None;
        self.slash_suggestions.clear();
    }

    #[cfg(feature = "pi-compat")]
    fn persist(&mut self) -> std::io::Result<()> {
        if let Some((session, dir)) = &mut self.session {
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

    fn cancel_turn(&mut self) {
        if !self.busy {
            return;
        }
        if self.permission_prompt {
            self.resolve_permission(false);
        }
        self.cancellation_requested = true;
        if let Some(cancellation) = &self.cancellation {
            cancellation.cancel();
        }
        for message in &mut self.messages {
            message.is_streaming = false;
        }
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
        let filtered_models = self.filtered_models();
        tpl.set(
            "selected_model",
            self.model_choice
                .and_then(|index| filtered_models.get(index).map(|model| model.id.clone()))
                .unwrap_or_default(),
        );
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
        let file_rows = self
            .file_suggestions
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let mut row = TemplateContext::new();
                row.set("path", path.clone());
                row.set("selected", index == self.file_choice);
                row
            })
            .collect();
        tpl.set("has_file_suggestions", !self.file_suggestions.is_empty());
        tpl.set("file_rows", TemplateValue::List(file_rows));
        let slash_rows = self
            .slash_suggestions
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let mut row = TemplateContext::new();
                row.set("command", command.clone());
                row.set("selected", index == self.slash_choice);
                row
            })
            .collect();
        tpl.set("has_slash_suggestions", !self.slash_suggestions.is_empty());
        tpl.set("slash_rows", TemplateValue::List(slash_rows));
        tpl.set("busy", self.busy);
        tpl.set("auto_scroll", self.auto_scroll);
        tpl.set("version", env!("CARGO_PKG_VERSION"));
        tpl.set("session_name", self.session_name.clone());
        tpl.set("show_header", self.show_header);
        tpl.set(
            "spinner",
            if self.busy {
                spinner_frame(self.spinner_start)
            } else {
                ""
            },
        );
        tpl.set("cursor", blink_cursor(self.cursor_start));
        tpl.set("prompt_char", self.prompt_char.clone());
        tpl.set("agent_mode", self.agent_mode.clone());
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
        tpl.set("project", self.project.clone());
        tpl.set("branch", self.branch.clone());
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
        let text = self.expanded_input().trim().to_string();
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

        self.clear_input();
        self.cancellation_requested = false;
        self.busy = true;

        let agent = agent.clone();
        tokio::spawn(async move {
            let mut agent = agent.lock().await;
            let result = agent.prompt(&text).await;
            if let Err(error) = result {
                if !matches!(error, AgentError::Cancelled) {
                    let _ = tx.send(AppEvent::PromptFailed { prompt: text });
                }
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
            AppEvent::PromptFailed { prompt } => {
                if self.input.is_empty() {
                    self.input = prompt;
                }
                self.file_suggestions.clear();
                self.pending_file_query = None;
            }
            AppEvent::FileSuggestions { query, paths } => {
                if file_query(&self.input) == Some(query.as_str()) {
                    self.file_suggestions = paths;
                    self.file_choice = 0;
                }
                if self.pending_file_query.as_deref() == Some(query.as_str()) {
                    self.pending_file_query = None;
                }
            }
            AppEvent::McpTools(names) => {
                self.mcp_tools = names;
            }
            AppEvent::Idle => {
                self.busy = false;
            }
        }
    }

    fn refresh_file_suggestions(&mut self, tx: tokio::sync::mpsc::UnboundedSender<AppEvent>) {
        let Some(query) = file_query(&self.input).map(str::to_string) else {
            self.file_suggestions.clear();
            self.pending_file_query = None;
            return;
        };
        if self.pending_file_query.as_deref() == Some(query.as_str()) {
            return;
        }
        self.pending_file_query = Some(query.clone());
        tokio::task::spawn_blocking(move || {
            let paths = search_files(&query, 8);
            let _ = tx.send(AppEvent::FileSuggestions { query, paths });
        });
    }

    fn move_file_choice(&mut self, delta: isize) {
        if self.file_suggestions.is_empty() {
            return;
        }
        self.file_choice = (self.file_choice as isize + delta)
            .rem_euclid(self.file_suggestions.len() as isize) as usize;
    }

    fn refresh_slash_suggestions(&mut self) {
        self.slash_suggestions = matching_slash_commands(&self.input);
        self.slash_choice = 0;
    }

    fn move_slash_choice(&mut self, delta: isize) {
        if self.slash_suggestions.is_empty() {
            return;
        }
        self.slash_choice = (self.slash_choice as isize + delta)
            .rem_euclid(self.slash_suggestions.len() as isize) as usize;
    }

    fn choose_slash_command(&mut self) {
        let Some(command) = self.slash_suggestions.get(self.slash_choice) else {
            return;
        };
        self.input = format!("{command} ");
        self.slash_suggestions.clear();
    }

    fn dismiss_suggestions(&mut self) {
        self.slash_suggestions.clear();
        self.file_suggestions.clear();
        self.pending_file_query = None;
    }

    fn choose_file(&mut self) {
        let Some(path) = self.file_suggestions.get(self.file_choice) else {
            return;
        };
        let Some(query) = file_query(&self.input) else {
            return;
        };
        let start = self.input.len() - query.len();
        self.input.replace_range(start.., path);
        self.input.push(' ');
        self.file_suggestions.clear();
        self.pending_file_query = None;
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
                self.context_tokens = used_tokens;
                self.refresh_context_pct();
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
            // rx4 0.6.0 runs guardrails, self-healing and a plan gate inside
            // the loop. Surface them: a warning the user never sees is a
            // turn that changes behaviour for no visible reason.
            Rx4Event::GuardrailWarning { tool, reason } => {
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("guardrail on `{tool}`: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::GuardrailStop { tool, reason } => {
                self.messages.push(ChatMessage {
                    role: "error".to_string(),
                    content: format!("Stopped by guardrail on `{tool}`: {reason}"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                self.busy = false;
            }
            Rx4Event::SelfHealing {
                attempt,
                max_attempts,
                ..
            } => {
                self.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: format!("retrying after a tool failure ({attempt}/{max_attempts})"),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
            }
            Rx4Event::PlanProposed(_) | Rx4Event::PlanDecided { .. } => {
                // No plan approver is attached, so these are informational.
            }
            Rx4Event::Error(msg) => {
                if self.cancellation_requested && msg.to_ascii_lowercase().contains("cancel") {
                    return;
                }
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
        if self
            .providers
            .iter()
            .any(|provider| provider.id == "openai-codex")
        {
            choices.extend(
                rs_ai_oauth::codex::CHATGPT_CODEX_MODELS
                    .iter()
                    .map(|id| ModelChoice {
                        id: (*id).to_string(),
                        provider: "openai-codex".to_string(),
                    }),
            );
        }
        for provider in ["openai", "openai-codex"] {
            if self
                .providers
                .iter()
                .any(|configured| configured.id == provider)
            {
                choices.extend(GPT_5_6_MODELS.iter().map(|id| ModelChoice {
                    id: (*id).to_string(),
                    provider: provider.to_string(),
                }));
            }
        }
        choices.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        choices.dedup_by(|a, b| a.provider == b.provider && a.id == b.id);
        if let Some(choice) = choices.iter().find(|choice| choice.id == self.model) {
            self.provider_choice = self
                .providers
                .iter()
                .position(|provider| provider.id == choice.provider)
                .unwrap_or(0);
        }
        self.model_choices = choices;
        self.clear_input();
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
        self.set_model(model.id.clone());
        self.selecting_model = false;
        self.clear_input();
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

    fn set_model(&mut self, model: String) {
        self.model = model;
        self.context_window = context_window_for_model(&self.model);
        self.refresh_context_pct();
    }

    fn refresh_context_pct(&mut self) {
        self.context_pct = self
            .context_tokens
            .saturating_mul(100)
            .checked_div(self.context_window)
            .unwrap_or(0);
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

/// Map a CLI provider name onto the OAuth provider it logs into.
fn oauth_provider(name: &str) -> Option<rs_ai_oauth::OAuthProvider> {
    Some(match name {
        "grok" | "xai" => rs_ai_oauth::OAuthProvider::Xai,
        "openai" | "chatgpt" => rs_ai_oauth::OAuthProvider::ChatGpt,
        "gemini" | "google" => rs_ai_oauth::OAuthProvider::Gemini,
        "copilot" => rs_ai_oauth::OAuthProvider::Copilot,
        "kimi" => rs_ai_oauth::OAuthProvider::Kimi,
        "antigravity" => rs_ai_oauth::OAuthProvider::Antigravity,
        _ => return None,
    })
}

fn run_login(provider: Option<&str>) -> anyhow::Result<()> {
    // Ask rather than assume. Silently defaulting to one provider sends the
    // user through an OAuth flow for an account they may not even have.
    let provider = match provider {
        Some(name) => name,
        None => choose_provider()?,
    };
    let Some(oauth) = oauth_provider(provider) else {
        anyhow::bail!(
            "Unknown provider: {provider}. Available: openai, grok, gemini, copilot, kimi, antigravity"
        );
    };
    println!("Starting OAuth flow for {provider}...");
    let tokens = rs_ai_oauth::start_oauth_flow(oauth)?;
    // The shared store, so a login here is visible to every rs_ai_oauth tool.
    let path = rs_ai_oauth::credentials::save(&oauth, &tokens)?;
    println!("Token saved to {}", path.display());
    Ok(())
}

fn run_login_from_tui(provider: Option<&str>) -> anyhow::Result<()> {
    let raw_mode_was_enabled = disable_raw_mode().is_ok();
    println!("\r\n");
    let login_result = run_login(provider);
    let restore_result = raw_mode_was_enabled
        .then(enable_raw_mode)
        .transpose()
        .map_err(anyhow::Error::from);
    login_result.and(restore_result.map(|_| ()))
}

fn provider_is_configured(provider: &str) -> bool {
    let env_configured = match provider {
        "grok" => "XAI_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" => "GOOGLE_API_KEY",
        _ => "",
    };
    (!env_configured.is_empty()
        && std::env::var(env_configured)
            .ok()
            .is_some_and(|key| !key.is_empty()))
        || oauth_provider(provider)
            .and_then(|oauth| rs_ai_oauth::credentials::load(&oauth))
            .is_some_and(|tokens| !tokens.access_token.is_empty())
}

fn push_system_message(app: &mut App, content: impl Into<String>) {
    app.messages.push(ChatMessage {
        role: "system".to_string(),
        content: content.into(),
        is_tool: false,
        tool_name: String::new(),
        tool_call_id: String::new(),
        is_streaming: false,
    });
}

fn config_summary(app: &App) -> String {
    let providers = [
        ("1", "openai", "OpenAI"),
        ("2", "grok", "xAI"),
        ("3", "gemini", "Google Gemini"),
        ("4", "copilot", "GitHub Copilot"),
        ("5", "kimi", "Kimi"),
        ("6", "antigravity", "Antigravity"),
    ];
    let auth = providers
        .iter()
        .map(|(number, id, name)| {
            let status = if provider_is_configured(id) {
                "configured"
            } else {
                "not configured"
            };
            format!("  [{number}] {name:<16} {status}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let credentials = rs_ai_oauth::credentials::credentials_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unavailable".to_string());
    let workspace = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    format!(
        "Configuration\n  workspace: {workspace}\n  model: {}\n  scope: {}\n  credentials: {credentials}\n\nAuthentication\n{auth}\n\nCommands\n  /login [provider]       sign in and open the browser\n  /config login [provider]\n  /config model <name>\n  /config scope <name>",
        app.model, app.agent_mode
    )
}

fn choose_provider() -> anyhow::Result<&'static str> {
    // Every provider run_login accepts, so the menu and the command agree.
    const PROVIDERS: [(&str, &str); 6] = [
        ("1", "openai"),
        ("2", "grok"),
        ("3", "gemini"),
        ("4", "copilot"),
        ("5", "kimi"),
        ("6", "antigravity"),
    ];
    println!("Which provider do you want to log in with?");
    for (number, provider) in PROVIDERS {
        println!("  {number}) {provider}");
    }
    loop {
        print!("Provider [1-6]: ");
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
        println!("Choose 1-6 or enter a provider name.");
    }
}

/// A token left by an older telekinesis login whose file name does not match
/// the shared store's provider name — `openai` was written where the store
/// looks for `chatgpt`, so those logins would otherwise read as logged out.
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

fn setup_providers(rt: &tokio::runtime::Runtime) -> Vec<(ConfiguredProvider, String)> {
    let mut configured = Vec::new();

    if let Some(token) = saved_token("openai", rt) {
        configured.push((
            ConfiguredProvider {
                id: "openai-codex".to_string(),
                name: "ChatGPT Codex".to_string(),
                client: codex_provider::provider_arc(token),
            },
            "gpt-5.5".to_string(),
        ));
    } else if let Some(key) = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|key| !key.is_empty())
    {
        configured.push((
            ConfiguredProvider {
                id: "openai".to_string(),
                name: "OpenAI".to_string(),
                client: Arc::new(OpenAIProvider::with_base_url(
                    "https://api.openai.com/v1",
                    key,
                    "openai",
                    "OpenAI",
                )),
            },
            "gpt-5.4".to_string(),
        ));
    }

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
            "GOOGLE_API_KEY",
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google Gemini",
            "gemini-2.0-flash",
        ),
    ];
    configured.extend(
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
            }),
    );
    configured
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

/// Parsed `tk exec` invocation.
#[derive(Debug, Default, PartialEq)]
struct ExecArgs {
    /// `None` means "read the prompt from stdin".
    prompt: Option<String>,
    json: bool,
    cwd: Option<PathBuf>,
    help: bool,
}

/// Parse the arguments after `exec`.
///
/// `-` and a missing prompt both mean stdin, so a caller with a long task can
/// pipe it in instead of fighting shell quoting.
fn parse_exec_args(args: &[String]) -> Result<ExecArgs, String> {
    let mut parsed = ExecArgs::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" => parsed.help = true,
            "--json" => parsed.json = true,
            "--cwd" => {
                index += 1;
                let dir = args
                    .get(index)
                    .ok_or_else(|| "--cwd requires a directory".to_string())?;
                parsed.cwd = Some(PathBuf::from(dir));
            }
            "-" => parsed.prompt = None,
            _ if arg.starts_with("--") => return Err(format!("Unknown option: {arg}")),
            _ => {
                if parsed.prompt.is_some() {
                    return Err(format!("Unexpected extra argument: {arg}"));
                }
                parsed.prompt = Some(arg.to_string());
            }
        }
        index += 1;
    }
    Ok(parsed)
}

fn exec_help() {
    eprintln!("tk exec — run one agent turn without a TUI");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  tk exec \"<prompt>\"      Run the prompt and print the final text to stdout");
    eprintln!("  tk exec -               Read the prompt from stdin");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --json          Emit {{\"ok\",\"text\",\"error\"}} on stdout instead of prose");
    eprintln!("  --cwd <dir>     Workspace to run against (default: current directory)");
    eprintln!("  --help          Show this help");
    eprintln!();
    eprintln!("Only the final text goes to stdout; status and errors go to stderr.");
}

/// Report an exec failure the way the caller asked for it, then exit non-zero.
fn exec_failure(json: bool, message: &str) -> ! {
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": false, "text": "", "error": message })
        );
    }
    eprintln!("error: {message}");
    std::process::exit(1);
}

/// One-shot headless run: no terminal, no TUI, final text on stdout.
fn run_exec(args: &[String]) -> anyhow::Result<()> {
    let parsed = match parse_exec_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("error: {message}");
            exec_help();
            std::process::exit(2);
        }
    };
    if parsed.help {
        exec_help();
        return Ok(());
    }
    let json = parsed.json;

    if let Some(dir) = &parsed.cwd {
        if let Err(error) = std::env::set_current_dir(dir) {
            exec_failure(
                json,
                &format!("cannot use --cwd {}: {error}", dir.display()),
            );
        }
    }

    let prompt = match parsed.prompt {
        Some(prompt) => prompt,
        None => {
            use std::io::{IsTerminal, Read};
            if stdin().is_terminal() {
                exec_failure(json, "no prompt given; pass one as an argument or on stdin");
            }
            let mut buffer = String::new();
            if let Err(error) = stdin().read_to_string(&mut buffer) {
                exec_failure(json, &format!("cannot read prompt from stdin: {error}"));
            }
            buffer
        }
    };
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        exec_failure(json, "empty prompt");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let providers = setup_providers(&rt);
    // Never fall back to the interactive login: this run has nobody to answer.
    let Some((configured, default_model)) = providers.into_iter().next() else {
        exec_failure(json, "no provider credentials; run `tk login <provider>`");
    };

    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (mut agent, _subagent_manager) =
        build_agent(configured.client, &default_model, "high", workspace.clone());
    // Nothing can answer an approval prompt headlessly; the policy still gates.
    agent.set_approver(Arc::new(rx4::permissions::AlwaysAllow));

    // Tool activity is progress reporting, not output — it belongs on stderr.
    agent.subscribe(move |event: &Rx4Event| match event {
        Rx4Event::ToolExecutionStart(call) => eprintln!("· {}", call.name),
        Rx4Event::Error(message) => eprintln!("· error: {message}"),
        _ => {}
    });

    eprintln!(
        "· {} / {} in {}",
        configured.name,
        default_model,
        workspace.display()
    );

    let result = rt.block_on(agent.prompt(&prompt));
    if let Err(error) = result {
        exec_failure(json, &error.to_string());
    }

    let text = agent
        .messages
        .read()
        .iter()
        .rev()
        .find(|message| {
            matches!(message.role, Role::Assistant) && !message.content.trim().is_empty()
        })
        .map(|message| message.content.trim().to_string())
        .unwrap_or_default();

    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "text": text, "error": serde_json::Value::Null })
        );
    } else {
        println!("{text}");
    }
    Ok(())
}

/// Build the rx4 Agent every surface shares.
///
/// The TUI and `tk exec` must run the same prompt, tools, policy, sandbox and
/// skills — anything that drifts here becomes a headless run that behaves
/// unlike the interactive one. Only the approver and the event subscription
/// are left to the caller, because those are what actually differ.
fn build_agent(
    provider: Arc<dyn Provider>,
    model: &str,
    effort: &str,
    workspace: PathBuf,
) -> (Agent, Arc<ParkingMutex<SubagentManager>>) {
    let mut agent = Agent::new();
    agent.set_system_prompt(include_str!("../SYSTEM_PROMPT.md"));
    agent.set_scope(Scope::Coding);
    let subagent_manager = Arc::new(ParkingMutex::new(
        SubagentManager::new()
            .with_provider(provider.clone())
            .with_model(model.to_string()),
    ));
    agent.set_tools(build_tool_registry(&subagent_manager, &[]));
    subagent_manager.lock().set_tools(agent.tools.clone());
    agent.set_workspace_root(workspace);
    agent.load_project_context();
    agent.set_model(model);
    agent.set_reasoning_effort(Some(effort.to_string()));
    agent.set_provider(provider);
    // Policy.workspace_write enables OS sandbox flag; enable_os_sandbox installs runner.
    agent.set_policy(product_policy::tele_coding_policy());
    let _ = agent.enable_os_sandbox();
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
    agent.set_graph_memory(rx4::GraphMemory::new());
    agent.enable_auto_dream(true);
    (agent, subagent_manager)
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

    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (mut agent, subagent_manager) =
        build_agent(provider.clone(), &model, &resumed_effort, workspace);
    #[cfg(feature = "pi-compat")]
    if let Some(session) = &loaded_session {
        *agent.messages.write() = session.messages();
    }
    let (approver, approval_rx) = ChannelApprover::pair();
    let approval_mode = approver.mode();
    agent.set_approver(Arc::new(approver));

    let event_tx_clone = event_tx.clone();
    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx_clone.send(AppEvent::Rx4(event.clone()));
    });

    let cancellation = agent.cancellation_handle();
    let agent = Arc::new(Mutex::new(agent));

    {
        let agent = agent.clone();
        let manager = Arc::clone(&subagent_manager);
        let tx = event_tx.clone();
        rt.spawn(async move {
            let (specs, errors) = discover_mcp_tools().await;
            for error in errors {
                let _ = tx.send(AppEvent::Error(error));
            }
            if specs.is_empty() {
                return;
            }
            let names: Vec<String> = specs.iter().map(|s| s.full_name.clone()).collect();
            let tools = build_tool_registry(&manager, &specs);
            {
                let mut agent = agent.lock().await;
                agent.set_tools(tools);
                manager.lock().set_tools(agent.tools.clone());
            }
            let _ = tx.send(AppEvent::McpTools(names));
        });
    }

    let mut app = App::new();
    app.set_model(model);
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
    app.cancellation = Some(cancellation);
    app.event_rx = Some(event_rx);
    app.approval_rx = Some(approval_rx);
    app.approval_mode = Some(approval_mode);
    app.subagent_manager = Some(subagent_manager);

    let _rt_guard = rt.enter();

    enable_raw_mode()?;
    let mut stdout = stdout();
    crossterm::execute!(stdout, EnableBracketedPaste)?;
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
        app.refresh_branch();

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
            match crossterm::event::read()? {
                Event::Paste(pasted) => {
                    app.paste(&pasted);
                    if app.selecting_model {
                        app.reset_model_choice();
                    }
                }
                Event::Key(key) => {
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
                                if key.code == KeyCode::Esc {
                                    app.cancel_turn();
                                }
                                continue;
                            }
                            _ => continue,
                        }
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Enter, KeyModifiers::SHIFT) => {
                            app.insert_newline();
                        }
                        (KeyCode::Tab, _) if !app.slash_suggestions.is_empty() => {
                            app.choose_slash_command();
                        }
                        (KeyCode::Tab, _) if !app.file_suggestions.is_empty() => {
                            app.choose_file();
                        }
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
                                app.cancel_turn();
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
                            app.clear_input();
                        }
                        (KeyCode::Esc, _) if app.busy => {
                            app.cancel_turn();
                        }
                        (KeyCode::Esc, _)
                            if !app.slash_suggestions.is_empty()
                                || !app.file_suggestions.is_empty() =>
                        {
                            app.dismiss_suggestions();
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
                            if !app.slash_suggestions.is_empty() {
                                app.move_slash_choice(-1);
                                continue;
                            }
                            if !app.file_suggestions.is_empty() {
                                app.move_file_choice(-1);
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
                            if !app.slash_suggestions.is_empty() {
                                app.move_slash_choice(1);
                                continue;
                            }
                            if !app.file_suggestions.is_empty() {
                                app.move_file_choice(1);
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
                            } else {
                                app.refresh_slash_suggestions();
                                app.refresh_file_suggestions(event_tx.clone());
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
                            } else {
                                app.refresh_slash_suggestions();
                                app.refresh_file_suggestions(event_tx.clone());
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    terminal.backend_mut().flush()?;
    crossterm::execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    drop(terminal);
    disable_raw_mode()?;
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

/// A tool discovered on an MCP server, ready to register into a `ToolRegistry`.
struct McpToolSpec {
    full_name: String,
    description: String,
    parameters: String,
    remote_name: String,
    client: Arc<rx4::McpClient>,
}

/// Per-server budget for connecting and listing tools.
const MCP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Best-effort MCP discovery from ~/.telekinesis/mcp.json. Never fails TUI startup.
async fn discover_mcp_tools() -> (Vec<McpToolSpec>, Vec<String>) {
    let configs = mcp_config::load();
    let mut specs = Vec::new();
    let mut errors = Vec::new();
    if configs.is_empty() {
        return (specs, errors);
    }

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
        let listed = tokio::time::timeout(MCP_CONNECT_TIMEOUT, async {
            let client = rx4::McpClient::connect_config(&engine_cfg).await?;
            let listed = client.list_tools().await?;
            Ok::<_, anyhow::Error>((Arc::new(client), listed))
        })
        .await;
        let (client, listed) = match listed {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                errors.push(format!("MCP server `{}` unavailable: {e}", cfg.name));
                continue;
            }
            Err(_) => {
                errors.push(format!(
                    "MCP server `{}` timed out after {}s",
                    cfg.name,
                    MCP_CONNECT_TIMEOUT.as_secs()
                ));
                continue;
            }
        };
        for tool in listed {
            let description = if tool.description.is_empty() {
                format!("MCP tool {} from {}", tool.name, cfg.name)
            } else {
                tool.description.clone()
            };
            specs.push(McpToolSpec {
                full_name: format!("mcp__{}__{}", cfg.name, tool.name),
                description,
                parameters: tool.input_schema.to_string(),
                remote_name: tool.name.clone(),
                client: client.clone(),
            });
        }
    }
    (specs, errors)
}

fn register_mcp_tools(tools: &mut ToolRegistry, specs: &[McpToolSpec]) {
    for spec in specs {
        let client = spec.client.clone();
        let remote_name = spec.remote_name.clone();
        tools.register(
            ToolDefinition::new_boxed(
                spec.full_name.clone(),
                spec.description.clone(),
                spec.parameters.clone(),
                Box::new(move |_ctx, args| {
                    let client = client.clone();
                    let remote_name = remote_name.clone();
                    Box::pin(async move {
                        let value: serde_json::Value = serde_json::from_str(&args)
                            .unwrap_or_else(|_| serde_json::json!({ "raw": args }));
                        match client.call_tool(&remote_name, &value).await {
                            Ok(v) => ToolResult::ok(remote_name.clone(), v.to_string()),
                            Err(e) => ToolResult::err(remote_name.clone(), e.to_string()),
                        }
                    })
                }),
            )
            .with_effect(ToolEffect::Network),
        );
    }
}

const DARASH_ENDPOINT: &str = "http://localhost:8080";
const DARASH_TOOL_NAME: &str = "web_search";

fn execute_darash_search(ctx: Arc<ToolContext>, args: String) -> ToolFuture {
    Box::pin(async move {
        let value: serde_json::Value = match serde_json::from_str(&args) {
            Ok(value) => value,
            Err(error) => {
                return ToolResult::err(DARASH_TOOL_NAME, format!("invalid json: {error}"))
            }
        };
        let Some(query) = value.get("query").and_then(|value| value.as_str()) else {
            return ToolResult::err(DARASH_TOOL_NAME, "query required");
        };
        if query.trim().is_empty() {
            return ToolResult::err(DARASH_TOOL_NAME, "query must not be empty");
        }
        let Some(sandbox) = ctx.sandbox.as_ref() else {
            return ToolResult::err(DARASH_TOOL_NAME, "sandbox unavailable; network denied");
        };
        if let Err(error) = sandbox.validate_network() {
            return ToolResult::err(DARASH_TOOL_NAME, error.to_string());
        }
        let client = match SearchClient::new(DARASH_ENDPOINT) {
            Ok(client) => client,
            Err(error) => return ToolResult::err(DARASH_TOOL_NAME, error.to_string()),
        };
        match ctx
            .cancellation
            .run(client.search(&SearchQuery::new(query)))
            .await
        {
            Ok(Ok(response)) => {
                ToolResult::ok(DARASH_TOOL_NAME, format_search_response(query, &response))
            }
            Ok(Err(error)) => ToolResult::err(DARASH_TOOL_NAME, error.to_string()),
            Err(_) => ToolResult::err(DARASH_TOOL_NAME, "search cancelled"),
        }
    })
}

fn register_darash_tool(tools: &mut ToolRegistry) {
    tools.register(
        ToolDefinition::new_fn(
            DARASH_TOOL_NAME,
            "Search the local SearxNG instance at http://localhost:8080 with Darash and return bounded citations.",
            r#"{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}"#,
            execute_darash_search,
        )
        .with_effect(ToolEffect::Network),
    );
}

/// Build the complete tool registry. Called once at startup with no MCP tools,
/// and again once MCP discovery finishes so the swap is always all-or-nothing.
fn build_tool_registry(
    subagent_manager: &Arc<ParkingMutex<SubagentManager>>,
    mcp: &[McpToolSpec],
) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    rx4::computer_use::register_tools(&mut tools);
    register_darash_tool(&mut tools);
    register_mcp_tools(&mut tools, mcp);
    register_spawn_agent_tool(&mut tools, Arc::clone(subagent_manager));
    tools
}

fn plan_request(task: &str) -> String {
    format!(
        "Create a concrete implementation plan for: {task}\n\nInspect the relevant code and instructions first. Return the files to change, the ordered steps, risks, and verification commands. Do not modify the workspace."
    )
}

fn review_request(target: &str) -> String {
    format!(
        "Review {target} for correctness, security, regressions, and missing verification. Inspect the repository before reporting. Do not modify the workspace. Return only actionable findings, ordered by severity, with file paths and concise evidence; say explicitly when there are no findings."
    )
}

const SEARCH_RESULT_LIMIT: usize = 8;
const SEARCH_TEXT_LIMIT: usize = 600;

fn clean_search_text(value: &str, limit: usize) -> String {
    let mut text: String = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    if text.chars().count() > limit {
        text = text.chars().take(limit.saturating_sub(1)).collect();
        text.push('…');
    }
    text
}

fn format_search_response(query: &str, response: &SearchResponse) -> String {
    let mut lines = vec![format!(
        "Search results for {:?} ({} results)",
        clean_search_text(query, SEARCH_TEXT_LIMIT),
        response.number_of_results
    )];
    for result in response.results.iter().take(SEARCH_RESULT_LIMIT) {
        let title = clean_search_text(&result.title, SEARCH_TEXT_LIMIT);
        let title = if title.is_empty() {
            "(untitled)".to_string()
        } else {
            title
        };
        lines.push(format!(
            "\n{title}\n{}\n{}",
            clean_search_text(&result.url, SEARCH_TEXT_LIMIT),
            clean_search_text(&result.content, SEARCH_TEXT_LIMIT)
        ));
    }
    if !response.answers.is_empty() {
        lines.push(format!(
            "Answer: {}",
            clean_search_text(&response.answers.join("; "), SEARCH_TEXT_LIMIT)
        ));
    }
    if !response.suggestions.is_empty() {
        lines.push(format!(
            "Suggestions: {}",
            clean_search_text(&response.suggestions.join(", "), SEARCH_TEXT_LIMIT)
        ));
    }
    lines.join("\n")
}

fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    agent: &Arc<Mutex<Agent>>,
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
) {
    let parts: Vec<&str> = cmd.splitn(2, ' ').collect();
    let command = parts[0];
    let arg = parts.get(1).copied().unwrap_or("");
    app.clear_input();
    app.slash_suggestions.clear();

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
                content: "Commands: /login [provider], /config, /model [name], /scope <coding|research|plan|ask|computer_use>, /plan <task>, /review [target], /subagent spawn|list|cancel, /budget <max-cost>, /mcp, /todo, /clear, /cost, /help, /quit\nKeys: / command suggestions: Up/Down select, Tab insert · model selector: type search, Left/Right provider, Up/Down model, Enter apply, Esc cancel · Shift+Enter newline · Esc/Ctrl+C interrupt · Shift+Tab effort · Ctrl+B header · Ctrl+L clear".to_string(),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/login" => {
            let provider = (!arg.is_empty()).then_some(arg);
            let result = run_login_from_tui(provider);
            push_system_message(
                app,
                match result {
                    Ok(()) => "Login complete. Restart tk to load the new provider.".to_string(),
                    Err(error) => format!("Login failed: {error}"),
                },
            );
        }
        "/config" => {
            let config_parts: Vec<&str> = arg.splitn(2, ' ').collect();
            let subcommand = config_parts.first().copied().unwrap_or("");
            let rest = config_parts.get(1).copied().unwrap_or("");
            match subcommand {
                "" | "show" => {
                    let summary = config_summary(app);
                    push_system_message(app, summary);
                },
                "login" => {
                    let provider = (!rest.is_empty()).then_some(rest);
                    let result = run_login_from_tui(provider);
                    push_system_message(
                        app,
                        match result {
                            Ok(()) => "Login complete. Restart tk to load the new provider."
                                .to_string(),
                            Err(error) => format!("Login failed: {error}"),
                        },
                    );
                }
                "model" if !rest.is_empty() => {
                    handle_slash_command(app, &format!("/model {rest}"), agent, tx);
                }
                "scope" if !rest.is_empty() => {
                    handle_slash_command(app, &format!("/scope {rest}"), agent, tx);
                }
                _ => push_system_message(
                    app,
                    "Usage: /config | /config login [provider] | /config model <name> | /config scope <name>",
                ),
            }
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
            let Some(scope) = Scope::parse_scope(arg) else {
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: "Usage: /scope <coding|research|plan|ask|computer_use>".to_string(),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                return;
            };
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_scope(scope);
            }
            app.agent_mode = scope.name().to_string();
            app.messages.push(ChatMessage {
                role: "system".to_string(),
                content: format!("Scope set to: {}", scope.name()),
                is_tool: false,
                tool_name: String::new(),
                tool_call_id: String::new(),
                is_streaming: false,
            });
        }
        "/plan" => {
            if arg.is_empty() {
                app.messages.push(ChatMessage {
                    role: "system".to_string(),
                    content: "Usage: /plan <task>".to_string(),
                    is_tool: false,
                    tool_name: String::new(),
                    tool_call_id: String::new(),
                    is_streaming: false,
                });
                return;
            }
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_scope(Scope::Plan);
            }
            app.agent_mode = Scope::Plan.name().to_string();
            app.input = plan_request(arg);
            app.submit_prompt(agent, tx.clone());
        }
        "/review" => {
            if let Ok(mut agent) = agent.try_lock() {
                agent.set_scope(Scope::Research);
            }
            app.agent_mode = Scope::Research.name().to_string();
            let target = if arg.is_empty() {
                "the current workspace"
            } else {
                arg
            };
            app.input = review_request(target);
            app.submit_prompt(agent, tx.clone());
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
    if args.len() >= 2 && args[1] == "exec" {
        return run_exec(&args[2..]);
    }
    if args.len() >= 2 && (args[1] == "--help" || args[1] == "-h") {
        println!("telekinesis (tk) — AI coding agent TUI");
        println!();
        println!("USAGE:");
        println!("  tk              Start interactive TUI");
        println!("  tk -c           Continue newest session for this project");
        println!("  tk exec \"<prompt>\"   Run one turn headlessly, final text on stdout");
        println!("                       (prompt from stdin with `-`; --json, --cwd <dir>)");
        println!(
            "  tk login <provider>  OAuth login (openai, grok, gemini, copilot, kimi, antigravity)"
        );
        println!("  /login [provider]     OAuth login from the TUI");
        println!("  /config               Show runtime configuration and auth status");
        println!("  tk --help       Show this help");
        println!();
        println!("ENVIRONMENT:");
        println!("  XAI_API_KEY         xAI Grok API key");
        println!("  OPENAI_API_KEY      OpenAI API key");
        println!("  GOOGLE_API_KEY      Google Gemini API key");
        println!();
        println!("KEYS:");
        println!("  Enter        Submit prompt");
        println!("  Shift+Enter  New line");
        println!("  Esc/Ctrl+C   Interrupt / exit");
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
        build_tool_registry, clean_search_text, context_window_for_model, execute_darash_search,
        file_query, format_search_response, is_continue_arg, is_permission_toggle, load_template,
        matching_slash_commands, plan_request, review_request, search_files, tool_result_summary,
        App, ChatMessage, ConfiguredProvider, GPT_5_CONTEXT_WINDOW,
    };
    #[cfg(feature = "pi-compat")]
    use super::{restored_chat, PiEntryType, PiSession};
    use crossterm::event::{KeyCode, KeyModifiers};
    use darash::{SearchResponse, SearchResult};
    use rx4::provider::OpenAIProvider;
    use rx4::subagent::SubagentManager;
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

    fn exec_args(args: &[&str]) -> super::ExecArgs {
        super::parse_exec_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn exec_reads_prompt_from_argument_or_stdin() {
        assert_eq!(exec_args(&["say hi"]).prompt.as_deref(), Some("say hi"));
        assert_eq!(exec_args(&["-"]).prompt, None);
        assert_eq!(exec_args(&[]).prompt, None);
    }

    #[test]
    fn exec_parses_json_cwd_and_rejects_junk() {
        let parsed = exec_args(&["--json", "--cwd", "/tmp", "task"]);
        assert!(parsed.json);
        assert_eq!(parsed.cwd, Some(std::path::PathBuf::from("/tmp")));
        assert_eq!(parsed.prompt.as_deref(), Some("task"));
        assert!(super::parse_exec_args(&["--cwd".to_string()]).is_err());
        assert!(super::parse_exec_args(&["--nope".to_string()]).is_err());
        assert!(super::parse_exec_args(&["a".to_string(), "b".to_string()]).is_err());
    }

    #[test]
    fn continue_accepts_short_and_long_flags() {
        assert!(is_continue_arg("-c"));
        assert!(is_continue_arg("--continue"));
        assert!(!is_continue_arg("-C"));
    }

    #[test]
    fn plan_and_review_requests_are_read_only_and_specific() {
        let plan = plan_request("add a session browser");
        assert!(plan.contains("add a session browser"));
        assert!(plan.contains("Do not modify the workspace"));
        let review = review_request("ui/tui/src/main.rs");
        assert!(review.contains("ui/tui/src/main.rs"));
        assert!(review.contains("actionable findings"));
        assert!(review.contains("Do not modify the workspace"));
    }

    #[test]
    fn file_query_tracks_only_the_active_mention() {
        assert_eq!(file_query("review @src/ma"), Some("src/ma"));
        assert_eq!(file_query("review @src/main.rs next"), None);
        assert_eq!(file_query("plain"), None);
    }

    #[test]
    fn file_search_is_bounded_and_ignore_aware() {
        let paths = search_files("src/", 2);
        assert!(!paths.is_empty());
        assert!(paths.len() <= 2);
        assert!(paths.iter().all(|path| path.contains("src/")));
    }

    #[test]
    fn file_selection_replaces_the_active_mention() {
        let mut app = App::new();
        app.input = "review @src/ma".to_string();
        app.file_suggestions = vec!["src/main.rs".to_string(), "src/markdown.rs".to_string()];
        app.move_file_choice(1);
        app.choose_file();
        assert_eq!(app.input, "review @src/markdown.rs ");
        assert!(app.file_suggestions.is_empty());
    }

    #[test]
    fn slash_suggestions_filter_and_insert_commands() {
        assert_eq!(
            matching_slash_commands("/co"),
            vec!["/config".to_string(), "/cost".to_string()]
        );
        assert!(matching_slash_commands("/config show").is_empty());

        let mut app = App::new();
        app.input = "/m".to_string();
        app.refresh_slash_suggestions();
        assert!(app.slash_suggestions.contains(&"/mcp".to_string()));
        app.slash_choice = app
            .slash_suggestions
            .iter()
            .position(|command| command == "/model")
            .expect("model suggestion");
        app.choose_slash_command();
        assert_eq!(app.input, "/model ");
        assert!(app.slash_suggestions.is_empty());
    }

    #[test]
    fn search_results_are_bounded_and_terminal_safe() {
        let response = SearchResponse {
            query: "rust".to_string(),
            number_of_results: 1,
            results: vec![SearchResult {
                title: "Rust".to_string(),
                url: "https://example.com".to_string(),
                content: format!("\u{1b}[31m{}", "snippet ".repeat(200)),
                engine: None,
                engines: Vec::new(),
                category: None,
                published_date: None,
                score: None,
            }],
            answers: Vec::new(),
            corrections: Vec::new(),
            suggestions: Vec::new(),
        };

        let output = format_search_response("rust\u{1b}", &response);
        assert!(!output.contains('\u{1b}'));
        assert!(output.contains("Rust"));
        assert!(output.contains('…'));
        assert_eq!(clean_search_text("a\tb", 20), "a b");
    }

    #[test]
    fn darash_tool_is_registered_as_network_effect() {
        let manager = Arc::new(parking_lot::Mutex::new(SubagentManager::new()));
        let tools = build_tool_registry(&manager, &[]);
        assert!(tools
            .definitions()
            .iter()
            .any(|definition| definition["name"] == "web_search"));
        assert_eq!(tools.effect_of("web_search"), rx4::ToolEffect::Network);
    }

    #[tokio::test]
    async fn darash_tool_honors_network_sandbox() {
        let sandbox = rx4::SandboxManager::new(
            rx4::SandboxProfile::Workspace,
            std::path::PathBuf::from("/workspace"),
        );
        let ctx = rx4::ToolContext::new("/workspace").with_sandbox(Arc::new(sandbox));
        let result = execute_darash_search(Arc::new(ctx), r#"{"query":"rust"}"#.to_string()).await;
        assert!(result.is_error);
        assert!(result.content.contains("network access denied"));
    }

    #[test]
    fn dismissing_suggestions_keeps_the_input() {
        let mut app = App::new();
        app.input = "/m".to_string();
        app.slash_suggestions = vec!["/model".to_string()];
        app.file_suggestions = vec!["src/main.rs".to_string()];
        app.pending_file_query = Some("src/ma".to_string());

        app.dismiss_suggestions();

        assert_eq!(app.input, "/m");
        assert!(app.slash_suggestions.is_empty());
        assert!(app.file_suggestions.is_empty());
        assert!(app.pending_file_query.is_none());
    }

    #[test]
    fn failed_prompt_is_restored_for_editing() {
        let mut app = App::new();
        app.handle_event(super::AppEvent::PromptFailed {
            prompt: "try this".to_string(),
        });
        assert_eq!(app.input, "try this");
        assert!(app.messages.is_empty());

        app.input = "next prompt".to_string();
        app.handle_event(super::AppEvent::PromptFailed {
            prompt: "try this".to_string(),
        });
        assert_eq!(app.input, "next prompt");
        assert!(app.messages.is_empty());
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
    fn cancelling_active_turn_denies_prompt_and_stops_streaming() {
        let mut app = App::new();
        let agent = rx4::agent::Agent::new();
        app.cancellation = Some(agent.cancellation_handle());
        app.busy = true;
        app.permission_prompt = true;
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        app.permission_respond = Some(tx);
        app.messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: "working".to_string(),
            is_tool: false,
            tool_name: String::new(),
            tool_call_id: String::new(),
            is_streaming: true,
        });

        app.cancel_turn();

        assert_eq!(rx.recv().unwrap(), rx4::permissions::Decision::Deny);
        assert!(!app.permission_prompt);
        assert!(!app.messages[0].is_streaming);
        assert!(app.busy);
        app.handle_event(super::AppEvent::Idle);
        assert!(!app.busy);
    }

    #[test]
    fn cancellation_events_are_not_rendered_as_errors() {
        let mut app = App::new();
        app.cancellation_requested = true;
        app.handle_rx4_event(rx4::agent::Event::Error("request cancelled".to_string()));
        assert!(app.messages.is_empty());

        app.handle_rx4_event(rx4::agent::Event::Error("provider unavailable".to_string()));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn model_selector_and_effort_cycle_update_state() {
        let mut app = App::new();
        app.providers = vec![provider("openai")];
        app.open_model_selector();
        assert!(app.model_choice.is_some());
        assert!(!app.model_choices.is_empty());
        assert!(app
            .filtered_models()
            .iter()
            .all(|model| model.provider == "openai"));
        app.move_model_choice(1);
        app.choose_model();
        assert!(app.model_choice.is_none());
        assert!(app.model.starts_with("gpt-"));

        app.cycle_effort();
        assert_eq!(app.effort, "xhigh");
        app.cycle_effort();
        assert_eq!(app.effort, "low");
    }

    #[test]
    fn gpt_5_catalog_models_use_their_context_window() {
        let mut app = App::new();
        app.providers = vec![provider("openai-codex")];
        app.open_model_selector();
        for model in ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert!(app.model_choices.iter().any(|choice| choice.id == model));
            assert_eq!(context_window_for_model(model), GPT_5_CONTEXT_WINDOW);
        }

        app.context_tokens = 525_000;
        app.set_model("gpt-5.5".to_string());
        assert_eq!(app.context_window, GPT_5_CONTEXT_WINDOW);
        assert_eq!(app.context_pct, 50);
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

    #[test]
    fn paste_collapses_large_content_and_keeps_short_multiline_content() {
        let mut app = App::new();
        app.insert_newline();
        assert_eq!(app.input, "\n");
        app.clear_input();
        app.paste("first\r\nsecond");
        assert_eq!(app.input, "first\nsecond");

        let large = (1..=11)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.clear_input();
        app.paste(&large);
        assert_eq!(app.input, "[paste #1]");
        assert_eq!(app.expanded_input(), large);
    }

    #[test]
    fn embedded_template_keeps_status_rows_adjacent_and_flush() {
        use crepuscularity_tui::ratatui::backend::TestBackend;
        use crepuscularity_tui::ratatui::Terminal;

        let mut template = load_template(None).unwrap();
        App::new().update_template(&mut template);
        let mut terminal = Terminal::new(TestBackend::new(200, 9)).unwrap();
        terminal
            .draw(|frame| template.draw(frame, frame.area()).unwrap())
            .unwrap();
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let rows = buffer
            .content()
            .chunks(width)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let cost_row = rows.iter().position(|row| row.contains("$0.000")).unwrap();
        let model_row = rows
            .iter()
            .position(|row| row.contains("no-model · high"))
            .unwrap();
        assert_eq!(cost_row + 1, model_row);
        assert_eq!(rows[cost_row].find('$'), Some(0));
    }
}
