#!/bin/sh
set -e

# Defaults
SHAPE=on; RATE=5mbit; BURST=32kbit; LAT=80ms; JIT=20ms; LOSS=0%; INGRESS=0
QLOGDIR_HOST="$(pwd)/qlogs"; QLOGDIR_CONT="/qlogs"
mkdir -p "$QLOGDIR_HOST"

# Parse overrides
for arg in "$@"; do
  case $arg in
    SHAPE=*|RATE=*|BURST=*|LAT=*|JIT=*|LOSS=*|INGRESS=*) eval "$arg" ;;
    *) echo "Unknown arg: $arg"; exit 1 ;;
  esac
done

echo "Starting quiche-server with shaping:
  RATE=$RATE, BURST=$BURST, LAT=$LAT, JIT=$JIT, LOSS=$LOSS, INGRESS=$INGRESS
  QLOGDIR=$QLOGDIR_HOST"

docker run --rm -it \
  --init \
  --cap-add NET_ADMIN \
  -p 4433:4433/udp \
  -e SHAPE="$SHAPE" \
  -e RATE="$RATE" \
  -e BURST="$BURST" \
  -e LAT="$LAT" \
  -e JIT="$JIT" \
  -e LOSS="$LOSS" \
  -e INGRESS="$INGRESS" \
  -e RUST_LOG=info \
  -e RUST_BACKTRACE=0 \
  quiche-shaped
# ^ no command here; ENTRYPOINT+CMD inside the image run the server with cert/key
