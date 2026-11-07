//! Tiny immediate-mode UI on top of macroquad's drawing primitives:
//! buttons, text fields and text helpers in the 0xGG terminal style.

use macroquad::prelude::*;

pub mod theme {
    use macroquad::prelude::Color;
    pub const BG: Color = Color::new(0.035, 0.05, 0.07, 1.0);
    pub const PANEL: Color = Color::new(0.07, 0.1, 0.13, 1.0);
    pub const PANEL_HI: Color = Color::new(0.11, 0.15, 0.19, 1.0);
    pub const BORDER: Color = Color::new(0.2, 0.27, 0.33, 1.0);
    pub const TEXT: Color = Color::new(0.86, 0.9, 0.93, 1.0);
    pub const DIM: Color = Color::new(0.48, 0.55, 0.6, 1.0);
    pub const ACCENT: Color = Color::new(0.22, 1.0, 0.55, 1.0);
    pub const ERROR: Color = Color::new(1.0, 0.27, 0.3, 1.0);
    pub const WARN: Color = Color::new(1.0, 0.75, 0.25, 1.0);

    /// Player colors by slot (0x01..0x03): orange, blue, white — distinct for colorblind players too.
    pub fn slot_color(slot: u8) -> Color {
        match slot {
            1 => Color::new(1.0, 0.55, 0.15, 1.0),
            2 => Color::new(0.25, 0.6, 1.0, 1.0),
            _ => Color::new(0.93, 0.93, 0.93, 1.0),
        }
    }
}

pub fn text(s: &str, x: f32, y: f32, size: f32, color: Color) {
    draw_text(s, x, y, size, color);
}

pub fn text_width(s: &str, size: f32) -> f32 {
    measure_text(s, None, size as u16, 1.0).width
}

pub fn text_centered(s: &str, cx: f32, y: f32, size: f32, color: Color) {
    draw_text(s, cx - text_width(s, size) / 2.0, y, size, color);
}

pub fn panel(r: Rect) {
    draw_rectangle(r.x, r.y, r.w, r.h, theme::PANEL);
    draw_rectangle_lines(r.x, r.y, r.w, r.h, 1.0, theme::BORDER);
}

#[derive(Default)]
pub struct Ui {
    pub focus: Option<u32>,
    mouse: Vec2,
    clicked: bool,
    hit_widget: bool,
    typed: Vec<char>,
    backspace: bool,
    enter: bool,
    tab: bool,
}

impl Ui {
    pub fn begin_frame(&mut self) {
        self.mouse = mouse_position().into();
        self.clicked = is_mouse_button_pressed(MouseButton::Left);
        self.hit_widget = false;
        self.typed.clear();
        while let Some(c) = get_char_pressed() {
            if !c.is_control() {
                self.typed.push(c);
            }
        }
        self.backspace = is_key_pressed(KeyCode::Backspace);
        self.enter = is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter);
        self.tab = is_key_pressed(KeyCode::Tab);
    }

    pub fn end_frame(&mut self) {
        if self.clicked && !self.hit_widget {
            self.focus = None;
        }
    }

    pub fn button(&mut self, r: Rect, label: &str) -> bool {
        self.button_colored(r, label, theme::ACCENT)
    }

    pub fn button_colored(&mut self, r: Rect, label: &str, color: Color) -> bool {
        let hover = r.contains(self.mouse);
        draw_rectangle(r.x, r.y, r.w, r.h, if hover { theme::PANEL_HI } else { theme::PANEL });
        draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, if hover { color } else { theme::BORDER });
        let size = 22.0;
        text_centered(label, r.x + r.w / 2.0, r.y + r.h / 2.0 + 7.0, size, if hover { color } else { theme::TEXT });
        if hover && self.clicked {
            self.hit_widget = true;
            return true;
        }
        false
    }

    /// Single-line text input. Returns true when Enter is pressed while focused.
    /// `next` is the field Tab jumps to.
    #[allow(clippy::too_many_arguments)]
    pub fn text_field(
        &mut self,
        id: u32,
        r: Rect,
        label: &str,
        value: &mut String,
        masked: bool,
        max_len: usize,
        next: Option<u32>,
    ) -> bool {
        if self.clicked && r.contains(self.mouse) {
            self.focus = Some(id);
            self.hit_widget = true;
        }
        let focused = self.focus == Some(id);
        let mut submitted = false;
        if focused {
            for &c in &self.typed {
                if value.chars().count() < max_len {
                    value.push(c);
                }
            }
            if self.backspace {
                value.pop();
            }
            submitted = self.enter;
            if self.tab {
                self.focus = next;
                self.tab = false; // consumed
            }
        }

        if !label.is_empty() {
            text(label, r.x, r.y - 7.0, 18.0, theme::DIM);
        }
        draw_rectangle(r.x, r.y, r.w, r.h, theme::PANEL);
        draw_rectangle_lines(r.x, r.y, r.w, r.h, 2.0, if focused { theme::ACCENT } else { theme::BORDER });
        let shown = if masked { "*".repeat(value.chars().count()) } else { value.clone() };
        let size = 22.0;
        text(&shown, r.x + 10.0, r.y + r.h / 2.0 + 7.0, size, theme::TEXT);
        if focused && (get_time() * 2.0) as i64 % 2 == 0 {
            let cx = r.x + 12.0 + text_width(&shown, size);
            draw_rectangle(cx, r.y + 8.0, 2.0, r.h - 16.0, theme::ACCENT);
        }
        submitted
    }
}
