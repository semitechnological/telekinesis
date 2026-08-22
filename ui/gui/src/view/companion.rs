use crepuscularity_gpui::prelude::*;
use crepuscularity_macros::view_file;
use gpui::{ClickEvent, *};
use telekinesis_gui::host::{composer_input_from_key, CompanionHost, ComposerInput};

use crate::view::overlay::CursorOverlay;

struct ViewMessage {
    role: SharedString,
    content: SharedString,
    is_tool: bool,
    is_user: bool,
    is_error: bool,
}

#[cfg(target_os = "macos")]
use crate::platform::macos::with_ns_window;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PanelKind {
    Cursor,
    Desktop,
}

pub struct CompanionView {
    host: Entity<CompanionHost>,
    overlay: Option<Entity<CursorOverlay>>,
    panel_kind: PanelKind,
    pub cursor_panel_window: Option<gpui::WindowHandle<CompanionView>>,
    last_poll_generation: u64,
    #[allow(dead_code)]
    sidebar_expanded: bool,
    #[allow(dead_code)]
    sessions_expanded: bool,
    #[allow(dead_code)]
    recent_expanded: bool,
}

impl CompanionView {
    pub fn new(
        _cx: &mut Context<Self>,
        host: Entity<CompanionHost>,
        overlay: Option<Entity<CursorOverlay>>,
        panel_kind: PanelKind,
    ) -> Self {
        Self {
            host,
            overlay,
            panel_kind,
            cursor_panel_window: None,
            last_poll_generation: 0,
            sidebar_expanded: true,
            sessions_expanded: true,
            recent_expanded: false,
        }
    }

    pub fn tick(&mut self, cx: &mut Context<Self>) {
        let tick = self.host.update(cx, |host, _cx| host.poll());
        if let Some(point) = tick.point {
            if let Some(overlay) = &self.overlay {
                overlay.update(cx, |overlay, cx| {
                    overlay.point_to(point.x, point.y, point.label, cx);
                });
            }
        }
        let generation = self.host.read(cx).poll_generation();
        if tick.dirty || generation != self.last_poll_generation {
            self.last_poll_generation = generation;
            cx.notify();
        }
    }

    #[allow(dead_code)]
    fn toggle_sidebar(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_expanded = !self.sidebar_expanded;
        cx.notify();
    }

    #[allow(dead_code)]
    fn toggle_sessions(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.sessions_expanded = !self.sessions_expanded;
        cx.notify();
    }

    #[allow(dead_code)]
    fn toggle_recent(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.recent_expanded = !self.recent_expanded;
        cx.notify();
    }

    fn close_window(&mut self, _: &ClickEvent, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

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

    fn send_prompt(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.send_prompt());
        cx.notify();
    }

    pub fn capture_screen(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.capture_screen());
        cx.notify();
    }

    fn interrupt(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.interrupt());
        cx.notify();
    }

    fn start_login(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.start_login(None));
        cx.notify();
    }

    fn use_computer(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.use_computer());
        cx.notify();
    }

    fn use_coding(&mut self, _: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.host.update(cx, |host, _cx| host.use_coding());
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
        self.tick(cx);
        let key = &event.keystroke;
        let pending = self.host.read(cx).snapshot().permission_pending;
        if pending {
            if key.key == "y" {
                self.host
                    .update(cx, |host, _cx| host.resolve_permission(true));
                cx.notify();
                return;
            }
            if key.key == "n" || key.key == "escape" {
                self.host
                    .update(cx, |host, _cx| host.resolve_permission(false));
                cx.notify();
                return;
            }
            return;
        }
        let action = composer_input_from_key(
            &key.key,
            key.modifiers.shift,
            key.modifiers.secondary(),
            key.modifiers.control,
            key.modifiers.alt,
            key.key_char.as_deref(),
        );
        if let Some(action) = action {
            let clipboard = if action == ComposerInput::Paste {
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .filter(|text| !text.is_empty())
            } else {
                None
            };
            self.host.update(cx, |host, _cx| {
                host.apply_composer_input(action, clipboard.as_deref());
            });
            cx.notify();
        }
    }
}

impl Render for CompanionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.tick(cx);

        let snap = self.host.read(cx).snapshot();
        let model: SharedString = snap.model.into();
        let busy = snap.busy;
        let context_pct = snap.context_pct;
        let connected = snap.connected;
        let status: SharedString = snap.status.into();
        let provider: SharedString = snap.provider.into();
        let session_name: SharedString = snap.session_name.into();
        let computer_active = snap.computer_active;
        let coding_active = snap.coding_active;
        let login_busy = snap.login_busy;
        let _sidebar_expanded = self.sidebar_expanded;
        let _sessions_expanded = self.sessions_expanded;
        let _recent_expanded = self.recent_expanded;

        let input: SharedString = if snap.input.is_empty() {
            if login_busy {
                "complete login in the browser...".into()
            } else if self.panel_kind == PanelKind::Cursor {
                "ask anything...".into()
            } else if connected {
                "Describe what to change...".into()
            } else {
                "Run `tk login openai` or click login".into()
            }
        } else {
            snap.input.into()
        };

        let messages: Vec<ViewMessage> = snap
            .messages
            .into_iter()
            .map(|message| ViewMessage {
                role: message.role.into(),
                content: message.content.into(),
                is_tool: message.is_tool,
                is_user: message.is_user,
                is_error: message.is_error,
            })
            .collect();

        match self.panel_kind {
            PanelKind::Cursor => {
                let messages = messages.iter();
                view_file!("cursor_panel.crepus").on_key_down(cx.listener(Self::handle_key))
            }
            PanelKind::Desktop => {
                let messages = messages.iter();
                view_file!("desktop.crepus").on_key_down(cx.listener(Self::handle_key))
            }
        }
    }
}
