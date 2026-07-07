use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crepuscularity_gpui::prelude::*;
use crepuscularity_macros::view_file;
use gpui::*;

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
            if response.get("method").and_then(|v: &serde_json::Value| v.as_str()) == Some("event") {
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

struct MessageItem {
    role: SharedString,
    content: SharedString,
    color: u32,
}

struct TelekinesisView {
    input: String,
    messages: Vec<MessageItem>,
    streaming_role: Option<String>,
    streaming_content: String,
    model: SharedString,
    tools_count: usize,
    plugins_count: usize,
    status: SharedString,
    busy: bool,
    ipc: Option<IpcClient>,
}

impl TelekinesisView {
    fn new(_cx: &mut Context<Self>) -> Self {
        let ipc = IpcClient::connect(&socket_path()).ok();
        let mut view = Self {
            input: String::new(),
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            model: "disconnected".into(),
            tools_count: 0,
            plugins_count: 0,
            status: "Connecting...".into(),
            busy: false,
            ipc,
        };
        view.refresh_state();
        view
    }

    fn role_color(role: &str) -> u32 {
        match role {
            "user" => 0x818cf8,
            "assistant" => 0x34d399,
            "system" => 0xfbbf24,
            _ if role.starts_with("tool:") => 0x38bdf8,
            _ => 0xa1a1aa,
        }
    }

    fn apply_event(&mut self, event: &serde_json::Value) {
        let event_type = event.get("type").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                let role = event.get("role").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("assistant").to_string();
                self.streaming_role = Some(role);
                self.streaming_content.clear();
                self.busy = true;
            }
            "message_update" => {
                if let Some(delta) = event.get("delta").and_then(|v: &serde_json::Value| v.as_str()) {
                    self.streaming_content.push_str(delta);
                }
            }
            "message_end" => {
                let role = event.get("role").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("assistant").to_string();
                let color = Self::role_color(&role);
                let content = event.get("content").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
                self.messages.push(MessageItem {
                    role: role.into(),
                    content: content.into(),
                    color,
                });
                self.streaming_role = None;
                self.streaming_content.clear();
            }
            "tool_call" | "tool_execution_start" => {
                let name = event.get("tool_name").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("tool");
                let role = format!("tool:{name}");
                self.streaming_role = Some(role);
                self.streaming_content.clear();
                self.busy = true;
            }
            "tool_execution_update" => {
                if let Some(delta) = event.get("delta").and_then(|v: &serde_json::Value| v.as_str()) {
                    self.streaming_content.push_str(delta);
                }
            }
            "tool_execution_end" | "tool_result" => {
                if let Some(role) = self.streaming_role.take() {
                    let color = Self::role_color(&role);
                    self.messages.push(MessageItem {
                        role: role.into(),
                        content: std::mem::take(&mut self.streaming_content).into(),
                        color,
                    });
                }
            }
            "agent_end" | "turn_end" => {
                self.busy = false;
            }
            _ => {}
        }
    }

    fn refresh_state(&mut self) {
        let Some(ref mut ipc) = self.ipc else {
            self.status = "Disconnected".into();
            return;
        };
        match ipc.call("state", None) {
            Ok(state) => {
                let model: String = state.get("model").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("unknown").to_string();
                let tools = state.get("tools").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
                let plugins = state.get("plugins").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
                self.model = model.into();
                self.tools_count = tools;
                self.plugins_count = plugins;
                self.status = "Connected".into();
            }
            Err(e) => self.status = format!("Error: {e}").into(),
        }
        match ipc.call("messages", None) {
            Ok(msgs) => {
                if let Some(arr) = msgs.as_array() {
                    self.messages.clear();
                    for msg in arr {
                        let role = msg.get("role").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("?").to_string();
                        let color = Self::role_color(&role);
                        let content = msg.get("content").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
                        self.messages.push(MessageItem {
                            role: role.into(),
                            content: content.into(),
                            color,
                        });
                    }
                }
            }
            Err(e) => self.status = format!("Error: {e}").into(),
        }
    }

    fn drain_events(&mut self) {
        let Some(ref mut ipc) = self.ipc else { return };
        for event in ipc.drain_events() {
            self.apply_event(&event);
        }
    }

    fn send_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(ref mut ipc) = self.ipc else { return };
        if self.input.is_empty() || self.busy {
            return;
        }
        let text = self.input.clone();
        self.messages.push(MessageItem {
            role: "user".into(),
            content: text.clone().into(),
            color: Self::role_color("user"),
        });
        self.input.clear();
        self.busy = true;
        self.status = "Sending...".into();
        match ipc.call("prompt", Some(serde_json::json!({"text": text}))) {
            Ok(_) => {
                self.drain_events();
                self.refresh_state();
                self.status = "Ready".into();
            }
            Err(e) => {
                self.status = format!("Error: {e}").into();
                self.busy = false;
            }
        }
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "enter" {
            self.send_prompt(cx);
        } else if key.key == "backspace" {
            self.input.pop();
            cx.notify();
        } else if let Some(ch) = key.key_char.as_deref() {
            if !key.modifiers.control && !key.modifiers.alt {
                self.input.push_str(ch);
                cx.notify();
            }
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.drain_events();
        self.refresh_state();
        cx.notify();
    }
}

impl Render for TelekinesisView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let tools_count = self.tools_count;
        let plugins_count = self.plugins_count;
        let status = self.status.clone();
        let input: SharedString = if self.input.is_empty() {
            "Type a message...".into()
        } else {
            self.input.clone().into()
        };
        let busy = self.busy;

        let mut all_messages: Vec<MessageItem> = self.messages.clone();
        if let Some(role) = &self.streaming_role {
            all_messages.push(MessageItem {
                role: role.clone().into(),
                content: self.streaming_content.clone().into(),
                color: Self::role_color(role),
            });
        }
        let messages = all_messages.iter();

        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = this.update(cx, |view, cx| view.refresh(cx));
            }
        });
        poll.detach();

        view_file!("../app.crepus")
            .on_key_down(cx.listener(Self::handle_key))
    }
}

impl Clone for MessageItem {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
            content: self.content.clone(),
            color: self.color,
        }
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let options = crepuscularity_gpui::gpui_window_options(
            "telekinesis-gui",
            "Telekinesis",
            Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: Size {
                    width: px(900.0),
                    height: px(640.0),
                },
            })),
            Some(Size {
                width: px(600.0),
                height: px(400.0),
            }),
        );
        match cx.open_window(options, |_win, cx| cx.new(TelekinesisView::new)) {
            Ok(_) => {}
            Err(e) => eprintln!("failed to open window: {e:?}"),
        }
    });
}
