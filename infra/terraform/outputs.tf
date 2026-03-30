output "r2_endpoint" {
  description = "S3-compatible endpoint URL for the R2 bucket"
  value       = "https://${var.cloudflare_account_id}.r2.cloudflarestorage.com"
}

output "bucket_name" {
  description = "Name of the created R2 bucket"
  value       = cloudflare_r2_bucket.vai.name
}
