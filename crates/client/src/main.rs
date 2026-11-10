//! 0xGG client.
//!
//! usage: gg-client [--server HOST[:PORT]] [--host] [--user NAME] [--loss PERCENT] [--lag MS]
//!                  [--login NAME:PASS] [--autojoin] [--bot] [--add-bots N]
//!
//! Without flags the login screen asks for the host's IP (prefilled with the
//! last one used), or HOST GAME runs the server inside this window.
//! `--host` presses HOST GAME at startup.
//!
//! `--loss 20` drops 20% of UDP packets in each direction (demo of loss tolerance).
//! `--lag 75` delays every UDP packet by 75 ms each way (150 ms RTT), to demo
//! prediction and interpolation on a LAN. In a match, F1 toggles prediction
//! and F2 toggles interpolation.
//! `--login` registers-or-logs-in automatically; `--autojoin` joins the first
//! open room (or creates one) and readies up. Handy for local 3-client tests.
//! `--bot` plays as the sparring bot. `--add-bots N` (1-2) creates a port,
//! asks the server for N bots and starts: one-command solo practice (hosts
//! its own server unless `--server` is given).
//! `--fullscreen` starts fullscreen; F11 or Alt+Enter toggles it any time.
//! The window can be resized freely: the UI scales to fit (see `screen`).
//! `GG_SCREENSHOT=out.png:SECONDS` saves the window to a PNG after SECONDS and exits.
//! `--server` skips the title screen; `--server 0xgg.duckdns.org` opens the
//! CLOUD tab. `GG_CLOUD_SERVER=host[:port]` replaces the CLOUD tab's server.

mod app;
mod game_view;
mod host;
mod net;
mod netcode;
mod probe;
mod screen;
mod sprites;
mod title;
mod ui;

use macroquad::prelude::*;

fn window_conf() -> Conf {
    Conf {
        window_title: "0xGG".to_string(),
        window_width: 1280,
        window_height: 720,
        // Any size works: the 1280x720 layout is scaled to fit (letterboxed).
        window_resizable: true,
        fullscreen: std::env::args().any(|a| a == "--fullscreen"),
        // Render at the display's real resolution. Without this, Retina /
        // HiDPI screens get a 1280x720 image stretched 2x by the OS (blurry).
        high_dpi: true,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let mut server: Option<String> = None;
    let mut host = false;
    let mut user = String::new();
    let mut loss = 0.0f32;
    let mut lag_ms = 0.0f64;
    let mut auto = app::Auto::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => server = args.next(),
            "--host" => host = true,
            "--user" => user = args.next().unwrap_or_default(),
            "--loss" => {
                loss = args
                    .next()
                    .and_then(|v| v.parse::<f32>().ok())
                    .unwrap_or(0.0)
                    .clamp(0.0, 90.0)
                    / 100.0
            }
            "--lag" => {
                lag_ms = args
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1000.0)
            }
            "--login" => {
                if let Some((u, p)) = args.next().as_deref().and_then(|v| v.split_once(':')) {
                    auto.login = Some((u.to_string(), p.to_string()));
                }
            }
            "--autojoin" => auto.join = true,
            "--bot" => auto.bot = true,
            "--fullscreen" => {} // read by window_conf
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

    // Solo practice with bots needs a server: host one unless told where to go.
    host |= auto.add_bots > 0 && server.is_none();
    // Dev flags (--login/--autojoin) default to a server on this machine;
    // a normal launch prefills the last server the player joined.
    let dev = auto.login.is_some() || auto.join;
    // Told where to connect: go straight to the login screen.
    let explicit_server = server.is_some();
    let server = server.unwrap_or_else(|| {
        if dev {
            "127.0.0.1".to_string()
        } else {
            host::load_last_server().unwrap_or_default()
        }
    });

    let mut app = app::App::new(server, user, loss);
    app.auto = auto;
    app.lag_ms = lag_ms;
    if dev || host || explicit_server {
        app.skip_title();
    }
    if host {
        app.start_hosting();
    }
    let mut fullscreen = std::env::args().any(|a| a == "--fullscreen");
    loop {
        screen::handle_fullscreen_toggle(&mut fullscreen);
        screen::begin_frame(ui::theme::BG);
        app.poll_network();
        app.run_auto();
        app.ui.begin_frame();
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
