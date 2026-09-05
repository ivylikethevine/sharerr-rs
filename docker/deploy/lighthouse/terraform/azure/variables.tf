variable "subscription_id" {
  description = "Azure subscription to deploy into. Leave null to use ARM_SUBSCRIPTION_ID or the az CLI's current account."
  type        = string
  default     = null
}

variable "location" {
  description = "Azure region."
  type        = string
  default     = "westeurope"
}

variable "vm_size" {
  description = "Free-account eligible size: B1s for twelve months (750 hours a month)."
  type        = string
  default     = "Standard_B1s"
}

variable "ssh_public_key" {
  description = "OpenSSH public key for the admin user. Azure Linux VMs require one (or a password); cloud-init does the rest."
  type        = string
}

variable "ssh_cidr" {
  description = "Where SSH may come from. Narrow it to your own address."
  type        = string
  default     = "*"
}

variable "domain" {
  description = "Hostname to serve over TLS. Point its A record at the public_ip output; Caddy obtains the certificate. Empty means plain HTTP on 7878."
  type        = string
  default     = ""
}

variable "tz" {
  type    = string
  default = "Etc/UTC"
}

variable "rust_log" {
  type    = string
  default = "sharerr_lighthouse=info,warn"
}

variable "tags" {
  type    = map(string)
  default = {}
}
