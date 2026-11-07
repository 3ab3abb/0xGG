use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;

use gg_server::Config;
use gg_shared::constants::DEFAULT_PORT;

const USAGE: &str = "usage: gg-server [--bind ADDR:PORT] [--users FILE] [--name NAME]
  --bind   address to listen on (default 0.0.0.0:7777, TCP and UDP)
  --users  account database file (default users.json)
  --name   server name shown to clients (default 0xGG)";

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut bind = format!("0.0.0.0:{DEFAULT_PORT}");
    let mut users_path = PathBuf::from("users.json");
    let mut server_name = "0xGG".to_string();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("missing value for {arg}"));
        let res = match arg.as_str() {
            "--bind" => value().map(|v| bind = v),
            "--users" => value().map(|v| users_path = v.into()),
            "--name" => value().map(|v| server_name = v),
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => Err(format!("unknown argument {arg}")),
        };
        if let Err(e) = res {
            eprintln!("error: {e}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    }

    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot bind {bind}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let udp_port = listener.local_addr().map(|a| a.port()).unwrap_or(DEFAULT_PORT);

    match gg_server::run(listener, Config { server_name, users_path, udp_port }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
