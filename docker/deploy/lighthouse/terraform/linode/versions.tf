terraform {
  required_version = ">= 1.6"

  required_providers {
    linode = {
      source  = "linode/linode"
      version = "~> 4.7"
    }
  }
}

# No provider block: run as a root, the token comes from LINODE_TOKEN in the
# environment, never a variable, so it never lands in a .tfvars file or a
# plan; called as a child module, the caller's `provider "linode"` applies.
# An empty block here would be the deprecated proxy form, which init warns
# about; a configured one would shadow the caller's.
