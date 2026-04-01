# PRD 17: Hosting and Deployment

## Overview

Deploy vai to production using Fly.io for compute/Postgres and Cloudflare R2/Pages for storage/dashboard. Infrastructure as code via Fly.toml (compute) and Terraform (Cloudflare).

## Architecture

```
┌─────────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Fly.io             │     │  Fly.io           │     │  Cloudflare R2  │
│  vai-server         │────▶│  Managed Postgres │     │  Object Storage │
│  (Docker container) │     │  (free tier)      │     │  (free 10GB)    │
│                     │────▶│                   │     │                 │
│  REST + WebSocket   │     └──────────────────┘     └─────────────────┘
└────────┬────────────┘                                       ▲
         │                                                    │
         └────────────────────────────────────────────────────┘
         ▲                    ▲
         │                    │
┌────────┴───────┐   ┌───────┴────────┐
│  Cloudflare    │   │  Agents (CLI)  │
│  Pages         │   │  (vai-agent)   │
│  (dashboard)   │   │                │
└────────────────┘   └────────────────┘
```

## Cost (Single User / Development)

| Resource | Service | Monthly |
|----------|---------|---------|
| Compute | Fly.io shared-cpu-1x (256MB) | $3 |
| Database | Fly Postgres (free tier, 1GB) | $0 |
| Object Storage | Cloudflare R2 (10GB free, zero egress) | $0 |
| Dashboard | Cloudflare Pages (free) | $0 |
| DNS/TLS | Cloudflare (free) | $0 |
| **Total** | | **~$3/mo** |

## Infrastructure as Code

```
infra/
├── fly.toml              # Fly.io app config (vai-server)
├── Dockerfile.server     # Release Docker image for vai-server
├── Dockerfile.dashboard  # Release Docker image for vai-dashboard
├── terraform/
│   ├── main.tf           # Cloudflare provider, R2 bucket, Pages project
│   ├── variables.tf      # Configurable inputs
│   ├── outputs.tf        # R2 endpoint, bucket name, Pages URL
│   └── terraform.tfvars  # (gitignored) account-specific values
├── docker-compose.yml    # Self-hosted quickstart
└── .github/
    └── workflows/
        └── ci.yml        # Build, test, push images
```

## Fly.io Configuration

### fly.toml
```toml
app = "vai-server"
primary_region = "iad"

[build]
  dockerfile = "infra/Dockerfile.server"

[env]
  VAI_PORT = "7865"
  VAI_LOG_LEVEL = "info"

[http_service]
  internal_port = 7865
  force_https = true
  auto_stop_machines = false
  min_machines_running = 1

[checks]
  [checks.health]
    type = "http"
    port = 7865
    path = "/health"
    interval = "10s"
    timeout = "5s"
```

### Secrets (set via fly secrets set)
```
DATABASE_URL=postgres://...
VAI_S3_ENDPOINT=https://<account-id>.r2.cloudflarestorage.com
VAI_S3_BUCKET=vai-prod
VAI_S3_ACCESS_KEY=...
VAI_S3_SECRET_KEY=...
VAI_ADMIN_KEY=<generate-strong-key>
VAI_CORS_ORIGINS=https://dashboard.vai.dev
```

## Terraform (Cloudflare)

```hcl
# main.tf
terraform {
  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 4.0"
    }
  }
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}

resource "cloudflare_r2_bucket" "vai" {
  account_id = var.cloudflare_account_id
  name       = var.r2_bucket_name
  location   = "ENAM"
}
```

## Docker Images

### vai-server (Dockerfile.server)
```dockerfile
FROM rust:1.82-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --features full

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/vai /usr/local/bin/vai
EXPOSE 7865
ENTRYPOINT ["vai"]
CMD ["server", "--multi-repo"]
```

### vai-dashboard (Dockerfile.dashboard)
```dockerfile
FROM node:22-alpine AS builder
WORKDIR /app
COPY package.json pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY . .
RUN pnpm run build

FROM node:22-alpine
WORKDIR /app
COPY --from=builder /app/.output ./
EXPOSE 3000
CMD ["node", "server/index.mjs"]
```

## Docker Compose (Self-Hosted)

```yaml
version: "3.8"
services:
  vai:
    image: ghcr.io/jjordy/vai:latest
    ports: ["7865:7865"]
    environment:
      DATABASE_URL: postgres://vai:vai@postgres:5432/vai
      VAI_S3_ENDPOINT: http://minio:9000
      VAI_S3_BUCKET: vai
      VAI_S3_ACCESS_KEY: minioadmin
      VAI_S3_SECRET_KEY: minioadmin
      VAI_ADMIN_KEY: ${VAI_ADMIN_KEY:-change-me}
    depends_on: [postgres, minio]

  dashboard:
    image: ghcr.io/jjordy/vai-dashboard:latest
    ports: ["3000:3000"]
    environment:
      VITE_VAI_SERVER_URL: http://vai:7865

  postgres:
    image: postgres:16-alpine
    volumes: ["pgdata:/var/lib/postgresql/data"]
    environment:
      POSTGRES_DB: vai
      POSTGRES_USER: vai
      POSTGRES_PASSWORD: vai

  minio:
    image: minio/minio
    command: server /data --console-address ":9001"
    ports: ["9001:9001"]
    volumes: ["s3data:/data"]
    environment:
      MINIO_ROOT_USER: minioadmin
      MINIO_ROOT_PASSWORD: minioadmin

volumes:
  pgdata:
  s3data:
```

## CI/CD Pipeline

```yaml
# .github/workflows/ci.yml
name: CI
on:
  push:
    branches: [main]
  pull_request:

jobs:
  test-cli:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test
      - run: cargo clippy -- -D warnings
      - run: cargo audit --deny warnings

  test-full:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:16-alpine
        env:
          POSTGRES_DB: vai_test
          POSTGRES_USER: vai
          POSTGRES_PASSWORD: vai
        ports: [5432:5432]
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --features full
        env:
          VAI_TEST_DATABASE_URL: postgres://vai:vai@localhost:5432/vai_test

  build-image:
    needs: [test-cli, test-full]
    if: github.ref == 'refs/heads/main'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: docker/login-action@v3
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}
      - uses: docker/build-push-action@v5
        with:
          file: infra/Dockerfile.server
          push: true
          tags: ghcr.io/jjordy/vai:latest,ghcr.io/jjordy/vai:${{ github.sha }}
```

## Deployment Flow

### First-time setup (manual):
1. `fly auth login`
2. `fly launch --name vai-server --region iad`
3. `fly postgres create --name vai-db`
4. `fly postgres attach vai-db`
5. Create R2 bucket via Terraform: `cd infra/terraform && terraform apply`
6. Set secrets: `fly secrets set VAI_S3_ENDPOINT=... VAI_S3_BUCKET=... VAI_S3_ACCESS_KEY=... VAI_S3_SECRET_KEY=... VAI_ADMIN_KEY=...`
7. Deploy: `fly deploy`
8. Migrate: `vai remote add https://vai-server.fly.dev --key <admin-key> && vai remote migrate`

### Subsequent deploys:
1. Push to main
2. CI builds and pushes image
3. `fly deploy` (manual) or add auto-deploy step to CI

## Issue Breakdown

1. Create release Dockerfile for vai-server (multi-stage, --features full)
2. Create release Dockerfile for vai-dashboard (multi-stage, static output)
3. Set up GitHub Actions CI pipeline (test CLI, test full with Postgres, build images)
4. Create Docker Compose for self-hosted (vai + dashboard + Postgres + MinIO)
5. Add health check endpoint with subsystem status (Postgres + S3 connectivity)
6. Create Terraform config for Cloudflare R2 bucket
7. Create fly.toml and Fly.io deployment config
8. Create deployment documentation (first-time setup + subsequent deploys)
