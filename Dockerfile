# syntax=docker/dockerfile:1
# Headless Okytes: the daemon (core engine + authenticated HTTP API) and CLI.
# No desktop/GUI components are built or shipped.

# 1. Local Quick Download page, embedded into the daemon at compile time.
FROM node:24-bookworm-slim AS web
WORKDIR /src
RUN corepack enable && corepack prepare pnpm@11.11.0 --activate
COPY apps/desktop/package.json apps/desktop/pnpm-lock.yaml apps/desktop/pnpm-workspace.yaml apps/desktop/
RUN pnpm --dir apps/desktop install --frozen-lockfile
COPY apps/desktop apps/desktop
COPY assets assets
RUN pnpm --dir apps/desktop build:quick

# 2. Release binaries.
FROM rust:1.95-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends nodejs patch \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=web /src/apps/daemon/web apps/daemon/web
# Restores the checksum-pinned, patched torrent sources (vendor/ is not committed).
RUN node dev-tools/scripts/prepare-vendor.mjs
RUN cargo build --release --locked -p okytes-daemon -p okytes-cli

# 3. Minimal runtime as an unprivileged user.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin okytes \
 && mkdir -p /data /downloads \
 && chown okytes:okytes /data /downloads \
 && chmod 700 /data
COPY --from=build /src/target/release/okytes-daemon /src/target/release/okytes-cli /usr/local/bin/
USER okytes
VOLUME ["/data", "/downloads"]
EXPOSE 17841
ENV OKYTES_DATA_DIR=/data
# tini forwards SIGTERM so the daemon drains checkpoints before exiting.
ENTRYPOINT ["tini", "--", "okytes-daemon", "--data-dir", "/data", "--downloads", "/downloads"]
# Default: loopback only. See apps/daemon/SERVER.md for TLS and proxy modes.
CMD ["--bind", "127.0.0.1:17841"]
