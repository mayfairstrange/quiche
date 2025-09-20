# Build stage: compile quiche apps
FROM rust:1.82 AS build

WORKDIR /build

# Copy manifest and source directories required for building the apps
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

# Install build deps and build release binaries
RUN apt-get update && apt-get install -y cmake && rm -rf /var/lib/apt/lists/*

RUN cargo build --release --features sfv --manifest-path apps/Cargo.toml


##
## quiche-base: runtime image for running the server/client with optional shaping
##
FROM debian:latest AS quiche-base

# Install minimal runtime deps + iproute2 for tc
RUN apt-get update && apt-get install -y ca-certificates iproute2 && \
    rm -rf /var/lib/apt/lists/*
    
# Copy compiled binaries from build stage
COPY --from=build /build/target/release/quiche-client /usr/local/bin/
COPY --from=build /build/target/release/quiche-server /usr/local/bin/

# Copy the www directory into the container
COPY www /www


# Copy the demo self-signed certs
COPY apps/src/bin/cert.crt /certs/cert.pem
COPY apps/src/bin/cert.key /certs/priv.key

# Copy shaping and entrypoint helpers (these should exist in your repo's apps/ directory)
COPY apps/shape.sh /usr/local/bin/shape.sh
COPY apps/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/shape.sh /usr/local/bin/entrypoint.sh

ENV PATH="/usr/local/bin/:${PATH}"
ENV RUST_LOG=info

# default shaping envs (override at runtime)
ENV SHAPE=off \
    IFACE=eth0 \
    RATE=10mbit \
    BURST=32kbit \
    LAT=60ms \
    JIT=10ms \
    LOSS=0% \
    REORDER=0% \
    DUP=0% \
    CORRUPT=0% \
    INGRESS=0

# run the entrypoint wrapper which optionally applies shaping then execs the given CMD
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]


##
## quiche-qns: quiche image for quic-interop-runner (endpoint configured)
##
FROM martenseemann/quic-network-simulator-endpoint:latest AS quiche-qns

WORKDIR /quiche

# install wait-for-it + iproute2 (tc) for shaping inside qns endpoints
RUN apt-get update && apt-get install -y wait-for-it iproute2 && rm -rf /var/lib/apt/lists/*

# Copy compiled binaries and scripts into the qns image
COPY --from=build /build/target/release/quiche-client /quiche/quiche-client
COPY --from=build /build/target/release/quiche-server /quiche/quiche-server
COPY apps/run_endpoint.sh /quiche/run_endpoint.sh

# Also include the shaping and entrypoint helpers so qns endpoints can use them
COPY apps/shape.sh /quiche/shape.sh
COPY apps/entrypoint.sh /quiche/entrypoint.sh

RUN chmod +x /quiche/run_endpoint.sh /quiche/shape.sh /quiche/entrypoint.sh

ENV RUST_LOG=trace

ENTRYPOINT [ "./run_endpoint.sh" ]
