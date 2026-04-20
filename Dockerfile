# syntax=docker/dockerfile:1.7

# ---------- build stage ----------
FROM rust:1.83-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates pkg-config build-essential \
  && rm -rf /var/lib/apt/lists/*

# Install wasm-pack (precompiled binary — avoids a long cargo install)
RUN curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh

RUN rustup target add wasm32-unknown-unknown

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY shared ./shared
COPY server ./server
COPY client ./client
COPY web ./web

# Build the client wasm + bindings into web/pkg
RUN cd client && wasm-pack build --release --target web \
      --out-dir ../web/pkg --no-typescript

# Build the server
RUN cargo build --release -p server

# ---------- runtime stage ----------
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/server /app/server
COPY --from=builder /build/web /app/web

ENV HOLLOW_WEB_DIR=/app/web
ENV HOLLOW_DB_PATH=/data/hollowtide.redb
# PORT is injected by Railway; default to 8080 if not set.
ENV PORT=8080

# Volume mount point for persistent state.
VOLUME ["/data"]

EXPOSE 8080
CMD ["/app/server"]
