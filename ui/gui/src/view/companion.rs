use crepuscularity_gpui::prelude::*;
use crepuscularity_macros::view_file;
use gpui::{ClickEvent, *};

use crate::agent::{setup_agents, AgentSetup};
use crate::view::overlay::CursorOverlay;
use crate::view::session::{AgentSession, CompanionEvent, MessageItem, SessionKind};

#[cfg(target_os = "macos")]
use crate::platform::macos::with_ns_window;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PanelKind {
    Cursor,
    Desktop,
}

pub struct CompanionView {
    input: String,
    sessions: Vec<AgentSession>,
    active_session: usize,
    event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CompanionEvent>>,
    rt_handle: Option<tokio::runtime::Handle>,
    event_tx: Option<tokio::sync::mpsc::UnboundedSender<CompanionEvent>>,
    overlay: Option<Entity<CursorOverlay>>,
    panel_kind: PanelKind,
    pub cursor_panel_window: Option<gpui::WindowHandle<CompanionView>>,
    /// Desktop sidebar expanded
    sidebar_expanded: bool,
    /// Sessions section expanded
    sessions_expanded: bool,
    /// Recent section expanded
    recent_expanded: bool,
}

impl CompanionView {
    pub fn new(
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

    fn poll_events(&mut self, _cx: &mut Context<Self>) -> bool {
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
                    let _overlay = self.overlay.clone();
                    if let Some(session) = self.sessions.get_mut(idx) {
                        session.handle_rx4_event(e);
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

    pub fn capture_screen(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
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
