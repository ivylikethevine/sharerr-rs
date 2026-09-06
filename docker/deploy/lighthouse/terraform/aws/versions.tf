terraform {
  required_version = ">= 1.6"

  required_providers {
    aws = {
      source = "hashicorp/aws"
      # v6.0 removed `aws_eip`'s `vpc` argument (this config already used
      # `domain = "vpc"`) and now errors rather than warns when
      # `data.aws_ami`'s `most_recent = true` has no `owners`/image-id filter
      # (this config already sets `owners`) — both already compliant, so the
      # bump needed no config change. See
      # https://registry.terraform.io/providers/hashicorp/aws/latest/docs/guides/version-6-upgrade.
      version = "~> 6.63"
    }
  }
}

provider "aws" {
  region = var.region
}
