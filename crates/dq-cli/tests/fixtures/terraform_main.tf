terraform {
  required_version = ">= 1.5.0"

  backend "s3" {
    bucket = "dq-tfstate.example.test"
    region = "us-east-1"
  }
}

variable "region" {
  type    = string
  default = "us-east-1"
}

variable "instance_count" {
  type    = number
  default = 3
}
