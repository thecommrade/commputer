# Stage 1: Build
# rust >= 1.85 required for workspace edition 2024.
FROM rust:1.88-slim AS builder

RUN apt-get update && apt-get install -y \
    pkg-config \
    libclang-dev \
    clang \
    librocksdb-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY src/ ./src/

WORKDIR /build/src
RUN cargo build --release --bin commputer

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    librocksdb7.8 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/src/target/release/commputer /usr/local/bin/commputer

# P2P port
EXPOSE 9000
# RPC port
EXPOSE 9944

ENTRYPOINT ["commputer"]
CMD ["run", "--testnet"]
