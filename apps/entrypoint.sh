#!/usr/bin/env bash
set -euo pipefail

if [ "${SHAPE:-off}" = "on" ]; then
  /usr/local/bin/shape.sh || echo "[shape] warning: shaping failed (continuing)"
fi

exec "$@"
