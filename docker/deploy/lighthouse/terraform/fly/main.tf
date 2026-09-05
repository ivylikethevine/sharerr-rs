# A sharerr lighthouse on one Fly machine, driven through flyctl.
#
# Why flyctl and not a provider: Fly archived its official Terraform provider
# in 2024 and recommends flyctl or the Machines API directly. Rather than
# depend on an unmaintained provider, this wraps the three flyctl calls a
# deploy needs in `terraform_data` resources, so `apply` and `destroy` still
# mean what they mean elsewhere in this directory. flyctl must be installed
# and logged in (`flyctl auth login`), and every command below is idempotent
# against an app that already exists.
#
# Cost, as of writing: Fly no longer has a free allowance for new
# organisations. The smallest machine (shared-cpu-1x, 256 MB) plus a 1 GB
# volume runs to about two to three dollars a month, billed to the org.
# What Fly does give for free is TLS: <app_name>.fly.dev is HTTPS from the
# first deploy, which is the reason to pick it over a bare VM.

locals {
  fly_toml = templatefile("${path.module}/fly.toml.tftpl", {
    app_name = var.app_name
    region   = var.region
    image    = var.image
    vm_size  = var.vm_size
    memory   = var.memory
    tz       = var.tz
    rust_log = var.rust_log
  })
}

resource "local_file" "fly_toml" {
  content  = local.fly_toml
  filename = "${path.module}/.generated/fly.toml"
}

resource "terraform_data" "app" {
  input = {
    app_name = var.app_name
    org      = var.org
  }

  provisioner "local-exec" {
    command = "flyctl apps list --org ${self.input.org} --json | grep -q '\"Name\": *\"${self.input.app_name}\"' || flyctl apps create ${self.input.app_name} --org ${self.input.org}"
  }

  provisioner "local-exec" {
    when    = destroy
    command = "flyctl apps destroy ${self.input.app_name} --yes"
  }
}

# One 1 GB volume in the primary region. `/data` holds a single decoy secret;
# losing the volume reshuffles fabricated answers, nothing more, so there is
# no snapshot schedule to set up.
resource "terraform_data" "volume" {
  depends_on = [terraform_data.app]
  input = {
    app_name = var.app_name
    region   = var.region
  }

  provisioner "local-exec" {
    command = "flyctl volumes list --app ${self.input.app_name} --json | grep -q '\"name\": *\"lighthouse_data\"' || flyctl volumes create lighthouse_data --app ${self.input.app_name} --region ${self.input.region} --size 1 --yes"
  }
}

resource "terraform_data" "deploy" {
  depends_on       = [terraform_data.volume]
  triggers_replace = [local_file.fly_toml.content]

  provisioner "local-exec" {
    # `--ha=false`: one machine, one volume. Fly's default of two would need
    # a second volume and gives a rendezvous nothing.
    command = "flyctl deploy --config ${local_file.fly_toml.filename} --app ${var.app_name} --ha=false --yes"
  }
}

resource "terraform_data" "cert" {
  count      = var.domain != "" ? 1 : 0
  depends_on = [terraform_data.deploy]
  input = {
    app_name = var.app_name
    domain   = var.domain
  }

  provisioner "local-exec" {
    command = "flyctl certs list --app ${self.input.app_name} --json | grep -q '\"Hostname\": *\"${self.input.domain}\"' || flyctl certs add ${self.input.domain} --app ${self.input.app_name}"
  }
}
