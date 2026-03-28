FROM rust:1-bookworm

# Install system dependencies
RUN apt-get update && apt-get install -y \
  git \
  curl \
  jq \
  build-essential \
  pkg-config \
  libssl-dev \
  libsqlite3-dev \
  && rm -rf /var/lib/apt/lists/*

# Install GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
  | dd of=/usr/share/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
  | tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
  && apt-get update && apt-get install -y gh \
  && rm -rf /var/lib/apt/lists/*

# Install common Rust tools
RUN rustup component add clippy rustfmt
RUN cargo install cargo-watch

# Create a non-root user for Claude to run as
RUN useradd -m -s /bin/bash agent
USER agent

# Use agent-owned CARGO_HOME so cargo can write its registry cache
ENV CARGO_HOME=/home/agent/.cargo
ENV PATH="/home/agent/.cargo/bin:/home/agent/.local/bin:$PATH"

# Install Claude Code CLI
RUN curl -fsSL https://claude.ai/install.sh | bash

# Pre-warm Rust toolchain for agent user (installs to /home/agent/.cargo)
RUN rustup default stable

# Create repos directory
RUN mkdir -p /home/agent/repos

WORKDIR /home/agent

# Note: to build the vai server binary inside a container, use:
#   cargo build --release --features full
# The default `cargo build` produces a CLI-only binary without server/Postgres/S3 deps.

ENTRYPOINT ["sleep", "infinity"]
