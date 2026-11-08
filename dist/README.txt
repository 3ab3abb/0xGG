0xGG - Last packet standing.
============================

Nothing to install. Unzip the folder anywhere (Desktop is fine) and keep the files together.


1. CHARACTER ART (once)
-----------------------
The player sprites can't be published, so they are shared privately in the team.
Copy the 6 Seeker_Void_*.png files into:   assets\seeker\
No sprites? The game still works, players are just drawn as plain packets.


2. FIRST LAUNCH WARNINGS (normal, the game isn't code-signed)
-------------------------------------------------------------
Windows "Windows protected your PC":  click  More info  ->  Run anyway.
macOS "cannot be opened":             right-click the .command file -> Open -> Open,
                                      or once in Terminal:  xattr -dr com.apple.quarantine <this folder>


3. PLAYING
----------
One machine is the SERVER:
  Windows: double-click HOST.bat      macOS: double-click host.command
  It prints the machine's IP. If a firewall popup appears, allow it
  (on Windows tick BOTH Private and Public).
  If nobody can connect, run OPEN-FIREWALL.bat once (Windows, host only).

Every player:
  Windows: double-click PLAY.bat      macOS: double-click play.command
  Type the server's IP (e.g. 192.168.1.20). It is remembered for next time.
  Register a name + password, create or join a port (room), press READY.


4. IF IT DOESN'T CONNECT
------------------------
- Everyone must be on the SAME Wi-Fi or hotspot as the server.
- Use the server's LAN IP (192.168.x.x / 10.x.x.x / 172.x.x.x), never 127.0.0.1.
- Campus Wi-Fi often blocks laptops from talking to each other.
  Fix: turn on a phone hotspot and connect all 4 laptops to it.
- The server uses port 7777, TCP and UDP.


Controls: A/D move, SPACE jump, W aim up, S+SPACE drop through platform,
          J shoot, K pick up / throw, ESC forfeit   (or arrows + Z/X/C)

Art credits: see CREDITS.md
