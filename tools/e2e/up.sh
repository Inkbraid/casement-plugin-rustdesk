#!/bin/bash
# Starts hbbs, hbbr and a RustDesk peer in Docker and writes the address and
# password for tests/e2e.rs to tools/e2e/env (source it, or pass it to CI).
#
#   tools/e2e/up.sh && . tools/e2e/env && cargo test --test e2e
#
# The servers listen on localhost:21116/21117; the peer reaches them by name
# on a private Docker network, so the address carries relay=127.0.0.1.
set -euo pipefail
cd "$(dirname "$0")"
PASSWORD="${E2E_PASSWORD:-Peer-pass-123}"
docker rm -f e2e-hbbs e2e-hbbr e2e-peer >/dev/null 2>&1 || true
docker network create e2e-rustdesk >/dev/null 2>&1 || true
data="$(mktemp -d)"
docker run -d --name e2e-hbbs --network e2e-rustdesk -v "$data:/root" \
  -p 127.0.0.1:21116:21116 -p 127.0.0.1:21116:21116/udp \
  rustdesk/rustdesk-server:latest hbbs -r e2e-hbbr:21117 >/dev/null
docker run -d --name e2e-hbbr --network e2e-rustdesk -v "$data:/root" \
  -p 127.0.0.1:21117:21117 rustdesk/rustdesk-server:latest hbbr >/dev/null
for _ in $(seq 100); do [ -s "$data/id_ed25519.pub" ] && break; sleep 0.2; done
KEY="$(cat "$data/id_ed25519.pub")"
docker build -q -t casement-e2e-peer peer >/dev/null
docker run -d --name e2e-peer --network e2e-rustdesk \
  -e RD_SERVER=e2e-hbbs -e RD_RELAY=e2e-hbbr -e RD_KEY="$KEY" -e RD_PASSWORD="$PASSWORD" \
  casement-e2e-peer >/dev/null
ID=""
for _ in $(seq 150); do
  ID="$(docker logs e2e-peer 2>/dev/null | sed -n 's/^ID \([0-9]\+\)$/\1/p' | tail -1)"
  [ -n "$ID" ] && break
  sleep 0.2
done
[ -n "$ID" ] || { docker logs e2e-peer; echo "the peer didn't start" >&2; exit 1; }
# Give the peer a moment to register with hbbs.
for _ in $(seq 50); do docker logs e2e-hbbs 2>&1 | grep -q "update_pk $ID" && break; sleep 0.2; done
cat > env <<EOT
export CASEMENT_E2E_ADDRESS='rustdesk://$ID?server=127.0.0.1&key=$KEY&relay=127.0.0.1'
export CASEMENT_E2E_PASSWORD='$PASSWORD'
export CASEMENT_E2E_PEER_CONTAINER=e2e-peer
EOT
cat env
