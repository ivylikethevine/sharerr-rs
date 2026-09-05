# A sharerr lighthouse on one free-account B1s VM: Ubuntu 24.04, Docker from
# the distro, the standalone compose layout from this directory's parent, a
# static public IP, and either 7878 open (plain HTTP) or 80/443 with Caddy
# terminating TLS for `var.domain`. Same cloud-init as aws/.
#
# Free account, as of writing: 750 hours a month of B1s and two 64 GB
# managed disks for twelve months. A Standard-SKU static public IP is not in
# the free allowance and costs a few dollars a month — Basic SKU, which was,
# is retired — so this is "nearly free" rather than free. Container Apps'
# always-free grant was considered and does not cover an always-on replica.

locals {
  name = "sharerr-lighthouse"
  tags = merge({ project = "sharerr" }, var.tags)
  tls  = var.domain != ""
}

module "cloud_init" {
  source   = "../modules/cloud-init"
  domain   = var.domain
  tz       = var.tz
  rust_log = var.rust_log
}

resource "azurerm_resource_group" "lighthouse" {
  name     = local.name
  location = var.location
  tags     = local.tags
}

resource "azurerm_virtual_network" "lighthouse" {
  name                = local.name
  location            = azurerm_resource_group.lighthouse.location
  resource_group_name = azurerm_resource_group.lighthouse.name
  address_space       = ["10.10.0.0/16"]
  tags                = local.tags
}

resource "azurerm_subnet" "lighthouse" {
  name                 = "default"
  resource_group_name  = azurerm_resource_group.lighthouse.name
  virtual_network_name = azurerm_virtual_network.lighthouse.name
  address_prefixes     = ["10.10.1.0/24"]
}

resource "azurerm_public_ip" "lighthouse" {
  name                = local.name
  location            = azurerm_resource_group.lighthouse.location
  resource_group_name = azurerm_resource_group.lighthouse.name
  allocation_method   = "Static"
  sku                 = "Standard"
  tags                = local.tags
}

resource "azurerm_network_security_group" "lighthouse" {
  name                = local.name
  location            = azurerm_resource_group.lighthouse.location
  resource_group_name = azurerm_resource_group.lighthouse.name
  tags                = local.tags

  security_rule {
    name                       = "ssh"
    priority                   = 100
    direction                  = "Inbound"
    access                     = "Allow"
    protocol                   = "Tcp"
    source_port_range          = "*"
    destination_port_range     = "22"
    source_address_prefix      = var.ssh_cidr
    destination_address_prefix = "*"
  }

  # Plain HTTP on 7878 only when there is no domain to terminate TLS for;
  # with one, 7878 stays closed and Caddy answers on 80/443.
  security_rule {
    name                       = "lighthouse"
    priority                   = 110
    direction                  = "Inbound"
    access                     = "Allow"
    protocol                   = "Tcp"
    source_port_range          = "*"
    destination_port_ranges    = local.tls ? ["80", "443"] : ["7878"]
    source_address_prefix      = "*"
    destination_address_prefix = "*"
  }

  dynamic "security_rule" {
    for_each = local.tls ? [1] : []
    content {
      name                       = "http3"
      priority                   = 120
      direction                  = "Inbound"
      access                     = "Allow"
      protocol                   = "Udp"
      source_port_range          = "*"
      destination_port_range     = "443"
      source_address_prefix      = "*"
      destination_address_prefix = "*"
    }
  }
}

resource "azurerm_network_interface" "lighthouse" {
  name                = local.name
  location            = azurerm_resource_group.lighthouse.location
  resource_group_name = azurerm_resource_group.lighthouse.name
  tags                = local.tags

  ip_configuration {
    name                          = "primary"
    subnet_id                     = azurerm_subnet.lighthouse.id
    private_ip_address_allocation = "Dynamic"
    public_ip_address_id          = azurerm_public_ip.lighthouse.id
  }
}

resource "azurerm_network_interface_security_group_association" "lighthouse" {
  network_interface_id      = azurerm_network_interface.lighthouse.id
  network_security_group_id = azurerm_network_security_group.lighthouse.id
}

resource "azurerm_linux_virtual_machine" "lighthouse" {
  name                  = local.name
  location              = azurerm_resource_group.lighthouse.location
  resource_group_name   = azurerm_resource_group.lighthouse.name
  size                  = var.vm_size
  admin_username        = "lighthouse"
  network_interface_ids = [azurerm_network_interface.lighthouse.id]
  tags                  = local.tags

  admin_ssh_key {
    username   = "lighthouse"
    public_key = var.ssh_public_key
  }

  os_disk {
    caching              = "ReadWrite"
    storage_account_type = "Standard_LRS"
    disk_size_gb         = 30
  }

  source_image_reference {
    publisher = "Canonical"
    offer     = "ubuntu-24_04-lts"
    sku       = "server"
    version   = "latest"
  }

  custom_data = base64encode(module.cloud_init.user_data)
}
