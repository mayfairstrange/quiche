#!/usr/bin/env bash
set -euo pipefail

IF="${IFACE:-eth0}"
RATE="${RATE:-10mbit}"
BURST="${BURST:-32kbit}"
LAT="${LAT:-60ms}"
JIT="${JIT:-10ms}"
LOSS="${LOSS:-0%}"
REORDER="${REORDER:-0%}"
DUP="${DUP:-0%}"
CORRUPT="${CORRUPT:-0%}"
INGRESS="${INGRESS:-0}"

echo "[shape] tc on ${IF}: rate=${RATE} burst=${BURST} delay=${LAT}±${JIT} loss=${LOSS} reorder=${REORDER} dup=${DUP} corrupt=${CORRUPT} ingress=${INGRESS}"

# cleanup
tc qdisc del dev "$IF" root 2>/dev/null || true
tc qdisc del dev "$IF" ingress 2>/dev/null || true

# egress: tbf -> netem
tc qdisc add dev "$IF" root handle 1: tbf rate "$RATE" burst "$BURST" latency 400ms
tc qdisc add dev "$IF" parent 1: handle 10: netem \
  delay "$LAT" "$JIT" distribution normal \
  loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"

# optional ingress via IFB
if [ "$INGRESS" = "1" ]; then
  modprobe ifb numifbs=1 2>/dev/null || true
  ip link add ifb0 type ifb 2>/dev/null || true
  ip link set up dev ifb0
  tc qdisc add dev "$IF" handle ffff: ingress
  tc filter add dev "$IF" parent ffff: protocol all u32 match u32 0 0 action mirred egress redirect dev ifb0
  tc qdisc del dev ifb0 root 2>/dev/null || true
  tc qdisc add dev ifb0 root handle 1: tbf rate "$RATE" burst "$BURST" latency 400ms
  tc qdisc add dev ifb0 parent 1: handle 10: netem \
    delay "$LAT" "$JIT" distribution normal \
    loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"
fi

tc -s qdisc show dev "$IF" || true
[ "$INGRESS" = "1" ] && tc -s qdisc show dev ifb0 || true
