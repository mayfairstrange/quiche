#!/usr/bin/env bash
set -e

if [ "${SHAPE:-off}" = "on" ]; then
  /usr/local/bin/shape.sh
fi

exec "$@"
