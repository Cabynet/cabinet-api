# Multi-stage Dockerfile for Cabinet API (Rust + Axum)
# Output binary is stripped and runs on minimal Debian Trixie Slim with a non-root user.

FROM rust:1.99-trixie AS builder

WORKDIR /app

# 1. Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# 2. Copy dependency definitions first to leverage layer caching
COPY Cargo.toml Cargo.lock ./

# Create dummy src to build dependencies
# Cargo.toml declares both a lib and a bin target, so the dummy build needs both files.
RUN mkdir src && echo "fn main() {}" > src/main.rs && touch src/lib.rs
RUN cargo build --release && rm -rf src

# 3. Copy real source code and migrations
COPY src src
COPY migrations migrations

# Touch main.rs to invalidate dummy build and produce real release binary
RUN touch src/main.rs src/lib.rs && cargo build --release

# 4. Final runtime stage (ffmpeg and yt-dlp power "Download media")
FROM debian:trixie-slim AS runner

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tzdata curl ffmpeg && rm -rf /var/lib/apt/lists/*

# Standalone yt-dlp build, checksum-pinned; bump the version when a platform's downloads break.
ARG YTDLP_VERSION=2026.08.19
ARG TARGETARCH
RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) asset=yt-dlp_linux; sum=58162f9bfdc27458ea47bfcb311cf47028f17d8154a8bf7d689861d46399230a ;; \
      arm64) asset=yt-dlp_linux_aarch64; sum=b16e4dab368a816cd05d477d698a605a6ae87ccee1c8ffd38fa21d7254141fcc ;; \
      *) echo "unsupported arch ${TARGETARCH}"; exit 1 ;; \
    esac; \
    curl -fsSL -o /usr/local/bin/yt-dlp "https://github.com/yt-dlp/yt-dlp/releases/download/${YTDLP_VERSION}/${asset}"; \
    echo "${sum}  /usr/local/bin/yt-dlp" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/yt-dlp

# Create non-root system user
RUN groupadd -g 1001 appuser && useradd -u 1001 -g appuser -s /bin/sh appuser

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/cabinet-api /app/cabinet-api
COPY --from=builder /app/migrations /app/migrations

RUN chown -R appuser:appuser /app

USER appuser

ENV HOST=0.0.0.0
ENV PORT=4000
ENV RUST_LOG=cabinet_api=info,tower_http=info

EXPOSE 4000

ENTRYPOINT ["/app/cabinet-api"]
