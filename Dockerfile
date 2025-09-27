# =========================
# Build stage
# =========================
FROM rust:1.82 AS build

WORKDIR /build

# Copy manifests & source (as in your original)
COPY Cargo.toml ./ 
COPY apps/ ./apps/
COPY buffer-pool ./buffer-pool/
COPY datagram-socket/ ./datagram-socket/
COPY h3i/ ./h3i/
COPY octets/ ./octets/
COPY qlog/ ./qlog/
COPY quiche/ ./quiche/
COPY task-killswitch ./task-killswitch/
COPY tokio-quiche ./tokio-quiche/

# Build deps (cmake is needed for boringssl during build)
RUN apt-get update && apt-get install -y --no-install-recommends cmake \
  && rm -rf /var/lib/apt/lists/*

# Build release apps
RUN cargo build --release --features sfv --manifest-path apps/Cargo.toml
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      iproute2 ca-certificates procps kmod iptables openssl \
  && rm -rf /var/lib/apt/lists/*

# Binaries
COPY --from=build /build/target/release/quiche-server /usr/local/bin/
COPY --from=build /build/target/release/quiche-client /usr/local/bin/

# Static content
COPY www /www

# Shaping + entrypoint
COPY apps/shape.sh /usr/local/bin/shape.sh
COPY apps/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/shape.sh /usr/local/bin/entrypoint.sh

# Example certs
COPY apps/src/bin/cert.crt /certs/cert.pem
COPY apps/src/bin/cert.key /certs/priv.key

ENV PATH="/usr/local/bin:${PATH}" \
    RUST_LOG=info \
    SHAPE=off \
    INGRESS=0

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["quiche-server","--listen","0.0.0.0:4433","--root","/www","--cert","/certs/cert.pem","--key","/certs/priv.key","--disable-gso"]
