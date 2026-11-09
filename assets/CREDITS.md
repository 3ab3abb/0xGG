# Art credits

| What | Pack | Author | License | In repo? |
|---|---|---|---|---|
| Weapons (Ping Pistol, Broadcast Shotgun, Fork Bomb), muzzle flash, PACKET DROPPED puff (`sprites/`) | Free 2D Animated Vector Game Character Sprites | Raphael Gonçalves (rgsdev) | CC0 (public domain) | yes, downscaled |
| Player characters (`seeker/`) | Hooded Seeker - Metroidvania Character (Lite) | Primitive Pixels | Free for use in games; **sharing the art files is not allowed** | **no** (gitignored); compiled into `gg-client` at build time |

"Character art by Primitive Pixels."

Everything else (map, Firewall, Checksum Blade at rest, UI) is drawn procedurally in code.

Both credits are also shown in the game: on the title screen footer and in its CREDITS panel.

## How the character art gets into the game

The license allows using the Seeker art in our game but not sharing the files, so
the PNGs are never committed and never shipped as loose files:

- **Building from source:** get the free **Seeker_Lite** pack from Primitive Pixels and
  copy the `Seeker_Void_*.png` sheets from its `Spritesheets/` folder into `assets/seeker/`
  (or point `GG_SEEKER_DIR` at them). `crates/client/build.rs` compiles them into the binary.
- **Release builds (GitHub Actions):** the pack zip is stored base64-encoded in the
  encrypted repository secret `SEEKER_LITE_ZIP_B64`; the workflow unpacks it before
  building. Players get the art inside `gg-client`, with nothing to install.

Without the sheets the game still builds and runs, drawing players as the original
procedural packets. At runtime a local `assets/seeker/` folder (or `$GG_ASSETS/seeker`)
overrides the embedded art. Recolouring per player (orange / blue / white) happens at load time.
