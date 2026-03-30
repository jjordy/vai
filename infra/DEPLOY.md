# vai — Fly.io Deployment Guide

This guide covers deploying `vai-server` to [Fly.io](https://fly.io) with a managed Postgres database and Cloudflare R2 for object storage.

## Architecture

```
Fly.io vai-server  ──▶  Fly.io Postgres
        │
        └──────────────▶  Cloudflare R2
```

The vai-server binary runs as a Docker container on Fly.io. All metadata (repos, workspaces, issues, events) is stored in Fly Postgres. File contents (blobs, tarballs) are stored in Cloudflare R2.

## Prerequisites

- [flyctl](https://fly.io/docs/hands-on/install-flyctl/) installed and authenticated
- A [Cloudflare](https://cloudflare.com) account with R2 enabled
- Terraform (optional, for provisioning R2 via IaC — see `infra/terraform/`)

---

## First-Time Setup

### 1. Authenticate with Fly.io

```bash
fly auth login
```

### 2. Create the Fly app

Run this from the repository root (where `fly.toml` is located, or pass `-c infra/fly.toml`):

```bash
fly launch --no-deploy -c infra/fly.toml
```

This registers the app name `vai-server` in your Fly.io account. Skip the auto-generated `fly.toml` — the one in `infra/` is already configured.

### 3. Create a Fly Postgres cluster

```bash
fly postgres create --name vai-postgres --region iad --initial-cluster-size 1 --vm-size shared-cpu-1x --volume-size 1
```

This provisions a free-tier Postgres instance in the `iad` region.

### 4. Attach Postgres to the app

```bash
fly postgres attach vai-postgres --app vai-server
```

Fly automatically sets the `DATABASE_URL` secret on the app.

### 5. Provision Cloudflare R2

Either create a bucket manually in the Cloudflare dashboard, or use the Terraform config in `infra/terraform/`:

```bash
cd infra/terraform
cp terraform.tfvars.example terraform.tfvars  # fill in your values
terraform init
terraform apply
```

Take note of the R2 endpoint (`https://<account-id>.r2.cloudflarestorage.com`) and bucket name from the Terraform output (or from the Cloudflare dashboard).

### 6. Generate an admin key

```bash
openssl rand -hex 32
```

Keep this value — it is your `VAI_ADMIN_KEY`. You will need it to authenticate as admin.

### 7. Set secrets

```bash
fly secrets set \
  VAI_S3_ENDPOINT="https://<account-id>.r2.cloudflarestorage.com" \
  VAI_S3_BUCKET="vai-prod" \
  VAI_S3_ACCESS_KEY="<r2-access-key-id>" \
  VAI_S3_SECRET_KEY="<r2-secret-access-key>" \
  VAI_ADMIN_KEY="<generated-above>" \
  VAI_CORS_ORIGINS="https://dashboard.vai.dev" \
  --app vai-server
```

`DATABASE_URL` was already set by `fly postgres attach` in step 4.

### 8. Run database migrations

The server runs migrations automatically on startup. To run them manually before the first deploy:

```bash
fly ssh console --app vai-server -C "vai migrate"
```

Or simply deploy — the server applies any pending migrations on boot.

### 9. Deploy

```bash
fly deploy -c infra/fly.toml
```

Fly builds the Docker image using `infra/Dockerfile.server` and deploys it to the `iad` region.

### 10. Verify

```bash
fly status --app vai-server
curl https://vai-server.fly.dev/health
```

---

## Subsequent Deploys

```bash
fly deploy -c infra/fly.toml
```

Fly performs a rolling deploy: it starts a new machine, waits for the health check to pass, then terminates the old one. Downtime is zero for single-instance deployments with `auto_stop_machines = false`.

### Deploy from CI

The CI pipeline (`.github/workflows/ci.yml`) automatically builds and pushes the Docker image to GHCR on every push to `main`. To trigger a Fly deploy from CI, add a deploy step:

```yaml
- name: Deploy to Fly.io
  run: fly deploy -c infra/fly.toml --image ghcr.io/${{ github.repository }}:latest
  env:
    FLY_API_TOKEN: ${{ secrets.FLY_API_TOKEN }}
```

Set `FLY_API_TOKEN` in your GitHub repository secrets (`fly tokens create deploy`).

---

## Rollback

List recent releases:

```bash
fly releases --app vai-server
```

Roll back to a specific version:

```bash
fly releases rollback <version-number> --app vai-server
```

Example:

```bash
fly releases rollback 42 --app vai-server
```

Fly immediately redeploys the selected image version. The rollback takes effect within ~30 seconds.

---

## Running Migrations

Migrations run automatically on server startup via `sqlx::migrate!()`. The server will refuse to start if migrations fail.

To inspect or run migrations manually:

```bash
# Open a console on the running machine
fly ssh console --app vai-server

# Run migrations manually (inside the console)
vai migrate

# Or use sqlx CLI directly with the DATABASE_URL secret
fly ssh console --app vai-server -C "sqlx migrate run --database-url \$DATABASE_URL"
```

To check which migrations have been applied:

```bash
fly ssh console --app vai-server -C "sqlx migrate info --database-url \$DATABASE_URL"
```

---

## Secrets Reference

| Secret | Description |
|--------|-------------|
| `DATABASE_URL` | Postgres connection string (set automatically by `fly postgres attach`) |
| `VAI_S3_ENDPOINT` | R2 endpoint URL: `https://<account-id>.r2.cloudflarestorage.com` |
| `VAI_S3_BUCKET` | R2 bucket name (e.g. `vai-prod`) |
| `VAI_S3_ACCESS_KEY` | R2 API token key ID |
| `VAI_S3_SECRET_KEY` | R2 API token secret |
| `VAI_ADMIN_KEY` | Admin API key for privileged operations |
| `VAI_CORS_ORIGINS` | Comma-separated allowed CORS origins (e.g. `https://dashboard.vai.dev`) |

To update a secret:

```bash
fly secrets set VAI_ADMIN_KEY="<new-value>" --app vai-server
```

Fly automatically redeploys the app when secrets change.

---

## Monitoring

```bash
# Live logs
fly logs --app vai-server

# App status and machine health
fly status --app vai-server

# Postgres status
fly postgres status vai-postgres
```

The health check endpoint returns subsystem status:

```bash
curl https://vai-server.fly.dev/health
```

---

## Scaling

The default config runs one machine (`min_machines_running = 1`, `auto_stop_machines = false`). To scale:

```bash
# Add more machines
fly scale count 2 --app vai-server

# Upgrade VM size
fly scale vm shared-cpu-2x --app vai-server

# View current scale
fly scale show --app vai-server
```
