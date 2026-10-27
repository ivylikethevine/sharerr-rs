locals {
  # The instance has no private address, so its `ipv4` set is the one public
  # address; `ipv6` is a /128 with its prefix length attached.
  ipv4 = one(linode_instance.lighthouse.ipv4)
  ipv6 = split("/", linode_instance.lighthouse.ipv6)[0]
}

output "ipv4" {
  description = "The instance's public address. With a domain, point its A record here."
  value       = local.ipv4
}

output "ipv6" {
  description = "The instance's public IPv6 address, for an AAAA record; the firewall admits it on the same ports."
  value       = local.ipv6
}

output "lighthouse_url" {
  description = "What to put in a friend's `lighthouse.urls`."
  value       = local.tls ? "https://${var.domain}" : "http://${local.ipv4}:7878"
}

output "instance_id" {
  value = linode_instance.lighthouse.id
}
