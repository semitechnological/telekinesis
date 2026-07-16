use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Local;
use crepuscularity_tui::ratatui::prelude::*;
use crepuscularity_tui::{HotTemplate, TemplateContext, TemplateValue};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

const SPINNER_FRAMES: [&str; 10] = [
    "\u{280B}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283C}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280F}",
];

const SLASH_COMMANDS: &[&str] = &[
    "/model",
    "/tools",
    "/plugins",
    "/scope",
    "/permissions",
    "/compact",
    "/sessions",
    "/new",
    "/save",
    "/load",
    "/fork",
    "/merge",
    "/tree",
    "/clear",
    "/help",
    "/theme",
    "/cost",
    "/context",
    "/doctor",
    "/quit",
];

const THEMES: &[&str] = &[
    "auto",
    "dark",
    "light",
    "dracula",
    "nord",
    "gruvbox",
    "tokyo-night",
    "catppuccin",
];

const CONTEXT_WINDOW: usize = 128_000;
const CHARS_PER_TOKEN: usize = 3;
const MAX_HISTORY: usize = 100;

fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("TELEKINESIS_SOCKET") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".telekinesis/telekinesis.sock")
}

fn history_path() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        return home.join(".telekinesis/input_history.json");
    }
    PathBuf::from(".telekinesis/input_history.json")
}

fn load_history() -> Vec<String> {
    match std::fs::read_to_string(history_path()) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_history(history: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let trimmed: Vec<&String> = history.iter().take(MAX_HISTORY).collect();
    let _ = std::fs::write(path, serde_json::to_string(&trimmed).unwrap_or_default());
}

fn detect_theme() -> String {
    std::env::var("TELEKINESIS_THEME")
        .or_else(|_| std::env::var("ZERO_THEME"))
        .unwrap_or_else(|_| "auto".to_string())
}

fn now_ts() -> String {
    Local::now().format("%H:%M:%S").to_string()
}

fn estimate_tokens(text: &str) -> usize {
    text.chars().count() / CHARS_PER_TOKEN
}

fn context_color(pct: usize) -> &'static str {
    if pct >= 80 {
        "red-400"
    } else if pct >= 50 {
        "amber-400"
    } else {
        "green-400"
    }
}

fn context_bar(pct: usize) -> String {
    let filled = pct / 10;
    let empty = 10 - filled;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
}

fn role_color(role: &str) -> &'static str {
    match role {
        "user" => "blue-400",
        "assistant" => "green-400",
        "system" => "zinc-400",
        s if s.starts_with("tool") => "amber-400",
        _ => "zinc-100",
    }
}

fn role_label(role: &str) -> String {
    if let Some(tool) = role.strip_prefix("tool:") {
        return format!("[tool:{tool}]");
    }
    match role {
        "user" => "user".to_string(),
        "assistant" => "assistant".to_string(),
        "system" => "system".to_string(),
        other => other.to_string(),
    }
}

fn is_tool_role(role: &str) -> bool {
    role.starts_with("tool")
}

fn is_diff_content(content: &str) -> bool {
    content.contains("--- ") || content.contains("+++ ") || content.contains("@@ ")
}

fn line_color(text: &str, is_diff: bool) -> &'static str {
    if is_diff {
        if text.starts_with('+') {
            return "green-400";
        }
        if text.starts_with('-') {
            return "red-400";
        }
        return "zinc-400";
    }
    "zinc-100"
}

fn split_lines(content: &str) -> Vec<(String, &'static str)> {
    let is_diff = is_diff_content(content);
    content
        .lines()
        .map(|line| (line.to_string(), line_color(line, is_diff)))
        .collect()
}

fn spinner_frame(start: Instant) -> String {
    let elapsed = start.elapsed().as_millis();
    let idx = ((elapsed / 100) % SPINNER_FRAMES.len() as u128) as usize;
    SPINNER_FRAMES[idx].to_string()
}

fn blink_cursor(start: Instant) -> String {
    let elapsed = start.elapsed().as_millis();
    if (elapsed / 500).is_multiple_of(2) {
        "_".to_string()
    } else {
        " ".to_string()
    }
}

struct IpcClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
    pending_events: Vec<serde_json::Value>,
}

impl IpcClient {
    fn connect(path: &PathBuf) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(path)?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            next_id: 1,
            pending_events: Vec::new(),
        })
    }

    fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;

        loop {
            let mut response_line = String::new();
            let n = self.reader.read_line(&mut response_line)?;
            if n == 0 {
                anyhow::bail!("connection closed");
            }
            let response: serde_json::Value = serde_json::from_str(&response_line)?;
            if response.get("method").and_then(|v| v.as_str()) == Some("event") {
                if let Some(params) = response.get("params").cloned() {
                    self.pending_events.push(params);
                }
                continue;
            }
            if response.get("id") == Some(&serde_json::Value::from(id)) {
                if let Some(err) = response.get("error") {
                    anyhow::bail!("IPC error: {}", err);
                }
                return Ok(response
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
        }
    }

    fn drain_events(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.pending_events)
    }
}

struct App {
    ipc: IpcClient,
    input: String,
    messages: Vec<(String, String)>,
    streaming_role: Option<String>,
    streaming_content: String,
    model: String,
    tools_count: usize,
    plugins_count: usize,
    scroll: usize,
    status: String,
    busy: bool,
    session_id: String,
    permission_mode: String,
    scope: String,
    sidebar_open: bool,
    sidebar_scroll: usize,
    input_history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    autocomplete_open: bool,
    autocomplete_index: usize,
    autocomplete_items: Vec<String>,
    permission_prompt_open: bool,
    permission_tool: String,
    permission_args: String,
    permission_request_id: Option<String>,
    input_tokens: usize,
    output_tokens: usize,
    cost: f64,
    turn_start: Option<Instant>,
    spinner_start: Instant,
    cursor_start: Instant,
    theme: String,
    sessions: Vec<(String, bool)>,
    tools_list: Vec<(String, String)>,
    plugins_list: Vec<String>,
    always_allowed: Vec<String>,
}

impl App {
    fn new(ipc: IpcClient) -> Self {
        Self {
            ipc,
            input: String::new(),
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            model: "unknown".to_string(),
            tools_count: 0,
            plugins_count: 0,
            scroll: 0,
            status: "Connected".to_string(),
            busy: false,
            session_id: "default".to_string(),
            permission_mode: "workspace_write".to_string(),
            scope: "coding".to_string(),
            sidebar_open: false,
            sidebar_scroll: 0,
            input_history: load_history(),
            history_index: None,
            history_draft: String::new(),
            autocomplete_open: false,
            autocomplete_index: 0,
            autocomplete_items: Vec::new(),
            permission_prompt_open: false,
            permission_tool: String::new(),
            permission_args: String::new(),
            permission_request_id: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0.0,
            turn_start: None,
            spinner_start: Instant::now(),
            cursor_start: Instant::now(),
            theme: detect_theme(),
            sessions: Vec::new(),
            tools_list: Vec::new(),
            plugins_list: Vec::new(),
            always_allowed: Vec::new(),
        }
    }

    fn apply_event(&mut self, event: &serde_json::Value) {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                let role = event
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant")
                    .to_string();
                self.streaming_role = Some(role);
                self.streaming_content.clear();
                self.busy = true;
                self.turn_start = Some(Instant::now());
            }
            "message_update" => {
                if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                    self.streaming_content.push_str(delta);
                }
            }
            "message_end" => {
                let role = event
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant")
                    .to_string();
                let content = event
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.messages.push((role, content));
                self.streaming_role = None;
                self.streaming_content.clear();
            }
            "tool_call" | "tool_execution_start" => {
                let name = event
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                if self.always_allowed.iter().any(|t| t == name) {
                    let _ = self
                        .ipc
                        .call("approve_tool", Some(serde_json::json!({"tool": name})));
                } else if event.get("requires_approval").and_then(|v| v.as_bool()) == Some(true) {
                    self.permission_prompt_open = true;
                    self.permission_tool = name.to_string();
                    self.permission_args = event
                        .get("args")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.permission_request_id = event
                        .get("request_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                self.streaming_role = Some(format!("tool:{name}"));
                self.streaming_content.clear();
                self.busy = true;
            }
            "tool_execution_update" => {
                if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                    self.streaming_content.push_str(delta);
                }
            }
            "tool_execution_end" | "tool_result" => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages
                        .push((role, std::mem::take(&mut self.streaming_content)));
                }
            }
            "agent_end" | "turn_end" => {
                self.busy = false;
                self.turn_start = None;
                self.refresh_usage();
            }
            _ => {}
        }
    }

    fn refresh_state(&mut self) {
        match self.ipc.call("state", None) {
            Ok(state) => {
                self.model = state
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                self.tools_count = state
                    .get("tools")
                    .and_then(|v: &serde_json::Value| v.as_u64())
                    .unwrap_or(0) as usize;
                self.plugins_count = state
                    .get("plugins")
                    .and_then(|v: &serde_json::Value| v.as_u64())
                    .unwrap_or(0) as usize;
                if let Some(id) = state.get("session_id").and_then(|v| v.as_str()) {
                    self.session_id = id.to_string();
                }
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
        match self.ipc.call("messages", None) {
            Ok(msgs) => {
                if let Some(arr) = msgs.as_array() {
                    self.messages.clear();
                    for msg in arr {
                        let role = msg
                            .get("role")
                            .and_then(|v: &serde_json::Value| v.as_str())
                            .unwrap_or("?")
                            .to_string();
                        let content = msg
                            .get("content")
                            .and_then(|v: &serde_json::Value| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.messages.push((role, content));
                    }
                }
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
        self.refresh_sidebar_data();
        self.refresh_usage();
    }

    fn refresh_sidebar_data(&mut self) {
        if let Ok(sessions) = self.ipc.call("session_list", None) {
            self.sessions.clear();
            if let Some(arr) = sessions.as_array() {
                for s in arr {
                    let id = s
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let active = s.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    self.sessions.push((id, active));
                }
            }
        }
        if let Ok(tools) = self.ipc.call("tools", None) {
            self.tools_list.clear();
            if let Some(arr) = tools.as_array() {
                for t in arr {
                    let name = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let desc = t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    self.tools_list.push((name, desc));
                }
            }
        }
        if let Ok(plugins) = self.ipc.call("plugins", None) {
            self.plugins_list.clear();
            if let Some(arr) = plugins.as_array() {
                for p in arr {
                    let id = p
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    self.plugins_list.push(id);
                }
            }
        }
    }

    fn refresh_usage(&mut self) {
        match self.ipc.call("usage", None) {
            Ok(usage) => {
                self.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                self.output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                self.cost = usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
            }
            Err(_) => {
                let total: usize = self.messages.iter().map(|(_, c)| estimate_tokens(c)).sum();
                self.input_tokens = total;
            }
        }
    }

    fn context_usage(&self) -> (usize, usize) {
        let used = self.input_tokens + self.output_tokens;
        let pct = if self.input_tokens > 0 {
            (used * 100) / CONTEXT_WINDOW
        } else {
            let est: usize = self.messages.iter().map(|(_, c)| estimate_tokens(c)).sum();
            (est * 100) / CONTEXT_WINDOW
        };
        (used, pct.min(100))
    }

    fn drain_events(&mut self) {
        for event in self.ipc.drain_events() {
            self.apply_event(&event);
        }
    }

    fn update_autocomplete(&mut self) {
        if self.input.starts_with('/') && !self.input.contains(' ') {
            self.autocomplete_items = SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.starts_with(&self.input))
                .map(|s| s.to_string())
                .collect();
            self.autocomplete_open = !self.autocomplete_items.is_empty();
            if self.autocomplete_index >= self.autocomplete_items.len() {
                self.autocomplete_index = 0;
            }
        } else {
            self.autocomplete_open = false;
            self.autocomplete_items.clear();
            self.autocomplete_index = 0;
        }
    }

    fn autocomplete_select(&mut self) {
        if let Some(cmd) = self.autocomplete_items.get(self.autocomplete_index) {
            self.input = cmd.to_string();
            self.autocomplete_open = false;
            self.autocomplete_items.clear();
            self.autocomplete_index = 0;
        }
    }

    fn autocomplete_move(&mut self, delta: i32) {
        if self.autocomplete_items.is_empty() {
            return;
        }
        let len = self.autocomplete_items.len() as i32;
        let mut idx = self.autocomplete_index as i32 + delta;
        if idx < 0 {
            idx = len - 1;
        }
        if idx >= len {
            idx = 0;
        }
        self.autocomplete_index = idx as usize;
    }

    fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                self.history_draft = self.input.clone();
                self.history_index = Some(self.input_history.len() - 1);
            }
            Some(0) => return,
            Some(idx) => {
                self.history_index = Some(idx - 1);
            }
        }
        if let Some(idx) = self.history_index {
            self.input = self.input_history[idx].clone();
        }
    }

    fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(idx) => {
                if idx + 1 >= self.input_history.len() {
                    self.history_index = None;
                    self.input = self.history_draft.clone();
                    self.history_draft.clear();
                } else {
                    self.history_index = Some(idx + 1);
                    self.input = self.input_history[idx + 1].clone();
                }
            }
        }
    }

    fn history_add(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.input_history.last().map(|s| s.as_str()) != Some(text) {
            self.input_history.push(text.to_string());
            if self.input_history.len() > MAX_HISTORY {
                let start = self.input_history.len() - MAX_HISTORY;
                self.input_history = self.input_history[start..].to_vec();
            }
        }
        self.history_index = None;
        self.history_draft.clear();
    }

    fn respond_permission(&mut self, allow: bool, always: bool) {
        if let Some(req_id) = self.permission_request_id.take() {
            let method = if allow { "approve_tool" } else { "deny_tool" };
            let _ = self
                .ipc
                .call(method, Some(serde_json::json!({"request_id": req_id})));
            if always && allow && !self.always_allowed.contains(&self.permission_tool) {
                self.always_allowed.push(self.permission_tool.clone());
            }
        }
        self.permission_prompt_open = false;
        self.permission_tool.clear();
        self.permission_args.clear();
    }

    fn send_prompt(&mut self) {
        if self.input.is_empty() || self.busy {
            return;
        }
        let text = self.input.clone();
        self.input.clear();
        self.history_add(&text);
        save_history(&self.input_history);

        if text.starts_with('/') {
            self.handle_command(&text);
            return;
        }

        self.messages.push(("user".to_string(), text.clone()));
        self.busy = true;
        self.status = "Sending...".to_string();
        self.turn_start = Some(Instant::now());
        match self
            .ipc
            .call("prompt", Some(serde_json::json!({"text": text})))
        {
            Ok(_) => {
                self.drain_events();
                self.refresh_state();
                self.status = "Ready".to_string();
            }
            Err(e) => {
                self.status = format!("Error: {e}");
                self.busy = false;
            }
        }
    }

    fn handle_command(&mut self, cmd: &str) {
        let parts: Vec<&str> = cmd[1..].splitn(2, ' ').collect();
        let command = parts[0];
        let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

        match command {
            "model" => {
                if arg.is_empty() {
                    self.status = format!("Current model: {}", self.model);
                } else {
                    match self
                        .ipc
                        .call("set_model", Some(serde_json::json!({"model": arg})))
                    {
                        Ok(_) => {
                            self.model = arg.to_string();
                            self.status = format!("Model set to: {arg}");
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            "tools" => match self.ipc.call("tools", None) {
                Ok(tools) => {
                    let mut list = String::new();
                    if let Some(arr) = tools.as_array() {
                        for t in arr {
                            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                            let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                            list.push_str(&format!("  {name}: {desc}\n"));
                        }
                    }
                    self.messages
                        .push(("system".to_string(), format!("Available tools:\n{list}")));
                    self.status = "Tools listed".to_string();
                }
                Err(e) => self.status = format!("Error: {e}"),
            },
            "sessions" => match self.ipc.call("session_list", None) {
                Ok(sessions) => {
                    let mut list = String::new();
                    if let Some(arr) = sessions.as_array() {
                        if arr.is_empty() {
                            list.push_str("(no saved sessions)");
                        } else {
                            for s in arr {
                                let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                list.push_str(&format!("  {id}\n"));
                            }
                        }
                    }
                    self.messages
                        .push(("system".to_string(), format!("Sessions:\n{list}")));
                    self.status = "Sessions listed".to_string();
                }
                Err(e) => self.status = format!("Error: {e}"),
            },
            "new" => {
                let name = if arg.is_empty() { "new" } else { arg };
                match self
                    .ipc
                    .call("session_create", Some(serde_json::json!({"name": name})))
                {
                    Ok(result) => {
                        let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        self.messages.clear();
                        self.session_id = id.to_string();
                        self.status = format!("New session: {id}");
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "save" => match self.ipc.call("session_save", None) {
                Ok(_) => self.status = "Session saved".to_string(),
                Err(e) => self.status = format!("Error: {e}"),
            },
            "load" => {
                if arg.is_empty() {
                    self.status = "Usage: /load <session-id>".to_string();
                } else {
                    match self
                        .ipc
                        .call("session_load", Some(serde_json::json!({"id": arg})))
                    {
                        Ok(result) => {
                            let count =
                                result.get("messages").and_then(|v| v.as_u64()).unwrap_or(0);
                            self.refresh_state();
                            self.status = format!("Loaded session: {arg} ({count} messages)");
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            "fork" => {
                let from_entry: Option<serde_json::Value> = if arg.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({"from_entry": arg.parse::<u64>().unwrap_or(0)}))
                };
                match self.ipc.call("session_fork", from_entry) {
                    Ok(result) => {
                        let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let msgs = result.get("messages").and_then(|v| v.as_u64()).unwrap_or(0);
                        self.refresh_state();
                        self.status = format!("Forked session: {id} ({msgs} messages)");
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "merge" => {
                if arg.is_empty() {
                    self.status = "Usage: /merge <session-id>".to_string();
                } else {
                    match self
                        .ipc
                        .call("session_merge", Some(serde_json::json!({"id": arg})))
                    {
                        Ok(result) => {
                            let merged = result.get("merged").and_then(|v| v.as_u64()).unwrap_or(0);
                            let total = result.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                            self.refresh_state();
                            self.status =
                                format!("Merged {merged} entries from {arg} ({total} total)");
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            "clear" => match self.ipc.call("session_clear", None) {
                Ok(_) => {
                    self.messages.clear();
                    self.status = "Cleared".to_string();
                }
                Err(e) => self.status = format!("Error: {e}"),
            },
            "tree" => {
                let params = if arg.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({"session_id": arg}))
                };
                match self.ipc.call("session_tree", params) {
                    Ok(entries) => {
                        let mut tree = String::new();
                        if let Some(arr) = entries.as_array() {
                            struct EntryInfo {
                                id: u64,
                                parent_id: Option<u64>,
                                role: String,
                                preview: String,
                            }
                            let mut items: Vec<EntryInfo> = Vec::new();
                            for entry in arr {
                                let id = entry["id"].as_u64().unwrap_or(0);
                                let parent_id = entry["parent_id"].as_u64();
                                let role = entry["role"].as_str().unwrap_or("?").to_string();
                                let content = entry["content"].as_str().unwrap_or("").to_string();
                                let preview: String = content.chars().take(60).collect();
                                items.push(EntryInfo {
                                    id,
                                    parent_id,
                                    role,
                                    preview,
                                });
                            }

                            tree.push_str("Session tree:\n");
                            let roots: Vec<&EntryInfo> =
                                items.iter().filter(|e| e.parent_id.is_none()).collect();
                            for root in &roots {
                                let role_char = match root.role.as_str() {
                                    "user" => "U",
                                    "assistant" => "A",
                                    "system" => "S",
                                    "tool" => "T",
                                    _ => "?",
                                };
                                tree.push_str(&format!(
                                    "{role_char} [{}] {}\n",
                                    root.id, root.preview
                                ));
                                fn show_children(
                                    tree: &mut String,
                                    items: &[EntryInfo],
                                    parent_id: u64,
                                    depth: usize,
                                ) {
                                    for child in
                                        items.iter().filter(|e| e.parent_id == Some(parent_id))
                                    {
                                        let indent = "  ".repeat(depth);
                                        let role_char = match child.role.as_str() {
                                            "user" => "U",
                                            "assistant" => "A",
                                            "system" => "S",
                                            "tool" => "T",
                                            _ => "?",
                                        };
                                        tree.push_str(&format!(
                                            "{indent}+-- {role_char} [{}] {}\n",
                                            child.id, child.preview
                                        ));
                                        show_children(tree, items, child.id, depth + 1);
                                    }
                                }
                                show_children(&mut tree, &items, root.id, 1);
                            }

                            if tree.lines().count() <= 1 {
                                tree.push_str("  (empty session)\n");
                            }
                        } else {
                            tree.push_str("No session data available.\n");
                        }
                        self.messages.push(("system".to_string(), tree));
                        self.status = "Session tree shown".to_string();
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "scope" => {
                if arg.is_empty() {
                    self.status = "Usage: /scope coding|research|plan|ask|computer_use".to_string();
                } else {
                    match self
                        .ipc
                        .call("set_scope", Some(serde_json::json!({"scope": arg})))
                    {
                        Ok(_) => {
                            self.scope = arg.to_string();
                            self.status = format!("Scope set to: {arg}");
                        }
                        Err(_) => {
                            self.status = format!(
                                "scope request: {arg} (server apply via rotary Agent.setScope)"
                            );
                        }
                    }
                    self.messages.push((
                        "system".to_string(),
                        format!("Scope `{arg}`: coding tools/FS; research read-ish; plan no writes; ask tools-off; computer_use rs_peekaboo."),
                    ));
                }
            }
            "permissions" | "perms" => {
                let mode = if arg.is_empty() {
                    "workspace_write"
                } else {
                    arg
                };
                match self
                    .ipc
                    .call("set_permissions", Some(serde_json::json!({"mode": mode})))
                {
                    Ok(_) => {
                        self.permission_mode = mode.to_string();
                        self.status = format!("Permissions set to: {mode}");
                    }
                    Err(_) => {
                        self.status = format!("permissions: {mode}");
                    }
                }
                self.messages.push((
                    "system".to_string(),
                    format!("Permission modes: full_access | read_only | workspace_write | deny_all\nSelected: {mode}"),
                ));
            }
            "compact" => match self.ipc.call("compact", None) {
                Ok(_) => self.status = "Context compacted".to_string(),
                Err(_) => {
                    self.messages.push((
                            "system".to_string(),
                            "Compact: hosts call rotary Agent.compact over IPC when exposed; auto-compact also runs past threshold.".to_string(),
                        ));
                    self.status = "compact noted".to_string();
                }
            },
            "plugins" => match self.ipc.call("plugins", None) {
                Ok(plugins) => {
                    let mut list = String::new();
                    if let Some(arr) = plugins.as_array() {
                        for p in arr {
                            let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                            list.push_str(&format!("  {id}\n"));
                        }
                        if arr.is_empty() {
                            list.push_str("(none loaded)");
                        }
                    }
                    self.messages
                        .push(("system".to_string(), format!("Plugins:\n{list}")));
                    self.status = "Plugins listed".to_string();
                }
                Err(e) => self.status = format!("Error: {e}"),
            },
            "theme" => {
                if arg.is_empty() {
                    self.status = format!(
                        "Current theme: {} (available: {})",
                        self.theme,
                        THEMES.join(", ")
                    );
                } else if THEMES.contains(&arg) {
                    self.theme = arg.to_string();
                    self.status = format!("Theme set to: {arg}");
                } else {
                    self.status = format!("Unknown theme: {arg}. Available: {}", THEMES.join(", "));
                }
            }
            "cost" => {
                self.refresh_usage();
                self.messages.push((
                    "system".to_string(),
                    format!(
                        "Cost: ${:.4}\nInput tokens: {}\nOutput tokens: {}",
                        self.cost, self.input_tokens, self.output_tokens
                    ),
                ));
                self.status = format!("Cost: ${:.4}", self.cost);
            }
            "context" => {
                let (used, pct) = self.context_usage();
                match self.ipc.call("context", None) {
                    Ok(ctx) => {
                        let real_used =
                            ctx.get("used")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(used as u64) as usize;
                        let real_pct = ctx
                            .get("percent")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(pct as u64) as usize;
                        self.messages.push((
                            "system".to_string(),
                            format!("Context: {real_used} / {CONTEXT_WINDOW} tokens ({real_pct}%)"),
                        ));
                    }
                    Err(_) => {
                        self.messages.push((
                            "system".to_string(),
                            format!(
                                "Context: {used} / {CONTEXT_WINDOW} tokens ({pct}%) [estimated]"
                            ),
                        ));
                    }
                }
                self.status = "Context shown".to_string();
            }
            "doctor" => {
                let mut report = String::new();
                report.push_str(&format!("  model: {}\n", self.model));
                report.push_str(&format!("  session: {}\n", self.session_id));
                report.push_str(&format!("  tools: {}\n", self.tools_count));
                report.push_str(&format!("  plugins: {}\n", self.plugins_count));
                report.push_str(&format!("  scope: {}\n", self.scope));
                report.push_str(&format!("  permissions: {}\n", self.permission_mode));
                report.push_str(&format!("  theme: {}\n", self.theme));
                let (used, pct) = self.context_usage();
                report.push_str(&format!("  context: {used}/{CONTEXT_WINDOW} ({pct}%)\n"));
                report.push_str(&format!("  cost: ${:.4}\n", self.cost));
                report.push_str(&format!(
                    "  history: {} entries\n",
                    self.input_history.len()
                ));
                self.messages.push((
                    "system".to_string(),
                    format!("Telekinesis health:\n{report}"),
                ));
                self.status = "Doctor report shown".to_string();
            }
            "quit" => {
                self.status = "quitting".to_string();
            }
            "help" => {
                let help = "Commands:\n\
  /model <name>     Set or show model\n\
  /tools            List tools\n\
  /plugins          List plugins\n\
  /scope <name>     coding|research|plan|ask|computer_use\n\
  /permissions [m]  full_access|read_only|workspace_write|deny_all\n\
  /compact          Compact context\n\
  /sessions         List sessions\n\
  /new [name]       New session\n\
  /save             Save session\n\
  /load <id>        Load session\n\
  /fork [entry]     Fork session\n\
  /merge <id>       Merge session\n\
  /tree [id]        Session tree\n\
  /clear            Clear conversation\n\
  /theme [name]     Set or show theme\n\
  /cost             Show cost/usage\n\
  /context          Show context usage\n\
  /doctor           Health check\n\
  /quit             Exit\n\
  /help             This help\n\
\n\
Shortcuts:\n\
  Ctrl+B  Toggle sidebar\n\
  Ctrl+C  Cancel turn or exit\n\
  Ctrl+L  Clear screen\n\
  Ctrl+R  Search history\n\
  Shift+Tab  Cycle permission mode\n\
  Up/Down  History (when input empty) or scroll\n\
  Page Up/Down  Scroll messages\n\
  Home/End  Jump to top/bottom";
                self.messages.push(("system".to_string(), help.to_string()));
                self.status = "Help shown".to_string();
            }
            _ => {
                self.status = format!("Unknown command: /{command}. Try /help");
            }
        }
    }
}

fn build_message_ctx(role: &str, content: &str, is_streaming: bool, ts: &str) -> TemplateContext {
    let mut ctx = TemplateContext::new();
    let tool = is_tool_role(role);
    let diff = is_diff_content(content);
    ctx.set("is_tool", tool);
    ctx.set("is_diff", diff);
    ctx.set("is_streaming", is_streaming);
    ctx.set("role_color", role_color(role));
    ctx.set("role_label", role_label(role));
    ctx.set("timestamp", ts);
    let lines: Vec<TemplateContext> = split_lines(content)
        .into_iter()
        .map(|(text, color)| {
            let mut lc = TemplateContext::new();
            lc.set("text", text.as_str());
            lc.set("color", color);
            lc
        })
        .collect();
    ctx.set("lines", TemplateValue::List(lines));
    ctx
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "login" {
        return run_login(args.get(2).map(|s| s.as_str()));
    }

    let sock = socket_path();

    // If the socket doesn't exist, auto-spawn `telekinesis serve` in the
    // background and wait for it to come up — like pi, `tk` just works.
    if !sock.exists() {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let data_dir = home.join(".telekinesis");
        let _ = std::fs::create_dir_all(&data_dir);

        // Try to find the `telekinesis` binary on PATH.
        let telekinesis_bin =
            std::env::var("TELEKINESIS_BIN").unwrap_or_else(|_| "telekinesis".to_string());

        let child = std::process::Command::new(&telekinesis_bin)
            .arg("serve")
            .arg(&sock)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        match child {
            Ok(mut child) => {
                // Wait up to 10s for the socket to appear.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while std::time::Instant::now() < deadline {
                    if sock.exists() {
                        break;
                    }
                    // Check if the process exited early (e.g. binary not found).
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        _ => std::thread::sleep(std::time::Duration::from_millis(200)),
                    }
                }
            }
            Err(_) => {
                // `telekinesis` binary not found — fall through to the
                // connection error which will tell the user what to do.
            }
        }
    }

    let ipc = IpcClient::connect(&sock).map_err(|e| {
        anyhow::anyhow!(
            "Cannot connect to Telekinesis at {}.\n\
             Run `telekinesis serve` in another terminal, or install the\n\
             `telekinesis` binary so `tk` can auto-start it.\n\
             Error: {}",
            sock.display(),
            e
        )
    })?;

    let mut app = App::new(ipc);
    app.refresh_state();

    let template_path = std::env::var("TELEKINESIS_TUI_TEMPLATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Try CARGO_MANIFEST_DIR (dev), then ~/.telekinesis, then /usr/local/share
            let candidates = [
                std::env::var("CARGO_MANIFEST_DIR")
                    .ok()
                    .map(|d| PathBuf::from(d).join("../shell.crepus")),
                dirs::home_dir().map(|h| h.join(".telekinesis/shell.crepus")),
                Some(PathBuf::from("/usr/local/share/telekinesis/shell.crepus")),
                Some(PathBuf::from("shell.crepus")),
            ];
            for p in candidates.iter().flatten() {
                if p.exists() {
                    return p.clone();
                }
            }
            PathBuf::from("shell.crepus")
        });

    let mut hot = HotTemplate::watch(&template_path).map_err(|e| anyhow::anyhow!(e))?;

    let mut terminal = crepuscularity_tui::ratatui::init();
    let result = run(&mut terminal, &mut hot, &mut app);
    crepuscularity_tui::ratatui::restore();
    save_history(&app.input_history);
    result
}

fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    hot: &mut HotTemplate,
    app: &mut App,
) -> anyhow::Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    loop {
        app.drain_events();

        let (_, context_pct) = app.context_usage();
        let elapsed = app
            .turn_start
            .map(|s| {
                let d = s.elapsed();
                format!("{}m{}s", d.as_secs() / 60, d.as_secs() % 60)
            })
            .unwrap_or_else(|| "0m0s".to_string());

        let tpl = hot.template_mut();
        tpl.set("model", app.model.as_str());
        tpl.set("tools_count", app.tools_count as i64);
        tpl.set("plugins_count", app.plugins_count as i64);
        tpl.set("status", app.status.as_str());
        tpl.set("input", app.input.as_str());
        tpl.set("input_len", app.input.chars().count() as i64);
        tpl.set("message_count", app.messages.len() as i64);
        tpl.set("busy", app.busy);
        tpl.set("session_id", app.session_id.as_str());
        tpl.set("permission_mode", app.permission_mode.as_str());
        tpl.set("scope", app.scope.as_str());
        tpl.set("sidebar_open", app.sidebar_open);
        tpl.set("sidebar_scroll", app.sidebar_scroll as i64);
        tpl.set("scroll", app.scroll as i64);
        tpl.set("context_pct", context_pct as i64);
        tpl.set("context_bar", context_bar(context_pct).as_str());
        tpl.set("context_color", context_color(context_pct));
        tpl.set("spinner", spinner_frame(app.spinner_start).as_str());
        tpl.set("cursor", blink_cursor(app.cursor_start).as_str());
        tpl.set("input_tokens", app.input_tokens as i64);
        tpl.set("output_tokens", app.output_tokens as i64);
        tpl.set("cost", format!("{:.4}", app.cost).as_str());
        tpl.set("elapsed", elapsed.as_str());
        tpl.set("theme", app.theme.as_str());
        tpl.set("autocomplete_open", app.autocomplete_open);
        tpl.set("autocomplete_scroll", app.autocomplete_index as i64);
        tpl.set("permission_prompt_open", app.permission_prompt_open);
        tpl.set("permission_tool", app.permission_tool.as_str());
        tpl.set("permission_args", app.permission_args.as_str());

        let sessions: Vec<TemplateContext> = app
            .sessions
            .iter()
            .map(|(id, active)| {
                let mut c = TemplateContext::new();
                c.set("session_id", id.as_str());
                c.set("active", *active);
                c
            })
            .collect();
        tpl.set("sessions", TemplateValue::List(sessions));

        let tools: Vec<TemplateContext> = app
            .tools_list
            .iter()
            .map(|(name, desc)| {
                let mut c = TemplateContext::new();
                c.set("tool_name", name.as_str());
                c.set("tool_desc", desc.as_str());
                c
            })
            .collect();
        tpl.set("tools", TemplateValue::List(tools));

        let plugins: Vec<TemplateContext> = app
            .plugins_list
            .iter()
            .map(|id| {
                let mut c = TemplateContext::new();
                c.set("plugin_id", id.as_str());
                c
            })
            .collect();
        tpl.set("plugins", TemplateValue::List(plugins));

        let autocomplete: Vec<TemplateContext> = app
            .autocomplete_items
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut c = TemplateContext::new();
                c.set("name", name.as_str());
                c.set("selected", i == app.autocomplete_index);
                c
            })
            .collect();
        tpl.set("autocomplete", TemplateValue::List(autocomplete));

        let mut all_messages: Vec<(String, String)> = app.messages.clone();
        if let Some(role) = &app.streaming_role {
            all_messages.push((role.clone(), app.streaming_content.clone()));
        }

        let ts = now_ts();
        let msgs: Vec<TemplateContext> = all_messages
            .iter()
            .map(|(role, content)| {
                let is_streaming = app.streaming_role.as_deref() == Some(role.as_str())
                    && app.streaming_content == *content;
                build_message_ctx(role, content, is_streaming, &ts)
            })
            .collect();
        tpl.set("messages", TemplateValue::List(msgs));

        terminal.draw(|frame| {
            let _ = hot.poll_and_draw_full(frame);
        })?;

        if crossterm::event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if app.permission_prompt_open {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.respond_permission(true, false);
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.respond_permission(false, false);
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            app.respond_permission(true, true);
                        }
                        _ => {}
                    }
                    continue;
                }

                if app.autocomplete_open {
                    match key.code {
                        KeyCode::Esc => {
                            app.autocomplete_open = false;
                            app.autocomplete_items.clear();
                        }
                        KeyCode::Up => {
                            app.autocomplete_move(-1);
                        }
                        KeyCode::Down => {
                            app.autocomplete_move(1);
                        }
                        KeyCode::Enter | KeyCode::Tab => {
                            app.autocomplete_select();
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                            app.update_autocomplete();
                        }
                        KeyCode::Char(c) => {
                            app.input.push(c);
                            app.update_autocomplete();
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.busy {
                            app.busy = false;
                            app.status = "Cancelled".to_string();
                        } else {
                            return Ok(());
                        }
                    }
                    KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.sidebar_open = !app.sidebar_open;
                        if app.sidebar_open {
                            app.refresh_sidebar_data();
                        }
                    }
                    KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.messages.clear();
                        app.scroll = 0;
                        app.status = "Screen cleared".to_string();
                    }
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.status = "History search: type to filter".to_string();
                    }
                    KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                        let modes = ["full_access", "read_only", "workspace_write", "deny_all"];
                        let idx = modes
                            .iter()
                            .position(|m| *m == app.permission_mode)
                            .unwrap_or(0);
                        let next = modes[(idx + 1) % modes.len()];
                        app.permission_mode = next.to_string();
                        let _ = app
                            .ipc
                            .call("set_permissions", Some(serde_json::json!({"mode": next})));
                        app.status = format!("Permission mode: {next}");
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        app.send_prompt();
                        app.update_autocomplete();
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                        app.update_autocomplete();
                    }
                    KeyCode::Char(c) => {
                        app.input.push(c);
                        app.update_autocomplete();
                    }
                    KeyCode::Up => {
                        if app.input.is_empty() {
                            app.history_prev();
                        } else if app.scroll > 0 {
                            app.scroll -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if app.input.is_empty() && app.history_index.is_some() {
                            app.history_next();
                        } else {
                            app.scroll += 1;
                        }
                    }
                    KeyCode::PageUp => {
                        app.scroll = app.scroll.saturating_sub(10);
                    }
                    KeyCode::PageDown => {
                        app.scroll += 10;
                    }
                    KeyCode::Home => {
                        app.scroll = 0;
                    }
                    KeyCode::End => {
                        app.scroll = app.messages.len();
                    }
                    _ => {}
                }
            }
        }

        if app.status == "quitting" {
            return Ok(());
        }
    }
}

fn run_login(provider_arg: Option<&str>) -> anyhow::Result<()> {
    use rs_ai_oauth::{fetch_models, start_oauth_flow, OAuthProvider};

    let provider = match provider_arg {
        Some(p) => match OAuthProvider::parse(p) {
            Some(parsed) => parsed,
            None => {
                eprintln!(
                    "unknown provider '{p}'. supported: grok, openai, claude, gemini, antigravity, copilot, kimi"
                );
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("usage: tk login <provider>");
            eprintln!(
                "providers: grok (xAI), openai (ChatGPT), claude (Anthropic), gemini (Google), antigravity (Google), copilot (GitHub), kimi (Moonshot)"
            );
            std::process::exit(1);
        }
    };

    eprintln!("starting OAuth flow for {}...", provider.name());
    eprintln!("opening browser — sign in and authorize, then return here");

    let tokens = start_oauth_flow(provider).map_err(|e| anyhow::anyhow!("oauth failed: {e}"))?;

    eprintln!("\noauth successful!");
    eprintln!("access token expires at: {}", tokens.expires_at);

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let data_dir = home.join(".telekinesis");
    let _ = std::fs::create_dir_all(&data_dir);
    let token_path = data_dir.join(format!("oauth_{}.json", provider.name()));

    let token_json = serde_json::json!({
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_at": tokens.expires_at,
        "provider": provider.name(),
    });

    std::fs::write(&token_path, serde_json::to_vec_pretty(&token_json)?)?;
    eprintln!("tokens saved to {}", token_path.display());

    eprintln!("\nfetching available models...");
    match fetch_models(provider, &tokens.access_token) {
        Ok(models) => {
            eprintln!("\n{} models available:", models.len());
            for m in &models {
                eprintln!("  {}", m.id);
            }
            let env_var = match provider {
                OAuthProvider::Xai => "XAI_API_KEY",
                OAuthProvider::ChatGpt => "OPENAI_API_KEY",
                OAuthProvider::Claude => "ANTHROPIC_API_KEY",
                OAuthProvider::Gemini => "GEMINI_API_KEY",
                OAuthProvider::Antigravity => "ANTIGRAVITY_API_KEY",
                OAuthProvider::Copilot => "GITHUB_COPILOT_TOKEN",
                OAuthProvider::Kimi => "KIMI_API_KEY",
            };
            eprintln!("\nset {env_var}=your-token to use with telekinesis");
        }
        Err(e) => {
            eprintln!("could not fetch models: {e}");
        }
    }

    Ok(())
}
