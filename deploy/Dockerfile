# Item 97: Docker improvements — healthcheck, volume mounts, env config
FROM rust:1.83 AS builder
WORKDIR /build
COPY src/ ./src/
WORKDIR /build/src
RUN cargo build --release -p commputer

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/src/target/release/commputer /usr/local/bin/commputer

# Create data and wallet directories
RUN mkdir -p /data /wallet && \
    useradd -r -s /bin/false commputer && \
    chown -R commputer:commputer /data /wallet

USER commputer

# Environment variables for configuration
ENV COMMPUTER_PORT=9000
ENV COMMPUTER_RPC_PORT=9944
ENV COMMPUTER_LOG_LEVEL=info
ENV COMMPUTER_TESTNET=true

# Volumes for persistent data
VOLUME ["/data", "/wallet"]

# Expose ports
EXPOSE 9000 9000/udp 9944

# Healthcheck
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -f http://127.0.0.1:${COMMPUTER_RPC_PORT}/health || exit 1

ENTRYPOINT ["commputer"]
CMD ["run", "--testnet", "--port", "9000", "--rpc-port", "9944"]
