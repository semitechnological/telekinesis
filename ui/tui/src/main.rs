use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crepuscularity_tui::ratatui::prelude::*;
use crepuscularity_tui::{HotTemplate, TemplateContext, TemplateValue};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("TELEKINESIS_SOCKET") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".telekinesis/telekinesis.sock")
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

    fn call(&mut self, method: &str, params: Option<serde_json::Value>) -> anyhow::Result<serde_json::Value> {
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
                return Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null));
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
        }
    }

    fn apply_event(&mut self, event: &serde_json::Value) {
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                let role = event.get("role").and_then(|v| v.as_str()).unwrap_or("assistant").to_string();
                self.streaming_role = Some(role);
                self.streaming_content.clear();
                self.busy = true;
            }
            "message_update" => {
                if let Some(delta) = event.get("delta").and_then(|v| v.as_str()) {
                    self.streaming_content.push_str(delta);
                }
            }
            "message_end" => {
                let role = event.get("role").and_then(|v| v.as_str()).unwrap_or("assistant").to_string();
                let content = event.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                self.messages.push((role, content));
                self.streaming_role = None;
                self.streaming_content.clear();
            }
            "tool_call" | "tool_execution_start" => {
                let name = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("tool");
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
                    self.messages.push((role, std::mem::take(&mut self.streaming_content)));
                }
            }
            "agent_end" | "turn_end" => {
                self.busy = false;
            }
            _ => {}
        }
    }

    fn refresh_state(&mut self) {
        match self.ipc.call("state", None) {
            Ok(state) => {
                self.model = state.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                self.tools_count = state.get("tools").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
                self.plugins_count = state.get("plugins").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
        match self.ipc.call("messages", None) {
            Ok(msgs) => {
                if let Some(arr) = msgs.as_array() {
                    self.messages.clear();
                    for msg in arr {
                        let role = msg.get("role").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("?").to_string();
                        let content = msg.get("content").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
                        self.messages.push((role, content));
                    }
                }
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    fn drain_events(&mut self) {
        for event in self.ipc.drain_events() {
            self.apply_event(&event);
        }
    }

    fn send_prompt(&mut self) {
        if self.input.is_empty() || self.busy {
            return;
        }
        let text = self.input.clone();
        self.input.clear();

        if text.starts_with('/') {
            self.handle_command(&text);
            return;
        }

        self.messages.push(("user".to_string(), text.clone()));
        self.busy = true;
        self.status = "Sending...".to_string();
        match self.ipc.call("prompt", Some(serde_json::json!({"text": text}))) {
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
                    match self.ipc.call("set_model", Some(serde_json::json!({"model": arg}))) {
                        Ok(_) => {
                            self.model = arg.to_string();
                            self.status = format!("Model set to: {arg}");
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            "tools" => {
                match self.ipc.call("tools", None) {
                    Ok(tools) => {
                        let mut list = String::new();
                        if let Some(arr) = tools.as_array() {
                            for t in arr {
                                let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                let desc = t.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                list.push_str(&format!("  {name}: {desc}\n"));
                            }
                        }
                        self.messages.push(("system".to_string(), format!("Available tools:\n{list}")));
                        self.status = "Tools listed".to_string();
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "sessions" => {
                match self.ipc.call("session_list", None) {
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
                        self.messages.push(("system".to_string(), format!("Sessions:\n{list}")));
                        self.status = "Sessions listed".to_string();
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "new" => {
                let name = if arg.is_empty() { "new" } else { arg };
                match self.ipc.call("session_create", Some(serde_json::json!({"name": name}))) {
                    Ok(result) => {
                        let id = result.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        self.messages.clear();
                        self.status = format!("New session: {id}");
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "save" => {
                match self.ipc.call("session_save", None) {
                    Ok(_) => self.status = "Session saved".to_string(),
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
            "load" => {
                if arg.is_empty() {
                    self.status = "Usage: /load <session-id>".to_string();
                } else {
                    match self.ipc.call("session_load", Some(serde_json::json!({"id": arg}))) {
                        Ok(result) => {
                            let count = result.get("messages").and_then(|v| v.as_u64()).unwrap_or(0);
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
                    match self.ipc.call("session_merge", Some(serde_json::json!({"id": arg}))) {
                        Ok(result) => {
                            let merged = result.get("merged").and_then(|v| v.as_u64()).unwrap_or(0);
                            let total = result.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                            self.refresh_state();
                            self.status = format!("Merged {merged} entries from {arg} ({total} total)");
                        }
                        Err(e) => self.status = format!("Error: {e}"),
                    }
                }
            }
            "clear" => {
                match self.ipc.call("session_clear", None) {
                    Ok(_) => {
                        self.messages.clear();
                        self.status = "Cleared".to_string();
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
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
                            // Collect all entries with parent
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
                                items.push(EntryInfo { id, parent_id, role, preview });
                            }

                            // Compute depth by following parent chain (inline)
                            tree.push_str("Session tree:\n");
                            // Find roots (no parent), then their children
                            let roots: Vec<&EntryInfo> = items.iter().filter(|e| e.parent_id.is_none()).collect();
                            for root in &roots {
                                let role_char = match root.role.as_str() {
                                    "user" => "U", "assistant" => "A", "system" => "S", "tool" => "T", _ => "?",
                                };
                                tree.push_str(&format!("{role_char} [{}] {}\n", root.id, root.preview));
                                // Collect children and grandchildren
                                fn show_children(tree: &mut String, items: &[EntryInfo], parent_id: u64, depth: usize) {
                                    for child in items.iter().filter(|e| e.parent_id == Some(parent_id)) {
                                        let indent = "  ".repeat(depth);
                                        let role_char = match child.role.as_str() {
                                            "user" => "U", "assistant" => "A", "system" => "S", "tool" => "T", _ => "?",
                                        };
                                        tree.push_str(&format!("{indent}└─ {role_char} [{}] {}\n", child.id, child.preview));
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
            "help" => {
                let help = "Commands:\n  /model <name>  - Set or show model\n  /tools         - List available tools\n  /sessions      - List saved sessions\n  /new [name]    - Create new session\n  /save          - Save current session\n  /load <id>     - Load a session\n  /fork [entry]  - Fork session at entry\n  /merge <id>    - Merge session into current\n  /tree [id]     - Show session tree\n  /clear         - Clear conversation\n  /help          - Show this help";
                self.messages.push(("system".to_string(), help.to_string()));
                self.status = "Help shown".to_string();
            }
            _ => {
                self.status = format!("Unknown command: /{command}. Try /help");
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let sock = socket_path();
    let ipc = IpcClient::connect(&sock).map_err(|e| {
        anyhow::anyhow!("Cannot connect to Telekinesis at {}. Is `telekinesis serve` running?\n{}", sock.display(), e)
    })?;

    let mut app = App::new(ipc);
    app.refresh_state();

    let template_path = std::env::var("TELEKINESIS_TUI_TEMPLATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(manifest_dir).join("../shell.crepus")
        });

    let mut hot = HotTemplate::watch(&template_path).map_err(|e| anyhow::anyhow!(e))?;

    let mut terminal = crepuscularity_tui::ratatui::init();
    let result = run(&mut terminal, &mut hot, &mut app);
    crepuscularity_tui::ratatui::restore();
    result
}

fn run<B: Backend>(terminal: &mut Terminal<B>, hot: &mut HotTemplate, app: &mut App) -> anyhow::Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    loop {
        app.drain_events();

        let tpl = hot.template_mut();
        tpl.set("model", app.model.as_str());
        tpl.set("tools_count", app.tools_count as i64);
        tpl.set("plugins_count", app.plugins_count as i64);
        tpl.set("status", app.status.as_str());
        tpl.set("input", app.input.as_str());
        tpl.set("message_count", app.messages.len() as i64);
        tpl.set("busy", app.busy);

        let mut all_messages: Vec<(String, String)> = app.messages.clone();
        if let Some(role) = &app.streaming_role {
            all_messages.push((role.clone(), app.streaming_content.clone()));
        }

        let msgs: Vec<TemplateContext> = all_messages.iter().map(|(role, content)| {
            let mut ctx = TemplateContext::new();
            ctx.set("role", role.as_str());
            ctx.set("content", content.as_str());
            ctx
        }).collect();
        tpl.set("messages", TemplateValue::List(msgs));

        terminal.draw(|frame| {
            let _ = hot.poll_and_draw_full(frame);
        })?;

        if crossterm::event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = crossterm::event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        app.send_prompt();
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Char(c) => {
                        app.input.push(c);
                    }
                    KeyCode::Up => {
                        if app.scroll > 0 {
                            app.scroll -= 1;
                        }
                    }
                    KeyCode::Down => {
                        app.scroll += 1;
                    }
                    _ => {}
                }
            }
        }
    }
}
