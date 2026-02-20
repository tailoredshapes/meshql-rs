variable "aws_region" {
  description = "AWS region for Lambda and API Gateway"
  type        = string
  default     = "us-east-1"
}

variable "confluent_cloud_region" {
  description = "Confluent Cloud region for Kafka cluster"
  type        = string
  default     = "us-east-1"
}

variable "confluent_cloud_provider" {
  description = "Cloud provider for Confluent Cloud (AWS, GCP, AZURE)"
  type        = string
  default     = "AWS"
}

variable "environment_name" {
  description = "Confluent Cloud environment name"
  type        = string
  default     = "egg-economy"
}

variable "lambda_memory_size" {
  description = "Lambda function memory in MB"
  type        = number
  default     = 512
}

variable "lambda_timeout" {
  description = "Lambda function timeout in seconds"
  type        = number
  default     = 30
}

variable "topic_partitions" {
  description = "Number of partitions per Kafka topic"
  type        = number
  default     = 6
}
