use gpui::{App, Window, WindowHandle};

#[cfg(target_os = "macos")]
pub fn with_ns_window<F, R>(window: &Window, f: F) -> Option<R>
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

/// Configure a GPUI window as a borderless floating overlay (clicky pattern).
///
/// - `click_through = true`  → mouse events pass through to apps below (overlay)
/// - `click_through = false` → window receives mouse events but doesn't activate app (panel)
///
/// Sets: no shadow, non-opaque, transparent background, floating level (3),
/// borderless style, hidesOnDeactivate = false.
#[cfg(target_os = "macos")]
pub fn configure_borderless_overlay<V>(
    window: &WindowHandle<V>,
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
pub fn configure_borderless_overlay<V>(
    _window: &WindowHandle<V>,
    _click_through: bool,
    _cx: &mut App,
) where
    V: 'static,
{
}

/// Configure a floating panel that CAN become key window (receive keyboard input)
/// while still floating above other apps. Used for the cursor pill.
#[cfg(target_os = "macos")]
pub fn configure_floating_key_panel<V>(
    window: &WindowHandle<V>,
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
pub fn configure_floating_key_panel<V>(
    _window: &WindowHandle<V>,
    _cx: &mut App,
) where
    V: 'static,
{
}

/// Set the app to accessory mode (menu bar app, no dock icon).
#[cfg(target_os = "macos")]
pub fn configure_app_as_accessory() {
    use objc2_app_kit::{NSApp, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApp(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn configure_app_as_accessory() {}
