# Terraform — Cloudflare R2 Bucket

Provisions the Cloudflare R2 object storage bucket used by vai-server.

## Prerequisites

- [Terraform](https://developer.hashicorp.com/terraform/install) >= 1.0
- A Cloudflare account with R2 enabled
- A Cloudflare API token with **R2 Storage: Edit** permissions

## Setup

1. Copy the example vars file and fill in your values:

   ```sh
   cp terraform.tfvars.example terraform.tfvars
   ```

   Edit `terraform.tfvars`:

   ```hcl
   cloudflare_api_token  = "your-api-token"
   cloudflare_account_id = "your-account-id"
   r2_bucket_name        = "vai-prod"   # optional, defaults to vai-prod
   r2_location           = "ENAM"       # optional, defaults to ENAM
   ```

2. Initialize Terraform (downloads the Cloudflare provider):

   ```sh
   terraform init
   ```

3. Preview the changes:

   ```sh
   terraform plan
   ```

4. Apply (creates the R2 bucket):

   ```sh
   terraform apply
   ```

   After apply, Terraform prints the `r2_endpoint` and `bucket_name` outputs.
   Use `r2_endpoint` as the value for the `VAI_S3_ENDPOINT` secret in Fly.io.

## Variables

| Name | Default | Description |
|------|---------|-------------|
| `cloudflare_api_token` | — | Cloudflare API token (sensitive) |
| `cloudflare_account_id` | — | Cloudflare account ID |
| `r2_bucket_name` | `vai-prod` | R2 bucket name |
| `r2_location` | `ENAM` | Location hint: `ENAM`, `WNAM`, `WEUR`, `EEUR`, `APAC` |

## Outputs

| Name | Description |
|------|-------------|
| `r2_endpoint` | S3-compatible endpoint for `VAI_S3_ENDPOINT` |
| `bucket_name` | Name of the created bucket |

## Destroying

```sh
terraform destroy
```

> **Warning:** This deletes the R2 bucket and all its contents.
