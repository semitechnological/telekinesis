use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crepuscularity_gpui::prelude::*;
use crepuscularity_macros::view_file;
use futures::StreamExt;
use gpui::{ClickEvent, *};
use rx4::agent::{Agent, Event as Rx4Event};
use rx4::mode::Scope;
use rx4::provider::{Message, ProviderError, Role, StreamEvent};
use rx4::{register_builtin_tools, ToolRegistry};
use tokio::sync::Mutex;

struct OpenAICompatProvider {
    id: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAICompatProvider {
    fn new(id: &str, base_url: &str, api_key: &str) -> Self {
        Self {
            id: id.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn from_env() -> Option<Self> {
        if let Ok(key) = std::env::var("XAI_API_KEY") {
            return Some(Self::new("xai", "https://api.x.ai/v1", &key));
        }
        if let Ok(key) = std::env::var("OPENAI_API_KEY") {
            return Some(Self::new("openai", "https://api.openai.com/v1", &key));
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            return Some(Self::new("anthropic", "https://api.anthropic.com/v1", &key));
        }
        if let Ok(key) = std::env::var("GOOGLE_API_KEY") {
            return Some(Self::new(
                "google",
                "https://generativelanguage.googleapis.com/v1beta",
                &key,
            ));
        }
        None
    }

    fn default_model(&self) -> &str {
        match self.id.as_str() {
            "xai" => "grok-4.5",
            "openai" => "gpt-4o",
            "anthropic" => "claude-3-5-sonnet-20241022",
            "google" => "gemini-2.0-flash",
            _ => "gpt-4o",
        }
    }
}

impl rx4::provider::Provider for OpenAICompatProvider {
    fn id(&self) -> &str {
        &self.id
    }
    fn name(&self) -> &str {
        "OpenAI Compatible"
    }

    fn stream<
        'life0: 'async_trait,
        'life1: 'async_trait,
        'life2: 'async_trait,
        'life3: 'async_trait,
        'life4: 'async_trait,
        'async_trait,
    >(
        &'life0 self,
        messages: &'life1 [Message],
        system: &'life2 Option<String>,
        model: &'life3 str,
        _tools: &'life4 [serde_json::Value],
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<
                        Box<
                            dyn futures::Stream<Item = Result<StreamEvent, ProviderError>>
                                + Send
                                + Unpin
                                + 'static,
                        >,
                        ProviderError,
                    >,
                > + Send
                + 'async_trait,
        >,
    >
    where
        Self: 'async_trait,
    {
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let provider_id = self.id.clone();
        let model = model.to_string();
        let messages = messages.to_vec();
        let system = system.clone();
        let client = self.client.clone();

        Box::pin(async move {
            let mut req_messages: Vec<serde_json::Value> = Vec::new();
            if let Some(s) = &system {
                req_messages.push(serde_json::json!({"role": "system", "content": s}));
            }
            for msg in &messages {
                let role = match msg.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                    Role::Tool => "tool",
                };
                req_messages.push(serde_json::json!({"role": role, "content": msg.content}));
            }

            if provider_id == "anthropic" {
                rx4::apply_cache_control(
                    &mut req_messages,
                    &rx4::PromptCacheConfig::anthropic(),
                );
            }

            let body = serde_json::json!({
                "model": model,
                "messages": req_messages,
                "stream": true,
            });

            let url = format!("{}/chat/completions", base_url);
            let resp = client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| ProviderError::Http(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(ProviderError::Api(format!("{}: {}", status, text)));
            }

            let stream = resp
                .bytes_stream()
                .map(|chunk| chunk.map_err(std::io::Error::other));

            let event_stream = SseStream::new(stream);
            let rx4_stream = event_stream.filter_map(|event| async move {
                match event {
                    Ok(data) => {
                        if data.is_empty() || data == "[DONE]" {
                            return Some(Ok(StreamEvent::Done));
                        }
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                            if let Some(delta) = json
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("delta"))
                                .and_then(|d| d.get("content"))
                                .and_then(|c| c.as_str())
                            {
                                if !delta.is_empty() {
                                    return Some(Ok(StreamEvent::Delta(delta.to_string())));
                                }
                            }
                            if let Some(finish) = json
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("finish_reason"))
                                .and_then(|f| f.as_str())
                            {
                                if !finish.is_empty() {
                                    return Some(Ok(StreamEvent::Done));
                                }
                            }
                        }
                        None
                    }
                    Err(_) => Some(Err(ProviderError::Stream("stream error".to_string()))),
                }
            });

            Ok(Box::new(Box::pin(rx4_stream))
                as Box<
                    dyn futures::Stream<Item = Result<StreamEvent, ProviderError>> + Send + Unpin,
                >)
        })
    }
}

struct SseStream<S> {
    inner: S,
    buffer: String,
}

impl<S> SseStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
        }
    }
}

impl<S, E> futures::Stream for SseStream<S>
where
    S: futures::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
{
    type Item = Result<String, E>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(pos) = this.buffer.find("\n\n") {
                let block: String = this.buffer.drain(..pos + 2).collect();
                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        return std::task::Poll::Ready(Some(Ok(data.to_string())));
                    }
                }
                continue;
            }
            match futures::Stream::poll_next(std::pin::Pin::new(&mut this.inner), cx) {
                std::task::Poll::Ready(Some(Ok(chunk))) => {
                    this.buffer.push_str(&String::from_utf8_lossy(&chunk));
                    continue;
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    return std::task::Poll::Ready(Some(Err(e)));
                }
                std::task::Poll::Ready(None) => {
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => {
                    return std::task::Poll::Pending;
                }
            }
        }
    }
}

#[derive(Clone)]
struct MessageItem {
    role: SharedString,
    content: SharedString,
    color: u32,
}

struct PointTarget {
    x: f32,
    y: f32,
    label: String,
}

fn parse_point_tags(text: &str) -> (String, Vec<PointTarget>) {
    let mut clean = String::new();
    let mut targets = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[POINT:") {
        clean.push_str(&rest[..start]);
        let after = &rest[start + 7..];
        if let Some(end) = after.find(']') {
            let tag_content = &after[..end];
            let parts: Vec<&str> = tag_content.splitn(4, ':').collect();
            if parts.len() >= 2 {
                if let (Ok(x), Ok(y)) = (parts[0].parse::<f32>(), parts[1].parse::<f32>()) {
                    let label = parts.get(2).unwrap_or(&"").to_string();
                    targets.push(PointTarget { x, y, label });
                }
            }
            rest = &after[end + 1..];
        } else {
            clean.push_str(&rest[start..]);
            break;
        }
    }
    clean.push_str(rest);
    (clean, targets)
}

struct CursorOverlay {
    target_x: f32,
    target_y: f32,
    prev_x: f32,
    prev_y: f32,
    label: SharedString,
    active: bool,
    point_count: u64,
}

impl Default for CursorOverlay {
    fn default() -> Self {
        Self {
            target_x: 0.0,
            target_y: 0.0,
            prev_x: 0.0,
            prev_y: 0.0,
            label: "".into(),
            active: false,
            point_count: 0,
        }
    }
}

impl CursorOverlay {
    fn point_to(&mut self, x: f32, y: f32, label: String, cx: &mut Context<Self>) {
        self.prev_x = self.target_x;
        self.prev_y = self.target_y;
        self.target_x = x;
        self.target_y = y;
        self.label = label.into();
        self.active = true;
        self.point_count += 1;
        cx.notify();

        let point_count = self.point_count;
        let fade = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(4))
                .await;
            let _ = this.update(cx, |overlay, cx| {
                if overlay.point_count == point_count {
                    overlay.active = false;
                    cx.notify();
                }
            });
        });
        fade.detach();
    }
}

impl Render for CursorOverlay {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.active {
            return div().w_full().h_full();
        }

        let prev_x = self.prev_x;
        let prev_y = self.prev_y;
        let target_x = self.target_x;
        let target_y = self.target_y;
        let label = self.label.clone();
        let anim_id = self.point_count;

        div()
            .w_full()
            .h_full()
            .child(
                div()
                    .with_animation(
                        anim_id as usize,
                        Animation::new(Duration::from_millis(600)).with_easing(ease_in_out),
                        move |el, delta| {
                            let x = prev_x + (target_x - prev_x) * delta;
                            let y = prev_y + (target_y - prev_y) * delta;
                            el.absolute()
                                .left(px(x))
                                .top(px(y))
                                .child(
                                    div()
                                        .w(px(24.0))
                                        .h(px(24.0))
                                        .rounded_full()
                                        .bg(rgb(0x3b82f6))
                                        .border_2()
                                        .border_color(rgb(0xffffff)),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .left(px(28.0))
                                        .top(px(-4.0))
                                        .px(px(8.0))
                                        .py(px(4.0))
                                        .rounded(px(6.0))
                                        .bg(rgb(0x1e293b))
                                        .text_color(rgb(0xffffff))
                                        .text_size(px(12.0))
                                        .child(label.clone()),
                                )
                        },
                    ),
            )
    }
}

enum CompanionEvent {
    Rx4(Rx4Event),
    Error(String),
    Idle,
}

struct CompanionView {
    input: String,
    messages: Vec<MessageItem>,
    streaming_role: Option<String>,
    streaming_content: String,
    model: SharedString,
    status: SharedString,
    busy: bool,
    agent: Option<Arc<Mutex<Agent>>>,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>>,
    rt_handle: Option<tokio::runtime::Handle>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<CompanionEvent>>,
    overlay: Option<Entity<CursorOverlay>>,
}

impl CompanionView {
    #[allow(dead_code)]
    fn new(cx: &mut Context<Self>) -> Self {
        Self::with_overlay(cx, None)
    }

    fn with_overlay(_cx: &mut Context<Self>, overlay: Option<Entity<CursorOverlay>>) -> Self {
        let mut view = Self {
            input: String::new(),
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            model: "no-model".into(),
            status: "Initializing...".into(),
            busy: false,
            agent: None,
            event_rx: None,
            rt_handle: None,
            event_tx: None,
            overlay,
        };

        if let Some((agent, model, rx, handle, tx)) = setup_agent() {
            view.agent = Some(agent);
            view.model = model.into();
            view.status = "Ready".into();
            view.event_rx = Some(rx);
            view.rt_handle = Some(handle);
            view.event_tx = Some(tx);
        } else {
            view.status = "No API key — set XAI_API_KEY / OPENAI_API_KEY / ANTHROPIC_API_KEY".into();
        }

        view
    }

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

    fn handle_rx4_event(&mut self, event: Rx4Event, cx: &mut Context<Self>) {
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
                if let Some(ref overlay) = self.overlay {
                    let (_, targets) = parse_point_tags(&delta);
                    for target in targets {
                        let _ = overlay.update(cx, |o, cx| {
                            o.point_to(target.x, target.y, target.label, cx);
                        });
                    }
                }
            }
            Rx4Event::MessageEnd { content, .. } => {
                let role = self.streaming_role.take().unwrap_or_else(|| "assistant".to_string());
                let color = Self::role_color(&role);
                let raw = if content.is_empty() {
                    std::mem::take(&mut self.streaming_content)
                } else {
                    content
                };
                let (clean, targets) = parse_point_tags(&raw);
                if let Some(ref overlay) = self.overlay {
                    for target in targets {
                        let _ = overlay.update(cx, |o, cx| {
                            o.point_to(target.x, target.y, target.label, cx);
                        });
                    }
                }
                self.messages.push(MessageItem {
                    role: role.into(),
                    content: clean.into(),
                    color,
                });
                self.streaming_content.clear();
            }
            Rx4Event::ToolCall(call) => {
                if let Some(role) = self.streaming_role.take() {
                    let color = Self::role_color(&role);
                    self.messages.push(MessageItem {
                        role: role.into(),
                        content: std::mem::take(&mut self.streaming_content).into(),
                        color,
                    });
                }
                let tool_role = format!("tool:{}", call.name);
                self.streaming_role = Some(tool_role.clone());
                self.streaming_content.clear();
                self.busy = true;
            }
            Rx4Event::ApprovalRequired(req) => {
                self.messages.push(MessageItem {
                    role: "system".into(),
                    content: format!("Approval required: {} ({})", req.tool_name, req.reason).into(),
                    color: Self::role_color("system"),
                });
            }
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(role) = self.streaming_role.take() {
                    let color = Self::role_color(&role);
                    self.messages.push(MessageItem {
                        role: role.into(),
                        content: result.content.into(),
                        color,
                    });
                }
                self.streaming_content.clear();
            }
            Rx4Event::TurnEnd { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(role) = self.streaming_role.take() {
                    let color = Self::role_color(&role);
                    self.messages.push(MessageItem {
                        role: role.into(),
                        content: std::mem::take(&mut self.streaming_content).into(),
                        color,
                    });
                }
                self.busy = false;
            }
            Rx4Event::Error(msg) => {
                self.messages.push(MessageItem {
                    role: "error".into(),
                    content: format!("Error: {msg}").into(),
                    color: Self::role_color("error"),
                });
                self.busy = false;
            }
        }
    }

    fn poll_events(&mut self, cx: &mut Context<Self>) {
        let mut pending = Vec::new();
        if let Some(ref mut rx) = self.event_rx {
            while let Ok(event) = rx.try_recv() {
                pending.push(event);
            }
        }
        for event in pending {
            match event {
                CompanionEvent::Rx4(e) => self.handle_rx4_event(e, cx),
                CompanionEvent::Error(msg) => {
                    self.messages.push(MessageItem {
                        role: "error".into(),
                        content: format!("Error: {msg}").into(),
                        color: Self::role_color("error"),
                    });
                }
                CompanionEvent::Idle => {
                    self.busy = false;
                }
            }
        }
        cx.notify();
    }

    fn send_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(ref agent) = self.agent else {
            return;
        };
        let Some(ref handle) = self.rt_handle else {
            return;
        };
        let Some(ref tx) = self.event_tx else {
            return;
        };

        let text = self.input.trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }

        self.messages.push(MessageItem {
            role: "user".into(),
            content: text.clone().into(),
            color: Self::role_color("user"),
        });
        self.input.clear();
        self.busy = true;
        self.status = "Working...".into();

        let agent = agent.clone();
        let tx = tx.clone();
        handle.spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.prompt(&text).await {
                let _ = tx.send(CompanionEvent::Error(e.to_string()));
            }
            let _ = tx.send(CompanionEvent::Idle);
        });

        cx.notify();
    }

    fn capture_screen(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ref agent) = self.agent else {
            return;
        };
        let Some(ref handle) = self.rt_handle else {
            return;
        };
        let Some(ref tx) = self.event_tx else {
            return;
        };
        if self.busy {
            return;
        }

        let prompt = "Look at my screen using cu_see and tell me what you see. Then wait for my next instruction.";
        self.messages.push(MessageItem {
            role: "user".into(),
            content: "Look at screen".into(),
            color: Self::role_color("user"),
        });
        self.busy = true;
        self.status = "Capturing screen...".into();

        let agent = agent.clone();
        let tx = tx.clone();
        handle.spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.prompt(prompt).await {
                let _ = tx.send(CompanionEvent::Error(e.to_string()));
            }
            let _ = tx.send(CompanionEvent::Idle);
        });

        cx.notify();
    }

    fn clear_chat(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.messages.clear();
        self.streaming_role = None;
        self.streaming_content.clear();
        self.busy = false;
        self.status = "Ready".into();
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
}

impl Render for CompanionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let status = self.status.clone();
        let input: SharedString = if self.input.is_empty() {
            "Ask me to do anything on your computer...".into()
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
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let _ = this.update(cx, |view, cx| view.poll_events(cx));
            }
        });
        poll.detach();

        view_file!("companion.crepus").on_key_down(cx.listener(Self::handle_key))
    }
}

fn setup_agent() -> Option<(
    Arc<Mutex<Agent>>,
    String,
    tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>,
    tokio::runtime::Handle,
    tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
)> {
    let provider = OpenAICompatProvider::from_env()?;
    let model = provider.default_model().to_string();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok()?;

    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<CompanionEvent>();

    let mut agent = Agent::new();
    agent.set_scope(Scope::ComputerUse);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    rx4::computer_use::register_tools(&mut tools);
    agent.set_tools(tools);
    agent.set_workspace_root(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    agent.load_project_context();
    agent.set_model(&model);
    agent.set_provider(Arc::new(provider));
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    let _ = agent.enable_os_sandbox();
    agent.set_policy(rx4::Policy::workspace_write());
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
        let _ = event_tx_clone.send(CompanionEvent::Rx4(event.clone()));
    });

    let agent = Arc::new(Mutex::new(agent));

    std::mem::forget(rt);

    Some((agent, model, event_rx, handle, event_tx))
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let overlay = cx.new(|_cx| CursorOverlay::default());

        let overlay_options = WindowOptions {
            app_id: Some("telekinesis-overlay".to_string()),
            titlebar: None,
            window_bounds: Some(WindowBounds::Fullscreen(Bounds::default())),
            window_min_size: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            tabbing_identifier: None,
        };
        match cx.open_window(overlay_options, |_win, _cx| overlay.clone()) {
            Ok(_) => {}
            Err(e) => eprintln!("failed to open overlay window: {e:?}"),
        }

        let companion_options = WindowOptions {
            app_id: Some("telekinesis-companion".to_string()),
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Default::default(),
                size: Size {
                    width: px(400.0),
                    height: px(560.0),
                },
            })),
            window_min_size: Some(Size {
                width: px(320.0),
                height: px(400.0),
            }),
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: Some(WindowDecorations::Client),
            tabbing_identifier: None,
        };
        let overlay_for_companion = overlay.clone();
        match cx.open_window(companion_options, |_win, cx| {
            cx.new(|cx| CompanionView::with_overlay(cx, Some(overlay_for_companion)))
        }) {
            Ok(_) => {}
            Err(e) => eprintln!("failed to open companion window: {e:?}"),
        }
    });
}
