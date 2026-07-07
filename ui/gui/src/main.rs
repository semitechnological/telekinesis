use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crepuscularity_gpui::prelude::*;
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

struct TelekinesisView {
    input: String,
    messages: Vec<(String, String)>,
    model: String,
    tools_count: usize,
    plugins_count: usize,
    status: String,
    ipc: Option<IpcClient>,
}

impl TelekinesisView {
    fn new(_cx: &mut Context<Self>) -> Self {
        let ipc = IpcClient::connect(&socket_path()).ok();
        let mut view = Self {
            input: String::new(),
            messages: Vec::new(),
            model: "disconnected".to_string(),
            tools_count: 0,
            plugins_count: 0,
            status: "Connecting...".to_string(),
            ipc,
        };
        view.refresh_state();
        view
    }

    fn refresh_state(&mut self) {
        let Some(ref mut ipc) = self.ipc else {
            self.status = "Disconnected".to_string();
            return;
        };
        match ipc.call("state", None) {
            Ok(state) => {
                self.model = state.get("model").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("unknown").to_string();
                self.tools_count = state.get("tools").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
                self.plugins_count = state.get("plugins").and_then(|v: &serde_json::Value| v.as_u64()).unwrap_or(0) as usize;
                self.status = "Connected".to_string();
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
        match ipc.call("messages", None) {
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

    fn send_prompt(&mut self, _cx: &mut Context<Self>) {
        let Some(ref mut ipc) = self.ipc else { return };
        if self.input.is_empty() {
            return;
        }
        let text = self.input.clone();
        self.messages.push(("user".to_string(), text.clone()));
        self.input.clear();
        match ipc.call("prompt", Some(serde_json::json!({"text": text}))) {
            Ok(_) => {
                self.refresh_state();
                self.status = "Sent".to_string();
            }
            Err(e) => self.status = format!("Error: {e}"),
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "enter" {
            self.send_prompt(cx);
            cx.notify();
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

    fn refresh(&mut self, _cx: &mut Context<Self>) {
        self.refresh_state();
    }
}

impl Render for TelekinesisView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let tools = self.tools_count;
        let plugins = self.plugins_count;
        let status = self.status.clone();
        let input = self.input.clone();
        let messages: Vec<(String, String)> = self.messages.clone();

        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = this.update(cx, |view, cx| view.refresh(cx));
            }
        });
        poll.detach();

        div()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(0x09090b))
            .text_color(rgb(0xf4f4f5))
            .text_size(px(14.0))
            .on_key_down(cx.listener(Self::handle_key))
            .child(
                div()
                    .h(px(48.0))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(16.0))
                    .gap(px(12.0))
                    .border_b_1()
                    .border_color(rgb(0x27272a))
                    .child(div().text_size(px(18.0)).font_weight(FontWeight::SEMIBOLD).text_color(rgb(0x818cf8)).child("Telekinesis"))
                    .child(div().text_size(px(12.0)).text_color(rgb(0x71717a)).child("v0.1.0"))
                    .child(div().flex_1())
                    .child(div().text_size(px(12.0)).text_color(rgb(0xa1a1aa)).child(format!("model: {model}")))
                    .child(div().text_size(px(12.0)).text_color(rgb(0x71717a)).child(format!("tools: {tools}")))
                    .child(div().text_size(px(12.0)).text_color(rgb(0x71717a)).child(format!("plugins: {plugins}")))
            )
            .child(
                div()
                    .flex_1()
                    .w_full()
                    .px(px(16.0))
                    .py(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .overflow_hidden()
                    .children(messages.into_iter().map(|(role, content)| {
                        let role_color = match role.as_str() {
                            "user" => rgb(0x818cf8),
                            "assistant" => rgb(0x34d399),
                            "system" => rgb(0xfbbf24),
                            _ => rgb(0x71717a),
                        };
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(role_color)
                                    .child(format!("{role}"))
                            )
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .text_color(rgb(0xf4f4f5))
                                    .child(content)
                            )
                    }))
            )
            .child(
                div()
                    .h(px(48.0))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .px(px(16.0))
                    .gap(px(8.0))
                    .border_t_1()
                    .border_color(rgb(0x27272a))
                    .child(div().text_size(px(14.0)).text_color(rgb(0x818cf8)).child(">"))
                    .child(div().flex_1().text_size(px(14.0)).text_color(rgb(0xf4f4f5)).child(if input.is_empty() { "Type a message...".to_string() } else { input }))
                    .child(div().text_size(px(12.0)).text_color(rgb(0x71717a)).child(status))
            )
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
