#!/bin/bash
# Starts X, sound, a terminal and the RustDesk server, then sets the ID
# server, key and password. Prints the RustDesk ID on a line of its own
# ("ID <id>").
set -e
export DISPLAY=:0
rm -f /tmp/.X0-lock
Xvfb :0 -screen 0 "${SCREEN:-1280x800x24}" &
for _ in $(seq 50); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.1; done
# Sound: a 440 Hz tone on a null sink. RustDesk sends what the default sink
# plays (its monitor). Every PulseAudio client finds the server through
# /etc/pulse/client.conf (see the Dockerfile).
mkdir -p /tmp/pulse
pulseaudio --daemonize=no --exit-idle-time=-1 --disallow-exit -n --use-pid-file=no --system=no \
  --load="module-native-protocol-unix auth-anonymous=1 socket=/tmp/pulse/native" \
  --load="module-null-sink sink_name=e2e" --load="module-always-sink" >/tmp/pulseaudio.log 2>&1 &
for _ in $(seq 50); do pactl info >/dev/null 2>&1 && break; sleep 0.1; done
pactl load-module module-sine frequency=440 >/dev/null || { cat /tmp/pulseaudio.log; exit 1; }
openbox &
xterm -geometry 100x30+40+40 -fa Monospace -fs 14 -e bash &
rustdesk --option custom-rendezvous-server "$RD_SERVER"
rustdesk --option relay-server "$RD_RELAY"
rustdesk --option key "$RD_KEY"
rustdesk --server &
for _ in $(seq 50); do rustdesk --password "$RD_PASSWORD" 2>/dev/null | grep -q Done && break; sleep 0.2; done
echo "ID $(rustdesk --get-id)"
wait
