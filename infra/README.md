# vai — Self-Hosted Deployment

This directory contains everything needed to run vai on your own infrastructure using Docker Compose.

## Services

| Service      | Image                              | Port(s)          | Purpose                          |
|--------------|------------------------------------|------------------|----------------------------------|
| `vai-server` | `ghcr.io/jjordy/vai:latest`        | 7865             | vai API server                   |
| `dashboard`  | `ghcr.io/jjordy/vai-dashboard:latest` | 3000          | Web dashboard                    |
| `postgres`   | `postgres:16-alpine`               | 5432             | Metadata database                |
| `minio`      | `minio/minio`                      | 9000 / 9001      | S3-compatible file storage       |

## Prerequisites

- [Docker](https://docs.docker.com/get-docker/) 24+
- [Docker Compose](https://docs.docker.com/compose/install/) v2+

## Quick Start

### 1. Copy the server config

```bash
cp configs/server.toml.example configs/server.toml
```

The default `server.toml` configures the S3 endpoint to point at the MinIO container. No edits are required for a local deployment.

### 2. Generate an admin key (recommended)

```bash
export VAI_ADMIN_KEY=$(openssl rand -hex 32)
echo "VAI_ADMIN_KEY=$VAI_ADMIN_KEY" >> .env
```

If `VAI_ADMIN_KEY` is not set, the server generates a random key on each start and prints it to the logs. Set it explicitly so the key survives container restarts.

### 3. Start all services

```bash
docker compose up -d
```

Docker Compose starts the services in dependency order:
1. `postgres` and `minio` start first
2. `minio-init` creates the `vai-prod` bucket in MinIO
3. `vai-server` connects to both and runs database migrations
4. `dashboard` starts after the server is healthy

### 4. Verify everything is running

```bash
docker compose ps
```

Check server health:

```bash
curl http://localhost:7865/health
```

Open the dashboard at **http://localhost:3000**.

### 5. Get your admin key

If you did not set `VAI_ADMIN_KEY`, retrieve the generated key from the logs:

```bash
docker compose logs vai-server | grep "Admin key"
```

## Stopping

```bash
# Stop containers, keep volumes
docker compose down

# Stop containers and delete all data
docker compose down -v
```

## MinIO Console

The MinIO web console is available at **http://localhost:9001**.

Default credentials:
- Username: `minioadmin`
- Password: `minioadmin`

**Change these credentials** for any non-local deployment by editing the `MINIO_ROOT_USER` and `MINIO_ROOT_PASSWORD` environment variables in `docker-compose.yml` and updating the matching `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` values in the `vai-server` service.

## Configuration

### Environment variables

| Variable             | Service      | Description                              |
|----------------------|--------------|------------------------------------------|
| `VAI_ADMIN_KEY`      | vai-server   | Admin API key (generated if not set)     |
| `VAI_CORS_ORIGINS`   | vai-server   | Comma-separated allowed CORS origins     |
| `AWS_ACCESS_KEY_ID`  | vai-server   | MinIO access key                         |
| `AWS_SECRET_ACCESS_KEY` | vai-server | MinIO secret key                       |
| `VITE_VAI_SERVER_URL` | dashboard   | URL the browser uses to reach the server |
| `MINIO_ROOT_USER`    | minio        | MinIO root username                      |
| `MINIO_ROOT_PASSWORD` | minio       | MinIO root password                      |

### server.toml

Settings that cannot be passed as environment variables (S3 endpoint, bucket name, region) live in `configs/server.toml`, which is mounted into the `vai-server` container at `/root/.vai/server.toml`.

See `configs/server.toml.example` for all available options.

## Data persistence

Data is stored in two named Docker volumes:

| Volume          | Contents                                |
|-----------------|-----------------------------------------|
| `postgres_data` | PostgreSQL database (metadata, issues)  |
| `minio_data`    | MinIO object storage (file contents)    |

To back up:

```bash
# PostgreSQL
docker compose exec postgres pg_dump -U vai vai > vai-backup.sql

# MinIO (using mc)
docker run --rm --network host minio/mc \
  mirror local/vai-prod ./vai-backup-s3/
```

## Upgrading

```bash
docker compose pull
docker compose up -d
```

The server automatically runs database migrations on startup.

## Troubleshooting

**Server fails to start with "database connection error"**
- Check that `postgres` is healthy: `docker compose ps postgres`
- Verify the database URL in `docker-compose.yml` matches your Postgres credentials

**File uploads fail**
- Check that `minio` is running and the `vai-prod` bucket exists
- View MinIO logs: `docker compose logs minio`
- Confirm `configs/server.toml` points to `http://minio:9000`

**Dashboard cannot reach the server**
- `VITE_VAI_SERVER_URL` must be the URL the *browser* uses to reach the server
- For local deployments use `http://localhost:7865`
- For remote deployments use your server's public URL
