#!/usr/bin/env bash
set -euo pipefail

# ---- Minimal knobs (env) ----
# Detect IFACE if not provided
IFACE="${IFACE:-$(ip -o route get 1.1.1.1 2>/dev/null | awk '{print $5; exit}' || echo eth0)}"

# Shaping controls
RATE="${RATE:-100mbit}"     # e.g. 100mbit, 1gbit; empty => no rate limit
LAT="${LAT:-0ms}"           # e.g. 40ms; 0ms => no base delay
JIT="${JIT:-0ms}"           # e.g. 10ms; ignored if LAT is zero
LOSS="${LOSS:-0%}"          # e.g. 0.5%; 0% => no loss
INGRESS="${INGRESS:-0}"     # 1 to mirror shaping to ingress via ifb0

log() { echo "[shape] $*"; }

is_zero_time() {
  local v="${1:-}"; v="${v,,}"
  [[ -z "$v" || "$v" == "0" || "$v" == "0ms" || "$v" == "0s" ]]
}

is_zero_percent() {
  local v="${1:-}"; v="${v,,}"
  [[ -z "$v" ]] && return 0
  [[ "$v" =~ %$ ]] || return 1
  awk -v x="${v%%%}" 'BEGIN{exit (x+0.0)==0 ? 0 : 1}'
}

# Summary
log "tc on ${IFACE}: rate=${RATE:-none} delay=${LAT}±${JIT} loss=${LOSS} ingress=${INGRESS}"

# Clean existing qdiscs (ignore errors)
tc qdisc del dev "$IFACE" root 2>/dev/null || true
tc qdisc del dev "$IFACE" ingress 2>/dev/null || true
ip link del ifb0 2>/dev/null || true

# Build NETEM args based on non-zero knobs
NETEM_ARGS=()
ADD_NETEM=false

# Delay/jitter
if ! is_zero_time "$LAT"; then
  ADD_NETEM=true
  if ! is_zero_time "$JIT"; then
    # jitter only matters if there is a base delay; add distribution only then
    NETEM_ARGS+=(delay "$LAT" "$JIT" distribution normal)
  else
    NETEM_ARGS+=(delay "$LAT")
  fi
fi

# Loss
if ! is_zero_percent "$LOSS"; then
  ADD_NETEM=true
  NETEM_ARGS+=(loss "$LOSS")
fi

# Rate — use NETEM's built-in rate limiter to keep things simple
if [[ -n "${RATE:-}" ]]; then
  ADD_NETEM=true
  NETEM_ARGS+=(rate "$RATE")
fi

# Apply egress shaping (or keep interface unshaped if nothing requested)
if "$ADD_NETEM"; then
  # 'limit' is queue cap (packets), not latency; keep modest to avoid buffering
  tc qdisc add dev "$IFACE" root handle 1: netem "${NETEM_ARGS[@]}" limit 1000
  log "egress: netem ${NETEM_ARGS[*]}"
else
  log "egress: no shaping parameters provided; leaving unshaped"
fi

# Optional ingress mirroring via IFB
if [[ "${INGRESS}" == "1" ]]; then
  if ip link add ifb0 type ifb 2>/dev/null; then
    ip link set up dev ifb0
    tc qdisc add dev "$IFACE" handle ffff: ingress
    tc filter add dev "$IFACE" parent ffff: protocol all u32 match u32 0 0 \
      action mirred egress redirect dev ifb0 2>/dev/null || {
        log "ingress: mirred action unavailable; skipping"
        tc qdisc del dev "$IFACE" ingress 2>/dev/null || true
        ip link del ifb0 2>/dev/null || true
      }
    if "$ADD_NETEM"; then
      tc qdisc add dev ifb0 root handle 2: netem "${NETEM_ARGS[@]}" limit 1000 2>/dev/null || true
      log "ingress: mirrored netem ${NETEM_ARGS[*]} onto ifb0"
    else
      log "ingress: nothing to mirror (no shaping)"
    fi
  else
    log "ingress: IFB not available; skipping"
  fi
fi

# Show result (best-effort)
tc -s qdisc show dev "$IFACE" || true
[[ "${INGRESS}" == "1" ]] && tc -s qdisc show dev ifb0 || true
