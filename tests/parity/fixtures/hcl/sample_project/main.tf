terraform {
  required_version = ">= 1.0"
}

# --- Variables ---

variable "region" {
  type        = string
  default     = "us-east-1"
  description = "AWS region to deploy into"
}

variable "env" {
  type    = string
  default = "prod"
}

# --- Locals ---

locals {
  common_tags = {
    Environment = var.env
    ManagedBy   = "terraform"
  }
}

# --- Data sources ---

data "aws_ami" "ubuntu" {
  most_recent = true

  filter {
    name   = "name"
    values = ["ubuntu/images/hvm-ssd/ubuntu-*-22.04-amd64-server-*"]
  }
}

data "aws_caller_identity" "current" {}

# --- Resources ---

resource "aws_s3_bucket" "app_artifacts" {
  bucket = "app-artifacts-${var.env}"
  tags   = local.common_tags
}

resource "aws_s3_bucket_versioning" "app_artifacts" {
  bucket = aws_s3_bucket.app_artifacts.id

  versioning_configuration {
    status = "Enabled"
  }
}

# --- Module ---

module "vpc" {
  source = "./modules/vpc"

  cidr    = "10.0.0.0/16"
  region  = var.region
  env     = var.env
}

# --- Outputs ---

output "bucket_name" {
  value       = aws_s3_bucket.app_artifacts.bucket
  description = "Name of the S3 artifact bucket"
}

output "vpc_id" {
  value = module.vpc.vpc_id
}
