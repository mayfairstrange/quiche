#!/usr/bin/env bash
set -e

# If shaping is requested, apply it once the container is up
if [ "${SHAPE:-off}" = "on" ]; then
  /usr/local/bin/shape.sh
fi

# Hand off to whatever CMD/args were provided
exec "$@"
