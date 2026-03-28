# PRD 17: Hosting and Deployment

## Overview

Get vai off the local machine and running as a hosted service. Start with the cheapest viable infrastructure (Fly.io + Cloudflare R2), design for portability across providers, and provide a self-hosted option for enterprise.

## Design Principles

1. **Provider-agnostic** — S3-compatible storage, standard Postgres, containerized compute. Switch providers by changing config, not code.
2. **Self-hosted is a first-class target** — Docker Compose with vai-server + Postgres + MinIO. Three commands to run.
3. **Infrastructure as code** — every environment reproducible from Terraform/Pulumi.
4. **Zero-downtime deploys** — rolling updates, health checks, graceful shutdown.
5. **vai deploys vai (eventually)** — once vai is stable enough, use it to manage its own deployments. Until then, GitHub Actions + container registry.

## Architecture

### Components

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   vai server    │────▶│   Postgres        │     │   S3-compatible │
│   (Rust binary) │     │   (managed)       │     │   (R2/S3/MinIO) │
│                 │────▶│                   │     │                 │
│   REST + WS     │     └──────────────────┘     └─────────────────┘
└────────┬────────┘                                       ▲
         │                                                │
         └────────────────────────────────────────────────┘
         ▲                    ▲
         │                    │
┌────────┴───────┐   ┌───────┴────────┐
│  vai-dashboard │   │  agents (CLI)  │
│  (static SPA)  │   │  (vai-agent)   │
└────────────────┘   └────────────────┘
```

### Provider Matrix

| Component | MVP (Cheapest) | Production | Self-Hosted |
|-----------|---------------|------------|-------------|
| Compute | Fly.io ($5-20/mo) | AWS ECS / Fly | Docker |
| Database | Neon free tier / Fly Postgres | AWS RDS | Docker (Postgres) |
| Object Storage | Cloudflare R2 (free egress) | R2 or AWS S3 | MinIO |
| Dashboard | Cloudflare Pages (free) | Cloudflare Pages | Served by vai-server |
| DNS/TLS | Cloudflare (free) | Cloudflare / Route53 | Let's Encrypt |
| CI/CD | GitHub Actions (free) | GitHub Actions | GitHub Actions |
| Monitoring | Fly metrics + Sentry free | Datadog / Grafana | Prometheus + Grafana |

### Cost Estimates

**MVP (single instance, ~100 users):**
| Resource | Monthly |
|----------|---------|
| Fly.io shared-cpu-1x (256MB) | $3 |
| Fly Postgres (single node) | $0 (free tier) |
| Cloudflare R2 (10GB) | $0 (free tier) |
| Cloudflare Pages | $0 |
| Total | **~$3/mo** |

**Growth (dedicated, ~1000 users):**
| Resource | Monthly |
|----------|---------|
| Fly.io performance-2x (4GB) | $60 |
| Fly Postgres (HA, 2 nodes) | $50 |
| Cloudflare R2 (100GB) | $1.50 |
| Sentry (error tracking, free tier) | $0 |
| Fly metrics (built-in) | $0 |
| PostHog (analytics, free tier 1M events) | $0 |
| Total | **~$112/mo** |

**Enterprise (self-hosted, customer pays):**
Customer runs Docker Compose or Helm chart on their infra. We provide the images and support.

### Observability Strategy (Low-Cost Start)

Avoid Datadog/New Relic until revenue justifies it. Start with free/cheap tools:

| Need | Tool | Cost |
|------|------|------|
| Error tracking | Sentry (free: 5K events/mo) | $0 |
| Metrics/dashboards | Fly.io built-in metrics + Grafana Cloud free | $0 |
| Uptime monitoring | Betterstack (free: 5 monitors) | $0 |
| Analytics | PostHog (free: 1M events/mo) | $0 |
| Logging | Fly.io log drains → Logtail free tier | $0 |
| Alerting | Sentry + Betterstack | $0 |

Graduate to paid tools when: >$10K MRR or free tiers become limiting. Prometheus + Grafana self-hosted is always an option for the cost-conscious.

### Pricing Tiers

| Tier | Monthly | Includes | Target |
|------|---------|----------|--------|
| **Free** | $0 | 1 repo, 50 submits/mo, 500MB S3 | Individual devs trying it out |
| **Personal** | $19/mo | 5 repos, 200 submits/mo, 5GB S3, 1 seat | Solo developers / freelancers |
| **Team** | $99/mo | 10 repos, 500 submits/mo, 10GB S3, 5 seats | Small teams |
| **Scale** | $499/mo | Unlimited repos, 5000 submits/mo, 100GB S3, unlimited seats | Growing teams |
| **Enterprise** | Custom | Self-hosted, SLA, SSO, audit logs, dedicated support | Large orgs |

Overage: $0.15/submit (Team), $0.08/submit (Scale). No overage on Free/Personal — hard cap.

Expected funnel: most users stay on Free for weeks/months. Personal captures solo devs who hit the 1-repo limit. Team is the conversion target once they bring it to work.

## Deployment Pipeline

### Phase 1: Manual Deploy (Now → MVP)

```
Developer pushes to GitHub
  → GitHub Actions builds release binary
  → Builds Docker image
  → Pushes to GitHub Container Registry (ghcr.io)
  → fly deploy (manual trigger or on tag)
```

### Phase 2: Automated Deploy (MVP → Production)

```
Developer pushes to main
  → GitHub Actions:
    1. cargo test + clippy
    2. Build multi-arch Docker image (amd64 + arm64)
    3. Push to ghcr.io
    4. Deploy to Fly.io staging
    5. Run E2E smoke tests against staging
    6. If pass → deploy to production (blue/green)
    7. Health check → rollback if unhealthy
```

### Phase 3: vai Deploys vai (Future)

```
Agent submits workspace to vai repo
  → vai creates new version
  → Webhook triggers GitHub Actions
  → CI builds + tests
  → Deploy to staging
  → Automated E2E verification
  → Promote to production
```

## Configuration

### Environment Variables

```bash
# Required
DATABASE_URL=postgres://user:pass@host:5432/vai
VAI_S3_ENDPOINT=https://xxx.r2.cloudflarestorage.com
VAI_S3_BUCKET=vai-prod
VAI_S3_ACCESS_KEY=xxx
VAI_S3_SECRET_KEY=xxx
VAI_ADMIN_KEY=xxx

# Optional
VAI_PORT=7865
VAI_CORS_ORIGINS=https://dashboard.vai.dev
VAI_LOG_LEVEL=info
VAI_MAX_UPLOAD_SIZE=104857600  # 100MB
VAI_PG_POOL_SIZE=25
```

### Docker Compose (Self-Hosted)

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
      VAI_ADMIN_KEY: ${VAI_ADMIN_KEY}
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
    volumes: ["s3data:/data"]
    environment:
      MINIO_ROOT_USER: minioadmin
      MINIO_ROOT_PASSWORD: minioadmin

volumes:
  pgdata:
  s3data:
```

### Fly.io Config

```toml
# fly.toml
app = "vai-server"
primary_region = "iad"

[build]
  dockerfile = "Dockerfile.release"

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

## Docker Images

### vai-server

```dockerfile
# Dockerfile.release
FROM rust:1.82-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --features full

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/vai /usr/local/bin/vai
EXPOSE 7865
CMD ["vai", "server", "--multi-repo", "--pg-url", "$DATABASE_URL"]
```

### vai-dashboard

```dockerfile
FROM node:22-alpine AS builder
WORKDIR /app
COPY package.json pnpm-lock.yaml ./
RUN corepack enable && pnpm install --frozen-lockfile
COPY . .
RUN pnpm run build

FROM node:22-alpine
WORKDIR /app
COPY --from=builder /app/dist ./dist
COPY --from=builder /app/node_modules ./node_modules
COPY --from=builder /app/package.json .
EXPOSE 3000
CMD ["node", "dist/server/index.mjs"]
```

## Database Migrations

Migrations run automatically on server startup via `sqlx::migrate!()`. For production:

1. Migrations are forward-only (no down migrations)
2. Each migration is idempotent (`CREATE TABLE IF NOT EXISTS`, `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`)
3. Breaking schema changes get a deprecation period: add new column → migrate data → remove old column (across 2 releases)
4. Backup database before deploying migrations in production

## Monitoring and Alerting

### Health Checks
- `GET /health` — returns 200 if server, Postgres, and S3 are reachable
- Fly.io auto-restarts unhealthy instances

### Metrics (Future — PRD XX-performance)
- Request latency, error rates per endpoint
- Postgres connection pool utilization
- S3 operation latency
- WebSocket connected clients
- Active workspace count

### Alerting Triggers
- Health check fails for > 30s
- Error rate > 5% for 5 minutes
- Postgres connection pool exhausted
- S3 unreachable

## Rollback Strategy

1. **Compute:** Fly.io keeps previous release. `fly releases rollback` instantly reverts.
2. **Database:** Forward-only migrations. If a migration breaks, fix forward with a new migration.
3. **S3:** Content-addressable — old content is never overwritten. Rollback is just updating path mappings.

## Issue Breakdown

1. **Create release Dockerfile for vai-server** — Multi-stage build, feature flags, health check, minimal image size. Priority: high.
2. **Create release Dockerfile for vai-dashboard** — Multi-stage build, static asset serving. Priority: high.
3. **Set up GitHub Actions CI pipeline** — cargo test, clippy, build Docker images, push to ghcr.io on tag. Priority: high.
4. **Deploy vai-server to Fly.io** — fly.toml config, secrets management, Fly Postgres setup. Priority: high.
5. **Configure Cloudflare R2 for production storage** — Create bucket, configure CORS, set up access keys. Priority: high.
6. **Deploy vai-dashboard to Cloudflare Pages** — Build config, environment variables, custom domain. Priority: high.
7. **Create Docker Compose for self-hosted** — vai-server + dashboard + Postgres + MinIO. Document quickstart. Priority: medium.
8. **Add health check endpoint with subsystem status** — Check Postgres connectivity, S3 connectivity, return detailed status. Priority: medium.
9. **Create Terraform modules for AWS deployment** — ECS + RDS + S3 modules for enterprise customers. Priority: low.
10. **Set up Helm chart for Kubernetes deployment** — For enterprise customers running k8s. Priority: low.

## Migration Path (Local Dev → Hosted)

1. Build and tag release: `git tag v0.1.0 && git push --tags`
2. GitHub Actions builds images automatically
3. Set up Fly.io app and Postgres: `fly launch && fly postgres create`
4. Set up R2 bucket: via Cloudflare dashboard
5. Configure secrets: `fly secrets set DATABASE_URL=... VAI_S3_ENDPOINT=...`
6. Deploy: `fly deploy`
7. Run migration from local repo: `vai remote add https://vai.fly.dev --key <admin-key> && vai remote migrate`
8. Deploy dashboard to Cloudflare Pages
9. Point DNS: `vai.dev` → Fly.io, `dashboard.vai.dev` → Cloudflare Pages

## Future: vai Deploys vai

Once vai is self-hosting:
1. vai server monitors its own GitHub repo via watcher
2. On new tag → creates issue "Deploy v0.2.0"
3. Agent claims issue, runs deployment script
4. Deployment script: pull image, blue/green swap, health check
5. Agent closes issue with deployment summary
6. If health check fails → agent creates rollback issue

This is the ultimate dogfood — vai managing its own infrastructure lifecycle.
