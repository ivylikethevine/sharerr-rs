variable "app_name" {
  description = "Fly app name; globally unique, becomes <app_name>.fly.dev."
  type        = string
}

variable "org" {
  description = "Fly organisation slug (`flyctl orgs list`)."
  type        = string
  default     = "personal"
}

variable "region" {
  description = "Fly region code (`flyctl platform regions`)."
  type        = string
  default     = "ams"
}

variable "image" {
  description = "The lighthouse image. `:latest` tracks the newest tagged release; pin a `vX.Y.Z` tag to hold it still."
  type        = string
  default     = "ghcr.io/ivylikethevine/sharerr-lighthouse:latest"
}

variable "vm_size" {
  description = "Fly machine preset. shared-cpu-1x is the smallest and plenty."
  type        = string
  default     = "shared-cpu-1x"
}

variable "memory" {
  description = "Machine memory. 256mb is the floor and plenty."
  type        = string
  default     = "256mb"
}

variable "domain" {
  description = "Optional custom hostname. Fly issues the certificate once its CNAME points at <app_name>.fly.dev; the app's own .fly.dev name has TLS regardless."
  type        = string
  default     = ""
}

variable "tz" {
  type    = string
  default = "Etc/UTC"
}

variable "rust_log" {
  type    = string
  default = "sharerr_lighthouse=info,warn"
}
