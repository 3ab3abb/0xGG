//! Embeds the Seeker character sheets into the client binary when they are
//! present at build time.
//!
//! Their license (Primitive Pixels) allows using the art in our game but not
//! sharing the files themselves, so the PNGs are never committed or shipped
//! as loose files: they only reach players compiled into the game.
//! Looked up in `$GG_SEEKER_DIR`, then `assets/seeker/`. Missing sheets are
//! fine: the client falls back to loading them at runtime, then to packets.

use std::env;
use std::fmt::Write as _;
use std::path::PathBuf;

const SHEETS: [&str; 5] = [
    "Seeker_Void_Idle_30x30_24f.png",
    "Seeker_Void_Run_30x30_10f.png",
    "Seeker_Void_JumpFall_30x30_28f.png",
    "Seeker_Void_Attack1_90x50_8f.png",
    "Seeker_Void_Hurt_55x80_9f.png",
];

fn main() {
    println!("cargo:rerun-if-env-changed=GG_SEEKER_DIR");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let mut dirs = Vec::new();
    if let Ok(d) = env::var("GG_SEEKER_DIR") {
        dirs.push(PathBuf::from(d));
    }
    dirs.push(manifest.join("../../assets/seeker"));
    for d in &dirs {
        println!("cargo:rerun-if-changed={}", d.display());
        for s in SHEETS {
            println!("cargo:rerun-if-changed={}", d.join(s).display());
        }
    }

    let found = dirs
        .iter()
        .find(|d| SHEETS.iter().all(|s| d.join(s).is_file()));
    let mut code = String::new();
    match found {
        Some(dir) => {
            let dir = dir.canonicalize().unwrap();
            code.push_str("pub static SEEKER: Option<&[(&str, &[u8])]> = Some(&[\n");
            for s in SHEETS {
                let path = dir.join(s);
                writeln!(code, "    ({s:?}, include_bytes!({:?})),", path.display().to_string()).unwrap();
            }
            code.push_str("]);\n");
        }
        None => {
            println!("cargo:warning=Seeker sheets not found: building without embedded character art (see assets/CREDITS.md)");
            code.push_str("pub static SEEKER: Option<&[(&str, &[u8])]> = None;\n");
        }
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded_art.rs");
    std::fs::write(out, code).unwrap();
}
