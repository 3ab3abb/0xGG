//! Title screen: logo, menu (PLAY / HOST GAME / CREDITS / QUIT), packets
//! streaming along network links, the three Seekers, and the art credits.

use macroquad::prelude::*;

use crate::screen::{VH, VW};
use crate::sprites::{Art, Seeker};
use crate::ui::{panel, text, text_centered, text_width, theme, Ui};

pub enum TitleAction {
    None,
    Play,
    Host,
    Quit,
}

pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// Credits panel lines: (heading, body).
const CREDITS: &[(&str, &str)] = &[
    (
        "GAME, CODE & NETCODE  -  CS411 COMPUTER NETWORKS, FALL 2026",
        "Kais Abaab, Mohamed Habib El Kateb, Fatma Ouahada, Omar Ben Tkhayat",
    ),
    ("CHARACTER ART", "\"Hooded Seeker\" by Primitive Pixels"),
    (
        "WEAPON & EFFECT SPRITES",
        "\"Free 2D Animated Vector Game Character Sprites\" by Raphael Goncalves (rgsdev), CC0",
    ),
    ("BUILT WITH", "Rust, macroquad, raw std::net TCP + UDP sockets"),
];

pub struct Title {
    art: Art,
    show_credits: bool,
    started: f64,
}

/// Cheap deterministic hash -> [0, 1).
fn h(i: u32, salt: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65536.0
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, a)
}

impl Title {
    pub fn new() -> Self {
        Title {
            art: Art::load(theme::slot_color),
            show_credits: false,
            started: get_time(),
        }
    }

    pub fn draw(&mut self, ui: &mut Ui) -> TitleAction {
        let t = get_time() - self.started;
        let action = self.layout(ui, t);
        self.footer();
        if self.show_credits {
            self.credits(ui);
            return TitleAction::None;
        }
        if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
            return TitleAction::Play;
        }
        action
    }

    // ------------------------------------------------------------ pieces

    fn seeker(&self, slot: u8) -> Option<&Seeker> {
        self.art.seeker(slot)
    }

    /// One Seeker (or a packet if the art is missing) with feet at `feet`.
    fn draw_seeker(&self, slot: u8, feet: Vec2, scale: f32, t: f64, running: bool, right: bool) {
        match self.seeker(slot) {
            Some(s) => {
                let (sheet, fps) = if running { (&s.run, 14.0) } else { (&s.idle, 10.0) };
                let frame = ((t + slot as f64 * 0.37) * fps) as usize % sheet.n;
                sheet.draw(frame, feet, right, scale, WHITE);
            }
            None => {
                let (w, hh) = (12.0 * scale, 14.0 * scale);
                let c = theme::slot_color(slot);
                draw_rectangle(feet.x - w / 2.0, feet.y - hh, w, hh, c);
                draw_rectangle(feet.x - w / 2.0, feet.y - hh, w, 4.0 * scale, with_alpha(BLACK, 0.4));
            }
        }
    }

    /// Big "0xGG" with a short RGB-split glitch every few seconds.
    fn logo(&self, cx: f32, y: f32, size: f32, t: f64) {
        let phase = t % 3.7;
        if phase < 0.12 {
            let d = size * 0.025;
            text_centered("0xGG", cx - d, y, size, with_alpha(theme::ERROR, 0.7));
            text_centered("0xGG", cx + d, y, size, with_alpha(theme::slot_color(2), 0.7));
        }
        text_centered("0xGG", cx, y, size, theme::ACCENT);
    }

    /// Faint dot grid.
    fn grid(&self) {
        let c = with_alpha(theme::BORDER, 0.35);
        let mut y = 16.0;
        while y < VH {
            let mut x = 16.0;
            while x < VW {
                draw_rectangle(x, y, 2.0, 2.0, c);
                x += 32.0;
            }
            y += 32.0;
        }
    }

    /// Horizontal "network links" with packets streaming along them.
    fn links(&self, t: f64, lanes: &[f32], alpha: f32) {
        for (li, &y) in lanes.iter().enumerate() {
            draw_line(0.0, y, VW, y, 1.0, with_alpha(theme::BORDER, 0.5 * alpha));
            for k in 0..5u32 {
                let id = li as u32 * 16 + k;
                let speed = 60.0 + 140.0 * h(id, 1);
                let dir = if li % 2 == 0 { 1.0 } else { -1.0 };
                let span = VW + 120.0;
                let x0 = h(id, 2) * span;
                let mut x = (x0 + dir * speed * t as f32).rem_euclid(span) - 60.0;
                if dir < 0.0 {
                    x = VW - x;
                }
                let slot = 1 + (id % 3) as u8;
                let c = if h(id, 3) < 0.35 {
                    theme::slot_color(slot)
                } else {
                    theme::ACCENT
                };
                let w = 10.0 + 10.0 * h(id, 4);
                draw_rectangle(x - w / 2.0, y - 3.0, w, 6.0, with_alpha(c, 0.55 * alpha));
                draw_rectangle(x - w / 2.0, y - 3.0, 3.0, 6.0, with_alpha(c, 0.9 * alpha));
            }
        }
    }

    /// Vertical menu; returns the clicked action.
    fn menu_vertical(&mut self, ui: &mut Ui, x: f32, y: f32, w: f32) -> TitleAction {
        let items = [
            ("PLAY", TitleAction::Play),
            ("HOST GAME", TitleAction::Host),
            ("CREDITS", TitleAction::None),
            ("QUIT", TitleAction::Quit),
        ];
        let mut out = TitleAction::None;
        for (i, (label, action)) in items.into_iter().enumerate() {
            let r = Rect::new(x, y + i as f32 * 58.0, w, 46.0);
            let color = if label == "QUIT" { theme::ERROR } else { theme::ACCENT };
            if ui.button_colored(r, label, color) {
                if label == "CREDITS" {
                    self.show_credits = true;
                } else {
                    out = action;
                }
            }
        }
        out
    }

    fn footer(&self) {
        text(VERSION, 24.0, VH - 16.0, 16.0, theme::DIM);
        let credit = "Character art by Primitive Pixels   -   weapon art by rgsdev (CC0)";
        let w = text_width(credit, 16.0);
        text(credit, VW - w - 24.0, VH - 16.0, 16.0, theme::DIM);
    }

    fn credits(&mut self, ui: &mut Ui) {
        draw_rectangle(0.0, 0.0, VW, VH, with_alpha(theme::BG, 0.85));
        let r = Rect::new(VW / 2.0 - 420.0, 110.0, 840.0, 470.0);
        panel(r);
        draw_rectangle(r.x, r.y, r.w, 3.0, theme::ACCENT);
        text("CREDITS", r.x + 28.0, r.y + 50.0, 32.0, theme::ACCENT);
        let mut y = r.y + 100.0;
        for (head, body) in CREDITS {
            text(head, r.x + 28.0, y, 16.0, theme::DIM);
            text(body, r.x + 28.0, y + 26.0, 20.0, theme::TEXT);
            y += 72.0;
        }
        if ui.button(Rect::new(r.x + r.w - 168.0, r.y + r.h - 64.0, 140.0, 44.0), "BACK")
            || is_key_pressed(KeyCode::Escape)
        {
            self.show_credits = false;
        }
    }

    // ------------------------------------------------------------ layout

    /// Centered logo and menu, the Seekers idling on a platform, links behind.
    fn layout(&mut self, ui: &mut Ui, t: f64) -> TitleAction {
        self.grid();
        self.links(t, &[150.0, 470.0, 610.0], 1.0);
        let cx = VW / 2.0;
        text_centered("30 78 47 47", cx, 72.0, 20.0, theme::DIM);
        self.logo(cx, 190.0, 150.0, t);
        text_centered("Last packet standing.", cx, 236.0, 26.0, theme::TEXT);

        // Three packets on a platform, left of the menu.
        let base = 520.0;
        draw_rectangle(120.0, base, 360.0, 6.0, theme::ACCENT);
        for (i, slot) in [1u8, 2, 3].into_iter().enumerate() {
            let x = 180.0 + i as f32 * 120.0;
            self.draw_seeker(slot, vec2(x, base), 4.0, t, false, i < 2);
            text_centered(&format!("0x{slot:02X}"), x, base - 120.0, 18.0, theme::slot_color(slot));
        }
        self.menu_vertical(ui, cx - 140.0, 300.0, 280.0)
    }

}
