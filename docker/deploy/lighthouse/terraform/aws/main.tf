# A sharerr lighthouse on one free-tier EC2 instance: Ubuntu 24.04, Docker
# from the distro, the standalone compose layout from this directory's
# parent, an Elastic IP so the address survives a stop/start, and either
# port 7878 open (plain HTTP) or 80/443 with Caddy terminating TLS for
# `var.domain`. State is one Docker volume on the root disk — the lighthouse
# persists a single decoy secret, nothing worth a snapshot.
#
# Free tier, as of writing: accounts created before mid-2025 get 750 hours a
# month of t2/t3.micro and 30 GB of EBS for twelve months; newer accounts get
# a credit allowance instead, which this instance sits well inside. An Elastic
# IP is free while attached to a running instance and billed while idle, so
# `terraform destroy` rather than stopping the instance when done.

locals {
  name = "sharerr-lighthouse"
  tags = merge({ Name = local.name, project = "sharerr" }, var.tags)
  tls  = var.domain != ""
  ssh  = var.ssh_public_key != ""
}

module "cloud_init" {
  source   = "../modules/cloud-init"
  domain   = var.domain
  tz       = var.tz
  rust_log = var.rust_log
}

data "aws_ami" "ubuntu" {
  most_recent = true
  owners      = ["099720109477"] # Canonical

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd-gp3/ubuntu-noble-24.04-amd64-server-*"]
  }

  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
}

# The account's default VPC, rather than a new one: a lighthouse is one
# public instance with no private tier, and a dedicated VPC would be
# structure with nothing to structure.
data "aws_vpc" "default" {
  default = true
}

data "aws_subnets" "default" {
  filter {
    name   = "vpc-id"
    values = [data.aws_vpc.default.id]
  }
}

resource "aws_security_group" "lighthouse" {
  name        = local.name
  description = "sharerr lighthouse: the one port friends' instances reach"
  vpc_id      = data.aws_vpc.default.id
  tags        = local.tags

  # Plain HTTP on 7878 only when there is no domain to terminate TLS for;
  # with one, 7878 stays closed and Caddy answers on 80/443.
  dynamic "ingress" {
    for_each = local.tls ? [80, 443] : [7878]
    content {
      description = "lighthouse"
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "tcp"
      cidr_blocks = ["0.0.0.0/0"]
    }
  }

  dynamic "ingress" {
    for_each = local.tls ? [443] : []
    content {
      description = "HTTP/3"
      from_port   = ingress.value
      to_port     = ingress.value
      protocol    = "udp"
      cidr_blocks = ["0.0.0.0/0"]
    }
  }

  dynamic "ingress" {
    for_each = local.ssh ? [22] : []
    content {
      description = "ssh"
      from_port   = 22
      to_port     = 22
      protocol    = "tcp"
      cidr_blocks = [var.ssh_cidr]
    }
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_key_pair" "lighthouse" {
  count      = local.ssh ? 1 : 0
  key_name   = local.name
  public_key = var.ssh_public_key
  tags       = local.tags
}

resource "aws_instance" "lighthouse" {
  ami                    = data.aws_ami.ubuntu.id
  instance_type          = var.instance_type
  subnet_id              = data.aws_subnets.default.ids[0]
  vpc_security_group_ids = [aws_security_group.lighthouse.id]
  key_name               = local.ssh ? aws_key_pair.lighthouse[0].key_name : null
  tags                   = local.tags

  # Provider v6.0 stores this in state as cleartext rather than a hash. Fine
  # here: `module.cloud_init.user_data` is domain/tz/log-filter and the
  # static compose/Caddy files, never a secret — the lighthouse's one secret
  # is generated on the box at first run, not templated in.
  user_data                   = module.cloud_init.user_data
  user_data_replace_on_change = true

  root_block_device {
    volume_type           = "gp3"
    volume_size           = 16
    delete_on_termination = true
    tags                  = local.tags
  }

  # IMDSv2 only; nothing on the box reads instance metadata, and v1 is the
  # classic SSRF foothold.
  metadata_options {
    http_tokens = "required"
  }
}

resource "aws_eip" "lighthouse" {
  domain = "vpc"
  tags   = local.tags
}

resource "aws_eip_association" "lighthouse" {
  instance_id   = aws_instance.lighthouse.id
  allocation_id = aws_eip.lighthouse.id
}
