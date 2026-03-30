variable "cloudflare_api_token" {
  description = "Cloudflare API token with R2 read/write permissions"
  type        = string
  sensitive   = true
}

variable "cloudflare_account_id" {
  description = "Cloudflare account ID"
  type        = string
}

variable "r2_bucket_name" {
  description = "Name of the R2 bucket to create"
  type        = string
  default     = "vai-prod"
}

variable "r2_location" {
  description = "R2 bucket location hint (ENAM, WNAM, WEUR, EEUR, APAC)"
  type        = string
  default     = "ENAM"
}
