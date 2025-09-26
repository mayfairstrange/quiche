#!/usr/bin/env bash
set -euo pipefail

# Defaults (can be overridden by env)
IF="${IFACE:-$(ip -o route get 1.1.1.1 2>/dev/null | awk '{print $5; exit}' || echo eth0)}"
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

# Clean up (ignore errors)
tc qdisc del dev "$IF" root 2>/dev/null || true
tc qdisc del dev "$IF" ingress 2>/dev/null || true
ip link del ifb0 2>/dev/null || true

have_qdisc() {
  local dev="$1" kind="$2"
  # Try to attach a throwaway qdisc to test availability
  if tc qdisc add dev "$dev" root handle 9999: "$kind" >/dev/null 2>&1; then
    tc qdisc del dev "$dev" root 2>/dev/null || true
    return 0
  fi
  return 1
}

# -------- EGRESS --------
if have_qdisc "$IF" "tbf rate $RATE burst $BURST latency 400ms"; then
  # TBF (rate) -> NETEM (impairments)
  tc qdisc add dev "$IF" root handle 1: tbf rate "$RATE" burst "$BURST" latency 400ms
  tc qdisc add dev "$IF" parent 1: handle 10: netem \
    delay "$LAT" "$JIT" distribution normal \
    loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"
  echo "[egress] using TBF -> NETEM"
else
  # Fallback: NETEM can also do simple rate limiting
  tc qdisc add dev "$IF" root handle 10: netem \
    rate "$RATE" \
    delay "$LAT" "$JIT" distribution normal \
    loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"
  echo "[egress] TBF unavailable, using NETEM with 'rate'"
fi

# -------- INGRESS (optional) --------
if [ "$INGRESS" = "1" ]; then
  # Check for ingress qdisc + IFB support
  if have_qdisc "$IF" "ingress"; then
    # Try to create IFB (will fail if CONFIG_IFB missing)
    if ip link add ifb0 type ifb 2>/dev/null; then
      ip link set up dev ifb0
      tc qdisc add dev "$IF" handle ffff: ingress
      tc filter add dev "$IF" parent ffff: protocol all u32 match u32 0 0 \
        action mirred egress redirect dev ifb0 2>/dev/null || {
          echo "[ingress] 'mirred' action not available; skipping ingress shaping"
          tc qdisc del dev "$IF" ingress 2>/dev/null || true
          ip link del ifb0 2>/dev/null || true
        }

      if ip link show ifb0 >/dev/null 2>&1; then
        # Mirror succeeded; apply same shaping on ifb0
        if have_qdisc "ifb0" "tbf rate $RATE burst $BURST latency 400ms"; then
          tc qdisc add dev ifb0 root handle 1: tbf rate "$RATE" burst "$BURST" latency 400ms
          tc qdisc add dev ifb0 parent 1: handle 10: netem \
            delay "$LAT" "$JIT" distribution normal \
            loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"
          echo "[ingress] using IFB + TBF -> NETEM"
        else
          tc qdisc add dev ifb0 root handle 10: netem \
            rate "$RATE" \
            delay "$LAT" "$JIT" distribution normal \
            loss "$LOSS" reorder "$REORDER" duplicate "$DUP" corrupt "$CORRUPT"
          echo "[ingress] IFB present but TBF unavailable; using NETEM with 'rate'"
        fi
      fi
    else
      echo "[ingress] IFB not available in kernel; skipping ingress shaping"
    fi
  else
    echo "[ingress] ingress qdisc not available; skipping ingress shaping"
  fi
fi

# Show result
tc -s qdisc show dev "$IF" || true
[ "$INGRESS" = "1" ] && tc -s qdisc show dev ifb0 || true
