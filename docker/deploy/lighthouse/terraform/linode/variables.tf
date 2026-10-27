variable "region" {
  description = "Linode region. It needs the Metadata service, which cloud-init reads; `linode-cli regions list` shows each region's capabilities."
  type        = string
  default     = "eu-central"
}

variable "type" {
  description = "Plan. A Nanode is far more than a lighthouse needs."
  type        = string
  default     = "g6-nanode-1"
}

variable "domain" {
  description = "Hostname to serve over TLS. Point its A record at the ipv4 output; Caddy obtains the certificate. Empty means plain HTTP on 7878."
  type        = string
  default     = ""
}

variable "ssh_public_key" {
  description = "OpenSSH public key for root. Empty leaves port 22 closed, which is fine: cloud-init does everything."
  type        = string
  default     = ""
}

variable "ssh_cidr" {
  description = "Where SSH may come from, when a key is given. Narrow it to your own address."
  type        = string
  default     = "0.0.0.0/0"
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
  description = "Extra tags on the instance and firewall."
  type        = list(string)
  default     = []
}
