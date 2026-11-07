//! 0xGG client.
//!
//! usage: gg-client [--server HOST[:PORT]] [--user NAME]

mod app;
mod net;
mod ui;

use macroquad::prelude::*;

fn window_conf() -> Conf {
    Conf {
        window_title: "0xGG".to_string(),
        window_width: 1280,
        window_height: 720,
        window_resizable: false,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let mut server = "127.0.0.1:7777".to_string();
    let mut user = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => server = args.next().unwrap_or(server),
            "--user" => user = args.next().unwrap_or_default(),
            _ => eprintln!("ignoring unknown argument {arg}"),
        }
    }
    macroquad::rand::srand((get_time() * 1e6) as u64 ^ std::process::id() as u64);

    let mut app = app::App::new(server, user);
    loop {
        app.poll_network();
        app.ui.begin_frame();
        clear_background(ui::theme::BG);
        app.draw();
        app.ui.end_frame();
        next_frame().await;
    }
}
