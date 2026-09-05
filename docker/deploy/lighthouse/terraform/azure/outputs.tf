output "public_ip" {
  description = "The VM's public address. With a domain, point its A record here."
  value       = azurerm_public_ip.lighthouse.ip_address
}

output "lighthouse_url" {
  description = "What to put in a friend's `lighthouse.urls`."
  value       = local.tls ? "https://${var.domain}" : "http://${azurerm_public_ip.lighthouse.ip_address}:7878"
}

output "ssh" {
  value = "ssh lighthouse@${azurerm_public_ip.lighthouse.ip_address}"
}
