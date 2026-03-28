# vai Production Deployment Security Checklist

This document covers hardening requirements for running the vai server in production. Follow every section before exposing the server to external traffic.

## Table of Contents

1. [Build the Server Binary](#1-build-the-server-binary)
2. [Environment Variables](#2-environment-variables)
3. [TLS Termination](#3-tls-termination)
4. [PostgreSQL Configuration](#4-postgresql-configuration)
5. [S3 Bucket Policy](#5-s3-bucket-policy)
6. [Firewall Rules](#6-firewall-rules)
7. [CORS Configuration](#7-cors-configuration)
8. [Log Rotation](#8-log-rotation)
9. [Backup Strategy](#9-backup-strategy)
10. [Dependency Auditing](#10-dependency-auditing)

---

## 1. Build the Server Binary

The server binary requires the `full` feature set (Postgres + S3 + HTTP server):

```bash
cargo build --release --features full
```

The default `cargo build` produces a CLI-only binary without server, Postgres, or S3 support. Never deploy the CLI-only build as a server.

---

## 2. Environment Variables

**Never put secrets in `~/.vai/server.toml` or any config file committed to source control.** All credentials must come from environment variables.

### Required

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | Postgres connection string, e.g. `postgres://vai:password@db:5432/vai`. Use `sslmode=require` in production. |
| `AWS_ACCESS_KEY_ID` | S3 or MinIO access key ID. |
| `AWS_SECRET_ACCESS_KEY` | S3 or MinIO secret access key. |

### Strongly Recommended

| Variable | Description |
|----------|-------------|
| `VAI_ADMIN_KEY` | Admin API key. If unset, a random key is generated on each startup and printed to stdout — this key is lost on restart. Set a stable value in production. |
| `VAI_CORS_ORIGINS` | Comma-separated list of allowed CORS origins, e.g. `https://app.example.com`. Defaults to `*` (all origins) if unset — **not safe in production**. |

### Optional

| Variable | Description |
|----------|-------------|
| `VAI_DATABASE_URL` | Alternative to `DATABASE_URL`. `VAI_DATABASE_URL` takes precedence. |

### Server config (`~/.vai/server.toml`)

Non-secret settings belong in the config file. Example production config:

```toml
[server]
host = "127.0.0.1"   # bind to loopback; nginx/caddy handles public TLS
port = 7865
storage_root = "/var/vai/repos"

[server.s3]
bucket = "vai-production"
region = "us-east-1"
# endpoint_url and force_path_style only needed for MinIO/self-hosted S3
```

Do not add `database_url`, `cors_origins`, or any secret to this file.

---

## 3. TLS Termination

vai does not handle TLS directly. Use nginx or Caddy as a TLS-terminating reverse proxy.

### Option A — Caddy (recommended for simplicity)

Caddy provisions and renews Let's Encrypt certificates automatically.

```
# /etc/caddy/Caddyfile
vai.example.com {
    reverse_proxy 127.0.0.1:7865

    # WebSocket pass-through (required for /api/repos/:repo/ws/events)
    @ws {
        header Connection *Upgrade*
        header Upgrade    websocket
    }
    reverse_proxy @ws 127.0.0.1:7865
}
```

### Option B — nginx

```nginx
# /etc/nginx/sites-available/vai
server {
    listen 443 ssl http2;
    server_name vai.example.com;

    ssl_certificate     /etc/letsencrypt/live/vai.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/vai.example.com/privkey.pem;

    # Modern TLS only
    ssl_protocols       TLSv1.2 TLSv1.3;
    ssl_ciphers         ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;
    ssl_prefer_server_ciphers off;

    # WebSocket upgrade headers
    proxy_http_version 1.1;
    proxy_set_header   Upgrade    $http_upgrade;
    proxy_set_header   Connection "upgrade";
    proxy_set_header   Host       $host;
    proxy_set_header   X-Real-IP  $remote_addr;

    # Increase timeouts for long-lived WebSocket connections
    proxy_read_timeout  3600s;
    proxy_send_timeout  3600s;

    location / {
        proxy_pass http://127.0.0.1:7865;
    }
}

server {
    listen 80;
    server_name vai.example.com;
    return 301 https://$host$request_uri;
}
```

Use Certbot to obtain and renew Let's Encrypt certificates:

```bash
certbot --nginx -d vai.example.com
```

### Checklist

- [ ] HTTPS is enforced; HTTP redirects to HTTPS
- [ ] TLS 1.2 minimum; TLS 1.3 preferred
- [ ] Weak cipher suites disabled
- [ ] WebSocket upgrade headers forwarded correctly
- [ ] `vai server start` binds to `127.0.0.1`, not `0.0.0.0`

---

## 4. PostgreSQL Configuration

### Connection

Always use `sslmode=require` in `DATABASE_URL`:

```
postgres://vai:password@db.internal:5432/vai?sslmode=require
```

### Recommended Postgres Settings

Apply these in `postgresql.conf` or via `ALTER SYSTEM`:

```sql
-- Enforce SSL for all connections
ssl = on
ssl_min_protocol_version = 'TLSv1.2'

-- Limit total connections; vai uses a pool (default 25 connections)
max_connections = 100

-- Statement timeout prevents runaway queries
statement_timeout = '30s'

-- Log slow queries for performance analysis
log_min_duration_statement = 1000   -- ms; queries slower than 1s are logged

-- Log connection/disconnection events
log_connections = on
log_disconnections = on
```

In `pg_hba.conf`, restrict access to the vai application user only:

```
# TYPE  DATABASE  USER  ADDRESS          METHOD
host    vai       vai   10.0.0.0/8       scram-sha-256
host    all       all   0.0.0.0/0        reject
```

### Role Permissions

Create a dedicated role with least-privilege access:

```sql
CREATE ROLE vai LOGIN PASSWORD 'strong-random-password';
CREATE DATABASE vai OWNER vai;

-- Grant only what vai needs (migrations run as vai owner, so this is sufficient)
GRANT CONNECT ON DATABASE vai TO vai;
```

### Connection Limits per Role

Prevent a misconfigured instance from exhausting the pool:

```sql
ALTER ROLE vai CONNECTION LIMIT 50;
```

---

## 5. S3 Bucket Policy

### Bucket settings

- **Block all public access** — enable all four "block public access" settings on the bucket. vai file content must never be publicly readable.
- **Versioning** — enable versioning for recovery from accidental deletes.
- **Server-side encryption** — enable SSE-S3 (AES-256) or SSE-KMS at minimum.

### IAM Policy for vai

The vai server only needs `GetObject`, `PutObject`, and `DeleteObject` on its own bucket. Example IAM policy (attach to the vai IAM user or role):

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "s3:GetObject",
        "s3:PutObject",
        "s3:DeleteObject"
      ],
      "Resource": "arn:aws:s3:::vai-production/*"
    },
    {
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::vai-production"
    }
  ]
}
```

### Object key layout

vai stores files content-addressably under `{repo_id}/{sha256}`. No object should ever be accessible outside this key prefix. The bucket policy should deny all other actions.

### Checklist

- [ ] Public access is fully blocked
- [ ] Versioning enabled
- [ ] Server-side encryption enabled
- [ ] vai IAM user/role has only the four permissions above
- [ ] No presigned URL generation unless explicitly needed
- [ ] Bucket is in the same region as the vai server (reduces latency and avoids cross-region data transfer costs)

---

## 6. Firewall Rules

Only port 443 (HTTPS/WSS) should be reachable from the public internet.

```
# Inbound — allow from anywhere
TCP  443   (HTTPS + WebSocket via TLS)

# Inbound — allow only from known admin IP ranges
TCP  22    (SSH for server management)

# All other inbound — DROP

# Outbound — allow
TCP  5432  (Postgres — to DB server IP only)
TCP  443   (S3 / AWS APIs)
TCP  80    (Let's Encrypt ACME challenges, if applicable)
```

The vai HTTP server (`127.0.0.1:7865`) must be accessible only from localhost. Confirm:

```bash
ss -tlnp | grep 7865
# Expected: 127.0.0.1:7865 — NOT 0.0.0.0:7865
```

If deploying with Docker:
- Do not publish port 7865 to the host (`-p 7865:7865`). Let nginx/Caddy on the host proxy to the container via an internal network.
- Use a Docker bridge network and reference the container by name.

---

## 7. CORS Configuration

In production, restrict CORS to the exact origin(s) of your dashboard. The default `*` (all origins) allows any website to make authenticated requests to the API using the visitor's cookies — this is unsafe in production.

Set via environment variable:

```bash
VAI_CORS_ORIGINS=https://app.example.com
```

Or in `server.toml` (comma-separated for multiple origins):

```toml
[server]
cors_origins = ["https://app.example.com", "https://admin.example.com"]
```

The CLI flag `--cors-origins` overrides both. Do not use `*` in any production configuration.

---

## 8. Log Rotation

vai writes structured logs to stdout. Use your process supervisor (systemd, Docker) to capture and rotate them.

### systemd

```ini
# /etc/systemd/system/vai.service
[Unit]
Description=vai version control server
After=network.target

[Service]
Type=simple
User=vai
EnvironmentFile=/etc/vai/env          # contains DATABASE_URL, AWS_*, etc.
ExecStart=/usr/local/bin/vai server start
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

journald retains logs by default. Configure retention in `/etc/systemd/journald.conf`:

```ini
[Journal]
SystemMaxUse=2G
SystemMaxFileSize=200M
MaxRetentionSec=90day
```

### Docker / Docker Compose

Use the `json-file` logging driver with rotation:

```yaml
services:
  vai:
    image: vai-server:latest
    logging:
      driver: json-file
      options:
        max-size: "100m"
        max-file: "10"
```

### logrotate (if writing to a file)

If you redirect stdout to a file, add a logrotate config:

```
/var/log/vai/server.log {
    daily
    rotate 30
    compress
    delaycompress
    missingok
    notifempty
    postrotate
        systemctl kill -s HUP vai.service
    endscript
}
```

---

## 9. Backup Strategy

### PostgreSQL

Run `pg_dump` daily and retain backups for at least 30 days. Store backups off-site (separate cloud region or storage account).

```bash
#!/bin/bash
# /etc/cron.daily/vai-pg-backup
set -euo pipefail

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
BACKUP_DIR=/var/backups/vai/postgres
mkdir -p "$BACKUP_DIR"

pg_dump \
  --format=custom \
  --compress=9 \
  --no-password \
  "$DATABASE_URL" \
  > "$BACKUP_DIR/vai_${TIMESTAMP}.dump"

# Retain 30 days
find "$BACKUP_DIR" -name "*.dump" -mtime +30 -delete
```

Verify backups are restorable by running a restore test monthly:

```bash
pg_restore --list vai_backup.dump | head -20
```

For managed Postgres (RDS, Supabase, Neon, etc.) enable automated snapshots with a minimum 7-day retention.

### S3

Enable S3 Versioning and S3 Object Lock (governance mode) on the vai bucket to prevent accidental deletion of file objects. Replication to a second region provides disaster recovery:

```bash
aws s3api put-bucket-replication \
  --bucket vai-production \
  --replication-configuration file://replication.json
```

At minimum, enable cross-region replication or take daily S3 inventory snapshots.

### Recovery Time Objective

| Component | Recommended RPO | Notes |
|-----------|----------------|-------|
| Postgres  | 24 hours (daily backup) | Point-in-time recovery with WAL archiving gives < 1 minute RPO |
| S3 files  | Near-zero with versioning + replication | Deletes are recoverable via versioning |

---

## 10. Dependency Auditing

CI automatically runs `cargo audit --deny warnings` on every push (see `.github/workflows/ci.yml`). This fails the build on any known advisory.

Run locally before shipping new dependencies:

```bash
cargo audit
```

If an advisory is found for an indirect dependency you cannot immediately update, review the advisory details and add it to `audit.toml` only if the vulnerability does not affect vai's usage:

```toml
# audit.toml
[[advisories.ignore]]
id = "RUSTSEC-YYYY-NNNN"
reason = "Explain why this advisory does not affect vai"
```

Do not ignore high-severity advisories without team review.

---

## Pre-Launch Checklist

- [ ] Binary built with `--features full --release`
- [ ] `DATABASE_URL` uses `sslmode=require`
- [ ] `VAI_ADMIN_KEY` is set to a stable random value (not auto-generated)
- [ ] `VAI_CORS_ORIGINS` is set to the production dashboard origin
- [ ] `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` set in environment, not config files
- [ ] vai server binds to `127.0.0.1`, not `0.0.0.0`
- [ ] nginx/Caddy is the only process listening on 443
- [ ] TLS 1.2+ enforced; HTTP → HTTPS redirect in place
- [ ] S3 bucket has public access fully blocked
- [ ] S3 IAM policy grants only `GetObject`, `PutObject`, `DeleteObject`, `ListBucket`
- [ ] Postgres role has connection limit; `pg_hba.conf` restricts to vai server IP only
- [ ] Firewall drops all inbound traffic except 443 (and 22 from admin IPs)
- [ ] Log rotation configured (journald limits or logrotate)
- [ ] Postgres backup running daily; restore tested
- [ ] S3 versioning enabled
- [ ] `cargo audit` passes with no warnings
