output "elastic_ip" {
  description = "The instance's public address. With a domain, point its A record here."
  value       = aws_eip.lighthouse.public_ip
}

output "lighthouse_url" {
  description = "What to put in a friend's `lighthouse.urls`."
  value       = local.tls ? "https://${var.domain}" : "http://${aws_eip.lighthouse.public_ip}:7878"
}

output "instance_id" {
  value = aws_instance.lighthouse.id
}
