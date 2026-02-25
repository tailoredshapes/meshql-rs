variable "subscription_id" {
  description = "Azure subscription ID"
  type        = string
}

variable "resource_group_name" {
  description = "Azure resource group name (must be pre-created if org policy restricts creation)"
  type        = string
}

variable "location" {
  description = "Azure region"
  type        = string
  default     = "eastus2"
}

variable "project" {
  description = "Project name prefix for resources (naming: {project}-{env}-{region}-{purpose})"
  type        = string
  default     = "farm"
}

variable "env" {
  description = "Environment code (e.g. dev, prod)"
  type        = string
  default     = "dev"
}

variable "region" {
  description = "Region code (e.g. eus2, wus2, uks)"
  type        = string
  default     = "eus2"
}
