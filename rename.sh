#!/usr/bin/env bash
# usage: ./rename.sh <newname>   e.g. ./rename.sh hardpoint
set -euo pipefail
NEW="$1"
grep -rl 'harness' --include='*.toml' --include='*.rs' . | xargs sed -i "s/harness-/${NEW}-/g; s/name = \"harness\"/name = \"${NEW}\"/; s/harness_core/${NEW}_core/g; s/harness_config/${NEW}_config/g; s/harness_session/${NEW}_session/g"
cd .. && mv harness-rs "$NEW"
echo "renamed to $NEW"
