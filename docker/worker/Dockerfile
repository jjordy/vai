# syntax=docker/dockerfile:1.7
# Canonical vai worker image — multi-arch (linux/amd64, linux/arm64)
#
# Build locally (from repo root or docker/worker/):
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     -t vai-worker:test docker/worker/
#
# Layers are ordered by cache-stability (most stable → least stable) so that
# routine toolchain bumps do not invalidate expensive browser or AI-tools layers.

# ── Layer 1: Base OS + system packages ────────────────────────────────────────
# Trixie (Debian 13) is required because the vai release binary is built on
# ubuntu-24.04 (glibc 2.39). Bookworm's glibc 2.36 fails at runtime.
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        curl \
        jq \
        ripgrep \
        ca-certificates \
        openssh-client \
    && rm -rf /var/lib/apt/lists/*

# ── Layer 2: Language toolchains ─────────────────────────────────────────────

# Node 22 LTS
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

# pnpm — global install usable by all users
RUN npm install -g pnpm@latest && npm cache clean --force

# Python 3 + pip
# python-is-python3 creates the `python` symlink so `python --version` works.
RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 \
        python3-pip \
        python3-venv \
        python-is-python3 \
    && rm -rf /var/lib/apt/lists/*

# Rust stable — installed system-wide (RUSTUP_HOME + CARGO_HOME under /usr/local)
# so that any uid can call rustc/cargo without re-installing.
# --profile minimal keeps the download small (rustc + cargo, no docs/src).
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y \
            --default-toolchain stable \
            --profile minimal \
            --no-modify-path \
    && rustup component add clippy rustfmt \
    && rm -rf /usr/local/cargo/registry \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo

# mise — per-repo version pinning via .tool-versions / mise.toml
# Installed to /usr/local/bin so it is on the default PATH for all users.
RUN curl https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh

# ── Layer 3: Browser (Chromium + Playwright system deps) ─────────────────────
# PLAYWRIGHT_BROWSERS_PATH placed under /usr/local so all users share one copy.
# `playwright install --with-deps chromium` handles both apt deps and the browser.
ENV PLAYWRIGHT_BROWSERS_PATH=/usr/local/share/playwright-browsers

RUN npm install -g playwright \
    && playwright install --with-deps chromium \
    && npm cache clean --force

# ── Layer 4: AI tools ─────────────────────────────────────────────────────────

# Claude Code CLI (works on linux/amd64 and linux/arm64 via npm)
RUN npm install -g @anthropic-ai/claude-code && npm cache clean --force

# Anthropic Python SDK
RUN pip3 install --break-system-packages anthropic

# ── Layer 5: vai CLI ──────────────────────────────────────────────────────────
# Download the pre-built vai binary from GitHub releases.
# The release workflow (release.yml) publishes tarballs as:
#   vai-v{VERSION}-{RUST_TARGET}.tar.gz   containing a single `vai` binary.
# TARGETARCH is injected automatically by docker buildx.
ARG VAI_VERSION=0.1.7
ARG TARGETARCH

RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) VAI_TRIPLE="x86_64-unknown-linux-gnu" ;; \
        arm64) VAI_TRIPLE="aarch64-unknown-linux-gnu" ;; \
        *) echo "Unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL \
        "https://github.com/jjordy/vai/releases/download/v${VAI_VERSION}/vai-v${VAI_VERSION}-${VAI_TRIPLE}.tar.gz" \
        | tar -xz -C /usr/local/bin vai \
    && chmod +x /usr/local/bin/vai

# ── Layer 6: Runtime user + entrypoint ───────────────────────────────────────
# Run the agent loop as non-root for security.
RUN useradd -m -s /bin/bash agent

USER agent
# Give the agent user its own CARGO_HOME so cargo can write its registry cache.
ENV HOME=/home/agent \
    CARGO_HOME=/home/agent/.cargo \
    PATH=/home/agent/.cargo/bin:$PATH
WORKDIR /home/agent

# loop.sh is the worker entrypoint (issue #348).
# Until it is present the container exits non-zero at runtime — this is expected.
CMD ["/usr/local/bin/loop.sh"]
