#!/bin/bash
# Double-click to play. Remembers the last server.
cd "$(dirname "$0")" || exit 1
LAST=$(cat last_server.txt 2>/dev/null)
read -r -p "Server IP${LAST:+ [Enter for $LAST]}: " IP
IP=${IP:-$LAST}
[ -z "$IP" ] && { echo "no server IP given"; exit 1; }
echo "$IP" > last_server.txt
./gg-client --server "$IP"
