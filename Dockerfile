FROM rust:1.97-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home gh-proxy
COPY --from=builder /build/target/release/gh-proxy /usr/local/bin/gh-proxy
USER gh-proxy
EXPOSE 1555
ENTRYPOINT ["/usr/local/bin/gh-proxy"]
