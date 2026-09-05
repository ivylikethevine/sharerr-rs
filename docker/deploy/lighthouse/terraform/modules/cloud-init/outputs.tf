output "user_data" {
  description = "The rendered cloud-init document, ready for user_data / custom_data."
  value       = local.user_data
}

output "tls" {
  description = "Whether the instance terminates TLS (a domain was given)."
  value       = local.tls
}
