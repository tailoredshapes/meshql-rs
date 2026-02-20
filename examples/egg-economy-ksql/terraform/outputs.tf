output "api_url" {
  description = "API Gateway endpoint URL"
  value       = aws_apigatewayv2_stage.default.invoke_url
}

output "kafka_cluster_id" {
  description = "Confluent Cloud Kafka cluster ID"
  value       = confluent_kafka_cluster.main.id
}

output "kafka_rest_endpoint" {
  description = "Kafka REST API endpoint"
  value       = confluent_kafka_cluster.main.rest_endpoint
}

output "ksqldb_endpoint" {
  description = "ksqlDB HTTP endpoint"
  value       = confluent_ksql_cluster.main.rest_endpoint
}

output "lambda_function_name" {
  description = "Lambda function name"
  value       = aws_lambda_function.egg_economy.function_name
}
