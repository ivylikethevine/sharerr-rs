terraform {
  required_version = ">= 1.6"

  required_providers {
    azurerm = {
      source  = "hashicorp/azurerm"
      version = "~> 5.3"
    }
  }
}

provider "azurerm" {
  features {}
  # Null falls back to ARM_SUBSCRIPTION_ID / `az account show`.
  subscription_id = var.subscription_id
}
