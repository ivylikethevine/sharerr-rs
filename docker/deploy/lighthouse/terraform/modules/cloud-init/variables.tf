variable "domain" {
  description = "Hostname to serve over TLS via Caddy. Empty serves plain HTTP on 7878 with no proxy."
  type        = string
  default     = ""
}

variable "tz" {
  description = "TZ for the lighthouse container's log timestamps."
  type        = string
  default     = "Etc/UTC"
}

variable "rust_log" {
  description = "RUST_LOG filter for the lighthouse."
  type        = string
  default     = "sharerr_lighthouse=info,warn"
}
