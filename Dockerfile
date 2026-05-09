ARG RUST_VERSION=1.93.1
ARG DEBIAN_VERSION=bookworm

FROM rust:${RUST_VERSION}-slim-${DEBIAN_VERSION} AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        libftdi1-dev \
        pkg-config \
        ca-certificates \
        git \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --bin naas-api

FROM debian:${DEBIAN_VERSION}-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        libftdi1-2 \
        ca-certificates \
        tini \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 naas \
    && useradd --system --uid 10001 --gid naas --shell /usr/sbin/nologin --no-create-home naas

COPY --from=builder /app/target/release/naas-api /usr/local/bin/naas-api

USER naas:naas
EXPOSE 8080 8081
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/naas-api"]
