#!/bin/bash
# Double-click to run the server. Players type the IP printed below.
cd "$(dirname "$0")" || exit 1
echo "============================================================"
echo "  0xGG SERVER  -  players type this IP:"
for i in en0 en1 bridge100; do ip=$(ipconfig getifaddr $i 2>/dev/null) && echo "    $ip   ($i)"; done
echo "  macOS asks to accept incoming connections? click Allow."
echo "  Stop with Ctrl+C or by closing this window."
echo "============================================================"
./gg-server --bind 0.0.0.0:7777
