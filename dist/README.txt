0xGG - Last packet standing.
============================

Nothing to install. Unzip the folder anywhere (Desktop is fine) and keep the files together.


1. FIRST LAUNCH WARNINGS (normal, the game isn't code-signed)
-------------------------------------------------------------
Windows "Windows protected your PC":  click  More info  ->  Run anyway.
macOS "cannot be opened":             right-click the .command file -> Open -> Open,
                                      or once in Terminal:  xattr -dr com.apple.quarantine <this folder>


2. PLAYING
----------
Everyone double-clicks PLAY.bat (Windows) or play.command (macOS).

ONE player hosts:
  Press HOST GAME on the login screen. Your IP appears (e.g. 192.168.1.20):
  send it to the others (the COPY button in the lobby / port puts it on your clipboard).
  If a firewall popup appears, allow it (on Windows tick BOTH Private and Public).
  Keep your window open: closing it stops the server for everyone.

The others:
  Type the host's IP in the "host's IP" box (it is remembered for next time).

Then everyone: register a name + password, create or join a port (room), press READY.

Dedicated server instead (optional): run HOST.bat / host.command on a machine that
doesn't play; players type the IP it prints.
If nobody can connect, run OPEN-FIREWALL.bat once on the host (Windows).


3. IF IT DOESN'T CONNECT
------------------------
- Everyone must be on the SAME Wi-Fi or hotspot as the server.
- Use the server's LAN IP (192.168.x.x / 10.x.x.x / 172.x.x.x), never 127.0.0.1.
- Campus Wi-Fi often blocks laptops from talking to each other.
  Fix: turn on a phone hotspot and connect all 4 laptops to it.
- The server uses port 7777, TCP and UDP.


Controls: A/D move, SPACE jump, W aim up, S+SPACE drop through platform,
          J shoot, K pick up / throw, ESC forfeit   (or arrows + Z/X/C)
          F11 (or Alt+Enter) fullscreen

Art credits: see CREDITS.md
