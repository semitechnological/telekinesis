use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub fn create_tray_icon() -> TrayIcon {
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
