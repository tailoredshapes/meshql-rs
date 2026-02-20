terraform {
  required_version = ">= 1.5"

  required_providers {
    confluent = {
      source  = "confluentinc/confluent"
      version = "~> 2.0"
    }
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

# Confluent Cloud provider — uses CONFLUENT_CLOUD_API_KEY / CONFLUENT_CLOUD_API_SECRET env vars
provider "confluent" {}

provider "aws" {
  region = var.aws_region
}
