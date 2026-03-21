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

# Install Claude Code CLI
RUN curl -fsSL https://claude.ai/install.sh | bash

# Add Claude to PATH
ENV PATH="/home/agent/.local/bin:$PATH"

# Pre-warm Rust toolchain for agent user
RUN rustup default stable

# Create repos directory
RUN mkdir -p /home/agent/repos

WORKDIR /home/agent
ENTRYPOINT ["sleep", "infinity"]
