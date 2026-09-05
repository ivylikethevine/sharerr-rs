output "lighthouse_url" {
  description = "What to put in a friend's `lighthouse.urls`."
  value       = var.domain != "" ? "https://${var.domain}" : "https://${var.app_name}.fly.dev"
}

output "dns" {
  description = "For a custom domain: the CNAME to create before the certificate can be issued."
  value       = var.domain != "" ? "${var.domain} CNAME ${var.app_name}.fly.dev" : null
}
