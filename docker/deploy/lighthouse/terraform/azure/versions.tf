terraform {
  required_version = ">= 1.6"

  required_providers {
    azurerm = {
      source  = "hashicorp/azurerm"
      version = "~> 5.4"
    }
  }
}

provider "azurerm" {
  features {}
  # Null falls back to ARM_SUBSCRIPTION_ID / `az account show`.
  subscription_id = var.subscription_id

  # v5.0 flipped this from "legacy" (register ~60 resource providers on
  # every init, whether this config uses them or not) to "none" (register
  # nothing, and rely on the subscription already having what a resource
  # needs). A subscription that has never deployed a VM/VNet/NSG before —
  # exactly the free-account case this module targets — would then fail
  # `terraform apply` with a resource-provider-not-registered error instead
  # of the provider handling it. "core" is the registry's own name for the
  # Compute/Networking/Storage set, which is precisely what main.tf uses.
  resource_provider_registrations = "core"
}
