# A sharerr lighthouse on one Linode Nanode: Ubuntu 24.04, Docker from the
# distro, the standalone compose layout from this directory's parent, and a
# Cloud Firewall that admits either port 7878 (plain HTTP) or 80/443 with
# Caddy terminating TLS for `var.domain`. The instance's public IPv4 is fixed
# for its lifetime, so no separate address resource is needed.
#
# Cost, as of writing: a Nanode is five dollars a month and the firewall is
# free. Linode has no free tier.

locals {
  name = "sharerr-lighthouse"
  tags = concat(["sharerr"], var.tags)
  tls  = var.domain != ""
  ssh  = var.ssh_public_key != ""
}

module "cloud_init" {
  source   = "../modules/cloud-init"
  domain   = var.domain
  tz       = var.tz
  rust_log = var.rust_log
}

resource "linode_firewall" "lighthouse" {
  label           = local.name
  tags            = local.tags
  inbound_policy  = "DROP"
  outbound_policy = "ACCEPT"
  linodes         = [linode_instance.lighthouse.id]

  # Plain HTTP on 7878 only when there is no domain to terminate TLS for;
  # with one, 7878 stays closed and Caddy answers on 80/443.
  inbound {
    label    = "lighthouse"
    action   = "ACCEPT"
    protocol = "TCP"
    ports    = local.tls ? "80,443" : "7878"
    ipv4     = ["0.0.0.0/0"]
    ipv6     = ["::/0"]
  }

  dynamic "inbound" {
    for_each = local.tls ? [443] : []
    content {
      label    = "http3"
      action   = "ACCEPT"
      protocol = "UDP"
      ports    = tostring(inbound.value)
      ipv4     = ["0.0.0.0/0"]
      ipv6     = ["::/0"]
    }
  }

  dynamic "inbound" {
    for_each = local.ssh ? [22] : []
    content {
      label    = "ssh"
      action   = "ACCEPT"
      protocol = "TCP"
      ports    = tostring(inbound.value)
      ipv4     = [var.ssh_cidr]
    }
  }
}

resource "linode_instance" "lighthouse" {
  label  = local.name
  region = var.region
  type   = var.type
  image  = "linode/ubuntu24.04"
  tags   = local.tags

  # With no key, root has a random password the provider never stores: the
  # box needs no login, since cloud-init does everything.
  authorized_keys = local.ssh ? [chomp(var.ssh_public_key)] : null

  # Linode hands this to cloud-init through its Metadata service. It holds
  # the domain, timezone, log filter and the static compose and Caddy files,
  # never a secret: the lighthouse's one secret is generated on the box.
  metadata {
    user_data = base64encode(module.cloud_init.user_data)
  }
}
