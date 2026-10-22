#!/bin/bash
# Starts X, a terminal and the RustDesk server, then sets the ID server, key
# and password. Prints the RustDesk ID on a line of its own ("ID <id>").
set -e
export DISPLAY=:0
rm -f /tmp/.X0-lock
Xvfb :0 -screen 0 "${SCREEN:-1280x800x24}" &
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
openbox &
xterm -geometry 100x30+40+40 -fa Monospace -fs 14 -e bash &
rustdesk --option custom-rendezvous-server "$RD_SERVER"
rustdesk --option relay-server "$RD_RELAY"
rustdesk --option key "$RD_KEY"
rustdesk --server &
for _ in $(seq 50); do rustdesk --password "$RD_PASSWORD" 2>/dev/null | grep -q Done && break; sleep 0.2; done
echo "ID $(rustdesk --get-id)"
wait
