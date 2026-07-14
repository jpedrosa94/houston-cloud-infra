# Houston Engine — production image for GKE Sandbox deployment.
#
# Build from the houston mono-repo root:
#
#   docker build -t houston/engine:v0.4.19 \
#     --build-arg VERSION=0.4.19 \
#     --build-arg COMMIT_SHA=$(git rev-parse --short HEAD) \
#     -f ../houston-cloud-infra/container/Dockerfile .
#
# Local dev via docker-compose (from this directory):
#
#   docker compose up --build

ARG VERSION=dev
ARG COMMIT_SHA=unknown

# ---------- Build stage ----------
FROM rust:1.88-slim-bookworm AS build
WORKDIR /src

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Copy only what the engine needs. The root Cargo.toml includes desktop
# app workspace members — strip them so Cargo doesn't look for
# directories we didn't copy.
COPY Cargo.toml Cargo.lock ./
COPY engine/ engine/
COPY ui/agent-schemas/ ui/agent-schemas/

RUN sed -i '/"app\//d' Cargo.toml && \
    sed -i '/houston-tauri/d' Cargo.toml

RUN cargo build --release -p houston-engine-server --bin houston-engine

# ---------- Runtime stage ----------
FROM debian:bookworm-slim

ARG VERSION
ARG COMMIT_SHA

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl bash unzip \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --create-home --home-dir /data houston \
 && mkdir -p /data/.houston/db /data/Houston /data/.claude /data/.codex /data/.composio /opt/houston/bin \
 && chown -R houston:houston /data /opt/houston

COPY --from=build /src/target/release/houston-engine /usr/local/bin/houston-engine

# Install CLIs to /opt/houston/bin/ — NOT under /data/ which gets
# overlaid by the PVC mount at runtime (K8s). HOME is /data for
# credential storage, but binaries live outside the mount.
ENV HOME=/data \
    PATH="/opt/houston/bin:/data/.local/bin:${PATH}"

# Install CLIs, then copy binaries to /opt/houston/bin/ so they
# survive the PVC mount over /data at runtime (K8s).
USER houston
RUN curl -fsSL https://claude.ai/install.sh | bash \
 && curl -fsSL https://github.com/openai/codex/releases/latest/download/codex-x86_64-unknown-linux-musl.tar.gz \
    | tar xz -C /data/.local/bin/ \
 && mv /data/.local/bin/codex-x86_64-unknown-linux-musl /data/.local/bin/codex \
 && curl -fsSL https://composio.dev/install | bash \
 && cp /data/.local/bin/claude /opt/houston/bin/claude \
 && cp /data/.local/bin/codex /opt/houston/bin/codex \
 && cp /data/.composio/composio /opt/houston/bin/composio

# Symlink CLIs into /usr/local/bin/ so they're always on PATH —
# even through login shell resolution (`bash -l -c 'echo $PATH'`)
# and after the PVC mount shadows /data/.local/bin/ at runtime.
USER root
RUN ln -sf /opt/houston/bin/claude /usr/local/bin/claude \
 && ln -sf /opt/houston/bin/codex /usr/local/bin/codex \
 && ln -sf /opt/houston/bin/composio /usr/local/bin/composio
USER houston

WORKDIR /data

ENV HOUSTON_HOME=/data/.houston \
    HOUSTON_DOCS=/data/Houston \
    HOUSTON_BIND=0.0.0.0:7777 \
    HOUSTON_BIND_ALL=1 \
    HOUSTON_NO_PARENT_WATCHDOG=1

EXPOSE 7777

STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -sf http://localhost:7777/v1/health || exit 1

LABEL org.opencontainers.image.source="https://github.com/houston-ai/houston" \
      org.opencontainers.image.title="houston-engine" \
      org.opencontainers.image.description="Houston Engine — standalone agent runtime" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${COMMIT_SHA}"

CMD ["/usr/local/bin/houston-engine"]
