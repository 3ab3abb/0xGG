# Art credits

| What | Pack | Author | License | In repo? |
|---|---|---|---|---|
| Weapons (Ping Pistol, Broadcast Shotgun, Fork Bomb), muzzle flash, PACKET DROPPED puff (`sprites/`) | Free 2D Animated Vector Game Character Sprites | Raphael Gonçalves (rgsdev) | CC0 (public domain) | yes, downscaled |
| Player characters (`seeker/`) | Hooded Seeker - Metroidvania Character (Lite) | Primitive Pixels | Free for use in games; **redistribution of the art is not allowed** | **no** (gitignored) |

"Character art by Primitive Pixels."

Everything else (map, Firewall, Checksum Blade at rest, UI) is drawn procedurally in code.

## Installing the character art

The Seeker sheets are not in the repo because their license forbids sharing them.
Get the free **Seeker_Lite** pack from Primitive Pixels and copy the six
`Seeker_Void_*.png` sheets from its `Spritesheets/` folder into `assets/seeker/`.

The client looks in `$GG_ASSETS/seeker`, then `./assets/seeker`, then next to the
binary. Without the sheets the game still runs and draws players as the original
procedural packets. Recolouring per player (orange / blue / white) happens at load time.
