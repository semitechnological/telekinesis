use std::sync::Arc;
use std::time::Duration;

mod shake;

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

/// Clicky-style system prompt for the telekinesis companion.
/// Instructs the agent to capture the screen, use [POINT:] tags for cursor
/// pointing, and be conversational.
const SYSTEM_PROMPT: &str = r#"you're telekinesis, a friendly companion that lives in the user's menu bar. you can see their screen via the cu_see tool and interact with their computer via cu_click, cu_type, cu_hotkey tools. your reply will be displayed in a chat panel.

rules:
- be direct and helpful. default to 1-3 sentences unless the user asks for more detail.
- casual, warm tone. no emojis.
- you can help with anything — coding, writing, general knowledge, computer tasks.
- when the user asks about something on their screen, use cu_see to capture the screen first, then answer based on what you see.
- you can click, type, and press keys on the user's computer using cu_click, cu_type, and cu_hotkey. ask before doing anything destructive.
- never say "simply" or "just".

element pointing:
you have a blue cursor overlay that can fly to and point at things on screen. use it whenever pointing would genuinely help the user — if they're asking how to do something, looking for a menu, trying to find a button, or need help navigating an app.

when you point, append a coordinate tag at the very end of your response, AFTER your text: [POINT:x,y:label] where x,y are integer pixel coordinates in the screenshot's coordinate space (the image from cu_see), and label is a short 1-3 word description of the element.

if pointing wouldn't help, append [POINT:none].

examples:
- "the color inspector is in the top right of the toolbar. click that to get the color wheels. [POINT:1100,42:color inspector]"
- "html is the skeleton of every web page. [POINT:none]"
- "see the source control menu up top? click that and hit commit. [POINT:285,11:source control]"
"#;

#[cfg(target_os = "macos")]
fn with_ns_window<F, R>(window: &Window, f: F) -> Option<R>
where
    F: FnOnce(*mut objc2::runtime::AnyObject) -> R,
{
    use objc2::msg_send;
    use raw_window_handle::HasWindowHandle;
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        if let raw_window_handle::RawWindowHandle::AppKit(appkit) = handle.as_raw() {
            let ns_view = appkit.ns_view.as_ptr() as *mut objc2::runtime::AnyObject;
            unsafe {
                let ns_window: *mut objc2::runtime::AnyObject = msg_send![ns_view, window];
                if !ns_window.is_null() {
                    return Some(f(ns_window));
                }
            }
        }
    }
    None
}

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
    is_tool: bool,
    is_user: bool,
    is_error: bool,
}

impl MessageItem {
    fn new(role: &str, content: impl Into<SharedString>) -> Self {
        let is_tool = role.starts_with("tool:");
        let is_user = role == "user";
        let is_error = role == "error";
        Self {
            role: role.to_string().into(),
            content: content.into(),
            is_tool,
            is_user,
            is_error,
        }
    }
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
    #[allow(dead_code)]
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

        // Clicky-style bezier flight: quadratic bezier with arc, rotation, scale pulse
        let dx = target_x - prev_x;
        let dy = target_y - prev_y;
        let distance = (dx * dx + dy * dy).sqrt();
        let flight_ms = (distance / 800.0 * 1000.0).clamp(600.0, 1400.0) as u64;
        let mid_x = (prev_x + target_x) / 2.0;
        let mid_y = (prev_y + target_y) / 2.0;
        let arc_height = (distance * 0.2).min(80.0);
        // Control point lifted upward (screen coords: y increases downward, so subtract)
        let ctrl_x = mid_x;
        let ctrl_y = mid_y - arc_height;

        div().w_full().h_full().child(
            div().with_animation(
                anim_id as usize,
                Animation::new(Duration::from_millis(flight_ms)).with_easing(ease_in_out),
                move |el, delta| {
                    // Quadratic bezier: B(t) = (1-t)²·P0 + 2(1-t)t·P1 + t²·P2
                    let t = delta;
                    let omt = 1.0 - t;
                    let x = omt * omt * prev_x + 2.0 * omt * t * ctrl_x + t * t * target_x;
                    let y = omt * omt * prev_y + 2.0 * omt * t * ctrl_y + t * t * target_y;

                    // Scale pulse: sin(πt) grows to 1.3x at midpoint
                    let scale = 1.0 + (std::f32::consts::PI * t).sin() * 0.3;

                    el.absolute()
                        .left(px(x))
                        .top(px(y))
                        .child(
                            // Triangle cursor (clicky-style blue cursor)
                            div()
                                .w(px(28.0 * scale))
                                .h(px(28.0 * scale))
                                .bg(rgb(0x3b82f6))
                                .border_2()
                                .border_color(rgb(0xffffff))
                                .rounded(px(4.0 * scale)),
                        )
                        .child(
                            // Label bubble
                            div()
                                .absolute()
                                .left(px(32.0))
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
    /// Session index this event belongs to
    Session(usize, Rx4Event),
    SessionError(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum PanelKind {
    Cursor,
    Desktop,
}

/// A single agent session — one agent with its own message history.
struct AgentSession {
    #[allow(dead_code)]
    name: SharedString,
    #[allow(dead_code)]
    kind: SessionKind,
    agent: Option<Arc<Mutex<Agent>>>,
    messages: Vec<MessageItem>,
    streaming_role: Option<String>,
    streaming_content: String,
    busy: bool,
    model: SharedString,
}

#[derive(Clone, Copy, PartialEq)]
enum SessionKind {
    ComputerUse,
    Coding,
}

impl AgentSession {
    fn new(name: &str, kind: SessionKind, agent: Option<Arc<Mutex<Agent>>>, model: &str) -> Self {
        Self {
            name: name.to_string().into(),
            kind,
            agent,
            messages: Vec::new(),
            streaming_role: None,
            streaming_content: String::new(),
            busy: false,
            model: model.to_string().into(),
        }
    }

    #[allow(dead_code)]
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

    fn handle_rx4_event(&mut self, event: Rx4Event, _overlay: Option<Entity<CursorOverlay>>, cx: &mut Context<CompanionView>) {
        let _ = cx;
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
            }
            Rx4Event::MessageEnd { content, .. } => {
                let role = self
                    .streaming_role
                    .take()
                    .unwrap_or_else(|| "assistant".to_string());
                let raw = if content.is_empty() {
                    std::mem::take(&mut self.streaming_content)
                } else {
                    content
                };
                self.messages.push(MessageItem::new(&role, raw));
                self.streaming_content.clear();
            }
            Rx4Event::ToolCall(call) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, std::mem::take(&mut self.streaming_content)));
                }
                let tool_role = format!("tool:{}", call.name);
                self.streaming_role = Some(tool_role.clone());
                self.streaming_content.clear();
                self.busy = true;
            }
            Rx4Event::ApprovalRequired(req) => {
                self.messages.push(MessageItem::new("system", format!("Approval required: {} ({})", req.tool_name, req.reason)));
            }
            Rx4Event::ToolExecutionStart(_) => {}
            Rx4Event::ToolExecutionEnd(result) => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, result.content));
                }
                self.streaming_content.clear();
            }
            Rx4Event::TurnEnd { .. } => {}
            Rx4Event::AgentEnd => {
                if let Some(role) = self.streaming_role.take() {
                    self.messages.push(MessageItem::new(&role, std::mem::take(&mut self.streaming_content)));
                }
                self.busy = false;
            }
            Rx4Event::Error(msg) => {
                self.messages.push(MessageItem::new("error", format!("Error: {msg}")));
            }
        }
    }
}

struct CompanionView {
    input: String,
    sessions: Vec<AgentSession>,
    active_session: usize,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>>,
    rt_handle: Option<tokio::runtime::Handle>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<CompanionEvent>>,
    overlay: Option<Entity<CursorOverlay>>,
    panel_kind: PanelKind,
    cursor_panel_window: Option<gpui::WindowHandle<CompanionView>>,
    /// Desktop sidebar expanded
    sidebar_expanded: bool,
    /// Sessions section expanded
    sessions_expanded: bool,
    /// Recent section expanded
    recent_expanded: bool,
}

impl CompanionView {
    fn new(
        _cx: &mut Context<Self>,
        overlay: Option<Entity<CursorOverlay>>,
        panel_kind: PanelKind,
    ) -> Self {
        let mut view = Self {
            input: String::new(),
            sessions: Vec::new(),
            active_session: 0,
            event_rx: None,
            rt_handle: None,
            event_tx: None,
            overlay,
            panel_kind,
            cursor_panel_window: None,
            sidebar_expanded: true,
            sessions_expanded: true,
            recent_expanded: false,
        };

        if let Some(AgentSetup { computer_use, coding, model, event_rx, rt_handle, event_tx }) = setup_agents() {
            view.sessions.push(AgentSession::new("computer use", SessionKind::ComputerUse, Some(computer_use), &model));
            view.sessions.push(AgentSession::new("coding", SessionKind::Coding, Some(coding), &model));
            view.event_rx = Some(event_rx);
            view.rt_handle = Some(rt_handle);
            view.event_tx = Some(event_tx);
        } else {
            view.sessions.push(AgentSession::new("no agent", SessionKind::ComputerUse, None, "no-model"));
        }

        // Note: GPUI's cx.spawn() foreground executor doesn't drive tasks in this version.
        // Event polling is handled by the App-level spawn loop in main().
        // Shake-to-show is handled by a std thread with raw NSWindow pointers in main().

        view
    }

    fn active_session(&self) -> Option<&AgentSession> {
        self.sessions.get(self.active_session)
    }

    fn active_session_mut(&mut self) -> Option<&mut AgentSession> {
        self.sessions.get_mut(self.active_session)
    }

    /// Toggle sidebar expand/collapse (desktop only)
    fn toggle_sidebar(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_expanded = !self.sidebar_expanded;
        cx.notify();
    }

    /// Toggle sessions section
    fn toggle_sessions(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.sessions_expanded = !self.sessions_expanded;
        cx.notify();
    }

    /// Toggle recent section
    fn toggle_recent(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.recent_expanded = !self.recent_expanded;
        cx.notify();
    }

    /// Close the desktop window
    fn close_window(&mut self, _: &ClickEvent, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    /// Minimize the desktop window
    fn minimize_window(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        {
            use objc2::msg_send;
            let _ = with_ns_window(_window, |ns_window| unsafe {
                let _: () = msg_send![ns_window, miniaturize: ns_window];
            });
        }
        let _ = cx;
    }

    /// Toggle maximize/restore the desktop window
    fn maximize_window(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        {
            use objc2::msg_send;
            let _ = with_ns_window(_window, |ns_window| unsafe {
                let _: () = msg_send![ns_window, toggleFullScreen: ns_window];
            });
        }
        let _ = cx;
    }

    fn poll_events(&mut self, cx: &mut Context<Self>) -> bool {
        let mut pending = Vec::new();
        if let Some(ref mut rx) = self.event_rx {
            while let Ok(event) = rx.try_recv() {
                pending.push(event);
            }
        }
        let had_events = !pending.is_empty();
        for event in pending {
            match event {
                CompanionEvent::Session(idx, e) => {
                    let overlay = self.overlay.clone();
                    if let Some(session) = self.sessions.get_mut(idx) {
                        session.handle_rx4_event(e, overlay, cx);
                    }
                }
                CompanionEvent::SessionError(msg) => {
                    if let Some(session) = self.active_session_mut() {
                        session.messages.push(MessageItem::new("error", format!("Error: {msg}")));
                    }
                }
            }
        }
        had_events
    }

    fn send_prompt(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.do_send_prompt(cx);
    }

    fn do_send_prompt(&mut self, cx: &mut Context<Self>) {
        let Some(ref handle) = self.rt_handle else {
            return;
        };
        let Some(ref tx) = self.event_tx else {
            return;
        };

        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }

        let session_idx = self.active_session;
        let Some(session) = self.sessions.get_mut(session_idx) else {
            return;
        };
        if session.busy {
            return;
        }
        let Some(ref agent) = session.agent else {
            return;
        };

        session.messages.push(MessageItem::new("user", text.clone()));
        self.input.clear();
        session.busy = true;

        let agent = agent.clone();
        let tx = tx.clone();
        handle.spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.prompt(&text).await {
                let _ = tx.send(CompanionEvent::SessionError(e.to_string()));
            }
        });

        cx.notify();
    }

    fn capture_screen(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(ref handle) = self.rt_handle else {
            return;
        };
        let Some(ref tx) = self.event_tx else {
            return;
        };

        let session_idx = self.active_session;
        let Some(session) = self.sessions.get_mut(session_idx) else {
            return;
        };
        if session.busy {
            return;
        }
        let Some(ref agent) = session.agent else {
            return;
        };

        let prompt = "Use cu_see to capture my screen, then tell me what you see. Wait for my next instruction.";
        session.messages.push(MessageItem::new("user", "see screen"));
        session.busy = true;

        let agent = agent.clone();
        let tx = tx.clone();
        handle.spawn(async move {
            let mut agent = agent.lock().await;
            if let Err(e) = agent.prompt(prompt).await {
                let _ = tx.send(CompanionEvent::SessionError(e.to_string()));
            }
        });

        cx.notify();
    }

    fn interrupt(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.active_session_mut() {
            session.busy = false;
            session.streaming_role = None;
            session.streaming_content.clear();
        }
        cx.notify();
    }

    fn hide_panel(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref handle) = self.cursor_panel_window {
            #[cfg(target_os = "macos")]
            {
                use objc2::msg_send;
                let _ = handle.update(cx, |_, window, _cx| {
                    let _ = with_ns_window(window, |ns_window| unsafe {
                        let _: () = msg_send![ns_window, orderOut: ns_window];
                    });
                });
            }
        }
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "enter" {
            self.do_send_prompt(cx);
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
        // Poll events on every render — GPUI's async spawn doesn't drive futures,
        // so we use render as our periodic callback. If events were processed,
        // notify to trigger another render.
        let had_events = self.poll_events(cx);
        if had_events {
            cx.notify();
        }

        let session = self.active_session();
        let model = session.map(|s| s.model.clone()).unwrap_or_else(|| "no-model".into());
        let busy = session.map(|s| s.busy).unwrap_or(false);

        let input: SharedString = if self.input.is_empty() {
            if self.panel_kind == PanelKind::Cursor {
                "ask anything...".into()
            } else {
                "Ask anything, / for commands...".into()
            }
        } else {
            self.input.clone().into()
        };

        // Build messages from the active session
        let mut all_messages: Vec<MessageItem> = session
            .map(|s| s.messages.clone())
            .unwrap_or_default();
        if let Some(s) = self.active_session() {
            if let Some(role) = &s.streaming_role {
                all_messages.push(MessageItem::new(role, s.streaming_content.clone()));
            }
        }

        // Desktop sidebar/section state
        let sidebar_expanded = self.sidebar_expanded;
        let sessions_expanded = self.sessions_expanded;
        let recent_expanded = self.recent_expanded;

        match self.panel_kind {
            PanelKind::Cursor => {
                let messages = all_messages.iter();
                view_file!("cursor_panel.crepus").on_key_down(cx.listener(Self::handle_key))
            }
            PanelKind::Desktop => {
                let messages = all_messages.iter();
                view_file!("desktop.crepus").on_key_down(cx.listener(Self::handle_key))
            }
        }
    }
}

struct AgentSetup {
    computer_use: Arc<Mutex<Agent>>,
    coding: Arc<Mutex<Agent>>,
    model: String,
    event_rx: tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>,
    rt_handle: tokio::runtime::Handle,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
}

fn create_agent(
    scope: Scope,
    model: &str,
    provider: Arc<dyn rx4::Provider>,
    event_tx: tokio::sync::mpsc::UnboundedSender<CompanionEvent>,
    session_idx: usize,
) -> Arc<Mutex<Agent>> {
    let mut agent = Agent::new();
    agent.set_scope(scope);
    let mut tools = ToolRegistry::new();
    register_builtin_tools(&mut tools);
    if scope == Scope::ComputerUse {
        rx4::computer_use::register_tools(&mut tools);
    }
    agent.set_tools(tools);
    agent.set_workspace_root(std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
    agent.set_system_prompt(SYSTEM_PROMPT);
    agent.set_model(model);
    agent.set_provider(provider);
    let workspace = agent.workspace_root.clone();
    agent.set_sandbox(Arc::new(rx4::SandboxManager::new(
        rx4::SandboxProfile::Workspace,
        workspace,
    )));
    agent.set_policy(rx4::Policy::workspace_write());

    agent.subscribe(move |event: &Rx4Event| {
        let _ = event_tx.send(CompanionEvent::Session(session_idx, event.clone()));
    });

    Arc::new(Mutex::new(agent))
}

fn setup_agents() -> Option<AgentSetup> {
    let (provider, model) = setup_provider()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .ok()?;

    let handle = rt.handle().clone();
    let _guard = rt.enter();

    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<CompanionEvent>();

    let computer_use = create_agent(
        Scope::ComputerUse,
        &model,
        provider.clone(),
        event_tx.clone(),
        0,
    );
    let coding = create_agent(
        Scope::Coding,
        &model,
        provider,
        event_tx.clone(),
        1,
    );

    std::mem::forget(rt);

    Some(AgentSetup {
        computer_use,
        coding,
        model,
        event_rx,
        rt_handle: handle,
        event_tx,
    })
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

/// Configure a GPUI window as a borderless floating overlay (clicky pattern).
///
/// - `click_through = true`  → mouse events pass through to apps below (overlay)
/// - `click_through = false` → window receives mouse events but doesn't activate app (panel)
///
/// Sets: no shadow, non-opaque, transparent background, floating level (3),
/// borderless style, hidesOnDeactivate = false.
#[cfg(target_os = "macos")]
fn configure_borderless_overlay<V>(
    window: &gpui::WindowHandle<V>,
    click_through: bool,
    cx: &mut App,
) where
    V: 'static,
{
    use objc2::{class, msg_send};

    let _ = window.update(cx, |_, window, _cx| {
        let _ = with_ns_window(window, |ns_window| unsafe {
            let _: () = msg_send![ns_window, setHasShadow: false];
            let _: () = msg_send![ns_window, setOpaque: false];
            let _: () = msg_send![ns_window, setIgnoresMouseEvents: click_through];
            let clear: *mut objc2::runtime::AnyObject =
                msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![ns_window, setBackgroundColor: clear];
            let _: () = msg_send![ns_window, setLevel: 3i64];
            let style: u64 = msg_send![ns_window, styleMask];
            let _: () = msg_send![ns_window, setStyleMask: style | 128u64];
            let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn configure_borderless_overlay<V>(
    _window: &gpui::WindowHandle<V>,
    _click_through: bool,
    _cx: &mut App,
) where
    V: 'static,
{
}

/// Configure a floating panel that CAN become key window (receive keyboard input)
/// while still floating above other apps. Used for the cursor pill.
#[cfg(target_os = "macos")]
fn configure_floating_key_panel<V>(
    window: &gpui::WindowHandle<V>,
    cx: &mut App,
) where
    V: 'static,
{
    use objc2::{class, msg_send};

    let _ = window.update(cx, |_, window, _cx| {
        let _ = with_ns_window(window, |ns_window| unsafe {
            let clear: *mut objc2::runtime::AnyObject =
                msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![ns_window, setBackgroundColor: clear];
            // NSFloatingWindowLevel = 3
            let _: () = msg_send![ns_window, setLevel: 3i64];
            // Borderless style
            let style: u64 = msg_send![ns_window, styleMask];
            let _: () = msg_send![ns_window, setStyleMask: style | 128u64];
            let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
            // Make it key so it can receive keyboard input
            let _: () = msg_send![ns_window, makeKeyAndOrderFront: ns_window];
        });
    });
}

#[cfg(not(target_os = "macos"))]
fn configure_floating_key_panel<V>(
    _window: &gpui::WindowHandle<V>,
    _cx: &mut App,
) where
    V: 'static,
{
}

/// Set the app to accessory mode (menu bar app, no dock icon).
#[cfg(target_os = "macos")]
fn configure_app_as_accessory() {
    use objc2_app_kit::{NSApp, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApp(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

#[cfg(not(target_os = "macos"))]
fn configure_app_as_accessory() {}

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

    // Shake detection — reports cursor position when shake is detected
    let (shake_tx, shake_rx) = std::sync::mpsc::channel::<(f64, f64)>();

    let _shake_detector = shake::ShakeDetector::start(move |x, y| {
        let _ = shake_tx.send((x, y));
    });

    Application::new().run(move |cx: &mut App| {
        configure_app_as_accessory();

        let overlay = cx.new(|_cx| CursorOverlay::default());

        // 1. Full-screen transparent overlay — click-through, floating
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
        if let Some(oh) = cx.open_window(overlay_options, |_win, _cx| overlay.clone()).ok() {
            configure_borderless_overlay(&oh, true, cx);
        }

        // 2. Cursor pill — small floating panel near cursor, shown on shake.
        //    Floating but CAN become key window for keyboard input.
        //    Created visible but immediately hidden via NSWindow orderOut.
        let cursor_panel_options = WindowOptions {
            app_id: Some("telekinesis-cursor-panel".to_string()),
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(0.0), px(0.0)),
                size: size(px(420.0), px(320.0)),
            })),
            window_min_size: None,
            focus: true,
            show: true,
            kind: WindowKind::PopUp,
            is_movable: true,
            is_resizable: false,
            is_minimizable: true,
            display_id: None,
            window_background: WindowBackgroundAppearance::Transparent,
            window_decorations: None,
            tabbing_identifier: None,
        };
        let overlay_for_cursor = overlay.clone();
        let cursor_panel_handle = cx
            .open_window(cursor_panel_options, |_win, cx| {
                cx.new(|cx| CompanionView::new(cx, Some(overlay_for_cursor), PanelKind::Cursor))
            })
            .ok();
        if let Some(ref ch) = cursor_panel_handle {
            configure_floating_key_panel(ch, cx);

            // Extract raw NSWindow pointer so we can show/hide it from a std thread
            // (GPUI's async spawn doesn't drive futures, so we bypass it entirely)
            #[cfg(target_os = "macos")]
            {
                use objc2::msg_send;
                let mut ns_window_ptr: usize = 0;
                let _ = ch.update(cx, |_, window, _cx| {
                    if let Some(ptr) = with_ns_window(window, |ns_window| {
                        unsafe {
                            // Hide initially — we'll show on shake
                            let _: () = msg_send![ns_window, orderOut: ns_window];
                        }
                        ns_window as usize
                    }) {
                        ns_window_ptr = ptr;
                    }
                });

                // Spawn a std thread that polls shake_rx and shows the window
                // by dispatching NSWindow calls to the main thread via
                // performSelectorOnMainThread (required by AppKit).
                if ns_window_ptr != 0 {
                    let screen_h_val = screen_h;

                    std::thread::spawn(move || {
                        eprintln!("[shake-handler] thread started, ns_window=0x{ns_window_ptr:x}");
                        while let Ok((mouse_x, mouse_y)) = shake_rx.recv() {
                            eprintln!("[shake-handler] shake at ({mouse_x:.0},{mouse_y:.0})");
                            let panel_w = 420.0f64;
                            let panel_h = 320.0f64;
                            let panel_x = (mouse_x + 20.0).min(screen_w as f64 - panel_w - 20.0);
                            let panel_y = (mouse_y + 20.0).min(screen_h as f64 - panel_h - 20.0);
                            // NSWindow frame origin is bottom-left, so flip Y
                            let origin_x = panel_x;
                            let origin_y = screen_h_val as f64 - panel_y - panel_h;

                            // Call makeKeyAndOrderFront: on the main thread via
                            // performSelectorOnMainThread:withObject:waitUntilDone:
                            unsafe {
                                use objc2::{class, msg_send};

                                let ns_window = ns_window_ptr as *mut objc2::runtime::AnyObject;

                                // setFrameOrigin: takes an NSPoint — we need to create one
                                // NSPoint = {x: f64, y: f64}
                                #[repr(C)]
                                struct NSPoint {
                                    x: f64,
                                    y: f64,
                                }
                                let point = NSPoint { x: origin_x, y: origin_y };

                                // Use NSValue to wrap the point for performSelectorOnMainThread
                                let ns_value: *mut objc2::runtime::AnyObject = msg_send![
                                    class!(NSValue),
                                    valueWithBytes: &point as *const NSPoint as *const std::ffi::c_void,
                                    objCType: b"{CGPoint=dd}\0".as_ptr() as *const i8
                                ];

                                // setFrameOrigin: on main thread
                                let _: () = msg_send![
                                    ns_window,
                                    performSelectorOnMainThread: objc2::sel!(setFrameOrigin:),
                                    withObject: ns_value,
                                    waitUntilDone: false
                                ];

                                // makeKeyAndOrderFront: on main thread
                                let nil: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
                                let _: () = msg_send![
                                    ns_window,
                                    performSelectorOnMainThread: objc2::sel!(makeKeyAndOrderFront:),
                                    withObject: nil,
                                    waitUntilDone: false
                                ];

                                eprintln!("[shake-handler] dispatched to main thread");
                            }
                        }
                        eprintln!("[shake-handler] receiver closed, exiting");
                    });
                }
            }

            let _ = ch.update(cx, |view, _window, cx| {
                view.cursor_panel_window = Some(ch.clone());
                cx.notify();
            });
        }

        // 3. Desktop window — opencode-style coding UI, proper window (1280x800)
        let desktop_options = WindowOptions {
            app_id: Some("telekinesis-desktop".to_string()),
            titlebar: None,
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: point(px(80.0), px(60.0)),
                size: size(px(1280.0), px(800.0)),
            })),
            window_min_size: Some(Size {
                width: px(640.0),
                height: px(400.0),
            }),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            display_id: None,
            window_background: WindowBackgroundAppearance::Blurred,
            window_decorations: None,
            tabbing_identifier: None,
        };
        let overlay_for_desktop = overlay.clone();
        let desktop_handle = cx
            .open_window(desktop_options, |_win, cx| {
                cx.new(|cx| CompanionView::new(cx, Some(overlay_for_desktop), PanelKind::Desktop))
            })
            .ok();

        let _tray = create_tray_icon();

        let poll = cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;

                // Global hotkey (Ctrl+Alt+Space) — show desktop window
                if let Some(hid) = hotkey_id {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                        if event.id == hid && event.state == HotKeyState::Pressed {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = desktop_handle {
                                    let _ = handle.update(cx, |_view, window, cx| {
                                        window.activate_window();
                                        cx.notify();
                                    });
                                }
                            });
                        }
                    }
                }

                // Tray menu events
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    match event.id.0.as_str() {
                        "show" => {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = desktop_handle {
                                    let _ = handle.update(cx, |_view, window, cx| {
                                        window.activate_window();
                                        cx.notify();
                                    });
                                }
                            });
                        }
                        "capture" => {
                            let _ = cx.update(|cx| {
                                if let Some(ref handle) = desktop_handle {
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
