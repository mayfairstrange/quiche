#!/bin/sh
set -e

# Default shaping values
SHAPE=on
RATE=5mbit
BURST=32kbit
LAT=80ms
JIT=20ms
LOSS=0%
INGRESS=0
QLOGDIR_HOST="$(pwd)/qlogs"   # host directory for qlogs
QLOGDIR_CONT="/qlogs"         # container path for qlogs

# Create qlog directory if missing
mkdir -p "$QLOGDIR_HOST"

# Parse key=value args from the command line to override defaults
for arg in "$@"; do
  case $arg in
    SHAPE=*)   SHAPE="${arg#*=}" ;;
    RATE=*)    RATE="${arg#*=}" ;;
    BURST=*)   BURST="${arg#*=}" ;;
    LAT=*)     LAT="${arg#*=}" ;;
    JIT=*)     JIT="${arg#*=}" ;;
    LOSS=*)    LOSS="${arg#*=}" ;;
    INGRESS=*) INGRESS="${arg#*=}" ;;
    *)
      echo "Unknown argument: $arg"
      exit 1
      ;;
  esac
done

echo "Starting quiche-server with shaping:"
echo "  RATE=$RATE, BURST=$BURST, LAT=$LAT, JIT=$JIT, LOSS=$LOSS, INGRESS=$INGRESS"
echo "  QLOGDIR=$QLOGDIR_HOST"

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
  -e QLOGDIR="$QLOGDIR_CONT" \
  -v "$QLOGDIR_HOST":"$QLOGDIR_CONT" \
  quiche-shaped \
  quiche-server \
    --listen 0.0.0.0:4433 \
    --root /www \
    --cert /certs/cert.pem \
    --key /certs/priv.key \
    --disable-gso
