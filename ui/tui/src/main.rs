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
        self.messages.push(("user".to_string(), text.clone()));
        self.input.clear();
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
