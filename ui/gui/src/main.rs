use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crepuscularity_gpui::prelude::*;
use crepuscularity_macros::view_file;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use gpui::{ClickEvent, *};
use rx4::agent::{Agent, Event as Rx4Event};
use rx4::mode::Scope;
use rx4::provider::{OpenAIProvider, Role};
use rx4::{register_builtin_tools, ToolRegistry};
use tokio::sync::Mutex;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

fn setup_provider() -> Option<(Arc<dyn rx4::Provider>, String)> {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.is_empty() {
            return Some((
                Arc::new(OpenAIProvider::anthropic(key)),
                "claude-3-5-sonnet-20241022".into(),
            ));
        }
    }
    for (env_var, base_url, id, name, model) in [
        ("XAI_API_KEY", "https://api.x.ai/v1", "xai", "xAI", "grok-4.5"),
        ("OPENAI_API_KEY", "https://api.openai.com/v1", "openai", "OpenAI", "gpt-4o"),
        (
            "GOOGLE_API_KEY",
            "https://generativelanguage.googleapis.com/v1beta",
            "google",
            "Google",
            "gemini-2.0-flash",
        ),
    ] {
        if let Ok(key) = std::env::var(env_var) {
            if !key.is_empty() {
                return Some((
                    Arc::new(OpenAIProvider::with_base_url(base_url, key, id, name)),
                    model.into(),
                ));
            }
        }
    }
    None
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

#[derive(Default)]
struct CursorOverlay {
    target_x: f32,
    target_y: f32,
    prev_x: f32,
    prev_y: f32,
    label: SharedString,
    active: bool,
    point_count: u64,
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

        div().w_full().h_full().child(
            div().with_animation(
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
    fn with_overlay(cx: &mut Context<Self>, overlay: Option<Entity<CursorOverlay>>) -> Self {
        // ponytail: poll loop spawned once here, not in render() — render fires on every keystroke
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
            view.status = "No API key — set XAI_API_KEY / OPENAI_API_KEY / ANTHROPIC_API_KEY"
                .into();
        }

        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let _ = this.update(cx, |view, cx| view.poll_events(cx));
            }
        });
        poll.detach();

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
                        overlay.update(cx, |o, cx| {
                            o.point_to(target.x, target.y, target.label, cx);
                        });
                    }
                }
            }
            Rx4Event::MessageEnd { content, .. } => {
                let role = self
                    .streaming_role
                    .take()
                    .unwrap_or_else(|| "assistant".to_string());
                let color = Self::role_color(&role);
                let raw = if content.is_empty() {
                    std::mem::take(&mut self.streaming_content)
                } else {
                    content
                };
                let (clean, _targets) = parse_point_tags(&raw);
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
    let (provider, model) = setup_provider()?;

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
    agent.set_provider(provider);
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

fn create_tray_icon() -> TrayIcon {
    let menu = Menu::new();
    let show_item = MenuItem::with_id("show", "Show/Hide", true, None);
    let capture_item = MenuItem::with_id("capture", "Capture Screen", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);
    let separator = PredefinedMenuItem::separator();

    let _ = menu.append(&show_item);
    let _ = menu.append(&capture_item);
    let _ = menu.append(&separator);
    let _ = menu.append(&quit_item);

    let rgba: Vec<u8> = [0x81u8, 0x8c, 0xf8, 0xff]
        .repeat(22 * 22);
    let icon =
        tray_icon::Icon::from_rgba(rgba, 22, 22).expect("failed to create tray icon from rgba");

    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Telekinesis Companion")
        .with_icon(icon)
        .build()
        .expect("failed to create tray icon")
}

#[cfg(target_os = "macos")]
fn screen_size() -> (f32, f32) {
    use objc2::{class, msg_send};
    use objc2_core_foundation::CGRect;
    unsafe {
        let main: *mut objc2::runtime::AnyObject = msg_send![class!(NSScreen), mainScreen];
        let frame: CGRect = msg_send![main, frame];
        (frame.size.width as f32, frame.size.height as f32)
    }
}

#[cfg(not(target_os = "macos"))]
fn screen_size() -> (f32, f32) {
    (1440.0, 900.0)
}

fn main() {
    let hotkey_manager = GlobalHotKeyManager::new().ok();
    let hotkey_id = if let Some(ref manager) = hotkey_manager {
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
        match manager.register(hotkey) {
            Ok(_) => Some(hotkey.id()),
            Err(e) => {
                eprintln!("failed to register global hotkey: {e}");
                None
            }
        }
    } else {
        eprintln!("failed to create global hotkey manager");
        None
    };

    let (screen_w, screen_h) = screen_size();

    Application::new().run(move |cx: &mut App| {
        let overlay = cx.new(|_cx| CursorOverlay::default());

        let overlay_options = WindowOptions {
            app_id: Some("telekinesis-overlay".to_string()),
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(screen_w), px(screen_h)),
            })),
            window_min_size: None,
            focus: false,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: None,
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
                origin: point(px(0.0), px(0.0)),
                size: size(px(400.0), px(560.0)),
            })),
            window_min_size: Some(Size {
                width: px(320.0),
                height: px(400.0),
            }),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: None,
            tabbing_identifier: None,
        };
        let overlay_for_companion = overlay.clone();
        let companion_handle = cx
            .open_window(companion_options, |_win, cx| {
                cx.new(|cx| CompanionView::with_overlay(cx, Some(overlay_for_companion)))
            })
            .ok();

        let _tray = create_tray_icon();

        let poll = cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;

                if let Some(hid) = hotkey_id {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                        if event.id == hid && event.state == HotKeyState::Pressed {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = companion_handle {
                                    let _ = handle.update(cx, |_view, window, cx| {
                                        if window.is_window_active() {
                                            window.minimize_window();
                                        } else {
                                            window.activate_window();
                                        }
                                        cx.notify();
                                    });
                                }
                            });
                        }
                    }
                }

                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    match event.id.0.as_str() {
                        "show" => {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = companion_handle {
                                    let _ = handle.update(cx, |_view, window, cx| {
                                        if window.is_window_active() {
                                            window.minimize_window();
                                        } else {
                                            window.activate_window();
                                        }
                                        cx.notify();
                                    });
                                }
                            });
                        }
                        "capture" => {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = companion_handle {
                                    let _ = handle.update(cx, |view, window, cx| {
                                        view.capture_screen(&ClickEvent::default(), window, cx);
                                    });
                                }
                            });
                        }
                        "quit" => {
                            let _ = cx.update(|cx| {
                                cx.quit();
                            });
                        }
                        _ => {}
                    }
                }
            }
        });
        poll.detach();
    });
}
