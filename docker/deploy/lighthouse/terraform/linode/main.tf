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

# Linode refuses an image with neither a key nor a root password, so with no
# key root gets a generated one. It is in the state and in no output: port 22
# stays closed, and Lish, behind the Linode account's own login, is the only
# place it could be typed.
resource "random_password" "root" {
  count  = local.ssh ? 0 : 1
  length = 32
}

resource "linode_instance" "lighthouse" {
  label  = local.name
  region = var.region
  type   = var.type
  image  = "linode/ubuntu24.04"
  tags   = local.tags

  # One or the other: the box needs no login, since cloud-init does
  # everything.
  authorized_keys = local.ssh ? [chomp(var.ssh_public_key)] : null
  root_pass       = local.ssh ? null : random_password.root[0].result

  # Linode hands this to cloud-init through its Metadata service. It holds
  # the domain, timezone, log filter and the static compose and Caddy files,
  # never a secret: the lighthouse's one secret is generated on the box.
  metadata {
    user_data = base64encode(module.cloud_init.user_data)
  }

  # A change of password is a reset the provider may power the instance off
  # for, and nothing here ever wants one: the password is set at creation.
  lifecycle {
    ignore_changes = [root_pass]
  }
}
