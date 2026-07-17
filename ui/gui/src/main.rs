use std::time::Duration;

use crepuscularity_gpui::prelude::*;
use gpui::{ClickEvent, *};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use tray_icon::menu::MenuEvent;

mod shake;
mod agent;
mod platform;
mod view;
mod tray;

use crate::platform::macos::{
    configure_app_as_accessory, configure_borderless_overlay, configure_floating_key_panel,
};
use crate::view::companion::{CompanionView, PanelKind};
use crate::view::overlay::CursorOverlay;

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
                use crate::platform::macos::with_ns_window;
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

        let _tray = tray::create_tray_icon();

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
