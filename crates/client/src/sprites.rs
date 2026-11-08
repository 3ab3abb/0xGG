//! Sprite art. Purely cosmetic: game state and hitboxes are unchanged.
//!
//! Two sources (see `assets/CREDITS.md`):
//! - `assets/sprites/*.png`: weapons and effects from the CC0 "Free 2D Animated
//!   Vector Game Character Sprites" pack, embedded in the binary.
//! - `assets/seeker/`: the Seeker_Lite character sheets (Primitive Pixels). Their
//!   license forbids redistributing the art, so they are gitignored and loaded at
//!   runtime when present. Without them players are drawn as procedural packets.

use std::path::{Path, PathBuf};

use macroquad::prelude::*;

/// Vector art is stored at this many texels per world pixel (2x the HiDPI screen).
const VECTOR_TEXELS: f32 = 8.0;

/// One horizontal strip of equally sized frames, with the feet pivot in frame pixels.
pub struct Sheet {
    pub tex: Texture2D,
    pub fw: f32,
    pub fh: f32,
    pub n: usize,
    pub pivot: Vec2,
}

impl Sheet {
    fn from_image(img: &Image, fw: u16, fh: u16, pivot: (f32, f32)) -> Sheet {
        let tex = Texture2D::from_image(img);
        tex.set_filter(FilterMode::Nearest);
        Sheet {
            tex,
            fw: fw as f32,
            fh: fh as f32,
            n: (img.width / fw) as usize,
            pivot: vec2(pivot.0, pivot.1),
        }
    }

    /// Draw frame `i` with its feet pivot at screen point `feet`, `scale` screen
    /// px per frame px, mirrored when facing left.
    pub fn draw(&self, i: usize, feet: Vec2, right: bool, scale: f32, color: Color) {
        let i = i.min(self.n - 1);
        let px = if right { self.pivot.x } else { self.fw - self.pivot.x };
        let pos = snap(feet - vec2(px, self.pivot.y) * scale);
        draw_texture_ex(
            &self.tex,
            pos.x,
            pos.y,
            color,
            DrawTextureParams {
                dest_size: Some(vec2(self.fw, self.fh) * scale),
                source: Some(Rect::new(i as f32 * self.fw, 0.0, self.fw, self.fh)),
                flip_x: !right,
                ..Default::default()
            },
        );
    }
}

/// Round to whole physical pixels so pixel art stays crisp on any DPI.
fn snap(p: Vec2) -> Vec2 {
    let d = screen_dpi_scale().max(1.0);
    (p * d).round() / d
}

/// The Seeker character, recoloured for one player slot.
pub struct Seeker {
    pub idle: Sheet,
    pub run: Sheet,
    pub jump: Sheet,
    /// Attack1 with the slash pixels removed (the body and sword).
    pub attack_body: Sheet,
    /// Only the slash pixels of Attack1, drawn scaled to the blade's real reach.
    pub attack_slash: Sheet,
    pub hurt: Sheet,
}

/// Seeker is 24 px tall vs the 14 px hitbox; labels go this much higher.
pub const SEEKER_EXTRA_H: f32 = 10.0;

pub struct Art {
    pub pistol: Texture2D,
    pub shotgun: Texture2D,
    pub fork_bomb: Texture2D,
    pub muzzle: Texture2D,
    pub drops: [Texture2D; 3],
    /// Indexed by slot - 1 (slots 1..=3); None when the sheets aren't installed.
    pub seeker: Option<Vec<Seeker>>,
}

fn png(bytes: &[u8]) -> Texture2D {
    Texture2D::from_file_with_format(bytes, Some(ImageFormat::Png))
}

impl Art {
    pub fn load(slot_color: impl Fn(u8) -> Color) -> Art {
        let seeker = find_seeker_dir().and_then(|dir| {
            let sheets = (1..=3u8)
                .map(|slot| load_seeker(&dir, slot_color(slot), slot == 3))
                .collect::<Option<Vec<_>>>();
            if sheets.is_none() {
                eprintln!("seeker sprites in {} are incomplete; using packets", dir.display());
            }
            sheets
        });
        Art {
            pistol: png(include_bytes!("../../../assets/sprites/ping_pistol.png")),
            shotgun: png(include_bytes!("../../../assets/sprites/broadcast_shotgun.png")),
            fork_bomb: png(include_bytes!("../../../assets/sprites/fork_bomb.png")),
            muzzle: png(include_bytes!("../../../assets/sprites/muzzle.png")),
            drops: [
                png(include_bytes!("../../../assets/sprites/drop_0.png")),
                png(include_bytes!("../../../assets/sprites/drop_1.png")),
                png(include_bytes!("../../../assets/sprites/drop_2.png")),
            ],
            seeker,
        }
    }

    pub fn seeker(&self, slot: u8) -> Option<&Seeker> {
        let s = self.seeker.as_ref()?;
        s.get((slot.clamp(1, 3) - 1) as usize)
    }
}

/// Draw a vector sprite (stored at `VECTOR_TEXELS` per world px). `anchor` is a
/// point inside the texture as a fraction of its size (e.g. the grip) that lands
/// on screen point `at`. `angle` rotates around that point; `flip` mirrors it.
pub fn draw_vector(
    tex: &Texture2D,
    at: Vec2,
    anchor: Vec2,
    world_scale: f32,
    angle: f32,
    flip: bool,
    color: Color,
) {
    let size = tex.size() / VECTOR_TEXELS * world_scale;
    let ax = if flip { 1.0 - anchor.x } else { anchor.x };
    let pos = at - vec2(ax * size.x, anchor.y * size.y);
    draw_texture_ex(
        tex,
        pos.x,
        pos.y,
        color,
        DrawTextureParams {
            dest_size: Some(size),
            rotation: angle,
            pivot: Some(at),
            flip_x: flip,
            ..Default::default()
        },
    );
}

fn find_seeker_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(dir) = std::env::var("GG_ASSETS") {
        candidates.push(PathBuf::from(dir).join("seeker"));
    }
    candidates.push(PathBuf::from("assets/seeker"));
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(exe_dir.join("assets/seeker"));
        candidates.push(exe_dir.join("../../assets/seeker"));
    }
    candidates.push(PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/seeker")));
    candidates
        .into_iter()
        .find(|d| d.join("Seeker_Void_Idle_30x30_24f.png").is_file())
}

fn load_image(dir: &Path, name: &str) -> Option<Image> {
    let bytes = std::fs::read(dir.join(name)).ok()?;
    Image::from_file_with_format(&bytes, Some(ImageFormat::Png)).ok()
}

fn load_seeker(dir: &Path, target: Color, white: bool) -> Option<Seeker> {
    let sheet = |name: &str, fw, fh, pivot| {
        let mut img = load_image(dir, name)?;
        recolor(&mut img, target, white);
        Some(Sheet::from_image(&img, fw, fh, pivot))
    };
    let mut attack = load_image(dir, "Seeker_Void_Attack1_90x50_8f.png")?;
    recolor(&mut attack, target, white);
    let (body, slash) = split_slash(&attack, 90);
    Some(Seeker {
        idle: sheet("Seeker_Void_Idle_30x30_24f.png", 30, 30, (15.0, 28.0))?,
        run: sheet("Seeker_Void_Run_30x30_10f.png", 30, 30, (17.0, 28.0))?,
        jump: sheet("Seeker_Void_JumpFall_30x30_28f.png", 30, 30, (16.0, 28.0))?,
        attack_body: Sheet::from_image(&body, 90, 50, (16.0, 38.0)),
        attack_slash: Sheet::from_image(&slash, 90, 50, (16.0, 38.0)),
        hurt: sheet("Seeker_Void_Hurt_55x80_9f.png", 55, 80, (26.0, 52.0))?,
    })
}

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

/// Shift Seeker's purple cloak to the slot colour (eyes and slash untouched).
fn recolor(img: &mut Image, target: Color, white: bool) {
    let (th, ts, _) = rgb_to_hsv(target.r, target.g, target.b);
    for px in img.get_image_data_mut() {
        if px[3] == 0 {
            continue;
        }
        let (h, s, v) = rgb_to_hsv(px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0);
        if !(0.65..0.95).contains(&h) || s <= 0.15 {
            continue;
        }
        let (s2, v2) = if white {
            (0.05, (v * 2.4).min(1.0))
        } else {
            (ts * (s * 1.6).min(1.0), (v * 1.9).min(1.0))
        };
        let (r, g, b) = hsv_to_rgb(th, s2, v2);
        px[0] = (r * 255.0) as u8;
        px[1] = (g * 255.0) as u8;
        px[2] = (b * 255.0) as u8;
    }
}

/// Split Attack1 into (body+sword, slash): the slash is the bright green pixels
/// in front of the body.
fn split_slash(img: &Image, fw: u16) -> (Image, Image) {
    let mut body = img.clone();
    let mut slash = img.clone();
    let w = img.width as usize;
    let (b, s) = (body.get_image_data_mut(), slash.get_image_data_mut());
    for (i, px) in img.get_image_data().iter().enumerate() {
        let fx = (i % w) % fw as usize;
        let (h, sat, v) = rgb_to_hsv(px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0);
        let is_slash = px[3] > 0 && fx >= 22 && (0.25..0.55).contains(&h) && sat > 0.3 && v > 0.45;
        if is_slash {
            b[i] = [0, 0, 0, 0];
        } else {
            s[i] = [0, 0, 0, 0];
        }
    }
    (body, slash)
}
