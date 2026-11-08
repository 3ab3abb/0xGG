//! 0xGG client.
//!
//! usage: gg-client [--server HOST[:PORT]] [--user NAME] [--loss PERCENT]
//!                  [--login NAME:PASS] [--autojoin] [--bot] [--add-bots N]
//!
//! `--loss 20` drops 20% of UDP packets in each direction (demo of loss tolerance).
//! `--login` registers-or-logs-in automatically; `--autojoin` joins the first
//! open room (or creates one) and readies up. Handy for local 3-client tests.
//! `--bot` plays as the sparring bot. `--add-bots N` (1-2) creates a port,
//! asks the server for N bots and starts: one-command solo practice.
//! `GG_SCREENSHOT=out.png:SECONDS` saves the window to a PNG after SECONDS and exits.

mod app;
mod game_view;
mod net;
mod sprites;
mod ui;

use macroquad::prelude::*;

fn window_conf() -> Conf {
    Conf {
        window_title: "0xGG".to_string(),
        window_width: 1280,
        window_height: 720,
        window_resizable: false,
        // Render at the display's real resolution. Without this, Retina /
        // HiDPI screens get a 1280x720 image stretched 2x by the OS (blurry).
        high_dpi: true,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let mut server = "127.0.0.1:7777".to_string();
    let mut user = String::new();
    let mut loss = 0.0f32;
    let mut auto = app::Auto::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => server = args.next().unwrap_or(server),
            "--user" => user = args.next().unwrap_or_default(),
            "--loss" => {
                loss = args
                    .next()
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0)
                    .clamp(0.0, 90.0)
                    / 100.0
            }
            "--login" => {
                if let Some((u, p)) = args.next().as_deref().and_then(|v| v.split_once(':')) {
                    auto.login = Some((u.to_string(), p.to_string()));
                }
            }
            "--autojoin" => auto.join = true,
            "--bot" => auto.bot = true,
            "--add-bots" => {
                auto.add_bots = args.next().and_then(|v| v.parse().ok()).unwrap_or(2).min(2);
                auto.join = true;
            }
            _ => eprintln!("ignoring unknown argument {arg}"),
        }
    }
    macroquad::rand::srand((get_time() * 1e6) as u64 ^ std::process::id() as u64);

    let screenshot: Option<(String, f64)> = std::env::var("GG_SCREENSHOT").ok().and_then(|v| {
        let (path, secs) = v.rsplit_once(':')?;
        Some((path.to_string(), secs.parse().ok()?))
    });

    let mut app = app::App::new(server, user, loss);
    app.auto = auto;
    loop {
        app.poll_network();
        app.run_auto();
        app.ui.begin_frame();
        clear_background(ui::theme::BG);
        app.draw();
        app.ui.end_frame();
        if let Some((path, secs)) = &screenshot {
            if get_time() >= *secs {
                get_screen_data().export_png(path);
                return;
            }
        }
        next_frame().await;
    }
}
