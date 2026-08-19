#!/usr/bin/env bash
# Keep the preview server up: restart it whenever it exits, so a stray
# exception never leaves a reviewer looking at a dead port.
#
#   setsid nohup tools/uiwatch/keep.sh --port 5200 >/dev/null 2>&1 &
#   kill "$(cat /tmp/uiwatch.pid)"     # stop it
set -u
cd "$(dirname "$0")/../.."
echo $$ > /tmp/uiwatch.pid
trap 'rm -f /tmp/uiwatch.pid' EXIT
while true; do
    python3 tools/uiwatch/serve.py "$@" >> /tmp/uiwatch.log 2>&1
    echo "uiwatch: server exited ($?), restarting in 2s" >> /tmp/uiwatch.log
    sleep 2
done
