# Multi-stage build for the Trust server.
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p trust-server

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/trust-server /usr/local/bin/trust-server
ENV TRUST_BIND_ADDR=0.0.0.0:8080
EXPOSE 8080
CMD ["trust-server"]
