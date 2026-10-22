#!/bin/bash
# Removes what up.sh started.
docker rm -f e2e-hbbs e2e-hbbr e2e-peer >/dev/null 2>&1
docker network rm e2e-rustdesk >/dev/null 2>&1
rm -f "$(dirname "$0")/env"
true
