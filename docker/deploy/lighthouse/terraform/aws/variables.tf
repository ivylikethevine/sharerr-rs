variable "region" {
  description = "AWS region. Pick one close to the friend group; the lighthouse is latency-insensitive but not free of it."
  type        = string
  default     = "eu-west-1"
}

variable "instance_type" {
  description = "Free-tier eligible burstable instance. t3.micro in most regions; t2.micro where t3 is not free-tier eligible."
  type        = string
  default     = "t3.micro"
}

variable "domain" {
  description = "Hostname to serve over TLS. Point its A record at the elastic_ip output; Caddy obtains the certificate. Empty means plain HTTP on 7878."
  type        = string
  default     = ""
}

variable "ssh_public_key" {
  description = "OpenSSH public key for the ubuntu user. Empty leaves no SSH access at all, which is fine: cloud-init does everything."
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
  description = "Extra tags on every resource."
  type        = map(string)
  default     = {}
}
