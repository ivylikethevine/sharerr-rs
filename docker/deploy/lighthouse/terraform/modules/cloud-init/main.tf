# Renders the cloud-init document both VM targets (aws/, azure/) hand to
# their instance. One module rather than two copies of the template call, so
# the compose files it embeds are read from exactly one place: the standalone
# lighthouse layout two directories up. What a VM runs is byte-for-byte what
# `docker compose up -d` in `docker/deploy/lighthouse/` runs, TLS override
# included, so there is one image pin for dependabot to move and one file to
# read to know what is on the box.
#
# Plain `templatefile` rather than the cloudinit provider's MIME wrapper:
# both clouds accept a bare `#cloud-config` document, and a bare document is
# one you can read back out of the instance's metadata and diff.

locals {
  layout = "${path.module}/../../.."
  tls    = var.domain != ""

  user_data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
    tls              = local.tls
    domain           = var.domain
    tz               = var.tz
    rust_log         = var.rust_log
    compose_yaml     = file("${local.layout}/compose.yaml")
    compose_tls_yaml = file("${local.layout}/compose.tls.yaml")
    caddyfile        = file("${local.layout}/Caddyfile")
  })
}
