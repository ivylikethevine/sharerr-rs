terraform {
  required_version = ">= 1.6"

  required_providers {
    linode = {
      source  = "linode/linode"
      version = "~> 4.7"
    }
  }
}

# The token comes from LINODE_TOKEN in the environment, never a variable, so
# it never lands in a .tfvars file or a plan.
provider "linode" {}
