ARG RUST_VERSION=1.93.1
ARG DEBIAN_VERSION=bookworm

FROM lukemathwalker/cargo-chef:0.1-rust-${RUST_VERSION}-${DEBIAN_VERSION} AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        libftdi1-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json --bin naas-api
COPY . .
RUN cargo build --release --bin naas-api

FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        libftdi1 \
        ca-certificates \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 naas \
    && useradd --system --uid 10001 --gid naas --shell /usr/sbin/nologin --no-create-home naas

COPY --from=builder /app/target/release/naas-api /usr/local/bin/naas-api

USER naas:naas
EXPOSE 8080 8081
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/naas-api"]
