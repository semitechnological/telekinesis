use std::time::Duration;

use crepuscularity_gpui::prelude::*;
use gpui::*;

#[derive(Default)]
pub struct CursorOverlay {
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
    pub fn point_to(&mut self, x: f32, y: f32, label: String, cx: &mut Context<Self>) {
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
