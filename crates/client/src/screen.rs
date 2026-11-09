//! Resolution independence: every screen is laid out on a fixed 1280x720
//! virtual canvas, which is scaled uniformly to fit the real window
//! (windowed, resized or fullscreen), with letterbox bars if the aspect
//! ratio differs. Text is rasterized at the real output size, so it stays
//! sharp at any scale.

use std::cell::Cell;

use macroquad::prelude::*;

/// Virtual canvas size: all layout code uses these instead of the window size.
pub const VW: f32 = 1280.0;
pub const VH: f32 = 720.0;

thread_local! {
    /// (scale, offset_x, offset_y): virtual -> window logical pixels.
    static FIT: Cell<(f32, f32, f32)> = const { Cell::new((1.0, 0.0, 0.0)) };
}

/// Clear the window and point drawing at the virtual canvas. Call at the
/// start of every frame, before any drawing.
pub fn begin_frame(bg: Color) {
    set_default_camera();
    clear_background(Color::new(0.0, 0.0, 0.0, 1.0));
    let (sw, sh) = (screen_width(), screen_height());
    let scale = (sw / VW).min(sh / VH);
    let (w, h) = (VW * scale, VH * scale);
    let (ox, oy) = ((sw - w) / 2.0, (sh - h) / 2.0);
    FIT.with(|f| f.set((scale, ox, oy)));

    // The viewport is in physical pixels (HiDPI screens have 2+ per point).
    let dpi = screen_dpi_scale();
    let mut cam = Camera2D::from_display_rect(Rect::new(0.0, 0.0, VW, VH));
    // from_display_rect is y-up (made for render targets); the screen is y-down.
    cam.zoom.y = -cam.zoom.y;
    cam.viewport = Some((
        (ox * dpi).round() as i32,
        (oy * dpi).round() as i32,
        (w * dpi).round() as i32,
        (h * dpi).round() as i32,
    ));
    set_camera(&cam);
    draw_rectangle(0.0, 0.0, VW, VH, bg);
}

/// Mouse position on the virtual canvas.
pub fn mouse() -> Vec2 {
    let (scale, ox, oy) = FIT.with(|f| f.get());
    let (mx, my) = mouse_position();
    vec2((mx - ox) / scale, (my - oy) / scale)
}

/// Window points per virtual pixel right now (macroquad adds HiDPI on top).
pub fn scale() -> f32 {
    FIT.with(|f| f.get()).0
}

/// F11 or Alt+Enter: toggle fullscreen.
pub fn handle_fullscreen_toggle(fullscreen: &mut bool) {
    let alt = is_key_down(KeyCode::LeftAlt) || is_key_down(KeyCode::RightAlt);
    if is_key_pressed(KeyCode::F11) || (alt && is_key_pressed(KeyCode::Enter)) {
        *fullscreen = !*fullscreen;
        set_fullscreen(*fullscreen);
        if !*fullscreen {
            request_new_screen_size(VW, VH);
        }
    }
}
