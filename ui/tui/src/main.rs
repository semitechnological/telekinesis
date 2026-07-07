use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crepuscularity_tui::{HotTemplate, TemplateContext, TemplateValue};
use crepuscularity_tui::ratatui::prelude::*;
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
    next_id: u64,
}

impl IpcClient {
    fn connect(path: &PathBuf) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(path)?;
        Ok(Self { stream, next_id: 1 })
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
        let mut reader = BufReader::new(&self.stream);
        let mut response_line = String::new();
        reader.read_line(&mut response_line)?;
        let response: serde_json::Value = serde_json::from_str(&response_line)?;
        if let Some(err) = response.get("error") {
            anyhow::bail!("IPC error: {}", err);
        }
        Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }
}

struct App {
    ipc: IpcClient,
    input: String,
    messages: Vec<(String, String)>,
    model: String,
    tools_count: usize,
    plugins_count: usize,
    scroll: usize,
    status: String,
}

impl App {
    fn new(ipc: IpcClient) -> Self {
        Self {
            ipc,
            input: String::new(),
            messages: Vec::new(),
            model: "unknown".to_string(),
            tools_count: 0,
            plugins_count: 0,
            scroll: 0,
            status: "Connected".to_string(),
        }
    }

    fn refresh_state(&mut self) {
        match self.ipc.call("state", None) {
            Ok(state) => {
                self.model = state.get("model").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                self.tools_count = state.get("tools").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                self.plugins_count = state.get("plugins").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
        match self.ipc.call("messages", None) {
            Ok(msgs) => {
                if let Some(arr) = msgs.as_array() {
                    self.messages.clear();
                    for msg in arr {
                        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?").to_string();
                        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        self.messages.push((role, content));
                    }
                }
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    fn send_prompt(&mut self) {
        if self.input.is_empty() {
            return;
        }
        let text = self.input.clone();
        self.messages.push(("user".to_string(), text.clone()));
        self.input.clear();
        match self.ipc.call("prompt", Some(serde_json::json!({"text": text}))) {
            Ok(_) => {
                self.refresh_state();
                self.status = "Sent".to_string();
            }
            Err(e) => self.status = format!("Error: {e}"),
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
        app.refresh_state();

        let tpl = hot.template_mut();
        tpl.set("model", app.model.as_str());
        tpl.set("tools_count", app.tools_count as i64);
        tpl.set("plugins_count", app.plugins_count as i64);
        tpl.set("status", app.status.as_str());
        tpl.set("input", app.input.as_str());
        tpl.set("message_count", app.messages.len() as i64);

        let msgs: Vec<TemplateContext> = app.messages.iter().map(|(role, content)| {
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
